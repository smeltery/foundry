#![cfg(unix)]
use foundry_tools::{Grants, Limits, ToolError, WorkspaceTools};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::symlink};
use tempfile::tempdir;

fn overwrite() -> Grants {
    Grants {
        read: true,
        overwrite: true,
        ..Grants::default()
    }
}
fn call(tools: &WorkspaceTools, input: Value) -> foundry_tools::Result<Value> {
    tools.execute_json("edit", &serde_json::to_vec(&input).unwrap())
}
/// Names the I/O error kind; the loop and not-a-directory kinds are not yet
/// matchable on stable Rust.
fn io_kind(result: foundry_tools::Result<Value>) -> String {
    match result {
        Err(ToolError::Io(kind)) => format!("{kind:?}"),
        other => panic!("expected an I/O error, got {other:?}"),
    }
}
fn edit(path: &str, old_text: &str, new_text: &str) -> Value {
    json!({"path":path,"old_text":old_text,"new_text":new_text})
}

#[test]
fn edit_is_denied_without_both_read_and_overwrite() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("file"), "alpha").unwrap();
    for grants in [
        Grants::default(),
        Grants {
            read: true,
            list: true,
            create: true,
            overwrite: false,
        },
        // Overwrite alone would turn match results into a read of the file.
        Grants {
            overwrite: true,
            ..Grants::default()
        },
    ] {
        let tools = WorkspaceTools::open(dir.path(), grants, Limits::default()).unwrap();
        assert!(matches!(
            call(&tools, edit("file", "alpha", "beta")),
            Err(ToolError::Denied)
        ));
        // Grants are checked before input is parsed.
        assert!(matches!(call(&tools, json!({})), Err(ToolError::Denied)));
    }
    assert_eq!(
        fs::read_to_string(dir.path().join("file")).unwrap(),
        "alpha"
    );
}

#[test]
fn allowed_edit_replaces_one_literal_occurrence_in_place() {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("sub")).unwrap();
    fs::write(dir.path().join("sub/file"), "keep $(x) λ keep").unwrap();
    let tools = WorkspaceTools::open(dir.path(), overwrite(), Limits::default()).unwrap();
    let result = call(&tools, edit("sub/file", "$(x) λ", "longer replacement")).unwrap();
    assert_eq!(result, json!({"replacements":1,"bytes_written":28}));
    assert_eq!(
        fs::read_to_string(dir.path().join("sub/file")).unwrap(),
        "keep longer replacement keep"
    );
    // Shrinking truncates the old tail rather than leaving stale bytes.
    call(&tools, edit("sub/file", "longer replacement", "")).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("sub/file")).unwrap(),
        "keep  keep"
    );
}

#[test]
fn ambiguous_missing_and_replace_all_matches_are_explicit() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("file"), "a-a-a").unwrap();
    let tools = WorkspaceTools::open(dir.path(), overwrite(), Limits::default()).unwrap();
    assert!(matches!(
        call(&tools, edit("file", "a", "b")),
        Err(ToolError::Invalid("old_text is not unique"))
    ));
    assert!(matches!(
        call(&tools, edit("file", "z", "b")),
        Err(ToolError::Invalid("old_text was not found"))
    ));
    assert!(matches!(
        call(&tools, edit("file", "", "b")),
        Err(ToolError::Invalid(_))
    ));
    assert_eq!(
        fs::read_to_string(dir.path().join("file")).unwrap(),
        "a-a-a"
    );
    let result = call(
        &tools,
        json!({"path":"file","old_text":"a","new_text":"bb","replace_all":true}),
    )
    .unwrap();
    assert_eq!(result["replacements"], 3);
    assert_eq!(
        fs::read_to_string(dir.path().join("file")).unwrap(),
        "bb-bb-bb"
    );
}

#[test]
fn edit_never_creates_and_rejects_malformed_input() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("binary"), [255, 254]).unwrap();
    let tools = WorkspaceTools::open(dir.path(), overwrite(), Limits::default()).unwrap();
    assert!(matches!(
        call(&tools, edit("missing", "a", "b")),
        Err(ToolError::Io(_))
    ));
    assert!(!dir.path().join("missing").exists());
    assert!(matches!(
        call(&tools, edit("binary", "a", "b")),
        Err(ToolError::Invalid(_))
    ));
    for input in [
        json!({"path":"binary","old_text":"a"}),
        json!({"path":"binary","old_text":"a","new_text":"b","extra":true}),
        json!({"path":"binary","content":"b","mode":"overwrite"}),
    ] {
        assert!(matches!(call(&tools, input), Err(ToolError::Invalid(_))));
    }
    for path in ["../outside", "/absolute", "./binary", "sub/../binary", ""] {
        assert!(
            matches!(
                call(&tools, edit(path, "a", "b")),
                Err(ToolError::Invalid(_))
            ),
            "{path}"
        );
    }
}

