#![cfg(unix)]
use foundry_tools::{Grants, Limits, ToolError, WorkspaceTools};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::symlink};
use tempfile::tempdir;

fn grants() -> Grants {
    Grants {
        read: true,
        list: true,
        create: true,
        overwrite: true,
    }
}
fn call(tools: &WorkspaceTools, name: &str, input: Value) -> foundry_tools::Result<Value> {
    tools.execute_json(name, &serde_json::to_vec(&input).unwrap())
}

#[test]
fn default_deny_and_unknown_tool_are_explicit() {
    let dir = tempdir().unwrap();
    let tools = WorkspaceTools::open(dir.path(), Grants::default(), Limits::default()).unwrap();
    for name in ["read", "write", "list"] {
        assert!(matches!(
            call(&tools, name, json!({})),
            Err(ToolError::Denied)
        ));
    }
    for name in ["shell", "network", "unknown"] {
        assert!(matches!(
            call(&tools, name, json!({})),
            Err(ToolError::UnknownTool)
        ));
    }
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn create_read_list_and_overwrite_are_literal_and_scoped() {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("sub")).unwrap();
    let tools = WorkspaceTools::open(dir.path(), grants(), Limits::default()).unwrap();
    let text = "literal $(touch nope) ; UTF-8 λ";
    call(
        &tools,
        "write",
        json!({"path":"sub/file","content":text,"mode":"create"}),
    )
    .unwrap();
    assert_eq!(
        call(&tools, "read", json!({"path":"sub/file"})).unwrap()["content"],
        text
    );
    assert_eq!(
        call(&tools, "list", json!({"path":"sub"})).unwrap()["entries"],
        json!([{"name":"file","kind":"file"}])
    );
    call(
        &tools,
        "write",
        json!({"path":"sub/file","content":"short","mode":"overwrite"}),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("sub/file")).unwrap(),
        "short"
    );
    assert!(!dir.path().join("nope").exists());
}

#[test]
fn creation_and_overwrite_require_separate_grants_and_existing_state() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("existing"), "unchanged").unwrap();
    let create = WorkspaceTools::open(
        dir.path(),
        Grants {
            create: true,
            ..Grants::default()
        },
        Limits::default(),
    )
    .unwrap();
    assert!(
        call(
            &create,
            "write",
            json!({"path":"existing","content":"bad","mode":"create"})
        )
        .is_err()
    );
    assert!(matches!(
        call(
            &create,
            "write",
            json!({"path":"existing","content":"bad","mode":"overwrite"})
        ),
        Err(ToolError::Denied)
    ));
    let overwrite = WorkspaceTools::open(
        dir.path(),
        Grants {
            overwrite: true,
            ..Grants::default()
        },
        Limits::default(),
    )
    .unwrap();
    assert!(
        call(
            &overwrite,
            "write",
            json!({"path":"missing","content":"bad","mode":"overwrite"})
        )
        .is_err()
    );
    assert!(matches!(
        call(
            &overwrite,
            "write",
            json!({"path":"missing","content":"bad","mode":"create"})
        ),
        Err(ToolError::Denied)
    ));
    assert_eq!(
        fs::read_to_string(dir.path().join("existing")).unwrap(),
        "unchanged"
    );
    assert!(!dir.path().join("missing").exists());
}

#[test]
fn traversal_absolute_and_portable_escape_forms_are_rejected() {
    let dir = tempdir().unwrap();
    let tools = WorkspaceTools::open(dir.path(), grants(), Limits::default()).unwrap();
    for path in [
        "../outside",
        "sub/../../outside",
        "/absolute",
        "C:\\outside",
        "file:stream",
        "sub/./file",
        "./file",
        "",
    ] {
        for name in ["read", "list", "write"] {
            let input = if name == "write" {
                json!({"path":path,"content":"bad","mode":"create"})
            } else {
                json!({"path":path})
            };
            assert!(
                matches!(call(&tools, name, input), Err(ToolError::Invalid(_))),
                "{name}: {path}"
            );
        }
    }
    assert_eq!(
        call(&tools, "list", json!({"path":"."})).unwrap(),
        json!({"entries":[]})
    );
}

