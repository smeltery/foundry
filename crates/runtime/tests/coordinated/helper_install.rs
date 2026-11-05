use std::{collections::HashSet, hash::Hash, hash::Hasher, os::unix::fs::PermissionsExt};
use std::{path::Path, path::PathBuf, sync::Mutex, sync::OnceLock};

/// Helpers this process installed; the lock keeps test threads from staging duplicates.
static INSTALLED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

/// Installs the tool helper once per content in `directory` and reuses it, since macOS
/// endpoint security scans each new executable on first exec and can delay it past test
/// deadlines. Fixtures share the file and name its mode in their own `helper-mode`.
pub fn installed_helper(directory: &Path, python: &Path) -> PathBuf {
    let source = format!("#!{}\n{}", python.display(), include_str!("helper.py"));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    let path = directory.join(format!("coordinated-helper-{:016x}", hasher.finish()));
    let mut installed = INSTALLED.get_or_init(Default::default).lock().unwrap();
    if installed.contains(&path) || path.exists() {
        installed.insert(path.clone());
        return path;
    }
    // Other test processes may install the same helper; the rename is atomic, so no
    // process can execute a partially written file.
    let staged = directory.join(format!("coordinated-helper-{}", uuid::Uuid::new_v4()));
    let mode = std::fs::Permissions::from_mode(0o700);
    let staging = std::fs::write(&staged, source)
        .and_then(|()| std::fs::set_permissions(&staged, mode))
        .and_then(|()| std::fs::rename(&staged, &path));
    if let Err(error) = staging {
        let _ = std::fs::remove_file(&staged);
        panic!("cannot install {}: {error}", path.display());
    }
    installed.insert(path.clone());
    path
}
