use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;

pub struct Fixture {
    pub directory: TempDir,
    pub adapter: PathBuf,
}
impl Fixture {
    pub fn new(mode: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let directory = TempDir::new().unwrap();
        std::fs::create_dir(directory.path().join("work")).unwrap();
        let python = Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        assert!(python.status.success());
        let python = String::from_utf8(python.stdout).unwrap();
        let adapter = directory.path().join("adapter");
        std::fs::write(
            &adapter,
            format!(
                "#!{}\nmode={mode:?}\n{}",
                python.trim(),
                include_str!("adapter.py")
            ),
        )
        .unwrap();
        std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { directory, adapter }
    }
    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_f5y"));
        command
            .current_dir(self.directory.path())
            .env_remove("FOUNDRY_DATABASE")
            .env_remove("FOUNDRY_TOOLHOST")
            .env("FOUNDRY_ADAPTER", &self.adapter)
            .env("FOUNDRY_TEST_SECRET", "must-not-pass")
            .args(["--database", "state.db", "--json"]);
        command
    }
    pub fn root(&self) -> &Path {
        self.directory.path()
    }
    pub fn task(&self, helper: &Path, grants: &[&str]) -> Output {
        let mut command = self.command();
        command
            .env("FOUNDRY_TOOLHOST", helper)
            .args(["fixture", "--dir", "work"]);
        for grant in grants {
            command.args(["--tool-grant", grant]);
        }
        command.output().unwrap()
    }
}
pub fn helper() -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os("FOUNDRY_TEST_TOOLHOST")
            .expect("build f5y-toolhost and set absolute FOUNDRY_TEST_TOOLHOST"),
    );
    assert!(path.is_absolute() && path.is_file());
    path
}
pub fn response(output: &Output) -> Value {
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
