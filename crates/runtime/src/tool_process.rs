//! Trusted one-call helper supervision. Does not enable adapter tool dispatch.
//! Poll never joins a live worker. Drop stops and joins; OS scheduling, spawn and
//! uninterruptible kernel I/O/reaping prevent an unconditional cleanup deadline.
//! Requires exclusive ownership of child reaping: no process-wide SIGCHLD auto-reap
//! or external wait/reaper for these children. ECHILD fails closed without further
//! signals, but an external reaper racing between observation and signal cannot
//! be made safe by a portable PID-based API.
mod child;
mod wire;
use crate::pipe_workers::{self, POLL_INTERVAL, Pipe, Workers};
use cap_std::fs::Dir;
use foundry_tools::host::{Reply, Request};
use std::{
    io::{Read, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub struct ToolProcessSpec {
    pub helper: PathBuf,
    pub directory: Dir,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ToolProcessError {
    #[error("invalid tool process configuration")]
    Invalid,
    #[error("tool process permission denied")]
    Denied,
    #[error("tool process I/O failed")]
    Io,
    #[error("tool process protocol failed")]
    Protocol,
    #[error("tool process timed out")]
    Timeout,
    #[error("tool process stopped")]
    Stopped,
    #[error("tool process exited unsuccessfully")]
    Exit,
    #[error("tool process cleanup failed")]
    Cleanup,
    #[error("tool process worker failed")]
    Worker,
    #[error("tool processes unsupported on this platform")]
    Unsupported,
}
type Result<T> = std::result::Result<T, ToolProcessError>;

pub enum ToolPoll {
    Pending,
    /// One terminal result, emitted only after owned cleanup. Errors after dispatch
    /// may follow filesystem effects. The caller must commit its durable receipt.
    Finished(Result<Reply>),
    /// The single result has already been consumed. This handle cannot restart.
    Consumed,
}

pub struct ToolProcess {
    stop: Arc<AtomicBool>,
    caller_stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<Reply>>>,
}
impl ToolProcess {
    /// Trusted configuration only. No root/policy/helper fields from adapter output.
    /// The shared stop flag is monotonic: callers must never reset it to false.
    pub fn start(
        spec: ToolProcessSpec,
        request: Request,
        caller_stop: Arc<AtomicBool>,
    ) -> Result<Self> {
        if !spec.helper.is_absolute() || spec.timeout.is_zero() {
            return Err(ToolProcessError::Invalid);
        }
        if caller_stop.load(Ordering::Acquire) {
            return Err(ToolProcessError::Stopped);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let owned_stop = Arc::clone(&stop);
        let external_stop = Arc::clone(&caller_stop);
        let worker = thread::Builder::new()
            .name("tool-lifecycle".into())
            .spawn(move || run(spec, request, owned_stop, external_stop))
            .map_err(|_| ToolProcessError::Worker)?;
        Ok(Self {
            stop,
            caller_stop,
            worker: Some(worker),
        })
    }
    pub fn request_stop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
    pub fn poll(&mut self) -> ToolPoll {
        let Some(worker) = self.worker.as_ref() else {
            return ToolPoll::Consumed;
        };
        if !worker.is_finished() {
            return ToolPoll::Pending;
        }
        let result = self
            .worker
            .take()
            .expect("finished worker exists")
            .join()
            .unwrap_or(Err(ToolProcessError::Worker));
        ToolPoll::Finished(prioritize_cleanup(
            result,
            cancelled(&self.stop, &self.caller_stop),
        ))
    }
}
impl Drop for ToolProcess {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn cancelled(local: &AtomicBool, caller: &AtomicBool) -> bool {
    local.load(Ordering::Acquire) || caller.load(Ordering::Acquire)
}

enum Event {
    Input(Result<()>),
    Output(Result<Vec<u8>>),
    Stderr(Result<()>),
}

fn run(
    spec: ToolProcessSpec,
    request: Request,
    stop: Arc<AtomicBool>,
    caller: Arc<AtomicBool>,
) -> Result<Reply> {
    if cancelled(&stop, &caller) {
        return Err(ToolProcessError::Stopped);
    }
    let deadline = Instant::now()
        .checked_add(spec.timeout)
        .ok_or(ToolProcessError::Invalid)?;
    let bytes = wire::prepare(&request)?;
    if cancelled(&stop, &caller) {
        return Err(ToolProcessError::Stopped);
    }
    let mut child = child::OwnedChild::spawn(&spec, &stop, &caller)?;
    let mut workers = Workers::default();
    let result = (|| {
        if cancelled(&stop, &caller) {
            return Err(ToolProcessError::Stopped);
        }
        let stdin = child.process.stdin.take().ok_or(ToolProcessError::Io)?;
        let stdout = child.process.stdout.take().ok_or(ToolProcessError::Io)?;
        let stderr = child.process.stderr.take().ok_or(ToolProcessError::Io)?;
        pipe_workers::nonblocking(&stdin).map_err(|_| ToolProcessError::Io)?;
        pipe_workers::nonblocking(&stdout).map_err(|_| ToolProcessError::Io)?;
        pipe_workers::nonblocking(&stderr).map_err(|_| ToolProcessError::Io)?;
        let (sender, events) = mpsc::sync_channel(3);
        let input_events = sender.clone();
        let input_stop = Arc::clone(&stop);
        let external = Arc::clone(&caller);
        workers
            .spawn("tool-input", move |worker_stop| {
                let mut pipe = Pipe::new(stdin, Arc::clone(&worker_stop));
                let mut result = Ok(());
                // Check caller stop between bounded writes. An in-flight syscall or
                // previously dispatched operation cannot be rolled back by cancellation.
                for chunk in bytes.chunks(4096) {
                    if cancelled(&input_stop, &external) {
                        result = Err(ToolProcessError::Stopped);
                        break;
                    }
                    if pipe.write_all(chunk).is_err() {
                        result = Err(ToolProcessError::Io);
                        break;
                    }
                }
                drop(pipe); // EOF permits the helper to execute exactly once.
                pipe_workers::send(&input_events, Event::Input(result), &worker_stop);
            })
            .map_err(|_| ToolProcessError::Worker)?;
        let output_events = sender.clone();
        workers
            .spawn("tool-output", move |worker_stop| {
                let mut pipe = Pipe::new(stdout, Arc::clone(&worker_stop));
                let result = wire::read_reply(&mut pipe);
                pipe_workers::send(&output_events, Event::Output(result), &worker_stop);
            })
            .map_err(|_| ToolProcessError::Worker)?;
        workers
            .spawn("tool-stderr", move |worker_stop| {
                let mut pipe = Pipe::new(stderr, Arc::clone(&worker_stop));
                let mut buffer = [0; 8192];
                let result = loop {
                    match pipe.read(&mut buffer) {
                        Ok(0) => break Ok(()),
                        Ok(_) => {}
                        Err(_) => break Err(ToolProcessError::Io),
                    }
                };
                pipe_workers::send(&sender, Event::Stderr(result), &worker_stop);
            })
            .map_err(|_| ToolProcessError::Worker)?;
        monitor(&mut child, &events, &request, deadline, &stop, &caller)
    })();
    workers.request_stop();
    let child_result = child.cleanup();
    let worker_result = workers.join().map_err(|_| ToolProcessError::Worker);
    child_result?;
    worker_result?;
    prioritize_cleanup(result, cancelled(&stop, &caller))
}

fn monitor(
    child: &mut child::OwnedChild,
    events: &Receiver<Event>,
    request: &Request,
    deadline: Instant,
    stop: &AtomicBool,
    caller: &AtomicBool,
) -> Result<Reply> {
    let mut input_done = false;
    let mut reply = None;
    let mut clean_exit = false;
    loop {
        if cancelled(stop, caller) {
            return Err(ToolProcessError::Stopped);
        }
        if Instant::now() >= deadline {
            return Err(ToolProcessError::Timeout);
        }
        // Observation preserves the PID reservation until cleanup signals the group.
        if !clean_exit && child.exited()? {
            if !child.cleanup()?.success() {
                return Err(ToolProcessError::Exit);
            }
            clean_exit = true;
        }
        match events.try_recv() {
            Ok(Event::Input(result)) => {
                result?;
                input_done = true;
            }
            Ok(Event::Output(result)) => {
                reply = Some(wire::validate(&result?, request)?);
            }
            Ok(Event::Stderr(result)) => {
                result?;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) if reply.is_none() || !input_done => {
                return Err(ToolProcessError::Worker);
            }
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
        if clean_exit && input_done && reply.is_some() {
            return Ok(reply.take().expect("validated reply"));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn prioritize_cleanup(result: Result<Reply>, stopped: bool) -> Result<Reply> {
    match result {
        Err(error @ (ToolProcessError::Cleanup | ToolProcessError::Worker)) => Err(error),
        _ if stopped => Err(ToolProcessError::Stopped),
        result => result,
    }
}

#[cfg(test)]
#[path = "tool_process/tests.rs"]
mod tests;
