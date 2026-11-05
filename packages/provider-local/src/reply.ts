import { TransportFailure } from "./errors.ts";

/** Largest reply head read from an endpoint, per head. */
const MAX_HEAD_BYTES = 64 * 1024;
/** Largest reply body read from an endpoint, after any chunked decoding. */
export const MAX_REPLY_BYTES = 4 * 1024 * 1024;
/** Most informational (1xx) heads skipped before the final one. */
const MAX_INFORMATIONAL = 8;
/**
 * Hard bound on raw bytes read for one reply. A one-byte chunk takes six raw
 * bytes, so a full body in the smallest chunks still fits.
 */
const MAX_RAW_BYTES = MAX_HEAD_BYTES * (MAX_INFORMATIONAL + 2) + MAX_REPLY_BYTES * 6;
/** Longest chunk-size line, including any extensions. */
const MAX_CHUNK_LINE = 1024;

export interface Reply {
  status: number;
  headers: ReadonlyMap<string, string>;
  body: Buffer;
}

const CRLF = Buffer.from("\r\n");

function protocol(): TransportFailure {
  return new TransportFailure("protocol");
}

/**
 * Incremental, bounded HTTP/1.1 reply parser for one non-persistent exchange.
 * Raw bytes are consumed as they arrive, so work stays linear in the reply.
 */
export class ReplyReader {
  /** Raw bytes not yet consumed: a partial head or chunk framing. */
  private pending: Buffer = Buffer.alloc(0);
  private raw = 0;
  private informational = 0;
  private head: { status: number; headers: Map<string, string> } | undefined;
  private body: BodyDecoder | undefined;

  push(chunk: Buffer): Reply | undefined {
    this.raw += chunk.length;
    if (this.raw > MAX_RAW_BYTES) throw protocol();
    this.pending = this.pending.length ? Buffer.concat([this.pending, chunk]) : chunk;
    return this.advance(false);
  }

  end(): Reply {
    const reply = this.advance(true);
    if (!reply) throw protocol();
    return reply;
  }

  private advance(ended: boolean): Reply | undefined {
    while (!this.head) {
      const head = this.takeHead();
      if (!head) return undefined;
      // 101 switches protocols, which this client never asks for.
      if (head.status >= 100 && head.status < 200 && head.status !== 101) {
        if (++this.informational > MAX_INFORMATIONAL) throw protocol();
        continue;
      }
      this.head = head;
      // Only a 200 reply's body is ever read.
      if (head.status !== 200) return { ...head, body: Buffer.alloc(0) };
      this.body = decoderFor(head.headers);
    }
    if (this.head.status !== 200) return { ...this.head, body: Buffer.alloc(0) };
    const data = this.pending;
    this.pending = Buffer.alloc(0);
    const body = this.body!.push(data, ended);
    return body && { ...this.head, body };
  }

  private takeHead(): { status: number; headers: Map<string, string> } | undefined {
    const end = this.pending.indexOf("\r\n\r\n");
    if (end < 0) {
      if (this.pending.length > MAX_HEAD_BYTES) throw protocol();
      return undefined;
    }
    if (end > MAX_HEAD_BYTES) throw protocol();
    const head = parseHead(this.pending.subarray(0, end).toString("latin1"));
    this.pending = this.pending.subarray(end + 4);
    return head;
  }
}

function parseHead(text: string): { status: number; headers: Map<string, string> } {
  const [statusLine, ...lines] = text.split("\r\n");
  const status = statusLine?.match(/^HTTP\/1\.[01] (\d{3})(?: .*)?$/)?.[1];
  if (!status) throw protocol();
  const headers = new Map<string, string>();
  for (const line of lines) {
    const separator = line.indexOf(":");
    if (separator <= 0) throw protocol();
    const name = line.slice(0, separator).trim().toLowerCase();
    const value = line.slice(separator + 1).trim();
    const previous = headers.get(name);
    headers.set(name, previous === undefined ? value : `${previous}, ${value}`);
  }
  return { status: Number(status), headers };
}

/** Accepts body bytes; returns the whole body once it is complete. */
interface BodyDecoder {
  push(data: Buffer, ended: boolean): Buffer | undefined;
}

