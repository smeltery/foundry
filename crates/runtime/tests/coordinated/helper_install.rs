use std::{hash::Hash, hash::Hasher, os::unix::fs::PermissionsExt, path::Path, path::PathBuf};

/// Installs the tool helper once per content in `directory` and reuses it. macOS
/// endpoint security scans each newly written executable on first exec, which can delay
/// a fresh per-test helper past its deadline; a reused file is scanned only once. Tests
/// share the file, so each fixture names the helper's mode in its own `helper-mode`.
pub fn installed_helper(directory: &Path, python: &Path) -> PathBuf {
    let source = format!("#!{}\n{}", python.display(), include_str!("helper.py"));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    let path = directory.join(format!("coordinated-helper-{:016x}", hasher.finish()));
    if path.exists() {
        return path;
    }
    // Concurrent test processes may install the same helper; the rename is atomic, so
    // no process can execute a partially written file.
    let staged = directory.join(format!("coordinated-helper-{}", uuid::Uuid::new_v4()));
    std::fs::write(&staged, source).unwrap();
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::rename(&staged, &path).unwrap();
    path
}
