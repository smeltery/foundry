use super::*;
use foundry_contracts::context::{ContextDigest, ContextId};
#[test]
fn full_command_queue_retains_exact_tail_and_single_latch_generation() {
    let python = Command::new("python3")
        .args(["-c", "import sys;print(sys.executable)"])
        .output()
        .unwrap();
    let mut spec = AdapterSpec::new(String::from_utf8(python.stdout).unwrap().trim());
    spec.args = ["-u", "-c", "import time;time.sleep(30)"]
        .into_iter()
        .map(Into::into)
        .collect();
    let scope = LoadingScope {
        restore_id: ContextId::new("restore").unwrap(),
        load_id: ContextReceipt::new(uuid::Uuid::new_v4().to_string()).unwrap(),
    };
    let (mut process, events) = Process::spawn_loading(
        &spec,
        LoadingProcessSpec {
            directory: &std::env::temp_dir(),
            scope: scope.clone(),
        },
    )
    .unwrap();
    let (sender, receiver) = sync_channel(1);
    // Isolate queue scheduling from OS pipe throughput; the actual process remains owned.
    process.commands = Some(sender.clone());
    sender
        .try_send(InputFrame {
            bytes: b"occupied".to_vec(),
        })
        .ok()
        .unwrap();
    let id = |s| ContextId::new(s).unwrap();
    let identity = ContinuationIdentity {
        attempt_id: id("attempt"),
        continuation_id: id("continue"),
        input_id: id("input"),
        input_sequence: 1,
        input_digest: ContextDigest::of(b"text"),
        input_bytes: 4,
        authorization_binding: ContextDigest::of(b"grant"),
    };
    let receipt = ContextReceipt::new(uuid::Uuid::new_v4().to_string()).unwrap();
    let frame =
        LoadingFrame::continuation(scope.clone(), identity.clone(), "text", receipt.clone())
            .unwrap();
    let command = process.prepare_continuation(&frame).unwrap();
    let Err(LoadingSendError::Full(command)) = process.try_send_loading(command) else {
        panic!()
    };
    assert!(matches!(
        *process.latch.as_ref().unwrap().lock().unwrap(),
        Latch::AwaitAccepted { .. }
    ));
    assert!(process.prepare_continuation(&frame).is_err());
    assert_eq!(receiver.recv().unwrap().bytes, b"occupied");
    assert!(process.try_send_loading(command).is_ok());
    let sent = receiver.recv().unwrap().bytes;
    let decoded = foundry_contracts::context_v2::decode_frame(&sent[..sent.len() - 1]).unwrap();
    assert!(decoded.receipt() == Some(&receipt));
    assert_eq!(decoded.text(), Some("text"));
    assert!(receiver.try_recv().is_err());
    let accepted = LoadingFrame::accepted(scope.clone(), identity, receipt).unwrap();
    let mut ready = true;
    assert!(
        decode(
            &foundry_contracts::context_v2::encode_frame(&accepted).unwrap(),
            &scope,
            &mut ready,
            process.latch.as_ref().unwrap(),
            &process.receipts
        )
        .is_ok()
    );
    process.admit_continuation("attempt", false).unwrap();
    assert!(process.admit_continuation("attempt", false).is_err());
    process.stop_context_checked(&events).unwrap();
}
