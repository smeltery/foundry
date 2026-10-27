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
    pub fail: bool,
}
impl Accounts for AccountMap {
    fn workspace(&mut self, credential: &Candidate) -> Result<Workspace, ConfigFailure> {
        self.calls.push(credential.origin);
        if self.fail {
            return Err(ConfigFailure::AccountUnavailable);
        }
        match credential.value.expose() {
            "private-key-A" => Ok(Workspace::new("workspace-A")),
            "private-login-B" => Ok(Workspace::new("workspace-B")),
            _ => Err(ConfigFailure::AccountUnavailable),
        }
    }
}
#[derive(Default)]
pub struct Consumer {
    pub calls: Vec<(Request, ConfigurationContext)>,
}
impl Executor for Consumer {
    fn execute(
        &mut self,
        request: Request,
        context: ConfigurationContext,
    ) -> Result<Response, CommandError> {
        self.calls.push((request, context));
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
