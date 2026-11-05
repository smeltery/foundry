import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { createServer as createTcpServer, type Server, type Socket } from "node:net";
import type { Json } from "../../adapter/src/protocol.ts";
import { LocalEndpoint } from "../src/endpoint.ts";
import type { EndpointSettings } from "../src/settings.ts";

/** Fixture names only; nothing here refers to a real service or model. */
export const PROVIDER = "stub-local";
export const MODEL = "fixture-model";
export const KEY = "fixture-key-0123456789";

export interface Recorded {
  method: string;
  path: string;
  headers: IncomingMessage["headers"];
  body: Record<string, Json>;
}

/** Answers one recorded request. Steps run in order; the last one repeats. */
export type Step = (request: Recorded, response: ServerResponse) => void;

export interface Stub {
  port: number;
  requests: Recorded[];
  close(): Promise<void>;
}

/** A deterministic HTTP stub on loopback, scripted per request. */
export async function startStub(...steps: Step[]): Promise<Stub> {
  const requests: Recorded[] = [];
  const server = createServer(async (incoming, response) => {
    const chunks: Buffer[] = [];
    for await (const chunk of incoming) chunks.push(chunk as Buffer);
    const recorded = {
      method: incoming.method ?? "",
      path: incoming.url ?? "",
      headers: incoming.headers,
      body: JSON.parse(Buffer.concat(chunks).toString("utf8")),
    };
    requests.push(recorded);
    steps[Math.min(requests.length, steps.length) - 1]!(recorded, response);
  });
  return { ...(await listen(server)), requests };
}

/** A loopback TCP server that hands each raw socket to `accept`. */
export async function startRaw(accept: (socket: Socket) => void): Promise<Omit<Stub, "requests">> {
  return listen(
    createTcpServer((socket) => {
      socket.on("error", () => {});
      accept(socket);
    }),
  );
}

async function listen(server: Server): Promise<Omit<Stub, "requests">> {
  const sockets = new Set<Socket>();
  server.on("connection", (socket: Socket) => {
    sockets.add(socket);
    socket.on("close", () => sockets.delete(socket));
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("stub has no port");
  return {
    port: address.port,
    close: async () => {
      for (const socket of sockets) socket.destroy();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    },
  };
}

export function reply(status: number, value: unknown, headers: Record<string, string> = {}): Step {
  return (_request, response) => {
    response.writeHead(status, { "content-type": "application/json", ...headers });
    response.end(typeof value === "string" ? value : JSON.stringify(value));
  };
}

export function chat(content: string | null, toolCalls: { id: string; name: string; input: Json }[] = []) {
  return {
    choices: [
      {
        message: {
          role: "assistant",
          content,
          ...(toolCalls.length && {
            tool_calls: toolCalls.map((call) => ({
              id: call.id,
              type: "function",
              function: { name: call.name, arguments: JSON.stringify(call.input) },
            })),
          }),
        },
      },
    ],
    usage: { prompt_tokens: 7, completion_tokens: 3 },
  };
}

/** An endpoint on the stub's loopback port with the loopback opt-in. */
export function endpointFor(port: number, settings: Partial<EndpointSettings> = {}): LocalEndpoint {
  return new LocalEndpoint({
    providerId: PROVIDER,
    baseUrl: `http://127.0.0.1:${port}/v1`,
    models: [MODEL],
    optIn: ["loopback"],
    timeoutMs: 2_000,
    key: KEY,
    ...settings,
  });
}

export const ask = { model: MODEL, messages: [{ role: "user" as const, content: "hello" }], maxOutputTokens: 64 };

/** Asserts that nothing secret or address-like leaked into an error. */
export function leakFree(error: unknown, port: number): string {
  const text = `${String(error)} ${JSON.stringify(error)}`;
  for (const secret of [KEY, String(port), "127.0.0.1", "/v1", "hello"]) {
    if (text.includes(secret)) throw new Error(`error text leaked ${secret}`);
  }
  return text;
}
