use cap_std::fs::Dir;
use foundry_runtime::tool_process::{ToolPoll, ToolProcess, ToolProcessError, ToolProcessSpec};
use foundry_tools::host::{self, Request};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

pub struct Fixture {
    pub path: PathBuf,
    pub workspace: PathBuf,
    pub helper: PathBuf,
}
impl Fixture {
    pub fn new(scenario: &str) -> Self {
        let python = Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        assert!(python.status.success());
        let python = String::from_utf8(python.stdout).unwrap();
        let path = std::env::temp_dir().join(format!("tool-process-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        let workspace = path.join("workspace");
        fs::create_dir(&workspace).unwrap();
        let helper = path.join("helper");
        fs::write(
            &helper,
            format!(
                "#!{}\nSCENARIO={scenario:?}\n{}",
                python.trim(),
                include_str!("../tool_fixture.py")
            ),
        )
        .unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            path,
            workspace,
            helper,
        }
    }
    pub fn spec(&self) -> ToolProcessSpec {
        ToolProcessSpec {
            helper: self.helper.clone(),
            directory: Dir::open_ambient_dir(&self.workspace, cap_std::ambient_authority())
                .unwrap(),
            timeout: Duration::from_secs(3),
        }
    }
    pub fn start(&self) -> ToolProcess {
        ToolProcess::start(self.spec(), request(), Arc::new(AtomicBool::new(false))).unwrap()
    }
    pub fn pid(&self, name: &str) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(text) = fs::read_to_string(self.workspace.join(name))
                && let Ok(pid) = text.parse()
            {
                return pid;
            }
            assert!(Instant::now() < deadline, "missing child marker");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Only detached test descendants need explicit fixture cleanup. Never
        // signal a completed/reaped leader's old group.
        if let Ok(text) = fs::read_to_string(self.workspace.join("descendant"))
            && let Ok(pid) = text.parse::<i32>()
        {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        let _ = fs::remove_dir_all(&self.path);
    }
}
pub fn request() -> Request {
    serde_json::from_value(serde_json::json!({"version":1,"attempt_id":"attempt","call_id":"call","grants":["read"],
        "limits":{"request_bytes":524288,"read_bytes":262144,"write_bytes":262144,"output_bytes":1048576,"entries":256},"tool":"read","input":{"path":"value"}})).unwrap()
}
pub fn finish(process: &mut ToolProcess) -> Result<host::Reply, ToolProcessError> {
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        match process.poll() {
            ToolPoll::Pending => {}
            ToolPoll::Finished(result) => return result,
            ToolPoll::Consumed => panic!("duplicate consume"),
        }
        assert!(Instant::now() < deadline, "supervisor did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
}
pub fn assert_gone(pid: i32) {
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}
