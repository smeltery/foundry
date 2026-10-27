#![cfg(unix)]
use foundry_tools::{Grants, Limits, WorkspaceTools};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::symlink,
    sync::{Arc, Barrier},
};
use tempfile::tempdir;

#[test]
fn directory_symlink_swaps_do_not_redirect_operations_outside_capability() {
    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    fs::create_dir(root.path().join("slot")).unwrap();
    fs::write(root.path().join("slot/secret"), "inside").unwrap();
    fs::write(outside.path().join("secret"), "outside").unwrap();
    let tools = WorkspaceTools::open(
        root.path(),
        Grants {
            read: true,
            list: true,
            create: true,
            overwrite: true,
        },
        Limits::default(),
    )
    .unwrap();
    let root_path = root.path().to_owned();
    let outside_path = outside.path().to_owned();
    let barrier = Arc::new(Barrier::new(2));
    let worker_barrier = Arc::clone(&barrier);
    let worker = std::thread::spawn(move || {
        worker_barrier.wait();
        for _ in 0..500 {
            fs::rename(root_path.join("slot"), root_path.join("parked")).unwrap();
            symlink(&outside_path, root_path.join("slot")).unwrap();
            std::thread::yield_now();
            fs::remove_file(root_path.join("slot")).unwrap();
            fs::rename(root_path.join("parked"), root_path.join("slot")).unwrap();
        }
    });
    barrier.wait();
    for _ in 0..500 {
        if let Ok(result) = tools.execute_json("read", br#"{"path":"slot/secret"}"#) {
            assert_eq!(result["content"], "inside");
        }
        let _ = tools.execute_json(
            "write",
            &serde_json::to_vec(&json!({"path":"slot/new","content":"inside","mode":"create"}))
                .unwrap(),
        );
        let _ = tools.execute_json("list", br#"{"path":"slot"}"#);
    }
    worker.join().unwrap();
    assert!(!outside.path().join("new").exists());
    assert_eq!(
        fs::read_to_string(outside.path().join("secret")).unwrap(),
        "outside"
    );
}

#[test]
fn leaf_symlink_swaps_cannot_redirect_overwrites() {
    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    fs::write(root.path().join("file"), "inside").unwrap();
    fs::write(outside.path().join("secret"), "outside").unwrap();
    let tools = WorkspaceTools::open(
        root.path(),
        Grants {
            overwrite: true,
            ..Grants::default()
        },
        Limits::default(),
    )
    .unwrap();
    let root_path = root.path().to_owned();
    let target = outside.path().join("secret");
    let barrier = Arc::new(Barrier::new(2));
    let worker_barrier = Arc::clone(&barrier);
    let worker = std::thread::spawn(move || {
        worker_barrier.wait();
        for _ in 0..500 {
            fs::rename(root_path.join("file"), root_path.join("parked")).unwrap();
            symlink(&target, root_path.join("file")).unwrap();
            std::thread::yield_now();
            fs::remove_file(root_path.join("file")).unwrap();
            fs::rename(root_path.join("parked"), root_path.join("file")).unwrap();
        }
    });
    barrier.wait();
    for _ in 0..500 {
        let _ = tools.execute_json(
            "write",
            br#"{"path":"file","content":"inside","mode":"overwrite"}"#,
        );
    }
    worker.join().unwrap();
    assert_eq!(
        fs::read_to_string(outside.path().join("secret")).unwrap(),
        "outside"
    );
}

#[test]
fn root_handle_remains_bound_when_ambient_path_is_replaced() {
    let temporary = tempdir().unwrap();
    let root = temporary.path().join("root");
    let outside = temporary.path().join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(root.join("file"), "inside").unwrap();
    fs::write(outside.join("file"), "outside").unwrap();
    let tools = WorkspaceTools::open(
        &root,
        Grants {
            read: true,
            ..Grants::default()
        },
        Limits::default(),
    )
    .unwrap();
    fs::rename(&root, temporary.path().join("moved")).unwrap();
    symlink(outside, &root).unwrap();
    assert_eq!(
        tools.execute_json("read", br#"{"path":"file"}"#).unwrap()["content"],
        "inside"
    );
}
