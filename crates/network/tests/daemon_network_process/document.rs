use super::{
    client::Client,
    config::{Config, Root, profile},
    database::team_document,
    flow::*,
};
use foundry_network::{profile::WireProfile, team::TeamDocument, team_state::TeamState};
use iroh::EndpointId;
use serde_json::{Value, json};

/// A daemon publishes changed state before later texts on one ordered stream,
/// and its peer merges each frame before reading the next. The first text may
/// share a batch that read the older state; the second is sent by a later batch,
/// so its arrival proves the peer merged every change made before the first.
async fn fence(
    (from, auth): (&mut Client, &Value),
    (to, peer, remote): (&mut Client, &Value, &Config),
    label: &str,
) {
    let ids = [format!("{label}-first"), format!("{label}-second")];
    for id in &ids {
        send(from, auth, remote, id, "state fence").await;
        peek(to, peer, &[(id, "state fence")]).await;
        consume(to, peer, &[(id, "state fence")]).await;
    }
}
fn verified(document: &TeamDocument, members: &[EndpointId]) {
    let profile: WireProfile = serde_json::from_value(profile()).unwrap();
    let team = &document.id;
    assert!(
        document.meta.verify(team, &profile).is_ok(),
        "metadata signature"
    );
    for record in document
        .members
        .values()
        .chain(document.admissions.values())
    {
        assert!(record.verify(team, &profile).is_ok(), "member signature");
    }
    for record in document.invites.values() {
        assert!(record.verify(team, &profile).is_ok(), "invite signature");
    }
    let link = document.link.as_ref().expect("signed link record");
    assert!(link.verify(team, &profile).is_ok(), "link signature");
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let Ok((state, report)) = TeamState::bootstrap(team, document.founder, document, &profile, now)
    else {
        panic!("stored document does not verify as a complete team state");
    };
    assert_eq!((report.rejected, report.proof_required), (0, 0));
    for member in members {
        assert!(
            state.is_active(*member),
            "joined member missing from document"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn joined_daemons_hold_identical_complete_signed_team_documents() {
    let root = Root::new();
    let (a, mut ca, aa, b, mut cb, ab, _) = pair(&root).await;
    let endpoint = |value: Value| -> EndpointId {
        serde_json::from_value::<iroh::EndpointAddr>(value["endpoint"].clone())
            .unwrap()
            .id
    };
    let (founder, member) = (
        endpoint(ca.status(&aa).await),
        endpoint(cb.status(&ab).await),
    );
    let team = ca.status(&aa).await["team"].clone();
    // A founder metadata change and invitation must reach the member whole,
    // and a member-issued invitation must reach the founder whole as well.
    ca.control(&aa, json!({"kind":"rotate"})).await;
    invite(&mut ca, &aa, false).await;
    fence((&mut ca, &aa), (&mut cb, &ab, &b.config), "to-member").await;
    invite(&mut cb, &ab, false).await;
    fence((&mut cb, &ab), (&mut ca, &aa, &a.config), "to-founder").await;
    let (state_a, state_b) = (a.config.state.clone(), b.config.state.clone());
    b.stop(&mut cb, &ab, &[]).await;
    a.stop(&mut ca, &aa, &[]).await;
    // Read only after both owners exit; an outside WAL reader is not safe live.
    let (left, right) = (team_document(&state_a), team_document(&state_b));
    let (Some(left), Some(right)) = (left, right) else {
        panic!("both daemons must keep an active signed team document");
    };
    assert!(
        left == right,
        "daemons hold different signed team documents"
    );
    verified(&left, &[founder, member]);
    assert_eq!(json!(left.id), team);
    let signed_by = |signer: EndpointId| {
        left.invites
            .values()
            .filter(|record| record.by == signer && !record.body.reusable)
            .count()
    };
    assert_eq!((signed_by(founder), signed_by(member)), (2, 1));
    let joined = left
        .invites
        .values()
        .find(|record| record.body.used_by == Some(member))
        .expect("consumed pairing invitation");
    let link = left.link.as_ref().unwrap();
    assert!(
        link.body.ts > joined.body.ts,
        "rotated link record did not replace the formation link"
    );
}
