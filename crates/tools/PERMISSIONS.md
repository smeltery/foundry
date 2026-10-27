# Permission and race semantics

## Authority boundary

The trusted caller opens one directory with `WorkspaceTools::open`; that selection
uses explicit ambient authority once. Tool requests receive no ambient filesystem
API and cannot change the root. Grants are enforced before tool input processing.
No operation grants shell, networking, recursive traversal, symlink creation,
directory creation, deletion, permissions changes or model execution.

This is an application policy over directory handles, not an OS sandbox. Other
code in the same process and an external adapter may retain their own OS authority.
The process's OS permissions still apply; a grant cannot bypass them. A caller
choosing an overly broad root grants that entire capability, including hidden files.
There are no per-file exclusions or dynamic revocation of already opened handles.

## Resolution

All request paths are relative. Absolute paths, parent/dot segments, NUL, backslash
and colon are rejected; `.` is supported solely for listing the root. Backslash and
colon restrictions intentionally avoid platform-dependent separators and stream
syntax. Tool paths are never canonicalized and reopened by ambient pathname.

The root is a `cap_std::fs::Dir`. Each intermediate component opens through
`cap_fs_ext::DirExt::open_dir_nofollow`; the leaf opens relative to the resulting
handle with `OpenOptionsFollowExt::follow(FollowSymlinks::No)`. Listing likewise
opens each directory without following links. File operations inspect metadata on
the opened descriptor, not on a separately checked pathname. New files use
exclusive create and request mode 0600, subject to OS ACL/umask behavior.

A swapped symlink therefore fails rather than redirecting the operation. Once a
directory/file has been opened, concurrent renames do not revoke its authority:
the handle still refers to that object, even if another process moves it outside
the original pathname tree. The policy does not promise confinement to a changing
pathname prefix. Regression tests race both directory and leaf symlink swaps and
verify a replaced ambient root path cannot redirect an existing handle. Passing
these tests is not a formal proof across filesystems or operating systems.

## Aliases, special files and concurrent writes

Read authority includes file objects already linked into the selected workspace.
A hard link can alias an object also named elsewhere; no pathname resolver can
infer its complete provenance. Overwrite refuses descriptors reporting multiple
hard links, but another authorized OS process can add a link after this check.
Mount changes, privileged device creation and adversarial same-user filesystem
mutation are not isolated by this library. Do not treat the policy as a boundary
against processes with equivalent or greater OS authority.

Unix opens use `O_NONBLOCK` so ordinary FIFOs do not hang before descriptor checks;
nonregular files are rejected. This is not a general I/O deadline and cannot prevent
every device-open effect under privileged concurrent mutation. Filesystem or disk
operations can still stall.

Overwrite opens without truncation, verifies the descriptor, then truncates and
writes that same object. It never checks a path and later reopens it. Writes are
not transactional: disk errors or crashes can leave empty/partial files, and
concurrent writers can interleave. File synchronization occurs before success,
but directory entries are not separately synchronized and no crash-atomic creation
promise is made. An error must not be presented as a completed write; consumers
must reconcile uncertain side effects before retrying. Creation never overwrites
an existing name, even if that name is a symlink.

## Primary API review

Reviewed on 2026-10-27 against downloaded crate source version 4.0.3 and these
primary references:

- [Directory capability API](https://docs.rs/cap-std/4.0.3/cap_std/fs/struct.Dir.html)
- [OpenOptions follow extension](https://docs.rs/cap-fs-ext/4.0.3/cap_fs_ext/trait.OpenOptionsFollowExt.html)
- [Directory no-follow extension](https://docs.rs/cap-fs-ext/4.0.3/cap_fs_ext/trait.DirExt.html)
- [OpenOptions create behavior](https://docs.rs/cap-std/4.0.3/cap_std/fs/struct.OpenOptions.html)

The published crate metadata did not declare an MSRV. Compatibility was established
by an actual Rust 1.95 build and test run. Cargo preserves dependency licensing
metadata; these libraries' source or notices are not copied into this crate.
