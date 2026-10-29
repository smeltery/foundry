import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import {
  decodeContext,
  candidate as encodeCandidate,
  observed,
} from "../src/context_codec.ts";
import {
  restoreBinding,
  payload as decodePayload,
  MAX_CONTEXT_BYTES,
  hash,
} from "../src/context_validation.ts";
import { incoming } from "../src/adapter_input.ts";
import {
  fixture,
  harness,
  loaded,
  capture,
  exporting,
  release,
  start,
  begin,
  payload,
  observe,
  continuation,
  scope,
  descriptor,
  binding,
  candidate,
  body,
  receipt,
} from "./lifecycle_helpers.ts";
const golden: { binding: string; frames: string[] } = JSON.parse(
  readFileSync(new URL("./context-golden.json", import.meta.url), "utf8"),
);
const frame = (type: string) =>
  golden.frames.find((value) => JSON.parse(value).type === type)!;

test("context codecs match literal frames and binding from Rust contracts", () => {
  expect(restoreBinding(scope, descriptor)).toBe(golden.binding);
  expect(binding).toBe(
    "7afd8dd91e77b715cb58e082e51101957af30d768337579ca8beedb77f9cf103",
  );
  for (const value of [
    capture,
    exporting,
    release,
    begin,
    payload,
    observe,
    continuation,
  ]) {
    expect(JSON.stringify(value)).toBe(frame(value.type));
    expect<string>(decodeContext(JSON.parse(frame(value.type))).type).toBe(
      value.type,
    );
  }
  expect(Buffer.from(encodeCandidate(exporting, candidate)).toString()).toBe(
    frame("context_candidate") + "\n",
  );
  expect(
    Buffer.from(
      observed(observe, { instance_id: receipt, bytes: body }),
    ).toString(),
  ).toBe(frame("context_observed_v1") + "\n");
});

test("live controller responses match the Rust goldens without field-order drift", async () => {
  const f = fixture();
  const h = harness(f.facade);
  await loaded(h);
  h.send(continuation);
  await h.wait("settled");
  h.send(exporting);
  await h.wait("context_candidate");
  h.send(release);
  expect((await h.result).status).toBe("completed");
  h.close();
  for (const event of h.events.filter(
    (event) => !["ready", "text", "settled"].includes(event.type as string),
  )) {
    expect(JSON.stringify(event)).toBe(frame(event.type as string));
  }
  const fresh = fixture();
  const g = harness(fresh.facade);
  await g.wait("ready");
  g.send(capture);
  await g.wait("context_capture_ready");
  expect(JSON.stringify(g.events.at(-1))).toBe(frame("context_capture_ready"));
  g.send(start);
  await g.wait("settled");
  g.send(exporting);
  await g.wait("context_candidate");
  g.send(release);
  expect((await g.result).status).toBe("completed");
  g.close();
});

test("strict context input rejects duplicate fields, response direction and unsafe identities", async () => {
  const valid = JSON.stringify(begin);
  const cases = [
    valid.replace(
      '"type":"restore_begin_v2"',
      '"type":"start","type":"restore_begin_v2"',
    ),
    valid.replace(
      '"restore_id":"restore"',
      '"restore_id":"restore","restore_id":"restore"',
    ),
    valid.replace('"load_id":', '"extra":1,"load_id":'),
    frame("restore_prepared_v2"),
    frame("context_candidate"),
    JSON.stringify({
      ...continuation,
      continuation: {
        ...continuation.continuation,
        input_sequence: 9007199254740992,
      },
    }),
    JSON.stringify({
      ...continuation,
      continuation: { ...continuation.continuation, input_bytes: 5 },
    }),
    JSON.stringify({ ...capture, attempt_id: "\ud800" }),
    JSON.stringify({ ...capture, challenge: receipt.toUpperCase() }),
    JSON.stringify({ ...capture, format_digest: "AB".repeat(32) }),
    JSON.stringify({ ...payload, payload_base64: "Ym9keR==" }),
    JSON.stringify({ ...payload, payload_base64: "Ym9keQ==\n" }),
    JSON.stringify({ ...payload, payload_length: 3 }),
    JSON.stringify({ ...begin, descriptor: { ...descriptor, unknown: true } }),
  ];
  for (const text of cases) {
    const read = async () => {
      for await (const _item of incoming(
        (async function* () {
          yield Buffer.from(text + "\n");
        })(),
        true,
      )) {
      }
    };
    await expect(read()).rejects.toThrow();
  }
});

test("payload snapshots preserve maximum binary data and reject limits without truncation", () => {
  const data = Buffer.alloc(MAX_CONTEXT_BYTES, 255);
  const decoded = decodePayload(
    data.toString("base64"),
    data.length,
    hash(data),
  );
  expect(Buffer.from(decoded).equals(data)).toBe(true);
  expect(() =>
    decodePayload(
      Buffer.alloc(MAX_CONTEXT_BYTES + 1).toString("base64"),
      MAX_CONTEXT_BYTES + 1,
      hash(data),
    ),
  ).toThrow();
  expect(() => decodePayload("", 0, hash(""))).toThrow();
  const result = { ...candidate, bytes: data };
  const output = JSON.parse(
    Buffer.from(encodeCandidate(exporting, result)).toString(),
  );
  expect(output.payload_length).toBe(MAX_CONTEXT_BYTES);
  expect(output.payload_digest).toBe(hash(data));
  expect(() =>
    encodeCandidate(exporting, {
      ...candidate,
      bytes: Buffer.alloc(MAX_CONTEXT_BYTES + 1),
    }),
  ).toThrow();
  expect(() =>
    encodeCandidate(exporting, {
      ...candidate,
      coverage: {
        ...candidate.coverage,
        inputs: Array.from({ length: 1025 }, () => ({
          input_id: "id",
          input_digest: hash("input"),
          native_ids: ["native"],
        })),
      },
    }),
  ).toThrow();
});

test("bad late input and two coalesced operations cannot overtake a pending load", async () => {
  const f = fixture();
  let imports = 0;
  f.facade.lifecycle.restore!.load = async () => {
    imports++;
  };
  const h = harness(f.facade);
  await h.wait("ready");
  h.raw(
    Buffer.from(JSON.stringify(begin) + "\n" + JSON.stringify(payload) + "\n"),
  );
  expect((await h.result).status).toBe("failed");
  h.close();
  expect(imports).toBe(0);
});
