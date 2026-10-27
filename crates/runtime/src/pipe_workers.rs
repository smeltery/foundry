//! Owned, cooperatively stopped pipe workers. Only parent pipe ends are changed.
use crate::RuntimeError;
use std::{
    io::{self, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Default)]
pub(crate) struct Workers {
    stop: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
}

impl Workers {
    pub fn spawn(
        &mut self,
        name: &str,
        work: impl FnOnce(Arc<AtomicBool>) + Send + 'static,
    ) -> io::Result<()> {
        let stop = Arc::clone(&self.stop);
        self.handles.push(
            thread::Builder::new()
                .name(name.into())
                .spawn(move || work(stop))?,
        );
        Ok(())
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    pub fn join(&mut self) -> Result<(), RuntimeError> {
        self.request_stop();
        let mut panicked = false;
        for handle in self.handles.drain(..) {
            panicked |= handle.join().is_err();
        }
        if panicked {
            Err(RuntimeError::Protocol("adapter pipe worker panicked"))
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    pub fn remaining(&self) -> usize {
        self.handles.len()
    }
}

impl Drop for Workers {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

pub(crate) fn stopped(stop: &AtomicBool) -> bool {
    stop.load(Ordering::Acquire)
}

pub(crate) fn send<T>(sender: &SyncSender<T>, mut value: T, stop: &AtomicBool) -> bool {
    while !stopped(stop) {
        match sender.try_send(value) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(unsent)) => value = unsent,
        }
        thread::sleep(POLL_INTERVAL);
    }
    false
}

pub(crate) struct Pipe<T> {
    inner: T,
    stop: Arc<AtomicBool>,
}

impl<T> Pipe<T> {
    pub fn new(inner: T, stop: Arc<AtomicBool>) -> Self {
        Self { inner, stop }
    }

    fn retry<R>(&mut self, mut operation: impl FnMut(&mut T) -> io::Result<R>) -> io::Result<R> {
        loop {
            if stopped(&self.stop) {
                return Err(io::Error::from(io::ErrorKind::ConnectionAborted));
            }
            match operation(&mut self.inner) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(POLL_INTERVAL)
                }
                result => return result,
            }
        }
    }
}
impl<T: Read> Read for Pipe<T> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.retry(|inner| inner.read(buffer))
    }
}
impl<T: Write> Write for Pipe<T> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        // write_all retains partial offsets and diagnoses a zero write as WriteZero.
        self.retry(|inner| inner.write(buffer))
    }
    fn flush(&mut self) -> io::Result<()> {
        self.retry(Write::flush)
    }
}

#[cfg(unix)]
pub(crate) fn nonblocking(pipe: &impl std::os::fd::AsRawFd) -> io::Result<()> {
    let fd = pipe.as_raw_fd();
    let flags = loop {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags >= 0 {
            break flags;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    };
    loop {
        if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(not(unix))]
pub(crate) fn nonblocking<T>(_: &T) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;

    #[test]
    fn full_output_queue_stops_and_all_handles_are_joined() {
        let (sender, receiver) = sync_channel(1);
        sender.send("original").unwrap();
        let mut workers = Workers::default();
        workers
            .spawn("full-output", move |stop| {
                assert!(!send(&sender, "pending", &stop));
            })
            .unwrap();
        workers.join().unwrap();
        assert_eq!(workers.remaining(), 0);
        assert_eq!(receiver.recv().unwrap(), "original");
    }

    #[test]
    fn retry_keeps_the_exact_unsent_value() {
        let (sender, receiver) = sync_channel(1);
        sender.send(vec![1]).unwrap();
        let mut workers = Workers::default();
        workers
            .spawn("retry-output", move |stop| {
                assert!(send(&sender, vec![2, 3, 4], &stop));
            })
            .unwrap();
        assert_eq!(receiver.recv().unwrap(), vec![1]);
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
            vec![2, 3, 4]
        );
        workers.join().unwrap();
        assert_eq!(workers.remaining(), 0);
    }

    #[test]
    fn partial_setup_failure_drops_and_joins_started_workers() {
        let exited = Arc::new(AtomicBool::new(false));
        let marker = Arc::clone(&exited);
        let setup = || -> io::Result<()> {
            let mut workers = Workers::default();
            workers.spawn("partial-setup", move |stop| {
                while !stopped(&stop) {
                    thread::sleep(POLL_INTERVAL);
                }
                marker.store(true, Ordering::Release);
            })?;
            // Model a later thread-spawn/setup failure without exhausting OS resources.
            Err(io::Error::from(io::ErrorKind::ResourceBusy))
        };
        assert!(setup().is_err());
        assert!(exited.load(Ordering::Acquire));
    }

    #[test]
    fn panic_does_not_skip_other_joins() {
        let mut workers = Workers::default();
        workers
            .spawn("panicking-worker", |_| panic!("fixture"))
            .unwrap();
        workers
            .spawn("other-worker", |stop| {
                while !stopped(&stop) {
                    thread::sleep(POLL_INTERVAL);
                }
            })
            .unwrap();
        assert!(matches!(workers.join(), Err(RuntimeError::Protocol(_))));
        assert_eq!(workers.remaining(), 0);
    }

    #[test]
    fn interrupted_and_zero_writes_keep_standard_io_semantics() {
        struct Writer(bool);
        impl Write for Writer {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                if !self.0 {
                    self.0 = true;
                    Err(io::Error::from(io::ErrorKind::Interrupted))
                } else {
                    Ok(0)
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut pipe = Pipe::new(Writer(false), Arc::new(AtomicBool::new(false)));
        assert_eq!(
            pipe.write_all(b"pending").unwrap_err().kind(),
            io::ErrorKind::WriteZero
        );
    }
}
