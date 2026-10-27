#![cfg(unix)]
use cap_std::fs::Dir;
use foundry_tools::{Grants, Limits, WorkspaceTools, host};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::AsRawFd,
        unix::{fs::symlink, process::CommandExt},
    },
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn request(tool: &str, input: Value, grants: &[&str]) -> Value {
    json!({"version":1,"attempt_id":"attempt-a","call_id":"call-a","grants":grants,
        "limits":{"request_bytes":524288,"read_bytes":262144,"write_bytes":262144,"output_bytes":1048576,"entries":256},
        "tool":tool,"input":input})
}
fn frame(value: &Value) -> Vec<u8> {
    let bytes = serde_json::to_vec(value).unwrap();
    let mut framed = (bytes.len() as u32).to_be_bytes().to_vec();
    framed.extend(bytes);
    framed
}
fn decode(bytes: &[u8]) -> Value {
    assert!(bytes.len() >= 4);
    let size = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
    assert_eq!(size, bytes.len() - 4);
    assert!(size <= host::MAX_REPLY_BYTES);
    serde_json::from_slice(&bytes[4..]).unwrap()
}

// Parent-side fixture only: descriptor setup stays async-signal-safe in pre_exec.
fn run(root: Option<&File>, bytes: &[u8]) -> (bool, Vec<u8>, String) {
    let mut input = tempfile::tempfile().unwrap();
    input.write_all(bytes).unwrap();
    input.rewind().unwrap();
    let mut output = tempfile::tempfile().unwrap();
    let mut errors = tempfile::tempfile().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_f5y-toolhost"));
    command
        .env_clear()
        .current_dir("/")
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(output.try_clone().unwrap()))
        .stderr(Stdio::from(errors.try_clone().unwrap()));
    let descriptor = root.map(AsRawFd::as_raw_fd);
    unsafe {
        command.pre_exec(move || {
            match descriptor {
                Some(fd) => {
                    if fd != 3 && libc::dup2(fd, 3) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                None => {
                    libc::close(3);
                }
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("helper did not exit");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    output.seek(SeekFrom::Start(0)).unwrap();
    errors.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = Vec::new();
    output.read_to_end(&mut bytes).unwrap();
    let mut diagnostic = String::new();
    errors.read_to_string(&mut diagnostic).unwrap();
    (status.success(), bytes, diagnostic)
}
fn invoke(root: &File, value: &Value) -> Value {
    let (success, bytes, stderr) = run(Some(root), &frame(value));
    assert!(success, "{stderr}");
    assert!(stderr.is_empty());
    let result = decode(&bytes);
    assert_eq!(result["version"], 1);
    assert_eq!(result["attempt_id"], "attempt-a");
    assert_eq!(result["call_id"], "call-a");
    result["result"].clone()
}

#[test]
fn inherited_capability_survives_rename_and_ambient_path_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("value"), "original").unwrap();
    let fd = File::open(&root).unwrap();
    let tools = WorkspaceTools::from_dir(
        Dir::from_std_file(fd.try_clone().unwrap()),
        Grants {
            read: true,
            ..Grants::default()
        },
        Limits::default(),
    )
    .unwrap();
    fs::rename(&root, temp.path().join("moved")).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("value"), "decoy").unwrap();
    assert_eq!(
        tools.execute_json("read", br#"{"path":"value"}"#).unwrap()["content"],
        "original"
    );
    let reply = invoke(&fd, &request("read", json!({"path":"value"}), &["read"]));
    assert_eq!(reply["output"]["content"], "original");
}

#[test]
fn missing_or_regular_file_descriptor_fails_with_neutral_diagnostic() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sensitive-name");
    fs::write(&path, "secret").unwrap();
    let file = File::open(path).unwrap();
    let payload = frame(&request("list", json!({"path":"."}), &["list"]));
    for fd in [None, Some(&file)] {
        let (success, stdout, stderr) = run(fd, &payload);
        assert!(!success);
        assert!(stdout.is_empty());
        assert_eq!(stderr, "toolhost_bootstrap\n");
    }
    assert!(
        WorkspaceTools::from_dir(
            Dir::from_std_file(file),
            Grants::default(),
            Limits::default()
        )
        .is_err()
    );
}

#[test]
fn missing_grants_deny_every_operation_and_explicit_grants_are_independent() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("present"), "old").unwrap();
    let fd = File::open(temp.path()).unwrap();
    for (tool, input) in [
        ("read", json!({"path":"present"})),
        ("list", json!({"path":"."})),
        (
            "write",
            json!({"path":"new","content":"new","mode":"create"}),
        ),
        (
            "write",
            json!({"path":"present","content":"new","mode":"overwrite"}),
        ),
    ] {
        let mut value = request(tool, input, &[]);
        value.as_object_mut().unwrap().remove("grants");
        let reply = invoke(&fd, &value);
        assert_eq!(
            reply,
            json!({"status":"error","code":"denied","effect":"none"})
        );
    }
    assert!(!temp.path().join("new").exists());
    assert_eq!(
        fs::read_to_string(temp.path().join("present")).unwrap(),
        "old"
    );
    let create = request(
        "write",
        json!({"path":"new","content":"new","mode":"create"}),
        &["write.create"],
    );
    assert_eq!(invoke(&fd, &create)["status"], "ok");
    assert_eq!(invoke(&fd, &create)["effect"], "unknown");
    let overwrite = request(
        "write",
        json!({"path":"new","content":"updated","mode":"overwrite"}),
        &["write.overwrite"],
    );
    assert_eq!(invoke(&fd, &overwrite)["status"], "ok");
    assert_eq!(
        invoke(&fd, &request("read", json!({"path":"new"}), &["list"]))["code"],
        "denied"
    );
    assert_eq!(
        invoke(
            &fd,
            &request(
                "write",
                json!({"path":"absent","content":"x","mode":"overwrite"}),
                &["write.overwrite"]
            )
        )["status"],
        "error"
    );
    assert!(!temp.path().join("absent").exists());
}

