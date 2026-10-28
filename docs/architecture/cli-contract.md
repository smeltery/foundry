# CLI target contract

This version-1 contract defines the complete Foundry command target. It does not
certify that every operation is implemented. The [user guide](../user/cli.md)
records current executable availability; the [configuration boundary](cli-configuration.md)
records the tested injected shell. Unsupported functionality remains required
work. A successful parse, fake response or design review proves only that layer.

The contract consists of this command inventory, the
[lifetime and entity contract](cli-lifetime-contract.md),
[configuration and output contract](cli-settings-contract.md),
[network and plugin contract](cli-network-plugin-contract.md), and
[platform contract](platform-contract.md). Changes must preserve requirements
across all five sections and update their acceptance scenarios together.

## Profiles and explicit selection

The target has two behavior profiles: `current` is the default and `legacy`
retains command-bound local serving, older device operations and capacity rules.
The original Foundry setting `FOUNDRY_PROFILE=current|legacy` selects the profile
before parsing. Absence selects `current`; an empty or unknown value fails before
state or credential access. This setting is a target design, not an implemented
environment reader in the current executable.

The selected profile travels with execution requests and persisted connection
configuration. A remote execution owner must acknowledge that profile or return
unsupported before admission. It must not fall back based on host platform,
installed version or whichever backend is available. Profile changes cannot
silently alter an existing session's execution semantics. A new observer attaches
to the session's recorded profile. Both profiles remain qualification obligations. Attempt/checkpoint compatibility
must include the effective profile so restoration cannot cross those semantics.
Version output stays profile-independent and configuration-free. Profile-specific
help may inspect the explicit profile selector only; an invalid selector uses the
existing usage-error renderer without credential, database or backend access.

Detailed command requirements govern general descriptions; platform-specific
rules govern generic platform statements. A current requirement does not erase
a conflicting legacy obligation. Evidence gaps remain assigned acceptance work;
they do not authorize convenient defaults or removal of required targets.

## Complete command inventory

The neutral acceptance identifiers below are product requirements. Exact
implementation and verification status belongs to the owning feature; this table
is neither a runnable test list nor a completed-feature checklist. JSON selectors
mean `--json` and `--output-format json` where specified. Model effort uses the
canonical `--effort low|medium|high|xhigh` on the four task-bearing routes.

