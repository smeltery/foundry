use super::*;
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    process::Command,
};

struct Fixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    destination: PathBuf,
    helper: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let helper = std::env::var_os("FOUNDRY_SNAPSHOT_TEST_HELPER")
            .expect("set FOUNDRY_SNAPSHOT_TEST_HELPER to the rebuilt fixed f5y-toolhost")
            .into();
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let destination = root.path().join("artifacts");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o700)).unwrap();
        // macOS temp paths can contain /var -> /private/var symlinks. The
        // production API deliberately requires non-symlink absolute components.
        let workspace = workspace.canonicalize().unwrap();
        let destination = destination.canonicalize().unwrap();
        Self {
            root,
            workspace,
            destination,
            helper,
        }
    }
    fn limits(&self) -> Limits {
        Limits {
            bytes: 1024 * 1024,
            entries: 1000,
            depth: 32,
            timeout_ms: 10_000,
        }
    }
    fn run(&self, limits: Limits) -> Result<SnapshotArtifact, SnapshotFailure> {
        attempt(
            &self.helper,
            &self.workspace,
            &self.destination,
            limits,
            || false,
        )
    }
    fn no_stage(&self) {
        assert!(fs::read_dir(&self.destination).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with('.')
        }));
    }
}

#[test]
#[ignore = "requires rebuilt fixed toolhost via FOUNDRY_SNAPSHOT_TEST_HELPER"]
fn real_tree_contains_uncommitted_untracked_and_unfollowed_symlink() {
    let fixture = Fixture::new();
    let git = |args: &[&str]| {
        assert!(
            Command::new("/usr/bin/git")
                .args(args)
                .current_dir(&fixture.workspace)
                .env_clear()
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap()
                .status
                .success()
        );
    };
    git(&["init", "--quiet"]);
    fs::write(fixture.workspace.join("tracked"), b"index contents").unwrap();
    git(&["add", "tracked"]);
    fs::write(fixture.workspace.join("tracked"), b"uncommitted edit").unwrap();
    fs::write(fixture.workspace.join("untracked"), b"untracked contents").unwrap();
    let outside = fixture.root.path().join("outside-private");
    fs::write(&outside, b"must not follow").unwrap();
    symlink(&outside, fixture.workspace.join("link")).unwrap();
    let artifact = fixture.run(fixture.limits()).unwrap();
    assert_eq!(
        fs::read(artifact.directory.join("tree/tracked")).unwrap(),
        b"uncommitted edit"
    );
    assert_eq!(
        fs::read(artifact.directory.join("tree/untracked")).unwrap(),
        b"untracked contents"
    );
    assert_eq!(
        fs::read_link(artifact.directory.join("tree/link")).unwrap(),
        outside
    );
    assert!(artifact.directory.join("tree/.git/index").is_file());
    let manifest = fs::read(artifact.directory.join("manifest.jsonl")).unwrap();
    assert_eq!(
        artifact.manifest_digest,
        <[u8; 32]>::from(Sha256::digest(&manifest))
    );
    let records: Vec<serde_json::Value> = manifest
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(artifact.entries as usize, records.len());
    let tracked = records
        .iter()
        .find(|record| record["path"] == serde_json::json!(b"tracked".to_vec()))
        .unwrap();
    assert_eq!(
        tracked["digest"],
        serde_json::json!(<[u8; 32]>::from(Sha256::digest(b"uncommitted edit")))
    );
    assert_eq!(
        fs::metadata(artifact.directory.join("tree/tracked"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    fixture.no_stage();
}

#[test]
#[ignore = "requires rebuilt fixed toolhost via FOUNDRY_SNAPSHOT_TEST_HELPER"]
fn failed_copy_keeps_prior_artifact_and_removes_partial_staging() {
    let fixture = Fixture::new();
    fs::write(fixture.workspace.join("file"), b"previous").unwrap();
    let prior = fixture.run(fixture.limits()).unwrap();
    let prior_manifest = fs::read(prior.directory.join("manifest.jsonl")).unwrap();
    fs::write(fixture.workspace.join("file"), vec![b'x'; 2 * 1024 * 1024]).unwrap();
    let mut limits = fixture.limits();
    // Permit the first streamed chunk, then fail with a real partial file.
    limits.bytes = 70 * 1024;
    assert!(matches!(fixture.run(limits), Err(SnapshotFailure::Limit)));
    assert_eq!(
        fs::read(prior.directory.join("tree/file")).unwrap(),
        b"previous"
    );
    assert_eq!(
        fs::read(prior.directory.join("manifest.jsonl")).unwrap(),
        prior_manifest
    );
    assert_eq!(fs::read_dir(&fixture.destination).unwrap().count(), 1);
    fixture.no_stage();
}

#[test]
#[ignore = "requires rebuilt fixed toolhost via FOUNDRY_SNAPSHOT_TEST_HELPER"]
fn hard_abort_after_copy_started_cleans_child_and_staging() {
    let fixture = Fixture::new();
    fs::File::create(fixture.workspace.join("large"))
        .unwrap()
        .set_len(512 * 1024 * 1024)
        .unwrap();
    let mut limits = fixture.limits();
    limits.bytes = 1024 * 1024 * 1024;
    let observed = std::cell::Cell::new(false);
    let result = attempt(
        &fixture.helper,
        &fixture.workspace,
        &fixture.destination,
        limits,
        || {
            let copying = fs::read_dir(&fixture.destination).unwrap().any(|entry| {
                let path = entry.unwrap().path().join("tree/large");
                fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0)
            });
            observed.set(observed.get() || copying);
            observed.get()
        },
    );
    assert!(observed.get());
    assert!(matches!(result, Err(SnapshotFailure::AbortedHard)));
    assert_eq!(fs::read_dir(&fixture.destination).unwrap().count(), 0);
    // Successful reuse verifies the cancelled child cannot keep modifying staging.
    fs::remove_file(fixture.workspace.join("large")).unwrap();
    fixture.run(fixture.limits()).unwrap();
    fixture.no_stage();
}

#[test]
#[ignore = "requires rebuilt fixed toolhost via FOUNDRY_SNAPSHOT_TEST_HELPER"]
fn elapsed_budget_cleans_partial_copy_and_hard_preflight_creates_nothing() {
    let fixture = Fixture::new();
    fs::File::create(fixture.workspace.join("large"))
        .unwrap()
        .set_len(512 * 1024 * 1024)
        .unwrap();
    let mut limits = fixture.limits();
    limits.bytes = 1024 * 1024 * 1024;
    limits.timeout_ms = 1;
    assert!(matches!(fixture.run(limits), Err(SnapshotFailure::Limit)));
    assert_eq!(fs::read_dir(&fixture.destination).unwrap().count(), 0);
    assert!(matches!(
        attempt(
            &fixture.helper,
            &fixture.workspace,
            &fixture.destination,
            fixture.limits(),
            || true
        ),
        Err(SnapshotFailure::AbortedHard)
    ));
    assert_eq!(fs::read_dir(&fixture.destination).unwrap().count(), 0);
}

#[test]
#[ignore = "requires rebuilt fixed toolhost via FOUNDRY_SNAPSHOT_TEST_HELPER"]
fn rejects_inside_destination_symlink_roots_special_files_and_bounds() {
    let fixture = Fixture::new();
    let inside = fixture.workspace.join("artifacts");
    fs::create_dir(&inside).unwrap();
    fs::set_permissions(&inside, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(
        attempt(
            &fixture.helper,
            &fixture.workspace,
            &inside,
            fixture.limits(),
            || false
        ),
        Err(SnapshotFailure::Invalid)
    ));
    fs::remove_dir(inside).unwrap();
    let alias = fixture.root.path().join("alias");
    symlink(&fixture.workspace, &alias).unwrap();
    assert!(
        attempt(
            &fixture.helper,
            &alias,
            &fixture.destination,
            fixture.limits(),
            || false
        )
        .is_err()
    );
    let socket = std::os::unix::net::UnixListener::bind(fixture.workspace.join("socket")).unwrap();
    assert!(matches!(
        fixture.run(fixture.limits()),
        Err(SnapshotFailure::Unsupported)
    ));
    drop(socket);
    fs::remove_file(fixture.workspace.join("socket")).unwrap();
    fs::create_dir_all(fixture.workspace.join("one/two")).unwrap();
    let mut limits = fixture.limits();
    limits.depth = 1;
    assert!(matches!(fixture.run(limits), Err(SnapshotFailure::Limit)));
    limits = fixture.limits();
    limits.entries = 1;
    assert!(matches!(fixture.run(limits), Err(SnapshotFailure::Limit)));
    assert_eq!(fs::read_dir(&fixture.destination).unwrap().count(), 0);
}
