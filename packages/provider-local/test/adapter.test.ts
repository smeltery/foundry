import { afterEach, describe, expect, test } from "bun:test";
import { harness, start } from "../../adapter/test/helpers.ts";
import { createLocalFacade } from "../src/facade.ts";
import { chat, endpointFor, KEY, MODEL, PROVIDER, reply, startRaw, startStub, type Stub } from "./stub.ts";

const open: Omit<Stub, "requests">[] = [];
afterEach(async () => {
  await Promise.all(open.splice(0).map((stub) => stub.close()));
});

const readNote = {
  name: "read_note",
  description: "Read a note from the session directory.",
  parameters: { type: "object", properties: { path: { type: "string" } }, required: ["path"] },
};

function startFor(model: { provider: string; model: string } | null = { provider: PROVIDER, model: MODEL }) {
  return { ...start, session: { ...start.session, prompt: "summarize the note", model: model && { ...model, effort: null } } };
}

describe("adapter driven by a local endpoint", () => {
  test("streams text, requests a tool through core and returns its result", async () => {
    const server = await startStub(
      reply(200, chat("Reading the note.", [{ id: "call-1", name: "read_note", input: { path: "notes.txt" } }])),
      reply(200, chat("The note says the build is green.")),
    );
    open.push(server);
    const facade = createLocalFacade({
      endpoint: endpointFor(server.port),
      system: "Answer from tool results only.",
      tools: [readNote],
    });
    const h = harness(facade);
    h.send(startFor());
    await h.wait("tool_call");
    h.send({ type: "tool_result", attempt_id: "attempt", call_id: "call-1", result: { text: "build is green" } });
    expect(await h.result).toEqual({ status: "completed" });
    h.close();

    expect(h.events.filter((event) => event.type !== "ready")).toEqual([
      { type: "text", attempt_id: "attempt", text: "Reading the note." },
      { type: "tool_call", attempt_id: "attempt", call_id: "call-1", name: "read_note", input: { path: "notes.txt" } },
      { type: "text", attempt_id: "attempt", text: "The note says the build is green." },
      // An endpoint reply is not proof the task is done, so success is never claimed.
      { type: "settled", attempt_id: "attempt", outcome: "action_required", result: "The note says the build is green." },
    ]);

    const [first, second] = server.requests;
    expect(first!.headers.authorization).toBe(`Bearer ${KEY}`);
    expect(first!.body.tools).toEqual([{ type: "function", function: readNote }]);
    expect(first!.body.messages).toEqual([
      { role: "system", content: "Answer from tool results only." },
      { role: "user", content: "summarize the note" },
    ]);
    expect(second!.body.messages).toEqual([
      { role: "system", content: "Answer from tool results only." },
      { role: "user", content: "summarize the note" },
      {
        role: "assistant",
        content: "Reading the note.",
        tool_calls: [
          { id: "call-1", type: "function", function: { name: "read_note", arguments: '{"path":"notes.txt"}' } },
        ],
      },
      { role: "tool", tool_call_id: "call-1", content: '{"text":"build is green"}' },
    ]);
  });

  test("a tool denial is passed back to the endpoint, not turned into success", async () => {
    const server = await startStub(
      reply(200, chat(null, [{ id: "call-9", name: "read_note", input: { path: "secret.txt" } }])),
      reply(200, chat("I could not read the note.")),
    );
    open.push(server);
    const h = harness(createLocalFacade({ endpoint: endpointFor(server.port), tools: [readNote] }));
    h.send(startFor());
    await h.wait("tool_call");
    h.send({ type: "tool_result", attempt_id: "attempt", call_id: "call-9", result: { denied: true } });
    expect(await h.result).toEqual({ status: "completed" });
    h.close();
    expect(h.events.filter((event) => event.type === "text")).toHaveLength(1);
    expect(h.events.at(-1)).toMatchObject({ type: "settled", outcome: "action_required" });
    expect(server.requests[1]!.body.messages).toContainEqual({
      role: "tool",
      tool_call_id: "call-9",
      content: '{"denied":true}',
    });
  });

  test("endpoint failures end the attempt without leaking the key, address or body", async () => {
    const server = await startStub(reply(500, { error: `failed for ${KEY}` }));
    open.push(server);
    const h = harness(createLocalFacade({ endpoint: endpointFor(server.port) }));
    h.send(startFor());
    expect(await h.result).toEqual({ status: "failed", code: "adapter_failed" });
    h.close();
    const output = JSON.stringify(h.events);
    for (const secret of [KEY, String(server.port), "127.0.0.1", "failed for"]) expect(output).not.toContain(secret);
  });

  test("a session for another provider or without a model never reaches the endpoint", async () => {
    const server = await startStub(reply(200, chat("unexpected")));
    open.push(server);
    for (const model of [null, { provider: "other-local", model: MODEL }, { provider: PROVIDER, model: "other" }]) {
      const h = harness(createLocalFacade({ endpoint: endpointFor(server.port) }));
      h.send(startFor(model));
      expect(await h.result).toEqual({ status: "failed", code: "adapter_failed" });
      h.close();
    }
    expect(server.requests).toHaveLength(0);
  });

  test("cancellation aborts a pending endpoint call", async () => {
    let connected!: () => void;
    const reached = new Promise<void>((resolve) => (connected = resolve));
    const silent = await startRaw(() => connected());
    open.push(silent);
    const h = harness(createLocalFacade({ endpoint: endpointFor(silent.port, { timeoutMs: 60_000 }) }));
    h.send(startFor());
    await reached;
    h.send({ type: "cancel", attempt_id: "attempt", mode: "hard" });
    expect(await h.result).toEqual({ status: "cancelled" });
    h.close();
  });

  test("the tool round limit stops a looping endpoint without claiming success", async () => {
    const server = await startStub(reply(200, chat(null, [{ id: "loop", name: "read_note", input: { path: "a" } }])));
    open.push(server);
    const h = harness(createLocalFacade({ endpoint: endpointFor(server.port), tools: [readNote], maxToolRounds: 0 }));
    h.send(startFor());
    expect(await h.result).toEqual({ status: "completed" });
    h.close();
    expect(h.events.at(-1)).toEqual({
      type: "settled",
      attempt_id: "attempt",
      outcome: "action_required",
      result: "tool round limit reached",
    });
    expect(h.events.some((event) => event.type === "tool_call")).toBe(false);
  });
});