| ID | Command or capability | Required contract |
| --- | --- | --- |
| C01 | `f5y [PROMPT]` | No prompt opens TUI. A prompt uses the local computer/folder by default and preserves uncommitted files. Retain `--resume ID`, `--cloud`, `--on NAME`, `--dir`, `--yes`, `--detach`, `--repo`, `--no-repo`, `--update`, `--model`, `--effort`, and text/JSON output. `--cloud` selects the cloud target; detachment requires a cloud or machine target. Cloud prompt checkout can derive from the working tree's GitHub/GitLab origin; interactions with other implicit repository defaults require qualification. |
| C02 | `run --task TEXT` | Cloud-default noninteractive execution; wait unless detached. Preserve repository/ref/model/effort/output settings. Human intervention ends action-required rather than asking a question. Local-only execution is an implementation limitation, not the target default. |
| C03 | `session list`, `ls` | Limit, active/archived/all filtering, origin values ui/external/external:cli/external:mcp/external:api, and selected fields. Always retain session ID; invalid fields fail. Account-scoped cached pages may live at most five minutes without live counters/model, ten seconds when those are requested. Alias option differences remain qualification questions. |
| C04 | `session start --prompt TEXT` | Detached cloud operation with repository, provider, model, effort and JSON options. Without an applicable repository default, use scratch. |
| C05 | `session get ID` | Retrieve structured session state without deriving success from lifecycle or fabricating an unknown CLI wrapper. |
| C06 | `session watch ID`, `watch ID` | Follow through terminal-event drain; retain interval and JSON selectors. Current observation creates no attempt. Legacy local-serving duties remain profile-specific. |
| C07 | `session send ID --message TEXT`, `send ID MESSAGE` | Steer active work or continue suspended work in the same session with verified context and authority. Distinguish acceptance, delivery and completion; queue acceptance alone is insufficient. Current waiting behavior remains an explicit observation gate. |
| C08 | `session cancel ID`, `cancel ID` | Graceful cancellation attempts a final snapshot. `--hard` upgrades pending cancellation and skips that snapshot. Settlement is suspended; continuation uses the last verified checkpoint. |
| C09 | `session attachments ID` | Enumerate retained reports, patches, screenshots and recordings with stable identifiers. |
| C10 | `session download ID [ATTACHMENT]` | Select one item by ID/name or download all. Retain output path and force options. A single item defaults to its filename in cwd. Existing files survive without explicit force; multi-item defaults, collision and path rules require qualification. |
| C11 | `session share-link ID` | Read an HTTPS URL only from stdin; retain label/expiry options. Expose only an opaque reference. The URL never enters command arguments, output or diagnostics. The cloud browser opens the registered link once. |
| C12 | `connect` | Personal connection by default; named-machine/token variants enroll workers. Retain `--to`, `--token`, `--once`, `--api-key`, `--yes`. Connection API keys require the device-write capability; once is ephemeral. |
| C13 | Connection settings | Preserve folder/name/endpoint/proxy/CA, Chrome, minimal, local-logins, keep-awake and folder changes. Current capacity flags have no scheduler effect; legacy capacity remains effective. See setting-specific rules. |
| C14 | `disconnect [NAME]` | Stop and remove the personal connection, forget its renewal credential and persist a disconnected marker. Named disconnect removes this computer's workers. Script confirmation flag does not add a prompt. |
| C15 | `serve [NAME]` | Join a named machine queue, creating it only with authority. Retain token/directory/once/sandbox/yes/service/stop settings. Bare signed-in enrollment may use hostname; token enrollment refers to its machine without inferred decoding, and reconnect reuses verified saved identity. Missing, empty or competing identity inputs remain owner policy. Fresh directory is cwd. Consume enrollment tokens once. Maintain one worker per machine/folder identity. |
| C16 | `stop NAME`, `serve NAME --stop` | Stop local workers and remove background services while retaining registration for restart. Preserve documented JSON support on the Serve spelling. |
| C17 | `machines` | List machines, online workers, and queued/running sessions; support JSON. |
| C18 | `machines add/token/rm NAME` | Create an empty queue; issue an enrollment token and install command, creating a missing queue when authorized; remove and disconnect workers. Mutations/token creation require owner/admin authority. Preserve JSON and remove confirmation settings. |
| C19 | `desktop install` | Idempotently provision pinned desktop/browser/tool components, undo minimal mode and restart a connection when new capabilities become available. macOS permission levels none/standard/all remain distinct. |
| C20 | `desktop minimal/status` | Minimal prevents adding components during installation/update without erasing an existing profile. Status distinguishes missing, outdated, permission-needed and ready capabilities. |
| C21 | `desktop browser-use/mentions` | Personal macOS browser readiness and running-app/tab enumeration without page-content access. Preserve JSON surface; exact selector enforcement and other-platform availability remain qualification questions. |
| C22 | `permissions [allow [screen\|accessibility\|disk]]` | List holder-aware permissions or request missing permissions on macOS. Full-disk access uses OS settings instructions and observation; do not claim a direct OS prompt. |
| C23 | `device` | Current alias of connect. Legacy name/no-sandbox and connect/status/disconnect forms retain separate confined-workspace semantics. |
| C24 | `login/logout/whoami` | Browser-approved, revocable workspace login; account/org inspection. Logout affects only its own account. Deployment-specific pairing/setup grammar remains an explicit acceptance gate. |
| C25 | `status` | Version, account, computer/folders, update and multi-workspace status, with JSON support. Unobserved remote envelopes remain opaque. |
| C26 | `update`, `upgrade`, root `--upgrade` | Preserve install method; check/quiet/JSON modes and distinct updater exit statuses. Update desktop components unless minimal/network-only. A newer installed version counts as current. Integrated pre-session fallback is narrower than standalone shell chaining. |
| C27 | `providers connect anthropic` | Preserve pair/browser/token-stdin flows, isolated setup configuration and callback fallback where enabled. Never echo stdin tokens. Deployment-gated defaults require qualification. |
| C28 | `acp` | Stdio protocol with diagnostics on stderr; task targeting/model/effort, stable editor session IDs, streaming text/tool/diff activity, stop/follow-ups/questions, images/files, history/resume and model selection. Use the agent's own tools. Images require deployment-read authority. |
| C29 | `network` | Full standalone/composed network surface and adapter operation, including bare setup, addressing and wrapper serving; see its dedicated contract. |
| C30 | Help/version | Short top-level help, exhaustive `help all`, nested help and version must be configuration-free. Short help does not define the whole command inventory. |
| C31 | TUI | Key help, command menu, machine selection, image paste/removal and resume. Failed sends retain draft attachments. Machine changes preserve session identity; complete keymap/menu qualification remains required. |
| C32 | Local/custom models | Explicit model selection and worker-side localhost/LAN/tailnet requests; chat-completions, responses and messages formats. Public cloud endpoints require HTTPS. Enforce private-network boundaries and value-free DNS/connect/timeout/TLS/HTTP diagnostics. |
| C33 | Task exits | Success/accepted detach 0; task failure 1; runtime/auth/usage failure 2; action-required 3; local interruption 130. Acceptance is not completed work. |
| C34 | Signals | First interruption requests graceful cancellation and waits; second forces stop without a fresh snapshot. Observer interruption never cancels an independent producer. Completion races require separate evidence. |
| C35 | Updater exits | Current/updated/newer installed 0; check finds update 10; check/update failure 1; unverifiable installed version 75. Do not substitute task-error mapping. |
| C36 | Output | Task final JSON object, progress on stderr; session JSON selectors equivalent; ACP stdout protocol-only. Event streams, query values and terminal task results are separate domains. |
| C37 | Environment | Owned `FOUNDRY_` settings with per-setting resolution, independent update/notice settings and no global precedence rule. |
| C38 | State | Owned native roots, separate credential/worker/connection/cache/desktop/worktree scopes, account isolation and explicit migration only. |
| C39 | Connection ownership | One computer per host/workspace/person. Personal visibility differs from team admission. Teammates can observe existing sessions. Authorized setup/app enrollment can add up to eight workspaces; plain login/API key/once remain single-workspace paths. |
| C40 | Persistence | Current local connection/session survives observer terminal closure; extra folders join that computer. Disconnect prevents implicit reenrollment. Session deletion removes its managed worktree while retaining the branch. Legacy command-scoped serving remains required. |
| C41 | Session identity | Stable across follow-ups/retries/machine moves; attempts are distinct execution generations. Lifecycle/outcome/result/summary/timestamps/PR metadata/usage/cost/diagnostics remain distinct; null does not mean success. |
| C42 | Credentials | Short-lived approvals, revocable logins, single-use enrollment and renewal-only retained connection keys. Provider setup remains isolated. Explicit local-login access and ordinary login are separate operations; refresh tokens are excluded. |

