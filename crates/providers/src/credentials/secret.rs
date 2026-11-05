//! Write-only secret values and the `--token-stdin` reader.
use super::CredentialError;
use std::fmt;
use std::io::{self, BufRead, IsTerminal, Read};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

/// Largest accepted API key or token, in bytes.
pub const MAX_SECRET_BYTES: usize = 8192;

/// An API key or token. It has no `Display`, its `Debug` is redacted, its
/// bytes are zeroed on drop, and equality is constant-time for values of equal
/// length (only the length can be observed).
#[derive(Clone)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    /// Accepts 1 to [`MAX_SECRET_BYTES`] bytes.
    pub fn new(value: String) -> Result<Self, CredentialError> {
        let value = Zeroizing::new(value);
        if value.is_empty() || value.len() > MAX_SECRET_BYTES {
            return Err(CredentialError::invalid("secret"));
        }
        Ok(Self(value))
    }

    /// The value, for a validator or the private-input handoff only.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Moves the value out for a consumer that takes a plain `String`; that
    /// copy is no longer zeroed on drop.
    pub(crate) fn into_inner(mut self) -> String {
        std::mem::take(&mut *self.0)
    }

    /// Rebuilds a stored value. Invalid bytes are zeroed before the error.
    pub(crate) fn from_stored(bytes: Vec<u8>) -> Result<Self, CredentialError> {
        let mut bytes = Zeroizing::new(bytes);
        let corrupt = || CredentialError::unavailable("malformed stored secret");
        if std::str::from_utf8(&bytes).is_err() {
            return Err(corrupt());
        }
        let text = String::from_utf8(std::mem::take(&mut *bytes)).map_err(|_| corrupt())?;
        Self::new(text).map_err(|_| corrupt())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(redacted)")
    }
}

impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_bytes().ct_eq(other.0.as_bytes()).into()
    }
}

impl Eq for Secret {}

/// Reads one token line. Only the line ending (LF or CRLF) is removed. Empty,
/// oversize or non-UTF-8 input is `invalid_request`, and reading stops one
/// line ending past the limit, so an endless stream is never buffered.
///
/// The line buffer is zeroed, and sized up front so it never reallocates and
/// leaves copies behind. The reader's own buffer (such as the one inside
/// `BufReader` or `Stdin`) is outside this function's control and keeps the
/// bytes until they are overwritten.
pub fn read_token(input: &mut impl BufRead) -> Result<Secret, CredentialError> {
    let mut line = Zeroizing::new(Vec::with_capacity(MAX_SECRET_BYTES + 2));
    let limit = (MAX_SECRET_BYTES + 2) as u64;
    Read::take(&mut *input, limit)
        .read_until(b'\n', &mut line)
        .map_err(|_| CredentialError::invalid("token_input"))?;
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    } else if line.len() as u64 == limit {
        return Err(CredentialError::invalid("secret"));
    }
    let text = std::str::from_utf8(&line).map_err(|_| CredentialError::invalid("secret"))?;
    Secret::new(text.to_owned())
}

/// Reads one token from standard input. On a terminal, echo is off for the
/// duration of the read; where echo cannot be controlled, a terminal is
/// refused rather than read with echo.
pub fn read_token_stdin() -> Result<Secret, CredentialError> {
    let stdin = io::stdin();
    #[cfg(unix)]
    let _quiet = {
        use std::os::fd::AsFd;
        EchoGuard::disable(stdin.as_fd()).map_err(|_| CredentialError::invalid("token_input"))?
    };
    #[cfg(not(unix))]
    if stdin.is_terminal() {
        return Err(CredentialError::invalid("token_input"));
    }
    read_token(&mut stdin.lock())
}

/// Terminal echo switched off on one descriptor until the guard drops, which
/// restores the saved settings. The newline still echoes so the prompt line
/// ends.
#[cfg(unix)]
pub struct EchoGuard<'fd> {
    fd: std::os::fd::BorrowedFd<'fd>,
    saved: libc::termios,
}

#[cfg(unix)]
impl<'fd> EchoGuard<'fd> {
    /// Returns `None` when `fd` is not a terminal, such as a pipe or file,
    /// since nothing echoes there.
    pub fn disable(fd: std::os::fd::BorrowedFd<'fd>) -> io::Result<Option<Self>> {
        use std::os::fd::AsRawFd;
        if !fd.is_terminal() {
            return Ok(None);
        }
        let mut saved = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: `fd` is a live borrowed descriptor and `saved` is a valid
        // out-pointer that tcgetattr fully initializes on success.
        if unsafe { libc::tcgetattr(fd.as_raw_fd(), saved.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: tcgetattr returned 0, so `saved` is initialized.
        let saved = unsafe { saved.assume_init() };
        let mut quiet = saved;
        quiet.c_lflag &= !(libc::ECHO | libc::ECHOE | libc::ECHOK);
        quiet.c_lflag |= libc::ECHONL;
        // SAFETY: `fd` is live and `quiet` is a valid termios value.
        if unsafe { libc::tcsetattr(fd.as_raw_fd(), libc::TCSANOW, &quiet) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(Self { fd, saved }))
    }
}

#[cfg(unix)]
impl Drop for EchoGuard<'_> {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // SAFETY: the borrowed descriptor outlives the guard and `saved` came
        // from tcgetattr. A failed restore has no caller to report to.
        unsafe { libc::tcsetattr(self.fd.as_raw_fd(), libc::TCSANOW, &self.saved) };
    }
}
