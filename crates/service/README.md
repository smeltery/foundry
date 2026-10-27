# Foundry application service

`Service` joins durable session storage with the subprocess runtime. It does not
supply a model provider. Existing run entry points deny tools; `run_with_tools`
requires an explicitly constructed trusted workspace-tool policy.

| Method | Return value inside `foundry_service::Result` |
| --- | --- |
| `Service::open(path)` | Service using a 60-second lease and 250 ms polling |
| `Service::open_with_options(path, ServiceOptions)` | Service with explicit lease/poll durations |
| `create(CreateSession)` | Session |
| `get(id)` | Session |
| `list(limit)` | Sessions, newest first |
| `events(id, after, limit)` | Durable events after an exclusive cursor |
| `cancel(id, CancelMode)` | Session with durable cancellation intent |
| `recover_expired()` | Explicitly recovered sessions |
| `run(id, expected_version, &AdapterSpec)` | Persisted terminal session; tools denied |
| `run_with_tools(id, version, &AdapterSpec, control, policy)` | Coordinated run with explicit trusted tool policy |

Create sessions with an absolute working directory. Pass an executable path in
`AdapterSpec`; executable names are not searched through PATH by the runtime.
`run` is synchronous and claims the session using its expected version. Independent
SQLite connections renew the lease and observe cancellation from other processes,
including while the adapter is silent. The poll interval must be positive and less
than one third of the lease duration. The database must be a persistent file and
its parent directory must already exist.

Adapter events receive unique IDs and immutable revision zero snapshots; the store
assigns ordered sequence numbers. Text events are kept as delivered rather than
assuming a delta or replacement convention. An `adapter_settled` event records an
adapter's claim; only the session's committed lifecycle and outcome establish the
result. Event callback failures request hard cancellation and remain errors even if
the adapter already returned success.

Cancellation is not completion. The owner persists suspended status after runtime
supervision returns. Runtime failures persist error status and return an error.
Lease loss requests a hard process-group stop and prevents stale writes. An expired
attempt remains available for explicit recovery, never automatic replay. Failed
terminal writes return errors; they cannot be reported as successful execution.
Store leases use system UTC time. An independent monotonic watchdog allows at most
two thirds of the configured lease interval from before each renewal operation;
a late renewal cannot revive expired execution authority. Database contention can
delay the eventual worker join and service return, but cancellation and expiry
remain observable while writes are blocked. OS scheduling is not a hard real-time
guarantee.

All subprocess cleanup is delegated to the runtime. Store recovery cannot undo
external effects or prove that a crashed owner's subprocess stopped. Reconcile any
uncertain external operation before starting another attempt. No fork, rewind,
provider credentials or automatic retry is implemented here.

Run `cargo test -p foundry-service` and
`cargo clippy -p foundry-service --all-targets -- -D warnings`. Unix integration
fixtures require Python 3; they do not call providers or external services.

## Messages during execution

`enqueue_message(session_id, id, text)` persists an instruction without claiming
execution. `messages(session_id, after, limit)` reads its state and
`withdraw_message(session_id, id)` withdraws only an unclaimed instruction.

An adapter must advertise `message_ack_v2` in its `Ready.capabilities`. After that
ready event is durable, the monitor atomically claims the oldest pending message
and submits it to the runtime. Only one in-flight message is allowed; unresolved
claims from prior attempts prevent further delivery. An adapter without the
capability leaves the queue untouched.

The runtime generates a fresh unpredictable `receipt` for each delivery and
serializes it after the message `text`. The adapter responds with
`MessageAccepted { attempt_id, message_id, receipt }` only after facade acceptance,
echoing the exact challenge. Matching the tail challenge proves knowledge of that
part of the message frame; facade acceptance remains the adapter's promise, not
proof of instruction execution. Missing/malformed, mismatched, foreign and
unsubmitted acknowledgments fail; there is no v1 fallback. The service records a
stable acknowledgment event and marks the matching fenced message delivered. Repeated acknowledgments are
idempotent. The event and delivery state use separate transactions; a failure
between them stops execution and leaves the claim unresolved. The delivery row is
authoritative, and the event alone is not proof that this transaction completed.

Delivered means adapter acceptance, not successful execution. Cancellation, broken
pipes, crashes and lost leases never automatically requeue or mark an in-flight
message delivered. A claimed message cannot be withdrawn, even when the process
may not yet have read it. Reconciliation remains explicit; exactly-once external
execution is not guaranteed.

## Local cancellation control

