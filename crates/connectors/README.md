# Connector configuration core

`foundry-connectors` parses the catalog contract in
[`marketplace/schema.json`](../../marketplace/schema.json) and prepares requests
in memory. An opt-in [MCP HTTP client](HTTP.md) performs bounded network
operations under an explicit policy. Catalog parsing and request preparation do
not perform network operations; no provider login, credential persistence, or
hosted adapter implementation is included.

## API

- `Catalog::parse(&[u8])` enforces typed shapes, unknown-field rejection, duplicate
  JSON keys, scoped IDs, URLs, skills, icons, headers and auth templates.
  Use this entrypoint for untrusted catalog JSON. Direct serde deserialization
  supplies types only; it does not replace catalog validation.
- `Catalog::validate()` validates typed values after edits.
- `Connector::configuration_status(server_id, &Credentials)` reports configured,
  missing credential, fixture-only, unsupported adapter or rejected configuration.
- `Connector::prepare(server_id, &Credentials)` rejects fixture connectors and
  returns `PreparedRequest`. `Server` exposes the same lower-level operations
  without connector-level fixture context.
- `Secret::new(String)` wraps a nonempty printable ASCII credential of at most
  8 KiB. `Credentials` distinguishes no auth, an OAuth access token, and an API key.
- `PreparedRequest::url()` and `headers()` expose sensitive data explicitly to a
  transport. Debug output and errors redact URLs, catalog values and secrets;
  these types intentionally have no serialization implementation.
- `PreparedRequest::validate_peer(IpAddr)` rejects nonpublic resolved peers.
  The transport must call this on every actual connection and retry, verify TLS
  for the original host, and prevent credential forwarding across redirects.

Configuration status indicates request preparation readiness, not a successful
connection or verified provider capability.

## Auth and URL behavior

- `none` produces no generated authorization header and rejects supplied
  credentials rather than silently discarding them.
- `oauth` binds an existing access token as `Authorization: Bearer ...` after
  token syntax checks. Login discovery, token refresh, scopes and token storage
  require a separate implementation.
- `api_key` substitutes `${credential}` literally in the catalog's header
  templates. Custom key headers, prefixed authorization headers and repeated
  placeholders are supported. Substitution is checked against size limits
  before allocation; header names are case-insensitive.
- Query strings, duplicate query keys, percent-encoding, ordering, explicit ports
  and URL paths are preserved exactly. The schema does not define API-key query
  placement: URL credential placeholders return `UnsupportedQueryAuth`. No query
  parameter, authentication endpoint or hosted adapter endpoint is invented.
- `endpoint.kind = adapter` with `state = unavailable` always reports unsupported,
  even when credentials are supplied.

Catalog HTTP(S) syntax is recognized, but preparing an executable request
requires HTTPS. Additional runtime restrictions reject URL userinfo, fragments,
control characters, unsafe host forms and nonpublic IP literals. Header controls,
non-ASCII values, conflicting auth and routing/framing overrides are rejected.
These safety restrictions are stricter than basic schema string patterns.

DNS names are checked syntactically without resolving them. DNS rebinding and
redirect enforcement remain transport obligations. Credentials remain ordinary
in-memory Rust strings; this crate does not guarantee memory zeroization.

## Verification

```sh
cargo test --locked -p foundry-connectors
cargo clippy --locked -p foundry-connectors --all-targets -- -D warnings
```

Tests use synthetic inline catalog records and local loopback mock servers. No
catalog bulk import, public provider calls, external credentials or real provider
configuration is needed.