## Integration boundary and delivery

Foundry owns canonical command names, typed requests, state and rendering. An
external integration adapter owns its service-specific codec, endpoints, headers,
link references and provider vocabulary. Configuration selects the adapter
explicitly; the built-in local backend is never an automatic substitute for a
requested cloud or remote capability. The neutral process boundary is not a
claim of wire equivalence, and changing a field's spelling cannot establish it.

An adapter must provide lossless operation-specific mapping, preserve unknown
structured result fields, acknowledge profile/capabilities and distinguish opaque
external identifiers from owned session/attempt IDs. Exact external strings and
mapping artifacts are supplied outside the repository. No hidden encoded map,
automatic alias loader, implicit account selection or state import is part of
this contract. Required notices remain with any distributed adapter artifact. Foundry also
retains every notice required for its own distributed dependencies. Bundling,
generating or redistributing an adapter subjects that artifact to the same
content and license gates; external placement is not permission to relocate
Foundry's mandatory notices. This design adds no download or install mechanism.
Direct integrations lacking an accepted codec or redistribution decision remain
blocked work; publishing this contract does not approve them.

Each capability needs its parser/request checks, consuming implementation,
applicable native/platform evidence and end-to-end outcome before it can be
reported as implemented. Contract acceptance freezes target semantics; source
observations can require a versioned correction, never a retroactive passing test.
