use foundry_connectors::{Catalog, ConnectorError};
use serde_json::{Value, json};

fn document() -> Value {
    json!({"schemaVersion":1,"connectors":[{
        "id":"sample","displayName":"Sample","description":"Synthetic fixture",
        "homepage":"https://provider.example/docs#home",
        "icon":{"kind":"local","path":"icons/sample.svg"},
        "servers":[{"id":"primary","transport":"streamable-http",
            "endpoint":{"kind":"remote","url":"https://api.example/mcp"},"auth":{"kind":"none"}}],
        "skills":[{"id":"sample","url":"https://provider.example/skill","revision":"a".repeat(40)}]
    }]})
}
fn parse(value: Value) -> Result<Catalog, ConnectorError> {
    Catalog::parse(&serde_json::to_vec(&value).unwrap())
}

#[test]
fn full_catalog_and_optional_fields_validate() {
    let catalog = parse(document()).unwrap();
    assert_eq!(catalog.schema_version, 1);
    assert_eq!(catalog.connectors[0].id, "sample");
    assert_eq!(
        catalog.connectors[0].skills.as_ref().unwrap()[0]
            .revision
            .len(),
        40
    );
}

#[test]
fn duplicate_json_keys_cannot_collapse() {
    let text = serde_json::to_string(&document()).unwrap();
    let duplicate = text.replace(
        "\"schemaVersion\":1",
        "\"schemaVersion\":1,\"schemaVersion\":1",
    );
    assert_eq!(
        Catalog::parse(duplicate.as_bytes()).unwrap_err(),
        ConnectorError::InvalidJson
    );
    let duplicate = text.replace("\"auth\":", "\"auth\":{\"kind\":\"oauth\"},\"auth\":");
    assert_eq!(
        Catalog::parse(duplicate.as_bytes()).unwrap_err(),
        ConnectorError::InvalidJson
    );
}

#[test]
fn unknown_fields_null_and_shape_mismatches_fail() {
    let cases = [
        ("/schemaVersion", json!(2)),
        ("/connectors", json!([])),
        ("/connectors/0/id", json!("Mixed_Case")),
        ("/connectors/0/displayName", json!("")),
        ("/connectors/0/homepage", Value::Null),
        ("/connectors/0/servers", json!([])),
        ("/connectors/0/servers/0/transport", json!("stdio")),
        (
            "/connectors/0/servers/0/auth",
            json!({"kind":"oauth","extra":true}),
        ),
        (
            "/connectors/0/servers/0/auth",
            json!({"kind":"api_key","placeholder":"other"}),
        ),
        ("/connectors/0/skills", json!([])),
        ("/connectors/0/skills/0/revision", json!("ABC")),
    ];
    for (path, value) in cases {
        let mut input = document();
        *input.pointer_mut(path).unwrap() = value;
        assert!(parse(input).is_err(), "accepted {path}");
    }
    let mut input = document();
    input["unexpected"] = json!(true);
    assert!(parse(input).is_err());
}

#[test]
fn duplicated_scoped_ids_fail() {
    for path in [
        "/connectors",
        "/connectors/0/servers",
        "/connectors/0/skills",
    ] {
        let mut input = document();
        let array = input.pointer_mut(path).unwrap().as_array_mut().unwrap();
        array.push(array[0].clone());
        assert!(parse(input).is_err());
    }
}

#[test]
fn unsafe_urls_are_rejected_without_echoing_values() {
    for address in [
        "file:///tmp/socket",
        "ftp://provider.example/file",
        "https://user:secret@api.example/mcp",
        "https://@api.example/mcp",
        "https://localhost/mcp",
        "https://sub.localhost./mcp",
        "https://127.1/mcp",
        "https://2130706433/mcp",
        "https://0x7f000001/mcp",
        "https://10.0.0.1/mcp",
        "https://169.254.169.254/mcp",
        "https://100.64.0.1/mcp",
        "https://192.0.2.1/mcp",
        "https://[::1]/mcp",
        "https://[::ffff:127.0.0.1]/mcp",
        "https://[2002:7f00:1::]/mcp",
        "https://[2001:db8::1]/mcp",
        "https://office.internal/mcp",
        "https://api.example:0/mcp",
        "https://api.example/mcp#fragment",
        "https://api.example/\nsecret",
        "https://api.example\\@localhost/mcp",
        "https://-bad.example/mcp",
    ] {
        let mut input = document();
        input["connectors"][0]["servers"][0]["endpoint"]["url"] = json!(address);
        let error = parse(input).unwrap_err();
        assert!(!error.to_string().contains(address));
    }
}

#[test]
fn unsafe_icon_and_headers_fail() {
    for path in [
        "../icon.svg",
        "/icon.svg",
        "icons//icon.svg",
        "icons/%2e%2e/icon.svg",
        "C:\\icon.svg",
    ] {
        let mut input = document();
        input["connectors"][0]["icon"]["path"] = json!(path);
        assert!(parse(input).is_err());
    }
    for headers in [
        json!({"X-Test":"a\r\nb"}),
        json!({"Bad Name":"value"}),
        json!({"X-Test":"one","x-test":"two"}),
        json!({"Host":"other.example"}),
        json!({"Transfer-Encoding":"chunked"}),
        json!({"Authorization":"Bearer secret"}),
        json!({"X-Token":"${credential}"}),
    ] {
        let mut input = document();
        input["connectors"][0]["servers"][0]["headers"] = headers;
        assert!(parse(input).is_err());
    }
}

#[test]
fn diagnostics_redact_entire_catalog() {
    let mut input = document();
    input["connectors"][0]["servers"][0]["headers"] = json!({"X-Static":"unique-sensitive-value"});
    let catalog = parse(input).unwrap();
    for debug in [
        format!("{catalog:?}"),
        format!("{:?}", catalog.connectors[0]),
        format!("{:?}", catalog.connectors[0].servers[0]),
    ] {
        assert!(!debug.contains("unique-sensitive-value"));
        assert!(!debug.contains("api.example"));
    }
}
