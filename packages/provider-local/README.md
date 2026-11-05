# Local Endpoint Facade

This package is a dependency-free adapter facade that drives one local or custom
model endpoint, so the [adapter](../adapter/README.md) can run a session end to
end without an SDK. It uses Node and Bun built-ins only. It makes **no claim of
parity** with any SDK or hosted service.

## API

```typescript
const endpoint = new LocalEndpoint({
  providerId: "stub-local",
  baseUrl: "http://127.0.0.1:8080/v1",
  models: ["fixture-model"],
  optIn: ["loopback"],
  timeoutMs: 60_000, // optional; at most 300_000
  key, // optional write-only key
});
const facade = createLocalFacade({ endpoint, system, tools });
await runAdapter({ input, write, facade });
```

The facade sends the session prompt, streams each reply's text as a delta, and
passes each requested tool call to core through the adapter. Tool replies,
including denials, go back to the endpoint unchanged; this package never runs a
tool. A finished session settles as `action_required` with the final reply text,
because an endpoint reply does not prove the task is done. Tool rounds are
bounded (64 at most). The session's model must name this endpoint's provider ID
and one of its listed models.

## Endpoint rules

These follow the Rust local endpoint client and the
[providers specification](../../docs/architecture/providers.md#local-and-custom-endpoints):

- Loopback, LAN and tailnet addresses need an explicit opt-in. Public addresses
  need HTTPS. Link-local, cloud metadata and reserved addresses are always refused,
  and DNS answers that span classes are refused.
- Each call resolves the host once and connects only to the vetted addresses, over
  a socket this package opens itself. Proxy variables are ignored, redirects are
  never followed, and replies are never decoded from a content encoding.
- One timeout bounds lookup, connection and reply. Replies are limited to 4 MiB,
  and request bodies to the same size.
- HTTPS uses the platform trust store and requires a verified peer certificate.
- Errors carry a neutral class (`invalid_request`, `not_found`,
  `insecure_transport`, `endpoint_not_permitted` or `endpoint_failure`) and, for
  failures, a category: `dns`, `connect`, `timeout`, `tls`, `http` with the
  status, or `protocol`. Their text names the provider ID and category only, never
  the URL, a key or a body. The adapter reports a thrown error as
  `adapter_failed` without its text.

## Limitations

- Only the `chat` wire format is built, without streaming. The `response` and
  `message` formats of the Rust client are not.
- Wire names are provisional pending the naming decision.
- A private certificate authority cannot be configured yet.
- The facade does not support steering or native context hooks, and usage counts
  are parsed but not reported over the adapter protocol.
- Nothing here downloads or runs model weights.

## Offline tests

```sh
bun test packages/provider-local/test
```

The tests run a deterministic stub server on loopback inside the test process and
use a fixture resolver for host names. They make no external calls and need no
credentials.
