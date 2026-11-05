/** The setting or request field a refusal is about, never its value. */
export type InvalidField =
  | "provider_id"
  | "base_url"
  | "models"
  | "opt_in"
  | "timeout"
  | "key"
  | "model"
  | "messages"
  | "max_output_tokens"
  | "tools";

/** Why an endpoint call failed. Names are provisional wire names. */
export type FailureCategory =
  | "dns"
  | "connect"
  | "timeout"
  | "tls"
  /** A status other than 200, including a redirect, which is never followed. */
  | "http"
  /** A 200 reply that is oversized, encoded, not JSON or lacks the format's text. */
  | "protocol";

/** Provisional neutral error classes from the providers specification. */
export type ErrorClass =
  | "invalid_request"
  | "not_found"
  | "insecure_transport"
  | "endpoint_not_permitted"
  | "endpoint_failure";

/**
 * A refused or failed endpoint operation. Its message and properties never
 * contain the endpoint URL, a key, a request or a reply body, and it keeps no
 * underlying cause, whose text could.
 */
export class EndpointError extends Error {
  private constructor(
    readonly errorClass: ErrorClass,
    message: string,
    readonly field?: InvalidField,
    readonly providerId?: string,
    readonly category?: FailureCategory,
    readonly status?: number,
  ) {
    super(message);
    this.name = "EndpointError";
  }

  static invalid(field: InvalidField): EndpointError {
    return new EndpointError("invalid_request", `endpoint request rejected (${field})`, field);
  }

  static unknownModel(): EndpointError {
    return new EndpointError("not_found", "model is not listed for this endpoint");
  }

  static insecureTransport(): EndpointError {
    return new EndpointError("insecure_transport", "public endpoint requires HTTPS");
  }

  static notPermitted(): EndpointError {
    return new EndpointError("endpoint_not_permitted", "endpoint address is not permitted");
  }

  static failure(providerId: string, category: FailureCategory, status?: number): EndpointError {
    const suffix = category === "http" ? ` ${status}` : "";
    return new EndpointError(
      "endpoint_failure",
      `${providerId} endpoint failure: ${category}${suffix}`,
      undefined,
      providerId,
      category,
      status,
    );
  }
}
