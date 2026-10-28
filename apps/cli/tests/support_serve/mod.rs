mod boundary;

use clap::Parser;
use f5y::{CommandError, Request, Response, args::Cli, configuration::*, execute};
use serde_json::{Value, json};

pub struct Source {
    pub snapshot: Option<InputSnapshot>,
    pub calls: Vec<Scope>,
}
impl Inputs for Source {
    fn snapshot(&mut self, scope: Scope) -> Result<InputSnapshot, ConfigFailure> {
        self.calls.push(scope);
        self.snapshot.take().ok_or(ConfigFailure::InputsUnavailable)
    }
}
pub struct NoAccounts;
impl Accounts for NoAccounts {
    fn workspace(
        &mut self,
        _: &Candidate,
        _: &Selection<Candidate>,
    ) -> Result<Workspace, ConfigFailure> {
        panic!("serve must not perform account resolution")
    }
}
pub struct Consumer {
    check: Box<dyn Fn(&ServeContext)>,
    pub requests: Vec<Request>,
    pub fail: bool,
}
impl Executor for Consumer {
    fn execute(
        &mut self,
        request: Request,
        context: ConfigurationContext,
    ) -> Result<Response, CommandError> {
        let ConfigurationContext::Serve(context) = context else {
            panic!("expected serve context")
        };
        (self.check)(&context);
        assert!(
            !request.command["arguments"]
                .as_object()
                .unwrap()
                .contains_key("token")
        );
        assert!(
            !serde_json::to_string(&request)
                .unwrap()
                .contains("private-")
        );
        assert!(!format!("{request:?}").contains("private-"));
        assert!(!context.persistence.permits_token_persistence());
        assert!(context.persistence.preserves_unrelated_saved_state());
        self.requests.push(request);
        if self.fail {
            return Err(CommandError::new("fixture", "Worker unavailable"));
        }
        Ok(Response {
            text: "consumed".into(),
            json: json!({"consumed":true}),
            exit_code: 3,
        })
    }
}
pub type Fixture = ConfiguredBackend<Source, NoAccounts, Consumer>;
pub fn fixture(inputs: ServeInputs, check: impl Fn(&ServeContext) + 'static) -> Fixture {
    ConfiguredBackend::new(
        Source {
            snapshot: Some(InputSnapshot::Serve(inputs)),
            calls: vec![],
        },
        NoAccounts,
        Consumer {
            check: Box::new(check),
            requests: vec![],
            fail: false,
        },
    )
}
pub fn private(value: &str) -> Option<PrivateValue> {
    Some(PrivateValue::new(value))
}
pub fn inputs() -> ServeInputs {
    ServeInputs {
        cwd: Some("/injected/work".into()),
        hostname: Some("injected-host".into()),
        ..Default::default()
    }
}
pub fn run(args: &[&str], backend: &mut Fixture) -> (u8, String, String) {
    let cli = Cli::try_parse_from(std::iter::once("f5y").chain(args.iter().copied())).unwrap();
    let (mut out, mut err) = (vec![], vec![]);
    let code = execute(cli, backend, &mut out, &mut err).unwrap();
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}
pub fn request(args: Value) -> Request {
    Request {
        version: 1,
        command: json!({"operation":"serve","arguments":args}),
    }
}
pub fn selected(value: &ProfileValue<String>, origin: ProfileOrigin, expected: &str) {
    let ProfileValue::Selected(candidate) = value else {
        panic!("expected selected profile value")
    };
    assert_eq!(candidate.origin, origin);
    assert_eq!(candidate.value, expected);
}
pub fn pending(
    value: &ProfileValue<String>,
    cause: PendingCause,
    expected: &[(ProfileOrigin, &str)],
) {
    let ProfileValue::Pending {
        cause: actual,
        candidates,
    } = value
    else {
        panic!("expected pending profile value")
    };
    assert_eq!(*actual, cause);
    assert_eq!(
        candidates
            .iter()
            .map(|c| (c.origin, c.value.as_str()))
            .collect::<Vec<_>>(),
        expected
    );
}
pub fn pending_identity(context: &ServeContext, cause: PendingCause, expected: &[(Origin, &str)]) {
    let Selection::Pending {
        cause: actual,
        candidates,
    } = &context.identity
    else {
        panic!("expected pending identity")
    };
    assert_eq!(*actual, cause);
    assert_eq!(
        candidates
            .iter()
            .map(|c| (c.origin, c.value.expose()))
            .collect::<Vec<_>>(),
        expected
    );
}
pub fn consumed(backend: Fixture) -> Request {
    let (source, _, mut consumer) = backend.into_parts();
    assert_eq!(source.calls, [Scope::Serve]);
    assert_eq!(consumer.requests.len(), 1);
    consumer.requests.pop().unwrap()
}
