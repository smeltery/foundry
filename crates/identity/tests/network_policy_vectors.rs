//! Replays `tests/identity/vectors/network_policy.json` against the policy
//! document and the egress evaluator. Authorization of the same cases is
//! replayed in `authz_vectors.rs`; this covers what follows it. Every expected
//! field must be understood, so a new field fails here instead of passing.
use std::fs;
use std::path::Path;

use foundry_identity::authz::WorkspaceId;
use foundry_identity::network_policy::*;
use serde_json::Value;

const BASELINE: &str = "baseline.example";

fn hosts(value: &Value) -> Vec<String> {
    let values = value.as_array().expect("host array");
    values
        .iter()
        .map(|host| host.as_str().expect("host").to_owned())
        .collect()
}

fn names(policy: &NetworkPolicy) -> Vec<String> {
    policy.hosts().iter().map(|host| host.to_string()).collect()
}

fn snapshot(policy: &NetworkPolicy) -> EgressPolicy {
    let baseline = Host::parse(BASELINE).expect("baseline host");
    EgressPolicy::at_execution_start([baseline], policy)
}

fn allowed(egress: &EgressPolicy, host: &str) -> bool {
    matches!(egress.evaluate(host), EgressDecision::Allow(_))
}

/// An execution started before the replace keeps its hosts; one started
/// after sees only the new list.
fn applies_at_next_start(saved: &NetworkPolicy, stored: &NetworkPolicy) {
    let (before, after) = (snapshot(saved), snapshot(stored));
    for host in saved.hosts() {
        assert!(allowed(&before, host.as_str()));
        assert_eq!(
            allowed(&after, host.as_str()),
            stored.hosts().contains(host)
        );
    }
    for host in stored.hosts() {
        assert!(allowed(&after, host.as_str()));
    }
}

/// The saved document a case starts from, and whether writes are enabled.
fn given(case: &Value) -> (NetworkPolicy, Writes) {
    let given = case.get("given").cloned().unwrap_or(Value::Null);
    let empty = NetworkPolicy::empty(WorkspaceId::new("ws-a"));
    let saved = match given.get("hosts") {
        Some(list) => {
            let request = ReplaceRequest {
                hosts: hosts(list),
                expected_revision: None,
            };
            empty
                .replace(&request, Writes::Enabled)
                .expect("given hosts")
        }
        None => empty,
    };
    let writes = match given.get("writes").and_then(Value::as_str) {
        None => Writes::Enabled,
        Some("disabled") => Writes::Disabled,
        Some(other) => panic!("unknown writes state {other}"),
    };
    (saved, writes)
}

/// The request hosts; a limit case without a list asks for that many hosts.
fn request(case: &Value) -> ReplaceRequest {
    let limit = case.get("limit").map(|limit| {
        assert_eq!(limit["name"], "policy_hosts");
        usize::try_from(limit["value"].as_u64().expect("limit value")).expect("fits")
    });
    let hosts = match (case["request"].get("hosts"), limit) {
        (Some(list), limit) => {
            let hosts = hosts(list);
            assert!(limit.is_none_or(|limit| limit == hosts.len()));
            hosts
        }
        (None, Some(count)) => (0..count)
            .map(|index| format!("h{index}.example"))
            .collect(),
        (None, None) => panic!("replace case without hosts"),
    };
    ReplaceRequest {
        hosts,
        expected_revision: None,
    }
}

/// Replays one case and returns the expected fields it checked.
fn replay(case: &Value) -> Vec<String> {
    let expect = case["expect"].as_object().expect("expect");
    let (saved, writes) = given(case);
    let mut checked = vec!["result".to_owned()];
    let stored = match case["operation"].as_str().expect("operation") {
        "network_policy.get" => Ok(saved.clone()),
        "network_policy.replace" => saved.replace(&request(case), writes),
        other => panic!("unexpected operation {other}"),
    };
    let stored = match (expect["result"].as_str(), stored) {
        (Some("ok"), Ok(stored)) => stored,
        (Some("error"), Err(error)) => {
            assert_eq!(Some(error.class.name()), expect["error"].as_str());
            checked.push("error".to_owned());
            return checked;
        }
        (wanted, actual) => panic!("want {wanted:?}, got {actual:?}"),
    };
    for (key, value) in expect.iter().filter(|(key, _)| *key != "result") {
        match (key.as_str(), value.as_str()) {
            ("hosts", _) => assert_eq!(names(&stored), hosts(value)),
            ("running_execution_revoked", _) if value == &Value::Bool(false) => {
                applies_at_next_start(&saved, &stored);
            }
            ("applies", Some("next_execution_start")) => applies_at_next_start(&saved, &stored),
            ("baseline", Some("unchanged")) => {
                assert_eq!(
                    snapshot(&stored).evaluate(BASELINE),
                    EgressDecision::Allow(Granted::Baseline)
                );
            }
            _ => panic!("unhandled expectation {key}: {value}"),
        }
        checked.push(key.clone());
    }
    checked
}

#[test]
fn network_policy_vectors_replay_exactly() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/identity/vectors/network_policy.json");
    let text = fs::read_to_string(&path).expect("vector file");
    let vectors: Value = serde_json::from_str(&text).expect("vector json");
    let cases = vectors["cases"].as_array().expect("cases");
    for case in cases {
        let checked = replay(case);
        let expected = case["expect"].as_object().expect("expect").len();
        assert_eq!(checked.len(), expected, "{}", case["name"]);
    }
    // Update deliberately when network policy vectors are added or removed.
    assert_eq!(cases.len(), 5);
}
