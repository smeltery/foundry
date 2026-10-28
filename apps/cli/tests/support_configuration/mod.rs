use clap::Parser;
use f5y::{CommandError, Request, Response, args::Cli, configuration::*, execute};
use serde_json::json;

pub struct Source {
    pub input: Option<InputSnapshot>,
    pub calls: Vec<Scope>,
}
impl Inputs for Source {
    fn snapshot(&mut self, scope: Scope) -> Result<InputSnapshot, ConfigFailure> {
        self.calls.push(scope);
        self.input.take().ok_or(ConfigFailure::InputsUnavailable)
    }
}
#[derive(Default)]
pub struct AccountMap {
    pub calls: Vec<Origin>,
    pub endpoints: Vec<EndpointView>,
    pub fail: bool,
}
impl Accounts for AccountMap {
    fn workspace(
        &mut self,
        credential: &Candidate,
        endpoint: &Selection<Candidate>,
    ) -> Result<Workspace, ConfigFailure> {
        self.calls.push(credential.origin);
        self.endpoints.push(endpoint_view(endpoint));
        if self.fail {
            return Err(ConfigFailure::AccountUnavailable);
        }
        let workspace = match credential.value.expose() {
            "private-key-A" => "workspace-A",
            "private-login-B" => "workspace-B",
            _ => return Err(ConfigFailure::AccountUnavailable),
        };
        let suffix = match endpoint {
            Selection::Selected(candidate) if candidate.value.expose() == ENDPOINT_A => "@A",
            Selection::Selected(candidate) if candidate.value.expose() == ENDPOINT_B => "@B",
            _ => "",
        };
        Ok(Workspace::new(format!("{workspace}{suffix}")))
    }
}
#[derive(Default)]
pub struct Consumer {
    pub calls: Vec<(Request, ConfigurationContext)>,
    pub endpoints: Vec<EndpointView>,
    pub fail: bool,
}
impl Executor for Consumer {
    fn execute(
        &mut self,
        request: Request,
        context: ConfigurationContext,
    ) -> Result<Response, CommandError> {
        let endpoint = match &context {
            ConfigurationContext::Serve(_) => panic!("unexpected serve context"),
            ConfigurationContext::Session { endpoint, .. }
            | ConfigurationContext::Connect { endpoint, .. } => endpoint,
        };
        self.endpoints.push(endpoint_view(endpoint));
        self.calls.push((request, context));
        if self.fail {
            return Err(CommandError::new("fixture", "Executor unavailable"));
        }
        Ok(Response {
            text: "consumed".into(),
            json: json!({"consumed":true}),
            exit_code: 3,
        })
    }
}
pub type Fixture = ConfiguredBackend<Source, AccountMap, Consumer>;
pub fn fixture(input: InputSnapshot) -> Fixture {
    ConfiguredBackend::new(
        Source {
            input: Some(input),
            calls: vec![],
        },
        AccountMap::default(),
        Consumer::default(),
    )
}
pub fn run(args: &[&str], backend: &mut Fixture) -> (u8, Vec<u8>, Vec<u8>) {
    let cli = Cli::try_parse_from(std::iter::once("f5y").chain(args.iter().copied())).unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = execute(cli, backend, &mut out, &mut err).unwrap();
    (code, out, err)
}
pub fn private(value: &str) -> Option<PrivateValue> {
    Some(PrivateValue::new(value))
}
pub fn identity(selection: &Selection<Identity>, origin: Origin, workspace: &str) {
    let Selection::Selected(value) = selection else {
        panic!("expected selected identity")
    };
    assert_eq!(value.credential.origin, origin);
    assert_eq!(value.workspace.id(), workspace);
}

// Synthetic private inputs; endpoint-dependent workspaces prove account consumption.
pub const ENDPOINT_A: &str = "https://private-user:private-pass@one.invalid/api?key=private-A";
pub const ENDPOINT_B: &str = "https://private-user:private-pass@two.invalid/api?key=private-B";
pub type EndpointView = (Option<PendingCause>, Vec<(Origin, String)>);
pub fn endpoint_view(selection: &Selection<Candidate>) -> EndpointView {
    match selection {
        Selection::Selected(c) => (None, vec![(c.origin, c.value.expose().into())]),
        Selection::Pending { cause, candidates } => (
            Some(*cause),
            candidates
                .iter()
                .map(|c| (c.origin, c.value.expose().into()))
                .collect(),
        ),
    }
}

pub fn assert_private_request(request: &Request) {
    assert!(!serde_json::to_string(request).unwrap().contains("private-"));
    assert!(!format!("{request:?}").contains("private-"));
}
