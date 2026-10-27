# Trusted one-call tool host

`f5y-toolhost` is a disposable Unix helper, built by the `foundry-tools` package.
It has no runtime, provider, persistence or CLI grant integration. Process
separation permits a future execution owner to supervise blocking filesystem
work; it does not provide an OS sandbox or contain arbitrary adapter code.

`f5y-toolhost --version` prints the stamped release version without a directory
capability or input. Execution accepts no arguments and uses the IPC below.

## Capability and authority

The trusted parent supplies an already-open directory on **fd 3**. The helper
adopts it once, checks that it is a directory and sets close-on-exec. It never
reopens a root pathname. Renaming the original directory or replacing that path
does not redirect the capability. Missing/wrong descriptors fail before execution.

Library callers may use `WorkspaceTools::from_dir(Dir, Grants, Limits)`. The
existing trusted `open` constructor remains available. See
[permission semantics](PERMISSIONS.md) for symlink, hard-link and race limitations.

The parent must select the absolute installed helper path, supply no arguments,
clear its environment, and transfer only intended descriptors. It must handle
source-fd-equals-3 and close-on-exec correctly using a reviewed spawn sequence.
Never pass this capability to the adapter. Never forward adapter-supplied grants,
limits, root paths or descriptor numbers into this trusted interface.

## Version 1 framing

Both stdin and stdout use a four-byte unsigned big-endian JSON byte length,
followed by exactly that many UTF-8 bytes. Stdin must then reach EOF. Any extra
byte, second frame, malformed JSON, invalid version or unknown field fails before
filesystem execution. The helper performs one call and exits; it has no loop.
Whitespace is allowed inside the JSON payload, not after the frame.

Request payload ceiling is 524288 bytes, including its entire JSON envelope.
Reply payload ceiling is 1048576 bytes, including IDs and result. Each frame
therefore occupies at most its payload ceiling plus four bytes. Both are below
2 MiB. Encoding expansion counts toward limits. Oversize output returns a small
error rather than truncation.

Example request JSON, to be framed by the trusted parent:

```json
{
  "version": 1,
  "attempt_id": "attempt-a",
  "call_id": "call-a",
  "grants": ["read"],
  "limits": {
    "request_bytes": 524288,
    "read_bytes": 262144,
    "write_bytes": 262144,
    "output_bytes": 1048576,
    "entries": 256
  },
  "tool": "read",
  "input": {"path": "src/lib.rs"}
}
```

All fields except `grants` are required. Absent/empty grants deny all operations.
Allowed grants are independent: `read`, `list`, `write.create`, `write.overwrite`.
Unknown/duplicate grants fail; there is no wildcard. Unknown input/policy fields
and duplicate known fields fail. IDs contain 1–128 UTF-8 bytes without controls;
tool names contain 1–64 bytes. The tool input shapes match the
[tools table](README.md): read/list require `path`, write requires `path`, `content`
and explicit `mode` (`create` or `overwrite`). Paths remain bounded to 4096 bytes.

All limits are explicit trusted values. Request/read/write/entries must be
positive; output must be at least 128 bytes. Upper bounds are the values shown in
the example. The request limit includes the IPC envelope, not just tool input.
The output limit applies to encoded tool output; the hard reply ceiling also
checks the correlation envelope.

Success reply:

```json
{
  "version": 1,
  "attempt_id": "attempt-a",
  "call_id": "call-a",
  "result": {"status": "ok", "output": {"content": "text"}}
}
```

Policy/tool failure reply:

```json
{
  "version": 1,
  "attempt_id": "attempt-a",
  "call_id": "call-a",
  "result": {"status": "error", "code": "denied", "effect": "none"}
}
```

Tool error codes are `unknown_tool`, `denied`, `invalid`, `limit`, `unsupported`
and `io`. `denied`/`unknown_tool` effects are `none`. Other write errors use
`unknown` conservatively, even when a specific failure may have preceded mutation.
Read/list errors use `none` for content mutation. Successful write replies do not
promise atomicity or rollback. A missing/broken reply after dispatch may follow a
successful or partial write; never automatically retry it.

A complete reply, including a tool-error reply, exits zero. Bootstrap/protocol/
policy/pipe failures exit 2 and emit one fixed stderr code: `toolhost_bootstrap`,
`toolhost_protocol`, `toolhost_policy`, `toolhost_input_io`, `toolhost_output_io` or
`toolhost_unsupported`. Diagnostics do not echo input, paths or file content.
A broken output pipe can follow filesystem effects, so nonzero exit is not proof
that nothing happened. Normal stdout contains only the framed reply.

## Parent integration remains required

The helper waits synchronously for EOF and executes synchronously. It has no
internal timeout or cancellation mechanism. The execution owner must supervise
its own process group, impose deadlines and bounds, close stdin, drain outputs,
kill/reap on failure, and join its pipe workers. It must accept success only after
a bounded matching reply and successful exit, then durable receipt confirmation.
Partial input, a blocked filesystem and parent crash need explicit lifecycle
handling. One-call execution does not deduplicate calls across helper processes.

No grant-enabling runtime behavior is included here. Claim/receipt persistence,
replay handling, cancellation, adapter isolation, helper packaging/discovery and
cross-platform execution tests are separate integration work.

Run `cargo test -p foundry-tools --locked`. Tests use temporary files and explicit
helper paths, real fd inheritance, root replacement, malformed/trailing/oversize
requests, denied grants, symlink checks, EOF ordering and broken output handling.
