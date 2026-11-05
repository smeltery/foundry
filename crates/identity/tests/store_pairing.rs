//! Pairing codes: format, single use, expiry, attempt limits, digest erasure
//! and concurrent claims through separate connections.
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use foundry_identity::authz::{PrincipalId, WorkspaceId};
use foundry_identity::store::{
    CredentialStore, ManualClock, PAIRING_MAX_ATTEMPTS, PAIRING_MAX_LIFETIME, PairingState,
    StoreError,
};
use rusqlite::Connection;

const WINDOW: Duration = Duration::from_secs(600);
const HOUR: Duration = Duration::from_secs(3600);

fn ws(id: &str) -> WorkspaceId {
    WorkspaceId::new(id)
}

fn who(id: &str) -> PrincipalId {
    PrincipalId::new(id)
}

/// Presentable text of a fresh login for `user` in `workspace`.
fn login(store: &CredentialStore, user: &str, workspace: &str) -> String {
    let issued = store
        .issue_login(&who(user), &ws(workspace), [], HOUR)
        .unwrap();
    issued.secret.expose().to_owned()
}

/// The issued code with its secret half replaced, keeping the public prefix.
fn wrong_secret(code: &str) -> String {
    let (prefix, secret) = code.split_once('-').unwrap();
    let replacement = if secret == "AAAA-AAAA-AAAA" {
        "BBBB-BBBB-BBBB"
    } else {
        "AAAA-AAAA-AAAA"
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
fn codes_show_three_secret_groups_and_accept_any_case_or_grouping() {
    let (store, _) = store();
    let own = login(&store, "user-a", "ws-a");
    let variants: [fn(&str) -> String; 4] = [
        str::to_owned,
        |code| format!("{}{}", &code[..9], code[9..].replace('-', "")),
        |code| format!("{}{}", &code[..13], code[13..].replacen('-', "", 1)),
        str::to_lowercase,
    ];
    for variant in variants {
        let issued = store
            .issue_pairing(&who("user-a"), &ws("ws-a"), WINDOW, 1)
            .unwrap();
        let code = issued.code.expose();
        let lengths: Vec<usize> = code.split('-').map(str::len).collect();
        assert_eq!(lengths, [8, 4, 4, 4], "{code}");
        assert!(code.starts_with(&issued.record.prefix));
        // Twelve secret symbols from a 32-symbol alphabet: 60 random bits.
        let alphabet = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789-";
        assert!(code.chars().all(|symbol| alphabet.contains(symbol)));
        let presented = variant(code);
        let claimed = store.claim_pairing(&own, "cli-1", &presented);
        assert!(claimed.is_ok(), "{presented}");
    }
}

#[test]
fn claim_binds_one_client_and_only_that_client_may_repeat_it() {
    let (store, _) = store();
    let issued = store
        .issue_pairing(&who("user-a"), &ws("ws-a"), WINDOW, 5)
        .unwrap();
    let code = issued.code.expose();
    let own = login(&store, "user-a", "ws-a");
    let claimed = store.claim_pairing(&own, "cli-1", code).unwrap();
    assert_eq!(claimed.state, PairingState::Claimed);
    assert_eq!(claimed.client.as_deref(), Some("cli-1"));
    assert_eq!(claimed.prefix, issued.record.prefix);

    assert_eq!(store.claim_pairing(&own, "cli-1", code).unwrap(), claimed);
    assert_eq!(
        store.claim_pairing(&own, "cli-2", code),
        Err(StoreError::PairingInvalid)
    );
}

#[test]
fn only_the_issuing_users_login_in_its_workspace_may_claim() {
    let (store, _) = store();
    let issued = store
        .issue_pairing(&who("user-a"), &ws("ws-a"), WINDOW, 5)
        .unwrap();
    let code = issued.code.expose();
    let key = store.issue_key(&who("user-a"), &ws("ws-a"), []).unwrap();
    for claimant in [
        key.secret.expose().to_owned(),
        login(&store, "user-b", "ws-a"),
        login(&store, "user-a", "ws-b"),
    ] {
        let refused = store.claim_pairing(&claimant, "cli-1", code).unwrap_err();
        assert_eq!(refused, StoreError::PairingInvalid);
        assert_eq!(refused.class(), "invalid_request");
        assert_eq!(refused.cause(), Some("pairing_invalid"));
    }
    let revoked = login(&store, "user-a", "ws-a");
    assert!(store.logout(&revoked).unwrap());
    for unauthenticated in [revoked.as_str(), "", "login_UNKNOWN234_X"] {
        assert_eq!(
            store.claim_pairing(unauthenticated, "cli-1", code),
            Err(StoreError::Unauthenticated)
        );
    }
    let record = store.pairing(&ws("ws-a"), &issued.record.prefix).unwrap();
    assert_eq!(
        (record.state, record.failed_attempts),
        (PairingState::Issued, 0)
    );
}

#[test]
fn other_callers_guessing_never_use_up_the_issuers_attempts() {
    let (store, _) = store();
    let issued = store
        .issue_pairing(&who("user-a"), &ws("ws-a"), WINDOW, 2)
        .unwrap();
    let guess = wrong_secret(issued.code.expose());
    let intruder = login(&store, "user-b", "ws-a");
    for _ in 0..10 {
        assert_eq!(
            store.claim_pairing(&intruder, "cli-x", &guess),
            Err(StoreError::PairingInvalid)
        );
    }
    let record = store.pairing(&ws("ws-a"), &issued.record.prefix).unwrap();
    assert_eq!(record.failed_attempts, 0);
    let own = login(&store, "user-a", "ws-a");
    assert!(
        store
            .claim_pairing(&own, "cli-1", issued.code.expose())
            .is_ok()
    );
}

#[test]
fn an_expired_code_cannot_be_claimed() {
    let (store, clock) = store();
    let issued = store
        .issue_pairing(&who("user-a"), &ws("ws-a"), WINDOW, 5)
        .unwrap();
    let own = login(&store, "user-a", "ws-a");
    clock.advance(600_000);
    assert_eq!(
        store.claim_pairing(&own, "cli-1", issued.code.expose()),
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
    let own = login(&store, "user-a", "ws-a");
    for _ in 0..3 {
        assert_eq!(
            store.claim_pairing(&own, "cli-1", &guess),
            Err(StoreError::PairingInvalid)
        );
    }
    let record = store.pairing(&ws("ws-a"), &issued.record.prefix).unwrap();
    assert_eq!(
        (record.state, record.failed_attempts),
        (PairingState::Exhausted, 3)
    );
    assert_eq!(
        store.claim_pairing(&own, "cli-1", code),
        Err(StoreError::PairingInvalid)
    );
}

#[test]
fn lifetime_and_attempts_are_capped() {
    let (store, _) = store();
    let issue = |lifetime, attempts| {
        store
            .issue_pairing(&who("user-a"), &ws("ws-a"), lifetime, attempts)
            .map(|_| ())
    };
    let invalid = |field| Err(StoreError::InvalidRequest { field });
    assert_eq!(issue(PAIRING_MAX_LIFETIME, PAIRING_MAX_ATTEMPTS), Ok(()));
    assert_eq!(
        issue(PAIRING_MAX_LIFETIME + Duration::from_millis(1), 3),
        invalid("lifetime")
    );
    assert_eq!(issue(Duration::ZERO, 3), invalid("lifetime"));
    assert_eq!(
        issue(WINDOW, PAIRING_MAX_ATTEMPTS + 1),
        invalid("max_attempts")
    );
    assert_eq!(issue(WINDOW, 0), invalid("max_attempts"));
    assert!(PAIRING_MAX_ATTEMPTS < 10 && PAIRING_MAX_LIFETIME <= Duration::from_secs(600));
}

#[test]
fn malformed_codes_and_bad_inputs_are_refused() {
    let (store, _) = store();
    let issued = store
        .issue_pairing(&who("user-a"), &ws("ws-a"), WINDOW, 3)
        .unwrap();
    let code = issued.code.expose();
    let own = login(&store, "user-a", "ws-a");
    let prefix = &issued.record.prefix;
    let secret = code[9..].replace('-', "");
    // Each keeps the real prefix, so it would use up an attempt if it reached
    // the stored digest; there are more of them than the pairing allows.
    for text in [
        String::new(),
        "-".to_owned(),
        "ABCDEFGH".to_owned(),
        code.replace('-', "_"),
        format!("{prefix}{secret}"),
        format!("{prefix}-{}", &secret[..8]),
        format!("{prefix}-{}", &secret[..11]),
        format!("{prefix}-{secret}A"),
        format!("{prefix}-{}-{}", &secret[..3], &secret[3..]),
        format!("{prefix}-{}--{}", &secret[..4], &secret[4..]),
        format!("{prefix}--{secret}"),
        format!("{prefix}-0{}", &secret[1..]),
        format!("{prefix}-{}O", &secret[..11]),
        format!("{prefix}-{}é", &secret[..11]),
        format!("{prefix}-{}\u{410}", &secret[..11]),
        format!("{prefix}-{}\u{2010}{}", &secret[..4], &secret[4..]),
        format!("{code}-"),
        format!(" {code}"),
        format!("{code}\n"),
    ] {
        assert_eq!(
            store.claim_pairing(&own, "cli-1", &text),
            Err(StoreError::PairingInvalid),
            "{text}"
        );
    }
    let lookup = store.pairing(&ws("ws-a"), &prefix.to_lowercase());
    assert_eq!(lookup.unwrap().failed_attempts, 0);
    assert_eq!(
        store.claim_pairing(&own, "", code),
        Err(StoreError::InvalidRequest { field: "client" })
    );
    assert_eq!(
        store.pairing(&ws("ws-b"), &issued.record.prefix),
        Err(StoreError::NotFound { what: "pairing" })
    );
}

fn digest_erased(path: &Path, prefix: &str) -> bool {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT secret_digest IS NULL FROM pairings WHERE prefix = ?1",
            [prefix],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn digests_are_erased_once_a_pairing_is_claimed_exhausted_or_expired() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("identity.sqlite3");
    let clock = Arc::new(ManualClock::default());
    let store = CredentialStore::open(&path, clock.clone()).unwrap();
    let own = login(&store, "user-a", "ws-a");
    let issue = |lifetime| {
        store
            .issue_pairing(&who("user-a"), &ws("ws-a"), lifetime, 2)
            .unwrap()
    };
    let claimed = issue(WINDOW);
    let exhausted = issue(WINDOW);
    let expiring = issue(Duration::from_secs(60));
    assert!(!digest_erased(&path, &claimed.record.prefix));

    store
        .claim_pairing(&own, "cli-1", claimed.code.expose())
        .unwrap();
    assert!(digest_erased(&path, &claimed.record.prefix));

    let guess = wrong_secret(exhausted.code.expose());
    store.claim_pairing(&own, "cli-1", &guess).unwrap_err();
    assert!(!digest_erased(&path, &exhausted.record.prefix));
    store.claim_pairing(&own, "cli-1", &guess).unwrap_err();
    assert!(digest_erased(&path, &exhausted.record.prefix));

    clock.advance(60_000);
    assert!(!digest_erased(&path, &expiring.record.prefix));
    let live = issue(WINDOW);
    assert!(digest_erased(&path, &expiring.record.prefix));
    assert!(!digest_erased(&path, &live.record.prefix));
}

/// Runs `claim` on `threads` threads at once, each through its own connection
/// to the database at `path`, and returns every result.
fn race<T: Send + 'static>(
    path: &Path,
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
    let own = login(&issuer, "user-a", "ws-a");
    for _ in 0..5 {
        let issued = issuer
            .issue_pairing(&who("user-a"), &ws("ws-a"), WINDOW, 5)
            .unwrap();
        let code = issued.code.expose().to_owned();
        let own = own.clone();
        let results = race(&path, 8, move |store, index| {
            store.claim_pairing(&own, &format!("cli-{index}"), &code)
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
    let own = login(&issuer, "user-a", "ws-a");
    let issued = issuer
        .issue_pairing(&who("user-a"), &ws("ws-a"), WINDOW, 3)
        .unwrap();
    let guess = wrong_secret(issued.code.expose());
    let results = race(&path, 8, move |store, index| {
        store.claim_pairing(&own, &format!("cli-{index}"), &guess)
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
