//! Endpoint admission by network class, URL validation and redaction. No
//! connection is made: literal addresses are classified directly and host
//! names resolve through a fixture resolver.

#[path = "local_endpoint_stub.rs"]
mod stub;

use foundry_providers::catalog::{EndpointFormat, ProviderKind};
use foundry_providers::local_endpoint::{
    EndpointError, EndpointSettings, InvalidField, LocalEndpoint, MAX_TIMEOUT, NetworkClass, OptIn,
    classify,
};
use std::time::Duration;
use stub::{FixtureResolver, KEY_CANARY, descriptor, model, settings};

fn endpoint(base_url: &str, opt_in: &[OptIn]) -> Result<LocalEndpoint, EndpointError> {
    LocalEndpoint::new(&descriptor(None), settings(base_url, opt_in))
}

async fn admitted(
    base_url: &str,
    opt_in: &[OptIn],
    resolved: &[&str],
) -> Result<NetworkClass, EndpointError> {
    let resolver = FixtureResolver::new(&[resolved]);
    let admission = endpoint(base_url, opt_in)?.admit(&resolver).await?;
    Ok(admission.network_class)
}

#[tokio::test]
async fn public_https_endpoint_needs_no_opt_in() {
    let endpoint = endpoint("https://models.example/v1", &[]).unwrap();
    let admission = endpoint
        .admit(&FixtureResolver::new(&[&["198.51.100.7"]]))
        .await
        .unwrap();
    assert_eq!(admission.network_class, NetworkClass::Public);
    assert_eq!(admission.format, EndpointFormat::Chat);
}

#[tokio::test]
async fn private_classes_need_their_own_opt_in() {
    let loopback = admitted("http://127.0.0.1:8080/v1", &[OptIn::Loopback], &[]).await;
    assert_eq!(loopback, Ok(NetworkClass::Loopback));
    let lan = admitted("http://192.168.1.20:8000/v1", &[OptIn::Lan], &[]).await;
    assert_eq!(lan, Ok(NetworkClass::Lan));
    let tailnet = "http://gpu-box.example:8000/v1";
    let opted = admitted(tailnet, &[OptIn::Tailnet], &["100.100.10.5"]).await;
    assert_eq!(opted, Ok(NetworkClass::Tailnet));

    for (url, opt_in) in [
        ("http://[::1]:8080/v1", &[][..]),
        (
            "http://127.0.0.1:8080/v1",
            &[OptIn::Lan, OptIn::Tailnet][..],
        ),
        ("http://192.168.1.20:8000/v1", &[OptIn::Loopback][..]),
        ("http://[fd00::5]:8000/v1", &[OptIn::Loopback][..]),
    ] {
        assert_eq!(
            endpoint(url, opt_in).unwrap_err(),
            EndpointError::NotPermitted
        );
    }
    let refused = admitted(tailnet, &[OptIn::Lan], &["100.100.10.5"]).await;
    assert_eq!(refused, Err(EndpointError::NotPermitted));
}

#[tokio::test]
async fn link_local_and_reserved_addresses_are_always_refused() {
    let every = [OptIn::Loopback, OptIn::Lan, OptIn::Tailnet];
    for url in [
        "http://169.254.169.254/v1",
        "http://[fe80::1]/v1",
        "http://0.0.0.0:8080/v1",
        "http://[::]:8080/v1",
        "http://[::ffff:169.254.169.254]/v1",
        "https://[2002:7f00:1::1]/v1",
        "https://[64:ff9b::7f00:1]/v1",
        "https://224.0.0.1/v1",
    ] {
        assert_eq!(
            endpoint(url, &every).unwrap_err(),
            EndpointError::NotPermitted,
            "{url}"
        );
    }
    let named = admitted("http://metadata.example/v1", &every, &["169.254.169.254"]).await;
    assert_eq!(named, Err(EndpointError::NotPermitted));
}

#[test]
fn mapped_addresses_take_their_ipv4_class() {
    assert_eq!(
        classify("::ffff:127.0.0.1".parse().unwrap()),
        NetworkClass::Loopback
    );
    assert_eq!(
        classify("::ffff:10.0.0.1".parse().unwrap()),
        NetworkClass::Lan
    );
    assert_eq!(
        classify("100.127.255.1".parse().unwrap()),
        NetworkClass::Tailnet
    );
    assert_eq!(
        classify("100.128.0.1".parse().unwrap()),
        NetworkClass::Public
    );
    assert_eq!(
        classify("172.32.0.1".parse().unwrap()),
        NetworkClass::Public
    );
    assert_eq!(
        classify("2001:4860::1".parse().unwrap()),
        NetworkClass::Public
    );
    let mapped_without_opt_in = endpoint("http://[::ffff:127.0.0.1]:8080/v1", &[]);
    assert_eq!(
        mapped_without_opt_in.unwrap_err(),
        EndpointError::NotPermitted
    );
}

