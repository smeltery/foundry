//! Real Rust/compiled TypeScript interoperability, without a model provider.
use foundry_contracts::{Outcome, Session, context::ContextDigest};
use foundry_runtime::{
    AdapterSpec, RunResult,
    bootstrap::{BootstrapIdentity, PrivateBootstrap},
    run_adapter,
};
use std::{path::PathBuf, sync::mpsc, time::Duration};

#[test]
#[ignore = "requires built adapter ESM and explicit FOUNDRY_TEST_BUN"]
fn compiled_typescript_consumes_private_bootstrap_before_start() {
    let bun = PathBuf::from(std::env::var_os("FOUNDRY_TEST_BUN").expect("explicit Bun executable"));
    assert!(bun.is_absolute() && bun.is_file());
    let root = std::env::temp_dir().canonicalize().unwrap();
    let session: Session = serde_json::from_value(serde_json::json!({
        "id":"session", "prompt":"verify transport", "directory":root,
        "model":null, "lifecycle":"running", "outcome":null, "result":null,
        "active_attempt_id":"attempt", "version":1,
    }))
    .unwrap();
    let digest = ContextDigest::of(b"public-fixture-selection");
    let body = (0..65_536).map(|index| (index % 256) as u8).collect();
    let bootstrap = PrivateBootstrap::new(
        BootstrapIdentity {
            schema_version: 1,
            selection_digest: digest,
        },
        body,
    )
    .unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/adapter/test/configured-interop-fixture.mjs")
        .canonicalize()
        .unwrap();
    let mut spec = AdapterSpec::new(bun).with_private_bootstrap(bootstrap);
    spec.args.push(fixture.into_os_string());
    spec.run_timeout = Duration::from_secs(5);
    let (_sender, cancel) = mpsc::channel();
    let mut events = Vec::new();
    let result = run_adapter(&spec, &session, "attempt", &cancel, |event| {
        events.push(serde_json::to_value(event).unwrap());
    })
    .unwrap();
    assert_eq!(
        result,
        RunResult::Completed {
            outcome: Outcome::ActionRequired,
            result: "transport verified".into(),
        }
    );
    let output = serde_json::to_string(&events).unwrap();
    assert!(output.contains("configuration accepted"));
    assert!(!output.contains("adapter_configured") && !output.contains("body_base64"));
    // Reusing the specification cannot silently become an unconfigured launch.
    assert!(run_adapter(&spec.clone(), &session, "attempt", &cancel, |_| {}).is_err());
}
