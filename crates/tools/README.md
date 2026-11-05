# Foundry workspace tools

A standalone, default-deny filesystem policy. No shell, network, model provider or
runtime integration is included. Unix execution only; other platforms return an
explicit unsupported error.

```rust,ignore
let tools = WorkspaceTools::open(root, Grants::default(), Limits::default())?;
let result = tools.execute_json("read", br#"{"path":"src/lib.rs"}"#);
// Denied until the trusted caller explicitly grants read access.
```

| Tool | JSON input | Required grant | Result |
| --- | --- | --- | --- |
| `read` | `{"path":"src/lib.rs"}` | `read` | `{"content":"..."}` |
| `list` | `{"path":"."}` | `list` | `{"entries":[{"name":"src","kind":"directory"}]}` |
| `write` | `{"path":"new.txt","content":"text","mode":"create"}` | `create` | `{"bytes_written":4}` |
| `write` | `{"path":"existing.txt","content":"text","mode":"overwrite"}` | `overwrite` | `{"bytes_written":4}` |
| `edit` | `{"path":"existing.txt","old_text":"a","new_text":"b"}` | `read` and `overwrite` | `{"replacements":1,"bytes_written":4}` |

`Grants` has independent `read`, `list`, `create` and `overwrite` booleans, all false
by default. Permissions apply to the entire selected directory capability. Unknown
tools, unknown input fields and implicit write modes fail. Errors expose a stable
`code()` and sanitized message, never file contents or ambient filesystem paths.

Edit replaces one literal occurrence of `old_text`, or every occurrence with
`"replace_all":true`; a missing or ambiguous match fails without writing. It never
creates a file and shares overwrite's symlink, hard-link, special-file and size
checks, with the current file bounded by the read limit and the result by the
write limit; the projected result size is checked before it is built. Edit needs
read as well as overwrite because its match result reveals file contents. Like
overwrite, it truncates and rewrites in place, so a failure after truncation can
leave the file empty or partial. The toolhost refuses edit as an unknown tool
until its IPC carries edit input.

`ToolProfile` maps a session's `tool_profile` reference to grants from a fixed
table: `default` enables read, list, write and edit; `default/read-only` enables
read and list. No reference grants nothing, and an unknown name is an error with
no fallback. Service create rejects an unknown profile as unsupported. Profile and
tool names are provisional.

Create refuses existing paths. Overwrite requires an existing regular file and
never creates missing files. Parent directories must exist. File contents must be
UTF-8 for reads; write strings are UTF-8. List is sorted, nonrecursive, and reports
symlinks without following them. Symlinks are denied for all operation paths,
including links whose targets are inside the workspace.

Default limits: 2 MiB encoded input, 256 KiB read/write content, 2 MiB encoded output,
and 256 directory entries. `Limits` can lower or raise these within validated hard
caps: 2 MiB input/output, 1 MiB read/write, and 4096 entries. Oversize results return
errors, not silently truncated output. Paths are at most 4096 UTF-8 bytes.

Dependencies are pinned `cap-std`/`cap-fs-ext` 4.0.3, Serde/JSON and Unix libc.
Development tests use tempfile. Rust 1.95 builds and tests are supported.

Run `cargo test -p foundry-tools` and
`cargo clippy -p foundry-tools --all-targets -- -D warnings` after workspace inclusion.
Tests use temporary directory trees only. See [permission and race semantics](PERMISSIONS.md)
before enabling grants in an execution owner.

The package also builds the trusted one-call `f5y-toolhost` Unix helper. It consumes
an inherited directory capability on fd 3 and explicit parent-supplied policy.
See the [toolhost IPC contract](TOOLHOST.md). Runtime dispatch remains unimplemented.
