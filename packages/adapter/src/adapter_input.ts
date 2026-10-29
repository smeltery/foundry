import {
  frames,
  decodeJson,
  command,
  ProtocolError,
  type Command,
} from "./protocol.ts";
import { decodeContext } from "./context_codec.ts";
import type { ContextCommand } from "./context.ts";
export type Incoming =
  | { kind: "ordinary"; command: Command }
  | { kind: "context"; command: ContextCommand };
export async function* incoming(
  input: AsyncIterable<Uint8Array>,
  context: boolean,
): AsyncGenerator<Incoming> {
  for await (const bytes of frames(input)) {
    const value = decodeJson(bytes);
    const type =
      value && typeof value === "object"
        ? (value as { type?: unknown }).type
        : undefined;
    if (
      ["start", "message", "cancel", "tool_result"].includes(type as string)
    ) {
      yield { kind: "ordinary", command: command(value) };
    } else {
      if (!context) throw new ProtocolError("invalid_command");
      if (!Buffer.from(JSON.stringify(value)).equals(bytes))
        throw new ProtocolError("invalid_context_encoding");
      yield { kind: "context", command: decodeContext(value) };
    }
  }
}
