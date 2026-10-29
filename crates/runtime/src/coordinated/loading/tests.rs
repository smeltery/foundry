use super::*;
use foundry_contracts::context::{ContextFormat, ContextId};
use std::{fs, process::Command, sync::mpsc, thread};

pub(super) fn plan() -> LoadingPlan {
    let id = |value| ContextId::new(value).unwrap();
    let payload = ContextPayload::new(b"private-context".to_vec()).unwrap();
    let descriptor = RestoreDescriptor {
        capture_id: id("capture"),
        checkpoint_id: id("checkpoint"),
        record_digest: ContextDigest::of(b"record"),
        witness_digest: ContextDigest::of(b"witness"),
        format: ContextFormat {
            profile: id("current"),
            format_version: id("1"),
            producer_build: id("fixture"),
        },
        configuration_digest: ContextDigest::of(b"config"),
        payload_length: payload.as_bytes().len() as u64,
        payload_digest: payload.digest(),
        retention_binding: ContextDigest::of(b"retention"),
    };
    LoadingPlan::new(
        std::env::temp_dir(),
        LoadingScope {
            restore_id: id("restore"),
            load_id: ContextReceipt::new(uuid::Uuid::new_v4().to_string()).unwrap(),
        },
        descriptor,
        payload,
        LoadingLimits {
            phase_timeout: Duration::from_secs(2),
            retained_timeout: Duration::from_secs(2),
        },
        LoadingPurpose::PrepareContinuation,
        vec!["fixture_retention".into()],
    )
    .unwrap()
}
pub(super) struct Fixture {
    pub(super) spec: AdapterSpec,
    log: PathBuf,
}
impl Fixture {
    pub(super) fn new(mode: &str, plan: &LoadingPlan) -> Self {
        let python = Command::new("python3")
            .args(["-c", "import sys;print(sys.executable)"])
            .output()
            .unwrap();
        assert!(python.status.success());
        let mut spec = AdapterSpec::new(String::from_utf8(python.stdout).unwrap().trim());
        spec.ready_timeout = Duration::from_secs(2);
        let log = std::env::temp_dir().join(format!("loading-{}.jsonl", uuid::Uuid::new_v4()));
        spec.args = vec![
            "-u".into(),
            "-c".into(),
            include_str!("fixture.py").into(),
            mode.into(),
            plan.binding.to_hex().into(),
            log.as_os_str().into(),
        ];
        Self { spec, log }
    }
    pub(super) fn records(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.log);
    }
}
fn assert_reaped(pid: u32) {
    assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}
