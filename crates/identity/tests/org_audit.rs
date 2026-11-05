//! The audit log: one entry per mutation, append-only storage, and stable
//! newest-first paging with filters and withheld entries.
mod org_fixture;

use foundry_identity::authz::{Check, Role, Scope};
use foundry_identity::org::{AUDIT_PAGE_DEFAULT, AUDIT_PAGE_MAX, AuditQuery, OrgError};
use org_fixture::{Fixture, failure, who, ws};

const AUDIT: &[Scope] = &[Scope::AuditRead];
const WRITE: &[Scope] = &[Scope::WorkspaceWrite];

fn team() -> Fixture {
    let fixture = Fixture::new();
    fixture.seed(
        "ws-a",
        "Team A",
        &[
            ("user-a", Role::Owner),
            ("user-b", Role::Admin),
            ("user-c", Role::Member),
            ("user-d", Role::Viewer),
        ],
    );
    fixture
}

/// `count` invitations from the owner, one millisecond apart.
fn invite(fixture: &Fixture, count: usize) {
    let owner = fixture.login("user-a", "ws-a", WRITE);
    for index in 0..count {
        fixture.clock.advance(1);
        let email = format!("person{index}@team.example");
        fixture
            .store
            .invite_members(&owner, &ws("ws-a"), &[&email], None)
            .unwrap();
    }
}

fn targets(fixture: &Fixture, query: &AuditQuery) -> (Vec<String>, Option<u64>) {
    let page = fixture
        .store
        .list_audit(&fixture.login("user-b", "ws-a", AUDIT), &ws("ws-a"), query)
        .unwrap();
    let targets = page
        .entries
        .into_iter()
        .map(|entry| entry.target.unwrap())
        .collect();
    (targets, page.next.map(|cursor| cursor.get()))
}

#[test]
fn every_mutation_records_actor_email_and_time() {
    let fixture = team();
    fixture.clock.advance(7);
    let owner = fixture.login("user-a", "ws-a", WRITE);
    fixture
        .store
        .rename_workspace(&owner, &ws("ws-a"), "Team Alpha")
        .unwrap();
    fixture
        .store
        .set_member_role(&owner, &ws("ws-a"), &who("user-c"), Role::Admin)
        .unwrap();
    fixture
        .store
        .remove_member(&owner, &ws("ws-a"), &who("user-c"))
        .unwrap();
    let page = fixture
        .store
        .list_audit(
            &fixture.login("user-a", "ws-a", AUDIT),
            &ws("ws-a"),
            &AuditQuery::default(),
        )
        .unwrap();
    let actions: Vec<_> = page
        .entries
        .iter()
        .map(|entry| entry.action.as_str())
        .collect();
    assert_eq!(
        actions,
        ["member.remove", "member.set_role", "workspace.rename"]
    );
    let newest = &page.entries[0];
    assert_eq!(newest.actor.as_ref(), Some(&who("user-a")));
    assert_eq!(newest.actor_email.as_deref(), Some("user-a@team.example"));
    assert_eq!(
        (newest.target_kind.as_str(), newest.target.as_deref()),
        ("member", Some("user-c"))
    );
    assert_eq!(
        (newest.time_ms, newest.withheld, page.next),
        (7, false, None)
    );
}

#[test]
fn pages_are_newest_first_and_follow_the_end_position() {
    let fixture = team();
    invite(&fixture, 5);
    let first = AuditQuery {
        page: Some(2),
        ..AuditQuery::default()
    };
    let (items, next) = targets(&fixture, &first);
    assert_eq!(items, ["person4@team.example", "person3@team.example"]);
    // A mutation between pages does not shift the next page.
    invite(&fixture, 1);
    let second = AuditQuery {
        before: next.map(foundry_identity::org::AuditCursor::new),
        ..first.clone()
    };
    let (items, next) = targets(&fixture, &second);
    assert_eq!(items, ["person2@team.example", "person1@team.example"]);
    let third = AuditQuery {
        before: next.map(foundry_identity::org::AuditCursor::new),
        ..first
    };
    assert_eq!(
        targets(&fixture, &third),
        (vec!["person0@team.example".to_owned()], None)
    );
}

#[test]
fn page_size_defaults_caps_and_refuses_zero() {
    let fixture = team();
    let owner = fixture.login("user-a", "ws-a", WRITE);
    let emails: Vec<String> = (0..205)
        .map(|index| format!("p{index}@team.example"))
        .collect();
    let emails: Vec<&str> = emails.iter().map(String::as_str).collect();
    fixture
        .store
        .invite_members(&owner, &ws("ws-a"), &emails, None)
        .unwrap();
    let (default, next) = targets(&fixture, &AuditQuery::default());
    assert_eq!(default.len(), AUDIT_PAGE_DEFAULT as usize);
    assert!(next.is_some());
    let capped = AuditQuery {
        page: Some(500),
        ..AuditQuery::default()
    };
    assert_eq!(targets(&fixture, &capped).0.len(), AUDIT_PAGE_MAX as usize);
    let zero = AuditQuery {
        page: Some(0),
        ..AuditQuery::default()
    };
    let refused =
        fixture
            .store
            .list_audit(&fixture.login("user-a", "ws-a", AUDIT), &ws("ws-a"), &zero);
    assert_eq!(failure(refused), ("invalid_request", None));
}

