import { createHash } from "node:crypto";
import { ProtocolError, MAX_FRAME_BYTES } from "./protocol.ts";
import type {
  ContextFormat,
  ContextCoverage,
  RestoreDescriptor,
  LoadingScope,
  ObservationBinding,
} from "./context.ts";
export const MAX_CONTEXT_BYTES = 1_048_576;
export function check(
  condition: unknown,
  code = "invalid_context",
): asserts condition {
  if (!condition) throw new ProtocolError(code);
}
export function record(
  value: unknown,
  keys: readonly string[],
): Record<string, unknown> {
  check(value !== null && typeof value === "object" && !Array.isArray(value));
  const object = value as Record<string, unknown>;
  check(
    Object.keys(object).length === keys.length &&
      keys.every((key) => Object.hasOwn(object, key)),
  );
  return object;
}
export function id(value: unknown): string {
  check(
    typeof value === "string" &&
      value.trim() &&
      Buffer.byteLength(value) <= 128 &&
      !/\p{Cc}|[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/u.test(
        value,
      ),
  );
  return value;
}
export function digest(value: unknown): string {
  check(typeof value === "string" && /^[0-9a-f]{64}$/.test(value));
  return value;
}
export function receipt(value: unknown): string {
  check(
    typeof value === "string" &&
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(
        value,
      ),
  );
  return value;
}
export function integer(
  value: unknown,
  maximum = Number.MAX_SAFE_INTEGER,
): number {
  check(
    Number.isSafeInteger(value) &&
      (value as number) > 0 &&
      (value as number) <= maximum,
    "context_limit",
  );
  return value as number;
}
export function hash(bytes: Uint8Array | string): string {
  return createHash("sha256").update(bytes).digest("hex");
}
export function bounded(value: unknown, limit: number): Buffer {
  const bytes = Buffer.from(JSON.stringify(value));
  check(bytes.length <= limit, "context_limit");
  return bytes;
}
export function scope(value: unknown): LoadingScope {
  const v = record(value, ["restore_id", "load_id"]);
  return Object.freeze({
    restore_id: id(v.restore_id),
    load_id: receipt(v.load_id),
  });
}
export function format(value: unknown): ContextFormat {
  const v = record(value, ["profile", "format_version", "producer_build"]);
  const result = {
    profile: id(v.profile),
    format_version: id(v.format_version),
    producer_build: id(v.producer_build),
  };
  bounded(result, 16_384);
  return Object.freeze(result);
}
export function descriptor(value: unknown): RestoreDescriptor {
  const v = record(value, [
    "capture_id",
    "checkpoint_id",
    "record_digest",
    "witness_digest",
    "format",
    "configuration_digest",
    "payload_length",
    "payload_digest",
    "retention_binding",
  ]);
  const result = {
    capture_id: id(v.capture_id),
    checkpoint_id: id(v.checkpoint_id),
    record_digest: digest(v.record_digest),
    witness_digest: digest(v.witness_digest),
    format: format(v.format),
    configuration_digest: digest(v.configuration_digest),
    payload_length: integer(v.payload_length, MAX_CONTEXT_BYTES),
    payload_digest: digest(v.payload_digest),
    retention_binding: digest(v.retention_binding),
  };
  bounded(result, 16_384);
  return Object.freeze(result);
}
export function observationBinding(value: unknown): ObservationBinding {
  const v = record(value, [
    "scope",
    "job_id",
    "begin_binding",
    "profile_digest",
  ]);
  return Object.freeze({
    scope: scope(v.scope),
    job_id: receipt(v.job_id),
    begin_binding: digest(v.begin_binding),
    profile_digest: digest(v.profile_digest),
  });
}
export function bytes(value: unknown): Uint8Array {
  check(value instanceof Uint8Array);
  check(
    value.byteLength > 0 && value.byteLength <= MAX_CONTEXT_BYTES,
    "context_limit",
  );
  return Uint8Array.from(value);
}
export function payload(
  encoded: unknown,
  length: unknown,
  expected: unknown,
): Uint8Array {
  check(
    typeof encoded === "string" && encoded.length <= 1_398_104,
    "context_limit",
  );
  const value = bytes(Buffer.from(encoded, "base64"));
  check(Buffer.from(value).toString("base64") === encoded);
  check(
    value.length === integer(length, MAX_CONTEXT_BYTES) &&
      hash(value) === digest(expected),
  );
  return value;
}
function ids(value: unknown): readonly string[] {
  check(
    Array.isArray(value) && value.length > 0 && value.length <= 1024,
    "context_limit",
  );
  return Object.freeze(value.map(id));
}
export function coverage(value: unknown): ContextCoverage {
  const v = record(value, ["completed_turn_id", "inputs", "results"]);
  check(
    Array.isArray(v.inputs) &&
      Array.isArray(v.results) &&
      v.inputs.length <= 1024 &&
      v.results.length <= 1024,
    "context_limit",
  );
  const result = {
    completed_turn_id: id(v.completed_turn_id),
    inputs: Object.freeze(
      v.inputs.map((item) => {
        const row = record(item, ["input_id", "input_digest", "native_ids"]);
        return Object.freeze({
          input_id: id(row.input_id),
          input_digest: digest(row.input_digest),
          native_ids: ids(row.native_ids),
        });
      }),
    ),
    results: Object.freeze(
      v.results.map((item) => {
        const row = record(item, [
          "attempt_id",
          "call_id",
          "result_digest",
          "native_ids",
        ]);
        return Object.freeze({
          attempt_id: id(row.attempt_id),
          call_id: id(row.call_id),
          result_digest: digest(row.result_digest),
          native_ids: ids(row.native_ids),
        });
      }),
    ),
  };
  bounded(result, 65_536);
  return Object.freeze(result);
}
export function restoreBinding(
  selected: LoadingScope,
  description: RestoreDescriptor,
): string {
  const pieces = [
    Buffer.from("foundry.restore.begin-wire.v2"),
    bounded(scope(selected), 16_384),
    bounded(descriptor(description), 16_384),
  ];
  return hash(
    Buffer.concat(
      pieces.flatMap((part) => {
        const size = Buffer.alloc(8);
        size.writeBigUInt64BE(BigInt(part.length));
        return [size, part];
      }),
    ),
  );
}
export function privateFrame(value: unknown): Uint8Array {
  return Buffer.concat([bounded(value, MAX_FRAME_BYTES), Buffer.from("\n")]);
}
