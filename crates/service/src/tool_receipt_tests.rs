use super::*;
use foundry_contracts::CreateSession;
use foundry_runtime::coordinated::SharedToolReply;

#[test]
fn shared_reply_retains_exact_ledger_digest_and_unknown_effect_evidence() {
    let mut store = Store::open(":memory:").unwrap();
    let session = store
        .create(CreateSession {
            prompt: "inspect".into(),
            directory: "/tmp".into(),
            model: None,
            idempotency_key: None,
        })
        .unwrap();
    let fence = store.begin_attempt(&session.id, 0).unwrap();
    let record = store.claim_tool(&fence, "call", "read", [1; 32]).unwrap();
    for (result, expected) in [
        (
            Outcome::Ok {
                output: serde_json::json!({"text":"value"}),
            },
            ToolOutcome::Success,
        ),
        (
            Outcome::Error {
                code: host::ErrorCode::Io,
                effect: Effect::None,
            },
            ToolOutcome::Failed,
        ),
        (
            Outcome::Error {
                code: host::ErrorCode::Io,
                effect: Effect::Unknown,
            },
            ToolOutcome::Uncertain,
        ),
    ] {
        let reply = host::Reply {
            version: host::VERSION,
            attempt_id: fence.attempt_id.clone(),
            call_id: record.call_id.clone(),
            result,
        };
        let expected_digest = tool_digest::output(&reply).unwrap();
        let shared = SharedToolReply::new(reply);
        let retained = shared.clone();
        assert!(std::ptr::eq(shared.reply(), retained.reply()));
        assert_eq!(
            validate_reply(&fence, &record, shared.reply()).unwrap(),
            (expected, Some(expected_digest))
        );
    }
}

#[test]
fn shared_evidence_does_not_relax_identity_or_size_checks() {
    let mut store = Store::open(":memory:").unwrap();
    let session = store
        .create(CreateSession {
            prompt: "inspect".into(),
            directory: "/tmp".into(),
            model: None,
            idempotency_key: None,
        })
        .unwrap();
    let fence = store.begin_attempt(&session.id, 0).unwrap();
    let record = store.claim_tool(&fence, "call", "read", [1; 32]).unwrap();
    let mut reply = host::Reply {
        version: host::VERSION,
        attempt_id: fence.attempt_id.clone(),
        call_id: "foreign".into(),
        result: Outcome::Ok {
            output: serde_json::json!(null),
        },
    };
    assert!(validate_reply(&fence, &record, &reply).is_err());
    reply.call_id = record.call_id.clone();
    reply.result = Outcome::Ok {
        output: serde_json::json!(""),
    };
    let overhead = serde_json::to_vec(&reply).unwrap().len();
    reply.result = Outcome::Ok {
        output: serde_json::json!("x".repeat(host::MAX_REPLY_BYTES - overhead)),
    };
    assert!(validate_reply(&fence, &record, &reply).is_ok());
    reply.result = Outcome::Ok {
        output: serde_json::json!("x".repeat(host::MAX_REPLY_BYTES - overhead + 1)),
    };
    assert!(validate_reply(&fence, &record, &reply).is_err());
    // Escaped output must be charged by encoded bytes, not raw string length.
    reply.result = Outcome::Ok {
        output: serde_json::json!("\n".repeat(host::MAX_REPLY_BYTES / 2)),
    };
    assert!(validate_reply(&fence, &record, &reply).is_err());
    reply.result = Outcome::Ok {
        output: serde_json::json!("x".repeat(host::MAX_REPLY_BYTES)),
    };
    assert!(validate_reply(&fence, &record, &reply).is_err());
}

