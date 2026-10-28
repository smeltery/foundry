# CLI lifetime and entity contract

This document defines the full product target for command lifetime, connection
profiles, privacy and session entities. A target requirement is not a claim that
its backend exists. The implementation boundary below describes current local
behavior; unresolved behavior is retained explicitly and cannot silently become
an implementation default or a removed requirement.

## Profiles and authority

**CURRENT is the default target. LEGACY remains a required historical profile.**
The canonical [command contract](cli-contract.md#profiles-and-explicit-selection)
selects the profile through `FOUNDRY_PROFILE=current|legacy` before parsing.
Absence selects current; empty or unknown values fail before state or credential
access. These are product target rules, not an implemented environment reader or
new parser flag. The [settings contract](cli-settings-contract.md) owns the
configuration boundary.

Carry the complete selected profile in execution requests and persisted connection
configuration. The execution owner must acknowledge it or return unsupported
before admission, never substitute another profile or backend. A profile change
must not silently alter an existing session; observers attach to its recorded
profile. Both CURRENT and LEGACY remain qualification obligations.

Detailed operation contracts take priority over broad feature descriptions.
Platform-specific requirements take priority over generic execution descriptions.
HTTP schemas describe their own operations; they do not define undocumented CLI
syntax, text tables or JSON envelopes. Existing local behavior does not replace a
target requirement. An unresolved mapping stays unresolved across implementations.

## Command lifetime

| Command or input | CURRENT target |
| --- | --- |
| Positional prompt without a target | Run on the local computer connection in the current folder, retaining uncommitted files. Establish the connection on first use when allowed. An outside-home folder belongs to the same computer identity. A disconnected marker prohibits implicit reconnection. |
| No prompt | Open the interactive interface. An unsupported result is an implementation limit, not completion of this target. |
| `run --task TEXT` | Cloud by default and noninteractive; wait for the outcome unless detached. A local adapter must not silently substitute local execution. |
| `session start --prompt TEXT` | Detached cloud compatibility operation. Use a scratch session when no explicit repository or applicable association supplies one. Do not turn it into synchronous local execution. |
| Closing a local task terminal | The connection and session survive. Terminal closure is not explicit cancellation. |
| Root or nested watch | Observe through terminal-output drain. Do not admit another attempt or take over a healthy connection. Finished sessions are read-only observations. |
| Root or nested send | Keep the same session and message identity. Steer active work through its owner; resume suspended work with the instruction only when authorization, target and verified context allow it. Durable queue acceptance, delivery and completed work are distinct. Exact waiting and return behavior remains unresolved. |
| Root resume | Reopen the same session and history. Display alone does not authorize a new instruction. Continuation requires explicit instruction or action; automatic-start and noninteractive details remain unresolved. |
| Graceful cancel | Record a request, stop execution through its owner, attempt a final workspace snapshot and settle suspended. Acceptance does not prove termination or snapshot success. |
| Hard cancel | Upgrade a pending graceful request, stop without waiting for a fresh snapshot, and use the last verified checkpoint for later continuation. Never downgrade hard cancellation. |
| Disconnect | Stop work on the personal connection, remove the connection and forget its renewal key. Persist the disconnected marker until explicit connect. Named disconnect removes this computer's workers for that machine. |
| Stop or `serve NAME --stop` | Stop local workers and remove background services while retaining registration for restart. This is distinct from named disconnect. |
| `connect --once` | Keep the connection only for the command lifetime and remove the ephemeral connection on exit; do not save a persistent identity. |

A selected target that is unavailable must fail explicitly, without cloud or
other-computer fallback. One session runs on one computer at a time; a machine
is a queue across workers. Moving a conversation does not move unpushed files.
Removing a folder leaves running sessions in other folders unaffected. Deleting
a managed session removes its managed worktree and retains its branch; managed
path and branch naming are owned by the state contract.

Closing a terminal, interrupting observation and cancelling execution are
separate inputs. CURRENT observer interruption stopping only observation is a
provisional policy, not a settled compatibility guarantee. Execution interruption
requests graceful stop first and forced stop on a second interruption. Exact
signal races, first-interruption exit and final framing remain unresolved.

### LEGACY lifetime obligations

A local prompt serves its folder while the command stays open. If no other
command serves that folder, watch temporarily serves it until the session ends.
If the original command still serves it, or the session is finished, watch only
reads. Send can serve the folder for a follow-up and wait for its result; first
interruption cancels and a second forces stop.

These cases must remain separately testable. They must not cause every CURRENT
watch to launch a worker. Abnormal exits, recovery timing and exact historical
external grammar/version availability remain unresolved; no inferred timing
substitutes for them. Product profile selection is fixed above.

### Execution ownership and restoration

Rust owns session admission and durable execution authority. Permit at most one
authorized active attempt per session. Observation grants no execution authority.
Continuation creates a new attempt within the existing session. Loading saved
context and authorizing continuation are separate transitions; restored bytes do
not themselves permit model or tool execution.

A verified context digest does not verify a filesystem snapshot. Failed final
snapshot publication preserves the previous verified checkpoint. Unknown tool
effects after interruption must not be replayed automatically. Receipt loss,
acceptance, delivery and completed execution remain distinct even after recovery.
A queue-only successful response does not establish suspended-session continuation.

## Deprecated capacity and device identity

| Control | CURRENT target | LEGACY obligation |
| --- | --- | --- |
| `connect --sessions VALUE` | Accepted and inert: no concurrency cap, scheduling change or admission restriction. Omission has no capacity effect. | Operational limit from 1 through 4; personal default4, machine default1. |
| `connect --cloud-sessions on\|off` | Both values are inert. | Historical active effect remains unresolved; invent none. |
| `device` | Alias of connect with equivalent request/options; malformed commands must not become task prompts. | Preserve the separate device connect/status/disconnect command family. |

The current shell consumes capacity as opaque text and omits both deprecated
options from the effective request. Known positive numeric cases and toggle
inertness are required fixtures. Full numeric grammar, boundaries, warnings and
historical availability remain unresolved. A digits-only parser is a provisional
alternative, not an acceptance gate; do not narrow current accepted tokens on
that basis. No warning is required or invented, especially on machine stdout.
Machine-queue capacity is independent of these CURRENT no-op controls.

## Connection, platform and privacy distinctions

CURRENT personal execution uses the user's permissions. A computer identity is
scoped to host, workspace and person; its folders do not create extra computer
identities. Personal is the initial visibility. Team visibility allows workspace
members to start sessions. Multiple accounts stay separate, and logout affects
only its own account. Plain login, API-key and once enrollment remain in one
workspace. Eligible setup or application enrollment can connect to as many as
eight additional workspaces; exact setup syntax and eligibility remain unresolved.

LEGACY macOS devices have a managed confined workspace and explicit no-sandbox
opt-out. Transition on next start to CURRENT user-permission execution is an
observable migration, not a silent reinterpretation of stored confinement.
Migration notification and consent remain unresolved; this contract performs or
authorizes no migration. Machine `serve --sandbox` is distinct: CURRENT sandboxed
machines cannot host sessions yet. Preserve that unavailable capability and the
LEGACY confined-worker obligation separately. A subprocess is not an OS sandbox.

| Platform or capability | Required distinction |
| --- | --- |
| macOS personal connection | Own-Chrome integration and keep-awake default on; preserve explicit controls. Own Chrome is distinct from a testing browser installation. |
| Linux connection/service | User-permission execution; service starts at login and at boot when lingering is permitted. Do not import macOS credential-store or permission-pane guarantees. |
| Windows connection | Retain x64/arm64 beta scope, Git Bash, Python for workspace checkpoints and first-connect administrator approval for command-sandbox setup. Generic user authority does not cancel this setup requirement. Exact isolation remains unresolved; release signing is a separate contract. |
| Desktop/browser installation | macOS, Linux and Windows beta; idempotent installation/update and minimal-mode handling. Historical narrower platform/component profiles remain obligations, not grounds to drop Linux. Component versions and OS constraints remain unresolved. |
| macOS permissions | Default none reports missing grants or leaves first-use prompts; standard requests Screen Recording and Accessibility; all adds Full Disk Access. Do not claim direct OS prompting for Full Disk Access. Historical broader permission-pane coverage remains unresolved. |

Desktop minimal prevents adding components while maintaining an installed profile.
Desktop install reverses minimal and restarts a connection when new capabilities
require it. Status distinguishes missing, outdated, needs-permission and ready;
an attempted installation is not ready. Browser-use reports own-Chrome readiness
on macOS. Mentions enumerates running applications and connected-profile tabs,
without reading page contents. Its selected product target is JSON-only, with
both supported global JSON selectors; reject absent/text format before desktop
access. Mandatory-format behavior and availability outside macOS remain
unresolved, and this target is not a claim about the present parser.

Privacy requirements are operation-specific:

- Plain login grants account/workspace access, not discovery of local provider
  credentials. An unconfigured adapter must not discover personal resources.
- Explicit connection setup can hand off current access tokens under account
  policy, never refresh tokens. Local-login defaults are on for the ordinary
  profile and off with an API key. Explicit/retained on with a key remains a
  policy question; neither silently allow nor ban it. Preserve off independently.
- Retained API keys support connection renewal and are forgotten on disconnect.
  macOS uses its credential store; other-platform protection remains unresolved,
  with no implied plaintext fallback.
- Own-Chrome off, local-logins off and minimal are independent controls. Minimal
  does not prove capability removal or erase an installed desktop profile.
  Confirmation flags grant no unrelated tool or credential authority.

## Entities and repository selection

| Entity | Product semantics |
| --- | --- |
| Project grouping | Membership only; no implied repository, machine, network host or execution target. |
| Persistent assistant | A distinct durable association with instructions and default repositories. Messaging it to decide how to work differs from creating a session associated with it. |
| Session | Stable task/conversation identity, history and target associations, lifecycle and optional outcome/result. Follow-up, retry and machine movement preserve it. |
| Attempt | One admitted execution generation with immutable model/configuration and execution authority. Changing model configuration requires another attempt, not in-place mutation. |
| Message | A durable instruction identity. Queue acceptance, adapter delivery and completed work are separate states/facts. |

Repository selection is explicit repository first; explicit supported suppression
of defaults selects scratch when no explicit repository is supplied. Otherwise,
a persistent-assistant association supplies its first default repository when
available. Group-only association supplies none and therefore yields scratch.
Target selection is independent. Persistent instruction precedence remains
unresolved. Do not treat conflicting association mappings as proof that grouping
and assistant entities are interchangeable. Exact compatibility field mapping
remains an external interface decision; internal concepts must stay distinct.
Where the HTTP association operation accepts both references, equal references
are permitted and unequal references must fail with status400. That equality
constraint does not resolve the behavior of a grouping-only compatibility request.

Lifecycle running/exit/error/suspended is separate from outcome
success/failure/action-required. A terminal process, null result or absent outcome
is not task success. Internal queued/admission states must not silently extend
another public lifecycle vocabulary. Archive selection is distinct from lifecycle
filtering; sparse list output retains session identity.

Resume reopens or continues an existing session. Retry repeats its last turn in
the same session/workspace after settlement and rejects a still-running session.
Fork creates a distinct session without changing the original. Rewind replaces a
selected conversational turn and continues with auditable history; it does not
imply filesystem rollback. Compact manages context without deleting transcript
history. Keep these operations distinct; do not add CLI verbs for API-only work.

### Independent read projections

Messages contains follow-up and mid-turn instructions with delivery notes,
excluding the opening prompt and agent replies. A new untouched session has an
empty messages view. Conversation includes the opening prompt, later messages
and turn replies with status/outcome, excluding delivery notes. It cannot be
built from messages alone. Require one final reply per completed turn; interim
reply timing remains unresolved.

Events contains intermediate text, tool and diagnostic activity. Revisions replace
visible data for a stable event identity; display order uses immutable position,
while delivery sequence advances. Preserve attempt-scale sequence jumps and do
not count items from cursor values. Messages, conversation and events have
independent paging tokens. Public number/string representation, overflow behavior,
framing and exact envelopes remain separate interface decisions.

## Current implementation boundary

The [local workflows](../user/cli.md) implement explicit-database operation,
synchronous configured adapters, queue receipts and polling watch. They do not
provide cloud-default run/start, persistent personal connections, interactive
execution, remote targeting or a verified CLI resume workflow. Local session
start can queue or execute synchronously; that limitation does not replace the
detached cloud target. Watch currently interrupts without cancelling the task;
this does not settle the unresolved profile behavior above.

The shared contracts contain session, message and event records, but no project,
persistent-assistant or conversation DTO. Current event sequence is a local
numeric counter without an immutable position field. The target projections are
not certified by those smaller DTOs. [Restoration](restoration.md) provides
owner-fenced storage operations and continuation permits, not native context
validation or an end-user resume command. [Injected configuration](cli-configuration.md)
expresses selected settings and intent, not enrollment, desktop or service effects.

## Required neutral scenarios

Use synthetic text/identities and inject `FOUNDRY_PROFILE` before parsing, outside
argv. Verify absent/current/legacy and empty/unknown values, propagation to the
owner, acknowledgement or unsupported before admission, and observation of an
existing session's recorded profile. These are acceptance scenarios, not claims
of tests already passing.

| Scenario | Required observation |
| --- | --- |
| CURRENT prompt, close terminal, reattach | Same session/connection; observation adds no attempt. Disconnect prevents implicit reenrollment. |
| LEGACY watch/send with and without a folder owner | Correct temporary serving versus read-only behavior; no duplicate owner; send follows the result. |
| Default run and session start | Cloud target preserved; waited versus detached lifetime distinct; unsupported cloud cannot become local. |
| Active steer and suspended follow-up | Stable session/message identity; explicit continuation admission; receipt does not imply completion. |
| Resume display, then explicit continuation | Display admits no work under the owned model; continuation gets a new attempt; unknown automatic-start behavior remains identified. |
| Graceful cancel, hard upgrade, failed snapshot | Request versus settlement distinct; no stale success; previous verified checkpoint retained. |
| CURRENT device/connect and deprecated controls | Equivalent requests; numeric positive cases and toggles have no scheduling effect; unknown grammar is not a fabricated golden. |
| LEGACY personal/machine capacity | Defaults4/1 and limits1–4 enforced separately from CURRENT. |
| Privacy and platform profiles | No login-time credential discovery or refresh-token access; independent controls; platform setup and permission distinctions retained. |
| Desktop minimal/install/status/mentions | Installed profile preserved, no false ready state or page-content access; format policy tested as product target. |
| Group, assistant, explicit repository, suppression | Distinct entities and checkout selection; target independent; disputed association mappings remain unresolved. |
| New session, delivered steer, completed/failed turn, revised event | Independent projections/cursors; stable session, separate attempts/outcome; final replies not delivery notes; revision and sequence gaps preserved. |
| Crash or lost continuation receipt | No automatic replay, duplicate permit or invented successful delivery; unresolved effects remain explicit. |

Required follow-up decisions remain external legacy grammar/version availability;
watch/send wait, interval, framing and interruption; resume automatic-start;
capacity grammar/warnings; multi-workspace setup; platform isolation/migration;
desktop dependencies/permission breadth; mentions availability/format enforcement;
association mapping/instruction precedence; and interim conversation/event cursor
representation. Retain these obligations until resolved; none is a scope waiver.
