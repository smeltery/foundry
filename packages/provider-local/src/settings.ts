import { EndpointError } from "./errors.ts";
import { OPT_INS, type OptIn, type Resolve } from "./network.ts";

/** Longest base URL an endpoint accepts. */
const MAX_URL_BYTES = 2048;
/** Most model IDs one endpoint lists. */
const MAX_MODELS = 256;
/** Upper bound on any configured call timeout. */
export const MAX_TIMEOUT_MS = 300_000;
export const DEFAULT_TIMEOUT_MS = 60_000;

/** How to reach one custom endpoint. Nothing is discovered from it. */
export interface EndpointSettings {
  /** Neutral Foundry label: lowercase letters, digits and hyphens. */
  providerId: string;
  baseUrl: string;
  /** Explicit model IDs; the endpoint is never asked for its models. */
  models: string[];
  /** Private address classes this endpoint may resolve to. */
  optIn?: OptIn[];
  /** Bounds each whole call: name lookup, connection, request and reply. */
  timeoutMs?: number;
  /** Write-only key, sent as a header and never included in errors. */
  key?: string;
  /** Defaults to the operating system resolver. */
  resolve?: Resolve;
}

/** Settings after validation, with the URL split into its connection parts. */
export interface Target {
  providerId: string;
  https: boolean;
  /** Bare host: a domain name or a literal address without brackets. */
  host: string;
  /** The Host header value, with the port when it is not the default. */
  authority: string;
  port: number;
  /** The base path without a trailing slash. */
  basePath: string;
  models: ReadonlySet<string>;
  optIn: readonly OptIn[];
  timeoutMs: number;
  key: string | undefined;
}

export function validateSettings(settings: EndpointSettings): Target {
  if (!/^[a-z][a-z0-9-]{0,63}$/.test(settings.providerId)) {
    throw EndpointError.invalid("provider_id");
  }
  const models = new Set(settings.models);
  if (
    models.size === 0 ||
    models.size !== settings.models.length ||
    models.size > MAX_MODELS ||
    !settings.models.every(isModelId)
  ) {
    throw EndpointError.invalid("models");
  }
  const optIn = settings.optIn ?? [];
  if (!optIn.every((choice) => OPT_INS.includes(choice))) throw EndpointError.invalid("opt_in");
  const timeoutMs = settings.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0 || timeoutMs > MAX_TIMEOUT_MS) {
    throw EndpointError.invalid("timeout");
  }
  const key = settings.key;
  // Visible ASCII only, so the key is always a valid header value.
  if (key !== undefined && !/^[\x21-\x7e]{1,4096}$/.test(key)) throw EndpointError.invalid("key");
  return { providerId: settings.providerId, models, optIn, timeoutMs, key, ...baseUrl(settings.baseUrl) };
}

function isModelId(value: string): boolean {
  return (
    typeof value === "string" &&
    value.trim() !== "" &&
    Buffer.byteLength(value) <= 256 &&
    !/\p{Cc}/u.test(value) &&
    value !== "auto"
  );
}

/**
 * An HTTP or HTTPS URL with a host and no user information, query, fragment,
 * whitespace, control characters or backslashes.
 */
function baseUrl(value: string): Pick<Target, "https" | "host" | "authority" | "port" | "basePath"> {
  const invalid = EndpointError.invalid("base_url");
  if (typeof value !== "string" || Buffer.byteLength(value) > MAX_URL_BYTES || /[\s\p{Cc}\\?#]/u.test(value)) {
    throw invalid;
  }
  // Checked on the raw text too, since parsing can drop an empty user part.
  const authority = value.split("://", 2)[1]?.split("/", 1)[0];
  if (!authority || authority.includes("@")) throw invalid;
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw invalid;
  }
  const https = url.protocol === "https:";
  if ((!https && url.protocol !== "http:") || url.username || url.password || !url.hostname || url.port === "0") {
    throw invalid;
  }
  return {
    https,
    // URL keeps brackets around a literal IPv6 host; sockets take it bare.
    host: url.hostname.replace(/^\[(.*)\]$/, "$1"),
    authority: url.host,
    port: url.port ? Number(url.port) : https ? 443 : 80,
    basePath: url.pathname.replace(/\/+$/, ""),
  };
}