#[test]
fn time_bounds_and_filters_narrow_the_page() {
    let fixture = team();
    invite(&fixture, 3); // times 1, 2 and 3
    let window = AuditQuery {
        since_ms: Some(1),
        until_ms: Some(3),
        ..AuditQuery::default()
    };
    let (items, _) = targets(&fixture, &window);
    assert_eq!(items, ["person1@team.example", "person0@team.example"]);
    let inverted = AuditQuery {
        since_ms: Some(3),
        until_ms: Some(1),
        ..AuditQuery::default()
    };
    let refused = fixture.store.list_audit(
        &fixture.login("user-a", "ws-a", AUDIT),
        &ws("ws-a"),
        &inverted,
    );
    assert_eq!(failure(refused), ("invalid_request", None));
    fixture
        .store
        .rename_workspace(
            &fixture.login("user-b", "ws-a", WRITE),
            &ws("ws-a"),
            "Renamed",
        )
        .unwrap();
    let by_actor = AuditQuery {
        actor: Some(who("user-b")),
        ..AuditQuery::default()
    };
    assert_eq!(targets(&fixture, &by_actor).0, ["ws-a"]);
    let by_action = AuditQuery {
        action: Some("member.invite".to_owned()),
        ..AuditQuery::default()
    };
    assert_eq!(targets(&fixture, &by_action).0.len(), 3);
}

#[test]
fn members_get_other_members_personal_entries_withheld() {
    let fixture = team();
    fixture
        .raw()
        .execute_batch(
            "INSERT INTO org_audit (workspace, action, actor, target_kind, target, label,
                 details, personal_to, created_ms)
             VALUES ('ws-a', 'secret.set', 'user-b', 'secret', 'PRIVATE_NAME', 'PRIVATE_NAME',
                 'names=PRIVATE_NAME', 'user-b', 1),
                    ('ws-a', 'secret.set', 'user-c', 'secret', 'OWN_NAME', NULL, '', 'user-c', 2);",
        )
        .unwrap();
    let member = fixture
        .store
        .list_audit(
            &fixture.login("user-c", "ws-a", AUDIT),
            &ws("ws-a"),
            &AuditQuery::default(),
        )
        .unwrap();
    let (own, other) = (&member.entries[0], &member.entries[1]);
    assert_eq!(
        (own.target.as_deref(), own.withheld),
        (Some("OWN_NAME"), false)
    );
    assert_eq!(
        (other.action.as_str(), other.actor.as_ref()),
        ("secret.set", Some(&who("user-b")))
    );
    assert_eq!(
        (&other.target, &other.label, &other.details, other.withheld),
        (&None, &None, &None, true)
    );
    let admin = fixture
        .store
        .list_audit(
            &fixture.login("user-a", "ws-a", AUDIT),
            &ws("ws-a"),
            &AuditQuery::default(),
        )
        .unwrap();
    assert!(
        admin
            .entries
            .iter()
            .all(|entry| !entry.withheld && entry.target.is_some())
    );
}

#[test]
fn reading_needs_the_audit_scope_and_a_known_role() {
    let fixture = team();
    let query = AuditQuery::default();
    let cases = [
        (
            fixture.login("user-a", "ws-a", &[Scope::WorkspaceRead]),
            Check::Scope,
        ),
        (
            fixture.login("user-d", "ws-a", AUDIT),
            Check::UndeterminedRole,
        ),
        (
            fixture.login("user-z", "ws-a", AUDIT),
            Check::Authentication,
        ),
    ];
    for (caller, check) in cases {
        let Err(OrgError::Denied(denial)) = fixture.store.list_audit(&caller, &ws("ws-a"), &query)
        else {
            panic!("{check:?} must deny")
        };
        assert_eq!(denial.check, check);
    }
}

#[test]
fn stored_entries_can_be_neither_updated_replaced_nor_deleted() {
    let fixture = team();
    invite(&fixture, 1);
    let raw = fixture.raw();
    let first: i64 = raw
        .query_row("SELECT min(sequence) FROM org_audit", [], |row| row.get(0))
        .unwrap();
    let forged = |verb: &str| {
        format!(
            "{verb} INTO org_audit (sequence, workspace, action, target_kind, target, details,
                 created_ms) VALUES ({first}, 'ws-a', 'forged', 'member', 'x', '', 0)"
        )
    };
    for statement in [
        "UPDATE org_audit SET action = 'forged'".to_owned(),
        "DELETE FROM org_audit".to_owned(),
        forged("INSERT OR REPLACE"),
        forged("REPLACE"),
        forged("INSERT"),
        format!(
            "{} ON CONFLICT (sequence) DO UPDATE SET action = 'forged'",
            forged("INSERT")
        ),
    ] {
        let error = raw.execute_batch(&statement).unwrap_err();
        assert!(error.to_string().contains("append-only"), "{error}");
    }
    assert_eq!(fixture.audit_actions("ws-a"), ["member.invite"]);
}
