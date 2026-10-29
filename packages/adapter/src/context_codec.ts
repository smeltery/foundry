import type {
  ContextCommand,
  ContinueRequest,
  NativeCandidate,
  NativeObservation,
  ExportRequest,
  ObserveRequest,
} from "./context.ts";
import * as v from "./context_validation.ts";

export function decodeContext(value: unknown): ContextCommand {
  v.check(value !== null && typeof value === "object");
  const tag = (value as { type?: unknown }).type;
  switch (tag) {
    case "context_capture_begin": {
      const f = v.record(value, [
        "type",
        "attempt_id",
        "capture_id",
        "format_digest",
        "configuration_digest",
        "challenge",
      ]);
      return {
        type: tag,
        request: Object.freeze({
          attempt_id: v.id(f.attempt_id),
          capture_id: v.id(f.capture_id),
          format_digest: v.digest(f.format_digest),
          configuration_digest: v.digest(f.configuration_digest),
          challenge: v.receipt(f.challenge),
        }),
      };
    }
    case "context_export": {
      const f = v.record(value, [
        "type",
        "attempt_id",
        "capture_id",
        "format_digest",
        "cut_digest",
        "challenge",
      ]);
      return {
        type: tag,
        request: Object.freeze({
          attempt_id: v.id(f.attempt_id),
          capture_id: v.id(f.capture_id),
          format_digest: v.digest(f.format_digest),
          cut_digest: v.digest(f.cut_digest),
          challenge: v.receipt(f.challenge),
        }),
      };
    }
    case "context_release": {
      const f = v.record(value, ["type", "attempt_id", "operation_id"]);
      return {
        type: tag,
        request: Object.freeze({
          attempt_id: v.id(f.attempt_id),
          operation_id: v.id(f.operation_id),
        }),
      };
    }
    case "context_failed": {
      const f = v.record(value, ["type", "attempt_id", "operation_id", "code"]);
      v.check(
        typeof f.code === "string" &&
          [
            "unsupported",
            "busy",
            "invalid_payload",
            "invalid_state",
            "limit",
            "validation",
            "ownership_lost",
            "cancelled",
          ].includes(f.code),
      );
      return {
        type: tag,
        request: Object.freeze({
          attempt_id: v.id(f.attempt_id),
          operation_id: v.id(f.operation_id),
          code: f.code,
        }),
      };
    }
    case "restore_begin_v2": {
      const f = v.record(value, ["type", "scope", "descriptor"]);
      return {
        type: tag,
        request: Object.freeze({
          scope: v.scope(f.scope),
          descriptor: v.descriptor(f.descriptor),
        }),
      };
    }
    case "restore_payload_v2": {
      const f = v.record(value, [
        "type",
        "scope",
        "begin_binding",
        "payload_length",
        "payload_digest",
        "payload_base64",
        "receipt",
      ]);
      return {
        type: tag,
        request: Object.freeze({
          scope: v.scope(f.scope),
          begin_binding: v.digest(f.begin_binding),
          payload_length: v.integer(f.payload_length, v.MAX_CONTEXT_BYTES),
          payload_digest: v.digest(f.payload_digest),
          receipt: v.receipt(f.receipt),
          bytes: v.payload(
            f.payload_base64,
            f.payload_length,
            f.payload_digest,
          ),
        }),
      };
    }
    case "context_observe_v1": {
      const f = v.record(value, ["type", "binding", "challenge"]);
      return {
        type: tag,
        request: Object.freeze({
          binding: v.observationBinding(f.binding),
          challenge: v.receipt(f.challenge),
        }),
      };
    }
    case "continue_context_v2": {
      const f = v.record(value, [
        "type",
        "scope",
        "continuation",
        "text",
        "receipt",
      ]);
      const c = v.record(f.continuation, [
        "attempt_id",
        "continuation_id",
        "input_id",
        "input_sequence",
        "input_digest",
        "input_bytes",
        "authorization_binding",
      ]);
      const continuation = Object.freeze({
        attempt_id: v.id(c.attempt_id),
        continuation_id: v.id(c.continuation_id),
        input_id: v.id(c.input_id),
        input_sequence: v.integer(c.input_sequence),
        input_digest: v.digest(c.input_digest),
        input_bytes: v.integer(c.input_bytes, 262_144),
        authorization_binding: v.digest(c.authorization_binding),
      });
      v.check(
        typeof f.text === "string" &&
          f.text.trim() &&
          !/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/u.test(
            f.text,
          ),
      );
      v.check(
        Buffer.byteLength(f.text) === continuation.input_bytes &&
          v.hash(f.text) === continuation.input_digest,
      );
      const request: ContinueRequest = Object.freeze({
        scope: v.scope(f.scope),
        continuation,
        text: f.text,
        receipt: v.receipt(f.receipt),
      });
      return { type: tag, request };
    }
    default:
      throw new Error("invalid_context");
  }
}
export function candidate(
  request: ExportRequest,
  result: NativeCandidate,
): Uint8Array {
  const bytes = v.bytes(result.bytes);
  return v.privateFrame({
    type: "context_candidate",
    attempt_id: request.attempt_id,
    capture_id: request.capture_id,
    challenge: request.challenge,
    format: v.format(result.format),
    coverage: v.coverage(result.coverage),
    payload_length: bytes.length,
    payload_digest: v.hash(bytes),
    payload_base64: Buffer.from(bytes).toString("base64"),
  });
}
export function observed(
  request: ObserveRequest,
  result: NativeObservation,
): Uint8Array {
  const bytes = v.bytes(result.bytes);
  return v.privateFrame({
    type: "context_observed_v1",
    binding: request.binding,
    instance_id: v.receipt(result.instance_id),
    payload_length: bytes.length,
    payload_digest: v.hash(bytes),
    payload_base64: Buffer.from(bytes).toString("base64"),
    challenge: request.challenge,
  });
}
