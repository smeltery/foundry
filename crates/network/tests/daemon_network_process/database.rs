use foundry_network::{full_team_store::FULL_TEAM_DATABASE, team::TeamDocument};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
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
/// Waits for the sender's durable claim: the text is on the wire, awaiting ACK.
pub async fn claimed(path: &Path, id: &str) -> i64 {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some((_, Some(attempt))) = transport(path, id) {
                return attempt;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("actual transport claim")
}
/// The complete signed team document from a stopped daemon's own checkpoint.
pub fn team_document(state: &Path) -> Option<TeamDocument> {
    let checkpoint = checkpoint(state);
    if checkpoint["state"]["kind"] != "active" {
        return None;
    }
    match serde_json::from_value(checkpoint["state"]["document"].clone()) {
        Ok(document) => Some(document),
        Err(_) => panic!("stored team document contract"),
    }
}
pub fn checkpoint(state: &Path) -> Value {
    let payload: Vec<u8> = reader(&state.join(FULL_TEAM_DATABASE))
        .query_row("SELECT payload FROM checkpoint WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    serde_json::from_slice(&payload).unwrap()
}
/// Every rollback-journal SQLite store a daemon owns, found from file headers
/// alone. WAL stores are skipped: another process must not open them live.
pub fn journal_stores(state: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(state)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|value| value == "sqlite3"))
        .filter(|path| {
            let mut header = [0; 20];
            std::io::Read::read_exact(&mut std::fs::File::open(path).unwrap(), &mut header)
                .unwrap();
            header[18] == 1
        })
        .collect();
    paths.sort();
    assert!(
        paths.iter().any(|path| path.ends_with("mail.sqlite3")),
        "daemon mail store uses a rollback journal"
    );
    paths
}
/// Fails if the owning daemon holds any read or write transaction on this
/// rollback-journal store for longer than `patience`.
pub fn uncontended(path: &Path, patience: Duration) {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    connection.busy_timeout(patience).unwrap();
    assert!(
        connection.execute_batch("BEGIN EXCLUSIVE").is_ok(),
        "daemon store transaction held across network I/O: {}",
        path.file_name().unwrap().to_string_lossy()
    );
    connection.execute_batch("ROLLBACK").unwrap();
}
pub fn received(path: &Path, id: &str) -> Vec<u8> {
    reader(path)
        .query_row("SELECT body FROM mail WHERE id=?1", [id], |row| row.get(0))
        .unwrap()
}
