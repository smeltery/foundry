import { describe, expect, test } from "bun:test";
import { EndpointError } from "../src/errors.ts";
import { LocalEndpoint } from "../src/endpoint.ts";
import type { EndpointSettings } from "../src/settings.ts";
import { ask, endpointFor, KEY, MODEL, PROVIDER } from "./stub.ts";

const valid: EndpointSettings = {
  providerId: PROVIDER,
  baseUrl: "http://127.0.0.1:9/v1",
  models: [MODEL],
  optIn: ["loopback"],
};

function rejected(settings: Partial<EndpointSettings>) {
  try {
    new LocalEndpoint({ ...valid, ...settings });
  } catch (error) {
    if (!(error instanceof EndpointError)) throw error;
    expect(String(error)).not.toContain(KEY);
    return [error.errorClass, error.field];
  }
  throw new Error("expected a refusal");
}

describe("endpoint settings", () => {
  test("base URLs with credentials, queries, fragments or odd characters are refused", () => {
    for (const baseUrl of [
      `http://user:${KEY}@127.0.0.1:9/v1`,
      "http://@127.0.0.1:9/v1",
      "http://127.0.0.1:9/v1?mode=a",
      "http://127.0.0.1:9/v1#part",
      "http://127.0.0.1:9/v 1",
      "http://127.0.0.1:9/v1\n",
      "http://127.0.0.1:9\\v1",
      "ftp://127.0.0.1/v1",
      "http://127.0.0.1:0/v1",
      "127.0.0.1:9/v1",
      `http://127.0.0.1/${"a".repeat(2048)}`,
    ]) {
      expect(rejected({ baseUrl })).toEqual(["invalid_request", "base_url"]);
    }
  });

  test("models are explicit, unique and never the literal auto", () => {
    for (const models of [[], [MODEL, MODEL], ["auto"], [" "], ["bad\u0007id"]]) {
      expect(rejected({ models })).toEqual(["invalid_request", "models"]);
    }
  });

  test("provider IDs, opt-ins, timeouts and keys are bounded", () => {
    expect(rejected({ providerId: "Stub" })).toEqual(["invalid_request", "provider_id"]);
    expect(rejected({ optIn: ["public" as "lan"] })).toEqual(["invalid_request", "opt_in"]);
    for (const timeoutMs of [0, -1, 1.5, 300_001]) {
      expect(rejected({ timeoutMs })).toEqual(["invalid_request", "timeout"]);
    }
    for (const key of ["", "has space", `${KEY}\n`, "x".repeat(4097)]) {
      expect(rejected({ key })).toEqual(["invalid_request", "key"]);
    }
  });

  test("requests are validated before any connection", async () => {
    // Port 9 has no listener; validation must fail first.
    const endpoint = endpointFor(9);
    const refused = async (request: typeof ask) => {
      const error = await endpoint.complete(request).catch((caught) => caught);
      return [error.errorClass, error.field];
    };
    expect(await refused({ ...ask, model: "other-model" })).toEqual(["not_found", undefined]);
    expect(await refused({ ...ask, messages: [] })).toEqual(["invalid_request", "messages"]);
    expect(await refused({ ...ask, maxOutputTokens: 0 })).toEqual(["invalid_request", "max_output_tokens"]);
    const tool = { name: "read_note", description: "", parameters: {} };
    expect(await refused({ ...ask, tools: [tool, tool] } as typeof ask)).toEqual(["invalid_request", "tools"]);
    const huge = [{ role: "user" as const, content: "x".repeat(4 * 1024 * 1024) }];
    expect(await refused({ ...ask, messages: huge })).toEqual(["invalid_request", "messages"]);
  });
});
