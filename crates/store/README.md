# Foundry store

SQLite persistence for shared session contracts. Writes use immediate transactions;
the database uses WAL, full synchronization, foreign keys and versioned migrations.
The database's parent directory must already exist.

| Method | Result |
| --- | --- |
| `Store::open(path)` | Store with a 60-second lease duration |
| `Store::open_with_lease(path, duration)` | Store with a configurable lease duration |
| `create(CreateSession)` | Queued session or the current session for an identical idempotent request |
| `get(session_id)` | Session, or a not-found error |
| `list(limit)` | Sessions in descending creation order |
| `begin_attempt(session_id, expected_version)` | Exclusive `AttemptFence` |
| `heartbeat(&fence)` | Renew a still-valid lease |
| `append_event(&fence, id, revision, kind, data)` | Persisted event or exact duplicate's original event |
| `events(session_id, after, limit)` | Revisions in sequence order after an exclusive cursor |
| `finish_attempt(&fence, lifecycle, outcome, result)` | Updated session |
| `request_cancel(session_id, mode)` | Session with a durable cancellation request |
| `cancellation(&fence)` | Current cancellation mode, if requested |
| `recover_expired()` | Sessions explicitly suspended after their leases expire |
| `work_revision(session_id)` | Durable admission/evidence counter, independent of session version |

Mutations require `&mut Store`. Separate processes may open independent connections.
Attempt fences carry an opaque ownership token; keep it private. Renew leases while
tools run. An expired owner cannot renew, append, or finish; explicit recovery must
release its claim before another attempt can begin. Opening a store never recovers
attempts. Recovery preserves event history and does not invent tool results or
successful outcomes. It cannot stop an external process or undo its side effects.
The runner must stop execution after losing its lease and reconcile uncertain tools
before resuming. Lease expiry uses system UTC time, so clock changes affect expiry.

Session versions advance on lifecycle or cancellation changes. Events use independent
per-session sequences starting at one, retained across attempts. Every revision is
immutable; event IDs remain attached to one attempt and kind. Exact retries are
idempotent while the fence is valid. Revision numbers must increase, but need not be
consecutive. SQLite integers support values through `i64::MAX`; larger unsigned
inputs fail explicitly. Zero list/event limits return empty pages.

`Exit` requires an outcome; `Error` and `Suspended` require no outcome. Queued/running
are not valid completion states. Cancellation upgrades from graceful to hard, never
backwards. A cancelled active attempt must finish suspended. Cancellation does not
prove a process has stopped; the execution owner must acknowledge it through finish.
Repeated requests on a settled session are no-ops.

