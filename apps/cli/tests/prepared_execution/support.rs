use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
};

pub struct Fixture {
    pub root: tempfile::TempDir,
    pub work: PathBuf,
    pub adapter: PathBuf,
    pub config: PathBuf,
    pub database: PathBuf,
}
impl Fixture {
    pub fn new(mode: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().canonicalize().unwrap().join("work");
        std::fs::create_dir(&work).unwrap();
        let agent = root.path().join("agent");
        std::fs::create_dir(&agent).unwrap();
        let config = root.path().join("private.json");
        let adapter = root.path().join("adapter");
        let python = Command::new("python3")
            .args(["-c", "import sys;print(sys.executable)"])
            .output()
            .unwrap();
        assert!(python.status.success());
        std::fs::write(
            &adapter,
            format!(
                "#!{}\nmode={mode:?}\n{}",
                String::from_utf8(python.stdout).unwrap().trim(),
                include_str!("adapter.py")
            ),
        )
        .unwrap();
        std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = Self {
            database: root.path().join("store.db"),
            root,
            work,
            adapter,
            config,
        };
        fixture.save(json!({"version":1,"selection":{
            "provider":"fixture-provider","model":"file-model","effort":"medium",
            "directory":fixture.work,"workspace_identity":"11".repeat(32),
            "profile":"explicit-core-v1","profile_version":1,"permission_profile":"current",
            "resource_policy":"explicit_system_v1","resource_identity":"22".repeat(32),
            "tool_catalog":"core_workspace_v1","tools":["read","write"],
            "policy_identity":"33".repeat(32),"generation":1,"max_output_bytes":4096,"max_pending_text_bytes":4096
        },"private":{"credential":{"kind":"api_key","provider":"fixture-provider","value":"private-key-canary"},
            "system_prompt":"private-system-canary","agent_directory":agent}}));
        fixture
    }
    pub fn save(&self, value: Value) {
        std::fs::write(&self.config, serde_json::to_vec(&value).unwrap()).unwrap();
        std::fs::set_permissions(&self.config, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    pub fn value(&self) -> Value {
        serde_json::from_slice(&std::fs::read(&self.config).unwrap()).unwrap()
    }
    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_f5y"));
        command
            .current_dir(&self.work)
            .env_remove("FOUNDRY_DATABASE")
            .env("FOUNDRY_ADAPTER", &self.adapter)
            .env("FOUNDRY_ADAPTER_CONFIG", &self.config)
            .env("FOUNDRY_TOOLHOST", "/usr/bin/true")
            .env("FOUNDRY_TEST_SECRET", "private-env-canary")
            .arg("--database")
            .arg(&self.database)
            .arg("--json");
        command
    }
    pub fn received(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.work.join("received.json")).unwrap()).unwrap()
    }
    pub fn assert_preflight(&self, output: &Output) {
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!self.database.exists());
        assert!(!self.work.join("received.json").exists());
        private_output(output);
    }
}
pub fn private_output(output: &Output) {
    let rendered = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for secret in [
        "private-key-canary",
        "private-system-canary",
        "private-env-canary",
    ] {
        assert!(!rendered.contains(secret));
    }
}
pub fn response(output: &Output) -> Value {
    private_output(output);
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
