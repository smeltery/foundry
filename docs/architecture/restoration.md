# Checkpoint restoration

Schema version 9 separates loading saved context from authorizing another turn
and supports atomically binding a continuation to a queued instruction.
These are storage APIs for an execution owner. They do not enable CLI resume or
provide native model-context validation. See [checkpoint storage](storage.md#opaque-context-checkpoints)
for capture and publication guarantees.

## Loading does not start work

`begin_restore` reserves a session under a new restore lease. It requires the
current checkpoint head, expected session version, matching configuration digest
and an unchanged durable work cut. Ordinary attempt admission and restore
reservation exclude each other in the same database transaction, including an
expired owner that has not yet been recovered.

The execution owner loads the private bytes without running a turn or dispatching
tools. After independently validating the native state, it calls
`confirm_restored`. This records the owner's confirmation; storage cannot verify
that native loading occurred. Neither call changes the prior session body,
outcome, result or work revision.

```mermaid
stateDiagram-v2
    [*] --> Loading: Reserve current checkpoint
    Loading --> Restored: Owner confirms native validation
    Restored --> Continuing: Commit one continuation authorization
    Loading --> Closed: Cleanup or lease recovery
    Restored --> Closed: Cleanup or lease recovery
    Continuing --> Finished: Attempt completes
    Continuing --> Expired: Attempt lease expires
```

The diagram groups pre-execution closure outcomes. Stored phases distinguish
closed, failed, cancelled and expired reservations. Closing or recovering a
loading/restored reservation preserves the prior session body exactly. A
cancellation request retains its reservation until cleanup or expiry recovery;
another owner cannot acquire the session while that reservation remains active.

## One authorization, no automatic replay

`authorize_continue` rechecks the restore lease, cancellation, checkpoint head,
configuration, session version and durable cut. It then creates an ordinary
attempt, advances the work revision, changes the session to running and records
the continuation receipt in one transaction. Only the first committed call
returns a `ContinuePermit`, consumed to obtain the new attempt fence.

An exact retry returns `AlreadyAuthorized` metadata, even after the attempt
ends. It never returns another permit. Reusing a request with different content
is a conflict. Begin retries likewise return metadata without renewing or
reissuing the loading credential. Retain the original credentials while their
owner remains alive; lookup is not execution authority.

| Failure point | Durable result and required handling |
| --- | --- |
| Before continuation commit | No work admitted; close or recover the loading reservation |
| Commit succeeds but its response is lost | Authorization is spent; lookup cannot recover a delivery permit |
| Command delivery or execution becomes uncertain | Do not automatically resend; recover the expired attempt and reconcile effects |
| A later attempt changes the session | Historical receipt remains historical; inspect current session separately |

The ordinary attempt lease governs continuing execution. Its expiry recovery
suspends uncertain work and marks the linked restore expired. Completion updates
the linked restore in the same transaction as terminal attempt finalization.
Neither path reissues continuation permission or undoes external effects.

## Continuing with an exact queued instruction

`authorize_continue_message` additionally takes a message ID and SHA-256 of its
exact UTF-8 text. The selected message must be the oldest pending instruction.
An older pending message, mismatched text, withdrawal or cancellation rejects the
operation; storage never substitutes another instruction.

```mermaid
sequenceDiagram
    participant Owner
    participant Store
    Owner->>Store: Authorize exact pending message and text digest
    Store->>Store: Check restored ownership, head, cut and FIFO selection
    Store->>Store: Admit attempt and claim message in one transaction
    Store->>Store: Record bound receipt and commit
    Store-->>Owner: One permit containing fence and exact claimed message
    Owner->>Store: Retry same request after a lost response
    Store-->>Owner: Historical metadata only
```

Admission and claiming each advance the work revision once; the session version
advances only for admission. Failure at either stage rolls back both changes.
The returned `ContinueMessagePermit::into_delivery` consumes the permit and
returns the new attempt fence plus the claimed message. It does not send a command.
The message remains in flight until the execution owner persists a validated
acceptance; other message claims cannot bypass that unresolved delivery.

The receipt records message identity, sequence, text digest and byte length, with
no text or credentials. Exact retries remain metadata-only after acknowledgement,
expiry or later attempts. Generic and message-bound authorization cannot be
reinterpreted as each other. Version 9 preserves earlier generic receipt hashes
and adds no historical claims during migration. Ordinary queue identifier behavior
is unchanged; this context-bound operation has stricter identifier limits.

Checkpoint references also expose the verified stored `record_digest`. Owners
must preserve its bytes when binding a native restore exchange; the request digest
and a hash of the record digest are different values. This is integrity metadata,
not proof of native state or permission to execute.

## Validation boundary

Restore history retains at most 1,024 rows per session and 65,536 per database.
New reservations fail at capacity; exact retries, cleanup and already-reserved
continuation remain available. History is not evicted to make room, preserving
the record of spent authorizations. These are logical row limits, not total
database, disk, WAL or process-memory guarantees.
Each message-bound authorization adds at most one receipt row under the same
restore-history limits; it does not duplicate the instruction text.

`recover_expired_restores` processes at most 128 reservations per transaction,
ordered by expiry and then ID. Repeat it until it returns an empty batch to drain
currently eligible reservations. A failed batch rolls back all its changes and
returns no recovered results. Continuing attempts use ordinary attempt recovery.

The trusted owner must compute configuration and delivery digests from the
effective settings and intended command. Echoed adapter hashes are insufficient.
Record hashes detect inconsistent stored identities and receipts; they do not
authenticate an attacker-controlled database or detect whole-database rollback.

Storage tests cover competing ordinary/restore owners, competing continuation
requests, cancellation during ownership transfer, lost-response lookup,
historical receipt corruption, migration and rollback after injected write
failures. Native validation, restored execution and CLI integration remain
separate work. The [fresh capture supervisor](runtime.md#fresh-capture-supervision)
does not establish a restored baseline. Passing these storage tests establishes no model-provider
or full-session restoration claim.
