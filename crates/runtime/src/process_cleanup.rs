//! Capture-only observations survive saturated queues and owned worker shutdown.
use super::{ContextOutput, Process, RuntimeError, WireEvent};
use std::sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    mpsc::{Receiver, TryRecvError},
};

const FAULT: u8 = 1;
const DISCARDED: u8 = 2;
const EARLY_CLOSE: u8 = 4;

#[derive(Default)]
pub(super) struct CaptureCleanup {
    flags: AtomicU8,
    shutdown: AtomicBool,
}
impl CaptureCleanup {
    pub fn begin_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }
    pub fn observe(&self, event: &ContextOutput) {
        match event {
            ContextOutput::Ordinary(WireEvent::Error(RuntimeError::Io(
                std::io::ErrorKind::ConnectionAborted,
            ))) if self.shutdown.load(Ordering::Acquire) => {}
            ContextOutput::Ordinary(WireEvent::Error(_)) => {
                self.flags.fetch_or(FAULT, Ordering::AcqRel);
            }
            ContextOutput::Ordinary(WireEvent::Closed)
                if !self.shutdown.load(Ordering::Acquire) =>
            {
                self.flags.fetch_or(EARLY_CLOSE, Ordering::AcqRel);
            }
            _ => {}
        }
    }
    pub fn discarded(&self) {
        self.flags.fetch_or(DISCARDED, Ordering::AcqRel);
    }
    #[cfg(test)]
    pub fn has_fault(&self) -> bool {
        self.flags.load(Ordering::Acquire) & FAULT != 0
    }
    fn finish(&self, events: &Receiver<ContextOutput>) -> Result<(), RuntimeError> {
        loop {
            match events.try_recv() {
                Ok(event) => {
                    self.observe(&event);
                    if matches!(
                        event,
                        ContextOutput::Context(_)
                            | ContextOutput::Loading(_)
                            | ContextOutput::Observation(_)
                            | ContextOutput::Ordinary(WireEvent::Event(_))
                    ) {
                        self.discarded();
                    }
                }
                Err(TryRecvError::Disconnected) => break,
                // All owned senders must be gone after joins; never wait for an unknown owner.
                Err(TryRecvError::Empty) => {
                    return Err(RuntimeError::Protocol(
                        "capture output owner still live after cleanup",
                    ));
                }
            }
        }
        if self.flags.load(Ordering::Acquire) != 0 {
            return Err(RuntimeError::Protocol(
                "capture transport did not close cleanly",
            ));
        }
        Ok(())
    }
}

impl Process {
    #[cfg(unix)]
    pub(super) fn check_child_ownership(&mut self) -> Result<(), RuntimeError> {
        if self.ownership_lost {
            return Err(RuntimeError::Protocol("adapter child ownership lost"));
        }
        loop {
            let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.child.id(),
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                self.ownership_lost = true;
            }
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error.into());
            }
        }
    }

    /// Cancellation discards bounded queued output after joins, never certifies a cut.
    pub fn stop_context_cancelled(
        &mut self,
        events: &Receiver<ContextOutput>,
    ) -> Result<(), RuntimeError> {
        if self.cleanup.is_none() {
            return Err(RuntimeError::Invalid(
                "cancellation cleanup requires context transport",
            ));
        }
        let stopped = self.stop();
        let drained = loop {
            match events.try_recv() {
                Ok(_) => {}
                Err(TryRecvError::Disconnected) => break Ok(()),
                Err(TryRecvError::Empty) => {
                    break Err(RuntimeError::Protocol(
                        "cancelled output owner still live after cleanup",
                    ));
                }
            }
        };
        stopped.and(drained)
    }

    pub fn stop_context_checked(
        &mut self,
        events: &Receiver<ContextOutput>,
    ) -> Result<(), RuntimeError> {
        if self.cleanup.is_none() {
            return Err(RuntimeError::Invalid(
                "capture cleanup requires context transport",
            ));
        }
        let stopped = self.stop();
        let observed = self.cleanup.as_ref().unwrap().finish(events);
        stopped.and(observed)
    }
}

/// Linux: orphans of the adapter's descendants, including double-forked ones,
/// reparent to the adapter instead of init while it is alive. The attribute
/// survives `execve` and is set in the forked child before exec.
#[cfg(target_os = "linux")]
pub(super) fn adopt_orphans() -> std::io::Result<()> {
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use descendants::stop_descendants;

/// Other platforms signal only the owned group.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn stop_descendants(_: i32) -> Result<(), RuntimeError> {
    Ok(())
}

