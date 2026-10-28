# Network and plugin target contract

This document specifies the required network and plugin interfaces. Requirements
remain in scope when an implementation is unavailable. The CLI shell owns grammar,
help, validation and dispatch; network services own delivery and team behavior;
client adapters own discovery and registration; release tooling owns distribution.

Current implementation: shared standalone/composed network parsing, versioned
requests, command-local output validation and plugin-management grammar exist.
The local backend does not execute network operations or install plugins. The
standalone network executable is buildable but is not included in release artifacts.
Wrapper requests are parsed without implementing their service behavior. Private
managed-relay framing is not specified. Library team-state operations described in
[durable team state](teams.md) do not complete these integrations.

## Network entry points and namespace

`f5y-network COMMAND` and `f5y network COMMAND` must select the same network state
and produce equivalent typed operations, argument bytes and defaults. Composition
must not reparse shell strings. Standalone operation remains usable without a
product account; account-backed wrapper onboarding is a separate capability.

| Route | Required distinction |
| --- | --- |
| Bare standalone/composed invocation | Membership and terminal state determine directory/onboarding; wrapper account bootstrap is separate |
| `f5y network ADDRESS MESSAGE...` | Ask and wait for a correlated answer; send acceptance is insufficient |
| `f5y network serve ADAPTER` | Agent adapter serving, distinct from root named-machine `serve` |
| Network `status`, `stop`, `upgrade` | Agent activity, daemon control and network artifact update; distinct from root account status, worker stop and product update |
| Network `mcp` | Network tools over stdio; distinct from session MCP and ACP |
| Network `bridge` | Native adapter protocol; not task-final JSON |
| Network `hook`, `tray`, `__relay` | Hidden lifecycle, platform UI and managed-relay routes; hidden does not mean optional |

Recognized commands must not silently become recipient execution after a malformed
invocation. Reserved-word addresses, delimiter handling, the full shorthand flag
surface and timeout policy need an explicit wrapper grammar decision. A delimiter
proposal is not accepted syntax. The current single-message wrapper parser is not
an exhaustive target specification. Preserve address and nonblank message intent
without inventing unspecified address parsing or numeric timeouts.

Adapter selection is capability-based. Claude, Grok and Codex are required example
integrations, not a closed list of possible adapter identifiers. Unknown or
unavailable capabilities fail before launch. Never fall back to another client,
a paid model, cloud session or machine worker. Adapter permissions must preserve
read/edit-without-shell and sandboxed execution distinctions. The runtime owner
must specify ask correlation, reply completion, interruption, permission escalation,
serve options and managed-relay frames before claiming those paths complete.

## Complete standalone network grammar

The table contains all 27 named commands, plus bare invocation. Flags are local;
booleans default false unless stated. Unsigned hours/wait inputs retain the full
unsigned 64-bit range at the parser boundary; runtime clamping is separate.