// Obtain an opaque job identity from the real coordinator; tests cannot mint it.
#[cfg(unix)]
fn checkpoint(session: &foundry_contracts::Session, attempt: &str) -> CheckpointId {
    use foundry_runtime::{
        AdapterSpec,
        coordinated::{RuntimePersistence, run_adapter_coordinated},
    };
    use std::{ffi::OsString, time::Duration};
    std::thread::scope(|scope| {
        let (cancel_tx, cancel_rx) = mpsc::channel();
        let (_message_tx, message_rx) = mpsc::channel();
        let (request_tx, request_rx) = mpsc::sync_channel(2);
        let (_reply_tx, reply_rx) = mpsc::sync_channel(2);
        let mut spec = AdapterSpec::new("/usr/bin/python3");
        spec.args = vec![
            OsString::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fake_adapter.py"
            )),
            "wait".into(),
        ];
        spec.ready_timeout = Duration::from_secs(3);
        spec.run_timeout = Duration::from_secs(3);
        let worker = scope.spawn(move || {
            run_adapter_coordinated(
                &spec,
                session,
                attempt,
                &cancel_rx,
                &message_rx,
                RuntimePersistence {
                    requests: request_tx,
                    replies: reply_rx,
                },
                None,
            )
        });
        let request = match request_rx.recv_timeout(Duration::from_secs(4)) {
            Ok(request) => request,
            Err(error) => {
                let _ = cancel_tx.send(CancelMode::Hard);
                panic!(
                    "missing runtime request: {error}; result: {:?}",
                    worker.join().unwrap()
                );
            }
        };
        cancel_tx.send(CancelMode::Hard).unwrap();
        worker.join().unwrap().unwrap();
        let PersistenceRequest::Event { checkpoint, .. } = request else {
            panic!("expected ready event")
        };
        checkpoint
    })
}

#[test]
#[cfg(unix)]
fn evidence_receipt_matches_committed_ledger_for_every_completion_kind() {
    use std::sync::atomic::AtomicU8;
    for kind in 0..5 {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("store.sqlite");
        let mut store = Store::open(&database).unwrap();
        let session = store
            .create(CreateSession {
                prompt: "inspect".into(),
                directory: "/tmp".into(),
                model: None,
                idempotency_key: None,
            })
            .unwrap();
        let fence = store.begin_attempt(&session.id, 0).unwrap();
        let checkpoint = checkpoint(&store.get(&session.id).unwrap(), &fence.attempt_id);
        if kind == 4 {
            rusqlite::Connection::open(&database).unwrap().execute_batch(
                "CREATE TRIGGER reject_receipt BEFORE UPDATE ON tool_calls BEGIN SELECT RAISE(ABORT,'injected write failure'); END;"
            ).unwrap();
        }
        let (evidence, expected_outcome, expected_digest) = match kind {
            0 | 1 => {
                let reply = host::Reply {
                    version: host::VERSION,
                    attempt_id: fence.attempt_id.clone(),
                    call_id: "call".into(),
                    result: if kind == 0 {
                        Outcome::Ok {
                            output: serde_json::json!({"value":1}),
                        }
                    } else {
                        Outcome::Error {
                            code: host::ErrorCode::Io,
                            effect: Effect::Unknown,
                        }
                    },
                };
                let digest = tool_digest::output(&reply).unwrap();
                (
                    ToolEvidence::SharedReply(SharedToolReply::new(reply)),
                    if kind == 0 {
                        ToolOutcome::Success
                    } else {
                        ToolOutcome::Uncertain
                    },
                    Some(digest),
                )
            }
            2 => (ToolEvidence::FailedBeforeSpawn, ToolOutcome::Failed, None),
            _ => (ToolEvidence::Uncertain, ToolOutcome::Uncertain, None),
        };
        let (requests_tx, requests_rx) = mpsc::sync_channel(2);
        let (replies_tx, replies_rx) = mpsc::sync_channel(2);
        requests_tx
            .send(PersistenceRequest::ClaimTool {
                checkpoint,
                intent: ToolIntent {
                    call_id: "call".into(),
                    tool: "read".into(),
                    input: Input::Path(host::PathArgs {
                        path: "note".into(),
                    }),
                },
            })
            .unwrap();
        requests_tx
            .send(PersistenceRequest::FinishToolWithEvidence {
                checkpoint,
                evidence,
            })
            .unwrap();
        drop(requests_tx);
        let (cancellation, _cancel_rx) = mpsc::channel();
        let result = run(
            &mut store,
            &fence,
            requests_rx,
            replies_tx,
            WorkerControl {
                permissions: None,
                signals: RunControl::new(Arc::new(AtomicU8::new(0))),
                activate_on_ready: false,
                expired: Arc::new(AtomicBool::new(false)),
                cancellation,
            },
        );
        assert!(
            matches!(replies_rx.recv().unwrap(), PersistenceReply::ToolClaimed { checkpoint: id } if id == checkpoint)
        );
        if kind == 4 {
            assert!(result.is_err());
            assert!(
                matches!(replies_rx.recv().unwrap(), PersistenceReply::Rejected { checkpoint: id, .. } if id == checkpoint)
            );
            assert!(replies_rx.try_recv().is_err());
            let record = store.tool_call(&session.id, "call").unwrap().unwrap();
            assert_eq!(record.outcome, None);
            assert_eq!(record.output_digest, None);
            continue;
        }
        result.unwrap();
        let PersistenceReply::ToolFinishedEvidence {
            checkpoint: id,
            disposition,
            output_digest,
        } = replies_rx.recv().unwrap()
        else {
            panic!("expected evidence receipt")
        };
        assert_eq!(id, checkpoint);
        assert_eq!(output_digest, expected_digest);
        let record = store.tool_call(&session.id, "call").unwrap().unwrap();
        assert_eq!(record.output_digest, output_digest);
        assert_eq!(record.outcome, Some(expected_outcome));
        assert_eq!(
            disposition,
            match expected_outcome {
                ToolOutcome::Success => ToolDisposition::Success,
                ToolOutcome::Failed => ToolDisposition::NoEffectFailure,
                ToolOutcome::Uncertain => ToolDisposition::Uncertain,
            }
        );
    }
}

