use foundry_contracts::{
    CreateSession,
    context::{ContextDigest, ContextFormat, ContextId},
};
use foundry_runtime::{
    AdapterSpec,
    coordinated::{
        CaptureExit, FreshCaptureOptions,
        capture::{OwnerValidationAttestation, ProfileBinding, StoredFormat},
        run_fresh_capture,
    },
};
use foundry_service::{
    ServiceOptions, fresh_capture_owner::*, native_publication::*,
    native_validation::NativeValidationFailure, restoration_owner::RestoreOwnerOptions,
};
use foundry_store::{Store, witness::*};
use std::{
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

// Synthetic semantic fixture only. The parent separately supplies the real codec.
struct Comparator;
impl NativePublicationComparator for Comparator {
    fn compare(
        &self,
        job: &NativePublicationJob,
    ) -> Result<NativePublicationComparison, NativeValidationFailure> {
        let runtime = job.runtime_job();
        let cut = runtime.cut();
        let identity = cut.identity();
        let profile = cut.profile();
        let snapshot = job.snapshot();
        if job.baseline().is_some() || !snapshot.tools.is_empty() || !snapshot.messages.is_empty() {
            return Err(NativeValidationFailure::Unsupported);
        }
        let mut witness =
            WitnessBodyV1::decode(include_bytes!("../../store/tests/fixtures/witness-v1.json"))
                .unwrap();
        witness.records.truncate(1);
        witness.prefix.truncate(1);
        witness.mappings.truncate(1);
        witness.boundaries.clear();
        witness.header.session_id = identity.session_id.clone();
        witness.header.source_attempt_id = identity.attempt_id.clone();
        witness.header.source_request_id = identity.request_id.clone();
        witness.header.parent = None;
        witness.header.cut = snapshot.cut.clone();
        witness.header.stored_format = foundry_store::ContextFormat {
            format_id: profile.stored.format_id.clone(),
            format_version: profile.stored.format_version,
            profile_id: profile.stored.profile_id.clone(),
            binding_id: profile.stored.binding_id.clone(),
            binding_version: profile.stored.binding_version.clone(),
        };
        witness.header.wire_format = WitnessWireFormat {
            profile: profile.wire.profile.as_str().into(),
            format_version: profile.wire.format_version.as_str().into(),
            producer_build: profile.wire.producer_build.as_str().into(),
        };
        witness.header.configuration_digest = profile.configuration_digest;
        witness.header.payload_digest = raw(ContextDigest::of(runtime.candidate().payload()));
        witness.header.payload_length = runtime.candidate().payload().len() as u64;
        witness.header.validation_binding = raw(runtime.binding_digest());
        witness.header.terminal_digest = raw(runtime.terminal().binding_digest());
        witness.records[0] = LogicalRecord::Prompt(PromptWitness {
            session_id: identity.session_id.clone(),
            attempt_id: identity.attempt_id.clone(),
            text_digest: snapshot.prompt_digest,
            text_bytes: snapshot.prompt_bytes,
        });
        Ok(NativePublicationComparison {
            input_digest: job.input_digest(),
            registration_digest: job.registration_digest(),
            candidate_digest: ContextDigest::of(runtime.candidate().payload()),
            attestation: OwnerValidationAttestation {
                validator_id: ContextId::new("fixture").unwrap(),
                validator_version: ContextId::new("1").unwrap(),
                completed_turn_id: runtime.candidate().coverage().completed_turn_id.clone(),
            },
            witness,
        })
    }
}
fn raw(value: ContextDigest) -> [u8; 32] {
    let text = value.to_hex();
    std::array::from_fn(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).unwrap())
}
fn configuration() -> PublicationConfiguration {
    let id = |value| ContextId::new(value).unwrap();
    PublicationConfiguration {
        profile: ProfileBinding {
            stored: StoredFormat {
                format_id: "stored-format".into(),
                format_version: 5,
                profile_id: "stored-profile".into(),
                binding_id: "binding".into(),
                binding_version: "binding-v7".into(),
            },
            wire: ContextFormat {
                profile: id("wire-profile"),
                format_version: id("wire-v3"),
                producer_build: id("fixture-build"),
            },
            configuration_digest: std::array::from_fn(|i| i as u8),
        },
        policy: b"fixture-policy".to_vec(),
        associations: b"fixture-independent-mappings".to_vec(),
        validator: NativePublicationRegistration {
            identity: ContextDigest::of(b"fixture"),
            timeout: Duration::from_secs(2),
            comparator: Arc::new(Comparator),
        },
    }
}
fn receive(owner: &mut FreshCaptureOwner) -> FreshCaptureReply {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(reply) = owner.try_recv().unwrap() {
            return reply;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn fresh_owner_persists_cut_and_commits_full_witness_only_after_real_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let mut store = Store::open(&path).unwrap();
    let session = store
        .create(CreateSession {
            prompt: "original fixture prompt".into(),
            directory: dir.path().display().to_string(),
            model: None,
            idempotency_key: None,
        })
        .unwrap();
    // Counters do not establish freshness; the Store checks atomic history absence.
    let database = rusqlite::Connection::open(&path).unwrap();
    database
        .execute("UPDATE sessions SET body=json_set(body,'$.version',7)", [])
        .unwrap();
    database
        .execute("UPDATE session_work SET revision=11", [])
        .unwrap();
    let mut owner = FreshCaptureOwner::start(
        path,
        session.id.clone(),
        configuration(),
        RestoreOwnerOptions {
            lease: ServiceOptions {
                lease_duration: Duration::from_secs(4),
                poll_interval: Duration::from_millis(25),
            },
            cleanup_timeout: Duration::from_secs(2),
        },
    )
    .unwrap();
    let FreshCaptureReply::Ready(ports) = receive(&mut owner) else {
        panic!()
    };
    assert_eq!(ports.session.version, 8);
    let cancellation = owner.take_cancellation().unwrap();
    let (_send, messages) = mpsc::channel();
    let mut spec = AdapterSpec::new("/usr/bin/python3");
    spec.args = vec![
        "-u".into(),
        "-c".into(),
        include_str!("context_capture/adapter.py").into(),
        dir.path().as_os_str().into(),
        "\"fixture terminal\"".into(),
    ];
    spec.run_timeout = Duration::from_secs(5);
    spec.cancel_timeout = Duration::from_millis(100);
    let result = run_fresh_capture(
        &spec,
        &ports.session,
        &ports.attempt_id,
        &cancellation,
        &messages,
        ports.persistence,
        FreshCaptureOptions {
            profile: ports.profile,
            owner: ports.owner,
            policy: None,
            phase_timeout: Duration::from_secs(3),
        },
    )
    .unwrap();
    let CaptureExit::Prepared(prepared) = result else {
        panic!()
    };
    assert!(store.checkpoint_head(&session.id).unwrap().is_none());
    owner.finish_capture(*prepared).unwrap();
    let FreshCaptureReply::Published(result) = receive(&mut owner) else {
        panic!()
    };
    let witness = store
        .read_checkpoint_witness(&session.id, &result.checkpoint.id)
        .unwrap();
    assert_eq!(witness.body().header.cut, result.checkpoint.cut);
    assert_eq!(witness.body().records.len(), 1);
    assert!(store.get(&session.id).unwrap().active_attempt_id.is_none());
    let report = owner.shutdown();
    report.operation.unwrap();
    report.release.unwrap();
}

fn start_for_test(
    path: std::path::PathBuf,
    id: String,
    configuration: PublicationConfiguration,
) -> FreshCaptureOwner {
    FreshCaptureOwner::start(
        path,
        id,
        configuration,
        RestoreOwnerOptions {
            lease: ServiceOptions {
                lease_duration: Duration::from_secs(4),
                poll_interval: Duration::from_millis(25),
            },
            cleanup_timeout: Duration::from_secs(2),
        },
    )
    .unwrap()
}
fn expect_initial_error(owner: &mut FreshCaptureOwner) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match owner.try_recv() {
            Err(_) => return,
            Ok(None) => {}
            Ok(Some(_)) => panic!("ports must not escape"),
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
}
fn create_initial(store: &mut Store) -> String {
    store
        .create(CreateSession {
            prompt: "fresh ownership".into(),
            directory: "/tmp".into(),
            model: None,
            idempotency_key: None,
        })
        .unwrap()
        .id
}
#[test]
fn fresh_actor_rejects_eventless_history_even_with_reset_version_and_no_head() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = create_initial(&mut store);
    let (fence, _) = store.begin_fresh_attempt(&session, 0).unwrap().into_parts();
    store
        .finish_attempt(&fence, foundry_contracts::Lifecycle::Suspended, None, None)
        .unwrap();
    let database = rusqlite::Connection::open(&path).unwrap();
    database
        .execute(
            "UPDATE sessions SET body=json_set(body,'$.lifecycle','queued','$.version',0)",
            [],
        )
        .unwrap();
    assert!(store.checkpoint_head(&session).unwrap().is_none());
    assert!(store.events(&session, 0, 10).unwrap().is_empty());
    let before = store.get(&session).unwrap();
    let mut owner = start_for_test(path, session.clone(), configuration());
    expect_initial_error(&mut owner);
    let report = owner.shutdown();
    assert!(report.operation.is_err());
    assert!(report.release.unwrap().is_none());
    assert_eq!(store.get(&session).unwrap(), before);
    assert_eq!(
        database
            .query_row("SELECT COUNT(*) FROM attempts", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        1
    );
}
#[test]
fn failed_publication_setup_retains_admitted_fence_and_reports_release_failure() {
    for prevent_release in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let session = create_initial(&mut store);
        let database = rusqlite::Connection::open(&path).unwrap();
        if prevent_release {
            database.execute_batch("CREATE TRIGGER prevent_attempt_release BEFORE UPDATE ON attempts BEGIN SELECT RAISE(ABORT,'fixture release failure'); END").unwrap();
        }
        let mut invalid = configuration();
        invalid.validator.timeout = Duration::ZERO;
        let mut owner = start_for_test(path, session.clone(), invalid);
        expect_initial_error(&mut owner);
        let report = owner.shutdown();
        assert!(report.operation.is_err());
        assert_eq!(report.release.is_err(), prevent_release);
        assert_eq!(
            store.get(&session).unwrap().active_attempt_id.is_some(),
            prevent_release
        );
        assert_eq!(
            database
                .query_row("SELECT COUNT(*) FROM attempts", [], |row| row
                    .get::<_, u64>(0))
                .unwrap(),
            1
        );
        assert!(store.checkpoint_head(&session).unwrap().is_none());
        let version = store.get(&session).unwrap().version;
        assert!(store.begin_fresh_attempt(&session, version).is_err());
    }
}