/// Freezes the adapter and every process whose parent chain leads to it, then
/// kills them. This reaches members that left the owned group or session; it
/// cannot follow a process whose parent chain no longer leads to the adapter.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod descendants {
    use crate::RuntimeError;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    const FREEZE_DEADLINE: Duration = Duration::from_secs(1);

    struct Entry {
        parent: i32,
        frozen: bool,
    }

    #[derive(PartialEq)]
    enum Signal {
        Sent,
        Gone,
        Denied,
    }

    fn signal(pid: i32, signal: libc::c_int) -> Signal {
        if unsafe { libc::kill(pid, signal) } == 0 {
            return Signal::Sent;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => Signal::Gone,
            _ => Signal::Denied,
        }
    }

    pub fn stop_descendants(leader: i32) -> Result<(), RuntimeError> {
        let deadline = Instant::now() + FREEZE_DEADLINE;
        // Member pid -> depth below the adapter.
        let mut tree = HashMap::from([(leader, 0_usize)]);
        let mut denied = signal(leader, libc::SIGSTOP) == Signal::Denied;
        let mut stable = 0;
        let mut failure = None;
        // Two consecutive complete listings without growth, the second begun after
        // every member was seen stopped: a stopped process cannot fork.
        while stable < 2 {
            let table = match processes(&tree) {
                Ok(table) => table,
                Err(error) => {
                    // Never leave already-stopped members behind: kill them below.
                    failure = Some(error);
                    break;
                }
            };
            let mut grew = false;
            loop {
                let found: Vec<_> = table
                    .iter()
                    .filter(|(pid, _)| !tree.contains_key(*pid))
                    .filter_map(|(pid, entry)| Some((*pid, tree.get(&entry.parent)? + 1)))
                    .collect();
                if found.is_empty() {
                    break;
                }
                for (pid, depth) in found {
                    // Discovered from a listing, so a running parent could reap it and the
                    // pid be recycled first; that requires a full pid-space wrap meanwhile.
                    denied |= signal(pid, libc::SIGSTOP) == Signal::Denied;
                    tree.insert(pid, depth);
                    grew = true;
                }
            }
            let mut frozen = true;
            for pid in tree.keys() {
                if table.get(pid).is_some_and(|entry| !entry.frozen) {
                    frozen = false;
                    // Also stops a member that another member resumed with SIGCONT.
                    denied |= signal(*pid, libc::SIGSTOP) == Signal::Denied;
                }
            }
            stable = if grew || !frozen { 0 } else { stable + 1 };
            if stable < 2 {
                if denied || Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        // Kill leaves first: no stopped member is left behind a dead parent, so an
        // orphaned-group SIGHUP/SIGCONT cannot resume one before its own SIGKILL.
        let mut members: Vec<_> = tree.into_iter().filter(|(pid, _)| *pid != leader).collect();
        members.sort_by_key(|(_, depth)| std::cmp::Reverse(*depth));
        for (pid, _) in members {
            denied |= signal(pid, libc::SIGKILL) == Signal::Denied;
        }
        if let Some(error) = failure {
            return Err(error);
        }
        if denied {
            return Err(RuntimeError::Protocol(
                "adapter descendant could not be signalled",
            ));
        }
        if stable < 2 {
            return Err(RuntimeError::Protocol(
                "adapter descendants did not stop in time",
            ));
        }
        Ok(())
    }

    /// Every visible process; one that exits while listed is simply absent.
    #[cfg(target_os = "linux")]
    fn processes(_: &HashMap<i32, usize>) -> Result<HashMap<i32, Entry>, RuntimeError> {
        let mut table = HashMap::new();
        for entry in std::fs::read_dir("/proc")? {
            let Some(pid) = entry?
                .file_name()
                .to_str()
                .and_then(|name| name.parse().ok())
            else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                continue;
            };
            // The command name may contain spaces and parentheses; it ends at the last ')'.
            let mut fields = stat
                .rsplit_once(')')
                .map_or("", |(_, rest)| rest)
                .split_whitespace();
            let (Some(state), Some(Ok(parent))) = (fields.next(), fields.next().map(str::parse))
            else {
                return Err(RuntimeError::Protocol("unreadable process status"));
            };
            let frozen = matches!(state, "T" | "t" | "Z" | "X");
            table.insert(pid, Entry { parent, frozen });
        }
        Ok(table)
    }

    /// Members and their children; a process that exits meanwhile is simply absent.
    #[cfg(target_os = "macos")]
    fn processes(members: &HashMap<i32, usize>) -> Result<HashMap<i32, Entry>, RuntimeError> {
        let mut table = HashMap::new();
        for &member in members.keys() {
            for pid in children(member)?.into_iter().chain([member]) {
                let mut info = unsafe { std::mem::zeroed::<libc::proc_bsdshortinfo>() };
                let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
                let read = unsafe {
                    libc::proc_pidinfo(
                        pid,
                        libc::PROC_PIDT_SHORTBSDINFO,
                        0,
                        (&raw mut info).cast(),
                        size,
                    )
                };
                if read == size {
                    let frozen = matches!(info.pbsi_status, libc::SSTOP | libc::SZOMB);
                    let parent = info.pbsi_ppid as i32;
                    table.insert(pid, Entry { parent, frozen });
                }
            }
        }
        Ok(table)
    }

    #[cfg(target_os = "macos")]
    fn children(parent: i32) -> Result<Vec<i32>, RuntimeError> {
        let mut capacity = 64_usize;
        loop {
            let mut pids = vec![0_i32; capacity];
            let bytes = libc::c_int::try_from(capacity * size_of::<i32>())
                .map_err(|_| RuntimeError::Protocol("too many adapter descendants"))?;
            let listed =
                unsafe { libc::proc_listchildpids(parent, pids.as_mut_ptr().cast(), bytes) };
            let Ok(listed) = usize::try_from(listed) else {
                return Err(std::io::Error::last_os_error().into());
            };
            // A full buffer may be truncated; list again with more room.
            if listed < capacity {
                pids.truncate(listed);
                return Ok(pids);
            }
            capacity *= 2;
        }
    }
}
