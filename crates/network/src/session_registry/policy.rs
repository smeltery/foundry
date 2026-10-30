use super::{MAX_FOLDER_RULES, RegistryError, Result};
use std::path::{Path, PathBuf};

/// Canonical directory rules. The deepest matching rule wins; default is allow.
/// Re-evaluated on every visibility, association and delivery operation.
#[derive(Debug, Default)]
pub struct FolderPolicy {
    rules: Vec<(PathBuf, bool)>,
}
impl FolderPolicy {
    pub fn new(rules: Vec<(PathBuf, bool)>) -> Result<Self> {
        if rules.len() > MAX_FOLDER_RULES {
            return Err(RegistryError::Limit);
        }
        let mut canonical = Vec::new();
        for (path, allow) in rules {
            let path = directory(&path)?;
            if canonical.iter().any(|(existing, _)| existing == &path) {
                return Err(RegistryError::InvalidPath);
            }
            canonical.push((path, allow));
        }
        Ok(Self { rules: canonical })
    }
    pub(super) fn permits(&self, path: &Path) -> bool {
        let Ok(current) = directory(path) else {
            return false;
        };
        // A moved directory or changed symlink must not acquire the old identity.
        if current != path {
            return false;
        }
        self.rules
            .iter()
            .filter(|(root, _)| current.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
            .is_none_or(|(_, allow)| *allow)
    }
}
pub(super) fn directory(path: &Path) -> Result<PathBuf> {
    super::types::text(path.to_str().ok_or(RegistryError::InvalidPath)?)?;
    let canonical = path
        .canonicalize()
        .map_err(|_| RegistryError::InvalidPath)?;
    super::types::text(canonical.to_str().ok_or(RegistryError::InvalidPath)?)?;
    if !canonical.is_dir() {
        return Err(RegistryError::InvalidPath);
    }
    Ok(canonical)
}
