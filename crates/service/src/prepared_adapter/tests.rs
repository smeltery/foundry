use super::*;
use foundry_contracts::{
    CreateSession, ModelConfig,
    permission::{PendingMode, PermissionDecision},
};
use serde_json::json;

fn selection(directory: &std::path::Path) -> AdapterSelection {
    serde_json::from_value(json!({
        "provider":"fixture", "model":"small", "effort":"high",
        "directory":directory.to_str().unwrap(), "workspace_identity":"11".repeat(32),
        "profile":"explicit-core-v1", "profile_version":1, "permission_profile":"current",
        "resource_policy":"explicit_system_v1", "resource_identity":"22".repeat(32),
        "tool_catalog":"core_workspace_v1", "tools":["read"], "policy_identity":"33".repeat(32),
        "generation":1, "max_output_bytes":4096, "max_pending_text_bytes":4096,
    }))
    .unwrap()
}

fn private(value: &str) -> AdapterPrivateInputs {
    AdapterPrivateInputs {
        credential: AdapterCredential::ApiKey {
            provider: "fixture".into(),
            value: value.into(),
        },
        system_prompt: "private-system-canary".into(),
        agent_directory: "/tmp/explicit-agent-directory".into(),
    }
}

fn tools() -> AdapterToolSettings {
    AdapterToolSettings {
        helper: "/usr/bin/true".into(),
        grants: vec![Grant::Read],
        missing_grant: PermissionDecision::Pending,
        pending_mode: PendingMode::ActionRequired,
        request_timeout: Duration::from_secs(2),
        helper_timeout: Duration::from_secs(2),
        limits: host::HostLimits {
            request_bytes: host::MAX_REQUEST_BYTES,
            read_bytes: 4096,
            write_bytes: 4096,
            output_bytes: host::MAX_REPLY_BYTES,
            entries: 10,
        },
    }
}

#[test]
fn private_inputs_preserve_their_codec_without_changing_public_identity() {
    let root = tempfile::tempdir().unwrap();
    let selection = selection(root.path());
    let expected = selection.digest().unwrap();
    for value in ["private-key-one", "private-key-two"] {
        let body: serde_json::Value =
            serde_json::from_slice(&private(value).encode(&selection).unwrap()).unwrap();
        assert_eq!(body["version"], 1);
        assert_eq!(body["private"]["credential"]["kind"], "api_key");
        assert_eq!(body["private"]["credential"]["value"], value);
        let public: AdapterSelection = serde_json::from_value(body["selection"].clone()).unwrap();
        assert!(public.digest().unwrap() == expected);
        let encoded = serde_json::to_string(&public).unwrap();
        assert!(!encoded.contains(value) && !encoded.contains("private-system-canary"));
    }
}

#[test]
fn private_configuration_and_tool_ceiling_reject_invalid_composition() {
    let root = tempfile::tempdir().unwrap();
    let selected = selection(root.path());
    let mut wrong_provider = private("key");
    wrong_provider.credential = AdapterCredential::ApiKey {
        provider: "other".into(),
        value: "key".into(),
    };
    assert!(wrong_provider.encode(&selected).is_err());
    assert!(private("").encode(&selected).is_err());
    assert!(private(&"x".repeat(16_385)).encode(&selected).is_err());
    let mut relative = private("key");
    relative.agent_directory = "relative".into();
    assert!(relative.encode(&selected).is_err());
    let mut tool_settings = tools();
    tool_settings.grants = vec![Grant::Create];
    assert!(
        PreparedLocalAdapter::new(
            AdapterSpec::new("/usr/bin/true"),
            selected.clone(),
            private("key"),
            tool_settings,
        )
        .is_err()
    );
    let mut oversized = private(&"\u{0001}".repeat(16_384));
    oversized.system_prompt = "\u{0001}".repeat(16_384);
    assert!(
        PreparedLocalAdapter::new(
            AdapterSpec::new("/usr/bin/true"),
            selected,
            oversized,
            tools(),
        )
        .is_err()
    );
}

#[test]
fn stored_selection_mismatch_is_rejected_before_attempt_admission() {
    for field in ["provider", "model", "effort", "directory"] {
        let root = tempfile::tempdir().unwrap();
        let selected = selection(root.path());
        let database = root.path().join("store.db");
        let mut service = Service::open(&database).unwrap();
        let mut request = CreateSession {
            prompt: "work".into(),
            directory: selected.directory.clone(),
            model: Some(ModelConfig {
                provider: "fixture".into(),
                model: "small".into(),
                effort: Some("high".into()),
            }),
            idempotency_key: None,
        };
        let model = request.model.as_mut().unwrap();
        match field {
            "provider" => model.provider = "other".into(),
            "model" => model.model = "other".into(),
            "effort" => model.effort = None,
            _ => request.directory = root.path().join("other").to_string_lossy().into(),
        }
        let session = service.create(request).unwrap();
        let prepared = PreparedLocalAdapter::new(
            AdapterSpec::new("/usr/bin/true"),
            selected,
            private("key"),
            tools(),
        )
        .unwrap();
        assert!(
            service
                .run_prepared(
                    &session.id,
                    session.version,
                    prepared,
                    Arc::new(AtomicU8::new(0))
                )
                .is_err()
        );
        assert_eq!(service.get(&session.id).unwrap(), session);
        assert!(service.events(&session.id, 0, 100).unwrap().is_empty());
        let connection = rusqlite::Connection::open(database).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM attempts", [], |row| row
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn prepared_revocation_handle_survives_moving_the_selection() {
    let root = tempfile::tempdir().unwrap();
    let prepared = PreparedLocalAdapter::new(
        AdapterSpec::new("/usr/bin/true"),
        selection(root.path()),
        private("key"),
        tools(),
    )
    .unwrap();
    let revoke = prepared.revocation();
    let owner = std::thread::spawn(move || {
        let identity = prepared.tools.policy.identity();
        prepared.tools.permissions.check_policy(&identity).unwrap();
        (prepared, identity)
    });
    let (prepared, identity) = owner.join().unwrap();
    revoke.invalidate();
    assert!(prepared.tools.permissions.check_policy(&identity).is_err());
    revoke.invalidate();
    assert!(prepared.tools.permissions.check_policy(&identity).is_err());
}

#[test]
fn native_parts_retain_configuration_and_the_original_revocation() {
    let root = tempfile::tempdir().unwrap();
    let selected = selection(root.path());
    let digest = *selected.digest().unwrap().as_bytes();
    let prepared = PreparedLocalAdapter::new(
        AdapterSpec::new("/usr/bin/true"),
        selected,
        private("key"),
        tools(),
    )
    .unwrap();
    let revoke = prepared.revocation();
    let parts = prepared.into_capture_parts().unwrap();
    assert_eq!(parts.permissions.configuration_digest(), digest);
    parts
        .permissions
        .check_policy(&parts.policy.identity())
        .unwrap();
    revoke.invalidate();
    assert!(
        parts
            .permissions
            .check_policy(&parts.policy.identity())
            .is_err()
    );
}

#[test]
fn native_transfer_rejects_revoked_or_shared_permissions() {
    let root = tempfile::tempdir().unwrap();
    for shared in [false, true] {
        let prepared = PreparedLocalAdapter::new(
            AdapterSpec::new("/usr/bin/true"),
            selection(root.path()),
            private("key"),
            tools(),
        )
        .unwrap();
        let retained = if shared {
            Some(Arc::clone(&prepared.tools.permissions))
        } else {
            prepared.revocation().invalidate();
            None
        };
        assert!(prepared.into_capture_parts().is_err());
        drop(retained);
    }
}
