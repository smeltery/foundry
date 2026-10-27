use super::*;
use crate::{AdapterMessage, MAX_MESSAGES_PER_ATTEMPT, delivery::Delivery};
use foundry_contracts::Lifecycle;
use std::{sync::mpsc, time::Duration};

fn fixture() -> (Process, Session) {
    let python = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    assert!(python.status.success());
    let mut spec = AdapterSpec::new(String::from_utf8(python.stdout).unwrap().trim());
    spec.args = [
        "-u",
        "-c",
        r#"
import json,sys
count=0
for line in sys.stdin:
    value=json.loads(line)
    if value['type']=='message':
        count+=1
        ack={k:value[k] for k in ('attempt_id','message_id','receipt')}
        ack['type']='message_accepted'
        print(json.dumps(ack),flush=True)
        print(json.dumps(ack),flush=True)
    else:
        print(json.dumps({'type':'text','attempt_id':'attempt','text':str(count)}),flush=True)
"#,
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    let session = Session {
        id: "session".into(),
        prompt: "fixture".into(),
        directory: std::env::temp_dir().to_string_lossy().into(),
        model: None,
        lifecycle: Lifecycle::Running,
        outcome: None,
        result: None,
        active_attempt_id: Some("attempt".into()),
        version: 1,
    };
    (Process::spawn(&spec, &session).unwrap(), session)
}

fn message(id: &str, attempt: &str) -> AdapterCommand {
    AdapterCommand::Message {
        attempt_id: attempt.into(),
        message_id: id.into(),
        text: "literal".into(),
        receipt: uuid::Uuid::new_v4().to_string(),
    }
}

fn assert_received_count(process: &Process, session: Session, count: usize) {
    process
        .send(&AdapterCommand::Start {
            version: 1,
            attempt_id: "attempt".into(),
            session,
        })
        .unwrap();
    let event = process.events.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(
        matches!(event, WireEvent::Event(AdapterEvent::Text { text, .. }) if text == count.to_string())
    );
}

#[test]
fn histories_retain_all_receipts_and_deny_next_message_without_pipe_enqueue() {
    let (mut process, session) = fixture();
    let mut delivery = Delivery::default();
    delivery.enabled = true;
    let (sender, receiver) = mpsc::channel();
    let first = "m".repeat(128);
    for index in 0..MAX_MESSAGES_PER_ATTEMPT {
        let id = if index == 0 {
            first.clone()
        } else {
            format!("m{index}")
        };
        sender
            .send(AdapterMessage {
                message_id: id.clone(),
                text: "literal".into(),
            })
            .unwrap();
        delivery.forward(&receiver, &process, "attempt").unwrap();
        for _ in 0..2 {
            let event = process.events.recv_timeout(Duration::from_secs(3)).unwrap();
            assert!(
                matches!(event, WireEvent::Event(AdapterEvent::MessageAccepted { message_id, .. }) if message_id == id)
            );
            delivery.validate_ack(&id).unwrap();
        }
    }
    let key = ("attempt".to_string(), first.clone());
    let original_receipt = process.receipts.lock().unwrap()[&key].clone();
    assert!(matches!(
        process.send(&message(&first, "attempt")),
        Err(RuntimeError::Invalid(_))
    ));
    assert!(matches!(
        process.send(&message("overflow", "attempt")),
        Err(RuntimeError::Invalid(_))
    ));
    assert_eq!(
        process.receipts.lock().unwrap().len(),
        MAX_MESSAGES_PER_ATTEMPT
    );
    assert_eq!(process.receipts.lock().unwrap()[&key], original_receipt);
    // Old duplicate receipts still validate at the limit; history was not evicted.
    assert!(matches!(
        validate_ingress(
            AdapterEvent::MessageAccepted {
                attempt_id: "attempt".into(),
                message_id: first.clone(),
                receipt: original_receipt,
            },
            &process.receipts
        ),
        WireEvent::Event(_)
    ));
    delivery.validate_ack(&first).unwrap();
    assert_received_count(&process, session, MAX_MESSAGES_PER_ATTEMPT);
    process.stop().unwrap();

    // Independent Delivery guard: a fresh Process has room, but cannot bypass it.
    let (mut fresh, session) = fixture();
    for id in [first, "overflow".into()] {
        sender
            .send(AdapterMessage {
                message_id: id,
                text: "literal".into(),
            })
            .unwrap();
        assert!(matches!(
            delivery.forward(&receiver, &fresh, "attempt"),
            Err(RuntimeError::Invalid(_))
        ));
    }
    assert_received_count(&fresh, session, 0);
    fresh.stop().unwrap();
}

#[test]
fn invalid_ids_and_receipts_never_reach_history_or_pipe() {
    let (mut process, session) = fixture();
    let mut delivery = Delivery::default();
    delivery.enabled = true;
    let (sender, receiver) = mpsc::channel();
    for invalid in [
        "".into(),
        " ".into(),
        "x\n".into(),
        "x\0".into(),
        "x".repeat(129),
        "é".repeat(65),
    ] {
        let mut command = message("id", "attempt");
        if let AdapterCommand::Message { receipt, .. } = &mut command {
            *receipt = invalid.clone();
        }
        assert!(matches!(
            process.send(&command),
            Err(RuntimeError::Invalid(_))
        ));
        assert!(process.receipts.lock().unwrap().is_empty());
        for (id, attempt) in [(invalid.as_str(), "attempt"), ("id", invalid.as_str())] {
            assert!(matches!(
                process.send(&message(id, attempt)),
                Err(RuntimeError::Invalid(_))
            ));
            sender
                .send(AdapterMessage {
                    message_id: id.into(),
                    text: "literal".into(),
                })
                .unwrap();
            assert!(matches!(
                delivery.forward(&receiver, &process, attempt),
                Err(RuntimeError::Invalid(_))
            ));
        }
    }
    assert!(process.receipts.lock().unwrap().is_empty());
    assert_received_count(&process, session.clone(), 0);
    let mut command = message("id", "attempt");
    if let AdapterCommand::Message { receipt, .. } = &mut command {
        *receipt = "r".repeat(128);
    }
    process.send(&command).unwrap();
    for _ in 0..2 {
        let event = process.events.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(
            matches!(event, WireEvent::Event(AdapterEvent::MessageAccepted { receipt, .. }) if receipt == "r".repeat(128))
        );
    }
    assert_received_count(&process, session, 1);
    process.stop().unwrap();
}
