# CLI settings and output target

This is the configuration, state and rendering section of the
[CLI target contract](cli-contract.md). It specifies required semantics and
explicit owned policies. The [injected configuration interface](cli-configuration.md)
is implemented; native environment/state readers, remote services and installers
are separate work. A value retained for policy resolution is not authenticated,
validated or safe to execute merely because a descriptor exists.

## Setting-specific resolution

Owned environment names use `FOUNDRY_`. No other application's aliases, accounts
or state are implicitly read. The following table is the resolution contract;
there is no universal flag-over-environment-over-saved rule.

| Setting | Required input and resolution |
| --- | --- |
| Profile | `FOUNDRY_PROFILE` selects current/legacy before parsing, as specified in the command contract. Propagate it to the execution owner. |
| Session identity | A supplied nonempty `FOUNDRY_API_KEY` selects its workspace; key absence uses the selected login. Empty keys and invalid-key fallback remain explicit policy questions; do not silently switch accounts. |
| Connection identity | Explicit API key, environment API key, enrollment token and saved renewal identity remain distinct candidates. No unestablished ordering or equal-value coalescing. A selected key needs device-write authority. |
| Endpoint | Connect `--api-url`, `FOUNDRY_API_URL` and saved connection endpoint are distinct candidates. Task/session contexts can carry the environment endpoint. Account lookup and execution receive the same selection. No guessed public endpoint or credential-derived URL. |
| Enrollment | Serve retains explicit token and `FOUNDRY_TOKEN` candidates for transient enrollment, and verified saved worker identity for reconnect. Bare signed-in enrollment depends on its established context; these paths impose no precedence among competing candidates. Connect token enrollment is a separate route. A token is transient; never persist it. Token/name/saved-identity conflicts require an explicit owner decision. |
| Connection folder | Explicit saved-setting change, retained folder, or fresh personal home/machine cwd. Empty/reset/normalization semantics remain unresolved. Task cwd is separate. |
| Connection name | Explicit change, retained name, or fresh hostname. Hostname is not a mandate to overwrite a saved name. |
| Proxy and CA | Explicit/saved proxy and CA inputs remain visible to the owner. Connection transport excludes `HTTPS_PROXY` even when no explicit proxy is supplied. No implicit fallback to other conventional proxy variables or guessed CA location. |
| User Chrome | Explicit/retained on/off setting; fresh macOS default on. Other-platform defaults and unsupported behavior require qualification. |
| Local logins | Fresh default on when key absence is established, off with an API key. Explicit/retained on with a key remains unresolved. Only access tokens may be consumed; no refresh tokens or passive discovery during plain login. |
| Keep awake | Explicit/retained on/off; fresh macOS default on. A change applies without reconnect. No invented other-platform default. |
| Minimal and folders | Minimal persists and desktop installation reverses it. Preserve an existing desktop profile. Add/remove folder changes apply immediately without stopping other folders' sessions; repetition/conflict ordering is not guessed. |
| Capacity | Current profile retains obsolete arguments as scheduler no-ops. Legacy profile retains its documented effective limits/defaults. Unobserved lexical boundaries are not silently narrowed. |
| Invocation directory | Prompt/Serve explicit directory or cwd. Do not substitute the personal connection's home-folder default. |
| Model and effort | Preserve explicit model and canonical low/medium/high/xhigh effort; absence stays absent. No invented default model, provider or saved/environment precedence. ACP later changes require a new execution configuration at the appropriate attempt boundary. |
| Output | Explicit text/JSON selectors; session JSON aliases select the same mode. No environment or saved output setting. Current shell conflict rejection is an owned rule, not proof of every external parser's ordering. |
| Pre-session update | Explicit prompt update and exact `FOUNDRY_AUTO_UPDATE=1` record one positive request for prompt/run/Serve. Preserve both provenances. Other bytes remain private unresolved observations; no boolean coercion or new run/Serve flag. |
| Daily notice | Exact `FOUNDRY_NO_UPDATE_NOTICE=1` suppresses only the daily notice. Updates, warnings and errors remain active. Timing and notice availability belong to the updater owner. |
| List/watch/share parameters | Preserve limit, fields, interval and expiry inputs; field selections retain session ID. Do not transplant remote API defaults into the CLI. Unobserved ranges and default values remain explicit qualification work. |
| Local database | Owned target policy: explicit `--database`, then `FOUNDRY_DATABASE`, then `sessions.db` in the native state root. The current executable still requires its existing explicit/environment selection until default-state support is implemented. |
| Adapter/tool host | Retain explicit owned selection and capability checks. `FOUNDRY_ADAPTER` selects an external adapter; `FOUNDRY_TOOLHOST` does not confer tool authority. No ambient PATH fallback or implicit cloud-to-local substitution. |

