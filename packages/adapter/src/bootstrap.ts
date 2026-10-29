import type { Facade } from "./facade.ts";
import { runAdapter, type AdapterOptions, type AdapterResult } from "./controller.ts";

export const BOOTSTRAP_VERSION = 1;
export const MAX_BOOTSTRAP_BODY_BYTES = 65_536;
const MAX_PRELUDE_BYTES = 96 * 1024;
const DIGEST = /^[0-9a-f]{64}$/;
const CHALLENGE = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

export interface ConfiguredFacade {
  facade: Facade;
  /** Recomputed from the complete validated public selection, excluding secrets. */
  selectionDigest: string;
}

export interface ConfiguredAdapterOptions extends Omit<AdapterOptions, "facade"> {
  signal?: AbortSignal;
  /** Trusted concrete binding; validate all inputs before constructing resources.
   * On rejection, this factory owns cleanup of any resources it created.
   * Construction must not execute a provider request or tool.
   */
  createFacade(body: Uint8Array): Promise<ConfiguredFacade>;
}

interface Prelude {
  body: Uint8Array;
  selection: string;
  challenge: string;
  input: AsyncIterable<Uint8Array>;
}

function decode(bytes: Uint8Array): Omit<Prelude, "input"> {
  const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  const value: unknown = JSON.parse(text);
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error();
  const wire = value as Record<string, unknown>;
  if (
    wire.type !== "adapter_bootstrap" || wire.version !== BOOTSTRAP_VERSION ||
    typeof wire.selection_digest !== "string" || !DIGEST.test(wire.selection_digest) ||
    typeof wire.challenge !== "string" || !CHALLENGE.test(wire.challenge) ||
    typeof wire.body_base64 !== "string" || wire.body_base64.length > 87_384
  ) throw new Error();
  const canonical = Buffer.from(JSON.stringify({
    type: wire.type, version: wire.version, selection_digest: wire.selection_digest,
    body_base64: wire.body_base64, challenge: wire.challenge,
  }));
  // Exact encoding rejects duplicate/unknown fields, alternate ordering and BOMs.
  if (!canonical.equals(bytes)) throw new Error();
  const body = Buffer.from(wire.body_base64, "base64");
  if (
    body.length === 0 || body.length > MAX_BOOTSTRAP_BODY_BYTES ||
    body.toString("base64") !== wire.body_base64
  ) throw new Error();
  return { body, selection: wire.selection_digest, challenge: wire.challenge };
}

async function prelude(input: AsyncIterable<Uint8Array>): Promise<Prelude> {
  const iterator = input[Symbol.asyncIterator]();
  const parts: Uint8Array[] = [];
  let length = 0;
  for (;;) {
    const next = await iterator.next();
    if (next.done || !(next.value instanceof Uint8Array)) throw new Error();
    const chunk = next.value;
    if (!chunk.length) continue;
    const end = chunk.indexOf(10);
    const size = end < 0 ? chunk.length : end + 1;
    if (length + size > MAX_PRELUDE_BYTES) throw new Error();
    parts.push(chunk.subarray(0, end < 0 ? chunk.length : end));
    length += size;
    if (end < 0) continue;
    const decoded = decode(Buffer.concat(parts));
    const remaining = chunk.subarray(end + 1);
    return {
      ...decoded,
      input: (async function* () {
        if (remaining.length) yield remaining;
        for (;;) {
          const value = await iterator.next();
          if (value.done) return;
          yield value.value;
        }
      })(),
    };
  }
}

/** One private configuration exchange, then the existing controller in the same
 * process. The owning supervisor enforces the original readiness deadline and
 * process cleanup, including a blocked input, factory or acknowledgement write.
 * The caller closes its input transport after this resolves, as for runAdapter.
 * Acknowledgement conveys configuration correlation, never permission.
 */
export async function runConfiguredAdapter(options: ConfiguredAdapterOptions): Promise<AdapterResult> {
  let configured: ConfiguredFacade | undefined;
  let transferred = false;
  try {
    const timeout = options.shutdownTimeoutMs ?? 1000;
    if (!Number.isFinite(timeout) || timeout < 1 || timeout > 60_000) throw new Error();
    options.signal?.throwIfAborted();
    const bootstrap = await prelude(options.input);
    options.signal?.throwIfAborted();
    configured = await options.createFacade(bootstrap.body);
    bootstrap.body = new Uint8Array();
    options.signal?.throwIfAborted();
    if (!configured || typeof configured.facade?.run !== "function" ||
      configured.selectionDigest !== bootstrap.selection) throw new Error();
    await options.write(Buffer.from(JSON.stringify({
      type: "adapter_configured", version: BOOTSTRAP_VERSION,
      selection_digest: bootstrap.selection, challenge: bootstrap.challenge,
    }) + "\n"));
    options.signal?.throwIfAborted();
    transferred = true;
    return await runAdapter({ ...options, input: bootstrap.input, facade: configured.facade });
  } catch {
    if (configured && !transferred) {
      try { await configured.facade?.dispose?.(); } catch { /* Fixed diagnostics only. */ }
    }
    return { status: "failed", code: "adapter_configuration_failed" };
  }
}
