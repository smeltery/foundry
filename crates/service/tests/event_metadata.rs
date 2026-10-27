#![cfg(unix)]
#[path = "../../../apps/cli/src/watch.rs"]
mod watch;
use cap_std::fs::Dir;
use foundry_contracts::{CreateSession, Lifecycle};
use foundry_runtime::{AdapterSpec, coordinated::TrustedToolPolicy};
use foundry_service::Service;
use foundry_store::Store;
use foundry_tools::host::{Grant, HostLimits};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::Command,
    sync::{Arc, atomic::AtomicU8},
    time::Duration,
};

const PRIVATE_PATH: &str = "private-path-sentinel.txt";
const PRIVATE_CONTENT: &str = "private-content-sentinel-é\n";
const ADAPTER: &str = r#"
import json, sys, time
args = json.loads(sys.argv[1]); fail = sys.argv[2] == 'fail'; name=sys.argv[3]
print(json.dumps({'type':'ready','version':1}),flush=True)
start=json.loads(sys.stdin.readline()); attempt=start['attempt_id']
print(json.dumps({'type':'tool_call','attempt_id':attempt,'call_id':'call','name':name,'input':args}),flush=True)
reply=json.loads(sys.stdin.readline()); assert reply['type']=='tool_result'
if fail:
    print(json.dumps({'type':'failed','attempt_id':attempt,'code':str(args),'message':str(args)}),flush=True)
else:
    print(json.dumps({'type':'settled','attempt_id':attempt,'outcome':'success','result':'complete'}),flush=True)
time.sleep(10)
"#;
const HELPER: &str = r#"
import json, os, struct, sys
size=struct.unpack('>I',sys.stdin.buffer.read(4))[0]
req=json.loads(sys.stdin.buffer.read(size)); assert sys.stdin.buffer.read()==b''
assert req['input']['path']=='private-path-sentinel.txt'
assert req['input']['content']=='private-content-sentinel-é\n'
marker=os.open('dispatched',os.O_CREAT|os.O_WRONLY,0o600,dir_fd=3); os.close(marker)
reply={'version':1,'attempt_id':req['attempt_id'],'call_id':req['call_id'],
       'result':{'status':'ok','output':{'bytes_written':len(req['input']['content'].encode())}}}
data=json.dumps(reply).encode(); sys.stdout.buffer.write(struct.pack('>I',len(data))+data);sys.stdout.buffer.flush()
"#;

fn assert_private_absent(text: &str) {
    assert!(!text.contains(PRIVATE_PATH));
    assert!(!text.contains("private-content-sentinel"));
}

