# Local CLI workflows

The `f5y` executable supports persistent local session records and synchronous
execution through an explicitly configured protocol adapter. Tests exercise the
execution path with offline adapters. Foundry does not include a model-provider
adapter or cloud execution. A running adapter can accept queued instructions
through the negotiated receipt protocol; this is verified with offline adapters.

## Choose a database

Supply `--database PATH` or set `FOUNDRY_DATABASE`. The command-line option takes
precedence. The parent directory must already exist; no default database is
created. Help and version commands do not require a database.

Run examples below from a development checkout with `target/debug/f5y` built.
These are development-binary examples, not a release installation procedure.

## Help, aliases and parser validation

These commands work without a database or adapter configuration:

```sh
./target/debug/f5y help all
./target/debug/f5y help session start
./target/debug/f5y help device
./target/debug/f5y version --json
```

`help all` prints the complete command tree. `help COMMAND SUBCOMMAND` and
`COMMAND SUBCOMMAND --help` show focused help. Help describes the parser surface;
it does not promise that every listed operation has an execution backend.

`device` is an alias for `connect`, preserving its options. Connection setup is
not implemented by the local backend. `upgrade` aliases the `update` subcommand,
and root `--upgrade` routes to that standalone update operation. It cannot be
combined with a task, task options or another subcommand. Root `--update` remains
a separate task option; it does not become a standalone update. The local backend
does not install updates, and active `--update` is unsupported for local execution.

Both send forms reject empty or whitespace-only messages before dispatch. Nonblank
message whitespace is preserved. Missing required values, conflicting flags and
invalid enum values produce parser errors, generally status 2 and text on stderr,
even with `--json`. Unknown paths passed to `help` produce a dispatch usage error
and honor the selected output format.

## Create and inspect a queued session

This exact sequence was verified with separate executable invocations. It creates
a temporary database and explicitly disables adapter execution:

```sh
unset FOUNDRY_ADAPTER
demo_dir="$(mktemp -d)"
export FOUNDRY_DATABASE="$demo_dir/state.db"
./target/debug/f5y session start --prompt "Record a local task" --json > "$demo_dir/session.json"
cat "$demo_dir/session.json"
session_id="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["session"]["id"])' "$demo_dir/session.json")"
./target/debug/f5y session list --json
./target/debug/f5y --database "$demo_dir/state.db" session get "$session_id" --json
./target/debug/f5y session cancel "$session_id" --json
./target/debug/f5y session get "$session_id" --json
```

Creation returns `execution: "not_started"`, `target: "local"` and a queued session.
No process or model starts. Listing and fetching in later processes read the same
record. Cancelling the queued session changes it to suspended; the last get
confirms that persisted state. The database remains at `$demo_dir/state.db`.

The example uses Python only to extract the returned session ID. `ls` is also
available for listing, and `cancel ID` is an alias for the cancellation operation.
For a running attempt, cancellation records a request; it does not prove the
process has already stopped. See [session lifecycle](sessions.md).

## Queue a follow-up instruction

`send ID MESSAGE` and `session send ID --message MESSAGE` persist an instruction
for an existing session. Each CLI invocation generates a fresh UUID message ID,
so repeating the command creates another instruction. These commands do not start
or resume an attempt and do not wait for an adapter acknowledgement.

A new instruction returns a `message` with state `pending`,
`delivery_not_confirmed: true` and `completion: "not_confirmed"`. A successful
command means the record was stored, not that an adapter received or executed it.
The session lifecycle and outcome are unchanged by enqueueing. If an execution
owner is already running an adapter that advertises `message_ack_v2`, its service
can claim and forward the instruction. Otherwise the instruction stays pending.
A returned record is a snapshot; another process may change it immediately.

MCP clients supply their own `message_id` to `session_send`. Reusing an ID with
identical text returns its current record; different text conflicts. A retry does
not reset a withdrawn or in-flight instruction. Even when a record has reached
`delivered` and `delivery_not_confirmed` becomes false, completion remains
`not_confirmed`. See [stored instructions](sessions.md#stored-instructions).

## Configure local execution

`FOUNDRY_ADAPTER` selects one absolute executable path. It is not a shell command
or a command plus arguments. The executable must implement the
[adapter protocol](../architecture/runtime.md); an arbitrary program is not a
compatible adapter.

| Invocation | Adapter unset | Adapter configured |
| --- | --- | --- |
| `session start --prompt TEXT` | Create a queued record only | Create and run synchronously |
| `run --task TEXT` | Explicit adapter-not-configured error | Create and run synchronously |
| Positional task prompt | Explicit adapter-not-configured error | Create and run synchronously |
| Session list/get/cancel | Persistent local operations | Persistent local operations |
| Send/session send | Persist an instruction only | Persist an instruction only |
| `mcp` | Local storage-backed MCP | Same storage-backed MCP; no adapter execution |

A configured adapter receives the working directory and session through the
runtime protocol. The positional prompt form accepts `--dir`; run and session
start use the current directory. A malformed relative adapter path is rejected
when constructing the execution backend, even for ordinary session queries.
Unset it to use the storage-only workflow above.

The [application service](../architecture/service.md) persists events and the
terminal session. Completion maps success to status 0, failure to 1,
action-required to 3 and suspension to 130. Runtime or persistence errors fail
explicitly. The synchronous result is not a live output-streaming interface.

## Watch and interrupt

`watch ID` and `session watch ID` replay stored events and then poll until a
terminal session is observed. `--interval` sets a positive number of seconds,
defaulting to one. Text progress goes to stderr and the final result to stdout;
JSON mode emits JSONL events followed by a result record. The database must
already exist. Watch observes execution; it does not start or resume it.

On Unix, the first observed SIGINT or SIGTERM during adapter execution requests
graceful cancellation. A subsequent observed signal requests a hard stop. The
service persists suspension when it still owns the attempt; verified signal
workflows return 130. Interrupting watch returns 130 without cancelling the task.
Repeated OS signals may coalesce. SIGKILL cannot be handled and provides no
cleanup guarantee. Watch cannot preempt a blocked database or output operation.

## Output and errors

Use `--json` or `--output-format json` for structured results; these switches
conflict. Ordinary successful responses go to stdout. Configuration and dispatch
errors go to stderr with status 2; JSON errors contain an `error` object with
`code` and `message`. Parser errors happen before dispatch and do not necessarily
use the JSON error format.

The storage backend currently emits JSON-shaped text even without `--json`.
Adapter execution in text mode emits its terminal result. Prefer `--json` when
another program consumes output.

## MCP and remaining boundaries

The `mcp` command runs the local stdio interface using the selected database.
Create, list, get, cancel and send tools are wired to storage. Create queues a
session without execution; send queues an instruction without starting or
resuming work. See the [MCP boundary](../architecture/mcp.md).

Interactive conversations, cloud workers, remote execution, provider account
setup, model selection, automatic updates and release
installation are not established by these local workflows. Help includes planned
commands and options that can still return unsupported.

For setup and checks, see the [development environment](../development/environment.md).
