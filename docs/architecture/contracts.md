# Session and backend contracts

This is the versioned contract index for session lifecycle, events, backend
capabilities, permission requests, cancellation, checkpoints and storage.
It distinguishes existing interfaces from selected target behavior. A codec,
negotiation result or storage receipt never proves that a native backend loaded
context, enforced permission or completed external work.

The [CLI target contract](cli-contract.md) governs command/profile/entity
semantics. [Capability negotiation](capability-contract.md) and
[permission decisions](permission-contract.md) define additive pure interfaces;
production adapters and service orchestration must explicitly integrate them.
The shipped ordinary process protocol is unchanged by these additions.

## Versions and ownership

| Boundary | Current interface | Compatibility rule |
| --- | --- | --- |
| Ordinary adapter stream | `PROTOCOL_VERSION = 1`, `AdapterCommand` / `AdapterEvent` | Exact protocol version; capabilities are explicit additional preconditions, not a version substitute |
| Message delivery | `message_ack_v2` with exact message/attempt/private receipt | Acknowledgement proves receipt, not completion; absent/legacy support cannot authorize delivery |
| Private capture | `context` codec and its capability | Bounded opaque content and exact bindings; capture support grants neither restore nor Continue |
| Private loading | `context_v2` codec and load scope without an attempt | Separate phase/direction validation; ordinary protocol acceptance is insufficient |
| Durable store | Schema 11, concrete `Store` APIs | Migrations preserve historical records; current ownership is required for mutation |
| Capability/permission helpers | Additive owned pure contracts | Not new ordinary wire frames, runtime advertisement or automatic tool authority |
| Product profiles | Current/legacy target contract | Explicit profile acknowledgement; include effective profile in execution/restore compatibility before those targets are integrated |

The service owns admission, durable transitions and leases. The runtime owns its
process tree, decoder and bounded queues. An adapter supplies backend events and
capability evidence; it cannot grant itself authority. Model and policy owners
qualify their own capability requirements. CLI/MCP schemas describe callable
shapes, not guaranteed operational availability. Credentials, fences, receipts,
raw checkpoint bytes and permission authority never become public display IDs.

## Session lifecycle and execution generations

The existing `Session` has a stable ID, prompt, directory, optional model,
lifecycle, optional outcome/result, active attempt ID and version. Its current
result is an optional string. General structured results, associations, archive
metadata and independent conversation/turn DTOs are target work; do not infer
their support from a directory or stringify unknown results as a conversion.

| Operation/state | Contract |
| --- | --- |
| Create | Owned queued admission state; no execution implied. Validate the existing `CreateSession` shape and bounds. Current create-idempotency compares the complete request; that is not an external prompt-only equivalence claim. |
| Begin | Expected version and exclusive live ownership admit one new attempt. Stable session identity remains; attempt identity is a separate generation. |
| Running | Only the current fenced owner may append/claim/finish. Model or effective execution-configuration changes need a new admitted generation. |
| Exit | Requires explicit success/failure/action-required outcome. Process exit or a null result does not imply success. |
| Error | Operational failure, with no task outcome fabricated. |
| Suspended | Retained task identity/history with no task outcome; cancellation/quota/uncertain recovery may require later explicit continuation. |
| Observe | Read-only history/receipt lookup; never an execution permit or lease renewal. |
| Recover | Explicit lease-expiry recovery preserves history and uncertainty. Opening a database does not recover or replay work. |

The owned queued state is not an extra external wire lifecycle by inference.
Session version, durable work revision and event sequence have separate meanings.
An attempt fence includes a private token and is not a public attempt descriptor.
Store expiry uses UTC and therefore has clock-change sensitivity; process and
permission deadlines use their explicitly injected clock contract.

## Messages, events and projections

Existing messages are pending, in-flight, delivered or withdrawn. Enqueueing
never starts an attempt. Claiming under a valid fence prevents withdrawal; one
unresolved in-flight message blocks later claims. The adapter's exact receipt
must match before delivery is persisted. Delivery is not completed instruction
work, and a lost receipt must not trigger automatic resend.

The current event envelope contains ID, session/attempt IDs, sequence, revision,
kind and JSON data. Revisions are immutable; an ID stays with one attempt/kind.
Exact retry is idempotent under a valid fence. Revisions increase but need not be
consecutive. Sequence is an exclusive durable cursor, not an item count; SQLite
bounds are enforced before mutation. Target immutable display position remains
separate work rather than an alias for the current delivery sequence.

Target messages exclude the opening prompt and agent replies. Conversation owns
opening text, turns and replies; events own intermediate activity/revisions.
Each projection has its own cursor and must not be reconstructed from another
projection's incomplete data. Project grouping and persistent assistant remain
separate associations under the [entity contract](cli-lifetime-contract.md).
Unknown external aliases, cursor encodings and interim-reply timing remain
qualification gates, not guessed DTO fields.

### Existing adapter event families

