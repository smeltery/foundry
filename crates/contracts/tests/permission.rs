use foundry_contracts::permission::*;

fn binding() -> PermissionBinding {
    PermissionBinding {
        session_id: "session-a".into(),
        attempt_id: "attempt-a".into(),
        call_id: "call-a".into(),
        tool: "write.create".into(),
        input_digest: [7; 32],
        profile: PermissionProfile::Current,
    }
}
fn request() -> PermissionRequest {
    PermissionRequest::new(binding(), 100, 0).unwrap()
}
struct Consumer {
    calls: usize,
}
impl Consumer {
    fn dispatch(
        &mut self,
        request: &mut PermissionRequest,
        permit: PermissionPermit,
        current: &PermissionBinding,
        now: u64,
    ) -> Result<(), PermissionError> {
        request.consume(permit, current, now)?;
        self.calls += 1;
        Ok(())
    }
}
fn allow(request: &mut PermissionRequest) -> PermissionPermit {
    request
        .decide(&binding(), PermissionDecision::Allow, 1)
        .unwrap();
    request.take_permit(&binding(), 2).unwrap()
}
#[test]
fn trusted_existing_grant_can_allow_without_interactive_round_trip() {
    let mut request = request();
    let permit = allow(&mut request);
    let mut consumer = Consumer { calls: 0 };
    consumer
        .dispatch(&mut request, permit, &binding(), 3)
        .unwrap();
    assert_eq!(consumer.calls, 1);
    assert_eq!(request.state(), PermissionState::Consumed);
    assert!(matches!(
        request.take_permit(&binding(), 4),
        Err(PermissionError::InvalidState)
    ));
    assert_eq!(
        request.decide(&binding(), PermissionDecision::Allow, 4),
        Ok(DecisionResult::AlreadyDecided(PermissionState::Consumed))
    );
    assert_eq!(
        request.decide(&binding(), PermissionDecision::Deny, 4),
        Err(PermissionError::ConflictingDecision)
    );
    request.cancel();
    assert_eq!(request.state(), PermissionState::Consumed);
    assert_eq!(consumer.calls, 1);
}
#[test]
fn pending_noninteractive_outcomes_do_not_deny_or_grant() {
    let mut request = request();
    for (mode, expected) in [
        (PendingMode::Interactive, PermissionDisposition::Pending),
        (
            PendingMode::ActionRequired,
            PermissionDisposition::ActionRequired,
        ),
        (PendingMode::Unsupported, PermissionDisposition::Unsupported),
    ] {
        assert_eq!(request.disposition(mode, 1), Ok(expected));
        assert_eq!(request.state(), PermissionState::Pending);
        assert!(matches!(
            request.take_permit(&binding(), 1),
            Err(PermissionError::InvalidState)
        ));
    }
    assert_eq!(
        request.decide(&binding(), PermissionDecision::Pending, 2),
        Ok(DecisionResult::Applied(PermissionState::Pending))
    );
    request
        .decide(&binding(), PermissionDecision::Allow, 3)
        .unwrap();
    assert!(request.take_permit(&binding(), 3).is_ok());
}
#[test]
fn denial_and_unsupported_are_distinct_terminal_decisions() {
    for (decision, state) in [
        (PermissionDecision::Deny, PermissionState::Denied),
        (
            PermissionDecision::Unsupported,
            PermissionState::Unsupported,
        ),
    ] {
        let mut request = request();
        assert_eq!(
            request.decide(&binding(), decision, 1),
            Ok(DecisionResult::Applied(state))
        );
        assert_eq!(
            request.decide(&binding(), decision, 2),
            Ok(DecisionResult::AlreadyDecided(state))
        );
        assert_eq!(
            request.decide(&binding(), PermissionDecision::Allow, 3),
            Err(PermissionError::ConflictingDecision)
        );
        assert!(matches!(
            request.take_permit(&binding(), 4),
            Err(PermissionError::InvalidState)
        ));
    }
}
#[test]
fn every_bound_field_prevents_cross_call_consumption() {
    let mut variants = Vec::new();
    let mut next = binding();
    next.session_id = "session-b".into();
    variants.push(next);
    let mut next = binding();
    next.attempt_id = "attempt-b".into();
    variants.push(next);
    let mut next = binding();
    next.call_id = "call-b".into();
    variants.push(next);
    let mut next = binding();
    next.tool = "read".into();
    variants.push(next);
    let mut next = binding();
    next.input_digest[0] ^= 1;
    variants.push(next);
    let mut next = binding();
    next.profile = PermissionProfile::Legacy;
    variants.push(next);
    for current in variants {
        let mut request = request();
        let permit = allow(&mut request);
        let mut consumer = Consumer { calls: 0 };
        assert_eq!(
            consumer.dispatch(&mut request, permit, &current, 3),
            Err(PermissionError::BindingMismatch)
        );
        assert_eq!(consumer.calls, 0);
        assert_eq!(request.state(), PermissionState::Stale);
        assert!(matches!(
            request.take_permit(&binding(), 4),
            Err(PermissionError::Stale)
        ));
    }
}
#[test]
fn cancellation_expiry_and_invalidation_revoke_issued_authority() {
    for (mode, error, now) in [
        (0, PermissionError::Cancelled, 3),
        (1, PermissionError::Expired, 100),
        (2, PermissionError::Stale, 3),
    ] {
        let mut request = request();
        let permit = allow(&mut request);
        match mode {
            0 => request.cancel(),
            2 => request.invalidate(),
            _ => {}
        }
        let mut consumer = Consumer { calls: 0 };
        assert_eq!(
            consumer.dispatch(&mut request, permit, &binding(), now),
            Err(error)
        );
        assert_eq!(
            request.decide(&binding(), PermissionDecision::Allow, now),
            Err(error)
        );
        assert_eq!(consumer.calls, 0);
    }
}
#[test]
fn duplicate_allow_never_reissues_a_lost_permit() {
    let mut request = request();
    let permit = allow(&mut request);
    drop(permit);
    assert_eq!(
        request.decide(&binding(), PermissionDecision::Allow, 3),
        Ok(DecisionResult::AlreadyDecided(PermissionState::Issued))
    );
    assert!(matches!(
        request.take_permit(&binding(), 3),
        Err(PermissionError::InvalidState)
    ));
}
#[test]
fn identical_requests_cannot_exchange_permits() {
    let mut first = request();
    let mut second = request();
    let foreign = allow(&mut first);
    let own = allow(&mut second);
    let mut consumer = Consumer { calls: 0 };
    assert_eq!(
        consumer.dispatch(&mut second, foreign, &binding(), 3),
        Err(PermissionError::ForeignPermit)
    );
    assert_eq!(consumer.calls, 0);
    consumer.dispatch(&mut second, own, &binding(), 3).unwrap();
    assert_eq!(consumer.calls, 1);
    assert!(matches!(
        first.take_permit(&binding(), 3),
        Err(PermissionError::InvalidState)
    ));
}
#[test]
fn expiry_and_stale_decisions_are_checked_before_allow() {
    let mut expired = request();
    assert_eq!(
        expired.decide(&binding(), PermissionDecision::Allow, 100),
        Err(PermissionError::Expired)
    );
    let mut cancelled = request();
    cancelled.cancel();
    assert_eq!(
        cancelled.decide(&binding(), PermissionDecision::Allow, 1),
        Err(PermissionError::Cancelled)
    );
    let mut stale = request();
    let mut current = binding();
    current.attempt_id = "new".into();
    assert_eq!(
        stale.decide(&current, PermissionDecision::Allow, 1),
        Err(PermissionError::BindingMismatch)
    );
    let mut clock = request();
    clock.disposition(PendingMode::Interactive, 20).unwrap();
    assert_eq!(
        clock.decide(&binding(), PermissionDecision::Allow, 19),
        Err(PermissionError::TimeRegression)
    );
    assert!(matches!(
        PermissionRequest::new(binding(), 1, 1),
        Err(PermissionError::InvalidExpiry)
    ));
    let mut invalid = binding();
    invalid.call_id.clear();
    assert!(matches!(
        PermissionRequest::new(invalid, 100, 0),
        Err(PermissionError::InvalidBinding)
    ));
    assert_eq!(
        PermissionError::BindingMismatch.to_string(),
        "permission binding mismatch"
    );
}
