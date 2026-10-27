import { expect, test } from "bun:test";
import { deferred, harness, start } from "./helpers.ts";

test("acknowledges only accepted messages and replays receipts without redispatch", async () => {
  const running = deferred<void>();
  const called = deferred<void>();
  const accepted = deferred<void>();
  let dispatches = 0;
  const h = harness({
    run: async () => {
      running.resolve();
      await new Promise(() => {});
    },
    steer: async () => {
      dispatches++;
      called.resolve();
      await accepted.promise;
    },
  });
  await h.wait("ready");
  expect(h.events[0]).toEqual({ type: "ready", version: 1, capabilities: ["message_ack_v2"] });
  h.send(start);
  await running.promise;
  const message = { type: "message", attempt_id: "attempt", message_id: "m", text: "instruction", receipt: "opaque-tail-challenge" };
  h.send(message);
  h.send(message);
  await called.promise;
  expect(h.events.some((event) => event.type === "message_accepted")).toBe(false);
  accepted.resolve();
  await h.wait("message_accepted");
  expect(dispatches).toBe(1);
  h.send(message);
  await h.wait("message_accepted", 2);
  expect(dispatches).toBe(1);
  expect(h.events.at(-1)).toEqual({
    type: "message_accepted",
    attempt_id: "attempt",
    message_id: "m",
    receipt: "opaque-tail-challenge",
  });
  h.send({ ...message, text: "different instruction" });
  expect(await h.result).toEqual({ status: "failed", code: "message_conflict" });
  expect(dispatches).toBe(1);
  h.close();
});

test("failed or cancelled acceptance never emits a receipt", async () => {
  for (const cancel of [false, true]) {
    const running = deferred<void>();
    const called = deferred<void>();
    const accepted = deferred<void>();
    const h = harness({
      run: async () => {
        running.resolve();
        await new Promise(() => {});
      },
      steer: async () => {
        called.resolve();
        await accepted.promise;
      },
    });
    h.send(start);
    await running.promise;
    h.send({ type: "message", attempt_id: "attempt", message_id: "m", text: "instruction", receipt: "opaque-tail-challenge" });
    await called.promise;
    if (cancel) {
      h.send({ type: "cancel", attempt_id: "attempt", mode: "hard" });
      expect(await h.result).toEqual({ status: "cancelled" });
      accepted.resolve();
    } else {
      accepted.reject(new Error("private-error"));
      expect(await h.result).toEqual({ status: "failed", code: "steering_failed" });
    }
    expect(h.events.some((event) => event.type === "message_accepted")).toBe(false);
    h.close();
  }
});

test("facades without steering do not advertise message acknowledgment", async () => {
  const running = deferred<void>();
  const h = harness({
    run: async () => {
      running.resolve();
      await new Promise(() => {});
    },
  });
  await h.wait("ready");
  expect(h.events[0]).toEqual({ type: "ready", version: 1, capabilities: [] });
  h.send(start);
  await running.promise;
  h.send({ type: "message", attempt_id: "attempt", message_id: "m", text: "instruction", receipt: "opaque-tail-challenge" });
  expect(await h.result).toEqual({ status: "failed", code: "steering_unsupported" });
  h.close();
});

test("a reused message ID cannot change its delivery challenge", async () => {
  for (const alreadyAccepted of [false, true]) {
    const running = deferred<void>();
    const called = deferred<void>();
    const acceptance = deferred<void>();
    let dispatches = 0;
    const h = harness({
      run: async () => {
        running.resolve();
        await new Promise(() => {});
      },
      steer: async () => {
        dispatches++;
        called.resolve();
        await acceptance.promise;
      },
    });
    h.send(start);
    await running.promise;
    const message = { type: "message", attempt_id: "attempt", message_id: "m", text: "instruction", receipt: "original-tail" };
    h.send(message);
    await called.promise;
    if (alreadyAccepted) {
      acceptance.resolve();
      await h.wait("message_accepted");
    }
    h.send({ ...message, receipt: "different-tail" });
    expect(await h.result).toEqual({ status: "failed", code: "message_conflict" });
    acceptance.resolve();
    expect(dispatches).toBe(1);
    expect(h.events.filter((event) => event.type === "message_accepted").map((event) => event.receipt))
      .toEqual(alreadyAccepted ? ["original-tail"] : []);
    h.close();
  }
});

test("invalid receipt is rejected before facade steering or acknowledgment", async () => {
  for (const receipt of [undefined, null, 42, ""]) {
    const running = deferred<void>();
    let dispatches = 0;
    const h = harness({
      run: async () => {
        running.resolve();
        await new Promise(() => {});
      },
      steer: async () => { dispatches++; },
    });
    h.send(start);
    await running.promise;
    h.send({ type: "message", attempt_id: "attempt", message_id: "m", text: "instruction", receipt });
    expect(await h.result).toEqual({ status: "failed", code: "invalid_command" });
    expect(dispatches).toBe(0);
    expect(h.events.some((event) => event.type === "message_accepted")).toBe(false);
    h.close();
  }
});