Schema version 5 tracks work admission independently of events. Starting an
attempt, claiming a message or tool, recording a changed receipt, and appending a
new event increment this counter transactionally. Exact retries and nonexecution
operations do not increment it. Overflow rejects the entire mutation. Migrated
sessions start at zero without reconstructing historical work; the revision is
not a saved conversation or a resume permit. See the
[storage architecture](../../docs/architecture/storage.md#durable-work-revision).

Fork, rewind, tool-side-effect reconciliation and automatic execution resumption are
not implemented. This store provides no claim of exactly-once external execution.

## Instruction queue

The instruction queue provides `enqueue_message`, `messages`, `withdraw_message`,
`claim_next_message` and `acknowledge_message`. Message IDs are idempotent within a session; changing the
text under an existing ID is a conflict. Ordered cursors include pending,
in-flight, delivered and withdrawn records. An identical retry preserves the current state,
including withdrawal. Enqueueing never starts an attempt or changes its outcome.

Claiming requires a valid attempt fence and atomically prevents withdrawal before
the owner sends an instruction. Only one message may be in flight per session.
Acknowledgment requires the same valid attempt fence and records delivery, not
execution success. The application must obtain an adapter acknowledgment before
marking delivery. After an uncertain crash, the in-flight record remains and
blocks later claims; no automatic redelivery occurs. Explicit reconciliation is
required and is not implemented yet. The application service integrates live
adapter receipts; this storage API only enforces durable ownership and state.
No exactly-once delivery is promised.

## Tool-call records

Schema version 4 adds `claim_tool`, `finish_tool` and `tool_call`. The trusted
execution owner must commit a claim before starting a helper, then commit observed
completion before returning a result to the adapter. Records contain identifiers
and 32-byte input/output digests, not file contents or credentials. The caller
must compute digests over its complete invocation and result; the store does not
verify filesystem effects.

Claims require a valid, uncancelled attempt. Repeated call IDs never issue a new
dispatch permit, even with identical input. Only one unresolved call may exist
per session. A crashed claim or explicitly uncertain outcome survives recovery
and prevents further dispatch, including from a new attempt. Explicit effect
reconciliation remains unimplemented.
An unresolved call also prevents terminal `Exit` outcomes across attempts;
`Error` and `Suspended` remain available for cleanup without claiming completion.

Success requires an output digest. `Failed` means the caller has evidence of no
effects; a write failure with unknown effects must use `Uncertain`. Completion
retries with the same outcome and digest are idempotent while the attempt fence
is valid; conflicting receipts are rejected. Cancellation or expiry prevents
completion from being recorded as success. SQLite and filesystem changes are
not one transaction, and these records do not promise exactly-once execution.
The application service coordinates helper dispatch with these records; this
storage API does not itself launch helpers.

## Opaque checkpoint API

Schema version 6 adds private byte storage with atomic head publication:

| Method | Result |
| --- | --- |
| `checkpoint_cut(&fence)` | Consistent work revision and event/input/tool digests |
| `capture_snapshot(&fence)` | One fenced snapshot of head, cut, sorted coverage and digest-only evidence |
| `commit_opaque_checkpoint(&fence, input)` | Immutable reference after fenced head comparison and publication |
| `checkpoint_head(session_id)` | Integrity-checked metadata for the latest head |
| `read_opaque_checkpoint(session_id, &id)` | Private bytes, coverage and annotation with integrity checks |
| `checkpoint_request(session_id, request_id)` | Read-only metadata lookup for an existing request |
| `finish_with_opaque_checkpoint(&fence, input, terminal)` | Schema-7 atomic checkpoint and terminal completion with historical receipt |
| `finished_checkpoint_request(session_id, request_id, expected_digest)` | Exact read-only recovery of historical finalization |

Capture `input.request_digest(&fence)` before publication and compare it with the
returned reference for exact lost-response reconciliation. Request-ID lookup
alone is not an acknowledgement of the caller's intended bytes. A stored
configuration digest or producer annotation does not authorize restore. See
[checkpoint guarantees and limits](../../docs/architecture/storage.md#opaque-context-checkpoints).

`capture_snapshot` returns the original prompt's UTF-8 byte length/SHA-256,
delivered message identities/lengths/digests, and completed tool ledger evidence
across attempts. Pending/withdrawn messages are excluded; in-flight messages and
claimed/uncertain tools reject the whole snapshot. A missing tool output digest
remains `None`, not invented evidence of delivery. Raw contents and owner tokens
are not returned, and these facts do not establish native inclusion or validation.

Before copying evidence, capture enforces **1,024 total evidence entries** including
the original prompt, **64 KiB aggregate evidence identity/name bytes**, and
**64 KiB exact JSON-encoded coverage** including escaping. Coverage ID copies add
at most another bounded 64 KiB. Prompt/message text is limited to 256 KiB and each
event data field to 2 MiB; context-compatible identifier/type checks also apply.
Oversized direct-store records remain intact but are ineligible for this capture
API; successful snapshots are never truncated. Existing cut hash encoding is
unchanged and hashes borrowed row values. SQLite cache, JSON extraction, sorting,
scan time, and total process RSS are not bounded by these DTO limits.

Atomic finalization instead uses `input.finalization_request_digest(&fence,
&terminal)`, binding the proposed outcome and result. Recovery returns historical
state without changing the current session or accepting an inactive fence for
new writes. The terminal proposal shares the checkpoint metadata size limit.

## Restore ownership and retained history

Schema 8 separates loading ownership from task execution. `begin_restore` and
`confirm_restored` preserve the prior session body and work revision. Cancellation
retains that ownership until cleanup or expiry. `authorize_continue` commits one
execution authorization before returning a permit; exact retries return metadata
without another credential. These APIs do not establish native restore semantics.

Restore history is retained for idempotency, including closed, failed, cancelled,
finished and expired rows. New requests are limited to **1,024 retained rows per
session** (`MAX_RESTORE_ROWS_PER_SESSION`) and **65,536 per database**
(`MAX_RESTORE_ROWS_PER_DATABASE`). Both counts are checked inside the admission
transaction, after exact-retry lookup. Reaching either cap rejects new requests
without deleting history; exact retries and cleanup remain available. Existing
history above a cap is retained and blocks further admission.

These caps allow repeated load-only attempts while bounding newly admitted
restore history. They are logical row limits, not full database, disk, WAL, or RSS
bounds; other tables and existing databases can be larger. No automatic pruning
or history-reset API is provided.

`recover_expired_restores` processes at most **128 rows** per transaction
(`RESTORE_RECOVERY_BATCH_SIZE`), ordered by expiry time and then binary ID.
Call repeatedly until it returns an empty batch. Results become visible to the
caller only after commit; a failed batch rolls back entirely. Loading/restored
expiry preserves the prior session body. Continuing attempts use the ordinary
attempt recovery API. The batch bounds returned restore records, not all SQLite
scan/sort memory or the separate ordinary-attempt recovery operation.

## Witnessed loading and acceptance

Schema 10 adds `finish_with_witnessed_checkpoint` and `read_checkpoint_witness`.
Private witnesses bind complete durable instruction/tool evidence to the payload,
configuration, cut and immediate parent. Request format 2 includes the witness;
legacy opaque request hashes remain unchanged and cannot be reinterpreted as
witnessed records. Publication is atomic with terminal finalization.

`restore_baseline_snapshot(&RestoreFence)` returns the selected private checkpoint,
witness, bounded facts and sanitized event evidence in one owner-checked read
transaction. It does not start execution. Schema 11 adds typed prebound continuation
intents, `accept_restore_continuation` for atomic event/acknowledgement/acceptance
persistence, and `continuation_acceptance` for historical recovery by request digest.
Later witnesses bind the original acceptance record digest.

The caller validates native loading and acknowledgement. Storage verifies durable
correspondence and grants no authority through historical lookup. See the
[restoration contract](../../docs/architecture/restoration.md) for limits, exact
retries, migration and cancellation behavior.

Run `cargo test -p foundry-store` and
`cargo clippy -p foundry-store --all-targets -- -D warnings`.
