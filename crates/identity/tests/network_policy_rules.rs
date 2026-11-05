//! Revision, validation and default-deny rules the vectors do not spell out.
use foundry_identity::authz::WorkspaceId;
use foundry_identity::network_policy::*;

fn request(hosts: &[&str], expected_revision: Option<Revision>) -> ReplaceRequest {
    ReplaceRequest {
        hosts: hosts.iter().map(|host| (*host).to_owned()).collect(),
        expected_revision,
    }
}

fn replace(policy: &NetworkPolicy, hosts: &[&str], expected: Revision) -> Replacement {
    policy
        .replace(&request(hosts, Some(expected)), Writes::Enabled)
        .expect("replace")
}

fn issue(policy: &NetworkPolicy, hosts: &[&str], expected: Option<Revision>) -> PolicyError {
    policy
        .replace(&request(hosts, expected), Writes::Enabled)
        .expect_err("rejected")
}

fn empty() -> NetworkPolicy {
    NetworkPolicy::empty(WorkspaceId::new("ws-a"))
}

fn names(policy: &NetworkPolicy) -> Vec<&str> {
    policy.hosts().iter().map(Host::as_str).collect()
}

#[test]
fn stale_revision_is_conflict_and_keeps_the_stored_list() {
    let first = replace(&empty(), &["registry.example"], Revision::INITIAL);
    assert_eq!(first.policy.revision, Revision(1));
    let second = replace(&first.policy, &["mirror.example"], Revision(1)).policy;
    assert_eq!(second.revision, Revision(2));

    let stale = issue(&second, &["other.example"], Some(Revision(1)));
    assert_eq!(stale.class.name(), "conflict");
    assert_eq!(
        stale.issue,
        PolicyIssue::StaleRevision {
            current: Revision(2)
        }
    );
    let ahead = issue(&second, &["other.example"], Some(Revision(9)));
    assert_eq!(ahead.class, PolicyErrorClass::Conflict);
    assert_eq!(names(&second), ["mirror.example"]);
}

#[test]
fn concurrent_replaces_carry_the_revision_the_store_must_compare() {
    // Both are valid against the same document; only the store's
    // compare-and-swap on `based_on` can let exactly one commit.
    let saved = empty();
    let left = replace(&saved, &["left.example"], Revision::INITIAL);
    let right = replace(&saved, &["right.example"], Revision::INITIAL);
    for replacement in [&left, &right] {
        assert_eq!(replacement.based_on, Revision::INITIAL);
        assert_eq!(replacement.policy.revision, Revision(1));
        assert!(replacement.changed());
    }
    // Once left commits, right recomputed from the stored document conflicts.
    let retried = issue(&left.policy, &["right.example"], Some(Revision::INITIAL));
    assert_eq!(retried.class, PolicyErrorClass::Conflict);
}

#[test]
fn missing_revision_is_invalid_request() {
    let missing = issue(&empty(), &["registry.example"], None);
    assert_eq!(missing.class.name(), "invalid_request");
    assert_eq!(missing.issue, PolicyIssue::MissingRevision);
}

#[test]
fn repeating_an_applied_replace_is_safe_and_keeps_the_revision() {
    let first = replace(&empty(), &["registry.example"], Revision::INITIAL).policy;
    let repeated = replace(&first, &["Registry.Example"], Revision::INITIAL);
    assert!(!repeated.changed());
    assert_eq!(repeated.based_on, Revision(1));
    assert_eq!(repeated.policy, first);
}

