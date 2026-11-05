import type { Json } from "../../adapter/src/protocol.ts";

/**
 * The `chat` wire format, the local endpoint default. Field names are
 * provisional pending the naming decision. No other format is built here.
 */
export const CHAT_PATH = "/chat/completions";

/** A tool the endpoint may ask for. Core decides whether it ever runs. */
export interface ToolSpec {
  name: string;
  description: string;
  /** A JSON Schema object describing the tool input. */
  parameters: { [key: string]: Json };
}

export interface ToolCall {
  id: string;
  name: string;
  input: { [key: string]: Json };
}

export type Message =
  | { role: "system" | "user"; content: string }
  | { role: "assistant"; content: string; toolCalls: ToolCall[] }
  | { role: "tool"; callId: string; content: string };

/** One non-streaming completion for an explicitly listed model. */
export interface CompletionRequest {
  model: string;
  messages: Message[];
  maxOutputTokens: number;
  tools?: ToolSpec[];
}

/** The reply text, any requested tool calls and the reported token counts. */
export interface Completion {
  text: string;
  toolCalls: ToolCall[];
  inputTokens: number | null;
  outputTokens: number | null;
}

export function requestBody(request: CompletionRequest): Json {
  const body: { [key: string]: Json } = {
    model: request.model,
    stream: false,
    max_tokens: request.maxOutputTokens,
    messages: request.messages.map(wireMessage),
  };
  if (request.tools?.length) {
    body.tools = request.tools.map((tool) => ({
      type: "function",
      function: { name: tool.name, description: tool.description, parameters: tool.parameters },
    }));
  }
  return body;
}

function wireMessage(message: Message): Json {
  switch (message.role) {
    case "assistant":
      return {
        role: "assistant",
        content: message.content,
        ...(message.toolCalls.length && {
          tool_calls: message.toolCalls.map((call) => ({
            id: call.id,
            type: "function",
            function: { name: call.name, arguments: JSON.stringify(call.input) },
          })),
        }),
      };
    case "tool":
      return { role: "tool", tool_call_id: message.callId, content: message.content };
    default:
      return { role: message.role, content: message.content };
  }
}

/**
 * Extracts the reply. `undefined` when it has neither text nor a well-formed
 * tool call, or when any tool call is malformed.
 */
export function parseReply(reply: unknown): Completion | undefined {
  const message = field(reply, "choices", 0, "message");
  const content = field(message, "content");
  const calls = field(message, "tool_calls") ?? [];
  if ((content != null && typeof content !== "string") || !Array.isArray(calls)) {
    return undefined;
  }
  const toolCalls: ToolCall[] = [];
  for (const call of calls) {
    const parsed = toolCall(call);
    if (!parsed) return undefined;
    toolCalls.push(parsed);
  }
  const text = content ?? "";
  if (!text && toolCalls.length === 0) return undefined;
  return {
    text,
    toolCalls,
    inputTokens: count(field(reply, "usage", "prompt_tokens")),
    outputTokens: count(field(reply, "usage", "completion_tokens")),
  };
}

function toolCall(call: unknown): ToolCall | undefined {
  const id = field(call, "id");
  const name = field(call, "function", "name");
  const text = field(call, "function", "arguments");
  if (typeof id !== "string" || !id.trim() || typeof name !== "string" || !name.trim()) return undefined;
  if (typeof text !== "string") return undefined;
  let input: unknown;
  try {
    input = JSON.parse(text);
  } catch {
    return undefined;
  }
  if (input === null || typeof input !== "object" || Array.isArray(input)) return undefined;
  return { id, name, input: input as ToolCall["input"] };
}

/** Follows object keys and array indexes; `undefined` once a step is missing. */
function field(value: unknown, ...path: (string | number)[]): unknown {
  let current = value;
  for (const step of path) {
    if (current === null || typeof current !== "object") return undefined;
    current = (current as Record<string | number, unknown>)[step];
  }
  return current;
}

function count(value: unknown): number | null {
  return Number.isSafeInteger(value) && (value as number) >= 0 ? (value as number) : null;
}
