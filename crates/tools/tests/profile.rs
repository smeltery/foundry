#![cfg(unix)]
use foundry_tools::{Grants, Limits, ToolError, ToolProfile, WorkspaceTools};
use std::fs;
use tempfile::tempdir;

const TOOLS: [&str; 4] = ["read", "list", "write", "edit"];

#[test]
fn no_reference_selects_no_tools() {
    assert_eq!(ToolProfile::select(None).unwrap(), Grants::default());
}

#[test]
fn profiles_select_fixed_tool_sets() {
    let names: Vec<_> = ToolProfile::all().iter().map(ToolProfile::name).collect();
    assert_eq!(names, ["default", "default/read-only"]);
    assert_eq!(
        ToolProfile::named("default").unwrap().tools(),
        ["read", "list", "write", "edit"]
    );
    assert_eq!(
        ToolProfile::named("default/read-only").unwrap().tools(),
        ["read", "list"]
    );
    for profile in ToolProfile::all() {
        // Repeated selection of the same name is identical.
        assert_eq!(ToolProfile::named(profile.name()).unwrap(), *profile);
        assert_eq!(
            ToolProfile::select(Some(profile.name())).unwrap(),
            profile.grants()
        );
    }
}

#[test]
fn unknown_profiles_never_fall_back() {
    for name in [
        "",
        "Default",
        "default/",
        "default/read-write",
        "custom",
        " default",
    ] {
        assert!(
            matches!(ToolProfile::named(name), Err(ToolError::Invalid(_))),
            "{name:?}"
        );
        assert!(ToolProfile::select(Some(name)).is_err(), "{name:?}");
    }
}

#[test]
fn selected_grants_enable_exactly_the_listed_tools() {
    for profile in ToolProfile::all() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("file"), "a").unwrap();
        let tools = WorkspaceTools::open(dir.path(), profile.grants(), Limits::default()).unwrap();
        let enabled = profile.tools();
        for name in TOOLS {
            // An empty object passes the grant check only when the tool is enabled,
            // then fails schema parsing without touching the workspace.
            let denied = matches!(tools.execute_json(name, b"{}"), Err(ToolError::Denied));
            assert_eq!(
                denied,
                !enabled.contains(&name),
                "{}: {name}",
                profile.name()
            );
        }
        assert_eq!(fs::read_to_string(dir.path().join("file")).unwrap(), "a");
    }
}
