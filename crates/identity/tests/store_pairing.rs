//! Pairing codes: single use, expiry, attempt limits and concurrent claims
//! through separate connections.
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use foundry_identity::authz::{
    Credential, CredentialKind, CredentialState, Principal, PrincipalId, WorkspaceId,
};
use foundry_identity::store::{CredentialStore, ManualClock, PairingState, StoreError};

const WINDOW: Duration = Duration::from_secs(600);

fn ws(id: &str) -> WorkspaceId {
    WorkspaceId::new(id)
}

fn who(id: &str) -> PrincipalId {
    PrincipalId::new(id)
}

fn caller(kind: CredentialKind, user: &str, workspace: &str) -> Principal {
    Principal::new(who(user), Credential::new(kind, ws(workspace)))
}

fn login(user: &str, workspace: &str) -> Principal {
    caller(CredentialKind::LoginGrant, user, workspace)
}

/// The issued code with its secret half replaced, keeping the public prefix.
fn wrong_secret(code: &str) -> String {
    let (prefix, secret) = code.split_once('-').unwrap();
    let replacement = if secret == "AAAAAAAA" {
        "BBBBBBBB"
    } else {
        "AAAAAAAA"
    };
    format!("{prefix}-{replacement}")
}

fn store() -> (CredentialStore, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::default());
    (
        CredentialStore::open_in_memory(clock.clone()).unwrap(),
        clock,
    )
}

#[test]
fn claim_binds_one_client_and_only_that_client_may_repeat_it() {
    let (store, _) = store();
    let issued = store
        .issue_pairing(&who("user-a"), &ws("ws-a"), WINDOW, 5)
        .unwrap();
    let code = issued.code.expose();
    let claimed = store
        .claim_pairing(&login("user-a", "ws-a"), "cli-1", code)
        .unwrap();
    assert_eq!(claimed.state, PairingState::Claimed);
    assert_eq!(claimed.client.as_deref(), Some("cli-1"));
    assert_eq!(claimed.prefix, issued.record.prefix);

    let repeated = store
        .claim_pairing(&login("user-a", "ws-a"), "cli-1", code)
        .unwrap();
    assert_eq!(repeated, claimed);
    assert_eq!(
        store.claim_pairing(&login("user-a", "ws-a"), "cli-2", code),
        Err(StoreError::PairingInvalid)
    );
}

#[test]
fn only_the_issuing_users_active_login_in_its_workspace_may_claim() {
    let (store, _) = store();
    let issued = store
        .issue_pairing(&who("user-a"), &ws("ws-a"), WINDOW, 5)
        .unwrap();
    let code = issued.code.expose();
    let revoked_login = Principal::new(
        who("user-a"),
        Credential::new(CredentialKind::LoginGrant, ws("ws-a"))
            .with_state(CredentialState::Revoked),
    );
    for claimant in [
        caller(CredentialKind::ScopedKey, "user-a", "ws-a"),
        login("user-b", "ws-a"),
        login("user-a", "ws-b"),
        revoked_login,
    ] {
        let refused = store.claim_pairing(&claimant, "cli-1", code).unwrap_err();
        assert_eq!(refused, StoreError::PairingInvalid);
        assert_eq!(refused.class(), "invalid_request");
        assert_eq!(refused.cause(), Some("pairing_invalid"));
    }
    let record = store.pairing(&ws("ws-a"), &issued.record.prefix).unwrap();
    assert_eq!(
        (record.state, record.failed_attempts),
        (PairingState::Issued, 0)
    );
}

#[test]
fn an_expired_code_cannot_be_claimed() {
    let (store, clock) = store();
    let issued = store
        .issue_pairing(&who("user-a"), &ws("ws-a"), WINDOW, 5)
        .unwrap();
    clock.advance(600_000);
    assert_eq!(
        store.claim_pairing(&login("user-a", "ws-a"), "cli-1", issued.code.expose()),
        Err(StoreError::PairingInvalid)
    );
    let record = store.pairing(&ws("ws-a"), &issued.record.prefix).unwrap();
    assert_eq!(record.state, PairingState::Expired);
}

