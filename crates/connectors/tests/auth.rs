use foundry_connectors::{
    AuthKind, Catalog, ConfigurationStatus, ConnectorError, Credentials, Secret, Server,
};
use serde_json::{Value, json};

fn server(auth: Value, headers: Value, url: &str) -> Server {
    let input = json!({"schemaVersion":1,"connectors":[{
        "id":"sample","displayName":"Sample","description":"Synthetic fixture",
        "servers":[{"id":"primary","transport":"streamable-http",
            "endpoint":{"kind":"remote","url":url},"auth":auth,"headers":headers}]
    }]});
    Catalog::parse(&serde_json::to_vec(&input).unwrap())
        .unwrap()
        .connectors
        .remove(0)
        .servers
        .remove(0)
}
fn api_key(value: &str) -> Credentials {
    Credentials::ApiKey(Secret::new(value.into()).unwrap())
}

#[test]
fn unauthenticated_query_order_encoding_and_duplicates_are_preserved() {
    let url = "https://api.example:443/mcp/a%2fb?key=one&key=two&empty=&signed=a%2Bb+z&flag";
    let server = server(json!({"kind":"none"}), json!({"X-Version":"1"}), url);
    let request = server.prepare(&Credentials::None).unwrap();
    assert_eq!(request.url(), url);
    assert_eq!(request.headers().get("x-version").unwrap(), "1");
    assert!(!request.headers().contains_key("authorization"));
}

#[test]
fn api_key_header_variants_and_literal_substitution() {
    for headers in [
        json!({"X-API-Key":"${credential}"}),
        json!({"Authorization":"Bearer ${credential}"}),
        json!({"X-Token":"prefix:${credential}:suffix","X-Second":"${credential}/${credential}"}),
    ] {
        let server = server(
            json!({"kind":"api_key","placeholder":"credential"}),
            headers.clone(),
            "https://api.example/mcp?project=one&project=two",
        );
        let credentials = api_key("sensitive-$&%+value");
        let request = server.prepare(&credentials).unwrap();
        for (name, value) in headers.as_object().unwrap() {
            assert_eq!(
                request.headers()[&name.to_ascii_lowercase()],
                value
                    .as_str()
                    .unwrap()
                    .replace("${credential}", "sensitive-$&%+value")
            );
        }
        assert_eq!(
            request.url(),
            "https://api.example/mcp?project=one&project=two"
        );
        assert!(!format!("{request:?} {credentials:?}").contains("sensitive-$&%+value"));
    }
}

#[test]
fn oauth_bearer_and_missing_credentials_are_typed() {
    let server = server(
        json!({"kind":"oauth"}),
        json!({}),
        "https://api.example/mcp?audience=a%2Fb",
    );
    assert_eq!(
        server.configuration_status(&Credentials::None),
        ConfigurationStatus::MissingCredential(AuthKind::OAuth)
    );
    let token = Credentials::OAuthToken(Secret::new("abc-_~+/==".into()).unwrap());
    let request = server.prepare(&token).unwrap();
    assert_eq!(request.headers()["authorization"], "Bearer abc-_~+/==");
    assert_eq!(request.url(), "https://api.example/mcp?audience=a%2Fb");
    assert_eq!(
        server.prepare(&api_key("wrong-type")).unwrap_err(),
        ConnectorError::WrongCredentialKind
    );
}

#[test]
fn credential_validation_rejects_injection_and_bad_bearer_grammar() {
    for value in [
        "",
        "secret\r\nX-Other: value",
        "secret\0value",
        "secret\tvalue",
    ] {
        let error = Secret::new(value.into()).unwrap_err();
        assert_eq!(error, ConnectorError::InvalidCredential);
    }
    let server = server(
        json!({"kind":"oauth"}),
        json!({}),
        "https://api.example/mcp",
    );
    for value in ["has space", "=abc", "a=b", "==="] {
        let credentials = Credentials::OAuthToken(Secret::new(value.into()).unwrap());
        assert_eq!(
            server.prepare(&credentials).unwrap_err(),
            ConnectorError::InvalidCredential
        );
    }
}