| Command | Inputs and defaults | Required backend behavior |
| --- | --- | --- |
| No command | No implicit command inserted | Outside a team, terminal onboarding or nonterminal setup hint; inside a team, directory |
| `who [FILTER] [--all] [--json]` | Optional filter | Directory; all includes recently offline sessions from the last day and offline machines |
| `send TO [MESSAGE...] [--reply-to ID] [--fyi] [--notify] [--from SESSION] [--file PATH]...` | Words joined with spaces; omitted message or sole `-` uses stdin; repeated files retained | Reject blank body; default wakes, FYI does not; FYI and notify conflict; preserve threading and queued/held/refused/offline results, not an answer |
| `status [DOING...] [--name NAME]` | No words means absent activity update; otherwise join words | Require registered current agent session, set activity/name, return address; outside-agent invocation fails |
| `init [TEAM] [--as PERSON] [--no-setup] [--relay URL] [--proof JWS] [--json]` | Default person comes from injected user identity; verified proof may supply name | Terminal may prompt for team; nonterminal missing team fails; bootstrap custom relay before startup; configure agents/service unless no-setup; expose partial setup failure after team creation |
| `invite [--me] [--code] [--hours N]` | Hours 24; code lifetime clamped to 1–720 | Default reusable link does not expire merely because hours is supplied; me/code issues one-use admission, me binds same-person machine; preserve online-inviter short code and long-ticket alternative; Unix/Windows enrollment instructions |
| `link [--new] [--json]` | New false | Read reusable link or rotate and invalidate old link; terminal clipboard path; team/link result |
| `join [INVITE] [--as PERSON] [--no-setup] [--proof JWS] [--json]` | Link, ticket or code; terminal may prompt | Nonterminal missing invite fails; optional setup, reconnect and bounded teammate discovery |
| `team [--json]` | No other flags | People, machines and reachability |
| `proof JWS` | Required proof | Attach/refresh machine-bound identity; validate signature, issuer, audience, type, endpoint, expiry and ownership |
| `endpoint` | No arguments | Print local public endpoint ID; may initialize local identity/state |
| `remove PERSON_OR_MACHINE` | Required selector | Remove person and all machines or one machine under trusted-team authorization |
| `leave [--force]` | Force false | Leave selected machine's team; force permits no online teammate; preserve state/daemon transitions and failures |
| `relay URL_OR_DEFAULT` | Required; literal `default` clears custom relay | Change relay, reload/restart relevant state; custom relay uses minimal discovery; default must be an owned configured service |
| `folder PATH off\|on` | Required path and enum | Canonical/deepest folder matching; an enabled child can remain excluded by ancestor; report effective exclusion and reload |
| `held` | No arguments | List inbound held messages; empty is normal |
| `approve [ID...] [--all]` | IDs or all required at dispatch | Release selected messages and report count; IDs plus all uses IDs, empty IDs plus all selects all |
| `reject [ID...] [--all]` | Same selection as approve | Drop held messages and report count; not session cancellation |
| `setup [--dry-run] [--remove] [--no-service]` | Ordinary terminal path may be guided | Configure supported agents/service; back up and alter only owned entries; dry-run plans only, remove undoes owned integration, no-service skips service install |
| `doctor` | No arguments | Diagnose setup, service and network; diagnosis is not repair |
| `upgrade` | No arguments | Standalone artifact update; managed installation refuses independent replacement and directs to product updater; checksum/version advancement, macOS signing identity and Windows waiting-copy |
| `daemon [--service]` | Service false | Foreground daemon; service mode waits for existing daemon exit; isolate each state root |
| `stop` | No arguments | Stop daemon via restart mechanism; report absence; do not delete startup registration or membership |
| `listen [--as NAME] [--wait SECONDS]` | Name `agent`, wait 600; zero allowed | Register/retain session; print inbox before acknowledging delivery; zero nonblocking, empty timeout normal; slug without letters/digits fails; preserve broken-output handling |
| `mcp` | Stdio protocol | `list_team`, `send_message`, `ask_user`, conditional `check_inbox`; capability-sensitive delivery, cancellation to `stop_waiting`, peek/delivered ordering |
| `bridge` | Stdio JSON lines | Multiplex native adapter frames, paths and delivery acknowledgments; enforce client allowlist and capability differences |
| `hook AGENT EVENT [--session ID] [--state STATE] [--doing TEXT]` | Hidden; required agent/event | Normalize lifecycle and session identity; processed hooks exit 0 rather than adopting task failure statuses |
| `tray` | Hidden | macOS menu-bar integration; other OS notification obligations remain separate |

Explicit send sender selection overrides inferred session identity; terminal sender
fallback and destination restrictions remain required. Preserve session, person,
wildcard, repository and global destination classes without flattening them into
unrestricted broadcast. Receipts, retries, offline mailboxes, held approval/refusal,
signed user approvals with notes and feature negotiation remain backend duties.

Attachments allow at most 10 text files, 256 KiB each and 512 KiB total. Enforce sender
workspace boundaries, hidden/secret-file exclusions, traversal, symlink and
case-collision checks. Received files must not be executable. Queuing a message
never proves recipient execution.

## Network configuration, transport and output

