use cap_std::fs::Dir;
use foundry_contracts::CreateSession;
use foundry_runtime::{AdapterSpec, coordinated::TrustedToolPolicy};
use foundry_service::{Service, ServiceOptions};
use foundry_tools::host::{Grant, HostLimits};
use std::{
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

pub struct Fixture {
    pub directory: TempDir,
    pub database: PathBuf,
    pub helper: PathBuf,
    pub session: String,
    pub service: Service,
}
impl Fixture {
    pub fn new(mode: &str, options: ServiceOptions) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("db");
        let mut service = Service::open_with_options(&database, options).unwrap();
        let session = service
            .create(CreateSession {
                prompt: "offline tool coordination".into(),
                directory: directory.path().display().to_string(),
                model: None,
                idempotency_key: None,
            })
            .unwrap()
            .id;
        let helper = directory.path().join("helper");
        let code = include_str!("tool_helper.py")
            .replace("FIXTURE_MODE", &format!("{mode:?}"))
            .replace(
                "FIXTURE_ROOT",
                &serde_json::to_string(&directory.path().display().to_string()).unwrap(),
            );
        std::fs::write(&helper, format!("#!{}\n{}", python(), code)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self {
            directory,
            database,
            helper,
            session,
            service,
        }
    }
    pub fn policy(&self, grants: Vec<Grant>) -> TrustedToolPolicy {
        policy(&self.helper, self.directory.path(), grants)
    }
    pub fn adapter(&self, mode: &str) -> AdapterSpec {
        let mut spec = AdapterSpec::new(python());
        spec.args = vec![
            "-u".into(),
            "-c".into(),
            include_str!("tool_adapter.py").into(),
            self.database.as_os_str().into(),
            self.directory.path().as_os_str().into(),
            mode.into(),
        ];
        spec.run_timeout = Duration::from_secs(5);
        spec.cancel_timeout = Duration::from_millis(50);
        spec
    }
}
pub fn policy(helper: &Path, root: &Path, grants: Vec<Grant>) -> TrustedToolPolicy {
    TrustedToolPolicy::new(
        helper.to_path_buf(),
        Dir::open_ambient_dir(root, cap_std::ambient_authority()).unwrap(),
        grants,
        HostLimits {
            request_bytes: 524288,
            read_bytes: 262144,
            write_bytes: 262144,
            output_bytes: 1048576,
            entries: 256,
        },
        Duration::from_secs(3),
    )
    .unwrap()
}
pub fn python() -> String {
    let output = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().into()
}
pub fn wait(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(Instant::now() < deadline, "fixture barrier timed out");
        thread::sleep(Duration::from_millis(5));
    }
}
pub fn alive(pid: &Path) -> bool {
    let pid = std::fs::read_to_string(pid).unwrap();
    Command::new("kill")
        .args(["-0", pid.trim()])
        .output()
        .unwrap()
        .status
        .success()
}
