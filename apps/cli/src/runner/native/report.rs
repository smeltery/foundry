use super::*;
use foundry_service::{ServiceError, restoration_owner::RestoreOwnerReport};

pub(super) fn check_report(
    report: RestoreOwnerReport,
    execution_failed: bool,
) -> Result<(), CommandError> {
    let execution = if execution_failed {
        "failed"
    } else {
        "completed"
    };
    let operation = report.operation.as_ref().err().map(label).unwrap_or("ok");
    let release = report.release.as_ref().err().map(label).unwrap_or("ok");
    if execution_failed
        || report.expired
        || report.watchdog_panicked
        || operation != "ok"
        || release != "ok"
    {
        return Err(failure(&format!(
            "capture owner failed (execution={execution}, operation={operation}, release={release}, expired={}, watchdog={})",
            report.expired, report.watchdog_panicked
        )));
    }
    Ok(())
}

// Only fixed local categories enter diagnostics; never format a source error.
fn label(error: &ServiceError) -> &'static str {
    match error {
        ServiceError::Store(_) => "storage",
        ServiceError::Runtime(_) => "runtime",
        ServiceError::Invalid(_) => "configuration",
        ServiceError::Coordination(message) => match *message {
            "native publication unsupported" => "publication_unsupported",
            "native publication invalid" => "publication_invalid",
            "native publication policy rejected" => "publication_policy",
            "native publication mapping rejected" => "publication_mapping",
            "native publication cancelled" => "publication_cancelled",
            "native publication deadline" | "publication comparison deadline" => {
                "publication_deadline"
            }
            "native publication cleanup failed" => "publication_cleanup",
            "native publication internal failure" => "publication_internal",
            "publication cut changed" => "publication_cut",
            "publication witness validation binding mismatch" => "publication_binding",
            "publication attestation mismatch" | "publication result correlation mismatch" => {
                "publication_correlation"
            }
            _ => "coordination",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operation_rejection_is_distinct_from_release_and_liveness_failures() {
        let error = check_report(
            RestoreOwnerReport {
                expired: false,
                watchdog_panicked: false,
                operation: Err(ServiceError::Coordination(
                    "native publication mapping rejected",
                )),
                release: Ok(None),
            },
            false,
        )
        .err()
        .unwrap();
        assert_eq!(
            error.message,
            "capture owner failed (execution=completed, operation=publication_mapping, release=ok, expired=false, watchdog=false)"
        );
        let error = check_report(
            RestoreOwnerReport {
                expired: true,
                watchdog_panicked: true,
                operation: Ok(()),
                release: Err(ServiceError::Invalid("private canary")),
            },
            false,
        )
        .err()
        .unwrap();
        assert_eq!(
            error.message,
            "capture owner failed (execution=completed, operation=ok, release=configuration, expired=true, watchdog=true)"
        );
        assert!(!error.message.contains("private canary"));
    }
    #[test]
    fn successful_report_remains_successful() {
        assert!(
            check_report(
                RestoreOwnerReport {
                    expired: false,
                    watchdog_panicked: false,
                    operation: Ok(()),
                    release: Ok(None)
                },
                false
            )
            .is_ok()
        );
    }
    #[test]
    fn runtime_execution_failure_survives_successful_owner_release() {
        let error = check_report(
            RestoreOwnerReport {
                expired: false,
                watchdog_panicked: false,
                operation: Ok(()),
                release: Ok(None),
            },
            true,
        )
        .err()
        .unwrap();
        assert_eq!(
            error.message,
            "capture owner failed (execution=failed, operation=ok, release=ok, expired=false, watchdog=false)"
        );
    }
}
