#![cfg(unix)]
mod tool_support;
use foundry_runtime::tool_process::{ToolPoll, ToolProcess, ToolProcessError};
use foundry_tools::host::{Input, Outcome};
use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tool_support::{Fixture, assert_gone, finish, request};

#[test]
fn validated_reply_clean_exit_and_isolated_context_have_single_terminal_delivery() {
    let fixture = Fixture::new("context");
    let mut process = fixture.start();
    let reply = finish(&mut process).unwrap();
    let Outcome::Ok { output } = reply.result else {
        panic!("missing success")
    };
    let context: serde_json::Value =
        serde_json::from_str(output["content"].as_str().unwrap()).unwrap();
    assert_eq!(context["pid"], context["group"]);
    assert_eq!(context["cwd"], "/");
    assert_eq!(context["home"], false);
    assert_eq!(context["path"], false);
    assert_gone(context["pid"].as_i64().unwrap() as i32);
    assert!(matches!(process.poll(), ToolPoll::Consumed));
    process.request_stop();
    assert!(matches!(process.poll(), ToolPoll::Consumed));
}

#[test]
fn stopped_or_denied_requests_do_not_spawn() {
    let fixture = Fixture::new("context");
    let stopped = Arc::new(AtomicBool::new(true));
    assert!(matches!(
        ToolProcess::start(fixture.spec(), request(), stopped),
        Err(ToolProcessError::Stopped)
    ));
    let mut denied = request();
    denied.grants.clear();
    let mut process =
        ToolProcess::start(fixture.spec(), denied, Arc::new(AtomicBool::new(false))).unwrap();
    assert!(matches!(
        finish(&mut process),
        Err(ToolProcessError::Denied)
    ));
    assert!(!fixture.workspace.join("started").exists());
}

#[test]
fn polling_stays_responsive_and_local_stop_reaps_before_terminal() {
    let fixture = Fixture::new("hang");
    let mut process = fixture.start();
    let pid = fixture.pid("started");
    let now = Instant::now();
    for _ in 0..1000 {
        assert!(matches!(process.poll(), ToolPoll::Pending));
    }
    assert!(now.elapsed() < Duration::from_millis(100));
    process.request_stop();
    assert!(matches!(
        finish(&mut process),
        Err(ToolProcessError::Stopped)
    ));
    assert_gone(pid);
}

#[test]
fn shared_stop_and_drop_interrupt_blocked_request_writer() {
    for drop_only in [false, true] {
        let fixture = Fixture::new("blocked_input");
        let stop = Arc::new(AtomicBool::new(false));
        let mut req = request();
        req.tool = "write".into();
        req.grants = vec![foundry_tools::host::Grant::Create];
        req.input = Input::Write(foundry_tools::host::WriteArgs {
            path: "unused".into(),
            content: "x".repeat(262144),
            mode: foundry_tools::host::WriteMode::Create,
        });
        let mut process = ToolProcess::start(fixture.spec(), req, Arc::clone(&stop)).unwrap();
        let pid = fixture.pid("started");
        if drop_only {
            drop(process);
        } else {
            stop.store(true, Ordering::Release);
            assert!(matches!(
                finish(&mut process),
                Err(ToolProcessError::Stopped)
            ));
        }
        assert_gone(pid);
        assert!(!fixture.workspace.join("unused").exists());
    }
}

#[test]
fn malformed_duplicate_and_mismatched_replies_never_become_success() {
    for scenario in [
        "wrong_id",
        "wrong_version",
        "wrong_shape",
        "duplicate",
        "unknown_error",
        "trailing",
        "huge",
        "nonzero",
    ] {
        let fixture = Fixture::new(scenario);
        let mut process = fixture.start();
        assert!(finish(&mut process).is_err(), "{scenario}");
        assert_gone(fixture.pid("started"));
    }
}

#[test]
fn reply_without_exit_or_eof_times_out() {
    for scenario in ["hang", "closed_hang"] {
        let fixture = Fixture::new(scenario);
        let mut process = fixture.start();
        let pid = fixture.pid("reply_sent");
        assert!(matches!(
            finish(&mut process),
            Err(ToolProcessError::Timeout)
        ));
        assert_gone(pid);
    }
}

#[test]
fn renamed_directory_capability_and_unrelated_descriptor_are_isolated() {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new("read_capability");
    fs::write(fixture.workspace.join("value"), "original").unwrap();
    let spec = fixture.spec();
    fs::rename(&fixture.workspace, fixture.path.join("moved")).unwrap();
    fs::create_dir(&fixture.workspace).unwrap();
    fs::write(fixture.workspace.join("value"), "decoy").unwrap();
    let mut process =
        ToolProcess::start(spec, request(), Arc::new(AtomicBool::new(false))).unwrap();
    let Outcome::Ok { output } = finish(&mut process).unwrap().result else {
        panic!("missing success")
    };
    assert_eq!(output["content"], "original");
    let fixture = Fixture::new("descriptor");
    let file = fs::File::create(fixture.path.join("sentinel")).unwrap();
    unsafe {
        assert_eq!(libc::fcntl(file.as_raw_fd(), libc::F_SETFD, 0), 0);
    }
    let mut req = request();
    let Input::Path(args) = &mut req.input else {
        unreachable!()
    };
    args.path = format!("{}:{}", file.as_raw_fd(), file.metadata().unwrap().ino());
    let mut process =
        ToolProcess::start(fixture.spec(), req, Arc::new(AtomicBool::new(false))).unwrap();
    let Outcome::Ok { output } = finish(&mut process).unwrap().result else {
        panic!("missing success")
    };
    assert_eq!(output["content"], "closed");
}

#[test]
fn bounded_stderr_drain_and_explicit_tool_error_are_clean_completions() {
    let fixture = Fixture::new("stderr");
    let mut process = fixture.start();
    assert!(finish(&mut process).is_ok());
    let fixture = Fixture::new("tool_error");
    let mut process = fixture.start();
    assert!(matches!(
        finish(&mut process).unwrap().result,
        Outcome::Error { .. }
    ));
}

#[test]
fn exited_leader_keeps_pid_reserved_until_owned_descendant_cleanup() {
    let fixture = Fixture::new("descendant");
    let mut process = fixture.start();
    let leader = fixture.pid("started");
    let descendant = fixture.pid("descendant");
    assert!(finish(&mut process).is_ok());
    assert_gone(leader);
    // Orphan zombies can await the system reaper; they are no longer executing.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let status = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &descendant.to_string()])
            .output()
            .unwrap();
        let state = String::from_utf8(status.stdout).unwrap();
        if state.trim().is_empty() || state.trim().starts_with('Z') {
            break;
        }
        assert!(Instant::now() < deadline, "owned descendant remained live");
        std::thread::sleep(Duration::from_millis(10));
    }
    // Fixture must not send another signal to a PID which may now be reusable.
    fs::remove_file(fixture.workspace.join("descendant")).unwrap();
}

#[test]
fn detached_pipe_holder_cannot_block_cooperative_worker_join() {
    let fixture = Fixture::new("detached");
    let mut process = fixture.start();
    let leader = fixture.pid("started");
    let descendant = fixture.pid("descendant");
    assert!(matches!(
        finish(&mut process),
        Err(ToolProcessError::Timeout)
    ));
    assert_gone(leader);
    assert_eq!(unsafe { libc::kill(descendant, 0) }, 0);
    // The deliberately detached fixture lies outside the owned group. Fixture
    // Drop kills it; production makes no sandbox/containment claim for detachment.
}