| Adapter event | Stored kind / payload boundary |
| --- | --- |
| Ready | `adapter_ready`; protocol version and advertised capability strings. Readiness is not admission or native validation. |
| Text | `text`; attempt-bound text activity. Treat content as task data, not authority. |
| ToolCall | `tool_request`; sanitized identity/name plus compact-input encoding, byte count and SHA-256. Raw arguments do not enter this event. |
| Settled | `adapter_settled`; explicit outcome and string result. The owner still checks lifecycle, cancellation and durable finalization. |
| Failed | `adapter_failed`; the service stores fixed safe code/message rather than arbitrary backend diagnostic text. |
| MessageAccepted | `message_accepted`; stable message event identity and exact acknowledgement handling. The private receipt is sensitive data, not a public grant. |

Ordinary message receipts are currently retained in event data; runtime correlation
validation does not remove them. Text is retained unchanged. Do not claim a
universal receipt-free or sanitized public event stream. The witness projection
has a narrower digest-only schema. Ordinary append and message acknowledgement
are separate service calls, unlike atomic schema-11 continuation acceptance.

The reserved `continue_accepted` payload contains restore/load/attempt/message
identities and authorization, tail-receipt and request digests. It has no raw
input, raw tail receipt or ordinary event `type` tag. Its dedicated transaction
binds the event and acceptance record; a generic event with that kind is not
authenticated continuation evidence. Orphans are rejected, not adopted.

These kinds describe the current service mapping, not all records stored by
checkpoint/continuation internals. Public event export must preserve the private
boundary of internal witness/acceptance material. An unknown event kind can be
retained only as opaque data in a boundary that expressly supports it; it must
never acquire executable semantics. Unknown ordinary enum variants fail decoding.
Existing ordinary serde structs can tolerate extra fields except where strict
validation is explicitly defined; private context codecs reject unknown/duplicate
fields. A future required semantic field needs a reviewed version/capability
change. Do not describe current permissive decoding as strict schema enforcement.

### Private context schema boundaries

Context1 keeps capture/export/release and its older attempt-bound restore forms
separate from context2 loading. Command tags are `context_capture_begin`,
`context_export`, `context_release`, `restore_begin`, `restore_payload`,
`continue_context` and `context_failed`; event tags are `context_capture_ready`,
`context_candidate`, `restore_prepared`, `context_restored`,
`continuation_accepted` and `context_failed`. Direction-specific decoding and
strict nested metadata apply. Never reinterpret its restore attempt as a
context2 pre-admission scope.

Context2 uses `restore_begin_v2`, `restore_prepared_v2`, `restore_payload_v2`,
`context_restored_v2`, `continue_context_v2` and `continuation_accepted_v2`.
Loading scope has restore/load IDs and no attempt. Begin binds checkpoint/record/
witness/configuration/payload and retention identities. Only Continue/Accepted
carry the real admitted attempt, continuation/input identity and authorization
binding. Exact scope/receipt comparison is inert validation, never permission.

Current private bounds are 1 MiB payload, 2 MiB frame, 16 KiB format, 64 KiB
coverage and 256 KiB continuation text, with canonical base64 and exact length/
digest validation. Current loading transport sends Begin/Payload and receives
Prepared/Restored after compatible readiness; it rejects ordinary output and
Continue/Accepted. It does not yet provide the coordinator transition below.

## Permissions and cancellation

Capability negotiation answers whether an operation can be supported; a
permission decision answers whether this bound invocation is authorized.
Existing trusted workspace grants remain valid policy inputs. A new permission
request cannot manufacture authority from a tool name, adapter event, `--yes`,
capability advertisement or a previously accepted unrelated call.

The [permission contract](permission-contract.md) binds the immutable input and
session/attempt/call/profile to an authorized decision owner. Pending, denied,
unsupported, cancelled, expired and consumed decisions are distinct. A one-use
accepted decision still needs current execution ownership and the tool's trusted
grant/dispatch checks. Interactive UI and durable distributed decision recovery
are implementation work; the pure contract does not supply authentication.

Graceful cancellation attempts a final workspace snapshot; hard cancellation
upgrades it and skips waiting for a new snapshot. Neither request proves a
process stopped. Current store cancellation is monotonic and excludes successful
completion of a cancelled active attempt. Observer interruption affects only the
observer. On lease loss, execution must stop and uncertain effects remain
unresolved. A new owner cannot infer a successful tool or replay a claimed call.

## Capture, loading and continuation authority

Use the concrete [storage API](../../crates/store/README.md) and
[restoration contract](restoration.md), not a second storage abstraction that
silently drops their fences, quotas or historical semantics.

```mermaid
flowchart LR
    A[Current fenced capture cut] --> B[Private witnessed checkpoint]
    B --> C[Reserve load without attempt]
    C --> D[Same-process native inspection]
    D --> E[Confirm restored body unchanged]
    E --> F[Consume one continuation grant]
    F --> G[Persist exact instruction acceptance]
    G --> H[Activate tools and messages]
    H --> I[Validate retained baseline plus new delta]
    I --> J[Publish witnessed child]
```

