use clap::Parser;
use f5y::{Backend, CommandError, Request, Response, args::Cli, configuration::*, dispatch};
use serde_json::{Value, json};

#[derive(Debug, PartialEq)]
pub enum Observed<T> {
    Selected(ProfileOrigin, T),
    Pending(PendingCause, Vec<(ProfileOrigin, T)>),
}
fn observe<T>(value: ProfileValue<T>) -> Observed<T> {
    match value {
        ProfileValue::Selected(c) => Observed::Selected(c.origin, c.value),
        ProfileValue::Pending { cause, candidates } => Observed::Pending(
            cause,
            candidates
                .into_iter()
                .map(|c| (c.origin, c.value))
                .collect(),
        ),
    }
}
pub fn selected<T>(origin: ProfileOrigin, value: T) -> Observed<T> {
    Observed::Selected(origin, value)
}
pub fn missing<T>() -> Observed<T> {
    Observed::Pending(PendingCause::Missing, vec![])
}
pub fn pending<T>(origin: ProfileOrigin, value: T) -> Observed<T> {
    Observed::Pending(PendingCause::OwnerPolicy, vec![(origin, value)])
}

pub struct ProfileView {
    pub name: Observed<String>,
    pub folder: Observed<String>,
    pub ca: Observed<String>,
    pub chrome: Observed<bool>,
    pub logins: Observed<bool>,
    pub awake: Observed<bool>,
    pub minimal: Observed<bool>,
    pub changes: ProfileChanges,
    pub immediate_awake: bool,
    pub immediate_folders: bool,
    pub preserves_sessions: bool,
    pub preserves_desktop: bool,
}
#[derive(Default)]
pub struct Consumer {
    pub calls: Vec<(Value, ProfileView)>,
}
impl Executor for Consumer {
    fn execute(
        &mut self,
        request: Request,
        context: ConfigurationContext,
    ) -> Result<Response, CommandError> {
        let ConfigurationContext::Connect { profile, .. } = context else {
            panic!("connect context required")
        };
        let p = *profile;
        // Inspect every field and intent inside the required consumer, before response.
        let view = ProfileView {
            name: observe(p.name),
            folder: observe(p.folder),
            ca: observe(p.proxy_ca_file),
            chrome: observe(p.user_chrome),
            logins: observe(p.local_logins),
            awake: observe(p.keep_awake),
            minimal: observe(p.minimal),
            immediate_awake: p.changes.keep_awake_without_reconnect(),
            immediate_folders: p.changes.folders_apply_immediately(),
            preserves_sessions: p.changes.preserves_other_folder_sessions(),
            preserves_desktop: p.changes.preserves_installed_desktop_profile(),
            changes: p.changes,
        };
        self.calls
            .push((serde_json::to_value(request).unwrap(), view));
        Ok(Response {
            text: "profile consumed".into(),
            json: json!({"consumed":true}),
            exit_code: 0,
        })
    }
}
pub struct Source {
    pub input: Option<ConnectInputs>,
    pub calls: Vec<Scope>,
}
impl Inputs for Source {
    fn snapshot(&mut self, scope: Scope) -> Result<InputSnapshot, ConfigFailure> {
        self.calls.push(scope);
        Ok(InputSnapshot::Connect(
            self.input.take().expect("one snapshot"),
        ))
    }
}
#[derive(Default)]
pub struct AccountsFixture {
    pub calls: Vec<Origin>,
}
impl Accounts for AccountsFixture {
    fn workspace(
        &mut self,
        credential: &Candidate,
        _: &Selection<Candidate>,
    ) -> Result<Workspace, ConfigFailure> {
        self.calls.push(credential.origin);
        assert_eq!(credential.value.expose(), "fixture-key");
        Ok(Workspace::new("fixture-workspace"))
    }
}
pub type Fixture = ConfiguredBackend<Source, AccountsFixture, Consumer>;
pub fn fixture(inputs: ConnectInputs) -> Fixture {
    ConfiguredBackend::new(
        Source {
            input: Some(inputs),
            calls: vec![],
        },
        AccountsFixture::default(),
        Consumer::default(),
    )
}
pub fn run(args: &[&str], inputs: ConnectInputs) -> (Value, ProfileView, Vec<Origin>) {
    let mut backend = fixture(inputs);
    let cli = Cli::try_parse_from(std::iter::once("f5y").chain(args.iter().copied())).unwrap();
    let response = dispatch(cli, &mut backend).unwrap();
    consumed(backend, response)
}
// Empty PathBuf arguments are rejected by Clap; R1 concerns direct backend inputs.
pub fn run_empty(inputs: ConnectInputs) -> (Value, ProfileView, Vec<Origin>) {
    let mut backend = fixture(inputs);
    let response = backend.execute(Request {
        version: 1,
        command: json!({"operation":"connect","arguments":{"name":"","folder":"","proxy_ca_file":""}}),
    }).unwrap();
    consumed(backend, response)
}
fn consumed(backend: Fixture, response: Response) -> (Value, ProfileView, Vec<Origin>) {
    assert_eq!(response.text, "profile consumed");
    let (source, accounts, mut consumer) = backend.into_parts();
    assert_eq!(source.calls, [Scope::Connect]);
    assert_eq!(consumer.calls.len(), 1);
    let (request, profile) = consumer.calls.pop().unwrap();
    (request, profile, accounts.calls)
}
pub fn facts(lifecycle: ProfileLifecycle) -> ProfileFacts {
    ProfileFacts {
        lifecycle,
        kind: ProfileKind::Personal,
        platform: ProfilePlatform::MacOs,
        home: Some("/injected/home".into()),
        cwd: Some("/injected/cwd".into()),
        hostname: Some("injected-host".into()),
        authentication: ProfileAuthentication::WithoutKey,
    }
}
pub fn inputs(facts: ProfileFacts, key: bool) -> ConnectInputs {
    ConnectInputs {
        profile: Some(Box::new(facts)),
        environment_key: key.then(|| PrivateValue::new("fixture-key")),
        ..ConnectInputs::default()
    }
}
pub fn saved() -> SavedProfile {
    SavedProfile {
        name: Some("saved name".into()),
        folder: Some("../saved folder".into()),
        proxy_ca_file: Some("/missing/ca.pem".into()),
        user_chrome: Some(false),
        local_logins: Some(false),
        keep_awake: Some(false),
        minimal: Some(true),
    }
}
pub fn no_changes(p: &ProfileView) {
    let c = &p.changes;
    assert!(c.name.is_none() && c.folder.is_none() && c.proxy_ca_file.is_none());
    assert!(c.user_chrome.is_none() && c.local_logins.is_none() && c.keep_awake.is_none());
    assert!(!c.minimal && c.add_folder.is_empty() && c.remove_folder.is_empty());
    assert!(!p.immediate_awake && !p.immediate_folders);
    assert!(p.preserves_sessions && p.preserves_desktop);
}
pub fn reject(request: Request) {
    let mut backend = fixture(inputs(facts(ProfileLifecycle::Fresh), true));
    let error = backend
        .execute(request)
        .err()
        .expect("invalid representation");
    assert_eq!(error.code, "configuration");
    assert_eq!(error.message, "Invalid configured backend request");
    let (source, accounts, consumer) = backend.into_parts();
    assert!(source.calls.is_empty() && accounts.calls.is_empty() && consumer.calls.is_empty());
}