#[test]
fn handshake_retains_same_live_process_and_explicit_cleanup() {
    let plan = plan();
    let expected = plan.scope.clone();
    let binding = plan.binding;
    let fixture = Fixture::new("ok", &plan);
    let (_owner, cancellation) = mpsc::channel();
    let mut loaded = load(&fixture.spec, plan, &cancellation).unwrap();
    assert!(loaded.scope() == &expected);
    assert!(loaded.begin_binding() == binding);
    loaded.poll(&cancellation).unwrap();
    let pid = loaded.process_id();
    let observer = loaded.cleanup_observer();
    assert_eq!(observer.status(), CleanupStatus::Pending);
    let records = fixture.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["kind"], "restore_begin_v2");
    assert_eq!(records[1]["kind"], "restore_payload_v2");
    assert!(records.iter().all(|record| record["pid"] == pid));
    loaded.close().unwrap();
    assert_eq!(observer.status(), CleanupStatus::Complete);
    assert_reaped(pid);
}
#[test]
fn exact_handshake_and_required_capabilities_reject_mutations() {
    for mode in [
        "bad_prepared",
        "bad_binding",
        "bad_payload",
        "bad_receipt",
        "bad_scope",
        "early_restored",
        "duplicate_prepared",
        "duplicate_ready",
        "ordinary",
        "missing_restore",
        "missing_continue",
        "missing_retention",
    ] {
        let plan = plan();
        let fixture = Fixture::new(mode, &plan);
        let (_owner, cancellation) = mpsc::channel();
        let result = load(&fixture.spec, plan, &cancellation);
        assert!(result.is_err(), "mode {mode}");
        for record in fixture.records() {
            assert_ne!(record["kind"], "unexpected");
            assert_reaped(record["pid"].as_u64().unwrap() as u32);
        }
    }
}
#[test]
fn positive_deadlines_bound_each_loading_wait() {
    for mode in ["stall_ready", "stall_prepared", "stall_restored"] {
        let mut plan = plan();
        plan.limits.phase_timeout = Duration::from_millis(80);
        let mut fixture = Fixture::new(mode, &plan);
        fixture.spec.ready_timeout = Duration::from_millis(80);
        let (_owner, cancellation) = mpsc::channel();
        let start = Instant::now();
        let error = match load(&fixture.spec, plan, &cancellation) {
            Err(error) => error,
            Ok(_) => panic!("deadline"),
        };
        assert!(matches!(error.cause, LoadingCause::PhaseTimeout));
        assert!(start.elapsed() < Duration::from_secs(3));
        for record in fixture.records() {
            assert_reaped(record["pid"].as_u64().unwrap() as u32);
        }
    }
    let mut plan = plan();
    plan.limits.phase_timeout = Duration::ZERO;
    let fixture = Fixture::new("ok", &plan);
    let (_owner, cancellation) = mpsc::channel();
    // Public construction rejects zero; internal mutation must still fail safely before Begin.
    assert!(load(&fixture.spec, plan, &cancellation).is_err());
    assert!(fixture.records().is_empty());
}
#[test]
fn cancellation_and_owner_disconnect_interrupt_wait_and_cleanup() {
    for disconnect in [false, true] {
        let plan = plan();
        let fixture = Fixture::new("stall_prepared", &plan);
        let (owner, cancellation) = mpsc::channel();
        let trigger = thread::spawn(move || {
            thread::sleep(Duration::from_millis(80));
            if !disconnect {
                owner.send(CancelMode::Graceful).unwrap();
            }
        });
        let error = match load(&fixture.spec, plan, &cancellation) {
            Err(error) => error,
            Ok(_) => panic!("cancel"),
        };
        trigger.join().unwrap();
        assert!(matches!(
            (disconnect, error.cause),
            (true, LoadingCause::OwnerDisconnected) | (false, LoadingCause::Cancelled)
        ));
        for record in fixture.records() {
            assert_reaped(record["pid"].as_u64().unwrap() as u32);
        }
    }
}
#[test]
fn retained_poll_rejects_extra_output_and_reports_cleanup() {
    for mode in ["extra", "exit"] {
        let plan = plan();
        let fixture = Fixture::new(mode, &plan);
        let (_owner, cancellation) = mpsc::channel();
        let mut loaded = load(&fixture.spec, plan, &cancellation).unwrap();
        let pid = loaded.process_id();
        let observer = loaded.cleanup_observer();
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            if loaded.poll(&cancellation).is_err() {
                break;
            }
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(5));
        }
        assert_ne!(observer.status(), CleanupStatus::Pending);
        assert_reaped(pid);
    }
}
#[test]
fn retained_cancel_expiry_and_drop_keep_cleanup_observable() {
    for mode in 0..3 {
        let mut plan = plan();
        if mode == 1 {
            plan.limits.retained_timeout = Duration::from_millis(20);
        }
        let fixture = Fixture::new("ok", &plan);
        let (owner, cancellation) = mpsc::channel();
        let mut loaded = load(&fixture.spec, plan, &cancellation).unwrap();
        let observer = loaded.cleanup_observer();
        let pid = loaded.process_id();
        match mode {
            0 => {
                owner.send(CancelMode::Hard).unwrap();
                assert!(matches!(
                    loaded.poll(&cancellation).unwrap_err().cause,
                    LoadingCause::Cancelled
                ));
            }
            1 => {
                thread::sleep(Duration::from_millis(25));
                assert!(matches!(
                    loaded.poll(&cancellation).unwrap_err().cause,
                    LoadingCause::RetainedTimeout
                ));
            }
            _ => {
                drop(loaded);
            }
        }
        assert_eq!(observer.status(), CleanupStatus::Complete);
        assert_reaped(pid);
    }
}

#[test]
fn purpose_capability_matrix_preserves_inspection_without_claiming_continue() {
    for purpose in [LoadingPurpose::Inspect, LoadingPurpose::PrepareContinuation] {
        for mode in [
            "ok",
            "missing_continue",
            "missing_restore",
            "missing_retention",
        ] {
            let mut plan = plan();
            plan.purpose = purpose;
            let fixture = Fixture::new(mode, &plan);
            let (_owner, cancellation) = mpsc::channel();
            let result = load(&fixture.spec, plan, &cancellation);
            let supported =
                mode == "ok" || (mode == "missing_continue" && purpose == LoadingPurpose::Inspect);
            if supported {
                let loaded = result.unwrap();
                assert_eq!(loaded.purpose(), purpose);
                assert_eq!(fixture.records().len(), 2);
                loaded.close().unwrap();
            } else {
                assert!(
                    result.is_err(),
                    "unsupported purpose/capability combination"
                );
                assert!(fixture.records().is_empty());
            }
        }
    }
}