`run_with_control(id, expected_version, &AdapterSpec, Arc<AtomicU8>)` accepts a
caller-owned control: zero means running, one requests graceful cancellation and
two requests hard cancellation. Advance it with `fetch_max`; observed escalation
is sticky even if the caller later decreases the value. Values above two are
handled as hard cancellation. Existing `run` uses a fresh zero-valued control.

The caller installs signal handlers; this crate installs none. An independent
watchdog observes local control every 5 ms without waiting for SQLite. The lease
monitor persists the request when storage is available.
Callbacks and finalization also observe the control, so an adapter's late success
cannot override observed cancellation. A pre-cancelled run claims and suspends the
attempt without launching a process. Cancellation after execution starts returns
only after runtime cleanup and terminal persistence; cleanup or storage failures
remain errors. Database contention can delay a monitor already inside a storage
operation; polling is not a hard real-time signal delivery guarantee.

## Explicit workspace tools

`run_with_tools` consumes a `foundry_runtime::coordinated::TrustedToolPolicy` built
by a trusted caller. It holds an already-open directory capability, an absolute
helper path, individual grants, bounded limits and a positive timeout. Adapter
JSON supplies only a tool name, call ID and typed input; it cannot supply or expand
policy authority. Resuming a session requires a fresh explicit policy. This is a
library entry point, not a CLI grant feature or an OS sandbox.

A bounded persistence worker commits a fenced tool claim before helper dispatch.
It classifies the cleaned-up helper's typed evidence and commits the completion
receipt before the runtime can deliver a tool result. Only one tool is admitted at
a time, with at most 128 calls per attempt. SQLite work never runs on the coordinated
supervisor loop. All owned process and persistence workers are joined before return.

A missing reply, uncertain effect, lost fence or partial receipt write never means
safe retry. Claimed and uncertain calls block further dispatch and terminal outcomes,
including across attempts. Cancellation may leave a claimed record if receipt
completion is no longer authorized. Completed receipts are not replayable output:
the ledger stores digests, and repeated call IDs are rejected rather than rerun.
No rollback or exactly-once external effects are promised.

Input digests use SHA-256 over `foundry.tool.input.v1` plus a NUL byte and fixed-field
JSON `{tool,input}` serialized from the validated types. Output digests use
`foundry.tool.output.v1` plus a NUL byte and the typed reply JSON, with all object
keys recursively sorted and array order preserved. Golden-vector tests pin both
encodings. The ledger receives no raw grants, helper diagnostics or result payloads.
Adapter failure diagnostics remain sanitized before event persistence.

Most coordination tests use controlled adapter/helper fixtures. To additionally
exercise the real workspace helper locally:

```sh
cargo build --locked -p foundry-tools --bin f5y-toolhost
FOUNDRY_TEST_TOOLHOST="$PWD/target/debug/f5y-toolhost" cargo test --locked -p foundry-service --test tools real_toolhost_executes_a_granted_write_and_commits_receipt -- --ignored
```

That test uses temporary files, explicit create permission and the real helper
process. It does not call providers, install agents or establish remote parity.

## Tool-request event retention

New service `tool_request` events retain only this metadata shape:

```json
{
  "type": "tool_call",
  "attempt_id": "attempt-id",
  "call_id": "call-id",
  "name": "write",
  "input_encoding": "serde_json.compact.v1",
  "input_bytes": 19,
  "input_sha256": "<64 lowercase hexadecimal characters>"
}
```

The attempt must match the active fence. Identity fields must be nonblank and
contain no control characters; attempt/call IDs allow at most 128 UTF-8 bytes and
names at most 64. The service stores no raw `input` field, input paths or file
contents in this event. Raw input remains available to ephemeral tool dispatch.
Denied and unknown tool requests still produce one ordered metadata event.

`input_bytes` counts UTF-8 bytes of compact JSON serialized from the decoded input
value, including JSON escaping. `input_sha256` is plain SHA-256 over those same
bytes. It does not hash original wire whitespace and is not a canonical signing
encoding. This event hash is distinct from the ledger's domain-separated,
typed `{tool,input}` invocation digest described above; it does not establish
permission, dispatch or completion.

This change applies to newly persisted service events. Historical rows are not
rewritten. Identity metadata is validated, not scrubbed for sensitive words.
Other event types, prompts, agent text and settlement payloads retain their
existing behavior; this is not a claim that all stored or displayed text is
secret-free. The store's general `append_event` API still accepts caller-provided
JSON and does not itself perform this service-level transformation.
