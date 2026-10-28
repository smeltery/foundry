use foundry_contracts::{AdapterEvent, PROTOCOL_VERSION, capability::*};

fn requirement(owner: Owner, capability: Capability, version: u32) -> Requirement {
    Requirement {
        owner,
        capability,
        version,
    }
}
fn supported(capability: Capability, version: u32) -> Advertisement<'static> {
    Advertisement::Versioned {
        capability,
        version,
        supported: true,
    }
}
fn operation(requirements: &[Requirement]) -> OperationRequirements<'_> {
    OperationRequirements {
        operation: "restore-continue",
        version: 2,
        profile: "current:config-7",
        requirements,
    }
}
fn owner<'a>(role: Owner, capabilities: &'a [Advertisement<'a>]) -> OwnerAdvertisement<'a> {
    OwnerAdvertisement {
        owner: role,
        operation: "restore-continue",
        version: 2,
        profile: Some("current:config-7"),
        capabilities,
    }
}

#[test]
fn every_owner_must_supply_its_exact_dependency_before_effects() {
    let required = [
        requirement(Owner::Backend, Capability::DurableAcceptance, 1),
        requirement(Owner::Adapter, Capability::Continuation, 2),
        requirement(Owner::Model, Capability::NativeValidation, 3),
        requirement(Owner::Policy, Capability::Execution, 1),
    ];
    let backend = [supported(Capability::DurableAcceptance, 1)];
    let adapter = [supported(Capability::Continuation, 2)];
    let model = [supported(Capability::NativeValidation, 3)];
    let policy = [supported(Capability::Execution, 1)];
    let mut offers = vec![
        owner(Owner::Backend, &backend),
        owner(Owner::Adapter, &adapter),
        owner(Owner::Model, &model),
        owner(Owner::Policy, &policy),
    ];
    let mut effects = 0;
    for missing in 0..4 {
        let removed = offers.remove(missing);
        let result = negotiate(&operation(&required), &offers);
        if result.is_ok() {
            effects += 1;
        }
        assert_eq!(result, Err(NegotiationError::MissingOwner(removed.owner)));
        offers.insert(missing, removed);
    }
    assert_eq!(effects, 0);
    let available = negotiate(&operation(&required), &offers).unwrap();
    effects += 1;
    assert_eq!(available.operation, "restore-continue");
    assert_eq!(available.version, 2);
    assert_eq!(available.profile, "current:config-7");
    assert_eq!(effects, 1);
    // This counter is a contract harness, not a runtime dispatch or admission test.
}

#[test]
fn owner_roles_cannot_substitute_for_each_other() {
    let declarations = [supported(Capability::Execution, 1)];
    for required in [Owner::Backend, Owner::Adapter, Owner::Model, Owner::Policy] {
        for offered in [Owner::Backend, Owner::Adapter, Owner::Model, Owner::Policy] {
            let requirements = [requirement(required, Capability::Execution, 1)];
            let result = negotiate(&operation(&requirements), &[owner(offered, &declarations)]);
            assert_eq!(result.is_ok(), required == offered);
        }
    }
}

#[test]
fn exact_version_and_support_cannot_be_inferred_from_newer_or_older() {
    let requirements = [requirement(Owner::Adapter, Capability::Continuation, 2)];
    for version in [1, 3, u32::MAX] {
        let declarations = [supported(Capability::Continuation, version)];
        assert_eq!(
            negotiate(
                &operation(&requirements),
                &[owner(Owner::Adapter, &declarations)]
            ),
            Err(NegotiationError::IncompatibleVersion {
                owner: Owner::Adapter,
                capability: Capability::Continuation
            })
        );
    }
    let mut declarations = [
        supported(Capability::Continuation, 3),
        Advertisement::Versioned {
            capability: Capability::Continuation,
            version: 2,
            supported: false,
        },
    ];
    for _ in 0..2 {
        assert_eq!(
            negotiate(
                &operation(&requirements),
                &[owner(Owner::Adapter, &declarations)]
            ),
            Err(NegotiationError::Unsupported {
                owner: Owner::Adapter,
                capability: Capability::Continuation
            })
        );
        declarations.reverse();
    }
    declarations[0] = supported(Capability::Continuation, 2);
    declarations[1] = supported(Capability::Continuation, 3);
    assert!(
        negotiate(
            &operation(&requirements),
            &[owner(Owner::Adapter, &declarations)]
        )
        .is_ok()
    );
}

#[test]
fn unknown_missing_and_legacy_are_distinct_without_alias_inference() {
    let requirements = [requirement(Owner::Adapter, Capability::MessageReceipt, 2)];
    let unknown = [Advertisement::Unknown {
        name: "message_ack_v2",
        version: 2,
    }];
    for declarations in [&[][..], &unknown[..]] {
        assert_eq!(
            negotiate(
                &operation(&requirements),
                &[owner(Owner::Adapter, declarations)]
            ),
            Err(NegotiationError::MissingCapability {
                owner: Owner::Adapter,
                capability: Capability::MessageReceipt
            })
        );
    }
    let legacy = [Advertisement::Legacy {
        capability: Capability::MessageReceipt,
    }];
    assert_eq!(
        negotiate(&operation(&requirements), &[owner(Owner::Adapter, &legacy)]),
        Err(NegotiationError::LegacyOnly {
            owner: Owner::Adapter,
            capability: Capability::MessageReceipt
        })
    );
    let combined = [
        unknown[0],
        legacy[0],
        supported(Capability::MessageReceipt, 2),
    ];
    assert!(
        negotiate(
            &operation(&requirements),
            &[owner(Owner::Adapter, &combined)]
        )
        .is_ok()
    );
}

#[test]
fn receipt_capture_export_restore_continue_and_validation_never_imply_each_other() {
    let capabilities = [
        Capability::MessageReceipt,
        Capability::Capture,
        Capability::Export,
        Capability::InertRestore,
        Capability::Continuation,
        Capability::NativeValidation,
        Capability::PermissionRequests,
        Capability::DurableAcceptance,
    ];
    for needed in capabilities {
        for offered in capabilities {
            let requirements = [requirement(Owner::Adapter, needed, 1)];
            let declarations = [supported(offered, 1)];
            assert_eq!(
                negotiate(
                    &operation(&requirements),
                    &[owner(Owner::Adapter, &declarations)]
                )
                .is_ok(),
                needed == offered
            );
        }
    }
}

#[test]
fn operation_and_complete_profile_are_acknowledged_exactly() {
    let requirements = [requirement(Owner::Backend, Capability::Execution, 1)];
    let declarations = [supported(Capability::Execution, 1)];
    for (profile, expected) in [
        (
            None,
            NegotiationError::ProfileNotAcknowledged(Owner::Backend),
        ),
        (
            Some("legacy:config-7"),
            NegotiationError::ProfileMismatch(Owner::Backend),
        ),
        (
            Some("current:config-8"),
            NegotiationError::ProfileMismatch(Owner::Backend),
        ),
        (
            Some("current"),
            NegotiationError::ProfileMismatch(Owner::Backend),
        ),
    ] {
        let mut offer = owner(Owner::Backend, &declarations);
        offer.profile = profile;
        assert_eq!(
            negotiate(&operation(&requirements), &[offer]),
            Err(expected)
        );
    }
    for changed_version in [false, true] {
        let mut offer = owner(Owner::Backend, &declarations);
        if changed_version {
            offer.version = 1;
        } else {
            offer.operation = "start";
        }
        assert_eq!(
            negotiate(&operation(&requirements), &[offer]),
            Err(NegotiationError::OperationMismatch(Owner::Backend))
        );
    }
}

#[test]
fn duplicate_and_conflicting_declarations_are_rejected_not_coalesced() {
    let required = requirement(Owner::Backend, Capability::Execution, 1);
    let declarations = [supported(Capability::Execution, 1)];
    assert_eq!(
        negotiate(&operation(&[required, required]), &[]),
        Err(NegotiationError::DuplicateRequirement)
    );
    assert_eq!(
        negotiate(
            &operation(&[required]),
            &[
                owner(Owner::Backend, &declarations),
                owner(Owner::Backend, &declarations)
            ]
        ),
        Err(NegotiationError::DuplicateOwner)
    );
    for duplicate in [
        [declarations[0], declarations[0]],
        [
            declarations[0],
            Advertisement::Versioned {
                capability: Capability::Execution,
                version: 1,
                supported: false,
            },
        ],
        [Advertisement::Legacy {
            capability: Capability::Execution,
        }; 2],
        [Advertisement::Unknown {
            name: "future",
            version: 1,
        }; 2],
    ] {
        assert_eq!(
            negotiate(
                &operation(&[required]),
                &[owner(Owner::Backend, &duplicate)]
            ),
            Err(NegotiationError::DuplicateAdvertisement)
        );
    }
}

#[test]
fn bounded_inputs_reject_empty_invalid_and_oversized_descriptors() {
    let required = [requirement(Owner::Backend, Capability::Execution, 1)];
    for id in ["", "with space", "newline\n", "λ", "path/value"] {
        let mut request = operation(&required);
        request.operation = id;
        assert_eq!(
            negotiate(&request, &[]),
            Err(NegotiationError::InvalidDescriptor)
        );
    }
    let long = "x".repeat(MAX_ID_BYTES + 1);
    let mut request = operation(&required);
    request.profile = &long;
    assert_eq!(negotiate(&request, &[]), Err(NegotiationError::Limit));
    assert_eq!(
        negotiate(&operation(&[]), &[]),
        Err(NegotiationError::InvalidDescriptor)
    );
    let many = vec![required[0]; MAX_REQUIREMENTS + 1];
    assert_eq!(
        negotiate(&operation(&many), &[]),
        Err(NegotiationError::Limit)
    );
    let many = vec![supported(Capability::Execution, 1); MAX_ADVERTISEMENTS + 1];
    assert_eq!(
        negotiate(&operation(&required), &[owner(Owner::Backend, &many)]),
        Err(NegotiationError::Limit)
    );
    let many: Vec<_> = (0..5).map(|_| owner(Owner::Backend, &[])).collect();
    assert_eq!(
        negotiate(&operation(&required), &many),
        Err(NegotiationError::Limit)
    );
}

#[test]
fn positive_versions_and_all_extra_descriptors_are_validated() {
    let required = [requirement(Owner::Backend, Capability::Execution, 1)];
    let good = [supported(Capability::Execution, 1)];
    let mut request = operation(&required);
    request.version = 0;
    assert_eq!(
        negotiate(&request, &[]),
        Err(NegotiationError::InvalidDescriptor)
    );
    assert_eq!(
        negotiate(
            &operation(&[requirement(Owner::Backend, Capability::Execution, 0)]),
            &[]
        ),
        Err(NegotiationError::InvalidDescriptor)
    );
    for bad in [
        supported(Capability::Execution, 0),
        Advertisement::Unknown {
            name: "future",
            version: 0,
        },
        Advertisement::Unknown {
            name: "",
            version: 1,
        },
    ] {
        let extra = [bad];
        let mut offers = [owner(Owner::Backend, &good), owner(Owner::Model, &extra)];
        for _ in 0..2 {
            assert_eq!(
                negotiate(&operation(&required), &offers),
                Err(NegotiationError::InvalidDescriptor)
            );
            offers.reverse();
        }
    }
    let mut extra = owner(Owner::Policy, &[]);
    extra.profile = None;
    assert_eq!(
        negotiate(
            &operation(&required),
            &[owner(Owner::Backend, &good), extra]
        ),
        Err(NegotiationError::ProfileNotAcknowledged(Owner::Policy))
    );
}

#[test]
fn ready_wire_one_remains_unchanged_and_does_not_become_target_advertisement() {
    let ready: AdapterEvent = serde_json::from_str(r#"{"type":"ready","version":1}"#).unwrap();
    assert_eq!(
        ready,
        AdapterEvent::Ready {
            version: PROTOCOL_VERSION,
            capabilities: vec![]
        }
    );
    assert_eq!(
        serde_json::to_string(&ready).unwrap(),
        r#"{"type":"ready","version":1}"#
    );
    let ready: AdapterEvent = serde_json::from_str(
        r#"{"type":"ready","version":1,"capabilities":["message_ack_v2","future"]}"#,
    )
    .unwrap();
    let AdapterEvent::Ready { capabilities, .. } = ready else {
        panic!("ready")
    };
    assert_eq!(capabilities, ["message_ack_v2", "future"]);
}
