import { expect } from "bun:test";
import { runAdapter } from "../src/controller.ts";
import type { ContextFacade, NativeCandidate } from "../src/context.ts";
import { hash, restoreBinding } from "../src/context_validation.ts";
import { deferred, start } from "./helpers.ts";
export { deferred, start };
export const receipt = "01234567-89ab-4cde-8f01-23456789abcd";
export const digest = "ab".repeat(32);
export const format = {
  profile: "profile",
  format_version: "1",
  producer_build: "fixture",
};
export const scope = { restore_id: "restore", load_id: receipt };
export const body = Buffer.from("body");
export const descriptor = {
  capture_id: "prior",
  checkpoint_id: "checkpoint",
  record_digest: hash("record"),
  witness_digest: hash("witness"),
  format,
  configuration_digest: hash("config"),
  payload_length: body.length,
  payload_digest: hash(body),
  retention_binding: hash("retention"),
};
export const binding = restoreBinding(scope, descriptor);
export const begin = { type: "restore_begin_v2", scope, descriptor };
export const payload = {
  type: "restore_payload_v2",
  scope,
  begin_binding: binding,
  payload_length: body.length,
  payload_digest: hash(body),
  payload_base64: body.toString("base64"),
  receipt,
};
export const observe = {
  type: "context_observe_v1",
  binding: {
    scope,
    job_id: receipt,
    begin_binding: binding,
    profile_digest: digest,
  },
  challenge: receipt,
};
export const continuation = {
  type: "continue_context_v2",
  scope,
  continuation: {
    attempt_id: "attempt",
    continuation_id: "continuation",
    input_id: "message",
    input_sequence: 1,
    input_digest: hash("next"),
    input_bytes: 4,
    authorization_binding: digest,
  },
  text: "next",
  receipt,
};
export const capture = {
  type: "context_capture_begin",
  attempt_id: "attempt",
  capture_id: "capture",
  format_digest: digest,
  configuration_digest: digest,
  challenge: receipt,
};
export const exporting = {
  type: "context_export",
  attempt_id: "attempt",
  capture_id: "capture",
  format_digest: digest,
  cut_digest: digest,
  challenge: receipt,
};
export const release = {
  type: "context_release",
  attempt_id: "attempt",
  operation_id: "capture",
};
export const candidate: NativeCandidate = {
  format,
  coverage: { completed_turn_id: "terminal", inputs: [], results: [] },
  bytes: body,
};
export function fixture() {
  const calls: string[] = [];
  const facade: ContextFacade = {
    lifecycle: {
      capture: {
        async prepare() {
          calls.push("capture");
        },
        async export() {
          calls.push("export");
          return candidate;
        },
      },
      restore: {
        async begin() {
          calls.push("begin");
        },
        async load() {
          calls.push("load");
        },
        async observe() {
          calls.push("observe");
          return { instance_id: receipt, bytes: body };
        },
        continuation: {
          async prepare() {
            calls.push("prepare");
            return {
              async run(context) {
                calls.push("continue");
                await context.text("continued");
                return { outcome: "success", result: "done" };
              },
            };
          },
        },
      },
    },
    async run(_session, context) {
      calls.push("start");
      await context.text("fresh");
      return { outcome: "success", result: "done" };
    },
    async cancel() {
      calls.push("cancel");
    },
    async dispose() {
      calls.push("dispose");
    },
  };
  return { facade, calls };
}
export function harness(
  facade: ContextFacade,
  write?: (event: Record<string, unknown>) => Promise<void>,
  signal?: AbortSignal,
) {
  let input!: ReadableStreamDefaultController<Uint8Array>;
  let closed = false;
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      input = controller;
    },
    cancel() {
      closed = true;
    },
  });
  const events: Record<string, unknown>[] = [];
  const changed = new Set<() => void>();
  let iterations = 0;
  const source = {
    [Symbol.asyncIterator]() {
      expect(++iterations).toBe(1);
      return stream[Symbol.asyncIterator]();
    },
  };
  const result = runAdapter({
    input: source,
    facade,
    signal,
    shutdownTimeoutMs: 100,
    write: async (frame) => {
      const event = JSON.parse(Buffer.from(frame).toString());
      if (write) await write(event);
      events.push(event);
      for (const notify of changed) notify();
    },
  });
  return {
    result,
    events,
    raw(bytes: Uint8Array) {
      input.enqueue(bytes);
    },
    send(value: unknown) {
      input.enqueue(Buffer.from(JSON.stringify(value) + "\n"));
    },
    close() {
      if (!closed) {
        closed = true;
        input.close();
      }
    },
    async wait(type: string, count = 1) {
      if (events.filter((event) => event.type === type).length < count) {
        const ready = deferred<void>();
        const notify = () => {
          if (events.filter((event) => event.type === type).length >= count) {
            changed.delete(notify);
            ready.resolve();
          }
        };
        changed.add(notify);
        await ready.promise;
      }
      await new Promise((resolve) => setTimeout(resolve, 0));
    },
  };
}
export async function loaded(h: ReturnType<typeof harness>) {
  await h.wait("ready");
  h.send(begin);
  await h.wait("restore_prepared_v2");
  h.send(payload);
  await h.wait("context_restored_v2");
  h.send(observe);
  await h.wait("context_observed_v1");
}
