use std::process::Command;

#[test]
fn version_requires_neither_a_directory_capability_nor_input() {
    let output = Command::new(env!("CARGO_BIN_EXE_f5y-toolhost"))
        .arg("--version")
        .env_clear()
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("f5y-toolhost {}\n", env!("FOUNDRY_VERSION"))
    );
    assert!(output.stderr.is_empty());
    let output = Command::new(env!("CARGO_BIN_EXE_f5y-toolhost"))
        .args(["--version", "extra"])
        .env_clear()
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}
