use crate::daemon_engine::Auth;
use iroh::{EndpointAddr, EndpointId};
use serde::{Deserialize, Serialize};

/// Network controls use the existing private session credential and connection.
/// No Debug: invitations, proof tokens and code addresses are privileged output.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NetworkEnvelope {
    pub id: u64,
    pub op: NetworkOperation,
    pub auth: Auth,
    pub command: Command,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NetworkOperation {
    Network,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Command {
    Status {},
    Form {
        name: String,
    },
    Invite {
        hours: u64,
        #[serde(default)]
        own: bool,
        #[serde(default)]
        short: bool,
    },
    Join {
        ticket: String,
        #[serde(default)]
        address: Option<EndpointAddr>,
        #[serde(default)]
        founder: Option<EndpointId>,
    },
    JoinCode {
        code: String,
        address: EndpointAddr,
        #[serde(default)]
        team: Option<String>,
        #[serde(default)]
        founder: Option<EndpointId>,
    },
    Connect {
        address: EndpointAddr,
    },
    Remove {
        endpoints: Vec<EndpointId>,
    },
    Rotate {},
    Depart {},
    CodePublications {},
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ControlError {
    Disabled,
    Unauthorized,
    Invalid,
    Busy,
    Storage,
    Transport,
    Rejected,
}
#[derive(Serialize)]
pub(crate) struct Status {
    pub endpoint: EndpointAddr,
    pub team: Option<String>,
    pub revision: Option<u64>,
    pub peers: Vec<EndpointId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn empty_controls_and_envelope_reject_unrecognized_fields() {
        for kind in ["status", "rotate", "depart", "code_publications"] {
            let mut value = json!({"id":1,"op":"network","auth":{
                "conversation":{"person":"a","machine":"b","name":"c"},
                "credential":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]},
                "command":{"kind":kind}});
            assert!(serde_json::from_value::<NetworkEnvelope>(value.clone()).is_ok());
            value["command"]["unexpected"] = json!(true);
            assert!(serde_json::from_value::<NetworkEnvelope>(value.clone()).is_err());
            value["command"]
                .as_object_mut()
                .unwrap()
                .remove("unexpected");
            value["unexpected"] = json!(true);
            assert!(serde_json::from_value::<NetworkEnvelope>(value).is_err());
        }
    }
}
