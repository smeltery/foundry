#[path = "team_state_support/mod.rs"]
mod support;
use support::*;

#[test]
fn occupied_name_retry_is_local_only_and_preserves_admission_and_invite() {
    let f = Fixture::new();
    let bob = key(2);
    let carol = key(3);
    let state = f
        .admit(&f.state, &f.owner, &carol, "carol", f.secret(&f.state), NOW)
        .unwrap();
    let state = state
        .issue_invite(&f.owner, "one-use", 1, false, NOW)
        .unwrap();
    let state = f
        .admit(&state, &f.owner, &bob, "bob", "one-use", NOW)
        .unwrap();
    let retry = f
        .admit(&state, &f.owner, &bob, "carol", "one-use", NOW + 3_600_001)
        .unwrap();
    assert_eq!(
        retry.document().members[&bob.public().to_string()]
            .body
            .person,
        "carol"
    );
    assert!(retry.document().admissions == state.document().admissions);
    assert!(retry.document().invites == state.document().invites);
    assert_eq!(
        state.document().members[&bob.public().to_string()]
            .body
            .person,
        "bob"
    );
    let (remote, report) = state
        .merge_from(f.owner.public(), retry.document(), NOW + 3_600_001)
        .unwrap();
    assert_eq!(report.proof_required, 1);
    assert_eq!(
        remote.document().members[&bob.public().to_string()]
            .body
            .person,
        "bob"
    );
}
