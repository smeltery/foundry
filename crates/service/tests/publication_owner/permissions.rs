use super::*;
use foundry_contracts::permission::{PendingMode, PermissionDecision, PermissionProfile};
use foundry_runtime::coordinated::TrustedToolPolicy;
use foundry_service::permission::{ToolPermissionSettings, ToolPermissions};
use foundry_tools::host;

pub(super) fn selection(
    root: &std::path::Path,
    allow: bool,
) -> (TrustedToolPolicy, ToolPermissions) {
    let policy = TrustedToolPolicy::new(
        "/usr/bin/true".into(),
        cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority()).unwrap(),
        vec![host::Grant::Read],
        host::HostLimits {
            request_bytes: host::MAX_REQUEST_BYTES,
            read_bytes: 4096,
            write_bytes: 4096,
            output_bytes: host::MAX_REPLY_BYTES,
            entries: 10,
        },
        Duration::from_secs(2),
    )
    .unwrap()
    .require_authorization();
    let permissions = ToolPermissions::new(
        policy.identity(),
        ToolPermissionSettings {
            profile: PermissionProfile::Current,
            configuration_digest: [7; 32],
            generation: 1,
            grants: if allow {
                vec![host::Grant::Read]
            } else {
                vec![]
            },
            missing_grant: PermissionDecision::Pending,
            pending_mode: PendingMode::ActionRequired,
            request_timeout: Duration::from_secs(2),
        },
    )
    .unwrap();
    (policy, permissions)
}

#[test]
fn revoked_fresh_selection_is_rejected_before_opening_storage() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("must-not-exist.db");
    let (_, permissions) = selection(dir.path(), true);
    permissions.revocation().invalidate();
    assert!(
        FreshCaptureOwner::start_with_permissions(
            path.clone(),
            "session".into(),
            configuration(),
            RestoreOwnerOptions {
                lease: ServiceOptions {
                    lease_duration: Duration::from_secs(4),
                    poll_interval: Duration::from_millis(25)
                },
                cleanup_timeout: Duration::from_millis(100),
            },
            permissions,
        )
        .is_err()
    );
    assert!(!path.exists());
}

#[test]
fn mismatched_capture_permission_configuration_cannot_open_storage() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("must-not-exist.db");
    let (_, permissions) = selection(dir.path(), true);
    let result = FreshCaptureOwner::start_with_permissions(
        path.clone(),
        "session".into(),
        configuration(),
        RestoreOwnerOptions {
            lease: ServiceOptions {
                lease_duration: Duration::from_secs(4),
                poll_interval: Duration::from_millis(25),
            },
            cleanup_timeout: Duration::from_millis(100),
        },
        permissions,
    );
    assert!(matches!(
        result,
        Err(foundry_service::ServiceError::Invalid(
            "capture permission configuration mismatch"
        ))
    ));
    assert!(!path.exists());
}
