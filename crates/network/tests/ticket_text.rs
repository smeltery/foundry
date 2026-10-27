use foundry_network::{
    WireProfile,
    ticket::{TicketCodec, display_name},
};

fn profile() -> WireProfile {
    WireProfile {
        team_alpn: b"test/team".to_vec(),
        join_alpn: b"test/join".to_vec(),
        protocol: 1,
        signing_prefix: "test/sign/".into(),
        invitation_endpoint_context: "test endpoint".into(),
        invitation_secret_context: "test secret".into(),
        ticket_prefixes: vec!["f5yt-".into(), "prior5t-".into()],
    }
}

#[test]
fn secret_boundary_and_ascii_prefix_boundaries_are_distinct() {
    let profile = profile();
    let codec = TicketCodec::new(&profile, None).unwrap();
    for prefix in &profile.ticket_prefixes {
        let public = format!("{prefix}{}", "a".repeat(21));
        assert_eq!(codec.redact(&public), public);
        let secret = format!("{public}b");
        assert_eq!(codec.redact(&secret), "(team link)");
        for before in ["x", "ABC"] {
            let text = format!("{before}{secret}");
            assert_eq!(codec.redact(&text), text);
        }
        for before in ["1", "%23", "文", "[", "https://example.test/#"] {
            assert_eq!(codec.redact(&format!("{before}{secret}")), "(team link)");
        }
    }
}

#[test]
fn invisible_characters_and_markdown_escapes_cannot_hide_fragments() {
    let profile = profile();
    let codec = TicketCodec::new(&profile, None).unwrap();
    let markers = [
        '\\',
        '\u{ad}',
        '\u{34f}',
        '\u{61c}',
        '\u{115f}',
        '\u{17b4}',
        '\u{180e}',
        '\u{200b}',
        '\u{202e}',
        '\u{206f}',
        '\u{3164}',
        '\u{fe0f}',
        '\u{feff}',
        '\u{ffa0}',
        '\u{1bca2}',
        '\u{e0100}',
    ];
    for marker in markers {
        let ticket = format!("f5yt-{}", "a".repeat(22));
        let hidden = ticket
            .chars()
            .map(|c| format!("{marker}{c}"))
            .collect::<String>();
        assert_eq!(codec.redact(&format!("x{hidden}")), "(team link)");
    }
}

#[test]
fn redaction_preserves_whitespace_and_ordinary_text_without_decoding() {
    let profile = profile();
    let codec = TicketCodec::new(&profile, None).unwrap();
    let secret = format!("f5yt-{}", "_".repeat(22));
    let text = format!("before\t[{secret}](https://example.test/)\r\n  after\u{2003}{secret}");
    assert_eq!(
        codec.redact(&text),
        "before\t(team link)\r\n  after\u{2003}(team link)"
    );
    for text in [
        "",
        "plain text\n",
        "f5yt-",
        "f5yt-abc!rest",
        "unconfigured-prefix-secret",
    ] {
        assert_eq!(codec.redact(text), text);
    }
}

#[test]
fn nested_marker_remains_sensitive_after_a_nonletter_boundary() {
    let profile = profile();
    let codec = TicketCodec::new(&profile, None).unwrap();
    let value = format!("xold-f5yt-{}", "a".repeat(22));
    assert_eq!(codec.redact(&value), "(team link)");
}

#[test]
fn display_names_are_single_line_and_truncate_by_characters() {
    assert_eq!(display_name("a\nb\tc\0d\u{2028}e\u{2029}f"), "a b c d e f");
    assert_eq!(display_name(&"🦀".repeat(60)), "🦀".repeat(60));
    assert_eq!(
        display_name(&"🦀".repeat(61)),
        format!("{}…", "🦀".repeat(59))
    );
    assert_eq!(display_name(""), "");
}