#[test]
fn symlink_escapes_are_never_followed() {
    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("secret"), "outside").unwrap();
    symlink(outside.path().join("secret"), root.path().join("file-link")).unwrap();
    symlink(outside.path(), root.path().join("dir-link")).unwrap();
    fs::write(root.path().join("inside"), "inside").unwrap();
    symlink("inside", root.path().join("internal-link")).unwrap();
    let tools = WorkspaceTools::open(root.path(), overwrite(), Limits::default()).unwrap();
    for (path, old_text) in [("file-link", "outside"), ("internal-link", "inside")] {
        assert_eq!(
            io_kind(call(&tools, edit(path, old_text, "bad"))),
            "FilesystemLoop"
        );
    }
    // A no-follow directory open reports ELOOP or ENOTDIR depending on the OS.
    let kind = io_kind(call(&tools, edit("dir-link/secret", "outside", "bad")));
    assert!(
        matches!(kind.as_str(), "FilesystemLoop" | "NotADirectory"),
        "{kind}"
    );
    assert_eq!(
        fs::read_to_string(outside.path().join("secret")).unwrap(),
        "outside"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("inside")).unwrap(),
        "inside"
    );
}

#[test]
fn hardlinks_and_fifos_are_rejected_like_overwrite() {
    let dir = tempdir().unwrap();
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("file"), "unchanged").unwrap();
    fs::hard_link(outside.path().join("file"), dir.path().join("alias")).unwrap();
    let tools = WorkspaceTools::open(dir.path(), overwrite(), Limits::default()).unwrap();
    assert!(matches!(
        call(&tools, edit("alias", "unchanged", "bad")),
        Err(ToolError::Invalid(_))
    ));
    assert_eq!(
        fs::read_to_string(outside.path().join("file")).unwrap(),
        "unchanged"
    );
    use std::os::unix::ffi::OsStrExt;
    let fifo = std::ffi::CString::new(dir.path().join("fifo").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(matches!(
        call(&tools, edit("fifo", "a", "b")),
        Err(ToolError::Invalid("only regular files can be edited"))
    ));
}

#[test]
fn limits_are_checked_before_mutating() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("large"), "123456789").unwrap();
    fs::write(dir.path().join("small"), "1234").unwrap();
    let limits = Limits {
        request_bytes: 128,
        read_bytes: 8,
        write_bytes: 8,
        output_bytes: 128,
        entries: 1,
    };
    let tools = WorkspaceTools::open(dir.path(), overwrite(), limits).unwrap();
    assert!(matches!(
        tools.execute_json("edit", &[b' '; 129]),
        Err(ToolError::Limit("request bytes"))
    ));
    assert!(matches!(
        call(&tools, edit("large", "1", "x")),
        Err(ToolError::Limit("read bytes"))
    ));
    assert!(matches!(
        call(&tools, edit("small", "1234", "123456789")),
        Err(ToolError::Limit("write bytes"))
    ));
    assert_eq!(
        fs::read_to_string(dir.path().join("large")).unwrap(),
        "123456789"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("small")).unwrap(),
        "1234"
    );
    // Exactly at the write limit succeeds.
    call(&tools, edit("small", "1234", "12345678")).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("small")).unwrap(),
        "12345678"
    );
}

#[test]
fn replace_all_result_size_is_bounded_before_it_is_built() {
    let dir = tempdir().unwrap();
    let content = "a".repeat(256 * 1024);
    fs::write(dir.path().join("file"), &content).unwrap();
    let tools = WorkspaceTools::open(dir.path(), overwrite(), Limits::default()).unwrap();
    // 262144 matches times a 1 MiB replacement would be 256 GiB if built first.
    let input = json!({"path":"file","old_text":"a","new_text":"b".repeat(1024 * 1024),
        "replace_all":true});
    assert!(matches!(
        call(&tools, input),
        Err(ToolError::Limit("write bytes"))
    ));
    assert_eq!(
        fs::read_to_string(dir.path().join("file")).unwrap(),
        content
    );
}
