use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{path::Path, time::Duration};
pub struct Claim {
    pub payload: Vec<u8>,
    pub attempt: Option<i64>,
    pub generation: Option<Vec<u8>>,
    pub revision: u64,
}
fn read(path: &Path) -> Connection {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    connection.busy_timeout(Duration::from_secs(1)).unwrap();
    connection
}
pub fn claim(path: &Path, id: &str) -> Option<Claim> {
    read(path)
        .query_row(
            "SELECT payload,attempt,generation,revision FROM transport_outbox WHERE id=?1",
            [id],
            |r| {
                Ok(Claim {
                    payload: r.get(0)?,
                    attempt: r.get(1)?,
                    generation: r.get(2)?,
                    revision: r.get(3)?,
                })
            },
        )
        .optional()
        .unwrap()
}
pub fn receiver(path: &Path, id: &str, expected: &[u8]) {
    let connection = read(path);
    let (count, state, read_ms): (u64, i64, Option<i64>) = connection
        .query_row(
            "SELECT count(*),max(state),max(read_ms) FROM mail WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(count, 1, "real receiver dedup row count");
    assert_eq!(state, 0);
    assert!(read_ms.is_none());
    let body: Vec<u8> = connection
        .query_row("SELECT body FROM mail WHERE id=?1", [id], |r| r.get(0))
        .unwrap();
    assert!(body == expected, "immutable received envelope changed");
}
