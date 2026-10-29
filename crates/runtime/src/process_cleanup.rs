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
