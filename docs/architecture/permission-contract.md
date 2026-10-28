# Permission request contract

The contracts package provides a pure permission state machine for a trusted
execution owner. It separates a policy decision, issuance of one permit and
consumption immediately before the bound call is admitted. It implements no UI,
network request, tool execution, credential discovery or ambient clock access.

This is additive to [workspace grants](../../crates/tools/PERMISSIONS.md).
Existing grants or explicit user authorization may satisfy policy immediately;
there is no always-ask requirement. A tool request from an adapter is input to
policy, never permission authority. The caller must establish authorization before
calling `decide(Allow)`; the type system cannot authenticate its caller.

## Binding and ownership

`PermissionBinding` fixes session, attempt, call, tool, input digest and effective
profile. The owner computes the digest from its canonical validated input under
a stable policy; it must not trust an adapter-supplied digest. No raw input or
credential is stored by this contract. The binding is immutable inside the
request and returned only by shared reference. A caller's cloned binding is
metadata, not authority.

Current and legacy are distinct profile values, consistent with the
[command profile contract](cli-contract.md#profiles-and-explicit-selection).
A decision cannot switch profiles or authorize another attempt, call, tool or
input. The owner must invalidate requests when policy, attempt ownership or
profile validity changes. This module does not independently negotiate profiles,
verify leases or inspect runtime state.

The trusted owner maintains one authoritative request per admitted call and
serializes decisions, cancellation and consumption. Constructing another request
is not an idempotent retry. The library does not persist a request ledger or
prevent a malicious caller from making a new request and granting it again.
Permission permits use a private per-request identity, so even separate requests
with identical public bindings cannot exchange permits.

## Decisions and one-use consumption

| State or operation | Contract |
| --- | --- |
| New request | Pending, with a valid binding and future expiry supplied by the owner. |
| Pending decision | No authority issued; the owner can later resolve policy. |
| Allow | Allowed, without dispatch. Existing authorized grants may produce this directly. |
| Deny | Denied; no permit. |
| Unsupported | Required decision capability is unavailable, distinct from denial. |
| Repeated same terminal decision | Metadata-only acknowledgement of current state; no new permit. |
| Different terminal decision | Fixed conflict error; the first decision remains authoritative. |
| Take permit | Allowed becomes Issued exactly once. Permit loss does not allow reissuance. |
| Consume permit | Verify private request identity, full current binding, time and Issued state; mark Consumed before returning success. |
| Cancel or invalidate | Revoke pending, allowed or issued authority. Consumed stays historical; no rollback of an admitted effect is claimed. |

`PermissionPermit` has no public constructor, Clone, Copy, Debug or serialization
implementation. `consume` takes ownership, so even failure consumes that object.
No permit or dispatch is retried automatically. A failed binding comparison marks
the request stale. A foreign permit does not change the receiving request's
valid authority. The original issuer still cannot reissue its lost permit.

The consumer must call `consume` with trusted current binding and time, and admit
the call only on success. Checking `state` or `disposition` is not consumption.
Allowed disposition includes an already issued policy decision; it cannot issue
a second permit. If a consumed result is lost or dispatch later fails, reconcile
that uncertainty instead of issuing again. This contract does not promise
exactly-once external effects or atomicity with a database or operating system.

## Time, cancellation and noninteractive clients

All time values are explicit comparable `u64` ticks in one caller-owned clock
domain. Expiry is exclusive: `now >= expires_at` revokes unconsumed authority.
Backwards time is an error. No time unit, wall clock, scheduler or expiry duration
is inferred. Decisions, issuance and consumption recheck live time; an expired
allow cannot be used simply because it was granted earlier.

Cancellation and invalidation take effect when the authoritative caller applies
them. The caller must synchronize external cancellation and the final admission
boundary; this in-memory object cannot observe another process automatically.
A cancelled, expired or stale request rejects late decisions and permits. An
already consumed request remains historical after cancellation or expiry.

Pending mode is explicit: an interactive-capable caller can expose Pending;
a noninteractive caller with outcome support exposes ActionRequired; a caller
without that capability exposes Unsupported. These dispositions do not turn
pending into denial or allowance and do not secretly mutate the policy decision.
A later authorized owner may resolve a still-live pending request. Unsupported
as a final policy decision remains a terminal decision for that request.

Errors contain fixed categories and messages, never input bytes or identifiers.
Binding and request values are not serializable or printable by this API. Safe
state/error summaries can be exposed by the owner, but any durable approval
record, transport schema or public identifier policy needs separate review.

## Acceptance evidence and limits

[Contract tests](../../crates/contracts/tests/permission.rs) use a consumer that
increments its dispatch counter only after production `consume` succeeds. Cases
cover direct grant allowance, denial and unavailable capability, noninteractive
pending outcomes, every binding field, profile/attempt changes, request identity,
exclusive expiry, cancellation, invalidation, backwards time, duplicate decisions
and lost permits. Successful allow dispatches once; revoked or mismatched permits
dispatch zero times. These fixtures use no real tools, accounts or UI.

The pure tests establish the request/decision contract. They do not establish
runtime wiring, durable approval recovery, OS containment or authorization of
untrusted frames. The existing tool host must continue enforcing its own grants,
input validation and workspace capabilities even when a trusted owner consumes
permission for a call.