function decoderFor(headers: ReadonlyMap<string, string>): BodyDecoder {
  const encoding = headers.get("transfer-encoding");
  if (encoding !== undefined) {
    if (encoding.toLowerCase() !== "chunked") throw protocol();
    return new ChunkedBody();
  }
  const length = headers.get("content-length");
  if (length === undefined) return new BodyParts(MAX_REPLY_BYTES, true);
  if (!/^\d{1,10}$/.test(length) || Number(length) > MAX_REPLY_BYTES) throw protocol();
  return new BodyParts(Number(length), false);
}

/** A body framed by its length, or by the end of the connection. */
class BodyParts implements BodyDecoder {
  private parts: Buffer[] = [];
  private size = 0;

  constructor(
    private readonly length: number,
    private readonly untilClose: boolean,
  ) {}

  push(data: Buffer, ended: boolean): Buffer | undefined {
    this.parts.push(data);
    this.size += data.length;
    if (this.size > this.length && this.untilClose) throw protocol();
    if (!this.untilClose && this.size >= this.length) {
      return Buffer.concat(this.parts).subarray(0, this.length);
    }
    if (!ended) return undefined;
    if (!this.untilClose) throw protocol();
    return Buffer.concat(this.parts);
  }
}

/**
 * A chunked body, decoded as a small state machine over arriving bytes. It
 * works on offsets and copies data once, so tiny chunks stay cheap.
 */
class ChunkedBody implements BodyDecoder {
  private pending: Buffer = Buffer.alloc(0);
  private body: Buffer = Buffer.alloc(0);
  private size = 0;
  private remaining = 0;
  private phase: "size" | "data" | "data_end" | "trailers" = "size";

  push(data: Buffer, ended: boolean): Buffer | undefined {
    this.pending = this.pending.length ? Buffer.concat([this.pending, data]) : data;
    const [done, consumed] = this.decode();
    this.pending = this.pending.subarray(consumed);
    if (done) return this.body.subarray(0, this.size);
    if (ended) throw protocol();
    return undefined;
  }

  /** Decodes as far as the pending bytes allow; returns completion and bytes consumed. */
  private decode(): [boolean, number] {
    const pending = this.pending;
    let at = 0;
    for (;;) {
      if (this.phase === "data") {
        const take = Math.min(this.remaining, pending.length - at);
        this.append(pending.subarray(at, at + take));
        at += take;
        this.remaining -= take;
        if (this.remaining > 0) return [false, at];
        this.phase = "data_end";
      } else if (this.phase === "data_end") {
        if (pending.length - at < 2) return [false, at];
        if (pending[at] !== 13 || pending[at + 1] !== 10) throw protocol();
        at += 2;
        this.phase = "size";
      } else {
        const limit = this.phase === "size" ? MAX_CHUNK_LINE : MAX_HEAD_BYTES;
        const lineEnd = pending.indexOf(CRLF, at);
        if (lineEnd < 0 ? pending.length - at > limit : lineEnd - at > limit) throw protocol();
        if (lineEnd < 0) return [false, at];
        const line = pending.subarray(at, lineEnd);
        at = lineEnd + 2;
        if (this.phase === "trailers") {
          // Trailers, if any, end with an empty line.
          if (line.length === 0) return [true, at];
          continue;
        }
        this.remaining = chunkSize(line);
        if (this.size + this.remaining > MAX_REPLY_BYTES) throw protocol();
        this.phase = this.remaining === 0 ? "trailers" : "data";
      }
    }
  }

  private append(data: Buffer): void {
    const needed = this.size + data.length;
    if (needed > this.body.length) {
      const grown = Buffer.alloc(Math.min(MAX_REPLY_BYTES, Math.max(needed, this.body.length * 2, 4096)));
      this.body.copy(grown, 0, 0, this.size);
      this.body = grown;
    }
    data.copy(this.body, this.size);
    this.size = needed;
  }
}

/** Parses a chunk-size line: one to eight hex digits, then optional extensions. */
function chunkSize(line: Buffer): number {
  let value = 0;
  let digits = 0;
  for (const byte of line) {
    if (byte === 0x3b || byte === 0x20 || byte === 0x09) break;
    value = value * 16 + hexDigit(byte);
    digits += 1;
  }
  if (digits < 1 || digits > 8) throw protocol();
  return value;
}

function hexDigit(byte: number): number {
  if (byte >= 0x30 && byte <= 0x39) return byte - 0x30;
  const lower = byte | 0x20;
  if (lower >= 0x61 && lower <= 0x66) return lower - 0x57;
  throw protocol();
}
