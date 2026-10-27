#![cfg(unix)]
mod persistence_support;
use foundry_network::persistence::{MAX_MEMBERSHIP_BYTES, MembershipStore, PersistenceError};
use iroh::SecretKey;
use persistence_support::*;
use serde_json::{Value, json};

fn mutate(fixture: &Fixture, mutation: impl FnOnce(&mut Value)) {
    let connection = fixture.connection();
    let bytes: Vec<u8> = connection
        .query_row("SELECT payload FROM projection", [], |r| r.get(0))
        .unwrap();
    let mut value: Value = serde_json::from_slice(&bytes).unwrap();
    mutation(&mut value);
    connection
        .execute(
            "UPDATE projection SET payload=?1",
            [serde_json::to_vec(&value).unwrap()],
        )
        .unwrap();
}
#[test]
fn load_checks_team_founder_and_every_profile_field() {
    let fixture = Fixture::new();
    let mut store = fixture.initialized();
    assert!(matches!(
        store.load("other", fixture.founder.public(), &fixture.profile, NOW),
        Err(PersistenceError::IdentityMismatch)
    ));
    assert!(matches!(
        store.load(
            "team",
            SecretKey::generate().public(),
            &fixture.profile,
            NOW
        ),
        Err(PersistenceError::IdentityMismatch)
    ));
    for field in [
        "team_alpn",
        "join_alpn",
        "protocol",
        "signing_prefix",
        "invitation_endpoint_context",
        "invitation_secret_context",
        "ticket_prefixes",
    ] {
        let mut value = serde_json::to_value(&fixture.profile).unwrap();
        value[field] = match field {
            "team_alpn" | "join_alpn" => json!([99]),
            "protocol" => json!(2),
            "ticket_prefixes" => json!(["different-"]),
            _ => json!("different/"),
        };
        let profile = serde_json::from_value(value).unwrap();
        assert!(matches!(
            store.load("team", fixture.founder.public(), &profile, NOW),
            Err(PersistenceError::IdentityMismatch)
        ));
    }
}
#[test]
fn corrupted_record_is_rejected_instead_of_filtered_on_reload() {
    for kind in ["metadata", "members", "admissions"] {
        let fixture = Fixture::new();
        let mut store = fixture.initialized();
        mutate(&fixture, |value| {
            let record = if kind == "metadata" {
                &mut value["snapshot"][kind]
            } else {
                &mut value["snapshot"][kind][fixture.founder.public().to_string()]
            };
            record["sig"] = json!("00".repeat(64));
        });
        assert!(matches!(
            store.load("team", fixture.founder.public(), &fixture.profile, NOW),
            Err(PersistenceError::Corrupt)
        ));
        assert!(
            store
                .merge(
                    1,
                    fixture.founder.public(),
                    &fixture.update(2),
                    &fixture.profile,
                    NOW
                )
                .is_err()
        );
    }
}
#[test]
fn full_document_unknown_fields_truncation_and_oversize_fail() {
    for mode in 0..4 {
        let fixture = Fixture::new();
        let mut store = fixture.initialized();
        match mode {
            0 => mutate(&fixture, |v| v["snapshot"]["invites"] = json!({})),
            1 => {
                fixture
                    .connection()
                    .execute("UPDATE projection SET payload=?1", [b"{".to_vec()])
                    .unwrap();
            }
            2 => {
                fixture
                    .connection()
                    .execute(
                        "UPDATE projection SET payload=zeroblob(?1)",
                        [MAX_MEMBERSHIP_BYTES + 1],
                    )
                    .unwrap();
            }
            _ => mutate(&fixture, |v| v["snapshot"]["members"] = json!({})),
        }
        let result = store.load("team", fixture.founder.public(), &fixture.profile, NOW);
        if mode == 3 {
            assert!(result.is_ok());
        }
        // No current activity is a valid projection.
        else {
            assert!(result.is_err());
        }
    }
}
#[test]
fn oversized_incoming_and_wrong_profile_initialization_write_nothing() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let state = fixture.state(&fixture.initial());
    let mut profile = fixture.profile.clone();
    profile.signing_prefix = "foreign/".into();
    assert!(matches!(
        store.initialize(&state, &profile, NOW),
        Err(PersistenceError::Corrupt)
    ));
    assert!(
        store
            .load("team", fixture.founder.public(), &fixture.profile, NOW)
            .unwrap()
            .is_none()
    );
    store.initialize(&state, &fixture.profile, NOW).unwrap();
    let mut incoming = fixture.update(2);
    incoming.metadata.body.name = "x".repeat(MAX_MEMBERSHIP_BYTES);
    assert!(matches!(
        store.merge(
            1,
            fixture.founder.public(),
            &incoming,
            &fixture.profile,
            NOW
        ),
        Err(PersistenceError::TooLarge)
    ));
    assert_eq!(
        store
            .load("team", fixture.founder.public(), &fixture.profile, NOW)
            .unwrap()
            .unwrap()
            .revision,
        1
    );
}
#[test]
fn incompatible_schema_never_gets_reinitialized() {
    for change in [
        "PRAGMA user_version=2",
        "PRAGMA application_id=1",
        "CREATE TABLE unrelated (x)",
    ] {
        let fixture = Fixture::new();
        drop(fixture.initialized());
        fixture.connection().execute_batch(change).unwrap();
        assert!(matches!(
            MembershipStore::open(&fixture.root, WAIT),
            Err(PersistenceError::Schema)
        ));
        assert_eq!(
            fixture
                .connection()
                .query_row("SELECT count(*) FROM projection", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}

#[test]
fn exact_payload_limit_commits_and_one_more_byte_is_rejected() {
    use foundry_network::signing::SignedRecord;
    let fixture = Fixture::new();
    let mut store = fixture.initialized();
    let mut incoming = fixture.update(2);
    let baseline = serde_json::to_vec(&json!({"profile":fixture.profile,"snapshot":incoming}))
        .unwrap()
        .len();
    incoming.metadata.body.name =
        "x".repeat(MAX_MEMBERSHIP_BYTES - baseline + incoming.metadata.body.name.len());
    incoming.metadata = SignedRecord::sign(
        incoming.metadata.body,
        &fixture.founder,
        "team",
        &fixture.profile,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_vec(&json!({"profile":fixture.profile,"snapshot":incoming}))
            .unwrap()
            .len(),
        MAX_MEMBERSHIP_BYTES
    );
    assert_eq!(
        store
            .merge(
                1,
                fixture.founder.public(),
                &incoming,
                &fixture.profile,
                NOW
            )
            .unwrap()
            .0
            .revision,
        2
    );
    incoming.metadata.body.name.push('x');
    assert!(matches!(
        store.merge(
            2,
            fixture.founder.public(),
            &incoming,
            &fixture.profile,
            NOW
        ),
        Err(PersistenceError::TooLarge)
    ));
    assert_eq!(
        store
            .load("team", fixture.founder.public(), &fixture.profile, NOW)
            .unwrap()
            .unwrap()
            .revision,
        2
    );
}

#[test]
fn valid_signed_but_conflicting_name_records_cannot_be_filtered_on_load() {
    use foundry_network::signing::SignedRecord;
    let fixture = Fixture::new();
    let mut store = fixture.initialized();
    let mut incoming = fixture.initial();
    for _ in 0..2 {
        let key = SecretKey::generate();
        let mut record = fixture.member(&key, true, 2);
        record.body.person = "shared".into();
        record =
            SignedRecord::sign(record.body, &fixture.founder, "team", &fixture.profile).unwrap();
        incoming
            .admissions
            .insert(key.public().to_string(), record.clone());
        incoming.members.insert(key.public().to_string(), record);
    }
    mutate(&fixture, |value| {
        value["snapshot"] = serde_json::to_value(&incoming).unwrap()
    });
    assert!(matches!(
        store.load("team", fixture.founder.public(), &fixture.profile, NOW),
        Err(PersistenceError::Corrupt)
    ));
}