Both entry points consume one resolved network configuration: selected state root,
person, sharing, LAN/tray, hosted sessions, inbound policy, relay/discovery/trust,
proxy routing, UDP port, logging and update policy. Shared configuration policy owns
spellings and root selection. Do not infer universal flag/environment/saved
precedence or import another application's state at startup.

Required network defaults are path sharing off, unmanaged sharing off, LAN on,
macOS tray on, hosted-session integration off, auto-update on and debug logging
off. An optional fixed UDP port retains random-port fallback. Preserve default and
per-person accept/hold/refuse policy and canonical folder exclusions. Private
identity, database/WAL/shared-memory files, inbox, configuration, lock/log, relay
cache and socket/pipe are isolated by selected state root.

Accountless teams, hosted verified identity, cloud-session polling, supervised
cloud relay jobs, signed relay directories and direct/LAN/relay/proxy routing are
separate capabilities. Supervisor scenarios must cover 15-second polling, selected
session fields, one-hour stale-list fallback and 30–600-second reconnect intervals.
The managed `__relay` schema still requires a network/runtime interface decision;
do not fabricate frames. Bootstrap, including directory refresh, may contact only
owned/configured services. Missing service configuration reports unavailable.

JSON support is command-specific: `who` returns a directory; init/join/link/team
have result/error paths. Their exact payload schemas still need backend contract
fixtures. Composed global JSON selectors may select only these supported routes;
standalone commands do not inherit root formatting/database options. MCP stdout
is protocol-only, bridge stdout is JSON-line transport, and human commands remain
human output. Do not apply task-final JSON or task exit rules to hooks, listen or
network upgrade. Setup flag combinations and precise parser/error/output framing
need command-specific decisions, not guessed universal envelopes.

## Plugin manager grammar and package identity

Plugin management is Foundry-owned package management. Installation does not
authenticate connectors, create sessions, contact models or grant tool authority.

```text
f5y plugins list [--available] [--client CLIENT]
                 [--scope user|project|all] [--project PATH]
f5y plugins add PACKAGE [--version VERSION] --client CLIENT
                [--scope user|project] [--project PATH]
                [--replace] [--dry-run] [--yes]
f5y plugins remove ID --client CLIENT
                   [--scope user|project] [--project PATH]
                   [--dry-run] [--yes]
```

All accept shared `--json` and `--output-format text|json`; selector conflicts
follow the shared output contract. Required clients are `claude`, `codex`, `cursor`
and `grok`. Unsupported versions/adapters fail explicitly. Shared layout does not
prove installation support. Mutations never implicitly target all clients.

A package is a catalog ID or explicit local directory path: `./`, `../`, absolute
Unix, drive-qualified Windows or UNC. Plain tokens are catalog IDs; arbitrary URLs,
branches and shell commands are not package references. Version is exact; a local
version must match its manifest. Without version, a catalog ID selects the single
default in the selected pinned snapshot. Missing/ambiguous default fails without
fetching latest. Descriptors bind immutable package bytes and digest. Removal ID
is the exact case-sensitive owned ID, not a path/glob. Reject destination collisions
on case-insensitive filesystems; aliases must not select another package.

## Discovery, scope and owned mutation

User is the default list/mutation scope. `--project` requires project scope, or all
for listing. Canonicalize explicit existing project paths; non-Git directories are
valid. Without a path, select the nearest ancestor with a Git directory or worktree
marker; otherwise require an explicit path. Missing directories fail rather than
being created as an installation side effect. Unsupported client project scope
fails before mutation, without global fallback.

Store managed bytes and receipts under the selected Foundry data root. Receipts
bind canonical project identity; client discovery entries use the client's actual
scope support. Plans/results expose absolute destination and scope. Exact supported
client versions, discovery paths and reload behavior require client adapter records.

Installed list reads owned receipts and validates registrations without executing
plugins or discovering credentials, OAuth, remote health or linked skills. An absent
store yields an empty successful list without creating it. Unmanaged entries can
be listed but not removed/replaced. Available list reads only the selected cached,
pinned catalog; absent catalog is `catalog_unavailable`, not empty success. All
scope lists user and selected project registrations separately.

