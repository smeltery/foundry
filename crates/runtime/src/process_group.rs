//! Signal-free proof that an owned process group is gone after its leader is reaped.
#![cfg(unix)]
use crate::pipe_workers::POLL_INTERVAL;
use std::{
    thread,
    time::{Duration, Instant},
};

/// Bound for the kernel to drop an exited group after its leader is reaped.
const TEARDOWN_GRACE: Duration = Duration::from_secs(1);
/// Bound once the caller is cancelled or past its deadline. It still exceeds the
/// measured teardown lag (about 33ms under load), so cancellation shortens the wait
/// without turning a cleanly exited group into a cleanup failure.
const INTERRUPTED_GRACE: Duration = Duration::from_millis(100);

/// macOS rejects group signals with EPERM while the only member is a zombie, and under
/// load it can keep reporting the group for tens of milliseconds after that member is
/// reaped. Accept the group as gone only once signal 0 reports ESRCH within the grace.
/// Fails on a signalable member, persistent EPERM or any other errno. The probe sends
/// no signal, so polling cannot reach a process that later reuses the released PID.
/// `interrupted` reports caller cancellation or a passed deadline; it shortens the wait.
pub(crate) fn reaped_group_vanishes(pgid: i32, interrupted: impl FnMut() -> bool) -> bool {
    let probe = || {
        if unsafe { libc::kill(-pgid, 0) } == 0 {
            None
        } else {
            std::io::Error::last_os_error().raw_os_error()
        }
    };
    vanishes(probe, TEARDOWN_GRACE, interrupted)
}

/// `probe` returns `None` when signal 0 is delivered, otherwise the errno.
fn vanishes(
    mut probe: impl FnMut() -> Option<i32>,
    grace: Duration,
    mut interrupted: impl FnMut() -> bool,
) -> bool {
    let started = Instant::now();
    loop {
        match probe() {
            Some(libc::ESRCH) => return true,
            Some(libc::EPERM) => {
                let limit = if interrupted() {
                    grace.min(INTERRUPTED_GRACE)
                } else {
                    grace
                };
                if started.elapsed() >= limit {
                    return false;
                }
                thread::sleep(POLL_INTERVAL);
            }
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reaped_group_is_gone_only_after_probe_reports_absence() {
        // macOS can report a reaped zombie's group with EPERM briefly before ESRCH.
        let mut probes = [libc::EPERM, libc::EPERM, libc::ESRCH]
            .into_iter()
            .map(Some);
        assert!(vanishes(
            || probes.next().unwrap(),
            Duration::from_secs(5),
            || false
        ));
        // A group that keeps rejecting probes, gains a signalable member, or reports any
        // other error is not proven gone.
        assert!(!vanishes(
            || Some(libc::EPERM),
            Duration::from_millis(30),
            || false
        ));
        let mut probes = [Some(libc::EPERM), None].into_iter();
        assert!(!vanishes(
            || probes.next().unwrap(),
            Duration::from_secs(5),
            || false
        ));
        assert!(!vanishes(
            || Some(libc::EINVAL),
            Duration::from_secs(5),
            || false
        ));
    }

    #[test]
    fn interruption_shortens_the_wait_but_still_covers_teardown_lag() {
        // A cancelled caller stops waiting on a stuck group well before the full grace.
        let started = Instant::now();
        assert!(!vanishes(
            || Some(libc::EPERM),
            Duration::from_secs(30),
            || true
        ));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(started.elapsed() >= INTERRUPTED_GRACE);
        // Interruption arriving mid-wait ends the wait at the next probe.
        let mut polls = 0;
        let started = Instant::now();
        assert!(!vanishes(
            || Some(libc::EPERM),
            Duration::from_secs(30),
            || {
                polls += 1;
                started.elapsed() >= INTERRUPTED_GRACE * 2
            }
        ));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(polls > 1);
        // Lag shorter than the shortened bound is still accepted as a vanished group.
        let mut probes = [libc::EPERM, libc::EPERM, libc::ESRCH]
            .into_iter()
            .map(Some);
        assert!(vanishes(
            || probes.next().unwrap(),
            Duration::from_secs(30),
            || true
        ));
    }

    #[test]
    fn reaped_and_live_groups_are_distinguished() {
        use std::os::unix::process::CommandExt;
        // A reaped single-member group vanishes; a group with a live member never does.
        let mut exited = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .process_group(0)
            .spawn()
            .unwrap();
        let pgid = exited.id() as i32;
        exited.wait().unwrap();
        assert!(reaped_group_vanishes(pgid, || false));
        let mut live = std::process::Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let pgid = live.id() as i32;
        let started = Instant::now();
        let vanished = reaped_group_vanishes(pgid, || false);
        live.kill().unwrap();
        live.wait().unwrap();
        assert!(!vanished);
        // Fails at once rather than waiting out the grace.
        assert!(started.elapsed() < TEARDOWN_GRACE);
    }
}
