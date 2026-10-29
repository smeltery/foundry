import { expect, test } from "bun:test";
import { runAdapter } from "../src/controller.ts";
import {
  fixture,
  harness,
  loaded,
  begin,
  payload,
  observe,
  continuation,
  capture,
  exporting,
  start,
  deferred,
} from "./lifecycle_helpers.ts";

test("failed Accepted write consumes prepared task without running it", async () => {
  const f = fixture();
  const h = harness(f.facade, async (event) => {
    if (event.type === "continuation_accepted_v2")
      throw Error("private-output");
  });
  await loaded(h);
  h.send(continuation);
  expect((await h.result).status).toBe("failed");
  h.close();
  expect(f.calls.filter((call) => call === "prepare")).toHaveLength(1);
  expect(f.calls).not.toContain("continue");
  expect(f.calls).toContain("dispose");
});

test("external stop interrupts each pending hook and suppresses its late completion", async () => {
  for (const phase of [
    "capture",
    "begin",
    "load",
    "observe",
    "prepare",
    "export",
  ] as const) {
    const f = fixture();
    const stop = new AbortController();
    const entered = deferred<void>();
    const late = deferred<void>();
    const hook = async (_request: unknown, signal: AbortSignal) => {
      entered.resolve();
      await late.promise;
      expect(signal.aborted).toBe(true);
    };
    if (phase === "capture") f.facade.lifecycle.capture!.prepare = hook;
    if (phase === "begin") f.facade.lifecycle.restore!.begin = hook;
    if (phase === "load") f.facade.lifecycle.restore!.load = hook;
    if (phase === "observe")
      f.facade.lifecycle.restore!.observe = async (r, s) => {
        await hook(r, s);
        return { instance_id: observe.challenge, bytes: Buffer.from("late") };
      };
    if (phase === "prepare")
      f.facade.lifecycle.restore!.continuation!.prepare = async (r, s) => {
        await hook(r, s);
        return {
          async run() {
            throw Error("must not run");
          },
        };
      };
    if (phase === "export")
      f.facade.lifecycle.capture!.export = async (r, s) => {
        await hook(r, s);
        throw Error("late");
      };
    const h = harness(f.facade, undefined, stop.signal);
    await h.wait("ready");
    if (phase === "capture") h.send(capture);
    else if (phase === "export") {
      h.send(capture);
      await h.wait("context_capture_ready");
      h.send(start);
      await h.wait("settled");
      h.send(exporting);
    } else {
      h.send(begin);
      if (phase !== "begin") {
        await h.wait("restore_prepared_v2");
        h.send(payload);
        if (phase !== "load") {
          await h.wait("context_restored_v2");
          h.send(observe);
          if (phase !== "observe") {
            await h.wait("context_observed_v1");
            h.send(continuation);
          }
        }
      }
    }
    await entered.promise;
    stop.abort();
    expect(await h.result).toEqual({ status: "cancelled" });
    const count = h.events.length;
    late.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(h.events).toHaveLength(count);
    h.close();
    expect(f.calls.filter((call) => call === "dispose")).toHaveLength(1);
    expect(f.calls.filter((call) => call === "cancel")).toHaveLength(1);
  }
});

test("stop at Accepted barrier never starts the prepared continuation", async () => {
  const f = fixture();
  const stop = new AbortController();
  const h = harness(
    f.facade,
    async (event) => {
      if (event.type === "continuation_accepted_v2") stop.abort();
    },
    stop.signal,
  );
  await loaded(h);
  h.send(continuation);
  expect(await h.result).toEqual({ status: "cancelled" });
  h.close();
  expect(f.calls).not.toContain("continue");
  expect(f.calls).toContain("dispose");
});

test("cleanup attempts dispose even when cancel fails and reports the failure", async () => {
  const f = fixture();
  const stop = new AbortController();
  f.facade.cancel = async () => {
    throw Error("private-cleanup");
  };
  const h = harness(f.facade, undefined, stop.signal);
  await h.wait("ready");
  stop.abort();
  expect(await h.result).toEqual({
    status: "failed",
    code: "adapter_shutdown_failed",
  });
  h.close();
  expect(f.calls).toEqual(["dispose"]);
});

test("retained EOF disposes without fabricating another terminal", async () => {
  const f = fixture();
  const h = harness(f.facade);
  await h.wait("ready");
  h.send(capture);
  await h.wait("context_capture_ready");
  h.send(start);
  await h.wait("settled");
  h.close();
  expect(await h.result).toEqual({ status: "failed", code: "input_closed" });
  expect(
    h.events.filter((event) =>
      ["settled", "failed"].includes(event.type as string),
    ),
  ).toHaveLength(1);
  expect(f.calls).toContain("dispose");
});

