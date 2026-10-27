#![cfg(unix)]
use cap_std::fs::Dir;
use foundry_tools::host::{self, HostError};
use serde_json::{Value, json};
use std::{
    io::{self, Cursor, Read, Write},
    path::PathBuf,
};

fn request() -> Value {
    json!({"version":1,"attempt_id":"attempt","call_id":"call","grants":["write.create"],
        "limits":{"request_bytes":524288,"read_bytes":262144,"write_bytes":262144,"output_bytes":1048576,"entries":256},
        "tool":"write","input":{"path":"created","content":"payload","mode":"create"}})
}
fn framed(value: &Value) -> Vec<u8> {
    let bytes = serde_json::to_vec(value).unwrap();
    let mut frame = (bytes.len() as u32).to_be_bytes().to_vec();
    frame.extend(bytes);
    frame
}
struct CheckEof {
    source: Cursor<Vec<u8>>,
    target: PathBuf,
    fail: bool,
    checked: bool,
}
impl Read for CheckEof {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let size = self.source.read(bytes)?;
        if size == 0 {
            assert!(
                !self.target.exists(),
                "filesystem mutation preceded request EOF"
            );
            self.checked = true;
            if self.fail {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
        }
        Ok(size)
    }
}

#[test]
fn complete_eof_is_required_before_any_mutation() {
    for fail in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = Dir::open_ambient_dir(temp.path(), cap_std::ambient_authority()).unwrap();
        let mut input = CheckEof {
            source: Cursor::new(framed(&request())),
            target: temp.path().join("created"),
            fail,
            checked: false,
        };
        let mut output = Vec::new();
        let result = host::serve(root, &mut input, &mut output);
        assert!(input.checked);
        if fail {
            assert!(matches!(result, Err(HostError::InputIo)));
            assert!(!input.target.exists());
            assert!(output.is_empty());
        } else {
            assert!(result.is_ok());
            assert_eq!(std::fs::read_to_string(input.target).unwrap(), "payload");
        }
    }
}

struct BrokenOutput;
impl Write for BrokenOutput {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn lost_reply_after_write_is_an_error_with_possible_effect_not_a_retry_signal() {
    let temp = tempfile::tempdir().unwrap();
    let root = Dir::open_ambient_dir(temp.path(), cap_std::ambient_authority()).unwrap();
    let result = host::serve(
        root,
        &mut Cursor::new(framed(&request())),
        &mut BrokenOutput,
    );
    assert!(matches!(result, Err(HostError::OutputIo)));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("created")).unwrap(),
        "payload"
    );
}

#[test]
fn strict_trusted_policy_rejects_missing_fields_unknown_grants_and_bounds() {
    let base = request();
    let mut cases = Vec::new();
    for field in [
        "limits",
        "version",
        "attempt_id",
        "call_id",
        "tool",
        "input",
    ] {
        let mut bad = base.clone();
        bad.as_object_mut().unwrap().remove(field);
        cases.push(bad);
    }
    for grants in [
        json!(["all"]),
        json!(["read", "read"]),
        json!(null),
        json!({"read":true}),
    ] {
        let mut bad = base.clone();
        bad["grants"] = grants;
        cases.push(bad);
    }
    for (field, value) in [
        ("entries", 0),
        ("entries", 257),
        ("request_bytes", 1),
        ("read_bytes", 262145),
        ("write_bytes", 262145),
        ("output_bytes", 127),
    ] {
        let mut bad = base.clone();
        bad["limits"][field] = json!(value);
        cases.push(bad);
    }
    for id in ["".to_string(), "x".repeat(129), "x\ny".into()] {
        let mut bad = base.clone();
        bad["call_id"] = json!(id);
        cases.push(bad);
    }
    let mut bad = base.clone();
    bad["limits"]["ambient_root"] = json!("ignored");
    cases.push(bad);
    for bad in cases {
        assert!(host::read_request(&mut Cursor::new(framed(&bad))).is_err());
    }
}

#[test]
fn correlation_envelope_is_included_in_reply_bound() {
    let temp = tempfile::tempdir().unwrap();
    // Escaping expands this content to exactly the configured output limit.
    let content = format!("{}{}", "\0".repeat(174760), "a".repeat(2));
    std::fs::write(temp.path().join("large"), &content).unwrap();
    let root = Dir::open_ambient_dir(temp.path(), cap_std::ambient_authority()).unwrap();
    let mut value = request();
    value["tool"] = json!("read");
    value["grants"] = json!(["read"]);
    value["input"] = json!({"path":"large"});
    value["call_id"] = json!("\"".repeat(128));
    let mut bytes = Vec::new();
    host::serve(root, &mut Cursor::new(framed(&value)), &mut bytes).unwrap();
    let reply: Value = serde_json::from_slice(&bytes[4..]).unwrap();
    assert_eq!(reply["result"]["code"], "limit");
    assert_eq!(reply["result"]["effect"], "none");
    assert!(bytes.len() <= host::MAX_REPLY_BYTES + 4);
}
