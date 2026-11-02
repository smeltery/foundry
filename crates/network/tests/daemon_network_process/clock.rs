use std::{
    fs::{self, OpenOptions, TryLockError},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct Clock {
    library: PathBuf,
    offset: PathBuf,
}
impl Clock {
    pub fn build(root: &Path) -> Self {
        let source = root.join("clock.c");
        fs::write(&source, include_bytes!("clock.c")).unwrap();
        let library = root.join(if cfg!(target_os = "macos") {
            "clock.dylib"
        } else {
            "clock.so"
        });
        let mut compiler = Command::new("cc");
        compiler.args(["-std=c11", "-O2", "-Wall", "-Wextra", "-Werror"]);
        if cfg!(target_os = "macos") {
            compiler.arg("-dynamiclib");
        } else {
            compiler.args(["-shared", "-fPIC"]);
        }
        compiler.arg(&source).arg("-o").arg(&library);
        if cfg!(target_os = "linux") {
            compiler.arg("-ldl");
        }
        clear(&mut compiler);
        checked(compiler);
        let clock = Self {
            library,
            offset: root.join("clock-offset"),
        };
        clock.set(0);
        clock.verify(root);
        clock
    }
    pub fn set(&self, seconds: u64) {
        assert!(seconds <= 31622400);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&self.offset)
            .unwrap();
        let metadata = file.metadata().unwrap();
        assert!(
            metadata.is_file()
                && metadata.nlink() == 1
                && metadata.uid() == unsafe { libc::geteuid() }
        );
        let deadline = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) if deadline.elapsed() < Duration::from_secs(2) => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(error) => panic!("fixture clock writer lock: {error}"),
            }
        }
        // Cooperating shared/exclusive locks keep one private inode and prevent
        // a reader from seeing partial bytes while the clock advances.
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .unwrap();
        file.set_len(0).unwrap();
        writeln!(file, "{seconds}").unwrap();
        file.sync_all().unwrap();
    }
    pub fn apply(&self, command: &mut Command) {
        clear(command);
        command.env("FOUNDRY_FIXTURE_CLOCK_DELTA", &self.offset);
        command.env(
            if cfg!(target_os = "macos") {
                "DYLD_INSERT_LIBRARIES"
            } else {
                "LD_PRELOAD"
            },
            &self.library,
        );
    }
    fn verify(&self, root: &Path) {
        let native_source = root.join("clock-probe.c");
        let native = root.join("clock-probe-c");
        fs::write(&native_source, include_bytes!("clock_probe.c")).unwrap();
        let mut cc = Command::new("cc");
        cc.args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
            .arg(&native_source)
            .arg("-o")
            .arg(&native);
        clear(&mut cc);
        checked(cc);
        let rust_source = root.join("clock-probe.rs");
        let rust = root.join("clock-probe-rust");
        fs::write(&rust_source, include_bytes!("clock_probe.rs")).unwrap();
        let mut rustc = Command::new("rustc");
        rustc
            .args(["--edition=2024", "--crate-name", "fixture_clock_probe"])
            .arg(&rust_source)
            .arg("-o")
            .arg(&rust);
        clear(&mut rustc);
        checked(rustc);
        let mut lock_check = Command::new(&rust);
        clear(&mut lock_check);
        lock_check.arg(&self.offset).arg(&native);
        checked(lock_check);
        let baseline = self.probe(&native);
        let unshifted = now();
        self.set(7201);
        let before = now();
        let shifted = self.probe(&native);
        let mut command = Command::new(&rust);
        self.apply(&mut command);
        let rust_wall: u64 = String::from_utf8(checked(command).stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let after = now();
        for observed in [shifted[0] as u64, shifted[1] as u64, rust_wall] {
            assert!(
                observed >= before + 7201 && observed <= after + 7201,
                "child wall clock was not interposed"
            );
        }
        assert!(
            shifted[2] >= baseline[2] && shifted[2] - baseline[2] < 10_000_000_000,
            "monotonic clock was changed or probe stalled"
        );
        assert!(
            after - unshifted < 10,
            "fixture changed the parent wall clock"
        );
        self.set(0);
    }
    fn probe(&self, binary: &Path) -> Vec<i64> {
        let mut command = Command::new(binary);
        self.apply(&mut command);
        let values: Vec<i64> = String::from_utf8(checked(command).stdout)
            .unwrap()
            .split_whitespace()
            .map(|v| v.parse().unwrap())
            .collect();
        assert_eq!(values.len(), 3);
        values
    }
}
pub fn clear(command: &mut Command) {
    command
        .env_remove("DYLD_INSERT_LIBRARIES")
        .env_remove("LD_PRELOAD")
        .env_remove("FOUNDRY_FIXTURE_CLOCK_DELTA");
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn checked(mut command: Command) -> Output {
    // Only explicitly selected probe commands receive the fixture loader env.
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("fixture compiler/probe spawn");
    let read = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.by_ref().take(65537).read_to_end(&mut bytes).unwrap();
            assert!(bytes.len() <= 65536, "fixture command output ceiling");
            bytes
        })
    };
    let stdout = read(Box::new(child.stdout.take().unwrap()));
    let stderr = read(Box::new(child.stderr.take().unwrap()));
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(30) {
            child.kill().unwrap();
            child.wait().unwrap();
            stdout.join().unwrap();
            stderr.join().unwrap();
            panic!("fixture compiler/probe deadline");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let result = Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    };
    assert!(
        result.status.success(),
        "fixture compiler/probe failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    result
}

#[test]
fn fixture_clock_shifts_native_and_rust_wall_time_without_touching_parent_or_monotonic_time() {
    let root = super::config::Root::new();
    Clock::build(&root.0);
}