#[test]
fn wrong_or_missing_credentials_never_silently_fall_back() {
    let server = server(
        json!({"kind":"api_key","placeholder":"credential"}),
        json!({"X-Key":"${credential}"}),
        "https://api.example/mcp",
    );
    assert_eq!(
        server.configuration_status(&Credentials::None),
        ConfigurationStatus::MissingCredential(AuthKind::ApiKey)
    );
    let anonymous = self::server(json!({"kind":"none"}), json!({}), "https://api.example/mcp");
    assert_eq!(
        anonymous.prepare(&api_key("extra")).unwrap_err(),
        ConnectorError::WrongCredentialKind
    );
}

#[test]
fn unavailable_hosted_adapter_stays_unsupported() {
    let server: Server =
        serde_json::from_value(json!({"id":"hosted","transport":"streamable-http",
        "endpoint":{"kind":"adapter","id":"hosted-search","state":"unavailable"},
        "auth":{"kind":"oauth"}}))
        .unwrap();
    assert_eq!(
        server.configuration_status(&Credentials::None),
        ConfigurationStatus::UnsupportedAdapter
    );
    assert_eq!(
        server
            .prepare(&Credentials::OAuthToken(
                Secret::new("token".into()).unwrap()
            ))
            .unwrap_err(),
        ConnectorError::UnsupportedAdapter
    );
}

#[test]
fn query_auth_and_unknown_templates_are_explicitly_rejected() {
    let mut server = server(
        json!({"kind":"api_key","placeholder":"credential"}),
        json!({"X-Key":"${credential}"}),
        "https://api.example/mcp",
    );
    server.endpoint = foundry_connectors::Endpoint::Remote {
        url: "https://api.example/mcp?api_key=${credential}".into(),
    };
    assert_eq!(
        server.prepare(&api_key("secret")).unwrap_err(),
        ConnectorError::UnsupportedQueryAuth
    );
    server.endpoint = foundry_connectors::Endpoint::Remote {
        url: "https://api.example/mcp".into(),
    };
    for value in ["${other}", "${credential", "no placeholder"] {
        server.headers.insert("X-Key".into(), value.into());
        assert_eq!(
            server.prepare(&api_key("secret")).unwrap_err(),
            ConnectorError::InvalidTemplate
        );
    }
}

#[test]
fn cleartext_and_private_resolved_peers_are_not_accepted() {
    let plain = server(json!({"kind":"none"}), json!({}), "http://api.example/mcp");
    assert_eq!(
        plain.prepare(&Credentials::None).unwrap_err(),
        ConnectorError::InsecureTransport
    );
    let secure = server(json!({"kind":"none"}), json!({}), "https://api.example/mcp");
    let request = secure.prepare(&Credentials::None).unwrap();
    for address in [
        "127.0.0.1",
        "10.1.2.3",
        "169.254.169.254",
        "::1",
        "fc00::1",
        "::ffff:127.0.0.1",
    ] {
        assert_eq!(
            request.validate_peer(address.parse().unwrap()).unwrap_err(),
            ConnectorError::UnsafeHost
        );
    }
    assert!(request.validate_peer("8.8.8.8".parse().unwrap()).is_ok());
    assert!(
        request
            .validate_peer("2606:4700::1111".parse().unwrap())
            .is_ok()
    );
}

#[test]
fn repeated_templates_cannot_expand_beyond_header_budget() {
    let server = server(
        json!({"kind":"api_key","placeholder":"credential"}),
        json!({"X-Key":"${credential}".repeat(3)}),
        "https://api.example/mcp",
    );
    assert_eq!(
        server.prepare(&api_key(&"a".repeat(8192))).unwrap_err(),
        ConnectorError::InvalidHeaders
    );
}

#[test]
fn fixture_connector_reports_unavailable_without_preparing_auth() {
    let input = json!({"schemaVersion":1,"connectors":[{
        "id":"sample","displayName":"Sample","description":"Synthetic fixture","fixture":true,
        "servers":[{"id":"primary","transport":"streamable-http",
            "endpoint":{"kind":"remote","url":"https://api.example/mcp"},"auth":{"kind":"none"}}]
    }]});
    let catalog = Catalog::parse(&serde_json::to_vec(&input).unwrap()).unwrap();
    let connector = &catalog.connectors[0];
    assert_eq!(
        connector.configuration_status("primary", &Credentials::None),
        ConfigurationStatus::FixtureOnly
    );
    assert_eq!(
        connector
            .prepare("primary", &Credentials::None)
            .unwrap_err(),
        ConnectorError::FixtureOnly
    );
}
