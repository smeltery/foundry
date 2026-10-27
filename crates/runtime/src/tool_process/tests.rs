use super::*;

#[test]
fn cancellation_never_masks_cleanup_or_worker_failure() {
    for error in [ToolProcessError::Cleanup, ToolProcessError::Worker] {
        assert!(matches!(prioritize_cleanup(Err(error),true), Err(actual) if actual==error));
        // Exercise actual poll, including its finished-worker gate and join.
        let mut process = ToolProcess {
            stop: Arc::new(AtomicBool::new(true)),
            caller_stop: Arc::new(AtomicBool::new(false)),
            worker: Some(thread::spawn(move || Err(error))),
        };
        while !process.worker.as_ref().unwrap().is_finished() {
            thread::yield_now();
        }
        assert!(matches!(process.poll(),ToolPoll::Finished(Err(actual)) if actual==error));
        assert!(matches!(process.poll(), ToolPoll::Consumed));
    }
}

#[test]
fn shared_stop_suppresses_completed_reply_but_not_a_panicked_worker() {
    let reply = Reply {
        version: 1,
        attempt_id: "attempt".into(),
        call_id: "call".into(),
        result: foundry_tools::host::Outcome::Ok {
            output: serde_json::json!({"content":"ok"}),
        },
    };
    assert!(matches!(
        prioritize_cleanup(Ok(reply), true),
        Err(ToolProcessError::Stopped)
    ));
    let mut process = ToolProcess {
        stop: Arc::new(AtomicBool::new(false)),
        caller_stop: Arc::new(AtomicBool::new(true)),
        worker: Some(thread::spawn(|| panic!("fixture worker panic"))),
    };
    while !process.worker.as_ref().unwrap().is_finished() {
        thread::yield_now();
    }
    assert!(matches!(
        process.poll(),
        ToolPoll::Finished(Err(ToolProcessError::Worker))
    ));
}

#[test]
fn write_reply_cannot_claim_impossible_success_or_no_effect_io_failure() {
    let request: Request=serde_json::from_value(serde_json::json!({
        "version":1,"attempt_id":"attempt","call_id":"call","grants":["write.create"],
        "limits":{"request_bytes":524288,"read_bytes":262144,"write_bytes":2,"output_bytes":1048576,"entries":256},
        "tool":"write","input":{"path":"new","content":"abc","mode":"create"}
    })).unwrap();
    for result in [
        serde_json::json!({"status":"ok","output":{"bytes_written":3}}),
        serde_json::json!({"status":"error","code":"io","effect":"none"}),
    ] {
        let bytes=serde_json::to_vec(&serde_json::json!({"version":1,"attempt_id":"attempt","call_id":"call","result":result})).unwrap();
        assert!(matches!(
            wire::validate(&bytes, &request),
            Err(ToolProcessError::Protocol)
        ));
    }
    let bytes=br#"{"version":1,"attempt_id":"attempt","call_id":"call","result":{"status":"error","code":"io","effect":"unknown"}}"#;
    assert!(wire::validate(bytes, &request).is_ok());
}

#[cfg(unix)]
#[test]
fn external_reap_loses_reservation_and_cleanup_never_signals() {
    let spec = ToolProcessSpec {
        helper: std::path::PathBuf::from("/usr/bin/true"),
        directory: cap_std::fs::Dir::open_ambient_dir(
            std::env::temp_dir(),
            cap_std::ambient_authority(),
        )
        .unwrap(),
        timeout: Duration::from_secs(1),
    };
    for observe_first in [false, true] {
        let stop = Arc::new(AtomicBool::new(false));
        let mut owned = child::OwnedChild::spawn(&spec, &stop, &stop).unwrap();
        // Deliberately violate the public ownership contract without changing the
        // global SIGCHLD disposition or touching unrelated children.
        assert!(owned.process.wait().unwrap().success());
        if observe_first {
            assert!(matches!(owned.exited(), Err(ToolProcessError::Cleanup)));
        }
        assert!(matches!(owned.cleanup(), Err(ToolProcessError::Cleanup)));
        assert!(matches!(owned.cleanup(), Err(ToolProcessError::Cleanup)));
        assert_eq!(owned.signals_sent, 0);
    }
}