#[test]
fn symlink_leaf_and_directory_escapes_are_never_followed() {
    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("secret"), "outside").unwrap();
    symlink(outside.path().join("secret"), root.path().join("file-link")).unwrap();
    symlink(outside.path(), root.path().join("dir-link")).unwrap();
    fs::write(root.path().join("inside"), "inside").unwrap();
    symlink("inside", root.path().join("internal-link")).unwrap();
    let tools = WorkspaceTools::open(root.path(), grants(), Limits::default()).unwrap();
    for path in ["file-link", "internal-link", "dir-link/secret"] {
        assert!(call(&tools, "read", json!({"path":path})).is_err());
        assert!(
            call(
                &tools,
                "write",
                json!({"path":path,"content":"bad","mode":"overwrite"})
            )
            .is_err()
        );
    }
    assert!(
        call(
            &tools,
            "write",
            json!({"path":"dir-link/new","content":"bad","mode":"create"})
        )
        .is_err()
    );
    assert!(call(&tools, "list", json!({"path":"dir-link"})).is_err());
    assert_eq!(
        fs::read_to_string(outside.path().join("secret")).unwrap(),
        "outside"
    );
    assert!(!outside.path().join("new").exists());
    let list = call(&tools, "list", json!({"path":"."})).unwrap();
    assert!(
        list["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"] == "symlink")
    );
}

#[test]
fn bounds_are_checked_before_mutating_and_without_silent_truncation() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("file"), "123456789").unwrap();
    let limits = Limits {
        request_bytes: 256,
        read_bytes: 8,
        write_bytes: 8,
        output_bytes: 128,
        entries: 1,
    };
    let tools = WorkspaceTools::open(dir.path(), grants(), limits).unwrap();
    assert!(matches!(
        tools.execute_json("read", &[b' '; 257]),
        Err(ToolError::Limit(_))
    ));
    assert!(matches!(
        call(&tools, "read", json!({"path":"file"})),
        Err(ToolError::Limit(_))
    ));
    assert!(matches!(
        call(
            &tools,
            "write",
            json!({"path":"file","content":"123456789","mode":"overwrite"})
        ),
        Err(ToolError::Limit(_))
    ));
    assert_eq!(
        fs::read_to_string(dir.path().join("file")).unwrap(),
        "123456789"
    );
    fs::write(dir.path().join("other"), "").unwrap();
    assert!(matches!(
        call(&tools, "list", json!({"path":"."})),
        Err(ToolError::Limit(_))
    ));
}

#[test]
fn output_budget_counts_json_escaping_and_entry_names() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("escaped"), "\"".repeat(100)).unwrap();
    let tools = WorkspaceTools::open(
        dir.path(),
        grants(),
        Limits {
            output_bytes: 128,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(matches!(
        call(&tools, "read", json!({"path":"escaped"})),
        Err(ToolError::Limit(_))
    ));
    fs::write(dir.path().join("long".repeat(40)), "").unwrap();
    assert!(matches!(
        call(&tools, "list", json!({"path":"."})),
        Err(ToolError::Limit(_))
    ));
}

#[test]
fn malformed_inputs_binary_files_and_implicit_write_modes_fail() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("binary"), [255, 254]).unwrap();
    let tools = WorkspaceTools::open(dir.path(), grants(), Limits::default()).unwrap();
    for input in [
        json!({"path":"file","content":"x"}),
        json!({"path":"file","content":"x","mode":"append"}),
        json!({"path":"file","content":"x","mode":"create","extra":true}),
    ] {
        assert!(matches!(
            call(&tools, "write", input),
            Err(ToolError::Invalid(_))
        ));
    }
    assert!(matches!(
        call(&tools, "read", json!({"path":"binary"})),
        Err(ToolError::Invalid(_))
    ));
    assert!(call(&tools, "read", json!({"path":".","extra":true})).is_err());
}

#[test]
fn static_hardlink_write_is_rejected_and_fifo_does_not_block() {
    let dir = tempdir().unwrap();
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("file"), "unchanged").unwrap();
    fs::hard_link(outside.path().join("file"), dir.path().join("alias")).unwrap();
    let tools = WorkspaceTools::open(dir.path(), grants(), Limits::default()).unwrap();
    assert!(matches!(
        call(
            &tools,
            "write",
            json!({"path":"alias","content":"bad","mode":"overwrite"})
        ),
        Err(ToolError::Invalid(_))
    ));
    assert_eq!(
        fs::read_to_string(outside.path().join("file")).unwrap(),
        "unchanged"
    );
    use std::os::unix::ffi::OsStrExt;
    let fifo = std::ffi::CString::new(dir.path().join("fifo").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(call(&tools, "read", json!({"path":"fifo"})).is_err());
    assert!(
        call(
            &tools,
            "write",
            json!({"path":"fifo","content":"bad","mode":"overwrite"})
        )
        .is_err()
    );
}
