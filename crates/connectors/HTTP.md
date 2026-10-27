# MCP HTTP client

`McpHttpClient` implements a bounded, sequential subset of MCP 2025-06-18:
initialization and the initialized notification, paged tool listing, and tool
calls. Both JSON responses and POST response SSE streams are supported.

## API and authentication

```rust,ignore
let policy = NetworkPolicy::default();
let request = server.prepare_with_policy(&credentials, &policy)?;
let mut client = McpHttpClient::new(request, policy, HttpLimits::default())?;
client.initialize().await?;
let page = client.list_tools(None).await?;
let result = client.call_tool("example", arguments).await?;
```

These async methods require a Tokio runtime with networking and time enabled.
Use existing `Credentials` and `PreparedRequest` header authentication. OAuth
acquisition, refresh and hosted adapters remain unsupported. Tool results retain
content, structured content, tool-error status and extension metadata; tool
errors are distinct from HTTP or protocol errors. Results are untrusted data.
Debug implementations and errors do not expose credentials, URLs, session IDs,
response bodies or tool data. Explicit data accessors can expose sensitive values
to callers and must not be logged indiscriminately.

## Network boundary

The default policy requires HTTPS and public addresses. For every POST, DNS
answers are resolved and all addresses validated before constructing an
HTTP client with those pinned addresses. Mixed public/private answers fail.
Literal IPs receive the same checks. A fresh client prevents stale pooled
connections; the returned peer is also checked against the pinned set.

No environment proxies, redirects, automatic retries, cookie storage, referer
forwarding, content decompression or TLS key logging are enabled. Framing and
protocol headers cannot be overridden through catalog headers. Request and
response bodies default to 1 MiB, and a complete operation defaults to 30 seconds,
including DNS, response consumption and the initialization notification.
The configurable maximum is 16 MiB and five minutes. Requests are sequential;
pagination is explicit, with at most 1,000 tools per page.

TLS uses an explicit ring provider and the platform certificate/hostname
verifier through a preconfigured rustls client. No certificate or hostname
validation bypass is exposed. This does not depend on a globally installed
crypto provider or another crate enabling a provider feature.

For local mocks, `NetworkPolicy::local_http("http://127.0.0.1:PORT")` permits
only that exact literal loopback HTTP origin. It does not permit other ports,
other loopback addresses, DNS aliases, private networks, or remote HTTP. Pass it
to `Server::prepare_with_policy` and the client constructor. This is an explicit
per-client capability, not a global insecure switch. Catalog default validation
continues rejecting local targets; local test servers use typed synthetic config.

## Supported protocol boundaries

Initialization validates the negotiated version, capabilities and server info,
then sends `notifications/initialized`. Optional server session IDs are bounded,
carried on subsequent requests and checked for unexpected replacement. Responses
must match the outstanding JSON-RPC ID. A 404 session expiration is returned to
the caller without automatic reinitialization or replay.

SSE supports bounded incremental framing, comments, CR/LF line endings, UTF-8
chunk boundaries and multiline data. Notifications preceding a response are
ignored; they are not delivered as application events. Server requests return
an explicit unsupported error. The client stops after the matching response.

Not implemented or claimed: full HTTP/SSE conformance, legacy HTTP+SSE, standalone
GET event streams, reconnect/resume or event replay, server-initiated requests,
cancellation notifications, session DELETE, OAuth flows, provider compatibility,
or all MCP versions. Unknown content types and compressed replies are rejected.
Dropping/timing out a request is not proof the remote operation was cancelled.
Callers must decide whether a tool operation is safe to retry.

## Dependencies and verification

Direct transport pins are reqwest 0.13.5 (`rustls-no-provider`, no defaults),
Tokio 1.53.3, rustls 0.23.46 (`ring`, `std`, `tls12`, no defaults), and
rustls-platform-verifier 0.7.1. The connector dependency graph contains ring and
no AWS-LC/CMake dependency. All new packages remain subject to the release
notice inventory and rights gates.

The reviewed official Rust SDK was rmcp 3.8.0 at commit
`cd88056ffdc97756fbd98e5a12bbeb41758f0ce8`. Its
[reqwest backend](https://github.com/modelcontextprotocol/rust-sdk/blob/cd88056ffdc97756fbd98e5a12bbeb41758f0ce8/crates/rmcp/src/transport/common/reqwest/streamable_http_client.rs)
accepts a custom HTTP client, so peer policy is possible. Its built-in JSON
response path reads the entire body. This owned transport keeps body limits,
redacted errors and explicit partial-protocol behavior in a small testable layer.
A future SDK adapter can reuse the network policy if its resource limits and
error handling are verified.

Local tests cover handshake/session/version headers, tools and pagination,
chunked SSE, exact query preservation, auth headers, response/request limits,
timeouts, redirect refusal, proxy environment isolation, session conflicts,
unsupported server requests and mixed-address policy rejection. No public
provider, live OAuth, or public TLS endpoint was contacted. Local TLS tests reject
untrusted certificates and mismatched identities before HTTP credentials arrive;
a trusted matching certificate permits the synthetic request. Test-only trust
and address injection is private and absent from production builds. These tests
ran on macOS; other platform certificate stores remain unverified.

Primary references:

- [MCP transports](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports)
- [MCP lifecycle](https://modelcontextprotocol.io/specification/2025-06-18/basic/lifecycle)
- [MCP tools](https://modelcontextprotocol.io/specification/2025-06-18/server/tools)
- [Reqwest client configuration](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html)
- [Platform certificate verifier](https://docs.rs/rustls-platform-verifier/0.7.1/rustls_platform_verifier/struct.Verifier.html)
