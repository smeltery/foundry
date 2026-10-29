import { expect, test } from "bun:test";
import { Lifecycle } from "../src/lifecycle.ts";
import { decodeContext } from "../src/context_codec.ts";
import {
  fixture,
  capture,
  begin,
  payload,
  observe,
  continuation,
  exporting,
} from "./lifecycle_helpers.ts";

async function flush() {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

test("abort after dispatch but before hook microtask prevents every lifecycle hook", async () => {
  for (const target of [
    capture,
    begin,
    payload,
    observe,
    continuation,
    exporting,
  ]) {
    const f = fixture();
    const abort = new AbortController();
    let lifecycle!: Lifecycle;
    lifecycle = new Lifecycle(f.facade, {
      signal: abort.signal,
      async write() {},
      execute() {
        lifecycle.settling();
        lifecycle.settled();
      },
      async close() {
        lifecycle.stop();
        await f.facade.dispose();
      },
    });
    const dispatch = async (value: unknown) => {
      lifecycle.accept(decodeContext(value));
      await flush();
    };
    if (
      [payload, observe, continuation, exporting].includes(
        target as typeof payload,
      )
    ) {
      await dispatch(begin);
      if (target !== payload) {
        await dispatch(payload);
        if (target !== observe) {
          await dispatch(observe);
          if (target === exporting) await dispatch(continuation);
        }
      }
    }
    const before = [...f.calls];
    lifecycle.accept(decodeContext(target));
    abort.abort();
    lifecycle.stop();
    await f.facade.dispose();
    await flush();
    expect(f.calls).toEqual([...before, "dispose"]);
  }
});
