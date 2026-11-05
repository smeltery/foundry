import { afterEach, describe, expect, test } from "bun:test";
import { EndpointError } from "../src/errors.ts";
import { MAX_REPLY_BYTES } from "../src/http.ts";
import { ask, chat, endpointFor, KEY, leakFree, MODEL, reply, startRaw, startStub, type Stub } from "./stub.ts";

const open: Omit<Stub, "requests">[] = [];
afterEach(async () => {
  await Promise.all(open.splice(0).map((stub) => stub.close()));
});

async function stub(...steps: Parameters<typeof startStub>) {
  const started = await startStub(...steps);
  open.push(started);
  return started;
}

async function raw(accept: Parameters<typeof startRaw>[0]) {
  const started = await startRaw(accept);
  open.push(started);
  return started;
}

/** The failure category and status, after checking the error is redacted. */
async function failure(port: number, call: Promise<unknown>) {
  const error = await call.then(
    () => {
      throw new Error("expected a failure");
    },
    (caught) => caught,
  );
  expect(error).toBeInstanceOf(EndpointError);
  leakFree(error, port);
  return [error.errorClass, error.category, error.status];
}

describe("local endpoint calls", () => {
  test("a text completion round trips through the stub", async () => {
    const server = await stub(reply(200, chat("fixture reply")));
    const completion = await endpointFor(server.port).complete({ ...ask, messages: [
      { role: "system", content: "be brief" },
      { role: "user", content: "hello" },
    ] });
    expect(completion).toEqual({ text: "fixture reply", toolCalls: [], inputTokens: 7, outputTokens: 3 });
    const [request] = server.requests;
    expect(request!.method).toBe("POST");
    expect(request!.path).toBe("/v1/chat/completions");
    expect(request!.headers.authorization).toBe(`Bearer ${KEY}`);
    expect(request!.headers.host).toBe(`127.0.0.1:${server.port}`);
    expect(request!.body).toEqual({
      model: MODEL,
      stream: false,
      max_tokens: 64,
      messages: [
        { role: "system", content: "be brief" },
        { role: "user", content: "hello" },
      ],
    });
  });

  test("names resolve once per call and only vetted answers are dialed", async () => {
    const server = await stub(reply(200, chat("named")));
    const lookups: string[] = [];
    const endpoint = endpointFor(server.port, {
      baseUrl: `http://stub.test:${server.port}/v1/`,
      resolve: async (host) => (lookups.push(host), ["127.0.0.1"]),
    });
    expect((await endpoint.complete(ask)).text).toBe("named");
    expect(lookups).toEqual(["stub.test"]);
    expect(server.requests[0]!.headers.host).toBe(`stub.test:${server.port}`);
    expect(server.requests[0]!.path).toBe("/v1/chat/completions");

    const rebound = endpointFor(server.port, {
      baseUrl: `http://stub.test:${server.port}/v1`,
      resolve: async () => ["127.0.0.1", "10.0.0.7"],
    });
    expect((await rebound.complete(ask).catch((error) => error)).errorClass).toBe("endpoint_not_permitted");
    expect(server.requests).toHaveLength(1);
  });

  test("a chunked reply is decoded", async () => {
    const server = await stub((_request, response) => {
      response.writeHead(200, { "content-type": "application/json; charset=utf-8" });
      const body = JSON.stringify(chat("chunked reply"));
      response.write(body.slice(0, 10));
      response.end(body.slice(10));
    });
    expect((await endpointFor(server.port).complete(ask)).text).toBe("chunked reply");
  });

  test("dns failures are categorized", async () => {
    const endpoint = (resolve: (host: string) => Promise<string[]>) =>
      endpointFor(9, { baseUrl: "http://stub.test:9/v1", resolve });
    expect(await failure(9, endpoint(async () => []).complete(ask))).toEqual(["endpoint_failure", "dns", undefined]);
    const missing = endpoint(async () => {
      throw new Error("stub.test 127.0.0.1:9 not found");
    });
    expect(await failure(9, missing.complete(ask))).toEqual(["endpoint_failure", "dns", undefined]);
  });

  test("connection failures are categorized", async () => {
    const closed = await raw(() => {});
    const port = closed.port;
    await closed.close();
    open.splice(open.indexOf(closed), 1);
    expect(await failure(port, endpointFor(port).complete(ask))).toEqual(["endpoint_failure", "connect", undefined]);
  });

  test("one timeout bounds lookup and reply", async () => {
    const silent = await raw(() => {});
    const endpoint = endpointFor(silent.port, { timeoutMs: 50 });
    expect(await failure(silent.port, endpoint.complete(ask))).toEqual(["endpoint_failure", "timeout", undefined]);
    const slowLookup = endpointFor(silent.port, {
      baseUrl: `http://stub.test:${silent.port}/v1`,
      timeoutMs: 50,
      resolve: () => new Promise(() => {}),
    });
    expect(await failure(silent.port, slowLookup.complete(ask))).toEqual(["endpoint_failure", "timeout", undefined]);
  });

  test("a failed TLS handshake is categorized", async () => {
    const plain = await raw((socket) => socket.end("HTTP/1.1 200 OK\r\n\r\nthis is not a TLS server hello"));
    const endpoint = endpointFor(plain.port, { baseUrl: `https://127.0.0.1:${plain.port}/v1` });
    expect(await failure(plain.port, endpoint.complete(ask))).toEqual(["endpoint_failure", "tls", undefined]);
  });

  test("non-200 statuses fail with their number and redirects are never followed", async () => {
    const target = await stub(reply(200, chat("followed")));
    const server = await stub(
      reply(503, { error: `upstream ${KEY}` }),
      reply(302, "", { location: `http://127.0.0.1:${target.port}/v1/chat/completions` }),
    );
    const endpoint = endpointFor(server.port);
    expect(await failure(server.port, endpoint.complete(ask))).toEqual(["endpoint_failure", "http", 503]);
    expect(await failure(server.port, endpoint.complete(ask))).toEqual(["endpoint_failure", "http", 302]);
    expect(target.requests).toHaveLength(0);
  });

  test("oversized, encoded, non-JSON and incomplete replies are protocol failures", async () => {
    const badReplies = [
      reply(200, `"${"x".repeat(MAX_REPLY_BYTES)}"`),
      reply(200, chat("encoded"), { "content-encoding": "gzip" }),
      reply(200, chat("typed"), { "content-type": "text/plain" }),
      reply(200, "{not json"),
      reply(200, { choices: [] }),
      reply(200, chat("")),
      reply(200, chat(null, [{ id: "call-1", name: "read_note", input: [] }])),
      reply(200, { choices: [{ message: { content: null, tool_calls: [{ id: "c", function: { name: "n" } }] } }] }),
    ];
    for (const bad of badReplies) {
      const server = await stub(bad);
      expect(await failure(server.port, endpointFor(server.port).complete(ask))).toEqual([
        "endpoint_failure",
        "protocol",
        undefined,
      ]);
    }
    const truncated = await raw((socket) =>
      socket.end("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 50\r\n\r\n{}"),
    );
    expect(await failure(truncated.port, endpointFor(truncated.port).complete(ask))).toEqual([
      "endpoint_failure",
      "protocol",
      undefined,
    ]);
  });

  test("proxy variables are ignored", async () => {
    const proxy = await stub(reply(200, chat("via proxy")));
    const server = await stub(reply(200, chat("direct")));
    const names = ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"];
    const saved = names.map((name) => process.env[name]);
    for (const name of names) process.env[name] = `http://127.0.0.1:${proxy.port}`;
    try {
      const endpoint = endpointFor(server.port, {
        baseUrl: `http://stub.test:${server.port}/v1`,
        resolve: async () => ["127.0.0.1"],
      });
      expect((await endpoint.complete(ask)).text).toBe("direct");
    } finally {
      names.forEach((name, index) => {
        if (saved[index] === undefined) delete process.env[name];
        else process.env[name] = saved[index];
      });
    }
    expect(proxy.requests).toHaveLength(0);
  });

  test("a caller abort is rethrown as is, not as a failure", async () => {
    const silent = await raw(() => {});
    const controller = new AbortController();
    const call = endpointFor(silent.port).complete(ask, controller.signal);
    const cause = new Error("caller stopped");
    setTimeout(() => controller.abort(cause), 20);
    expect(await call.catch((error) => error)).toBe(cause);
  });
});
