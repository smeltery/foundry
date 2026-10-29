use super::Error as Failure;
use std::{
    io::{Read, Write},
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

struct GroupReservation {
    pid: i32,
    owned: bool,
}
impl GroupReservation {
    fn exit_success(&mut self) -> Result<Option<bool>, Failure> {
        if !self.owned {
            return Err(Failure::Cleanup);
        }
        // WNOWAIT leaves the leader unreaped, reserving its PID until signaling ends.
        for _ in 0..4 {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                if error.raw_os_error() == Some(libc::ECHILD) {
                    self.owned = false;
                }
                return Err(Failure::Cleanup);
            }
            if unsafe { info.si_pid() } == 0 {
                return Ok(None);
            }
            return Ok(Some(
                info.si_code == libc::CLD_EXITED && unsafe { info.si_status() } == 0,
            ));
        }
        Err(Failure::Cleanup)
    }
    fn signal_with(&mut self, signal: impl FnOnce(i32) -> bool) -> Result<(), Failure> {
        self.exit_success()?;
        if signal(self.pid) {
            Ok(())
        } else {
            Err(Failure::Cleanup)
        }
    }
    fn stop(&mut self) -> Result<(), Failure> {
        let exited = self.exit_success()?.is_some();
        self.signal_with(|pid| {
            let result = unsafe { libc::kill(-pid, libc::SIGKILL) };
            if result == 0 {
                return true;
            }
            let error = std::io::Error::last_os_error().raw_os_error();
            if error == Some(libc::ESRCH) {
                return true;
            }
            // Darwin reports EPERM for a group containing only its zombie leader.
            // A second member or failed enumeration must remain cleanup failure.
            #[cfg(target_os = "macos")]
            if exited && error == Some(libc::EPERM) {
                let mut members = [0_i32; 2];
                let count = unsafe {
                    libc::proc_listpgrppids(
                        pid,
                        members.as_mut_ptr().cast(),
                        std::mem::size_of_val(&members) as i32,
                    )
                };
                return count == 1 && members[0] == pid;
            }
            let _ = exited;
            false
        })
    }
}

fn reap_until(
    child: &mut std::process::Child,
    group: &mut GroupReservation,
    deadline: Instant,
) -> bool {
    while Instant::now() < deadline {
        match group.exit_success() {
            Ok(Some(_)) => match child.try_wait() {
                Ok(Some(_)) => {
                    group.owned = false;
                    return true;
                }
                Ok(None) => {}
                Err(_) => return false,
            },
            Ok(None) => {}
            Err(_) => return false,
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    false
}

fn nonblocking(fd: i32) -> Result<(), Failure> {
    // The descriptor belongs to a live pipe and flags are only augmented.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(Failure::Internal);
    }
    Ok(())
}
fn drain(reader: &mut impl Read, bytes: &mut Vec<u8>, limit: usize) -> Result<bool, Failure> {
    let mut buffer = [0; 4096];
    // Bound each poll to preserve cancellation even while output keeps arriving.
    for _ in 0..4 {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => {
                if count > limit.saturating_sub(bytes.len()) {
                    return Err(Failure::Invalid);
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(Failure::Internal),
        }
    }
    Ok(false)
}
/// Unix host only. No pipe worker can outlive this call. The registered codec
/// and its descendants must remain in the owned process group.
pub fn run_with_output_limit(
    command: &mut Command,
    input: &[u8],
    deadline: Instant,
    cancelled: impl Fn() -> bool,
    output_limit: usize,
) -> Result<Vec<u8>, Failure> {
    if output_limit == 0 || output_limit > 2 * 1048576 {
        return Err(Failure::Invalid);
    }
    if input.len() > 8 * 1024 * 1024 {
        return Err(Failure::Invalid);
    }
    if Instant::now() >= deadline {
        return Err(Failure::Deadline);
    }
    if cancelled() {
        return Err(Failure::Cancelled);
    }
    let mut child = command
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| Failure::Internal)?;
    let mut group = GroupReservation {
        pid: child.id() as i32,
        owned: true,
    };
    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take().ok_or(Failure::Internal)?;
    let mut stderr = child.stderr.take().ok_or(Failure::Internal)?;
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let configured = nonblocking(stdin.as_ref().ok_or(Failure::Internal)?.as_raw_fd())
        .and_then(|_| nonblocking(stdout.as_raw_fd()))
        .and_then(|_| nonblocking(stderr.as_raw_fd()));
    if configured.is_err() {
        let cleanup_deadline = Instant::now() + Duration::from_millis(250);
        drop(stdin);
        let _ = group.stop();
        reap_until(&mut child, &mut group, cleanup_deadline);
        return Err(Failure::Cleanup);
    }
    let operation = (|| {
        let mut sent = 0;
        loop {
            if Instant::now() >= deadline {
                return Err(Failure::Deadline);
            }
            if cancelled() {
                return Err(Failure::Cancelled);
            }
            if let Some(writer) = stdin.as_mut() {
                match writer.write(&input[sent..]) {
                    Ok(n) => sent += n,
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => return Err(Failure::Invalid),
                }
                if sent == input.len() {
                    stdin = None;
                }
            }
            let out_eof = drain(&mut stdout, &mut output, output_limit)?;
            let err_eof = drain(&mut stderr, &mut errors, 4096)?;
            if let Some(success) = group.exit_success()? {
                if !success || !errors.is_empty() || sent != input.len() {
                    return Err(Failure::Invalid);
                }
                if out_eof && err_eof {
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    })();
    drop(stdin);
    // Kill the entire owned group even on successful parent exit: inherited pipes
    // must not keep an unchecked descendant alive.
    let cleanup_deadline = Instant::now() + Duration::from_millis(250);
    let kill_ok = group.stop().is_ok();
    let waited = reap_until(&mut child, &mut group, cleanup_deadline);
    let mut sink = [0; 4096];
    let mut out_closed = false;
    let mut err_closed = false;
    while Instant::now() < cleanup_deadline && !(out_closed && err_closed) {
        for (pipe, closed) in [
            (&mut stdout as &mut dyn Read, &mut out_closed),
            (&mut stderr as &mut dyn Read, &mut err_closed),
        ] {
            if *closed {
                continue;
            }
            match pipe.read(&mut sink) {
                Ok(0) => *closed = true,
                Ok(_) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => break,
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    if !kill_ok || !waited || !out_closed || !err_closed {
        return Err(Failure::Cleanup);
    }
    operation?;
    Ok(output)
}

#[cfg(test)]
mod reservation_tests {
    use super::*;
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
        assert!(group.signal_with(|_| false).is_err());
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
            reservation.signal_with(|_| {
                called = true;
                true
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
}
