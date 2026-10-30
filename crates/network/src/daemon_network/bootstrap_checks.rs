use super::*;
use crate::daemon_config::ResolvedStartup;
pub(super) fn pins(state: &State, team: &str, founder: EndpointId) -> Result<(), ControlError> {
    if state
        .settings
        .expected_team
        .as_deref()
        .is_some_and(|p| p != team)
    {
        return Err(ControlError::Rejected);
    }
    let (expected, root) = match &state.settings.startup {
        ResolvedStartup::JoinTicket {
            ticket,
            expected_founder,
            ..
        } => (Some(ticket.team.as_str()), *expected_founder),
        ResolvedStartup::JoinCode {
            expected_team,
            expected_founder,
            ..
        } => (expected_team.as_deref(), *expected_founder),
        _ => (None, None),
    };
    if expected.is_some_and(|p| p != team) || root.is_some_and(|p| p != founder) {
        return Err(ControlError::Rejected);
    }
    Ok(())
}
pub(super) fn reconcile_team(
    common: Option<&str>,
    selected: Option<&str>,
) -> Result<Option<String>, ControlError> {
    if let (Some(common), Some(selected)) = (common, selected)
        && common != selected
    {
        return Err(ControlError::Rejected);
    }
    Ok(common.or(selected).map(str::to_owned))
}
pub(super) fn require_endpoint(
    expected: EndpointId,
    actual: EndpointId,
) -> Result<(), ControlError> {
    if expected != actual {
        return Err(ControlError::Rejected);
    }
    Ok(())
}

pub(super) async fn validate_local(state: &State) -> Result<(), ControlError> {
    let view = state
        .authority
        .routing_view()
        .await
        .map_err(|_| ControlError::Rejected)?;
    let local = view
        .members
        .iter()
        .find(|m| m.active && m.endpoint == view.local)
        .ok_or(ControlError::Rejected)?;
    if local.person != state.person || local.machine != state.machine {
        return Err(ControlError::Rejected);
    }
    Ok(())
}

pub(super) fn validate_incoming_local(
    state: &State,
    document: &crate::team::TeamDocument,
) -> Result<(), ControlError> {
    let record = document
        .members
        .get(&state.authority.local().to_string())
        .ok_or(ControlError::Rejected)?;
    let local = &record.body;
    if local.endpoint != state.authority.local()
        || !local.active
        || local.person != state.person
        || local.machine != state.machine
    {
        return Err(ControlError::Rejected);
    }
    Ok(())
}
