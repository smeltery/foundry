import { expect, test } from "bun:test";
import { commands, encode, MAX_FRAME_BYTES } from "../src/protocol.ts";
import { start } from "./helpers.ts";

async function parse(...chunks: Uint8Array[]) {
  const input = (async function* () {
    yield* chunks;
  })();
  const result = [];
  for await (const command of commands(input)) result.push(command);
  return result;
}

test("UTF-8 JSONL survives arbitrary byte boundaries and multiple frames", async () => {
  const value = { ...start, session: { ...start.session, prompt: "hello 🦀" } };
  const bytes = new TextEncoder().encode(
    JSON.stringify(value) + "\n" + JSON.stringify(value) + "\n",
  );
  const parsed = await parse(...Array.from(bytes, (byte) => new Uint8Array([byte])));
  expect(parsed).toEqual([value, value]);
});

test("2 MiB bound is inclusive and enforced before unbounded buffering", async () => {
  const message = { type: "message", attempt_id: "attempt", message_id: "m", text: "", receipt: "opaque-tail-challenge" };
  const overhead = new TextEncoder().encode(JSON.stringify(message)).length;
  const text = "x".repeat(MAX_FRAME_BYTES - overhead);
  const bytes = new TextEncoder().encode(JSON.stringify({ ...message, text }) + "\n");
  expect((await parse(bytes)).length).toBe(1);
  await expect(parse(new Uint8Array(MAX_FRAME_BYTES + 1).fill(120))).rejects.toThrow(
    "frame_too_large",
  );
});

test("invalid UTF-8, JSON and partial final lines are rejected", async () => {
  await expect(parse(new Uint8Array([255, 10]))).rejects.toThrow("invalid_json_frame");
  await expect(parse(new TextEncoder().encode("private-data\n"))).rejects.toThrow(
    "invalid_json_frame",
  );
  await expect(parse(new TextEncoder().encode(JSON.stringify(start)))).rejects.toThrow(
    "unterminated_frame",
  );
  await expect(
    parse(
      new TextEncoder().encode(
        '{"type":"tool_result","attempt_id":"a","call_id":"c","result":1e999}\n',
      ),
    ),
  ).rejects.toThrow();
});

test("bounded output counts UTF-8 and escaped JSON bytes", () => {
  expect(() =>
    encode({ type: "text", attempt_id: "a", text: "🦀".repeat(MAX_FRAME_BYTES / 4) }),
  ).toThrow("frame_too_large");
  expect(() =>
    encode({ type: "text", attempt_id: "a", text: "\n".repeat(MAX_FRAME_BYTES / 2) }),
  ).toThrow("frame_too_large");
});

test("executable is unconfigured even when credentials exist in caller environment", async () => {
  const child = Bun.spawn([process.execPath, new URL("../src/main.ts", import.meta.url).pathname], {
    stdin: "pipe",
    stdout: "pipe",
    stderr: "pipe",
    env: { PATH: process.env.PATH, EXAMPLE_API_TOKEN: "test-sentinel-not-a-real-key" },
  });
  child.stdin.write(JSON.stringify(start) + "\n");
  await child.stdin.flush();
  const stdout = await new Response(child.stdout).text();
  const stderr = await new Response(child.stderr).text();
  expect(await child.exited).toBe(1);
  const events = stdout
    .trim()
    .split("\n")
    .map((line) => JSON.parse(line));
  expect(events.map((event) => event.type)).toEqual(["ready", "failed"]);
  expect(events[1].code).toBe("adapter_not_configured");
  expect(stdout + stderr).not.toContain("test-sentinel-not-a-real-key");
  expect(stderr).toBe("");
});

test("message v2 requires a nonempty string receipt with no legacy fallback", async () => {
  const base = { type: "message", attempt_id: "attempt", message_id: "m", text: "instruction" };
  for (const receipt of [undefined, null, 7, true, [], {}, "", " \t\n"]) {
    const frame = new TextEncoder().encode(JSON.stringify({ ...base, receipt }) + "\n");
    await expect(parse(frame)).rejects.toThrow("invalid_command");
  }
});

test("message receipt is preserved exactly and serialized after text", async () => {
  const receipt = " opaque-tail-🦀 ";
  const message = { type: "message", attempt_id: "attempt", message_id: "m", text: "payload", receipt } as const;
  const parsed = await parse(new TextEncoder().encode(JSON.stringify(message) + "\n"));
  expect(parsed).toEqual([message]);
  expect(JSON.stringify(parsed[0]).endsWith(`"text":"payload","receipt":${JSON.stringify(receipt)}}`)).toBe(true);
});
