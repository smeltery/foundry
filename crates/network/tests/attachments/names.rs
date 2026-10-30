use super::*;
#[test]
fn unicode_names_and_reserved_names_are_normalized_without_an_ascii_whitelist() {
    for (input, expected) in [
        ("Málaga 测试.md", "Málaga 测试.md"),
        ("..../draft?.txt", "_draft_.txt"),
        (".-.--report.txt", "report.txt"),
        ("prn.log", "_prn.log"),
        ("lpt³.csv", "_lpt³.csv"),
        ("conin$.txt", "_conin$.txt"),
        ("aux .json", "_aux .json"),
        ("COM12.log", "COM12.log"),
        ("hidden\u{200d}letter.txt", "hiddenletter.txt"),
        ("x\u{2029}y.txt", "xy.txt"),
        ("...", "file"),
        ("/absolute/leaf.txt", "_absolute_leaf.txt"),
    ] {
        assert_eq!(safe_name(input), expected, "{input}");
    }
}
#[test]
fn truncation_preserves_short_extensions_and_both_unicode_limits_with_suffixes() {
    let input = format!("{}.data", "文".repeat(150));
    let normalized = normalize_received(&[file(&input, "first"), file(&input, "second")]).unwrap();
    for item in &normalized {
        assert!(item.name.len() <= 200);
        assert!(item.name.chars().count() <= 100);
    }
    assert!(normalized[0].name.ends_with(".data"));
    assert!(normalized[1].name.ends_with("-2.data"));
    let ascii = safe_name(&format!("{}.txt", "r".repeat(200)));
    assert_eq!(ascii.len(), 100);
    assert!(ascii.ends_with(".txt"));
    let sixteen = ".abcdefghijklmnñ";
    assert_eq!(sixteen.chars().count(), 16);
    assert!(safe_name(&format!("{}{}", "q".repeat(180), sixteen)).ends_with(sixteen));
    let seventeen = ".abcdefghijklmnño";
    assert_eq!(seventeen.chars().count(), 17);
    assert!(!safe_name(&format!("{}{}", "q".repeat(180), seventeen)).ends_with(seventeen));
    let duplicates = normalize_received(&[
        file("Log.TXT", "a"),
        file("log.txt", "b"),
        file("LOG.txt", "c"),
    ])
    .unwrap();
    assert_eq!(
        duplicates
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["Log.TXT", "log-2.txt", "LOG-3.txt"]
    );
}
#[test]
fn renderer_words_are_explicit_and_unicode_brackets_are_not_blanket_rejected() {
    let input = [
        file("〈marble-note.txt", "text"),
        file("〈ordinary-note.txt", "text"),
    ];
    let raw = normalize_received(&input).unwrap();
    assert_eq!(raw[0].name, input[0].name);
    let configured = normalize_received_with_frames(&input, &["marble".into()]).unwrap();
    assert_eq!(configured[0].name, "[marble-note.txt");
    assert_eq!(configured[1].name, input[1].name);
    let disguised =
        normalize_received_with_frames(&[file("〈m4rbl3.txt", "text")], &["marble".into()])
            .unwrap();
    assert_eq!(disguised[0].name, "[m4rbl3.txt");
    assert_eq!(safe_name("〈日本語.txt"), "[日本語.txt");
}
#[test]
fn filesystem_unicode_aliases_receive_stable_suffixes_and_survive_reopen() {
    let root = Root::new();
    let mut store = AttachmentStore::open(&root.0).unwrap();
    let input = [
        file("épreuve.txt", "first"),
        file("e\u{301}preuve.txt", "second"),
    ];
    let result = store.persist(&key("unicode-alias"), &input, 0).unwrap();
    for (saved, original) in result.iter().zip(&input) {
        assert_eq!(
            fs::read_to_string(saved.path.as_ref().unwrap()).unwrap(),
            original.content
        );
    }
    assert_ne!(result[0].path, result[1].path);
    drop(store);
    let mut store = AttachmentStore::open(&root.0).unwrap();
    assert_eq!(store.recover(&key("unicode-alias")).unwrap(), result);
    assert_eq!(
        store.persist(&key("unicode-alias"), &input, 1).unwrap(),
        result
    );
    store.release(&key("unicode-alias"), 1).unwrap();
    store
        .cleanup(&key("unicode-alias"), 1 + RETAIN_RELEASED_MS)
        .unwrap();
}