#[test]
fn validation_rejects_the_whole_request() {
    let fifty: Vec<String> = (0..MAX_HOSTS)
        .map(|index| format!("h{index}.example"))
        .collect();
    let fifty: Vec<&str> = fifty.iter().map(String::as_str).collect();
    let stored = replace(&empty(), &fifty, Revision::INITIAL).policy;
    assert_eq!(stored.hosts().len(), MAX_HOSTS);

    let cases: [(&[&str], PolicyIssue); 3] = [
        (
            &["ok.example", "ok.example."],
            PolicyIssue::InvalidHost {
                index: 1,
                error: HostError::Label,
            },
        ),
        (
            &["a.example", "b.example", "A.example"],
            PolicyIssue::DuplicateHost { index: 2 },
        ),
        (
            &["*.example"],
            PolicyIssue::InvalidHost {
                index: 0,
                error: HostError::Character,
            },
        ),
    ];
    for (hosts, wanted) in cases {
        let error = issue(&empty(), hosts, Some(Revision::INITIAL));
        assert_eq!(error.class.name(), "invalid_request");
        assert_eq!(error.issue, wanted, "{hosts:?}");
    }
    // Writes being disabled is reported before anything in the body.
    let disabled = empty().replace(&request(&["*"], None), Writes::Disabled);
    assert_eq!(disabled.expect_err("disabled").class.name(), "unavailable");
}

#[test]
fn host_grammar_accepts_only_qualified_dns_names() {
    let long_label = format!("{}.example", "a".repeat(64));
    let long_host = format!("{}example", "abc.".repeat(63));
    let rejected = [
        ("", HostError::Length),
        (long_host.as_str(), HostError::Length),
        ("https://a.example", HostError::Character),
        ("a.example:443", HostError::Character),
        ("a.example/path", HostError::Character),
        ("[::1]", HostError::Character),
        ("caf\u{e9}.example", HostError::Character),
        ("a..example", HostError::Label),
        ("-a.example", HostError::Label),
        (long_label.as_str(), HostError::Label),
        ("localhost", HostError::NotQualified),
        ("10.0.0.1", HostError::NotQualified),
        // Hex and mixed forms URL parsers read as 169.254.169.254 or 127.0.0.1.
        ("0xa9.0xfe.0xa9.0xfe", HostError::NotQualified),
        ("169.254.169.0xfe", HostError::NotQualified),
        ("127.0.0.0x1", HostError::NotQualified),
        ("0x7f000001.0x0", HostError::NotQualified),
    ];
    for (input, wanted) in rejected {
        assert_eq!(Host::parse(input), Err(wanted), "{input}");
    }
    for accepted in ["Api.Example", "0x7f.example", "10.0.0.1.example"] {
        let host = Host::parse(accepted).expect(accepted);
        assert_eq!(host.as_str(), accepted.to_ascii_lowercase());
    }
}

#[test]
fn evaluator_denies_anything_unparsed_or_unlisted() {
    let policy = replace(&empty(), &["registry.example"], Revision::INITIAL).policy;
    let baseline = [Host::parse("baseline.example").expect("host")];
    let egress = EgressPolicy::at_execution_start(baseline, &policy);
    let decisions = [
        ("baseline.example", EgressDecision::Allow(Granted::Baseline)),
        (
            "REGISTRY.example",
            EgressDecision::Allow(Granted::Workspace),
        ),
        (
            "sub.registry.example",
            EgressDecision::Deny(Denied::NotListed),
        ),
        ("other.example", EgressDecision::Deny(Denied::NotListed)),
        (
            "registry.example.",
            EgressDecision::Deny(Denied::Unparsed(HostError::Label)),
        ),
        (
            "registry.example:443",
            EgressDecision::Deny(Denied::Unparsed(HostError::Character)),
        ),
        (
            "",
            EgressDecision::Deny(Denied::Unparsed(HostError::Length)),
        ),
    ];
    for (destination, wanted) in decisions {
        assert_eq!(egress.evaluate(destination), wanted, "{destination}");
    }
    // No baseline and no saved policy allows nothing.
    let closed = EgressPolicy::at_execution_start([], &empty());
    assert_eq!(
        closed.evaluate("registry.example"),
        EgressDecision::Deny(Denied::NotListed)
    );
}
