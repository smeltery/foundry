import type { Completion, Facade, RunContext } from "../../adapter/src/facade.ts";
import type { Session } from "../../adapter/src/protocol.ts";
import type { Message, ToolSpec } from "./chat.ts";
import type { LocalEndpoint } from "./endpoint.ts";
import { EndpointError } from "./errors.ts";

/** Upper bound on endpoint round trips that request tools in one session. */
const MAX_TOOL_ROUNDS = 64;
const DEFAULT_MAX_OUTPUT_TOKENS = 4096;

export interface LocalFacadeOptions {
  endpoint: LocalEndpoint;
  system?: string;
  /** Tools offered to the endpoint. Core decides whether any request runs. */
  tools?: ToolSpec[];
  maxOutputTokens?: number;
  /** Round trips that request tools before the session stops; at most 64. */
  maxToolRounds?: number;
}

/**
 * An adapter facade that drives one local endpoint without an SDK. Reply text
 * is streamed as deltas and requested tools go to core through the adapter;
 * their replies, including denials, are passed back to the endpoint verbatim.
 *
 * The facade never claims success: an endpoint reply is not evidence that the
 * task is done, so a finished session settles as `action_required` with the
 * final reply text. Endpoint errors are thrown and reported by the adapter
 * without their text.
 */
export function createLocalFacade(options: LocalFacadeOptions): Facade {
  const maxToolRounds = options.maxToolRounds ?? MAX_TOOL_ROUNDS;
  if (!Number.isSafeInteger(maxToolRounds) || maxToolRounds < 0 || maxToolRounds > MAX_TOOL_ROUNDS) {
    throw EndpointError.invalid("tools");
  }
  const maxOutputTokens = options.maxOutputTokens ?? DEFAULT_MAX_OUTPUT_TOKENS;
  return {
    run: (session, context) => run(options, maxToolRounds, maxOutputTokens, session, context),
  };
}

async function run(
  options: LocalFacadeOptions,
  maxToolRounds: number,
  maxOutputTokens: number,
  session: Readonly<Session>,
  context: RunContext,
): Promise<Completion> {
  const { endpoint } = options;
  if (!session.model) throw EndpointError.invalid("model");
  if (session.model.provider !== endpoint.providerId) throw EndpointError.unknownModel();
  const messages: Message[] = [
    ...(options.system ? [{ role: "system" as const, content: options.system }] : []),
    { role: "user", content: session.prompt },
  ];
  for (let round = 0; ; round += 1) {
    const reply = await endpoint.complete(
      { model: session.model.model, messages, maxOutputTokens, tools: options.tools },
      context.signal,
    );
    if (reply.text) await context.text(reply.text);
    if (reply.toolCalls.length === 0) return { outcome: "action_required", result: reply.text };
    if (round >= maxToolRounds) {
      return { outcome: "action_required", result: "tool round limit reached" };
    }
    messages.push({ role: "assistant", content: reply.text, toolCalls: reply.toolCalls });
    for (const call of reply.toolCalls) {
      const result = await context.tool(call.id, call.name, call.input);
      messages.push({ role: "tool", callId: call.id, content: JSON.stringify(result) });
    }
  }
}