Each command captures one scoped snapshot. Preserve absence, present-empty and
competing values separately. Known input types and command constraints are checked
before side effects; unknown compatibility-sensitive policy is passed to a required
consumer. If that consumer cannot resolve it, return an explicit capability error
without execution or persistence. This is the owned failure boundary, not a claim
that every external system rejects those inputs. In particular, do not promote
strict-empty rejection, equal-value coalescing or universal credential fallback
into a compatibility rule without evidence.

Help/version and parser failures access no credentials. Fixed configuration
errors contain no raw keys, endpoints, tokens or private settings. An arbitrary
consumer remains responsible for its own response and diagnostic hygiene; the
library does not globally redact explicit CLI values before generic parsing.

## Owned state locations

Native directory resolution is an original Foundry policy. Use the user's native
home/known-folder APIs; missing required directory facts are explicit failures,
not permission to search another application's files. Linux XDG variables must
be absolute when supplied; an empty or relative value uses the listed home
fallback. macOS and Windows use native paths rather than XDG overrides.

| Platform | Config | Durable state | Data | Cache |
| --- | --- | --- | --- | --- |
| Linux | `$XDG_CONFIG_HOME/foundry` or `~/.config/foundry` | `$XDG_STATE_HOME/foundry` or `~/.local/state/foundry` | `$XDG_DATA_HOME/foundry` or `~/.local/share/foundry` | `$XDG_CACHE_HOME/foundry` or `~/.cache/foundry` |
| macOS | `~/Library/Application Support/Foundry/config` | `~/Library/Application Support/Foundry/state` | `~/Library/Application Support/Foundry/data` | `~/Library/Caches/Foundry` |
| Windows | Native LocalAppData `Foundry/config` | Native LocalAppData `Foundry/state` | Native LocalAppData `Foundry/data` | Native LocalAppData `Foundry/cache` |

Use a versioned `config.json` for nonsecret product configuration and
`sessions.db` for the owned session store. Worker identity and connection records
must be scoped by account/workspace, host and folder identity. Exact internal
filename/hash encoding is a storage implementation detail; it cannot alter that
isolation or be used to claim another application's path compatibility. Data owns
managed worktrees and desktop assets; cache is reconstructible and never stores
the only copy of execution authority or renewal credentials.

Credential storage uses macOS Keychain, Windows Credential Manager or Linux
Secret Service through an explicitly supported secure-store backend. Unavailable
secure storage is an explicit persistence failure; there is no plaintext fallback.
Ephemeral `--once` operation does not create a saved identity or erase unrelated
records. Directory/file permissions and atomic replacement must protect the user
scope. State schema changes require migrations with recovery tests; unknown
versions must not be rewritten. Account logout removes only that account's
credentials and connections. Cross-application migration requires an explicit
separately reviewed operation and is absent from this contract.

## Output domains

The backend supplies completion classification independently from lifecycle and
structured result data. Unknown fields and integer precision must survive the
chosen transport. A successful HTTP call, `exit` lifecycle, process termination
or null result never establishes task success.