fn exercise(coordinated: bool, granted: bool, failed: bool, name: &str, input: Value) {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("db");
    let mut service = Service::open(&database).unwrap();
    let session = service
        .create(CreateSession {
            prompt: "metadata test".into(),
            directory: root.path().display().to_string(),
            model: None,
            idempotency_key: None,
        })
        .unwrap();
    let python = Command::new("python3")
        .args(["-c", "import sys;print(sys.executable)"])
        .output()
        .unwrap();
    assert!(python.status.success());
    let python = String::from_utf8(python.stdout).unwrap().trim().to_owned();
    let mut spec = AdapterSpec::new(&python);
    spec.args = vec![
        "-u".into(),
        "-c".into(),
        ADAPTER.into(),
        serde_json::to_string(&input).unwrap().into(),
        if failed { "fail" } else { "complete" }.into(),
        name.into(),
    ];
    spec.run_timeout = Duration::from_secs(4);
    let result = if coordinated {
        let helper = root.path().join("helper");
        fs::write(&helper, format!("#!{python}\n{HELPER}")).unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        let policy = TrustedToolPolicy::new(
            helper,
            Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap(),
            if granted { vec![Grant::Create] } else { vec![] },
            HostLimits {
                request_bytes: 524288,
                read_bytes: 262144,
                write_bytes: 262144,
                output_bytes: 1048576,
                entries: 256,
            },
            Duration::from_secs(2),
        )
        .unwrap();
        service.run_with_tools(&session.id, 0, &spec, Arc::new(AtomicU8::new(0)), policy)
    } else {
        service.run(&session.id, 0, &spec)
    };
    if failed {
        assert_private_absent(&result.as_ref().unwrap_err().to_string());
    } else {
        assert_eq!(result.unwrap().lifecycle, Lifecycle::Exit);
    }
    assert_eq!(root.path().join("dispatched").exists(), granted);
    drop(service);
    let service = Service::open(&database).unwrap();
    let events = service.events(&session.id, 0, 100).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>(),
        vec![
            "adapter_ready",
            "tool_request",
            if failed {
                "adapter_failed"
            } else {
                "adapter_settled"
            }
        ]
    );
    let data = &events[1].data;
    assert_eq!(data.as_object().unwrap().len(), 7);
    assert_eq!(data["type"], "tool_call");
    assert_eq!(data["attempt_id"], events[1].attempt_id);
    assert_eq!(data["call_id"], "call");
    assert_eq!(data["name"], name);
    assert_eq!(data["input_encoding"], "serde_json.compact.v1");
    let bytes = serde_json::to_vec(&input).unwrap();
    assert_eq!(data["input_bytes"], bytes.len());
    let expected: String = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(data["input_sha256"], expected);
    assert!(data.get("input").is_none());
    assert_private_absent(&serde_json::to_string(&events).unwrap());
    let connection = rusqlite::Connection::open(&database).unwrap();
    let persisted: String = connection
        .query_row(
            "SELECT group_concat(data,'') FROM events WHERE session_id=?1",
            [&session.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_private_absent(&persisted);
    if granted {
        let record = Store::open(&database)
            .unwrap()
            .tool_call(&session.id, "call")
            .unwrap()
            .unwrap();
        let ledger: String = record
            .input_digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_ne!(
            expected, ledger,
            "event and typed ledger hash domains must remain distinct"
        );
    }
    for format in [watch::WatchFormat::JsonLines, watch::WatchFormat::Text] {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = watch::watch(
            &database,
            &session.id,
            watch::WatchOptions {
                interval: Duration::from_millis(1),
                format,
                after_sequence: 0,
                timeout: Some(Duration::from_secs(1)),
            },
            &mut stdout,
            &mut stderr,
        )
        .unwrap();
        assert_eq!(code, if failed { 2 } else { 0 });
        assert_private_absent(std::str::from_utf8(&stdout).unwrap());
        assert_private_absent(std::str::from_utf8(&stderr).unwrap());
        if matches!(format, watch::WatchFormat::JsonLines) {
            let lines: Vec<Value> = std::str::from_utf8(&stdout)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(lines.len(), 4);
            assert_eq!(lines[1]["event"]["data"], *data);
        }
    }
}
fn write_input() -> Value {
    json!({"path":PRIVATE_PATH,"content":PRIVATE_CONTENT,"mode":"create"})
}
#[test]
fn default_denial_keeps_one_metadata_event_without_raw_input() {
    exercise(false, false, false, "write", write_input());
}
#[test]
fn coordinated_denial_keeps_one_metadata_event_without_raw_input() {
    exercise(true, false, false, "write", write_input());
}
#[test]
fn granted_dispatch_keeps_ephemeral_input_but_persists_only_metadata() {
    exercise(true, true, false, "write", write_input());
}
#[test]
fn failed_run_and_both_watch_formats_omit_tool_input_and_failure_echo() {
    exercise(true, true, true, "write", write_input());
}
#[test]
fn unknown_denied_tool_and_untyped_input_still_have_metadata() {
    exercise(
        false,
        false,
        false,
        "custom.tool",
        json!([PRIVATE_PATH,PRIVATE_CONTENT,{"extra":null}]),
    );
}
