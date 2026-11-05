//! Signal-free proof that an owned process group is gone after its leader is reaped.
#![cfg(unix)]
use super::{Process, RuntimeError};
use crate::pipe_workers::POLL_INTERVAL;
use std::{
    thread,
    time::{Duration, Instant},
};

/// Bound for the kernel to drop an exited group after its leader is reaped.
const TEARDOWN_GRACE: Duration = Duration::from_secs(1);
/// Bound once the caller is cancelled. It still exceeds the measured teardown lag
/// (about 33ms under load), so cancellation shortens the wait without turning a
/// cleanly exited group into a cleanup failure.
const CANCELLED_GRACE: Duration = Duration::from_millis(100);

/// macOS rejects group signals with EPERM while the only member is a zombie, and under
/// load it can keep reporting the group for tens of milliseconds after that member is
/// reaped. Accept the group as gone only once signal 0 reports ESRCH within the grace.
/// Fails on a signalable member, persistent EPERM or any other errno. The probe sends
/// no signal, so polling cannot reach a process that later reuses the released PID.
/// `cancelled` reports caller cancellation; it shortens the wait.
pub(crate) fn reaped_group_vanishes(
    probe: impl FnMut() -> Option<i32>,
    cancelled: impl FnMut() -> bool,
) -> bool {
    vanishes(probe, TEARDOWN_GRACE, cancelled)
}

/// Sends signal 0 to the group: `None` when delivered, otherwise the errno.
pub(crate) fn signal_probe(pgid: i32) -> impl FnMut() -> Option<i32> {
    move || {
        if unsafe { libc::kill(-pgid, 0) } == 0 {
            None
        } else {
            std::io::Error::last_os_error().raw_os_error()
        }
    }
}

impl Process {
    /// Accepts an EPERM group kill only once the exiting leader is reaped and `probe`
    /// proves the group gone. Callers past their deadline (loading stops) may still
    /// wait out the bounded teardown grace here.
    pub(super) fn reap_exiting_group(
        &mut self,
        probe: impl FnMut() -> Option<i32>,
    ) -> Result<bool, RuntimeError> {
        let deadline = Instant::now() + Duration::from_millis(100);
        loop {
            self.check_child_ownership()?;
            if self.child.try_wait()?.is_some() {
                // Reaping releases the PID reservation: never signal this group again.
                self.stopped = true;
                return Ok(reaped_group_vanishes(probe, || false));
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
}

fn vanishes(
    mut probe: impl FnMut() -> Option<i32>,
    grace: Duration,
    mut cancelled: impl FnMut() -> bool,
) -> bool {
    let started = Instant::now();
    loop {
        match probe() {
            Some(libc::ESRCH) => return true,
            Some(libc::EPERM) => {
                let limit = if cancelled() {
                    grace.min(CANCELLED_GRACE)
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
    fn cancellation_shortens_the_wait_but_still_covers_teardown_lag() {
        // A cancelled caller stops waiting on a stuck group well before the full grace.
        let started = Instant::now();
        assert!(!vanishes(
            || Some(libc::EPERM),
            Duration::from_secs(30),
            || true
        ));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(started.elapsed() >= CANCELLED_GRACE);
        // Cancellation arriving mid-wait ends the wait at the next probe.
        let mut polls = 0;
        let started = Instant::now();
        assert!(!vanishes(
            || Some(libc::EPERM),
            Duration::from_secs(30),
            || {
                polls += 1;
                started.elapsed() >= CANCELLED_GRACE * 2
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
        assert!(reaped_group_vanishes(signal_probe(pgid), || false));
        let mut live = std::process::Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let pgid = live.id() as i32;
        let started = Instant::now();
        let vanished = reaped_group_vanishes(signal_probe(pgid), || false);
        live.kill().unwrap();
        live.wait().unwrap();
        assert!(!vanished);
        // Fails at once rather than waiting out the grace.
        assert!(started.elapsed() < TEARDOWN_GRACE);
    }
}
