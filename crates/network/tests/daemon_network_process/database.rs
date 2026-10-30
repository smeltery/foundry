use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{
    path::Path,
    time::{Duration, Instant},
};

/// Locks existing owned fixture data without changing schema, payload or time.
pub struct WriteLock {
    connection: Connection,
    started: Instant,
}
impl WriteLock {
    pub fn new(path: &Path) -> Self {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        connection.busy_timeout(Duration::from_secs(1)).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        Self {
            connection,
            started: Instant::now(),
        }
    }
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}
impl Drop for WriteLock {
    fn drop(&mut self) {
        self.connection.execute_batch("ROLLBACK").unwrap();
    }
}
fn reader(path: &Path) -> Connection {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap()
}
pub fn transport(path: &Path, id: &str) -> Option<(Vec<u8>, Option<i64>)> {
    reader(path)
        .query_row(
            "SELECT payload,attempt FROM transport_outbox WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .unwrap()
}
pub fn received(path: &Path, id: &str) -> Vec<u8> {
    reader(path)
        .query_row("SELECT body FROM mail WHERE id=?1", [id], |row| row.get(0))
        .unwrap()
}
