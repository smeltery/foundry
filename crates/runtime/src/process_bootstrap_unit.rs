use super::*;
use std::time::Duration;
fn gate() -> (Prelude, Gate) {
    let spec = AdapterSpec::new("unused").with_private_bootstrap(
        PrivateBootstrap::new(
            BootstrapIdentity {
                schema_version: 1,
                selection_digest: ContextDigest::of(b"selection"),
            },
            b"secret".to_vec(),
        )
        .unwrap(),
    );
    let (prelude, gate) = prepare(&spec).unwrap();
    (prelude.unwrap(), gate.unwrap())
}
fn ack(g: &Gate) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"type":"adapter_configured","version":1,
        "selection_digest":g.identity.selection_digest,"challenge":g.challenge}))
    .unwrap()
}
#[test]
fn private_ack_waits_for_writer_completion_without_exposing_readiness() {
    let (prelude, mut gate) = gate();
    let bytes = ack(&gate);
    let state = Arc::clone(&gate.state);
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let worker = scope.spawn(|| gate.acknowledge(&bytes, &stop));
        thread::sleep(Duration::from_millis(30));
        assert!(!state.ready.load(Ordering::Acquire));
        state.written.store(1, Ordering::Release);
        worker.join().unwrap().unwrap();
    });
    assert!(gate.acknowledged);
    assert!(!gate.state.ready.load(Ordering::Acquire));
    let value: serde_json::Value = serde_json::from_slice(&prelude.bytes).unwrap();
    assert_eq!(value["body_base64"], "c2VjcmV0");
    assert!(
        String::from_utf8(prelude.bytes)
            .unwrap()
            .ends_with(&format!("\"challenge\":\"{}\"}}\n", gate.challenge))
    );
}
#[test]
fn private_ack_wait_observes_stop_and_late_writer_failure() {
    let (_, mut gate) = gate();
    let bytes = ack(&gate);
    gate.state.written.store(2, Ordering::Release);
    assert!(matches!(
        gate.acknowledge(&bytes, &AtomicBool::new(false)),
        Err(RuntimeError::Protocol("bootstrap write failed"))
    ));
    gate.state.written.store(0, Ordering::Release);
    assert!(matches!(
        gate.acknowledge(&bytes, &AtomicBool::new(true)),
        Err(RuntimeError::Protocol("bootstrap stopped"))
    ));
    assert!(!gate.acknowledged);
}
#[test]
fn private_deadline_is_created_before_spawn_and_never_restarted_by_ack() {
    let spec = AdapterSpec::new("unused").with_private_bootstrap(
        PrivateBootstrap::new(
            BootstrapIdentity {
                schema_version: 1,
                selection_digest: ContextDigest::of(b"x"),
            },
            vec![1],
        )
        .unwrap(),
    );
    let mut spec = spec;
    spec.ready_timeout = Duration::from_millis(20);
    let (_, g) = prepare(&spec).unwrap();
    let mut g = g.unwrap();
    let bytes = ack(&g);
    thread::sleep(Duration::from_millis(30));
    g.state.written.store(1, Ordering::Release);
    assert!(matches!(
        g.acknowledge(&bytes, &AtomicBool::new(false)),
        Err(RuntimeError::ReadyTimeout)
    ));
}