Add verifies exact descriptor/digest, schema, paths, assets and pinned dependencies;
then plans destination and registration. Fetch only descriptor-bound bytes for an
explicit add. Never run lifecycle scripts. Copy local content into managed storage
rather than executing a mutable directory. Dry-run validates locally and plans;
uncached payload means `requires_fetch`, not complete validation. Linked skills
without materialized reviewed bodies remain unavailable. A healthy exact match is
no-change success.

Explicit add/remove authorizes its scoped owned operation in terminal, nonterminal
and JSON modes. `--yes` is an accepted no-op, not a permission bypass or confirmation
gate. A different installed version/digest requires `--replace`; no extra approval
step is imposed. Unowned conflicts and modified owned bytes block mutation even
with yes/replace. Undeclared or unresolved dependencies block activation.

Stage verified content, journal owned changes, then atomically replace where the
filesystem supports it. Multi-file/client edits are not universally atomic. Preserve
backups and digest receipts. Pre-activation failure leaves prior registration
active; partial mutation attempts rollback only of recorded changes and reports
residual state plus recovery identifier. Matching retry reconciles the journal;
concurrent conflicts fail without overwriting.

Remove selects the exact ID/client/scope receipt and reports the selected files
and registrations. Missing owned installation is no-change; an unowned matching
entry is conflict. Preserve unrelated configuration, sessions, repositories,
credentials and other owners' caches. Do not revoke remote tokens or remove another
client's registration. Keep shared managed content while referenced. Do not kill
active clients; report `restart_required`. Modified entries require reconciliation
outside this grammar. Removal uses the same journal and rollback discipline.

## Management results and client workflows

Use shared success/error framing, with these operation payload requirements:

| Result | Required fields and behavior |
| --- | --- |
| List | Entries sorted by ID/client/scope/canonical destination; ID, nullable unmanaged version, client, scope, destination, owned/unmanaged ownership, available/installed/modified/unavailable state, capabilities and safe diagnostics |
| Add/remove | Operation, ID, version, client, scope, destination, planned/applied/no_change state, changes and restart_required; plans include requires_fetch and validation_complete |
| Failure | Safe operation context; partial failure includes recovery_id, applied_changes and residual_changes; complete rollback differs from residual state |

Keep unavailable capabilities visible. Never emit credentials or secret-bearing
locations, or applied success from an unsupported backend. Target management exits
are 0 for completed list/apply/no-change/plan, 1 for operational/conflict failure,
2 for usage/config/schema/unsupported-client-or-scope and 130 after interruption
and reconciliation attempt. Residual effects retain a recovery identifier. No
confirmation-required exit exists. Current unsupported shell rendering is not
proof that the management backend implements this target exit contract.

The six required client workflows are `ask`, `build`, `fix`, `fix-ci`, `review` and
`status`. Each must expose a contextual client instruction, preserve target and
workspace selection, and consume the applicable asynchronous backend lifecycle.
They are not management aliases; workflow status is not root status or plugins
list. Preserve orchestration instructions, assistant metadata, contextual handoff,
light/dark and skill-local assets, portable and client descriptors, three root
discovery manifests, and independent registry/package versions. HTTP,
Streamable-HTTP and URL-only MCP representations remain distinct adapter formats.
Use a deployable configured endpoint; no placeholder service can report readiness.

The backend contract includes authenticated remote MCP; workspace-bound consent;
membership, scopes, roles, private-project/IP checks and revocation; repository and
branch-head discovery; model discovery; asynchronous session create/list/get;
separate events, conversation and messages; send/steer/queue/withdraw; cancel,
retry, fork, rewind, archive and delete; reviews; schedules and trigger management;
devices, machines and queue status; persistent assistant conversations; attachments.
These operation groups do not cap the full active API requirement.

