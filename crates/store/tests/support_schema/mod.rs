/// Reverts the v13 and v12 migrations so a test can rebuild an older schema
/// faithfully. Column order matters: `origin_interface` has a check that names
/// `origin_kind`.
pub const UNDO_V12: &str = "ALTER TABLE sessions DROP COLUMN deleted_ms;
    DROP TABLE completion_decisions; DROP TABLE message_reconciliations;
    DROP TABLE session_tags; ALTER TABLE events DROP COLUMN position;
    ALTER TABLE events DROP COLUMN attempt_ordinal;
    ALTER TABLE sessions DROP COLUMN forked_from_session_id;
    ALTER TABLE sessions DROP COLUMN parent_session_id; ALTER TABLE sessions DROP COLUMN updated_ms;
    ALTER TABLE sessions DROP COLUMN created_ms; ALTER TABLE sessions DROP COLUMN origin_interface;
    ALTER TABLE sessions DROP COLUMN origin_kind; ALTER TABLE sessions DROP COLUMN title;
    ALTER TABLE sessions DROP COLUMN archived; ";
