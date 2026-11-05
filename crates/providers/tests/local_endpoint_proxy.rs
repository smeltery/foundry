//! Proxy variables are ignored. This binary holds one test and sets the
//! variables before any runtime thread exists, so `set_var` cannot race.

#[path = "local_endpoint_stub.rs"]
mod stub;

use foundry_providers::local_endpoint::{CompletionRequest, LocalEndpoint, Message, OptIn, Role};
use stub::{Behavior, FixtureResolver, Stub, descriptor, model, settings};

#[test]
fn proxy_environment_is_ignored() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // Bind the proxy first so its address can be exported before any request.
    let proxy = runtime.block_on(Stub::start(vec![Behavior::json(200, "{}")]));
    // SAFETY: the runtime is single-threaded and its only tasks are the stub
    // accept loops, which never read the environment; no other test runs in
    // this binary.
    unsafe {
        for name in ["HTTP_PROXY", "http_proxy", "HTTPS_PROXY", "ALL_PROXY"] {
            std::env::set_var(name, proxy.url("http", ""));
        }
    }
    runtime.block_on(async {
        let reply = r#"{"choices":[{"message":{"content":"hi"}}]}"#;
        let stub = Stub::start(vec![Behavior::json(200, reply)]).await;
        let settings = settings(&stub.url("http", "/v1"), &[OptIn::Loopback]);
        let endpoint = LocalEndpoint::new(&descriptor(None), settings).unwrap();
        let request = CompletionRequest {
            model: model("local-small"),
            system: None,
            messages: vec![Message {
                role: Role::User,
                content: "hello".into(),
            }],
            max_output_tokens: 8,
        };
        let completion = endpoint
            .complete(&FixtureResolver::new(&[]), &request)
            .await;
        assert_eq!(completion.unwrap().text, "hi");
        assert_eq!((proxy.connections(), stub.connections()), (0, 1));
    });
}
