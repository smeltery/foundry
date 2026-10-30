use super::*;
use std::os::unix::fs::symlink;
#[test]
fn human_selection_reads_hidden_credentials_outside_workspace_but_agent_cannot() {
    let project = Root::new();
    let personal = Root::new();
    personal.mkdir(".keys");
    personal.write(
        ".keys/private.pem",
        "-----BEGIN PRIVATE KEY-----\nexplicit selection\n",
    );
    let human = SenderPolicy::human(&project.0).unwrap();
    let chosen = personal.0.join(".keys/private.pem");
    let files = prepare(&human, std::slice::from_ref(&chosen)).unwrap();
    assert_eq!(files[0].name, "private.pem");
    assert!(prepare(&project.policy(), std::slice::from_ref(&chosen)).is_err());
    symlink(&chosen, project.0.join("selected.pem")).unwrap();
    assert_eq!(
        prepare(&human, &["selected.pem".into()]).unwrap()[0].name,
        "selected.pem"
    );
    assert!(prepare(&project.policy(), &["selected.pem".into()]).is_err());
    fs::hard_link(&chosen, project.0.join("authorized-link.pem")).unwrap();
    assert!(prepare(&human, &["authorized-link.pem".into()]).is_ok());
    assert_eq!(
        prepare(&project.policy(), &["authorized-link.pem".into()]).unwrap()[0].content,
        files[0].content
    );
    project.write(".selected", "personal text");
    assert_eq!(
        prepare(&human, &[".selected".into()]).unwrap()[0].name,
        "selected"
    );
    assert!(prepare(&project.policy(), &[".selected".into()]).is_err());
}
#[test]
fn safe_file_and_directory_aliases_use_target_names_and_disabled_targets_stay_blocked() {
    let root = Root::new();
    root.mkdir("docs");
    root.mkdir("disabled");
    root.mkdir("disabled/enabled");
    root.write("docs/guide.md", "guide");
    root.write("disabled/hidden.txt", "blocked");
    root.write("disabled/enabled/show.txt", "enabled");
    symlink(root.0.join("docs"), root.0.join("alias")).unwrap();
    symlink(root.0.join("docs/guide.md"), root.0.join("label.md")).unwrap();
    symlink(
        root.0.join("disabled/hidden.txt"),
        root.0.join("tempting.txt"),
    )
    .unwrap();
    let policy = SenderPolicy::new(&root.0, std::slice::from_ref(&root.0), &[])
        .unwrap()
        .with_folders(
            &[root.0.join("disabled/enabled")],
            &[root.0.join("disabled")],
        )
        .unwrap();
    let output = prepare(&policy, &["alias/guide.md".into(), "label.md".into()]).unwrap();
    assert_eq!(
        output.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        ["guide.md", "guide-2.md"]
    );
    assert!(prepare(&policy, &["tempting.txt".into()]).is_err());
    assert!(prepare(&policy, &["disabled/enabled/show.txt".into()]).is_ok());
}
#[test]
fn caller_supplied_personal_boundary_prevents_broad_agent_roots_without_discovery() {
    let root = Root::new();
    root.mkdir("project");
    assert!(
        SenderPolicy::agent(
            &root.0,
            std::slice::from_ref(&root.0),
            &[],
            std::slice::from_ref(&root.0)
        )
        .is_err()
    );
    assert!(
        SenderPolicy::agent(
            &root.0.join("project"),
            &[root.0.join("project")],
            &[],
            std::slice::from_ref(&root.0)
        )
        .is_ok()
    );
}

#[test]
fn absent_disabled_root_is_enforced_when_it_later_exists() {
    let root = Root::new();
    let future = root.0.join("future");
    let policy = SenderPolicy::new(
        &root.0,
        std::slice::from_ref(&root.0),
        std::slice::from_ref(&future),
    )
    .unwrap();
    root.mkdir("future");
    root.write("future/note.txt", "disabled");
    assert!(prepare(&policy, &["future/note.txt".into()]).is_err());
}

#[test]
fn folder_switches_do_not_override_agent_secret_roots_or_impose_agent_rules_on_human_mode() {
    let root = Root::new();
    root.mkdir("private");
    root.mkdir("private/visible");
    root.write("private/visible/.text", "selected");
    let target = root.0.join("private/visible/.text");
    let human = SenderPolicy::human(&root.0)
        .unwrap()
        .with_folders(&[root.0.join("private/visible")], &[root.0.join("private")])
        .unwrap();
    assert_eq!(
        prepare(&human, std::slice::from_ref(&target)).unwrap()[0].name,
        "text"
    );
    let disabled = SenderPolicy::human(&root.0)
        .unwrap()
        .with_folders(&[], &[root.0.join("private")])
        .unwrap();
    assert!(prepare(&disabled, std::slice::from_ref(&target)).is_err());
    root.write("private/visible/public.txt", "private state");
    let agent = SenderPolicy::new(
        &root.0,
        std::slice::from_ref(&root.0),
        &[root.0.join("private")],
    )
    .unwrap()
    .with_folders(&[root.0.join("private/visible")], &[])
    .unwrap();
    assert!(prepare(&agent, &["private/visible/public.txt".into()]).is_err());
}

#[test]
fn regular_agent_hardlinks_are_authorized_by_the_selected_path_in_both_modes() {
    use std::os::unix::fs::MetadataExt;
    let root = Root::new();
    root.mkdir("allowed");
    root.mkdir("denied");
    root.mkdir(".hidden");
    root.write("denied/original.txt", "owned fixture text");
    fs::hard_link(
        root.0.join("denied/original.txt"),
        root.0.join("allowed/alias.txt"),
    )
    .unwrap();
    assert!(
        fs::metadata(root.0.join("allowed/alias.txt"))
            .unwrap()
            .nlink()
            > 1
    );
    let agent = SenderPolicy::new(
        &root.0,
        std::slice::from_ref(&root.0),
        &[root.0.join("denied")],
    )
    .unwrap();
    assert_eq!(
        prepare(&agent, &["allowed/alias.txt".into()]).unwrap(),
        [file("alias.txt", "owned fixture text")]
    );
    assert!(prepare(&agent, &["denied/original.txt".into()]).is_err());
    symlink(
        root.0.join("denied/original.txt"),
        root.0.join("allowed/symlink.txt"),
    )
    .unwrap();
    assert!(prepare(&agent, &["allowed/symlink.txt".into()]).is_err());
    fs::hard_link(
        root.0.join("allowed/alias.txt"),
        root.0.join(".hidden/alias.txt"),
    )
    .unwrap();
    assert!(prepare(&agent, &[".hidden/alias.txt".into()]).is_err());
    let human = SenderPolicy::human(&root.0).unwrap();
    assert_eq!(
        prepare(&human, &[".hidden/alias.txt".into()]).unwrap(),
        [file("alias.txt", "owned fixture text")]
    );
}
