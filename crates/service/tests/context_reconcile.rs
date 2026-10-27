#![cfg(unix)]
#[path = "../src/context_reconcile.rs"]
mod context_reconcile;
#[path = "../src/tool_digest.rs"]
mod tool_digest;
use context_reconcile::reconcile_helper;
use foundry_contracts::CreateSession;
use foundry_runtime::coordinated::capture::{DeliveredTrace, ResultProvenance};
use foundry_service::{Result, ServiceError};
use foundry_store::{CaptureToolEvidence, Store, ToolOutcome};
use foundry_tools::host::{self, Input, Outcome, Reply};

fn fixture(missing: bool) -> (CaptureToolEvidence, Input, Reply, DeliveredTrace) {
    let directory = tempfile::tempdir().unwrap();
    if !missing {
        std::fs::write(directory.path().join("note.txt"), "line\n🦀").unwrap();
    }
    let mut store = Store::open(directory.path().join("store.db")).unwrap();
    let session = store
        .create(CreateSession {
            prompt: "read note".into(),
            directory: directory.path().display().to_string(),
            model: None,
            idempotency_key: None,
        })
        .unwrap();
    let fence = store.begin_attempt(&session.id, session.version).unwrap();
    let input = Input::Path(host::PathArgs {
        path: "note.txt".into(),
    });
    let request = host::Request {
        version: host::VERSION,
        attempt_id: fence.attempt_id.clone(),
        call_id: "read-1".into(),
        grants: vec![host::Grant::Read],
        limits: host::HostLimits {
            request_bytes: host::MAX_REQUEST_BYTES,
            read_bytes: 4096,
            write_bytes: 4096,
            output_bytes: 8192,
            entries: 16,
        },
        tool: "read".into(),
        input: input.clone(),
    };
    store
        .claim_tool(
            &fence,
            &request.call_id,
            &request.tool,
            tool_digest::input(&request.tool, &input).unwrap(),
        )
        .unwrap();
    let bytes = serde_json::to_vec(&request).unwrap();
    let mut frame = (bytes.len() as u32).to_be_bytes().to_vec();
    frame.extend(bytes);
    let root =
        cap_std::fs::Dir::open_ambient_dir(directory.path(), cap_std::ambient_authority()).unwrap();
    let mut response = Vec::new();
    host::serve(root, &mut frame.as_slice(), &mut response).unwrap();
    let length = u32::from_be_bytes(response[..4].try_into().unwrap()) as usize;
    assert_eq!(length, response.len() - 4);
    let reply: Reply = serde_json::from_slice(&response[4..]).unwrap();
    let digest = tool_digest::output(&reply).unwrap();
    let outcome = if missing {
        ToolOutcome::Failed
    } else {
        ToolOutcome::Success
    };
    store
        .finish_tool(&fence, &request.call_id, outcome, Some(digest))
        .unwrap();
    let record = store.capture_snapshot(&fence).unwrap().tools.remove(0);
    let mut trace = DeliveredTrace::new();
    trace
        .reserve_prompt(&session.id, &fence.attempt_id, "read note")
        .unwrap()
        .commit_enqueued();
    trace
        .reserve_helper_result(&fence.attempt_id, &request.call_id, digest, &reply.result)
        .unwrap()
        .commit_enqueued();
    (record, input, reply, trace)
}

#[test]
fn actual_host_success_and_read_failure_match_durable_receipts_and_wire() {
    for missing in [false, true] {
        let (record, input, reply, trace) = fixture(missing);
        reconcile_helper(&record, &input, &reply, &trace.entries()[1]).unwrap();
        let raw = serde_json::to_value(&input).unwrap();
        let event = tool_digest::event_input(&raw).unwrap();
        assert_eq!(event.bytes, serde_json::to_vec(&raw).unwrap().len());
        let typed_hex: String = record
            .input_digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_ne!(event.sha256, typed_hex);
        let full_hex: String = record
            .output_digest
            .unwrap()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_ne!(trace.entries()[1].wire_digest().to_hex(), full_hex);
    }
}

