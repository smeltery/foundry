//! Process-local signal delivery. Only the service decides how to persist a stop.

use std::{
    io,
    sync::{Arc, atomic::AtomicU8},
};

#[cfg(unix)]
use {
    signal_hook::{
        consts::signal::{SIGINT, SIGTERM},
        iterator::{Handle, Signals},
    },
    std::{
        sync::atomic::Ordering,
        thread::{self, JoinHandle},
    },
};

pub struct SignalControl {
    control: Arc<AtomicU8>,
    #[cfg(unix)]
    handle: Handle,
    #[cfg(unix)]
    worker: Option<JoinHandle<()>>,
}

impl SignalControl {
    /// First delivered interrupt requests graceful cancellation; subsequent
    /// interrupts request hard cancellation. Repeated OS signals can coalesce.
    pub fn install() -> io::Result<Self> {
        let control = Arc::new(AtomicU8::new(0));
        #[cfg(unix)]
        {
            let mut signals = Signals::new([SIGINT, SIGTERM])?;
            let handle = signals.handle();
            let shared = Arc::clone(&control);
            let worker = thread::Builder::new()
                .name("foundry-signals".into())
                .spawn(move || {
                    for _signal in signals.forever() {
                        let _ = shared.fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                            if value < 2 { Some(value + 1) } else { None }
                        });
                    }
                })?;
            Ok(Self {
                control,
                handle,
                worker: Some(worker),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = control;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "signal control requires Unix",
            ))
        }
    }

    pub fn control(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.control)
    }

    /// Wake the iterator and join the worker, including when no signal arrived.
    pub fn shutdown(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            self.handle.close();
            if let Some(worker) = self.worker.take() {
                worker
                    .join()
                    .map_err(|_| io::Error::other("signal worker panicked"))?;
            }
        }
        Ok(())
    }
}

impl Drop for SignalControl {
    fn drop(&mut self) {
        // Callers use shutdown() to observe errors; Drop still cleans up on early returns.
        let _ = self.shutdown();
    }
}
