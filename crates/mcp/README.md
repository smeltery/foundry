# Foundry MCP stdio interface

This library exposes a synchronous local MCP interface. The caller supplies the
backend and invokes `serve_stdio(&mut backend)`. No executable, OAuth server,
hosted endpoint, or session implementation is included.

Public API:

- `Backend::call(&mut self, name: &str, args: Value) -> Result<Value, McpError>`
- `McpError::new(code, message)` and `McpError::unsupported()`
- `Server::default().handle(frame: &[u8], &mut backend) -> Option<Value>`
- `list_tools() -> Value` returns the tool descriptor array.
- `serve(&mut reader, &mut writer, &mut backend) -> io::Result<()>`
- `serve_stdio(&mut backend) -> io::Result<()>`

The backend receives the following names and arguments unchanged:

| Tool | Required arguments | Optional arguments |
| --- | --- | --- |
| `session_create` | `prompt`, `directory` | `idempotency_key` |
| `session_list` | None | None |
| `session_get` | `session_id` | None |
| `session_send` | `session_id`, `message`, `message_id` | None |
| `session_cancel` | `session_id`, `mode` | None |

All arguments are strings. IDs and directory values must be nonempty; prompt and
message accept 1–65,536 Unicode scalar values. Cancel mode is `graceful` or `hard`.
The same descriptors drive validation. Backend business rules may reject a
schema-valid request, including blank-only prompts, missing sessions, permissions,
and lifecycle conflicts. Unsupported backend operations must return an explicit
error. Successful values are serialized into MCP text content; backend failures
produce `isError: true` with a JSON text object containing `code` and `message`.
Backend messages must not contain credentials or other sensitive diagnostics.

A connection must initialize with protocol version `2025-11-25`, capabilities,
and client name/version, then send `notifications/initialized`. Unsupported
versions are explicitly rejected. Ping is available during the handshake.
Unknown methods return -32601; invalid tool names or arguments return -32602.
Notifications produce no response and cannot dispatch a tool. Batch messages,
client responses, null/fractional request IDs, and invalid envelopes are rejected.
String and integer request IDs are preserved.

The transport accepts newline-delimited UTF-8 JSON, up to 2 MiB per payload.
Oversized or unterminated frames receive an error and terminate the connection;
reads never buffer more than the limit plus two bytes. Malformed JSON receives
-32700. Responses over 2 MiB are replaced with an error; backend results and their
serialization are still allocated before that output check.
If an echoed request ID makes even the replacement error exceed the frame limit,
the transport emits one bounded error with a null ID and closes with an error;
it cannot preserve request correlation in that case. EOF between frames
is clean shutdown. Stdout contains only protocol messages; transport diagnostics
use stderr. The single-message `handle` API expects framing to be supplied by its
caller and does not enforce an output allocation bound.

This is a narrow synchronous interface: no tool execution cancellation channel,
pagination, progress subscriptions, model selection,
resources, prompts, sampling, or remote authentication. Those requests are not
advertised as implemented. The CLI supplies a durable local backend through `f5y mcp`. Official SDK
interoperability checks are registered in the workspace CI; broader client and
remote-authentication workflows remain separate work.

Run `cargo test -p foundry-mcp` and
`cargo clippy -p foundry-mcp --all-targets -- -D warnings` for library verification. Library tests use an in-memory backend and streams;
`tests/mcp-sdk/verify.py` exercises the built CLI with the official SDK.

Request parameters accept an object-valued `_meta`. Unknown metadata remains
opaque; a request's `progressToken`, when present, must be a string or integer.
Metadata is not forwarded to the session backend. Progress tokens are accepted
without emitting progress notifications or advertising additional capabilities.
Invalid notification metadata is ignored without a response or handshake state
change. Tool argument validation remains separate and strict.
