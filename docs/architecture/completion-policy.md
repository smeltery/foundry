# Task completion policy

Status: **approved 2026-11-04; implemented (NIC-106).** Policy identifier
`foundry.completion.v1`. This page defines when a finished attempt may be stored
as `success`, `failure` or `action_required`. "Implementation" below maps each
rule to the code that enforces it and lists the known limitations.

## Scope and non-goals

The policy classifies one attempt against criteria declared before the attempt
ran. It is not a correctness oracle for arbitrary natural-language tasks. It does
not judge provider quality. It does not change lifecycle semantics: `error` and
`suspended` still carry no outcome (see [session contracts](contracts.md)).

None of these is completion evidence: an SDK stop signal or settled turn, model
text such as "done", every tool returning success, a child process exiting 0, or
a fixture that hard-codes an outcome. A model may propose completion. It cannot
produce the evidence that authorizes success.

## Decision binding

Each classification is a decision record bound to this tuple:

| Field | Meaning |
| --- | --- |
| `policy_version` | `foundry.completion.v1`. A record under another version is not reinterpreted. |
| `session_id`, `attempt_id` | The fenced attempt that owns finalization. |
| `configuration_digest` | Effective adapter/model/permission configuration admitted for this attempt. |
| `criteria_digest` | The criteria set fixed when the attempt was admitted. It is immutable for the attempt. |
| `work_revision`, `event_sequence` | The durable work cut the evidence evaluated. |
| `checkpoint_cut` | Native publication only: the witnessed cut that carries the terminal. |

The record also stores the outcome, its basis (`verified`, `adapter_downgrade`
or `default`), a fixed cause code and per-criterion results. The result string
remains display data, not evidence.

## Evidence and its authority

| Source | Examples | Authority |
| --- | --- | --- |
| Adapter-reported | Settled `outcome`, `result` text, completion summary | May always lower the outcome to `failure` or `action_required`. Never sufficient for `success`. |
| Runtime-observed | Fence and lease validity, cancellation state, pending or in-flight messages, unresolved or uncertain tool calls, current work revision and configuration | Gates whether any `exit` outcome may be written at all. |
| Runtime-verified criteria | Result of an explicitly authorized, bounded check that the runtime ran under the attempt fence and recorded with the binding tuple | The only basis for `success` and for criterion-driven `failure`. |

A verification check must already appear in the admitted criteria set. The model
cannot add, select or widen one. Running a check grants no new permissions.

### Rejection rules

- **Stale:** the evidence's `work_revision`, `configuration_digest`,
  `criteria_digest` or `policy_version` differs from the values current when the
  attempt finalizes, or it belongs to an earlier attempt. The evidence is ignored.
  The affected criterion counts as missing.
- **Missing:** a required criterion has no current result. The outcome is
  `action_required` with cause `criteria_unverified`.
- **Forged:** the evidence was not written by the runtime under this attempt's
  fence. That covers anything embedded in adapter result text, model text, tool
  output or event data, and any event carrying another attempt ID. It is ignored
  as evidence. A wrong attempt ID on the wire stays a protocol error, so the
  attempt ends in `error`.

## Decision procedure

The owner evaluates these steps in one store transaction. It rechecks the fence,
cancellation, messages and work revision before committing, so a check that
passed earlier cannot be overtaken by a later change.

```mermaid
flowchart TD
    Start[Adapter settles attempt] --> Fence{Fence and lease valid?}
    Fence -- No --> NoWrite[No terminal write; explicit recovery]
    Fence -- Yes --> Cancel{Cancellation requested?}
    Cancel -- Yes --> Suspended[Suspended, no outcome]
    Cancel -- No --> Work{Unresolved tools or protocol fault?}
    Work -- Yes --> Error[Error or suspended, no outcome]
    Work -- No --> Newer{Newer instruction pending?}
    Newer -- Yes --> ActionNewer[action_required: newer_instruction_pending]
    Newer -- No --> Proposal{Adapter proposal}
    Proposal -- failure --> Failure[failure: adapter_downgrade]
    Proposal -- action_required --> ActionAdapter[action_required: adapter_downgrade]
    Proposal -- success --> Criteria{Every required criterion verified and current?}
    Criteria -- All pass --> Success[success: verified]
    Criteria -- Any fails --> FailureVerified[failure: criterion_failed]
    Criteria -- Missing, stale or none declared --> ActionDefault[action_required: criteria_unverified]
```

