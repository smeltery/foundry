export const PROTOCOL_VERSION = 1;
export const MESSAGE_ACK_CAPABILITY = "message_ack_v2";
export const MAX_FRAME_BYTES = 2 * 1024 * 1024;
export type Json =
  null | boolean | number | string | Json[] | { [key: string]: Json };
export type Outcome = "success" | "failure" | "action_required";
export type CancelMode = "graceful" | "hard";

export interface Session {
  id: string;
  prompt: string;
  directory: string;
  model: { provider: string; model: string; effort: string | null } | null;
  lifecycle: "queued" | "running" | "exit" | "error" | "suspended";
  outcome: Outcome | null;
  result: string | null;
  active_attempt_id: string | null;
  version: number;
}

export type Command =
  | { type: "start"; version: number; attempt_id: string; session: Session }
  | {
      type: "message";
      attempt_id: string;
      message_id: string;
      text: string;
      receipt: string;
    }
  | { type: "cancel"; attempt_id: string; mode: CancelMode }
  | { type: "tool_result"; attempt_id: string; call_id: string; result: Json };

export type Event =
  | { type: "ready"; version: number; capabilities?: string[] }
  | {
      type: "message_accepted";
      attempt_id: string;
      message_id: string;
      receipt: string;
    }
  | { type: "text"; attempt_id: string; text: string }
  | {
      type: "tool_call";
      attempt_id: string;
      call_id: string;
      name: string;
      input: Json;
    }
  | { type: "settled"; attempt_id: string; outcome: Outcome; result: string }
  | { type: "failed"; attempt_id: string; code: string; message: string };

export class ProtocolError extends Error {
  constructor(public readonly code: string) {
    super(code);
  }
}

function object(value: unknown): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new ProtocolError("invalid_command");
  }
  return value as Record<string, unknown>;
}

function string(value: unknown, nonempty = true): string {
  if (typeof value !== "string" || (nonempty && !value.trim())) {
    throw new ProtocolError("invalid_command");
  }
  return value;
}

function nullableString(value: unknown): string | null {
  return value == null ? null : string(value, false);
}

function member<const T extends string>(
  value: unknown,
  choices: readonly T[],
): T {
  if (!choices.includes(value as T)) throw new ProtocolError("invalid_command");
  return value as T;
}

function integer(value: unknown): number {
  if (!Number.isSafeInteger(value) || (value as number) < 0) {
    throw new ProtocolError("unsafe_integer");
  }
  return value as number;
}

export function json(value: unknown, depth = 0): Json {
  if (depth > 64) throw new ProtocolError("json_depth_exceeded");
  if (value === null || typeof value === "string" || typeof value === "boolean")
    return value;
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (Array.isArray(value)) return value.map((item) => json(item, depth + 1));
  const record = object(value);
  return Object.fromEntries(
    Object.entries(record).map(([key, item]) => [key, json(item, depth + 1)]),
  );
}

function session(value: unknown): Session {
  const source = object(value);
  const prompt = string(source.prompt);
  if (new TextEncoder().encode(prompt).length > 256 * 1024) {
    throw new ProtocolError("prompt_too_large");
  }
  const model = source.model == null ? null : object(source.model);
  return {
    id: string(source.id),
    prompt,
    directory: string(source.directory),
    model: model && {
      provider: string(model.provider),
      model: string(model.model),
      effort: nullableString(model.effort),
    },
    lifecycle: member(source.lifecycle, [
      "queued",
      "running",
      "exit",
      "error",
      "suspended",
    ]),
    outcome:
      source.outcome == null
        ? null
        : member(source.outcome, ["success", "failure", "action_required"]),
    result: nullableString(source.result),
    active_attempt_id: nullableString(source.active_attempt_id),
    version: integer(source.version),
  };
}

export function command(value: unknown): Command {
  const source = object(value);
  const attempt_id = string(source.attempt_id);
  switch (source.type) {
    case "start":
      return {
        type: "start",
        attempt_id,
        version: integer(source.version),
        session: session(source.session),
      };
    case "message":
      return {
        type: "message",
        attempt_id,
        message_id: string(source.message_id),
        text: string(source.text, false),
        receipt: string(source.receipt),
      };
    case "cancel":
      return {
        type: "cancel",
        attempt_id,
        mode: member(source.mode, ["graceful", "hard"]),
      };
    case "tool_result":
      return {
        type: "tool_result",
        attempt_id,
        call_id: string(source.call_id),
        result: json(source.result),
      };
    default:
      throw new ProtocolError("invalid_command");
  }
}

export function encode(event: Event): Uint8Array {
  const bytes = new TextEncoder().encode(JSON.stringify(event));
  if (bytes.length > MAX_FRAME_BYTES)
    throw new ProtocolError("frame_too_large");
  const frame = new Uint8Array(bytes.length + 1);
  frame.set(bytes);
  frame[bytes.length] = 10;
  return frame;
}

/** One bounded frame reader shared by ordinary and context dispatch. */
export async function* frames(
  input: AsyncIterable<Uint8Array>,
): AsyncGenerator<Uint8Array> {
  const buffer = new Uint8Array(MAX_FRAME_BYTES);
  let length = 0;
  for await (const chunk of input) {
    if (!(chunk instanceof Uint8Array))
      throw new ProtocolError("invalid_input_chunk");
    let offset = 0;
    while (offset < chunk.length) {
      const newline = chunk.indexOf(10, offset);
      const end = newline < 0 ? chunk.length : newline;
      const size = end - offset;
      if (length + size > MAX_FRAME_BYTES)
        throw new ProtocolError("frame_too_large");
      buffer.set(chunk.subarray(offset, end), length);
      length += size;
      offset = end + (newline < 0 ? 0 : 1);
      if (newline >= 0) {
        yield buffer.slice(0, length);
        length = 0;
      }
    }
  }
  if (length) throw new ProtocolError("unterminated_frame");
}
export function decodeJson(bytes: Uint8Array): unknown {
  try {
    return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } catch {
    throw new ProtocolError("invalid_json_frame");
  }
}
export async function* commands(
  input: AsyncIterable<Uint8Array>,
): AsyncGenerator<Command> {
  for await (const bytes of frames(input)) yield command(decodeJson(bytes));
}