| Boundary | Required authority/evidence |
| --- | --- |
| Capture snapshot | Live uncancelled attempt; coherent head/work cut and bounded digest-only evidence. Claimed/uncertain tools and in-flight instructions cannot become completed evidence. |
| Publish | Immutable payload/reference and witnessed correspondence; compare current head, request digest and ownership transactionally. Content hash alone is insufficient. |
| Reserve load | Current checkpoint/configuration/session/work cut; exclusive restore lease; no fabricated attempt, model call or tool authority. |
| Validate live load | Inspect the actual same adapter process/import instance that would continue. Receipt echo, import success and stored bytes are insufficient. Unsupported inspection is explicit unsupported plus cleanup. |
| Confirm | Recheck live ownership/cancellation/configuration and selected checkpoint. Preserve prior body, outcome, result and durable work cut. |
| Authorize Continue | Actual first `Granted` owns the single-use permit and exact oldest instruction. `AlreadyAuthorized` is historical metadata and never recreates a permit. |
| Accept | Validate exact context/message identity and persist acceptance atomically before tool/message activation. Ordinary message acknowledgement cannot replace this transition. |
| Publish child | Preserve validated retained prefix and old attribution; include new delta/current cut under a checked parent and live owner. Fresh opaque capture is not restored-history proof. |

### Selected coordinator synchronization

The target uses one output reader and one shared phase latch with a generation
identity: Loading → AwaitAccepted → Buffered → Admitted. Change the latch before
enqueueing the sole Continue command. The reader compares the exact expected
private Accepted frame, enters bounded buffering, and can decode immediate
ordinary output without waiting for an acknowledgement queued behind that output.
Only the service's durable acceptance acknowledgement and current-owner check
activate delivery. Drain buffered events in order after activation. No second
receiver, respawn, ordinary Start or repeated rebinding is allowed.

The latch lock covers only phase/identity changes, never blocking pipe reads or
queue waits. Required positive frame/byte/deadline budgets bound buffering and
validation. Full-before-enqueue can retry the same unsent frame; disconnected or
uncertain delivery cannot resend. Buffer exhaustion and invalid phase/identity
stop the operation without inventing acceptance. Duplicate Accepted and old
loading frames fail in every later phase. This coordinator is a selected target;
the existing loading decoder alone does not implement it.

Potentially blocking native validation runs under a supervised process boundary.
The retained loaded-process owner must explicitly stop and join all owned workers
on timeout, cancellation, failed handoff or validation failure. Attempt every
cleanup even if one fails; report failures and preserve uncertainty. A cooperative
thread timeout or best-effort destructor is not cleanup evidence. Before admission,
use load-scoped supervised shutdown, not an attempt cancellation with a fake ID.

The service retains the real attempt fence when continuation authorization commits,
including when its reply is lost. Switch loading/attempt heartbeat generations
under the same ownership arbitration; a stale loading reply cannot affect the new
generation. User cancellation remains independent and survives that switch.
Failed handoff requires bounded termination/reconciliation by that service owner,
not closing an old load reservation and abandoning the admitted attempt. Lost
acceptance replies likewise confer no second delivery permit. Read-only lookup
can reconcile metadata, never reactivate execution.

### Storage invariants retained

Schema-11 witnessed baseline reads use one transaction, normalized evidence bounds,
current-witness validation and bounded parent/historical metadata inspection.
Do not recursively load ancestor bodies. Historical baseline integrity is distinct
from the new live-load validation binding. Typed continuation intent, acceptance
event/record correspondence, quota accounting and atomic rollback remain enforced
by their current store APIs. Neither SQLite commits nor these contracts promise
exactly-once filesystem/network execution.

## Acceptance evidence and implementation limits

| Subject | Existing evidence / required example |
| --- | --- |
| Lifecycle/messages/cancel | Contracts lifecycle serialization; store attempts/messages; runtime receipt and cancellation/process tests; service delivery fixtures |
| Capability | New pure negotiation examples plus existing receipt/capture/loading handshake fixtures; production integration must reject unsupported before effects |
| Permission | New bound request/decision examples for authorization, mismatch, expiry/cancel and no second consumption; existing static-grant tests cover their narrower layer |
| Event/projection | Service event sanitization and store revision/cursor fixtures; independent conversation/association models remain target-only |
| Private context | `context_codec`, `capture_handshake`, `context_v2` codecs and runtime loading tests establish exact framing/binding, not native inspection |
| Store restoration | Restore snapshot/lifecycle/race fixtures, continuation intent goldens, acceptance atomicity/quota/corruption/witness/lineage fixtures |
| End-to-end restoration | Still requires real capture A, load B, bound new instruction, witnessed child and load C, including inert import/prefix preservation and owned cleanup |

The contracts crate is the discoverable common vocabulary; concrete service/store
methods remain authoritative for current implementation. Future adapters, UI,
projection endpoints, permission persistence, native inspection and coordinator
activation must honor this specification and provide their own execution evidence.
No new ordinary protocol capability is advertised merely by publishing it.
