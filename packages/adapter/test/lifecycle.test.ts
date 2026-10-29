import { expect, test } from "bun:test";
import {
  fixture,
  harness,
  loaded,
  capture,
  exporting,
  release,
  continuation,
  start,
  deferred,
  observe,
  payload,
  begin,
} from "./lifecycle_helpers.ts";
import type { RunContext } from "../src/facade.ts";

test("fresh capture retains owner through settlement, Export and Release", async () => {
  const f = fixture();
  const h = harness(f.facade);
  await h.wait("ready");
  expect(h.events[0].capabilities).toEqual([
    "context_capture_v1",
    "context_export_v1",
    "context_restore_v2",
    "context_observe_v1",
    "context_continue_v2",
  ]);
  h.send(capture);
  await h.wait("context_capture_ready");
  h.send(start);
  await h.wait("settled");
  expect(f.calls).toEqual(["capture", "start"]);
  let done = false;
  void h.result.then(() => {
    done = true;
  });
  await Promise.resolve();
  expect(done).toBe(false);
  h.send(exporting);
  await h.wait("context_candidate");
  expect(f.calls).toEqual(["capture", "start", "export"]);
  h.send(release);
  expect(await h.result).toEqual({ status: "completed" });
  h.close();
  expect(f.calls).toEqual(["capture", "start", "export", "dispose"]);
});

test("restored continuation waits for Accepted write and never replays Start", async () => {
  const f = fixture();
  const entered = deferred<void>();
  const flush = deferred<void>();
  const h = harness(f.facade, async (event) => {
    if (event.type === "continuation_accepted_v2") {
      entered.resolve();
      await flush.promise;
    }
  });
  await loaded(h);
  h.send(continuation);
  await entered.promise;
  expect(f.calls).toEqual(["begin", "load", "observe", "prepare"]);
  flush.resolve();
  await h.wait("settled");
  expect(f.calls).toEqual(["begin", "load", "observe", "prepare", "continue"]);
  expect(h.events.map((event) => event.type)).toEqual([
    "ready",
    "restore_prepared_v2",
    "context_restored_v2",
    "context_observed_v1",
    "continuation_accepted_v2",
    "text",
    "settled",
  ]);
  h.send(exporting);
  await h.wait("context_candidate");
  h.send(release);
  expect(await h.result).toEqual({ status: "completed" });
  h.close();
  expect(f.calls.filter((call) => call === "dispose")).toHaveLength(1);
});

test("ordinary Start with context-capable facade still completes and disposes", async () => {
  const f = fixture();
  const h = harness(f.facade);
  await h.wait("ready");
  h.send(start);
  expect(await h.result).toEqual({ status: "completed" });
  h.close();
  expect(f.calls).toEqual(["start", "dispose"]);
});

test("context hooks preserve ordinary Start whitespace and escaped text", async () => {
  const f = fixture();
  let prompt: string | undefined;
  f.facade.run = async (session) => {
    prompt = session.prompt;
    return { outcome: "success", result: "done" };
  };
  const h = harness(f.facade);
  await h.wait("ready");
  const wire = JSON.stringify(start, null, " ")
    .replaceAll("\n", " ")
    .replace("offline test", "\\u006fffline test");
  h.raw(Buffer.from(wire + "\n"));
  expect(await h.result).toEqual({ status: "completed" });
  h.close();
  expect(prompt).toBe("offline test");
  expect(f.calls).toEqual(["dispose"]);
});

test("tool callback and message receipts share the retained execution reader and writer", async () => {
  const f = fixture();
  const finish = deferred<void>();
  let context!: RunContext;
  f.facade.run = async (_session, actual) => {
    context = actual;
    expect(await actual.tool("call", "read", { path: "item" })).toEqual({
      status: "denied",
    });
    await finish.promise;
    return { outcome: "action_required", result: "done" };
  };
  const steered: string[] = [];
  f.facade.steer = async (id) => {
    steered.push(id);
  };
  const h = harness(f.facade);
  await h.wait("ready");
  h.send(capture);
  await h.wait("context_capture_ready");
  h.send(start);
  await h.wait("tool_call");
  h.send({
    type: "tool_result",
    attempt_id: "attempt",
    call_id: "call",
    result: { status: "denied" },
  });
  const message = {
    type: "message",
    attempt_id: "attempt",
    message_id: "one",
    text: "same",
    receipt: "first",
  };
  h.send(message);
  await h.wait("message_accepted");
  h.send(message);
  await h.wait("message_accepted", 2);
  h.send({ ...message, message_id: "two", receipt: "second" });
  await h.wait("message_accepted", 3);
  expect(steered).toEqual(["one", "two"]);
  finish.resolve();
  await h.wait("settled");
  await expect(context.text("late")).rejects.toThrow();
  await expect(context.tool("late", "read", {})).rejects.toThrow();
  h.send(exporting);
  await h.wait("context_candidate");
  h.send(release);
  expect(await h.result).toEqual({ status: "completed" });
  h.close();
  expect(
    h.events
      .filter((event) => event.type === "message_accepted")
      .map((event) => event.receipt),
  ).toEqual(["first", "first", "second"]);
});

test("wrong phase, duplicate continuation and substituted scope never dispatch work twice", async () => {
  for (const bad of [
    start,
    payload,
    {
      ...observe,
      binding: { ...observe.binding, begin_binding: "ff".repeat(32) },
    },
    {
      ...continuation,
      scope: { ...continuation.scope, restore_id: "foreign" },
    },
  ]) {
    const f = fixture();
    const h = harness(f.facade);
    await loaded(h);
    h.send(bad);
    expect((await h.result).status).toBe("failed");
    h.close();
    expect(f.calls).not.toContain("continue");
    expect(f.calls).not.toContain("start");
    expect(h.events.some((event) => event.type === "failed")).toBe(false);
  }
  const f = fixture();
  const h = harness(f.facade);
  await loaded(h);
  h.send(continuation);
  await h.wait("settled");
  h.send(continuation);
  expect((await h.result).status).toBe("failed");
  h.close();
  expect(f.calls.filter((call) => call === "continue")).toHaveLength(1);
});

test("observation rejection follows cleanup with exact request correlation", async () => {
  const f = fixture();
  f.facade.lifecycle.restore!.observe = async () => {
    throw Error("private-canary");
  };
  const h = harness(f.facade, async (event) => {
    if (event.type === "context_observation_rejected_v1")
      expect(f.calls).toContain("dispose");
  });
  await h.wait("ready");
  h.send(begin);
  await h.wait("restore_prepared_v2");
  h.send(payload);
  await h.wait("context_restored_v2");
  h.send(observe);
  expect((await h.result).status).toBe("failed");
  h.close();
  expect(h.events.at(-1)).toEqual({
    type: "context_observation_rejected_v1",
    binding: observe.binding,
    code: "validation",
    challenge: observe.challenge,
  });
  expect(JSON.stringify(h.events)).not.toContain("private-canary");
});

test("inspection-only hooks advertise no capture or continuation capability", async () => {
  const f = fixture();
  const inspector = {
    ...f.facade,
    lifecycle: {
      restore: { ...f.facade.lifecycle.restore!, continuation: undefined },
    },
  };
  const h = harness(inspector);
  await loaded(h);
  expect(h.events[0].capabilities).toEqual([
    "context_restore_v2",
    "context_observe_v1",
  ]);
  h.send(continuation);
  expect((await h.result).status).toBe("failed");
  h.close();
  expect(f.calls).not.toContain("prepare");
  expect(f.calls).not.toContain("continue");
});
