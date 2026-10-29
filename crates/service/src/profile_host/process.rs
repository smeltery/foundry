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
    fn stop(&mut self) -> Result<(), Failure> {
        self.stop_using(|pid| {
            if unsafe { libc::kill(-pid, libc::SIGKILL) } == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        })
    }
    fn stop_using(
        &mut self,
        signal: impl FnOnce(i32) -> std::io::Result<()>,
    ) -> Result<(), Failure> {
        self.exit_success()?;
        let error = match signal(self.pid) {
            Ok(()) => return Ok(()),
            Err(error) => error.raw_os_error(),
        };
        if error == Some(libc::ESRCH) {
            return Ok(());
        }
        // Darwin can report EPERM after a live leader exits between the
        // ownership check and kill. Re-observe without reaping before checking
        // exact group membership; the reserved PID must never be reused here.
        #[cfg(target_os = "macos")]
        if error == Some(libc::EPERM) && self.exit_success()?.is_some() {
            let mut members = [0_i32; 2];
            let count = unsafe {
                libc::proc_listpgrppids(
                    self.pid,
                    members.as_mut_ptr().cast(),
                    std::mem::size_of_val(&members) as i32,
                )
            };
            if count == 1 && members[0] == self.pid {
                return Ok(());
            }
        }
        Err(Failure::Cleanup)
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
    let mut kill_ok = group.stop().is_ok();
    let mut sink = [0; 4096];
    let mut out_closed = false;
    let mut err_closed = false;
    while Instant::now() < cleanup_deadline {
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
        // A descendant can fork concurrently with the first group signal. Keep
        // the leader waitable so repeated signals cannot target a reused PID.
        // Require a successful final signal; an intermediate EPERM while other
        // dying members remain is not itself cleanup proof.
        kill_ok = group.stop().is_ok();
        if kill_ok && out_closed && err_closed {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    // No group signal is permitted after this final reap, even on timeout.
    let waited = reap_until(&mut child, &mut group, cleanup_deadline);
    if !kill_ok || !waited || !out_closed || !err_closed {
        return Err(Failure::Cleanup);
    }
    operation?;
    Ok(output)
}

#[cfg(test)]
#[path = "process_tests.rs"]
mod reservation_tests;
