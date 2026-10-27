mod common;
use foundry_network::{
    signing::{SignedRecord, signing_payload},
    team::{InviteRecord, LinkRecord},
};
use iroh::SecretKey;

fn invite() -> InviteRecord {
    InviteRecord {
        hash: "ab".repeat(32),
        expires: 1000,
        person: None,
        used_by: None,
        ts: 17,
        reusable: false,
    }
}
fn link() -> LinkRecord {
    LinkRecord {
        secret: "0123456789abcdef0123456789abcdef".into(),
        ts: 23,
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn invite_null_and_false_have_independent_golden_payload_and_signature() {
    let key = SecretKey::from_bytes(&[0; 32]);
    let expected = concat!(
        "fixture/signature/1/team-a/invite/{\"hash\":\"",
        "abababababababababababababababababababababababababababababababab",
        "\",\"expires\":1000,\"person\":null,\"used_by\":null,\"ts\":17,\"reusable\":false}"
    );
    assert_eq!(
        signing_payload(&common::config().profile, "team-a", &invite()).unwrap(),
        expected.as_bytes()
    );
    let record = SignedRecord::sign(invite(), &key, "team-a", &common::config().profile).unwrap();
    assert_eq!(
        record.sig,
        "cb7f5b077f7898c11e42a9854720caf884c03988e67d0ed16e251c42c89004d591fdf9ed4785a37922e40f785f115d09a083fd950a84049c4a9f5741eb80a601"
    );
    record.verify("team-a", &common::config().profile).unwrap();
}
#[test]
fn populated_invite_has_independent_golden_payload_and_signature() {
    let key = SecretKey::from_bytes(&[0; 32]);
    let body = InviteRecord {
        hash: "cd".repeat(32),
        expires: 2000,
        person: Some("ada".into()),
        used_by: Some(key.public()),
        ts: 18,
        reusable: true,
    };
    let expected = concat!(
        "fixture/signature/1/team-a/invite/{\"hash\":\"",
        "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
        "\",\"expires\":2000,\"person\":\"ada\",\"used_by\":\"",
        "3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29",
        "\",\"ts\":18,\"reusable\":true}"
    );
    assert_eq!(
        signing_payload(&common::config().profile, "team-a", &body).unwrap(),
        expected.as_bytes()
    );
    let record = SignedRecord::sign(body, &key, "team-a", &common::config().profile).unwrap();
    assert_eq!(
        record.sig,
        "9d1b5eea13e1831e2fa82528c47bedbfd83401235957c38b6149fb5efa9f701927c24ec4253dadf6c7009399a82cf5d5adc7edce1d23a3381c3b4899c6197508"
    );
    record.verify("team-a", &common::config().profile).unwrap();
}
#[test]
fn link_has_independent_golden_payload_and_signature() {
    let key = SecretKey::from_bytes(&[0; 32]);
    let expected=b"fixture/signature/1/team-a/link/{\"secret\":\"0123456789abcdef0123456789abcdef\",\"ts\":23}";
    assert_eq!(
        signing_payload(&common::config().profile, "team-a", &link()).unwrap(),
        expected
    );
    let record = SignedRecord::sign(link(), &key, "team-a", &common::config().profile).unwrap();
    assert_eq!(
        record.sig,
        "58085f04d2e41a30366a4b8dd379d7baa2f1540ea83c898873568f49c214bc3f0633e11f49b167a3df5eca57fd9354461bed5197eec24e1f01c8998d2349760b"
    );
    record.verify("team-a", &common::config().profile).unwrap();
}
#[test]
fn all_invite_fields_and_signer_team_domain_are_verified() {
    let key = SecretKey::generate();
    let record = SignedRecord::sign(invite(), &key, "team-a", &common::config().profile).unwrap();
    let mut altered = vec![record.clone(); 6];
    altered[0].body.hash.push('f');
    altered[1].body.expires += 1;
    altered[2].body.person = Some("ada".into());
    altered[3].body.used_by = Some(key.public());
    altered[4].body.ts += 1;
    altered[5].body.reusable = true;
    for change in altered {
        assert!(change.verify("team-a", &common::config().profile).is_err());
    }
    assert!(record.verify("team-b", &common::config().profile).is_err());
    let mut other = common::config().profile;
    other.signing_prefix = "other/".into();
    assert!(record.verify("team-a", &other).is_err());
    let mut forged = record;
    forged.by = SecretKey::generate().public();
    assert!(forged.verify("team-a", &common::config().profile).is_err());
}
#[test]
fn link_fields_and_kind_cannot_be_substituted() {
    let key = SecretKey::generate();
    let record = SignedRecord::sign(link(), &key, "team-a", &common::config().profile).unwrap();
    let mut secret = record.clone();
    secret.body.secret.push('f');
    assert!(secret.verify("team-a", &common::config().profile).is_err());
    let mut clock = record.clone();
    clock.body.ts += 1;
    assert!(clock.verify("team-a", &common::config().profile).is_err());
    let payload = signing_payload(&common::config().profile, "team-a", &record.body).unwrap();
    let foreign = String::from_utf8(payload)
        .unwrap()
        .replace("/link/", "/invite/");
    let mut wrong = record;
    wrong.sig = hex(&key.sign(foreign.as_bytes()).to_bytes());
    assert!(wrong.verify("team-a", &common::config().profile).is_err());
}
#[test]
fn missing_reusable_normalizes_but_raw_omitted_or_sorted_signatures_fail() {
    let key = SecretKey::generate();
    let record = SignedRecord::sign(invite(), &key, "team-a", &common::config().profile).unwrap();
    let mut value = serde_json::to_value(&record).unwrap();
    value["body"].as_object_mut().unwrap().remove("reusable");
    let mut decoded: SignedRecord<InviteRecord> = serde_json::from_value(value.clone()).unwrap();
    assert!(!decoded.body.reusable);
    decoded.verify("team-a", &common::config().profile).unwrap();
    let raw = format!("fixture/signature/1/team-a/invite/{}", value["body"]);
    decoded.sig = hex(&key.sign(raw.as_bytes()).to_bytes());
    assert!(decoded.verify("team-a", &common::config().profile).is_err());
    let sorted = format!(
        "fixture/signature/1/team-a/invite/{}",
        serde_json::to_value(invite()).unwrap()
    );
    decoded.sig = hex(&key.sign(sorted.as_bytes()).to_bytes());
    assert!(decoded.verify("team-a", &common::config().profile).is_err());
}
