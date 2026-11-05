import { connect as connectTcp, isIP, type Socket } from "node:net";
import { connect as connectTls } from "node:tls";
import type { FailureCategory } from "./errors.ts";

/** Largest reply head read from an endpoint. */
const MAX_HEAD_BYTES = 64 * 1024;
/** Largest reply body read from an endpoint. */
export const MAX_REPLY_BYTES = 4 * 1024 * 1024;

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

export interface Reply {
  status: number;
  headers: ReadonlyMap<string, string>;
  body: Buffer;
}

/** A categorized transport failure. It keeps no error text, which can name the peer. */
export class TransportFailure {
  constructor(readonly category: FailureCategory) {}
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
    return await read(socket, request.signal);
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
  // Bun 1.3 can report an authorized handshake with no certificate when the
  // peer closes mid-handshake, so a verified certificate is required as well.
  if (!secure.authorized || !secure.getPeerCertificate()?.raw) {
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

function read(socket: Socket, signal: AbortSignal): Promise<Reply> {
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
    socket.on("error", () => reject(new TransportFailure("connect")));
    // A destroyed socket after abort emits close without end.
    socket.once("close", () => reject(signal.aborted ? abortCause(signal) : new TransportFailure("connect")));
  });
}

interface Head {
  status: number;
  headers: Map<string, string>;
  bodyStart: number;
}

/** Incremental, bounded HTTP/1.1 reply parser for one non-persistent exchange. */
class ReplyReader {
  private buffer = Buffer.alloc(0);
  private head: Head | undefined;

  push(chunk: Buffer): Reply | undefined {
    // Leaves room for chunk framing; the decoded body has its own bound.
    if (this.buffer.length + chunk.length > MAX_HEAD_BYTES + MAX_REPLY_BYTES * 2) {
      throw new TransportFailure("protocol");
    }
    this.buffer = Buffer.concat([this.buffer, chunk]);
    return this.complete(false);
  }

  end(): Reply {
    const reply = this.complete(true);
    if (!reply) throw new TransportFailure("protocol");
    return reply;
  }

  private complete(ended: boolean): Reply | undefined {
    this.head ??= parseHead(this.buffer);
    if (!this.head) return undefined;
    const { status, headers, bodyStart } = this.head;
    // Only a 200 reply's body is ever read.
    if (status !== 200) return { status, headers, body: Buffer.alloc(0) };
    const data = this.buffer.subarray(bodyStart);
    const body = headers.has("transfer-encoding")
      ? dechunk(data, headers.get("transfer-encoding")!)
      : headers.has("content-length")
        ? fixedLength(data, headers.get("content-length")!)
        : ended
          ? data
          : undefined;
    if (body && body.length > MAX_REPLY_BYTES) throw new TransportFailure("protocol");
    if (body) return { status, headers, body };
    if (ended) throw new TransportFailure("protocol");
    return undefined;
  }
}

function parseHead(buffer: Buffer): Head | undefined {
  const end = buffer.indexOf("\r\n\r\n");
  if (end < 0) {
    if (buffer.length > MAX_HEAD_BYTES) throw new TransportFailure("protocol");
    return undefined;
  }
  if (end > MAX_HEAD_BYTES) throw new TransportFailure("protocol");
  const [statusLine, ...lines] = buffer.subarray(0, end).toString("latin1").split("\r\n");
  const status = statusLine?.match(/^HTTP\/1\.[01] (\d{3})(?: .*)?$/)?.[1];
  if (!status) throw new TransportFailure("protocol");
  const headers = new Map<string, string>();
  for (const line of lines) {
    const separator = line.indexOf(":");
    if (separator <= 0) throw new TransportFailure("protocol");
    const name = line.slice(0, separator).trim().toLowerCase();
    const value = line.slice(separator + 1).trim();
    const previous = headers.get(name);
    headers.set(name, previous === undefined ? value : `${previous}, ${value}`);
  }
  return { status: Number(status), headers, bodyStart: end + 4 };
}

function fixedLength(data: Buffer, header: string): Buffer | undefined {
  if (!/^\d{1,10}$/.test(header) || Number(header) > MAX_REPLY_BYTES) {
    throw new TransportFailure("protocol");
  }
  const length = Number(header);
  return data.length >= length ? data.subarray(0, length) : undefined;
}

/** Decodes a chunked body; `undefined` until the final chunk has arrived. */
function dechunk(data: Buffer, encoding: string): Buffer | undefined {
  if (encoding.toLowerCase() !== "chunked") throw new TransportFailure("protocol");
  const parts: Buffer[] = [];
  let size = 0;
  let offset = 0;
  for (;;) {
    const lineEnd = data.indexOf("\r\n", offset);
    if (lineEnd < 0) return undefined;
    const length = data.subarray(offset, lineEnd).toString("latin1").split(";", 1)[0]!.trim();
    if (!/^[0-9a-fA-F]{1,8}$/.test(length)) throw new TransportFailure("protocol");
    const chunkSize = parseInt(length, 16);
    size += chunkSize;
    if (size > MAX_REPLY_BYTES) throw new TransportFailure("protocol");
    const start = lineEnd + 2;
    if (chunkSize === 0) {
      // Trailers, if any, end with an empty line.
      return data.indexOf("\r\n\r\n", lineEnd) >= 0 ? Buffer.concat(parts) : undefined;
    }
    if (data.length < start + chunkSize + 2) return undefined;
    if (!data.subarray(start + chunkSize, start + chunkSize + 2).equals(CRLF)) {
      throw new TransportFailure("protocol");
    }
    parts.push(data.subarray(start, start + chunkSize));
    offset = start + chunkSize + 2;
  }
}

const CRLF = Buffer.from("\r\n");
