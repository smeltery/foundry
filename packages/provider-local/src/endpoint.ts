import { lookup } from "node:dns/promises";
import { isIP } from "node:net";
import { CHAT_PATH, parseReply, requestBody, type Completion, type CompletionRequest } from "./chat.ts";
import { EndpointError, TransportFailure, type FailureCategory } from "./errors.ts";
import { abortCause, postPinned } from "./http.ts";
import { admitAddresses, type Resolve } from "./network.ts";
import { MAX_REPLY_BYTES, type Reply } from "./reply.ts";
import { validateSettings, type EndpointSettings, type Target } from "./settings.ts";

/** Most messages one request carries. */
const MAX_MESSAGES = 1024;
/** Most tools one request offers. */
const MAX_TOOLS = 128;
/** Largest request body sent, matching the largest reply accepted. */
const MAX_REQUEST_BYTES = MAX_REPLY_BYTES;

/** The operating system resolver, which also reads the hosts file. */
const systemResolve: Resolve = async (host) =>
  (await lookup(host, { all: true, verbatim: true })).map((answer) => answer.address);

/**
 * A validated client for one custom model endpoint in the `chat` format.
 * Every call resolves the host once, admits the answers by network class and
 * connects only to them. Proxy variables are ignored and redirects are never
 * followed.
 */
export class LocalEndpoint {
  readonly #target: Target;
  readonly #resolve: Resolve;

  constructor(settings: EndpointSettings) {
    this.#target = validateSettings(settings);
    this.#resolve = settings.resolve ?? systemResolve;
    // A literal address is refused now rather than at the first call.
    if (isIP(this.#target.host)) {
      admitAddresses([this.#target.host], this.#target.https, this.#target.optIn);
    }
  }

  get providerId(): string {
    return this.#target.providerId;
  }

  get models(): string[] {
    return [...this.#target.models];
  }

  /**
   * Sends one completion. The configured timeout bounds the whole call; an
   * abort from `signal` is rethrown as the caller's abort, not as a failure.
   */
  async complete(request: CompletionRequest, signal?: AbortSignal): Promise<Completion> {
    const body = this.#encode(request);
    const timeout = AbortSignal.timeout(this.#target.timeoutMs);
    const combined = signal ? AbortSignal.any([signal, timeout]) : timeout;
    let reply: Reply;
    try {
      const addresses = await this.#admit(combined);
      reply = await postPinned({
        addresses,
        port: this.#target.port,
        https: this.#target.https,
        host: this.#target.host,
        authority: this.#target.authority,
        path: `${this.#target.basePath}${CHAT_PATH}`,
        headers: this.#headers(),
        body,
        signal: combined,
      });
    } catch (error) {
      signal?.throwIfAborted();
      if (error instanceof EndpointError) throw error;
      if (timeout.aborted) throw this.#failure("timeout");
      throw this.#failure(error instanceof TransportFailure ? error.category : "connect");
    }
    return this.#decode(reply);
  }

  #encode(request: CompletionRequest): Buffer {
    if (!this.#target.models.has(request.model)) throw EndpointError.unknownModel();
    if (request.messages.length < 1 || request.messages.length > MAX_MESSAGES) {
      throw EndpointError.invalid("messages");
    }
    if (!Number.isSafeInteger(request.maxOutputTokens) || request.maxOutputTokens <= 0) {
      throw EndpointError.invalid("max_output_tokens");
    }
    const tools = request.tools ?? [];
    const names = new Set(tools.map((tool) => tool.name.trim()));
    if (tools.length > MAX_TOOLS || names.size !== tools.length || names.has("")) {
      throw EndpointError.invalid("tools");
    }
    const body = Buffer.from(JSON.stringify(requestBody(request)));
    if (body.length > MAX_REQUEST_BYTES) throw EndpointError.invalid("messages");
    return body;
  }

  /** Resolves once within the call's deadline and admits every answer. */
  async #admit(signal: AbortSignal): Promise<string[]> {
    const { host, https, optIn } = this.#target;
    if (isIP(host)) return admitAddresses([host], https, optIn);
    let addresses: string[];
    try {
      addresses = await abortable(this.#resolve(host), signal);
    } catch (error) {
      if (signal.aborted) throw error;
      throw this.#failure("dns");
    }
    if (addresses.length === 0) throw this.#failure("dns");
    return admitAddresses(addresses, https, optIn);
  }

  #headers(): Record<string, string> {
    const headers: Record<string, string> = {
      accept: "application/json",
      "content-type": "application/json",
    };
    if (this.#target.key !== undefined) headers.authorization = `Bearer ${this.#target.key}`;
    return headers;
  }

  #decode(reply: Reply): Completion {
    if (reply.status !== 200) throw this.#failure("http", reply.status);
    const contentType = reply.headers.get("content-type")?.split(";", 1)[0]?.trim().toLowerCase();
    if (contentType !== "application/json" || reply.headers.has("content-encoding")) {
      throw this.#failure("protocol");
    }
    let parsed: unknown;
    try {
      parsed = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(reply.body));
    } catch {
      throw this.#failure("protocol");
    }
    const completion = parseReply(parsed);
    if (!completion) throw this.#failure("protocol");
    return completion;
  }

  #failure(category: FailureCategory, status?: number): EndpointError {
    return EndpointError.failure(this.#target.providerId, category, status);
  }
}

/** Settles with `promise`, or rejects first when the signal aborts. */
function abortable<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
  if (signal.aborted) return Promise.reject(abortCause(signal));
  return new Promise((resolve, reject) => {
    const abort = () => reject(abortCause(signal));
    signal.addEventListener("abort", abort, { once: true });
    promise.then(resolve, reject).finally(() => signal.removeEventListener("abort", abort));
  });
}
