use super::*;
const GOLDEN: &[u8] = include_bytes!("../../tests/fixtures/witness-v1.json");
fn fixture() -> WitnessBodyV1 {
    WitnessBodyV1::decode(GOLDEN).unwrap()
}
fn hex(value: [u8; 32]) -> String {
    value.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn independent_golden_bytes_and_domain_digest_are_frozen() {
    let body = fixture();
    let encoded = body.encode().unwrap();
    assert_eq!(encoded.bytes(), GOLDEN);
    assert_eq!(
        hex(encoded.digest()),
        "61d7eaa2ff7a1fdd5e291e7e8c17e10c726e04ed80572e209a3b1a72fa85bf3b"
    );
    assert_eq!(body.records.len(), 5);
    assert!(matches!(
        body.records[3],
        LogicalRecord::SyntheticNoEffect(_)
    ));
    assert!(matches!(body.records[4], LogicalRecord::DeniedResult(_)));
}

#[test]
fn unknown_duplicate_noncanonical_and_malformed_fields_reject() {
    let text = std::str::from_utf8(GOLDEN).unwrap();
    let mutations = [
        text.replacen("{", "{\"extra\":null,", 1),
        text.replacen("\"boundaries\":", "\"boundaries\":[],\"boundaries\":", 1),
        text.replacen("\"revision\":0", "\"revision\":0,\"revision\":0", 1),
        text.replacen("\"revision\":0", "\"revision\":-1", 1),
        text.replacen("\"revision\":0", "\"revision\":0.0", 1),
        text.replacen("\"revision\":0", "\"revision\":1e0", 1),
        text.replacen("\"revision\":0", "\"revision\":00", 1),
        text.replacen("\"revision\":0", "\"revision\":9223372036854775808", 1),
        text.replacen("\"profile\":\"wire\"", "\"profile\":\"w\\u0069re\"", 1),
        text.replacen("\"outcome\":\"success\"", "\"outcome\":\"uncertain\"", 1),
        text.replace("policy-🦀", "policy-\\ud800"),
        format!("{text}\n"),
        format!(" {text}"),
        format!("{text}null"),
    ];
    for value in mutations {
        assert!(WitnessBodyV1::decode(value.as_bytes()).is_err());
    }
    assert!(WitnessBodyV1::decode(&[0xff]).is_err());
}

#[test]
fn count_and_depth_preflight_reject_before_document_deserialization() {
    for (key, limit) in [
        ("records", 1024),
        ("prefix", 4096),
        ("mappings", 4096),
        ("boundaries", 4096),
    ] {
        let make = |count| format!("{{\"{key}\":[{}]}}", vec!["null"; count].join(","));
        assert!(preflight::check(make(limit).as_bytes()).is_ok());
        assert!(preflight::check(make(limit + 1).as_bytes()).is_err());
    }
    let nested = |n| format!("{}0{}", "[".repeat(n), "]".repeat(n));
    assert!(preflight::check(nested(15).as_bytes()).is_ok());
    assert!(preflight::check(nested(16).as_bytes()).is_err());
    assert!(WitnessBodyV1::decode(&vec![b' '; MAX_WITNESS_BYTES + 1]).is_err());
}

#[test]
fn utf8_identifier_bounds_and_fixed_digest_encoding_are_strict() {
    let mut body = fixture();
    body.header.trust_policy_revision = "🦀".repeat(32);
    assert!(body.encode().is_ok());
    body.header.trust_policy_revision.push('x');
    assert!(body.encode().is_err());
    for text in ["", " ", "line\n", "nul\0"] {
        body.header.trust_policy_revision = text.into();
        assert!(body.encode().is_err());
    }
    let text = std::str::from_utf8(GOLDEN).unwrap();
    assert!(
        WitnessBodyV1::decode(
            text.replacen(&"0a".repeat(32), &"0A".repeat(32), 1)
                .as_bytes()
        )
        .is_err()
    );
    assert!(WitnessBodyV1::decode(text.replacen(&"0a".repeat(32), "00", 1).as_bytes()).is_err());
}

#[test]
fn structural_duplicates_indices_and_unresolved_results_reject() {
    for mode in 0..12 {
        let mut body = fixture();
        match mode {
            0 => body.prefix[1].id = body.prefix[0].id.clone(),
            1 => body.prefix[0].parent_id = Some(body.prefix[1].id.clone()),
            2 => body.mappings[0].native_index = body.prefix.len() as u64,
            3 => body.mappings[0].logical_index = body.records.len() as u64,
            4 => body.mappings.push(MappingReference {
                logical_index: 0,
                native_index: 0,
                role: MappingRole::Input,
            }),
            5 => body.mappings[0].role = MappingRole::ToolResult,
            6 => {
                body.mappings.remove(0);
            }
            7 => body.boundaries[0].leaf_index = body.boundaries[0].end,
            8 => body.header.source_attempt_id = "not-uuid".into(),
            9 => {
                if let LogicalRecord::AdmittedResult(a) = &mut body.records[2] {
                    a.output_digest = None
                }
            }
            10 => {
                if let LogicalRecord::SyntheticNoEffect(s) = &mut body.records[3] {
                    s.identity.call_id = "call-1".into()
                }
            }
            _ => body.records.swap(0, 1),
        }
        assert!(body.encode().is_err(), "mode {mode}");
    }
}

fn with_prefix(count: usize) -> WitnessBodyV1 {
    let mut body = fixture();
    for n in body.prefix.len()..count {
        body.prefix.push(PrefixRecord {
            id: format!("entry-{n:04}"),
            parent_id: None,
            comparison_digest: [0; 32],
        });
    }
    body
}
#[test]
fn exact_encoded_limit_and_one_over_include_all_escaping() {
    let mut low = 5;
    let mut high = 4096;
    while low + 1 < high {
        let middle = (low + high) / 2;
        if with_prefix(middle).encode().is_ok() {
            low = middle;
        } else {
            high = middle;
        }
    }
    let mut body = with_prefix(low);
    let gap = MAX_WITNESS_BYTES - body.encode().unwrap().bytes().len();
    let last = body.prefix.last_mut().unwrap();
    assert!(last.id.len() + gap < 128);
    last.id.push_str(&"x".repeat(gap));
    let encoded = body.encode().unwrap();
    assert_eq!(encoded.bytes().len(), MAX_WITNESS_BYTES);
    assert!(WitnessBodyV1::decode(encoded.bytes()).is_ok());
    body.prefix.last_mut().unwrap().id.push('x');
    assert!(body.encode().is_err());
}

#[test]
fn logical_count_is_shared_by_all_variants_and_prefix_mapping_limits_are_independent() {
    let mut body = fixture();
    for index in body.records.len()..MAX_WITNESS_LOGICAL_RECORDS {
        body.records
            .push(LogicalRecord::Instruction(InstructionWitness {
                attempt_id: body.header.source_attempt_id.clone(),
                message_id: format!("message-{index}"),
                sequence: index as u64,
                text_bytes: 1,
                text_digest: [0; 32],
                delivery: DeliveryEvidence::OrdinaryEvent(EventIdentity {
                    id: format!("event-{index}"),
                    sequence: index as u64,
                    revision: 0,
                    digest: [0; 32],
                }),
            }));
        body.mappings.push(MappingReference {
            logical_index: index as u64,
            native_index: 0,
            role: MappingRole::Input,
        });
    }
    assert!(validate::structure(&body).is_ok());
    body.records.push(LogicalRecord::Prompt(PromptWitness {
        session_id: body.header.session_id.clone(),
        attempt_id: body.header.source_attempt_id.clone(),
        text_bytes: 1,
        text_digest: [0; 32],
    }));
    assert!(validate::structure(&body).is_err());
    let mut body = with_prefix(4096);
    assert!(validate::structure(&body).is_ok());
    body.prefix.push(PrefixRecord {
        id: "extra".into(),
        parent_id: None,
        comparison_digest: [0; 32],
    });
    assert!(validate::structure(&body).is_err());
    let mut body = with_prefix(4096);
    for index in 1..=4091 {
        body.mappings.push(MappingReference {
            logical_index: 0,
            native_index: index,
            role: MappingRole::Input,
        });
    }
    assert_eq!(body.mappings.len(), 4096);
    assert!(validate::structure(&body).is_ok());
    body.mappings.push(MappingReference {
        logical_index: 0,
        native_index: 4092,
        role: MappingRole::Input,
    });
    assert!(validate::structure(&body).is_err());
}

#[test]
fn parent_acceptance_and_failed_result_variants_round_trip_without_semantic_authority() {
    let mut body = fixture();
    body.header.parent = Some(WitnessParent {
        checkpoint_id: "33333333-3333-4333-8333-333333333333".into(),
        generation: 9,
        record_digest: [1; 32],
        witness_digest: [2; 32],
    });
    if let LogicalRecord::Instruction(instruction) = &mut body.records[1] {
        instruction.delivery = DeliveryEvidence::ContinueAcceptance(AcceptanceIdentity {
            restore_id: "44444444-4444-4444-8444-444444444444".into(),
            digest: [3; 32],
        });
    }
    if let LogicalRecord::AdmittedResult(result) = &mut body.records[2] {
        result.outcome = WitnessOutcome::Failed;
        result.output_digest = None;
    }
    let encoded = body.encode().unwrap();
    let read = WitnessBodyV1::decode(encoded.bytes()).unwrap();
    assert_eq!(read.header.parent.unwrap().generation, 9);
    assert!(matches!(
        read.records[1],
        LogicalRecord::Instruction(InstructionWitness {
            delivery: DeliveryEvidence::ContinueAcceptance(_),
            ..
        })
    ));
    assert!(matches!(
        read.records[2],
        LogicalRecord::AdmittedResult(AdmittedResultWitness {
            outcome: WitnessOutcome::Failed,
            output_digest: None,
            ..
        })
    ));
    assert!(matches!(
        read.records[3],
        LogicalRecord::SyntheticNoEffect(_)
    ));
    body.header.parent.as_mut().unwrap().generation = 0;
    assert!(body.encode().is_err());
}

#[test]
fn duplicate_durable_event_identity_cannot_hide_behind_a_changed_sequence() {
    let mut body = fixture();
    if let LogicalRecord::DeniedResult(denial) = &mut body.records[4] {
        denial.request_event.id = "accepted".into();
        denial.request_event.revision = 0;
        denial.request_event.sequence = 999;
    }
    assert!(body.encode().is_err());
    let mut body = fixture();
    if let LogicalRecord::DeniedResult(denial) = &mut body.records[4] {
        denial.request_event.sequence = 1;
    }
    assert!(body.encode().is_err());
}