#[test]
fn wrong_secrets_exhaust_the_pairing_and_the_right_one_then_fails() {
    let (store, _) = store();
    let issued = store
        .issue_pairing(&who("user-a"), &ws("ws-a"), WINDOW, 3)
        .unwrap();
    let code = issued.code.expose();
    let guess = wrong_secret(code);
    for _ in 0..3 {
        assert_eq!(
            store.claim_pairing(&login("user-a", "ws-a"), "cli-1", &guess),
            Err(StoreError::PairingInvalid)
        );
    }
    let record = store.pairing(&ws("ws-a"), &issued.record.prefix).unwrap();
    assert_eq!(
        (record.state, record.failed_attempts),
        (PairingState::Exhausted, 3)
    );
    assert_eq!(
        store.claim_pairing(&login("user-a", "ws-a"), "cli-1", code),
        Err(StoreError::PairingInvalid)
    );
}

#[test]
fn malformed_codes_and_bad_inputs_are_refused() {
    let (store, _) = store();
    let issued = store
        .issue_pairing(&who("user-a"), &ws("ws-a"), WINDOW, 3)
        .unwrap();
    let code = issued.code.expose();
    let claimant = login("user-a", "ws-a");
    for text in [
        "",
        "-",
        "ABCDEFGH",
        &code.to_lowercase(),
        &code.replace('-', "_"),
    ] {
        assert_eq!(
            store.claim_pairing(&claimant, "cli-1", text),
            Err(StoreError::PairingInvalid)
        );
    }
    assert_eq!(
        store.claim_pairing(&claimant, "", code),
        Err(StoreError::InvalidRequest { field: "client" })
    );
    assert_eq!(
        store
            .issue_pairing(&who("user-a"), &ws("ws-a"), WINDOW, 0)
            .unwrap_err(),
        StoreError::InvalidRequest {
            field: "max_attempts"
        }
    );
    assert_eq!(
        store
            .issue_pairing(&who("user-a"), &ws("ws-a"), Duration::ZERO, 3)
            .unwrap_err(),
        StoreError::InvalidRequest { field: "lifetime" }
    );
    assert_eq!(
        store.pairing(&ws("ws-b"), &issued.record.prefix),
        Err(StoreError::NotFound { what: "pairing" })
    );
}

/// Runs `claim` on `threads` threads at once, each through its own connection
/// to the database at `path`, and returns every result.
fn race<T: Send + 'static>(
    path: &std::path::Path,
    threads: usize,
    claim: impl Fn(&CredentialStore, usize) -> T + Send + Sync + 'static,
) -> Vec<T> {
    let barrier = Arc::new(Barrier::new(threads));
    let claim = Arc::new(claim);
    let handles: Vec<_> = (0..threads)
        .map(|index| {
            let store = CredentialStore::open(path, Arc::new(ManualClock::default())).unwrap();
            let (barrier, claim) = (barrier.clone(), claim.clone());
            thread::spawn(move || {
                barrier.wait();
                claim(&store, index)
            })
        })
        .collect();
    handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect()
}

#[test]
fn concurrent_claims_on_separate_connections_have_exactly_one_winner() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("identity.sqlite3");
    let issuer = CredentialStore::open(&path, Arc::new(ManualClock::default())).unwrap();
    for _ in 0..5 {
        let issued = issuer
            .issue_pairing(&who("user-a"), &ws("ws-a"), WINDOW, 5)
            .unwrap();
        let code = issued.code.expose().to_owned();
        let results = race(&path, 8, move |store, index| {
            store.claim_pairing(&login("user-a", "ws-a"), &format!("cli-{index}"), &code)
        });
        let winners: Vec<_> = results
            .iter()
            .filter_map(|result| result.as_ref().ok())
            .collect();
        assert_eq!(winners.len(), 1, "{results:?}");
        assert!(
            results
                .iter()
                .all(|result| result.is_ok() || result == &Err(StoreError::PairingInvalid))
        );
        let record = issuer.pairing(&ws("ws-a"), &issued.record.prefix).unwrap();
        assert_eq!(record.client, winners[0].client);
    }
}

#[test]
fn concurrent_wrong_guesses_never_exceed_the_attempt_limit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("identity.sqlite3");
    let issuer = CredentialStore::open(&path, Arc::new(ManualClock::default())).unwrap();
    let issued = issuer
        .issue_pairing(&who("user-a"), &ws("ws-a"), WINDOW, 3)
        .unwrap();
    let guess = wrong_secret(issued.code.expose());
    let results = race(&path, 8, move |store, index| {
        store.claim_pairing(&login("user-a", "ws-a"), &format!("cli-{index}"), &guess)
    });
    assert!(
        results
            .iter()
            .all(|result| result == &Err(StoreError::PairingInvalid))
    );
    let record = issuer.pairing(&ws("ws-a"), &issued.record.prefix).unwrap();
    assert_eq!(
        (record.state, record.failed_attempts),
        (PairingState::Exhausted, 3)
    );
}
