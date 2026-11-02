fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() == 2 {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&arguments[0])
            .unwrap();
        file.try_lock().expect("Rust fixture exclusive lock");
        let status = std::process::Command::new(&arguments[1])
            .arg("expect_locked")
            .env("FOUNDRY_FIXTURE_CLOCK_DELTA", &arguments[0])
            .status()
            .unwrap();
        assert!(
            status.success(),
            "native shared lock did not observe Rust exclusive lock"
        );
        return;
    }
    assert!(arguments.is_empty(), "fixture clock probe arguments");
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("fixture wall clock")
        .as_secs();
    println!("{seconds}");
}