#[test]
fn concurrent_fresh_owners_and_lost_ready_cannot_reissue_initial_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = create_initial(&mut store);
    let mut owners = [
        start_for_test(path.clone(), session.clone(), configuration()),
        start_for_test(path.clone(), session.clone(), configuration()),
    ];
    let mut outcomes = [None, None];
    let deadline = Instant::now() + Duration::from_secs(3);
    while outcomes.iter().any(Option::is_none) {
        for (owner, outcome) in owners.iter_mut().zip(&mut outcomes) {
            if outcome.is_some() {
                continue;
            }
            match owner.try_recv() {
                Ok(Some(FreshCaptureReply::Ready(ports))) => {
                    assert_eq!(
                        ports.session.active_attempt_id.as_deref(),
                        Some(ports.attempt_id.as_str())
                    );
                    // Discard delivery endpoints without starting a runtime process.
                    drop(ports);
                    *outcome = Some(true);
                }
                Err(_) => *outcome = Some(false),
                Ok(None) => {}
                Ok(Some(_)) => panic!("unexpected publication"),
            }
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(outcomes.iter().filter(|v| **v == Some(true)).count(), 1);
    for (owner, delivered) in owners.into_iter().zip(outcomes) {
        let report = owner.shutdown();
        assert_eq!(report.release.is_err(), delivered == Some(true));
    }
    let database = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        database
            .query_row("SELECT COUNT(*) FROM attempts", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        1
    );
    assert!(store.events(&session, 0, 10).unwrap().is_empty());
    let mut retry = start_for_test(path, session, configuration());
    expect_initial_error(&mut retry);
    assert!(retry.shutdown().operation.is_err());
    assert_eq!(
        database
            .query_row("SELECT COUNT(*) FROM attempts", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        1
    );
}