test("signal listener is removed and already-stopped owners emit no Ready", async () => {
  const f = fixture();
  const stop = new AbortController();
  stop.abort();
  const h = harness(f.facade, undefined, stop.signal);
  expect(await h.result).toEqual({ status: "cancelled" });
  expect(h.events).toEqual([]);
  h.close();
  const g = fixture();
  const other = new AbortController();
  const active = harness(g.facade, undefined, other.signal);
  await active.wait("ready");
  active.send(start);
  expect(await active.result).toEqual({ status: "completed" });
  other.abort();
  await Promise.resolve();
  expect(g.calls).not.toContain("cancel");
  active.close();
});

test("partial lifecycle groups fail without advertising capabilities", async () => {
  const f = fixture();
  const output: Uint8Array[] = [];
  const result = await runAdapter({
    input: (async function* () {})(),
    facade: {
      ...f.facade,
      lifecycle: { capture: {} },
    } as unknown as typeof f.facade,
    write: async (frame) => {
      output.push(frame);
    },
  });
  expect(result.status).toBe("failed");
  expect(output).toEqual([]);
  expect(f.calls).toContain("dispose");
});

test("failed private response write is not retried as a second private response", async () => {
  const f = fixture();
  const attempted: string[] = [];
  const h = harness(f.facade, async (event) => {
    attempted.push(event.type as string);
    if (event.type === "context_observed_v1") throw Error("output gone");
  });
  await h.wait("ready");
  h.send(begin);
  await h.wait("restore_prepared_v2");
  h.send(payload);
  await h.wait("context_restored_v2");
  h.send(observe);
  expect((await h.result).status).toBe("failed");
  h.close();
  expect(attempted).not.toContain("context_observation_rejected_v1");
  expect(f.calls).toContain("dispose");
});

test("saturated private output does not block cancellation or cleanup", async () => {
  const f = fixture();
  const stop = new AbortController();
  const entered = deferred<void>();
  const unblock = deferred<void>();
  const h = harness(
    f.facade,
    async (event) => {
      if (event.type === "context_observed_v1") {
        entered.resolve();
        await unblock.promise;
      }
    },
    stop.signal,
  );
  await h.wait("ready");
  h.send(begin);
  await h.wait("restore_prepared_v2");
  h.send(payload);
  await h.wait("context_restored_v2");
  h.send(observe);
  await entered.promise;
  stop.abort();
  expect((await h.result).status).toBe("failed");
  expect(f.calls).toContain("dispose");
  unblock.resolve();
  h.close();
});

test("graceful cancellation and hanging disposal share one shutdown budget", async () => {
  const f = fixture();
  const running = deferred<void>();
  const releaseRun = deferred<void>();
  const releaseDispose = deferred<void>();
  let context!: import("../src/facade.ts").RunContext;
  let disposing = false;
  f.facade.run = async (_session, actual) => {
    context = actual;
    running.resolve();
    await releaseRun.promise;
    return { outcome: "success", result: "late" };
  };
  f.facade.dispose = async () => {
    disposing = true;
    await releaseDispose.promise;
  };
  const h = harness(f.facade);
  await h.wait("ready");
  h.send(capture);
  await h.wait("context_capture_ready");
  h.send(start);
  await running.promise;
  const started = performance.now();
  h.send({ type: "cancel", attempt_id: "attempt", mode: "graceful" });
  await new Promise((resolve) => setTimeout(resolve, 0));
  await expect(context.text("after stop")).rejects.toThrow("adapter_stopped");
  expect((await h.result).status).toBe("failed");
  expect(performance.now() - started).toBeLessThan(180);
  expect(disposing).toBe(true);
  expect(
    h.events.some((event) => event.type === "text" || event.type === "settled"),
  ).toBe(false);
  releaseRun.resolve();
  releaseDispose.resolve();
  h.close();
});

test("early Export produces scoped failure after cleanup without Candidate", async () => {
  const f = fixture();
  const h = harness(f.facade);
  await h.wait("ready");
  h.send(exporting);
  expect((await h.result).status).toBe("failed");
  h.close();
  expect(h.events.at(-1)).toEqual({
    type: "context_failed",
    attempt_id: "attempt",
    operation_id: "capture",
    code: "invalid_state",
  });
  expect(f.calls).toContain("dispose");
  expect(f.calls).not.toContain("export");
});

test("synchronous terminal encoding failure still owns failing disposal", async () => {
  const f = fixture();
  f.facade.run = async () => ({
    outcome: "success",
    result: "x".repeat(2 * 1024 * 1024),
  });
  let disposed = false;
  f.facade.dispose = async () => {
    disposed = true;
    throw Error("private-dispose");
  };
  const h = harness(f.facade);
  await h.wait("ready");
  h.send(start);
  expect(await h.result).toEqual({
    status: "failed",
    code: "adapter_shutdown_failed",
  });
  h.close();
  expect(disposed).toBe(true);
  expect(h.events.some((event) => event.type === "settled")).toBe(false);
});