#[tokio::test]
async fn public_cleartext_is_insecure_and_private_answers_are_refused() {
    let cleartext = admitted("http://models.example/v1", &[], &["203.0.113.10"]).await;
    assert_eq!(cleartext, Err(EndpointError::InsecureTransport));
    let literal = endpoint("http://198.51.100.7/v1", &[OptIn::Loopback]);
    assert_eq!(literal.unwrap_err(), EndpointError::InsecureTransport);
    let private = admitted("https://models.example/v1", &[], &["10.1.2.3"]).await;
    assert_eq!(private, Err(EndpointError::NotPermitted));
}

#[tokio::test]
async fn mixed_dns_answers_are_refused() {
    let mixed = ["203.0.113.10", "127.0.0.1"];
    let class = admitted("https://models.example/v1", &[OptIn::Loopback], &mixed).await;
    assert_eq!(class, Err(EndpointError::NotPermitted));
    let both = ["127.0.0.1", "10.0.0.5"];
    let all = [OptIn::Loopback, OptIn::Lan];
    let spanning = admitted("http://box.example/v1", &all, &both).await;
    assert_eq!(spanning, Err(EndpointError::NotPermitted));
}

#[test]
fn malformed_base_urls_are_invalid() {
    for url in [
        "https://user@models.example/v1",
        "https://user:secret@models.example/v1",
        "https://@models.example/v1",
        "ftp://models.example/v1",
        "https://models.example/v1#part",
        "https://models.example/v1?mode=x",
        "https://models.example/v1\n",
        "https://models.example\\v1",
        "http://127.0.0.1:0/v1",
        "models.example/v1",
    ] {
        let error = endpoint(url, &[OptIn::Loopback]).unwrap_err();
        assert_eq!(
            error,
            EndpointError::InvalidRequest(InvalidField::BaseUrl),
            "{url}"
        );
        assert_eq!(error.class(), "invalid_request");
    }
}

#[test]
fn models_timeout_and_descriptor_are_explicit() {
    let base = "https://models.example/v1";
    let check = |change: fn(&mut EndpointSettings), field| {
        let mut settings = settings(base, &[]);
        change(&mut settings);
        let error = LocalEndpoint::new(&descriptor(None), settings).unwrap_err();
        assert_eq!(error, EndpointError::InvalidRequest(field));
    };
    check(|s| s.models.clear(), InvalidField::Models);
    check(
        |s| s.models.push(model("local-small")),
        InvalidField::Models,
    );
    check(|s| s.timeout = Duration::ZERO, InvalidField::Timeout);
    check(
        |s| s.timeout = MAX_TIMEOUT + Duration::from_millis(1),
        InvalidField::Timeout,
    );

    let mut hosted = descriptor(None);
    hosted.kind = ProviderKind::HostedApi;
    let error = LocalEndpoint::new(&hosted, settings(base, &[])).unwrap_err();
    assert_eq!(
        error,
        EndpointError::InvalidRequest(InvalidField::Descriptor)
    );
    for (format, expected) in [
        (None, EndpointFormat::Chat),
        (Some(EndpointFormat::Response), EndpointFormat::Response),
        (Some(EndpointFormat::Message), EndpointFormat::Message),
    ] {
        let endpoint = LocalEndpoint::new(&descriptor(format), settings(base, &[])).unwrap();
        assert_eq!(endpoint.format(), expected);
        assert_eq!(endpoint.models(), [model("local-small")]);
    }
}

#[test]
fn debug_output_hides_url_and_key() {
    let url = "https://models.example/canary-path";
    let settings = settings(url, &[]);
    let client = LocalEndpoint::new(&descriptor(None), settings.clone()).unwrap();
    let credentials = endpoint("https://user:pw-canary@models.example/v1", &[]).unwrap_err();
    for text in [
        format!("{settings:?}"),
        format!("{client:?}"),
        format!("{credentials:?} {credentials}"),
    ] {
        assert!(!text.contains(KEY_CANARY), "{text}");
        assert!(!text.contains("canary-path"), "{text}");
        assert!(!text.contains("pw-canary"), "{text}");
        assert!(!text.contains("models.example"), "{text}");
    }
}
