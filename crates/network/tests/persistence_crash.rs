#![cfg(unix)]
mod persistence_support;
use foundry_network::persistence::MEMBERSHIP_DATABASE;
use persistence_support::*;
use std::{
    process::Command,
    thread,
    time::{Duration, Instant},
};

#[test]
#[ignore = "invoked as a bounded child by rollback test"]
fn uncommitted_child() {
    let path = std::env::var_os("PROJECTION_CRASH_DB").unwrap();
    let connection = rusqlite::Connection::open(std::path::PathBuf::from(path)).unwrap();
    connection
        .execute_batch(
            "BEGIN IMMEDIATE; UPDATE projection SET revision=99,payload=zeroblob(1000000)",
        )
        .unwrap();
    // Exit without running Connection/transaction destructors.
    std::process::exit(0);
}
#[test]
fn abrupt_process_exit_does_not_publish_uncommitted_snapshot() {
    let fixture = Fixture::new();
    drop(fixture.initialized());
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "uncommitted_child", "--ignored"])
        .env(
            "PROJECTION_CRASH_DB",
            fixture.root.join(MEMBERSHIP_DATABASE),
        )
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("child deadline or wait failure");
            }
        }
    };
    assert!(status.success());
    let loaded = fixture
        .open()
        .load("team", fixture.founder.public(), &fixture.profile, NOW)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.revision, 1);
    assert_eq!(loaded.state.snapshot(), &fixture.initial());
}
