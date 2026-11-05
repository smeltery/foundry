use std::path::Path;

/// Expires every active attempt lease of a session by writing a past deadline.
///
/// Tests use this instead of a short lease and a sleep, so recovery runs
/// exactly when the test asks for it rather than whenever the scheduler
/// lets the wall clock pass the deadline.
pub fn expire_attempt_leases(path: &Path, session_id: &str) {
    let expired = rusqlite::Connection::open(path)
        .unwrap()
        .execute(
            "UPDATE attempts SET expires_ms=0 WHERE session_id=?1 AND active=1",
            [session_id],
        )
        .unwrap();
    assert!(expired > 0, "no active attempt lease to expire");
}
