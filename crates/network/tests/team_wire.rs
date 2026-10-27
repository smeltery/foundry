use foundry_network::{
    signing::{MetadataRecord, SignedRecord},
    team::{InviteRecord, JoinReply, JoinRequest, LinkRecord, TeamDocument},
};
use iroh::SecretKey;
use serde_json::json;
use std::collections::BTreeMap;

fn document() -> TeamDocument {
    let key = SecretKey::from_bytes(&[0; 32]);
    TeamDocument {
        id: "team-a".into(),
        founder: key.public(),
        meta: SignedRecord {
            body: MetadataRecord {
                name: "fixture".into(),
                relay: None,
                ts: 1,
            },
            by: key.public(),
            sig: "fixture-signature".into(),
        },
        admissions: BTreeMap::new(),
        members: BTreeMap::new(),
        invites: BTreeMap::new(),
        link: None,
        attestations: BTreeMap::new(),
    }
}
fn expected() -> String {
    let id = SecretKey::from_bytes(&[0; 32]).public().to_string();
    format!(
        "{{\"id\":\"team-a\",\"founder\":\"{id}\",\"meta\":{{\"body\":{{\"name\":\"fixture\",\"relay\":null,\"ts\":1}},\"by\":\"{id}\",\"sig\":\"fixture-signature\"}},\"admissions\":{{}},\"members\":{{}},\"invites\":{{}},\"link\":null}}"
    )
}
#[test]
fn full_document_exact_order_defaults_and_omission() {
    assert_eq!(serde_json::to_string(&document()).unwrap(), expected());
    let mut value = serde_json::to_value(document()).unwrap();
    for field in ["admissions", "link"] {
        value.as_object_mut().unwrap().remove(field);
    }
    let decoded: TeamDocument = serde_json::from_value(value).unwrap();
    assert_eq!(serde_json::to_string(&decoded).unwrap(), expected());
    for field in ["id", "founder", "meta", "members", "invites"] {
        let mut value = serde_json::to_value(document()).unwrap();
        value.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<TeamDocument>(value).is_err());
    }
}
#[test]
fn full_document_preserves_invites_link_and_attestations_without_verifying() {
    let mut doc = document();
    let by = doc.founder;
    doc.invites.insert(
        "fixture-hash".into(),
        SignedRecord {
            body: InviteRecord {
                hash: "fixture-hash".into(),
                expires: 99,
                person: None,
                used_by: None,
                ts: 3,
                reusable: true,
            },
            by,
            sig: "unverified".into(),
        },
    );
    doc.link = Some(SignedRecord {
        body: LinkRecord {
            secret: "fixture-bearer".into(),
            ts: 4,
        },
        by,
        sig: "unverified".into(),
    });
    doc.attestations
        .insert(by.to_string(), "fixture.unverified.proof".into());
    let encoded = serde_json::to_vec(&doc).unwrap();
    let decoded: TeamDocument = serde_json::from_slice(&encoded).unwrap();
    assert!(decoded == doc);
    assert!(decoded.link.unwrap().verify("team-a", &profile()).is_err());
    assert!(
        serde_json::from_slice::<foundry_network::membership::MembershipSnapshot>(&encoded)
            .is_err()
    );
    let text = String::from_utf8(encoded).unwrap();
    assert!(text.find("\"invites\"").unwrap() < text.find("\"link\"").unwrap());
    assert!(text.find("\"link\"").unwrap() < text.find("\"attestations\"").unwrap());
}
fn profile() -> foundry_network::WireProfile {
    foundry_network::WireProfile {
        team_alpn: b"fixture/t".to_vec(),
        join_alpn: b"fixture/j".to_vec(),
        protocol: 1,
        signing_prefix: "fixture/".into(),
        invitation_endpoint_context: "endpoint".into(),
        invitation_secret_context: "secret".into(),
        ticket_prefixes: vec!["fixture-".into()],
    }
}
#[test]
fn join_request_exact_omission_and_populated_field_order() {
    let base = r#"{"team":"team-a","secret":"fixture","person":"ada","machine":"desk"}"#;
    let mut request: JoinRequest = serde_json::from_str(base).unwrap();
    assert!(request.proof.is_none());
    assert!(!request.owner_only);
    assert_eq!(serde_json::to_string(&request).unwrap(), base);
    request.proof = Some("fixture.proof".into());
    request.owner_only = true;
    assert_eq!(
        serde_json::to_string(&request).unwrap(),
        r#"{"team":"team-a","secret":"fixture","person":"ada","machine":"desk","proof":"fixture.proof","owner_only":true}"#
    );
    let explicit = base.trim_end_matches('}').to_string() + ",\"proof\":null,\"owner_only\":false}";
    let decoded: JoinRequest = serde_json::from_str(&explicit).unwrap();
    assert_eq!(serde_json::to_string(&decoded).unwrap(), base);
}
#[test]
fn join_reply_tags_and_payload_order_are_exact() {
    let reply = JoinReply::Ok {
        state: Box::new(document()),
    };
    let bytes = serde_json::to_string(&reply).unwrap();
    assert_eq!(bytes, format!("{{\"t\":\"ok\",\"state\":{}}}", expected()));
    assert!(serde_json::from_str::<JoinReply>(&bytes).unwrap() == reply);
    assert_eq!(
        serde_json::to_string(&JoinReply::Err {
            error: "fixture\n\"é".into()
        })
        .unwrap(),
        "{\"t\":\"err\",\"error\":\"fixture\\n\\\"é\"}"
    );
    for value in [
        json!({"t":"success","state":document()}),
        json!({"t":"ok"}),
        json!({"t":"err"}),
    ] {
        assert!(serde_json::from_value::<JoinReply>(value).is_err());
    }
}
#[test]
fn duplicate_known_fields_and_invalid_types_are_rejected() {
    for text in [
        r#"{"secret":"x","secret":"y","ts":1}"#,
        r#"{"secret":"x","ts":9223372036854775808}"#,
        r#"{"secret":2,"ts":1}"#,
    ] {
        assert!(serde_json::from_str::<LinkRecord>(text).is_err());
    }
    assert!(
        serde_json::from_str::<InviteRecord>(r#"{"hash":"h","expires":3,"ts":1,"reusable":null}"#)
            .is_err()
    );
    assert!(
        serde_json::from_str::<JoinRequest>(
            r#"{"team":"t","secret":"s","person":"p","machine":"m","owner_only":1}"#
        )
        .is_err()
    );
}
#[test]
fn typed_unknown_fields_do_not_gain_signed_meaning() {
    let body: LinkRecord =
        serde_json::from_str(r#"{"secret":"fixture","ts":1,"extra":"ignored"}"#).unwrap();
    assert_eq!(
        serde_json::to_string(&body).unwrap(),
        r#"{"secret":"fixture","ts":1}"#
    );
    let invite: InviteRecord = serde_json::from_str(r#"{"hash":"h","expires":3,"ts":1}"#).unwrap();
    assert_eq!(
        serde_json::to_string(&invite).unwrap(),
        r#"{"hash":"h","expires":3,"person":null,"used_by":null,"ts":1,"reusable":false}"#
    );
}
