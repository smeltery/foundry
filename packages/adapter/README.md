# Adapter Wire Layer

This package implements the version 1 JSONL subprocess boundary used by the Rust
runtime. It contains **no model provider or agent SDK**. An owned facade can be
injected by trusted application code; the fake facades exist only in tests.

## API

Import `runAdapter` and the `Facade` type from `src/index.ts`, or use the compiled
ESM/declaration exports after `bun run adapter:build`:

```typescript
const result = await runAdapter({
  input, // AsyncIterable<Uint8Array>
  write, // async (frame: Uint8Array) => void
  facade, // optional trusted binding
  shutdownTimeoutMs: 1000,
  signal, // optional process-owner AbortSignal
});
```

The caller closes its input transport when the result resolves. A facade runs one
session with an abort signal and asynchronous text/tool-request callbacks. Its
optional steering, cancellation and disposal methods must cooperate with abort.
The controller bounds shutdown even if the facade does not cooperate. It cannot
stop arbitrary in-process code; the Rust supervisor owns process termination.

The standalone entry point is deliberately unconfigured:

```sh
bun packages/adapter/src/main.ts
```

It emits `ready`, waits for `start`, then returns `adapter_not_configured` and
exits unsuccessfully. It never discovers credentials, runs a model, or claims a
task succeeded. There is no fake-provider executable mode. The entry point strips
inherited environment variables except `PATH`, `LANG`, `LC_ALL`, and `TZ`.
Library callers supply a trusted facade explicitly; that facade is not sandboxed.

`runConfiguredAdapter` accepts a trusted `createFacade(body)` factory instead of
an already constructed facade. It consumes one bounded private bootstrap on the
same input stream, checks the factory's independently computed selection digest,
acknowledges configuration and then starts the ordinary controller. Failed
configuration does not release Start. Credentials are explicit private inputs;
the factory must validate them and clean up partially constructed resources.
See [configuration and permissions](../../docs/architecture/adapter-configuration.md).

A concrete `ContextFacade` may supply typed capture, restore, observation and
continuation hooks. The controller advertises complete supported hook groups,
retains the same owner across settlement/export, and awaits the continuation
acknowledgement before execution. Native semantic validation and actual instance
continuity remain responsibilities of the concrete binding and registered service
comparators. The package includes no native codec or provider implementation.

## Protocol Behavior

- Inputs and outputs are bounded to 2 MiB per JSON frame, excluding its newline.
- UTF-8 decoding is strict. Partial final frames and unknown commands fail.
- Ordinary execution accepts one version-checked `start`; later commands must
  match its attempt. Native restoration uses explicit Continue without replaying Start.
- Text events are append-only deltas. No rich event snapshot protocol is implied.
- Tool calls only request core execution. Replies, including denials, are passed
  back without turning them into success. This layer never executes tools.
- `ready` advertises `message_ack_v2` only when the facade supports steering.
  `message` requires an opaque nonempty `receipt` serialized after `text`.
  The runtime generates a fresh unpredictable challenge for each delivery. After
  facade acceptance, `message_accepted` echoes that exact receipt. Missing/malformed
  receipts are rejected; duplicate IDs with different text or receipts conflict.
  The echo proves knowledge of the frame's tail challenge. Facade acceptance is
  still the adapter's promise, not proof that the instruction executed. No v1
  acknowledgment fallback is supported.
  Messages are deduplicated by ID; conflicting text or receipts fail. An accepted
  duplicate re-echoes the same receipt without redispatch. Steering does not block
  cancellation.
  Receipts are per process; persisted queues are not implemented.
- Cancellation aborts pending tool waits, suppresses late output and completion,
  and closes without a fabricated success event. Version 1 has no cancellation
  event; the Rust supervisor interprets closure after cancellation.
- Completion without an explicit verdict becomes `action_required`. A trusted
  facade's explicit verdict is not independently verified by this wire layer.
- Exception text and malformed input are never copied into diagnostic events.

The layer bounds pending output frames to 32, pending tools to 64, and tool/message
IDs to 1,024 each per attempt. JSON tool values are limited to 64 nested levels.
Versions above JavaScript's safe integer range are rejected rather than rounded.
These limits are explicit implementation constraints, not new Rust contract fields.

## Offline Tests

```sh
bun test packages/adapter/test
```

Tests cover byte framing, unconfigured startup, attempt fencing, tool denials,
private bootstrap, native lifecycle routing, steering, cancellation, terminal-state
races and safe diagnostics. They require
no credentials, external packages or network requests.

The cross-language tests require a freshly built Rust CLI supplied explicitly:

```sh
cargo build -p f5y
FOUNDRY_TEST_BINARY="$PWD/target/debug/f5y" bun test packages/adapter/test/rust-interop.test.ts
```

Without that absolute binary path, those tests are reported as skipped. They use
temporary test-only facade shims with an absolute Bun shebang, an unusable `PATH`,
and disposable databases. They exercise the Rust service and supervisor through
the TS wire layer; no model runs and no requested tool is executed. The tests
never build Rust themselves or modify Cargo files.

`python3 scripts/checks/all.py --group integration` builds the needed artifacts and
also runs the Rust-to-compiled-ESM private bootstrap test and real helper permission
tests. This is the same integration group used by CI.

## Integration boundary

The package supplies transport and lifecycle control. A separately provisioned
binding supplies model execution, model/effort support and native state semantics.
The service owns durable messages, consuming permissions and checkpoint publication.
Neither a hook capability nor a receipt grants that authority. CLI native resume,
live provider compatibility, usage reporting and structured completion evidence
are not established by these neutral tests.
