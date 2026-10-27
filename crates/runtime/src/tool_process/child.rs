use super::{Result, ToolProcessError as Error, ToolProcessSpec};
use std::{
    process::{Child, ExitStatus},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(super) struct OwnedChild {
    pub process: Child,
    status: Option<ExitStatus>,
    ownership_lost: bool,
    #[cfg(test)]
    pub(super) signals_sent: usize,
}
impl OwnedChild {
    #[cfg(unix)]
    pub fn spawn(
        spec: &ToolProcessSpec,
        stop: &Arc<AtomicBool>,
        caller: &Arc<AtomicBool>,
    ) -> Result<Self> {
        use std::{
            os::{
                fd::AsRawFd,
                unix::{fs::PermissionsExt, process::CommandExt},
            },
            process::{Command, Stdio},
        };
        let helper = spec.helper.canonicalize().map_err(|_| Error::Io)?;
        let metadata = helper.metadata().map_err(|_| Error::Io)?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(Error::Invalid);
        }
        if !spec
            .directory
            .dir_metadata()
            .map_err(|_| Error::Io)?
            .is_dir()
        {
            return Err(Error::Invalid);
        }
        let fd = spec.directory.as_raw_fd();
        // Include extant descriptors above a lowered soft limit. The parent must
        // not race descriptor-limit changes against this trusted spawn operation.
        let limit = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
        if !(0..=1_048_576).contains(&limit) {
            return Err(Error::Invalid);
        }
        let highest = std::fs::read_dir("/dev/fd")
            .map_err(|_| Error::Io)?
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<i32>().ok())
            .max()
            .unwrap_or(3);
        let limit = (limit as i32)
            .max(highest.saturating_add(1))
            .max(fd.saturating_add(1));
        if super::cancelled(stop, caller) {
            return Err(Error::Stopped);
        }
        let child_stop = Arc::clone(stop);
        let child_caller = Arc::clone(caller);
        let mut command = Command::new(helper);
        command
            .env_clear()
            .current_dir("/")
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(move || {
                if child_stop.load(Ordering::Acquire) || child_caller.load(Ordering::Acquire) {
                    return Err(std::io::Error::from_raw_os_error(libc::ECANCELED));
                }
                if fd != 3 && libc::dup2(fd, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // Mark rather than close: preserve Rust's exec-error pipe until exec.
                for extra in 4..limit {
                    if libc::fcntl(extra, libc::F_SETFD, libc::FD_CLOEXEC) < 0 {
                        let error = std::io::Error::last_os_error();
                        if error.raw_os_error() != Some(libc::EBADF) {
                            return Err(error);
                        }
                    }
                }
                Ok(())
            });
        }
        let process = command.spawn().map_err(|_| Error::Io)?;
        Ok(Self {
            process,
            status: None,
            ownership_lost: false,
            #[cfg(test)]
            signals_sent: 0,
        })
    }
    #[cfg(not(unix))]
    pub fn spawn(_: &ToolProcessSpec, _: &Arc<AtomicBool>, _: &Arc<AtomicBool>) -> Result<Self> {
        Err(Error::Unsupported)
    }

    #[cfg(unix)]
    pub fn exited(&mut self) -> Result<bool> {
        if self.ownership_lost {
            return Err(Error::Cleanup);
        }
        if self.status.is_some() {
            return Ok(true);
        }
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.process.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                self.ownership_lost = true;
            }
            if error.kind() == std::io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(Error::Cleanup);
        }
        Ok(unsafe { info.si_pid() } == self.process.id() as i32)
    }
    #[cfg(not(unix))]
    pub fn exited(&mut self) -> Result<bool> {
        Err(Error::Unsupported)
    }

    pub fn cleanup(&mut self) -> Result<ExitStatus> {
        if self.ownership_lost {
            return Err(Error::Cleanup);
        }
        if let Some(status) = self.status {
            return Ok(status);
        }
        #[cfg(unix)]
        {
            // Cancellation may enter cleanup before the monitor has observed exit.
            // ECHILD means the PID reservation is lost: do not signal or wait again.
            self.exited()?;
            let group = -(self.process.id() as i32);
            let mut failure = None;
            loop {
                #[cfg(test)]
                {
                    self.signals_sent += 1;
                }
                if unsafe { libc::kill(group, libc::SIGKILL) } == 0 {
                    break;
                }
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                if error.raw_os_error() != Some(libc::ESRCH) {
                    failure = Some(error);
                }
                break;
            }
            if failure.is_some() {
                let _ = self.process.kill();
            }
            // No nonzero signals after wait releases the direct child's PID.
            let status = self.process.wait().map_err(|error| {
                if error.raw_os_error() == Some(libc::ECHILD) {
                    self.ownership_lost = true;
                }
                Error::Cleanup
            })?;
            self.status = Some(status);
            if let Some(error) = failure {
                let absent = unsafe { libc::kill(group, 0) } < 0
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
                if error.raw_os_error() != Some(libc::EPERM) || !absent {
                    return Err(Error::Cleanup);
                }
            }
            Ok(status)
        }
        #[cfg(not(unix))]
        {
            Err(Error::Unsupported)
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}