Rules the diagram compresses:

1. Lifecycle comes first. Cancellation, lease loss, protocol faults and
   unresolved tools decide the lifecycle before any outcome is considered.
2. A failed intermediate tool call that the attempt later recovered from is not a
   task failure. Only a failed required criterion, or an adapter's explicit
   `failure`, produces `failure`.
3. A permission request that needs the user stays `action_required` with cause
   `permission_action_required`.
4. If no criteria were declared, an attempt can never be classified `success`.

## Races and continuation

- **Cancellation versus settlement:** if cancellation commits before the terminal
  transaction, the attempt ends `suspended` with no outcome, even if the adapter
  already settled with `success`. Once a decision commits, a later cancellation
  request does not rewrite it.
- **New instruction versus settlement:** if a message is pending or in flight for
  the session when the decision commits, the criteria set no longer reflects the
  whole instruction. The attempt ends `action_required` with cause
  `newer_instruction_pending`. The message stays pending for a later attempt and
  is never redelivered automatically.
- **Same-session continuation:** each attempt gets its own decision record and
  its own criteria set. A continuation never reclassifies an earlier attempt's
  record. Restoring a checkpoint preserves the stored outcome unchanged (see
  [restoration](restoration.md)). `Session.outcome` reflects the latest finished
  attempt's decision.

## Presentation mapping

Every frontend reads the stored session. None reclassifies the outcome from text,
process status or event history.

| Stored state | CLI `run`/`prompt` | CLI `watch` | MCP `session_get` |
| --- | --- | --- | --- |
| `exit` + `success` | 0 | 0 | `isError: false`, `outcome: "success"` |
| `exit` + `failure` | 1 | 1 | `isError: false`, `outcome: "failure"` |
| `exit` + `action_required` | 3 | 3 | `isError: false`, `outcome: "action_required"` |
| `error` | 2 (execution error) | 2 | `isError: false`, `lifecycle: "error"`, no outcome |
| `suspended` | 130 | 130 | `isError: false`, `lifecycle: "suspended"`, no outcome |

MCP `isError` reports a failed tool invocation, such as an unknown session or a
storage error. It never reports a task outcome.

## Implementation

- **Lifecycle and outcome are separate types.** `Lifecycle` and `Outcome` are
  distinct enums and `Session.outcome` is optional
  (`crates/contracts/src/lib.rs`). `validate_terminal` requires an outcome for
  `exit` and forbids one for `error` and `suspended`
  (`crates/store/src/attempts.rs`).
- **Fenced finalization.** `check_terminal` checks the attempt fence. It forces
  `suspended` once cancellation is recorded and refuses `exit` while tool calls
  are unresolved or uncertain (`crates/store/src/attempts.rs`). The service
  re-reads cancellation after the adapter settles and after the checks, and
  retries a failed finish as `suspended` when cancellation won.
- **Criteria storage and admission.** `Store::create` accepts the `completion`
  option and keeps it in the immutable creation record; a profile reference is
  rejected as `unsupported` because no configuration profile source exists yet.
  `completion::admit` (`crates/service/src/completion.rs`) fixes the attempt's
  checks and configuration digest right after attempt admission. The merge rule
  for profile defaults (inline check with the same `id` replaces it, others are
  appended) is `admitted_checks`. A continuation inherits the session's criteria;
  `Service::declare_next_criteria` replaces them for the next attempt only.
- **Runtime checks.** After a `success` proposal, and only then, the service runs
  each admitted check under the live fence (`crates/service/src/completion_check.rs`).
  A command runs without a shell in the session directory, with no input,
  discarded output and an environment reduced to `PATH`, in its own process
  group that is killed at the timeout. Exiting 0 passes; any other status or the
  timeout fails. A file check opens the path through a capability handle on the
  workspace, so a path or symlink that leaves it is refused and counts as
  unverified. Durable cancellation is polled while a check runs and stops it.
