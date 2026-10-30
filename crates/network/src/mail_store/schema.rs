use super::*;
const APP: i64 = 0x464d414c;
pub(super) const MAIL_V2: &str = "CREATE TABLE mail(id TEXT PRIMARY KEY NOT NULL,sender TEXT NOT NULL,recipient TEXT NOT NULL,body BLOB NOT NULL,reply_to TEXT,wake INTEGER NOT NULL CHECK(wake IN (0,1)),notify INTEGER NOT NULL CHECK(notify IN (0,1)),state INTEGER NOT NULL CHECK(state BETWEEN 0 AND 3),tries INTEGER NOT NULL DEFAULT 0 CHECK(tries BETWEEN 0 AND 8),next_ms INTEGER NOT NULL DEFAULT 0 CHECK(next_ms>=0),attempt INTEGER,session TEXT,expires INTEGER,CHECK((attempt IS NULL AND session IS NULL AND expires IS NULL) OR (state=0 AND attempt IS NOT NULL AND session IS NOT NULL AND expires IS NOT NULL)))";
const SEQUENCE: &str = "CREATE TABLE sequence(id INTEGER PRIMARY KEY CHECK(id=1),value INTEGER NOT NULL CHECK(value>=0))";
const READ_COLUMN: &str =
    "read_ms INTEGER CHECK(read_ms IS NULL OR (read_ms>=0 AND state=0 AND attempt IS NULL))";
const ENCODING_COLUMN: &str = "encoding INTEGER NOT NULL DEFAULT 0 CHECK(encoding IN (0,1))";
const POLL: &str = "CREATE TABLE poll_attempt(id INTEGER PRIMARY KEY,message_id TEXT NOT NULL REFERENCES mail(id),session TEXT NOT NULL,expires INTEGER NOT NULL,UNIQUE(message_id,session))";
fn mail_v3() -> String {
    MAIL_V2.replace(
        ",CHECK((attempt",
        &format!(", {READ_COLUMN}, {ENCODING_COLUMN},CHECK((attempt"),
    )
}
fn verify(connection: &Connection, version: i64) -> Result<()> {
    let count: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?;
    if count != if version == 2 { 2 } else { 3 } {
        return Err(MailError::Schema);
    }
    let mut expected = vec![
        (
            "mail",
            if version == 2 {
                MAIL_V2.to_owned()
            } else {
                mail_v3()
            },
        ),
        ("sequence", SEQUENCE.to_owned()),
    ];
    if version == 3 {
        expected.push(("poll_attempt", POLL.to_owned()));
    }
    for (name, sql) in expected {
        let actual: String =
            connection.query_row("SELECT sql FROM sqlite_schema WHERE name=?1", [name], |r| {
                r.get(0)
            })?;
        if actual != sql {
            return Err(MailError::Schema);
        }
    }
    Ok(())
}
/// v1/unknown schemas are rejected without rewriting data. v2 is an exact checked
/// additive migration: no dropped tables, deleted mail or reset sequence/retry state.
pub(super) fn initialize(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let app: i64 = tx.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let count: i64 = tx.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?;
    if app == 0 && version == 0 && count == 0 {
        tx.execute_batch(MAIL_V2)?;
        tx.execute_batch(SEQUENCE)?;
        tx.execute("INSERT INTO sequence VALUES(1,0)", [])?;
    } else if app == APP && matches!(version, 2 | 3) {
        verify(&tx, version)?;
    } else {
        return Err(MailError::Schema);
    }
    if version != 3 {
        tx.execute_batch(&format!("ALTER TABLE mail ADD COLUMN {READ_COLUMN}"))?;
        tx.execute_batch(&format!("ALTER TABLE mail ADD COLUMN {ENCODING_COLUMN}"))?;
        tx.execute_batch(POLL)?;
        tx.pragma_update(None, "application_id", APP)?;
        tx.pragma_update(None, "user_version", 3)?;
    }
    verify(&tx, 3)?;
    tx.commit()?;
    Ok(())
}
