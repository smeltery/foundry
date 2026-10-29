use super::*;
#[cfg(target_os = "macos")]
#[test]
fn leader_exit_between_observation_and_signal_keeps_cleanup_provable() {
    let mut child = Command::new("/bin/cat")
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut group = GroupReservation {
        pid: child.id() as i32,
        owned: true,
    };
    assert_eq!(group.exit_success().unwrap(), None);
    let result = group.stop_using(|pid| {
        // The initial ownership/exit observation has already seen a live leader.
        drop(child.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe {
                    libc::waitid(
                        libc::P_PID,
                        pid as libc::id_t,
                        &mut info,
                        libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                    )
                },
                0
            );
            if unsafe { info.si_pid() } != 0 {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(unsafe { libc::kill(-pid, libc::SIGKILL) }, -1);
        let error = std::io::Error::last_os_error();
        assert_eq!(error.raw_os_error(), Some(libc::EPERM));
        Err(error)
    });
    // Always reap the fixture, including the expected pre-fix failure.
    assert!(child.wait().unwrap().success());
    result.expect("current reserved zombie-only group is cleaned");
}
#[test]
fn marker_triggered_cancellation_cleans_shell_and_child_pipes() {
    let root = tempfile::tempdir().unwrap();
    for index in 0..32 {
        let marker = root.path().join(index.to_string());
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "/usr/bin/touch \"$1\"; /bin/sleep 3", "fixture"])
            .arg(&marker);
        let result = run_with_output_limit(
            &mut command,
            b"request",
            Instant::now() + Duration::from_secs(5),
            || marker.exists(),
            1024,
        );
        assert!(
            matches!(result, Err(Failure::Cancelled)),
            "marker cancellation case {index}: {result:?}"
        );
    }
}
#[test]
fn failed_stop_does_not_wait_for_a_live_leader() {
    let mut child = Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let mut group = GroupReservation {
        pid: child.id() as i32,
        owned: true,
    };
    assert!(
        group
            .stop_using(|_| Err(std::io::ErrorKind::PermissionDenied.into()))
            .is_err()
    );
    let started = Instant::now();
    assert!(!reap_until(
        &mut child,
        &mut group,
        started + Duration::from_millis(25)
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(child.try_wait().unwrap().is_none());
    group.stop().unwrap();
    assert!(reap_until(
        &mut child,
        &mut group,
        Instant::now() + Duration::from_secs(1)
    ));
}
#[test]
fn reaped_leader_never_signals_a_numeric_group() {
    let mut child = Command::new("/usr/bin/true")
        .process_group(0)
        .spawn()
        .unwrap();
    let mut reservation = GroupReservation {
        pid: child.id() as i32,
        owned: true,
    };
    child.wait().unwrap();
    let mut called = false;
    assert!(matches!(
        reservation.stop_using(|_| {
            called = true;
            Ok(())
        }),
        Err(Failure::Cleanup)
    ));
    assert!(!called);
    assert!(!reservation.owned);
}
#[test]
fn observed_exit_keeps_leader_waitable_until_cleanup() {
    let mut child = Command::new("/usr/bin/true")
        .process_group(0)
        .spawn()
        .unwrap();
    let mut reservation = GroupReservation {
        pid: child.id() as i32,
        owned: true,
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while reservation.exit_success().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(reservation.exit_success().unwrap(), Some(true));
    reservation.stop().unwrap();
    assert!(child.wait().unwrap().success());
}