#[test]
#[cfg(unix)]
fn capture_ready_persists_without_opening_message_admission() {
    use std::sync::atomic::AtomicU8;
    for activate_on_ready in [false, true] {
        let mut store = Store::open(":memory:").unwrap();
        let session = store
            .create(CreateSession {
                prompt: "inspect".into(),
                directory: "/tmp".into(),
                model: None,
                idempotency_key: None,
            })
            .unwrap();
        let fence = store.begin_attempt(&session.id, 0).unwrap();
        let checkpoint = checkpoint(&store.get(&session.id).unwrap(), &fence.attempt_id);
        let signals = RunControl::new(Arc::new(AtomicU8::new(0)));
        let (requests_tx, requests_rx) = mpsc::sync_channel(2);
        let (replies_tx, replies_rx) = mpsc::sync_channel(2);
        requests_tx
            .send(PersistenceRequest::Event {
                checkpoint,
                event: AdapterEvent::Ready {
                    version: foundry_contracts::PROTOCOL_VERSION,
                    capabilities: vec![MESSAGE_ACK_CAPABILITY.into()],
                },
            })
            .unwrap();
        drop(requests_tx);
        let (cancellation, _cancel_rx) = mpsc::channel();
        run(
            &mut store,
            &fence,
            requests_rx,
            replies_tx,
            WorkerControl {
                permissions: None,
                signals: signals.clone(),
                activate_on_ready,
                expired: Arc::new(AtomicBool::new(false)),
                cancellation,
            },
        )
        .unwrap();
        assert!(
            matches!(replies_rx.recv().unwrap(), PersistenceReply::EventCommitted { checkpoint: id } if id == checkpoint)
        );
        assert_eq!(signals.accepting.load(Ordering::Acquire), activate_on_ready);
        assert_eq!(store.events(&session.id, 0, 10).unwrap().len(), 1);
    }
}
