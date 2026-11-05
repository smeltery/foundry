import { connect as connectTcp, isIP, type Socket } from "node:net";
import { checkServerIdentity, connect as connectTls } from "node:tls";
import { TransportFailure } from "./errors.ts";
import { ReplyReader, type Reply } from "./reply.ts";

/** One POST to vetted addresses only. The host name is never resolved again. */
export interface PinnedPost {
  addresses: readonly string[];
  port: number;
  https: boolean;
  host: string;
  authority: string;
  path: string;
  headers: Readonly<Record<string, string>>;
  body: Buffer;
  signal: AbortSignal;
}

/**
 * Sends one HTTP/1.1 request over a socket this module opens itself, so no
 * proxy variable, agent pool, redirect or content decoding takes part.
 */
export async function postPinned(request: PinnedPost): Promise<Reply> {
  const socket = await open(request);
  const abort = () => socket.destroy();
  request.signal.addEventListener("abort", abort, { once: true });
  try {
    const head = [
      `POST ${request.path} HTTP/1.1`,
      `host: ${request.authority}`,
      ...Object.entries(request.headers).map(([name, value]) => `${name}: ${value}`),
      `content-length: ${request.body.length}`,
      "connection: close",
    ];
    socket.write(`${head.join("\r\n")}\r\n\r\n`);
    socket.write(request.body);
    return await read(socket, request.https, request.signal);
  } finally {
    request.signal.removeEventListener("abort", abort);
    socket.destroy();
  }
}

async function open(request: PinnedPost): Promise<Socket> {
  let tcp: Socket | undefined;
  for (const address of request.addresses) {
    const candidate = connectTcp({ host: address, port: request.port });
    try {
      await settle(candidate, "connect", request.signal);
      tcp = candidate;
      break;
    } catch (error) {
      candidate.destroy();
      if (request.signal.aborted) throw error;
    }
  }
  if (!tcp) throw new TransportFailure("connect");
  if (!request.https) return tcp;
  const secure = connectTls({
    socket: tcp,
    host: request.host,
    // SNI takes names only; a literal address is checked against the certificate's IP entries.
    servername: isIP(request.host) ? undefined : request.host,
    ALPNProtocols: ["http/1.1"],
  });
  try {
    await settle(secure, "secureConnect", request.signal);
  } catch (error) {
    secure.destroy();
    throw request.signal.aborted ? error : new TransportFailure("tls");
  }
  // The chain and the host identity are checked here as well, rather than
  // trusted to the runtime: Bun 1.3 can report an authorized handshake with
  // no certificate when the peer closes mid-handshake, and skips the identity
  // check for a literal address.
  const certificate = secure.getPeerCertificate();
  if (!secure.authorized || !certificate?.raw || checkServerIdentity(request.host, certificate)) {
    secure.destroy();
    throw new TransportFailure("tls");
  }
  return secure;
}

/** What `throwIfAborted` throws for an aborted signal. */
export function abortCause(signal: AbortSignal): unknown {
  try {
    signal.throwIfAborted();
  } catch (error) {
    return error;
  }
  return undefined;
}

/** Waits for one event, rejecting on a socket error or on abort. */
function settle(socket: Socket, event: string, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const cleanup = () => {
      socket.off(event, done);
      socket.off("error", fail);
      signal.removeEventListener("abort", abort);
    };
    const done = () => {
      cleanup();
      resolve();
    };
    const fail = (error: unknown) => {
      cleanup();
      reject(error);
    };
    const abort = () => fail(abortCause(signal));
    if (signal.aborted) return abort();
    socket.once(event, done);
    socket.once("error", fail);
    signal.addEventListener("abort", abort, { once: true });
  });
}

function read(socket: Socket, https: boolean, signal: AbortSignal): Promise<Reply> {
  return new Promise((resolve, reject) => {
    const reader = new ReplyReader();
    const finish = (step: () => Reply | undefined) => {
      try {
        const reply = step();
        if (reply) resolve(reply);
      } catch (error) {
        reject(error);
      }
    };
    socket.on("data", (chunk: Buffer) => finish(() => reader.push(chunk)));
    socket.once("end", () => finish(() => reader.end()));
    socket.on("error", (error) => reject(new TransportFailure(https && isTlsError(error) ? "tls" : "connect")));
    // A destroyed socket after abort emits close without end.
    socket.once("close", () => reject(signal.aborted ? abortCause(signal) : new TransportFailure("connect")));
  });
}

/** Whether a socket error came from the TLS layer, judged by its code only. */
export function isTlsError(error: unknown): boolean {
  const code = (error as { code?: unknown } | null)?.code;
  return typeof code === "string" && /^(ERR_SSL_|ERR_TLS_|ERR_OSSL_|SSL_)/.test(code);
}