#[test]
fn identities_inputs_outcomes_and_each_digest_are_independently_bound() {
    let (record, input, reply, trace) = fixture(false);
    for changed in [
        "attempt", "call", "tool", "input", "output", "outcome", "missing",
    ] {
        let mut altered = record.clone();
        match changed {
            "attempt" => altered.attempt_id = "another-attempt".into(),
            "call" => altered.call_id = "another-call".into(),
            "tool" => altered.tool = "write".into(),
            "input" => altered.input_digest[0] ^= 1,
            "output" => altered.output_digest.as_mut().unwrap()[0] ^= 1,
            "outcome" => altered.outcome = ToolOutcome::Failed,
            _ => altered.output_digest = None,
        }
        assert!(
            reconcile_helper(&altered, &input, &reply, &trace.entries()[1]).is_err(),
            "{changed}"
        );
    }
    let wrong_input = Input::Path(host::PathArgs {
        path: "other.txt".into(),
    });
    assert!(reconcile_helper(&record, &wrong_input, &reply, &trace.entries()[1]).is_err());
    for field in ["version", "attempt", "call"] {
        let mut changed: Reply =
            serde_json::from_value(serde_json::to_value(&reply).unwrap()).unwrap();
        match field {
            "version" => changed.version += 1,
            "attempt" => changed.attempt_id = "another-attempt".into(),
            _ => changed.call_id = "another-call".into(),
        }
        assert!(reconcile_helper(&record, &input, &changed, &trace.entries()[1]).is_err());
    }
}

#[test]
fn wrong_wire_or_provenance_cannot_borrow_a_valid_ledger_digest() {
    let (record, input, reply, _) = fixture(false);
    for mode in ["wire", "ledger", "denied", "synthetic"] {
        let mut trace = DeliveredTrace::new();
        trace
            .reserve_prompt("session", &record.attempt_id, "read note")
            .unwrap()
            .commit_enqueued();
        let provenance = match mode {
            "denied" => ResultProvenance::Denied,
            "synthetic" => ResultProvenance::SyntheticNoEffect,
            "ledger" => ResultProvenance::PersistedHelper {
                ledger_output_digest: [0; 32],
            },
            _ => ResultProvenance::PersistedHelper {
                ledger_output_digest: record.output_digest.unwrap(),
            },
        };
        let body = if mode == "wire" {
            serde_json::json!({"status":"ok","output":"substituted"})
        } else {
            serde_json::to_value(&reply.result).unwrap()
        };
        trace
            .reserve_result(&record.attempt_id, &record.call_id, provenance, &body)
            .unwrap()
            .commit_enqueued();
        assert!(
            reconcile_helper(&record, &input, &reply, &trace.entries()[1]).is_err(),
            "{mode}"
        );
    }
}

#[test]
fn unknown_effects_and_oversized_trusted_values_never_reconcile() {
    let (mut record, input, mut reply, trace) = fixture(false);
    reply.result = Outcome::Error {
        code: host::ErrorCode::Io,
        effect: host::Effect::Unknown,
    };
    record.outcome = ToolOutcome::Uncertain;
    record.output_digest = Some(tool_digest::output(&reply).unwrap());
    assert!(reconcile_helper(&record, &input, &reply, &trace.entries()[1]).is_err());
    let oversized = Input::Path(host::PathArgs {
        path: "\n".repeat(host::MAX_REQUEST_BYTES),
    });
    assert!(reconcile_helper(&record, &oversized, &reply, &trace.entries()[1]).is_err());
    reply.result = Outcome::Ok {
        output: serde_json::json!("\n".repeat(host::MAX_REPLY_BYTES)),
    };
    assert!(reconcile_helper(&record, &input, &reply, &trace.entries()[1]).is_err());
}
