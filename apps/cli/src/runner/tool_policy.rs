//! Trusted CLI policy construction; adapter JSON never selects helper/root/grants.
//! FOUNDRY_TOOLHOST is an explicit development override, not verified release
//! provenance. It must name an absolute existing file. Normal installs use the
//! sibling f5y-toolhost, never PATH. The helper IPC version gates compatibility;
//! paired release packaging, not this module, verifies matching release versions.
use super::{CommandError, invalid};
use cap_std::fs::Dir;
use foundry_runtime::coordinated::TrustedToolPolicy;
use foundry_tools::host::{self, Grant, HostLimits};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

const TOOL_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) fn grants(value: &Value) -> Result<Vec<Grant>, CommandError> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    let values = value
        .as_array()
        .ok_or_else(|| invalid("tool grants must be an array"))?;
    let mut grants = Vec::new();
    for value in values {
        let grant: Grant = serde_json::from_value(value.clone())
            .map_err(|_| invalid("unknown workspace tool grant"))?;
        if !grants.contains(&grant) {
            grants.push(grant);
        }
    }
    Ok(grants)
}

pub(super) fn build(
    directory: &str,
    grants: Vec<Grant>,
    helper_override: Option<&Path>,
) -> Result<Option<TrustedToolPolicy>, CommandError> {
    if grants.is_empty() {
        return Ok(None);
    }
    let helper = match helper_override {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(_) => {
            return Err(invalid(
                "FOUNDRY_TOOLHOST must be an absolute executable path",
            ));
        }
        None => {
            let executable = std::env::current_exe()
                .map_err(|_| invalid("CLI executable path is unavailable"))?;
            executable
                .parent()
                .ok_or_else(|| invalid("CLI installation directory is unavailable"))?
                .join("f5y-toolhost")
        }
    };
    let helper: PathBuf = helper
        .canonicalize()
        .map_err(|_| invalid("workspace tool helper is unavailable"))?;
    if !helper.is_file() {
        return Err(invalid("workspace tool helper must be a file"));
    }
    let directory = Dir::open_ambient_dir(directory, cap_std::ambient_authority())
        .map_err(|_| invalid("workspace tool directory is unavailable"))?;
    let limits = HostLimits {
        request_bytes: host::MAX_REQUEST_BYTES,
        read_bytes: 256 * 1024,
        write_bytes: 256 * 1024,
        output_bytes: host::MAX_REPLY_BYTES,
        entries: 256,
    };
    TrustedToolPolicy::new(helper, directory, grants, limits, TOOL_TIMEOUT)
        .map(Some)
        .map_err(|_| invalid("invalid workspace tool policy"))
}