#[test]
fn malformed_oversized_duplicate_and_trailing_requests_never_write() {
    let temp = tempfile::tempdir().unwrap();
    let fd = File::open(temp.path()).unwrap();
    let valid = request(
        "write",
        json!({"path":"never","content":"x","mode":"create"}),
        &["write.create"],
    );
    let mut cases = vec![
        vec![],
        vec![0, 0, 0, 0],
        ((host::MAX_REQUEST_BYTES + 1) as u32)
            .to_be_bytes()
            .to_vec(),
        vec![0, 0, 0, 3, b'{'],
    ];
    let mut twice = frame(&valid);
    twice.extend(frame(&valid));
    cases.push(twice);
    let mut trailing = frame(&valid);
    trailing.push(b'\n');
    cases.push(trailing);
    for field in ["root", "fd", "extra"] {
        let mut value = valid.clone();
        value[field] = json!("bad");
        cases.push(frame(&value));
    }
    let mut wrong = valid.clone();
    wrong["version"] = json!(2);
    cases.push(frame(&wrong));
    let mut wrong = valid.clone();
    wrong["limits"]["output_bytes"] = json!(1048577);
    cases.push(frame(&wrong));
    let mut wrong = valid.clone();
    wrong["grants"] = json!(["write.create", "write.create"]);
    cases.push(frame(&wrong));
    let mut wrong = valid.clone();
    wrong["input"]["root"] = json!("bad");
    cases.push(frame(&wrong));
    let encoded = serde_json::to_string(&valid).unwrap().replacen(
        "\"path\":\"never\"",
        "\"path\":\"never\",\"path\":\"other\"",
        1,
    );
    let mut duplicate = (encoded.len() as u32).to_be_bytes().to_vec();
    duplicate.extend(encoded.as_bytes());
    cases.push(duplicate);
    for bytes in cases {
        let (success, stdout, stderr) = run(Some(&fd), &bytes);
        assert!(!success);
        assert!(stdout.is_empty());
        assert!(stderr.starts_with("toolhost_"));
        assert!(stderr.len() < 40);
        assert!(!temp.path().join("never").exists());
    }
}

#[test]
fn symlink_escapes_and_content_limits_are_enforced_in_helper() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret"), "unchanged").unwrap();
    symlink(outside.path(), temp.path().join("escape")).unwrap();
    symlink(outside.path().join("secret"), temp.path().join("leaf")).unwrap();
    let fd = File::open(temp.path()).unwrap();
    for path in ["leaf", "escape/secret", "../secret", "/etc/passwd"] {
        let reply = invoke(&fd, &request("read", json!({"path":path}), &["read"]));
        assert_eq!(reply["status"], "error");
        let reply = invoke(
            &fd,
            &request(
                "write",
                json!({"path":path,"content":"changed","mode":"overwrite"}),
                &["write.overwrite"],
            ),
        );
        assert_eq!(reply["status"], "error");
    }
    assert_eq!(
        fs::read_to_string(outside.path().join("secret")).unwrap(),
        "unchanged"
    );
    let mut limited = request(
        "write",
        json!({"path":"new","content":"abc","mode":"create"}),
        &["write.create"],
    );
    limited["limits"]["write_bytes"] = json!(2);
    assert_eq!(invoke(&fd, &limited)["code"], "limit");
    assert!(!temp.path().join("new").exists());
}

#[test]
fn reply_envelope_and_escaped_output_remain_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let fd = File::open(temp.path()).unwrap();
    fs::write(temp.path().join("large"), vec![0; 262144]).unwrap();
    let reply = invoke(&fd, &request("read", json!({"path":"large"}), &["read"]));
    assert_eq!(reply["code"], "limit");
    fs::write(temp.path().join("large"), "a".repeat(262144)).unwrap();
    let reply = invoke(&fd, &request("read", json!({"path":"large"}), &["read"]));
    assert_eq!(reply["output"]["content"].as_str().unwrap().len(), 262144);
}