| Domain | Required behavior and owned rendering boundary |
| --- | --- |
| Waited task text | Final result on stdout; progress on stderr. Do not synthesize a success sentence from absent data. Exact external whitespace and non-string rendering remain qualification questions. |
| Waited task JSON | One terminal result object, not event JSONL. Preserve its fields without guessing a surrounding session envelope. The owned renderer emits compact JSON with LF. |
| Detached acceptance | Return session ID only after admission is accepted. Owned text is the ID plus LF; owned JSON uses a typed `session_id` result. This is acceptance, not task completion. |
| Session query/mutation | Render the operation-specific backend value; the renderer never guesses list/object shape. Preserve backend status and distinct text/JSON representations. Exact external tables/wrappers remain unverified. |
| Watch | Owned streaming form is JSONL events followed by a terminal result, with separate event IDs, revisions and cursors. Preserve the implemented form while qualifying external framing; task final-object rules do not change watch into final-only output. |
| ACP | Stdout carries only ACP frames; diagnostics go to stderr. No task wrapper, update notice or progress banner may enter that stream. |
| Secret link | Only an opaque reference can leave registration. The owned URI namespace is `foundry://link/` followed by an opaque service-issued identifier. External adapters map service references explicitly; they never rename a URI and assume the remote service accepts it. |
| Update check | Installed/latest/update-available/install-method/channel fields remain required, with unknown additional fields preserved. Exact types and mixed quiet/JSON behavior remain updater qualification gates. Quiet-current is silent; changes and errors remain visible. |

Keep current structured parser errors and their selector/channel rules. Help and
version remain their own output paths. A failed write is an error; never retry a
partially emitted JSON document as another complete result. Notice delivery uses
a separate categorized sink, leaving task responses unchanged.

| Completion | Task domain | Standalone updater domain |
| --- | --- | --- |
| Success or accepted detach | 0 | Not a task result |
| Task failure | 1 | Not a task result |
| Runtime/auth/usage failure | 2 | Check/update failure 1 |
| Action required | 3 | Not a task result |
| Local interruption | 130 | Independently qualified |
| Current, updated, or installed newer | Not a task result | 0 |
| Check finds update | Not a task result | 10 |
| Installed version unverifiable | Not a task result | 75 |

Query success does not inherit the historical task's failure status. For owned
local observation, interruption returns 130 without cancelling an independent
producer. Owned active cancellation distinguishes request from settlement;
completion races and external first-signal semantics remain separate evidence
requirements. Unknown terminal classification fails explicitly rather than
fabricating success; this owned error policy is not external outcome equivalence.

Pre-session preparation runs before exactly one launch. An updated disposition
must reach a distinct updated-launch consumer with its opaque verified handoff.
Only a specifically classified unreachable release server emits a warning and
continues on the installed version. Verification, unknown policy or other
preparation failures do not receive this fallback. Daily-notice suppression
cannot hide warnings/errors. Diagnostic delivery failure prevents silent success.
Actual verification, installation and process replacement remain updater work.

## Required acceptance scenarios

These are contract assertions, not claims that the native implementations exist.

- Capture key/login/endpoint inputs once, consume the same endpoint during account
  resolution and execution, and prove no fallback to another account on failure.
- Preserve conflicting/empty inputs byte-for-byte until explicit policy resolution;
  no private values enter request JSON, diagnostics or errors.
- Exercise fresh/retained connection defaults, key-dependent login defaults,
  immediate changes, unaffected folders and once-state isolation.
- Exercise every native root in an isolated home, XDG absence/empty/relative
  values, secure-store absence, schema failures and cross-account isolation.
- Verify update-before-launch, installed/updated handoff, notice-only suppression,
  release-unreachable continuation and no fallback for verification failures.
- Verify independent text/JSON results, nonzero backend statuses, write failure,
  parser-error selectors, task final-object versus watch stream and ACP isolation.
- Register a secret link through stdin and prove that only the opaque reference
  appears in output, diagnostics, persisted metadata and process arguments.
