use clap::Parser;
use f5y::{Backend, CommandError, Request, Response, args::Cli, dispatch};
use serde_json::{Value, json};

#[derive(Default)]
struct Recorder(Vec<Request>);
impl Backend for Recorder {
    fn execute(&mut self, request: Request) -> Result<Response, CommandError> {
        self.0.push(request);
        Ok(Response {
            text: String::new(),
            json: Value::Null,
            exit_code: 0,
        })
    }
}

pub fn route(argv: &[&str], expected_command: Value) {
    let cli = Cli::try_parse_from(std::iter::once("f5y").chain(argv.iter().copied()))
        .unwrap_or_else(|error| panic!("{argv:?}: {error}"));
    let mut recorder = Recorder::default();
    dispatch(cli, &mut recorder).unwrap_or_else(|error| panic!("{argv:?}: {error:?}"));
    assert_eq!(recorder.0.len(), 1, "{argv:?}");
    let actual = serde_json::to_value(recorder.0.pop().unwrap()).unwrap();
    assert_eq!(
        actual,
        json!({"version":1,"command":expected_command}),
        "{argv:?}"
    );
}

pub fn op(operation: &str, arguments: Value) -> Value {
    json!({"operation":operation,"arguments":arguments})
}

// Hand-authored version-1 expectations. No parser defaults or serialization are
// used to generate expected values; null is an omitted request option, not a
// claim about a backend's effective default.
pub fn fields(mut defaults: Value, explicit: Value) -> Value {
    let defaults = defaults.as_object_mut().unwrap();
    for (key, value) in explicit.as_object().unwrap() {
        assert!(defaults.contains_key(key), "unknown fixture field: {key}");
        defaults.insert(key.clone(), value.clone());
    }
    Value::Object(defaults.clone())
}
