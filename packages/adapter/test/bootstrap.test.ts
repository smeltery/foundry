import { expect, test } from "bun:test";
import { runConfiguredAdapter, MAX_BOOTSTRAP_BODY_BYTES, type ConfiguredFacade } from "../src/bootstrap.ts";
import { deferred, start } from "./helpers.ts";

const selection = "ab".repeat(32);
const challenge = "b45dc104-890a-4c59-804b-9996a861cebe";
const secret = "private-bootstrap-canary";
function packet(body = Buffer.from(secret)) {
  return {
    type: "adapter_bootstrap", version: 1, selection_digest: selection,
    body_base64: body.toString("base64"), challenge,
  };
}
function line(value: unknown) { return Buffer.from(JSON.stringify(value) + "\n"); }

function configured(createFacade: (body: Uint8Array) => Promise<ConfiguredFacade>, failWrite = false) {
  let input!: ReadableStreamDefaultController<Uint8Array>;
  const stream = new ReadableStream<Uint8Array>({ start(controller) { input = controller; } });
  const output: Record<string, unknown>[] = [];
  const result = runConfiguredAdapter({
    input: stream, createFacade,
    write: async (frame) => {
      if (failWrite) throw new Error(secret);
      output.push(JSON.parse(Buffer.from(frame).toString()));
    },
  });
  return { input, output, result };
}

test("configuration precedes acknowledgement and Ready; coalesced Start uses the same reader", async () => {
  const entered = deferred<void>();
  const release = deferred<void>();
  let calls = 0;
  let disposed = 0;
  const h = configured(async (body) => {
    expect(Buffer.from(body).toString()).toBe(secret);
    entered.resolve();
    await release.promise;
    return {
      selectionDigest: selection,
      facade: {
        async run() { calls++; return { outcome: "success", result: "complete" }; },
        async dispose() { disposed++; },
      },
    };
  });
  h.input.enqueue(Buffer.concat([line(packet()), line(start)]));
  await entered.promise;
  expect(h.output).toEqual([]);
  expect(calls).toBe(0);
  release.resolve();
  expect(await h.result).toEqual({ status: "completed" });
  expect(h.output.map((frame) => frame.type)).toEqual(["adapter_configured", "ready", "settled"]);
  expect(h.output[0]).toEqual({ type: "adapter_configured", version: 1, selection_digest: selection, challenge });
  expect(JSON.stringify(h.output)).not.toContain(secret);
  expect(calls).toBe(1);
  expect(disposed).toBe(1);
  h.input.close();
});

test("private body boundary and split transport preserve exact bytes", async () => {
  const body = Buffer.alloc(MAX_BOOTSTRAP_BODY_BYTES, 255);
  const h = configured(async (actual) => {
    expect(Buffer.from(actual).equals(body)).toBe(true);
    return { selectionDigest: selection, facade: { async run() {} } };
  });
  const bytes = Buffer.concat([line(packet(body)), line(start)]);
  for (let offset = 0; offset < bytes.length; offset += 997) {
    h.input.enqueue(new Uint8Array());
    h.input.enqueue(bytes.subarray(offset, offset + 997));
  }
  expect(await h.result).toEqual({ status: "completed" });
  h.input.close();
});

test("invalid or noncanonical bootstrap fails without constructing or acknowledging", async () => {
  const valid = JSON.stringify(packet());
  const invalid = [
    line({ ...packet(), version: 2 }),
    line({ ...packet(), extra: secret }),
    line({ ...packet(), selection_digest: "bad" }),
    line({ ...packet(), challenge: "not-a-uuid" }),
    line({ ...packet(), body_base64: "Zg=" }),
    line(packet(Buffer.alloc(0))),
    line(packet(Buffer.alloc(MAX_BOOTSTRAP_BODY_BYTES + 1))),
    Buffer.from(valid.replace('"version":1', '"version":1,"version":1') + "\n"),
    Buffer.from(" " + valid + "\n"),
    Buffer.from("\ufeff" + valid + "\n"),
    Buffer.concat([Buffer.from([255]), Buffer.from("\n")]),
    Buffer.alloc(96 * 1024 + 1, 120),
    line({ type: "ready", version: 1, capabilities: [] }),
  ];
  for (const bytes of invalid) {
    let calls = 0;
    const h = configured(async () => { calls++; throw new Error(secret); });
    h.input.enqueue(bytes);
    expect(await h.result).toEqual({ status: "failed", code: "adapter_configuration_failed" });
    expect(calls).toBe(0);
    expect(h.output).toEqual([]);
    h.input.close();
  }
});

test("configuration mismatch and failed acknowledgement dispose without Ready or Start", async () => {
  for (const failWrite of [false, true]) {
    let disposed = 0;
    let ran = false;
    const h = configured(async () => ({
      selectionDigest: failWrite ? selection : "cd".repeat(32),
      facade: {
        async run() { ran = true; },
        async dispose() { disposed++; },
      },
    }), failWrite);
    h.input.enqueue(Buffer.concat([line(packet()), line(start)]));
    expect(await h.result).toEqual({ status: "failed", code: "adapter_configuration_failed" });
    expect(h.output).toEqual([]);
    expect(ran).toBe(false);
    expect(disposed).toBe(1);
    h.input.close();
  }
});

test("factory errors and incomplete bootstrap never expose private diagnostics", async () => {
  for (const complete of [false, true]) {
    const h = configured(async () => { throw new Error(secret); });
    h.input.enqueue(complete ? line(packet()) : Buffer.from('{"private":"' + secret));
    h.input.close();
    expect(await h.result).toEqual({ status: "failed", code: "adapter_configuration_failed" });
    expect(h.output).toEqual([]);
  }
});

test("owner stop at configuration boundaries prevents Ready and execution", async () => {
  for (const boundary of ["before_input", "after_input", "after_factory", "after_ack"]) {
    const stop = new AbortController();
    let factories = 0;
    let disposed = 0;
    let runs = 0;
    const output: string[] = [];
    if (boundary === "before_input") stop.abort();
    const result = await runConfiguredAdapter({
      signal: stop.signal,
      input: (async function* () {
        if (boundary === "after_input") stop.abort();
        yield Buffer.concat([line(packet()), line(start)]);
      })(),
      async createFacade() {
        factories++;
        if (boundary === "after_factory") stop.abort();
        return {
          selectionDigest: selection,
          facade: {
            async run() { runs++; },
            async dispose() { disposed++; },
          },
        };
      },
      async write(bytes) {
        output.push(JSON.parse(Buffer.from(bytes).toString()).type);
        if (boundary === "after_ack") stop.abort();
      },
    });
    expect(result).toEqual({ status: "failed", code: "adapter_configuration_failed" });
    expect(runs).toBe(0);
    expect(factories).toBe(boundary === "before_input" || boundary === "after_input" ? 0 : 1);
    expect(disposed).toBe(factories);
    expect(output).toEqual(boundary === "after_ack" ? ["adapter_configured"] : []);
  }
});
