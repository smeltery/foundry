import { describe, expect, test } from "bun:test";
import { TransportFailure } from "../src/errors.ts";
import { isTlsError } from "../src/http.ts";
import { MAX_REPLY_BYTES, ReplyReader } from "../src/reply.ts";

const OK = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n";

/** Feeds the reply in pieces of `size` bytes, then ends it. */
function read(raw: Buffer | string, size = raw.length) {
  const bytes = Buffer.from(raw);
  const reader = new ReplyReader();
  for (let offset = 0; offset < bytes.length; offset += size) {
    const reply = reader.push(bytes.subarray(offset, offset + size));
    if (reply) return reply;
  }
  return reader.end();
}

function failure(run: () => unknown): string {
  try {
    run();
  } catch (error) {
    if (error instanceof TransportFailure) return error.category;
    throw error;
  }
  throw new Error("expected a failure");
}

/** A chunked body that sends every byte of `body` as its own chunk. */
function oneByteChunks(body: Buffer): Buffer {
  const framed = Buffer.alloc(body.length * 6 + 5);
  let offset = 0;
  for (const byte of body) {
    framed.set([0x31, 13, 10, byte, 13, 10], offset);
    offset += 6;
  }
  framed.write("0\r\n\r\n", offset, "latin1");
  return framed;
}

describe("reply parsing", () => {
  test("a full-size body in one-byte chunks is accepted, and one byte more is not", () => {
    const body = Buffer.alloc(MAX_REPLY_BYTES, "a");
    const head = Buffer.from(`${OK}transfer-encoding: chunked\r\n\r\n`);
    const reply = read(Buffer.concat([head, oneByteChunks(body)]), 64 * 1024);
    expect(reply.status).toBe(200);
    expect(reply.body.equals(body)).toBe(true);
    const over = Buffer.concat([head, oneByteChunks(Buffer.alloc(MAX_REPLY_BYTES + 1, "a"))]);
    expect(failure(() => read(over, 64 * 1024))).toBe("protocol");
  });

  test("chunk framing split anywhere decodes the same body", () => {
    const raw = `${OK}transfer-encoding: chunked\r\n\r\n5;ext=1\r\nhello\r\n1\r\n \r\n5\r\nworld\r\n0\r\nx-trailer: 1\r\n\r\n`;
    for (const size of [1, 2, 3, 7, raw.length]) {
      expect(read(raw, size).body.toString()).toBe("hello world");
    }
  });

  test("malformed or truncated framing is a protocol failure", () => {
    const chunked = `${OK}transfer-encoding: chunked\r\n\r\n`;
    for (const raw of [
      `${chunked}zz\r\nhello\r\n0\r\n\r\n`,
      `${chunked}5\r\nhelloXX0\r\n\r\n`,
      `${chunked}5\r\nhel`,
      `${chunked}${"1".repeat(2048)}`,
      `${OK}transfer-encoding: gzip\r\n\r\n`,
      `${OK}content-length: 10\r\n\r\nshort`,
      `${OK}content-length: ${MAX_REPLY_BYTES + 1}\r\n\r\n`,
      "HTTP/2 200\r\n\r\n",
      `${OK}no separator\r\n\r\n`,
      "HTTP/1.1 200 OK\r\n",
    ]) {
      expect(failure(() => read(raw))).toBe("protocol");
    }
  });

  test("informational heads are skipped before the final status", () => {
    const raw = `HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\nlink: </a>\r\n\r\n${OK}content-length: 2\r\n\r\n{}`;
    for (const size of [1, 5, raw.length]) {
      const reply = read(raw, size);
      expect([reply.status, reply.body.toString()]).toEqual([200, "{}"]);
    }
    expect(read("HTTP/1.1 101 Switching Protocols\r\nupgrade: x\r\n\r\n").status).toBe(101);
    expect(failure(() => read("HTTP/1.1 100 Continue\r\n\r\n".repeat(9)))).toBe("protocol");
  });

  test("a non-200 status is returned without reading its body", () => {
    const reply = read("HTTP/1.1 503 Busy\r\ncontent-length: 9999\r\n\r\npartial");
    expect([reply.status, reply.body.length]).toEqual([503, 0]);
  });

  test("socket errors are judged TLS by their code only", () => {
    for (const code of ["ERR_SSL_DECRYPTION_FAILED_OR_BAD_RECORD_MAC", "ERR_TLS_CERT_ALTNAME_INVALID", "ERR_OSSL_BAD"]) {
      expect(isTlsError(Object.assign(new Error("x"), { code }))).toBe(true);
    }
    for (const error of [Object.assign(new Error("x"), { code: "ECONNRESET" }), new Error("ssl"), null, "ERR_SSL_X"]) {
      expect(isTlsError(error)).toBe(false);
    }
  });
});
