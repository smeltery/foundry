use super::{
    harness::Harness,
    terminal_tests::{attestation, validation},
    *,
};
use crate::coordinated::{capture::*, engine::EngineExit, *};
use foundry_contracts::AdapterEvent;

#[test]
fn instruction_receipt_and_both_tool_paths_enter_exact_validation_trace() {
    for kind in ["instruction", "denied", "helper"] {
        let mut h = if kind == "instruction" {
            Harness::new("settle", Duration::from_secs(2))
        } else {
            Harness::with_helper(
                "tool_terminal",
                Duration::from_secs(2),
                (kind == "helper").then_some("ok"),
            )
        };
        if kind == "helper" {
            let (checkpoint, evidence) = super::tool_tests::finish_request(&h);
            assert!(matches!(evidence, ToolEvidence::SharedReply(_)));
            assert!(h.commands().iter().all(|c| c["type"] != "tool_result"));
            h.replies
                .send(PersistenceReply::ToolFinishedEvidence {
                    checkpoint,
                    disposition: ToolDisposition::Success,
                    output_digest: Some([9; 32]),
                })
                .unwrap();
        } else {
            h.ready();
            let (correlation, _) = h.activation();
            h.activate(correlation);
            if kind == "denied" {
                let (job, _) = h.event();
                h.commit(job);
            }
        }
        let (text, event) = h.event();
        assert!(matches!(event, AdapterEvent::Text { .. }));
        h.commit(text);
        if kind == "instruction" {
            h.messages
                .send(crate::AdapterMessage {
                    message_id: "m".into(),
                    text: "exact instruction".into(),
                })
                .unwrap();
            let (receipt, event) = h.event();
            assert!(matches!(event, AdapterEvent::MessageAccepted { .. }));
            assert!(
                h.owner
                    .requests
                    .recv_timeout(Duration::from_millis(40))
                    .is_err()
            );
            h.commit(receipt);
        }
        let (terminal, _) = h.event();
        let CaptureRequest::FreezeAdmission { correlation } = h
            .owner
            .requests
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
        else {
            panic!("freeze")
        };
        let (correlation, job) = validation(&h, terminal, correlation);
        assert_eq!(job.trace().entries().len(), 2);
        let entry = &job.trace().entries()[1];
        match kind {
            "instruction" => {
                assert!(entry.instruction_is_durable());
                assert_eq!(entry.bytes(), b"exact instruction");
            }
            "denied" => assert!(matches!(
                entry.kind(),
                TraceKind::ToolResult(ResultProvenance::Denied)
            )),
            _ => assert!(
                matches!(entry.kind(), TraceKind::ToolResult(ResultProvenance::PersistedHelper { ledger_output_digest }) if *ledger_output_digest == [9;32])
            ),
        }
        if kind != "instruction" {
            let body = std::fs::read(h.root.join("result-body")).unwrap();
            assert_eq!(entry.bytes(), body);
            assert!(entry.wire_digest() == ContextDigest::of(&body));
        }
        let verified = job.accept_from_trusted_owner(attestation()).unwrap();
        h.owner
            .replies
            .send(CaptureReply::CandidateVerified {
                correlation,
                verified: Box::new(verified),
            })
            .ok()
            .unwrap();
        assert!(matches!(h.finish_owned(), Ok(EngineExit::Prepared(_))));
    }
}