- **Decision record.** Every `exit` terminal writes one `completion_decisions`
  row in the terminal transaction (`crates/store/src/completion_decisions.rs`),
  after the fence, cancellation and unresolved-tool checks. The row stores the
  binding tuple, outcome, basis, cause code and per-criterion results; native
  publication also stores the checkpoint ID. The checkpoint keeps the adapter's
  terminal proposal as historical data, while `Session.outcome` is the decision.
  Evidence is current only when its policy version, attempt, configuration
  digest, criteria digest and work revision equal the values at finalization and
  each result ID appears once. Results for checks that were not admitted are
  ignored. Adapter text and events are never read as evidence.
- **Cause codes.** `criteria_verified` (`verified`), `criterion_failed`
  (`verified`), `criteria_unverified` and `newer_instruction_pending`
  (`default`), and `adapter_failure`, `adapter_action_required` and
  `permission_action_required` (`adapter_downgrade`).
- **Presentation.** CLI exit codes and MCP outcomes read the stored session
  unchanged. `session_get` adds a `completion` object; its `policy_version` is
  null for a session without an outcome or with an outcome stored before the
  policy, which is never rewritten.
- **Attempt correlation.** Events naming another attempt are protocol errors.
  Adapter failures are stored with fixed, sanitized diagnostics
  (`crates/runtime/src/lib.rs`). Settling with outstanding tool work is rejected
  (`crates/runtime/src/coordinated/engine_events.rs`).
- **Adapter transport.** The adapter rejects outcomes outside the enum, pending
  tool calls and unaccepted messages, and defaults a missing completion to
  `action_required` (`packages/adapter/src/controller.ts`). Its `success` is only
  a proposal to the runtime.

Acceptance tests: `crates/store/tests/completion_decisions.rs` (stale, missing
and forged evidence, downgrade, pending instruction, cancellation, pre-policy
records), `crates/service/tests/completion.rs` (checks through the run path,
timeout, workspace escape, cancellation and new-instruction races,
continuation records), `crates/service/tests/publication_owner/completion.rs`
(native publication) and
`exit_zero_and_mcp_success_come_only_from_a_verified_decision` in
`apps/cli/tests/local_execution.rs`.

## Known limitations

- Named criteria profiles are unsupported until a configuration profile source
  exists.
- The admitted set and staged evidence live with the store connection that owns
  the attempt. An attempt finished through another connection falls back to the
  session's declared checks with no evidence, so it cannot be `success`.
- The configuration digest covers the adapter command, its timeouts, the stored
  model selection and whether tools and permissions were enabled. It does not
  cover individual tool grants. Native publication uses the profile's digest.
- On the native path, criteria are fixed at publication from the immutable
  creation record, and a per-attempt override does not apply there.
- The CLI and MCP have no option to declare criteria yet; sessions created
  through them have none, so a settled turn is `action_required`.

## Approved decisions

The user approved these on 2026-11-04:

1. **No adapter attestation.** An adapter's `success` never counts without
   runtime-verified criteria. With no criteria checked, the attempt is stored as
   `action_required` with cause `criteria_unverified`. This changes current
   behavior, where adapter-reported success is stored as `success`.
2. **Criteria source and v1 check kinds.** A configuration profile supplies default
   criteria, and the session create request may add to or override them. The
   admitted set is fixed when the attempt is admitted. v1 allows two check kinds:
   a bounded command that must exit with status 0, and a file that must exist or
   match a recorded digest.
3. **Newer instruction.** A message pending at settlement ends the attempt as
   `action_required` with cause `newer_instruction_pending`. The message stays
   queued for the next continuation and is never redelivered automatically.
4. **Continuation criteria.** A continuation inherits the session's criteria unless
   its new instruction declares its own.
5. **Adapter downgrade.** An adapter's explicit `failure` is accepted without
   verified evidence, because adapter reports may only lower an outcome.
6. **Existing records.** Records written before this policy keep their stored
   outcome and expose `policy_version: null`. They are never rewritten.