Preserve path/query versus body arguments, pagination, HTTP status/error/retry
metadata and idempotency. Creation requires a prompt, optional repository and
mutually exclusive pull-request/branch/ref selectors. Return session identity,
location and status asynchronously. Preserve sequence cursors and 20,000 ms progress
wait, independent conversation/delivered-message views, running/exit/error/suspended
states, success/action-required/failure outcomes, diagnostics and continuation
identity. Unique client message IDs, graceful/hard cancellation, terminal retry
conflicts, independent forks and rewind/archive semantics require backend fixtures.
Exact target selection must not silently fall back. MCP upload is capped at 1 MiB,
results at 2 MiB; the larger REST upload route remains distinct. MCP credentials and
REST credentials are not interchangeable by assumption.

Authorization retains run, repository-read, automation-write and device-use scopes.
Schedules select exactly one of cron, interval or scheduled instant and expose the
next run time. Preserve cloud-default, personal desktop, headless device and named
worker-pool targets without offline fallback. Moving a conversation does not move
unpushed files. Archive differs from destructive deletion. Distinguish unauthorized,
forbidden, missing, conflict and rate-limit responses, including idempotency
conflicts; an unavailable automatic review trigger must not report activation.

Allow at most ten prompt attachments. Preserve filename/base64 upload fields and
never forward service bearer credentials to an authorized download location.
Secrets remain write-only. Connection validation, user versus workspace connector
ownership and custom remote MCP connectors remain required. Workflows must return
working session and pull-request locations when those objects exist; fabricated
locations cannot stand in for backend completion. Suspension diagnostics and
follow-up continuation identity must remain available without conflating admission,
delivery and completed work.

Catalog coverage includes 385 connectors: 343 OAuth, 11 API-key and 31 anonymous;
66 skill references across 33 connectors; 357 local and 28 remote asset requirements.
Four service adapters remain explicit implementation duties: service-account
secret references, vault secret resolution, project/config secret lookup, and
workspace mail/calendar operations with OAuth lifecycle. Preserve endpoint path and
query semantics and non-Bearer authorization syntax. Keep catalog metadata separate
from principal/workspace/server credential records. Reject duplicate IDs, unsafe
paths, unknown authentication variants, undeclared placeholders and dangling
references. Never fetch or execute skill bodies during catalog load. GitHub, Slack,
Chrome and Computer Use remain required built-in capabilities outside this catalog.

## Neutral acceptance scenarios and remaining decisions

| Scenario group | Required observations | Owner |
| --- | --- | --- |
| Network grammar | Every command/default/invalid value, repeated files/words, hidden help, both entry points, root collisions and command-specific JSON | CLI shell and network |
| Delivery | Stdin/blank bodies, sender and destination classes, group limits, held/refused/queued versus answer, attachment limits and malformed frames | Network |
| Interactive lifecycle | Terminal/nonterminal onboarding/init/join/setup, no-setup, invite clamp/link/code, IDs plus all, dry-run ownership | Network |
| Streams and isolation | Zero/timeout/cancel, print failure before acknowledgment, hook exit, MCP/bridge framing, two roots, long IPC paths, no implicit state import or unconfigured service contact | Network and runtime |
| Wrapper/adapters | Correlated ask, timeout/interruption, permission profiles, unavailable adapter without fallback, all setup/removal ownership, cloud supervision and notifications | Runtime, network and client adapters |
| Package discovery | All scopes, worktrees/UNC/drive/case collisions, absent store versus absent catalog, unmanaged/modified/unavailable rows, offline reads | Client adapters and catalog |
| Package mutation | Pin/digest/path validation, no scripts, uncached dry-run, explicit authorization, no-change, replace conflicts, partial rollback/retry/interruption and shared references | Client adapters |
| Client workflows | Each of six workflows, each supported client/version/host, authenticated lifecycle, exact target, unavailable capabilities and complete catalog/skill/asset accounting | Client adapters, session service and catalog |

Unresolved wrapper grammar, private relay frames, command-specific output schemas,
network setting precedence and client discovery/version layouts retain their owners.
Each requires an explicit interface and isolated fixture record before implementation
completion. A parser test, manifest parse or local stdio exchange cannot substitute
for service behavior. See the [platform contract](platform-contract.md) for native
coverage and [CLI configuration](cli-configuration.md) for injected configuration.
