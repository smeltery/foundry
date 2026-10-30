use super::ConfigError;
use std::{io::Read, path::Path};

/// Public caller-selected inputs may be symlinked or shared. Bind the read to one
/// opened regular file and enforce the limit even if it grows after metadata.
pub(super) fn read(base: &Path, path: &Path, limit: u64) -> Result<Vec<u8>, ConfigError> {
    if !base.is_absolute() || path.as_os_str().is_empty() {
        return Err(ConfigError::Invalid);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(base.join(path))
            .map_err(|_| ConfigError::Storage)?;
        let metadata = file.metadata().map_err(|_| ConfigError::Storage)?;
        if !metadata.is_file() {
            return Err(ConfigError::UnsafeFile);
        }
        if metadata.len() > limit {
            return Err(ConfigError::Invalid);
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ConfigError::Storage)?;
        if bytes.is_empty() || bytes.len() as u64 > limit {
            return Err(ConfigError::Invalid);
        }
        Ok(bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = (base, path, limit);
        Err(ConfigError::Unsupported)
    }
}
