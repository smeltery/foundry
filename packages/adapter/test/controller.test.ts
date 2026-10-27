import { describe, expect, test } from "bun:test";
import type { Completion } from "../src/facade.ts";
import { MAX_FRAME_BYTES, type Json } from "../src/protocol.ts";
import { deferred, harness, start } from "./helpers.ts";
import { runAdapter } from "../src/controller.ts";

describe("owned adapter protocol", () => {
  test("blocked output cannot prevent bounded cancellation", async () => {
    let input!: ReadableStreamDefaultController<Uint8Array>;
    const blocked = deferred<void>();
    const release = deferred<void>();
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        input = controller;
      },
    });
    const result = runAdapter({
      input: stream,
      shutdownTimeoutMs: 20,
      facade: {
        run: async (_session, context) => {
          await context.text("blocked");
        },
      },
      write: async (frame) => {
        const event = JSON.parse(new TextDecoder().decode(frame));
        if (event.type === "text") {
          blocked.resolve();
          await release.promise;
        }
      },
    });
    const send = (value: unknown) =>
      input.enqueue(new TextEncoder().encode(JSON.stringify(value) + "\n"));
    send(start);
    await blocked.promise;
    send({ type: "cancel", attempt_id: "attempt", mode: "graceful" });
    expect(await result).toEqual({ status: "cancelled" });
    release.resolve();
    input.close();
  });

  test("unconfigured adapter never starts a model or reports success", async () => {
    const h = harness();
    await h.wait("ready");
    h.send(start);
    expect(await h.result).toEqual({ status: "failed", code: "adapter_not_configured" });
    expect(h.events.map((event) => event.type)).toEqual(["ready", "failed"]);
    h.close();
  });

  test("streams deltas and requires an explicit completion verdict", async () => {
    const h = harness({
      run: async (session, context) => {
        expect(session.prompt).toBe("offline test");
        await context.text("first ");
        await context.text("second");
        // No completion envelope must never imply success.
      },
    });
    h.send(start);
    expect(await h.result).toEqual({ status: "completed" });
    expect(h.events).toContainEqual({ type: "text", attempt_id: "attempt", text: "first " });
    expect(h.events.at(-1)).toEqual({
      type: "settled",
      attempt_id: "attempt",
      outcome: "action_required",
      result: "",
    });
    h.close();
  });

  test("tool replies remain denials and no tool is executed locally", async () => {
    let received: Json | undefined;
    const h = harness({
      run: async (_session, context) => {
        received = await context.tool("call", "read", { path: "/not-opened" });
        return { outcome: "action_required", result: "tool unavailable" };
      },
    });
    h.send(start);
    await h.wait("tool_call");
    h.send({
      type: "tool_result",
      attempt_id: "attempt",
      call_id: "call",
      result: { status: "denied", code: "tool_execution_disabled" },
    });
    expect((await h.result).status).toBe("completed");
    expect(received).toEqual({ status: "denied", code: "tool_execution_disabled" });
    h.close();
  });

  test("tool wait is interrupted by cancellation and late success is suppressed", async () => {
    let signal: AbortSignal | undefined;
    const h = harness({
      run: async (_session, context) => {
        signal = context.signal;
        try {
          await context.tool("call", "shell", {});
        } catch {}
        await context.text("late text");
        return { outcome: "success", result: "late result" };
      },
    });
    h.send(start);
    await h.wait("tool_call");
    h.send({ type: "cancel", attempt_id: "attempt", mode: "graceful" });
    expect(await h.result).toEqual({ status: "cancelled" });
    expect(signal?.aborted).toBe(true);
    expect(h.events.some((event) => event.type === "settled" || event.type === "text")).toBe(false);
    h.close();
  });

  test("unresponsive facade cancellation is bounded; hard upgrade is immediate", async () => {
    for (const hard of [false, true]) {
      const running = deferred<void>();
      const h = harness({
        run: async () => {
          running.resolve();
          await new Promise(() => {});
        },
      });
      h.send(start);
      await running.promise;
      h.send({ type: "cancel", attempt_id: "attempt", mode: "graceful" });
      if (hard) h.send({ type: "cancel", attempt_id: "attempt", mode: "hard" });
      expect(await h.result).toEqual({ status: "cancelled" });
      expect(h.events.some((event) => event.type === "settled")).toBe(false);
      h.close();
    }
  });

  test.each([
    [{ ...start, version: 2 }, "unsupported_version"],
    [{ ...start, session: { ...start.session, active_attempt_id: "other" } }, "stale_attempt"],
    [{ ...start, session: { ...start.session, directory: "relative" } }, "invalid_directory"],
    [{ ...start, session: { ...start.session, version: 2 ** 54 } }, "unsafe_integer"],
  ])("rejects invalid start %j", async (command, code) => {
    let invoked = false;
    const h = harness({
      run: async () => {
        invoked = true;
      },
    });
    h.send(command);
    expect(await h.result).toEqual({ status: "failed", code });
    expect(invoked).toBe(false);
    h.close();
  });

  test.each([
    [
      { type: "tool_result", attempt_id: "attempt", call_id: "unknown", result: {} },
      "unknown_tool_result",
    ],
    [{ type: "cancel", attempt_id: "other", mode: "hard" }, "stale_attempt"],
    [start, "duplicate_start"],
  ])("fences commands while a run is pending %j", async (command, code) => {
    const running = deferred<void>();
    const h = harness({
      run: async () => {
        running.resolve();
        await new Promise(() => {});
      },
    });
    h.send(start);
    await running.promise;
    h.send(command);
    expect(await h.result).toEqual({ status: "failed", code });
    h.close();
  });

  test("steering is deduplicated and cannot block cancellation", async () => {
    const running = deferred<void>();
    const steered = deferred<void>();
    let count = 0;
    const h = harness({
      run: async () => {
        running.resolve();
        await new Promise(() => {});
      },
      steer: async () => {
        count++;
        steered.resolve();
        await new Promise(() => {});
      },
    });
    h.send(start);
    await running.promise;
    const message = { type: "message", attempt_id: "attempt", message_id: "m", text: "new input", receipt: "opaque-tail-challenge" };
    h.send(message);
    h.send(message);
    await steered.promise;
    h.send({ type: "cancel", attempt_id: "attempt", mode: "hard" });
    expect(await h.result).toEqual({ status: "cancelled" });
    expect(count).toBe(1);
    h.close();
  });

  test("facade exceptions cannot leak credentials", async () => {
    const h = harness({
      run: async () => {
        throw new Error("private-credential-value");
      },
    });
    h.send(start);
    expect(await h.result).toEqual({ status: "failed", code: "adapter_failed" });
    expect(JSON.stringify(h.events)).not.toContain("private-credential-value");
    h.close();
  });

  test("oversized facade output cannot become successful settlement", async () => {
    const h = harness({
      run: async (_session, context) => {
        try {
          await context.text("x".repeat(MAX_FRAME_BYTES));
        } catch {}
        return { outcome: "success", result: "ignored" };
      },
    });
    h.send(start);
    expect((await h.result).status).toBe("failed");
    expect(h.events.some((event) => event.type === "settled")).toBe(false);
    h.close();
  });

  test("EOF fails an active attempt instead of fabricating completion", async () => {
    const running = deferred<void>();
    const done = deferred<Completion>();
    const h = harness({
      run: async () => {
        running.resolve();
        return done.promise;
      },
    });
    h.send(start);
    await running.promise;
    h.close();
    expect(await h.result).toEqual({ status: "failed", code: "input_closed" });
    done.resolve({ outcome: "success", result: "too late" });
    expect(h.events.some((event) => event.type === "settled")).toBe(false);
  });
});
