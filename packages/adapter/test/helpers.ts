import type { Facade } from "../src/facade.ts";
import { runAdapter } from "../src/controller.ts";
import type { Command, Event, Session } from "../src/protocol.ts";

export const session: Session = {
  id: "session",
  prompt: "offline test",
  directory: "/tmp",
  model: null,
  lifecycle: "running",
  outcome: null,
  result: null,
  active_attempt_id: "attempt",
  version: 1,
};
export const start: Extract<Command, { type: "start" }> = { type: "start", version: 1, attempt_id: "attempt", session };

export function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

export function harness(facade?: Facade) {
  let input!: ReadableStreamDefaultController<Uint8Array>;
  let closed = false;
  const stream = new ReadableStream<Uint8Array>({
    start(value) {
      input = value;
    },
    cancel() {
      closed = true;
    },
  });
  const events: Event[] = [];
  const changed = new Set<() => void>();
  const result = runAdapter({
    facade,
    input: stream,
    shutdownTimeoutMs: 40,
    write: async (frame) => {
      events.push(JSON.parse(new TextDecoder().decode(frame)) as Event);
      for (const notify of changed) notify();
    },
  });
  const raw = (bytes: Uint8Array) => input.enqueue(bytes);
  const send = (value: unknown) => raw(new TextEncoder().encode(JSON.stringify(value) + "\n"));
  const wait = async (type: Event["type"], count = 1) => {
    const enough = () => events.filter((event) => event.type === type).length >= count;
    if (enough()) return;
    const next = deferred<void>();
    const notify = () => {
      if (enough()) {
        changed.delete(notify);
        next.resolve();
      }
    };
    changed.add(notify);
    await next.promise;
  };
  return {
    result,
    events,
    send,
    raw,
    wait,
    close: () => {
      if (!closed) {
        closed = true;
        input.close();
      }
    },
  };
}
