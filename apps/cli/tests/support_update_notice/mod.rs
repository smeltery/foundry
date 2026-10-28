mod isolation;

use clap::Parser;
use f5y::{CommandError, Request, Response, args::Cli, configuration::*, execute};
use serde_json::json;
use std::{cell::RefCell, rc::Rc};

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Snapshot(Scope),
    Account,
    Prepare,
    Daily,
    Warning,
    Error(UpdateFailure),
    Installed,
    Updated(String),
}
pub type Ledger = Rc<RefCell<Vec<Event>>>;
pub struct Source {
    pub input: Option<InputSnapshot>,
    ledger: Ledger,
}
impl Inputs for Source {
    fn snapshot(&mut self, scope: Scope) -> Result<InputSnapshot, ConfigFailure> {
        self.ledger.borrow_mut().push(Event::Snapshot(scope));
        self.input.take().ok_or(ConfigFailure::InputsUnavailable)
    }
}
pub struct Account(Ledger);
impl Accounts for Account {
    fn workspace(
        &mut self,
        credential: &Candidate,
        endpoint: &Selection<Candidate>,
    ) -> Result<Workspace, ConfigFailure> {
        self.0.borrow_mut().push(Event::Account);
        assert_eq!(credential.value.expose(), "private-key");
        let Selection::Selected(endpoint) = endpoint else {
            panic!("selected endpoint")
        };
        assert_eq!(endpoint.value.expose(), "private-endpoint");
        Ok(Workspace::new("workspace"))
    }
}
pub struct Owner {
    ledger: Ledger,
    check: Box<dyn Fn(&PreSessionPlan)>,
    result: Option<Result<Preparation, UpdateFailure>>,
}
impl PreSessionOwner for Owner {
    fn prepare(&mut self, plan: &PreSessionPlan) -> Result<Preparation, UpdateFailure> {
        self.ledger.borrow_mut().push(Event::Prepare);
        (self.check)(plan);
        self.result.take().expect("one preparation")
    }
}
pub struct Sink {
    ledger: Ledger,
    pub fail: bool,
}
impl UpdateSink for Sink {
    fn emit(&mut self, diagnostic: UpdateDiagnostic) -> Result<(), UpdateFailure> {
        let event = match diagnostic {
            UpdateDiagnostic::DailyUpdateNotice => Event::Daily,
            UpdateDiagnostic::PreUpdateWarning => Event::Warning,
            UpdateDiagnostic::Error(failure) => Event::Error(failure),
        };
        self.ledger.borrow_mut().push(event);
        if self.fail {
            Err(UpdateFailure::SinkUnavailable)
        } else {
            Ok(())
        }
    }
}
pub struct Launcher {
    ledger: Ledger,
    pub calls: Vec<(Request, ConfigurationContext)>,
    pub fail: bool,
}
impl Launcher {
    fn consume(
        &mut self,
        request: Request,
        context: ConfigurationContext,
        updated: bool,
    ) -> Result<Response, CommandError> {
        assert!(
            !serde_json::to_string(&request)
                .unwrap()
                .contains("private-")
        );
        assert!(!format!("{request:?}").contains("private-"));
        self.calls.push((request, context));
        if self.fail {
            return Err(CommandError::new("launch", "Launch unavailable"));
        }
        Ok(Response {
            text: if updated {
                "updated task"
            } else {
                "installed task"
            }
            .into(),
            json: json!({"result":if updated { "updated" } else { "installed" }}),
            exit_code: if updated { 9 } else { 7 },
        })
    }
}
impl UpdateLauncher for Launcher {
    fn launch_installed(
        &mut self,
        request: Request,
        context: ConfigurationContext,
    ) -> Result<Response, CommandError> {
        self.ledger.borrow_mut().push(Event::Installed);
        self.consume(request, context, false)
    }
    fn launch_updated(
        &mut self,
        handoff: UpdatedLaunch,
        request: Request,
        context: ConfigurationContext,
    ) -> Result<Response, CommandError> {
        assert_eq!(handoff.handle().expose(), "private-verified-handoff");
        self.ledger
            .borrow_mut()
            .push(Event::Updated(handoff.handle().expose().into()));
        self.consume(request, context, true)
    }
}
pub type Fixture = ConfiguredBackend<Source, Account, PreSessionExecutor<Owner, Sink, Launcher>>;
pub fn private(value: &str) -> Option<PrivateValue> {
    Some(PrivateValue::new(value))
}
pub fn snapshot(scope: Scope, auto: Option<&str>, notice: Option<&str>) -> InputSnapshot {
    let updates = UpdateInputs {
        auto_update: auto.and_then(private),
        no_update_notice: notice.and_then(private),
    };
    match scope {
        Scope::Session => InputSnapshot::Session(SessionInputs {
            environment_key: private("private-key"),
            environment_endpoint: private("private-endpoint"),
            selected_login: None,
            updates,
        }),
        Scope::Serve => InputSnapshot::Serve(ServeInputs {
            environment_token: private("private-token"),
            cwd: Some("/injected/work".into()),
            updates,
            ..Default::default()
        }),
        Scope::Connect => InputSnapshot::Connect(ConnectInputs::default()),
    }
}
pub fn prepared(
    outcome: PreparationOutcome,
    daily_notice: bool,
) -> Result<Preparation, UpdateFailure> {
    Ok(Preparation {
        outcome,
        daily_notice,
        notice: NoticeDecision::Allow,
    })
}
pub fn fixture(
    input: InputSnapshot,
    result: Result<Preparation, UpdateFailure>,
    check: impl Fn(&PreSessionPlan) + 'static,
) -> (Fixture, Ledger) {
    let ledger = Rc::new(RefCell::new(Vec::new()));
    let backend = ConfiguredBackend::new(
        Source {
            input: Some(input),
            ledger: ledger.clone(),
        },
        Account(ledger.clone()),
        PreSessionExecutor::new(
            Owner {
                ledger: ledger.clone(),
                check: Box::new(check),
                result: Some(result),
            },
            Sink {
                ledger: ledger.clone(),
                fail: false,
            },
            Launcher {
                ledger: ledger.clone(),
                calls: vec![],
                fail: false,
            },
        ),
    );
    (backend, ledger)
}
pub fn run(args: &[&str], backend: &mut Fixture) -> (u8, String, String) {
    let cli = Cli::try_parse_from(std::iter::once("f5y").chain(args.iter().copied())).unwrap();
    let (mut out, mut err) = (vec![], vec![]);
    let status = execute(cli, backend, &mut out, &mut err).unwrap();
    (
        status,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}
pub fn observation(observed: &SettingObservation, raw: Option<&str>) {
    match (observed, raw) {
        (SettingObservation::Absent, None) | (SettingObservation::ExactOne, Some("1")) => {}
        (SettingObservation::Unresolved(value), Some(expected)) if expected != "1" => {
            assert_eq!(value.expose(), expected)
        }
        _ => panic!("setting provenance changed"),
    }
}
pub fn assert_session(context: &ConfigurationContext) {
    let ConfigurationContext::Session {
        identity, endpoint, ..
    } = context
    else {
        panic!("session context")
    };
    let Selection::Selected(identity) = identity else {
        panic!("selected identity")
    };
    assert_eq!(identity.credential.value.expose(), "private-key");
    assert_eq!(identity.workspace.id(), "workspace");
    let Selection::Selected(endpoint) = endpoint else {
        panic!("selected endpoint")
    };
    assert_eq!(endpoint.value.expose(), "private-endpoint");
}
pub fn launched(backend: Fixture) -> (Request, ConfigurationContext) {
    let (_, _, executor) = backend.into_parts();
    let (_, _, mut launcher) = executor.into_parts();
    assert_eq!(launcher.calls.len(), 1);
    launcher.calls.pop().unwrap()
}
