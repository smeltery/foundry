use super::*;
use foundry_contracts::context::ContextDigest;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink},
};

fn document(root: &Path) -> Value {
    let selection = AdapterSelection {
        provider: "fixture".into(),
        model: "small".into(),
        effort: "off".into(),
        directory: root.display().to_string(),
        workspace_identity: ContextDigest::of(b"workspace"),
        profile: "local_agent_v1".into(),
        profile_version: 1,
        permission_profile: "current".into(),
        resource_policy: "explicit_system_v1".into(),
        resource_identity: ContextDigest::of(b"resources"),
        tool_catalog: "core_workspace_v1".into(),
        tools: vec!["read".into()],
        policy_identity: ContextDigest::of(b"policy"),
        generation: 1,
        max_output_bytes: 65536,
        max_pending_text_bytes: 16384,
    };
    json!({"version":1,"selection":selection,"private":{
        "credential":{"kind":"api_key","provider":"fixture","value":"private-canary"},
        "system_prompt":"explicit system text","agent_directory":root.join("agent")
    }})
}

fn write(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

fn rejected(result: Result<LocalAdapterConfiguration, CommandError>) {
    let error = result.err().expect("configuration must reject");
    assert_eq!(error.code, "configuration");
    assert_eq!(
        error.message,
        "Private adapter configuration is unavailable or invalid"
    );
    assert!(!error.message.contains("private-canary"));
}

#[test]
fn explicit_selection_and_private_values_are_preserved() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("adapter.json");
    let input = document(root.path());
    write(&path, &serde_json::to_vec(&input).unwrap());
    let loaded = load(&path).unwrap();
    assert_eq!(
        serde_json::to_value(loaded.selection).unwrap(),
        input["selection"]
    );
    let AdapterCredential::ApiKey { provider, value } = loaded.private.credential else {
        panic!()
    };
    assert_eq!(provider, "fixture");
    assert_eq!(value, "private-canary");
    assert_eq!(loaded.private.system_prompt, "explicit system text");
    assert_eq!(loaded.private.agent_directory, root.path().join("agent"));
    let mut none = input;
    none["private"]["credential"] = json!({"kind":"none"});
    assert!(matches!(
        decode(&serde_json::to_vec(&none).unwrap())
            .unwrap()
            .private
            .credential,
        AdapterCredential::None
    ));
}

#[test]
fn links_permissions_and_nonregular_files_do_not_supply_credentials() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("adapter.json");
    write(&path, &serde_json::to_vec(&document(root.path())).unwrap());
    rejected(load(Path::new("relative.json")));
    rejected(load(root.path()));
    rejected(load(&root.path().join("absent")));
    let link = root.path().join("link");
    symlink(&path, &link).unwrap();
    rejected(load(&link));
    let alias = root.path().join("alias");
    fs::hard_link(&path, &alias).unwrap();
    rejected(load(&path));
    fs::remove_file(alias).unwrap();
    for mode in [0o644, 0o660, 0o700, 0o4600] {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        rejected(load(&path));
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(load(&path).is_ok());
    let fifo = root.path().join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    rejected(load(&fifo));
}

#[test]
fn malformed_duplicate_unknown_and_mismatched_fields_have_fixed_errors() {
    let root = tempfile::tempdir().unwrap();
    let input = document(root.path());
    for raw in [
        b"private-canary".as_slice(),
        b"{\"version\":1,\"version\":1}",
    ] {
        rejected(decode(raw));
    }
    let valid = serde_json::to_string(&input).unwrap();
    rejected(decode(valid.replacen('{', "{\"version\":1,", 1).as_bytes()));
    rejected(decode(
        valid
            .replace(
                "\"value\":\"private-canary\"",
                "\"value\":\"private-canary\",\"value\":\"other\"",
            )
            .as_bytes(),
    ));
    for mutation in 0..9 {
        let mut value = input.clone();
        match mutation {
            0 => value["version"] = json!(2),
            1 => value["unexpected"] = json!("private-canary"),
            2 => value["private"]["unexpected"] = json!(true),
            3 => value["private"]["credential"]["provider"] = json!("other"),
            4 => value["private"]["credential"]["value"] = json!(""),
            5 => value["private"]["agent_directory"] = json!("relative"),
            6 => value["selection"]["tools"] = json!(["read", "read"]),
            7 => value["private"]["credential"]["unexpected"] = json!("private-canary"),
            _ => value["selection"]["unexpected"] = json!(true),
        }
        rejected(decode(&serde_json::to_vec(&value).unwrap()));
    }
}

#[test]
fn file_and_private_value_limits_reject_without_echoing_input() {
    let root = tempfile::tempdir().unwrap();
    let exact = root.path().join("exact-bound");
    let mut bytes = serde_json::to_vec(&document(root.path())).unwrap();
    bytes.resize(MAX_BYTES as usize, b' ');
    write(&exact, &bytes);
    assert!(load(&exact).is_ok());
    let path = root.path().join("oversized");
    write(&path, &vec![b' '; MAX_BYTES as usize + 1]);
    rejected(load(&path));
    let empty = root.path().join("empty");
    write(&empty, b"");
    rejected(load(&empty));
    for key in ["system_prompt", "credential"] {
        let mut value = document(root.path());
        if key == "credential" {
            value["private"][key]["value"] = json!("x".repeat(16_385));
        } else {
            value["private"][key] = json!("x".repeat(16_385));
        }
        rejected(decode(&serde_json::to_vec(&value).unwrap()));
    }
}
