import type { CancelMode, Json, Outcome, Session } from "./protocol.ts";

export interface Completion {
  outcome: Outcome;
  result: string;
}

export interface RunContext {
  signal: AbortSignal;
  /** Append-only text deltas, with backpressure. */
  text(delta: string): Promise<void>;
  /** Request core execution; never execute a tool in the adapter. */
  tool(callId: string, name: string, input: Json): Promise<Json>;
}

/** Trusted, injected binding. No implementation or provider is installed here. */
export interface Facade {
  run(session: Readonly<Session>, context: RunContext): Promise<Completion | void>;
  steer?(messageId: string, text: string, signal: AbortSignal): Promise<void>;
  cancel?(mode: CancelMode): Promise<void>;
  dispose?(): Promise<void>;
}
