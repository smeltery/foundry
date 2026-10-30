#![cfg(unix)]
use cap_std::{ambient_authority, fs::Dir};
use foundry_network::{
    daemon_config::DaemonConfig,
    daemon_host::Daemon,
    daemon_lease::{DaemonLease, LeaseError},
    ipc_codec::{read_record, write_record},
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};
use tokio::{io::BufReader, net::UnixStream};

struct State(PathBuf);
impl State {
    fn new() -> Self {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path = PathBuf::from("/tmp").join(format!("cancel-{:016x}", u64::from_le_bytes(nonce)));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn config(&self) -> DaemonConfig {
        serde_json::from_value(json!({"state_directory":self.0,"socket_directory":self.0,"person":"alice","machine":"local","network":"disabled","folders":[]})).unwrap()
    }
    fn directory(&self) -> Dir {
        Dir::open_ambient_dir(&self.0, ambient_authority()).unwrap()
    }
}
impl Drop for State {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

async fn await_released(directory: &Dir) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match DaemonLease::acquire(directory) {
                Ok(lease) => {
                    drop(lease);
                    break;
                }
                Err(LeaseError::AlreadyRunning) => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                Err(error) => panic!("unexpected lease error: {error}"),
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cancelled_server_retains_lease_until_blocked_registration_finishes() {
    let state = State::new();
    let daemon = Daemon::open(state.config()).await.unwrap();
    let path = daemon.path().to_owned();
    let owner = tokio::spawn(daemon.serve());
    let mut client = BufReader::new(UnixStream::connect(path).await.unwrap());
    write_record(
        client.get_mut(),
        &json!({"id":1,"op":"who"}),
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    assert!(
        read_record::<Value>(&mut client, Duration::from_secs(2))
            .await
            .unwrap()
            .is_some()
    );
    let sql = rusqlite::Connection::open(state.0.join("sessions.sqlite3")).unwrap();
    sql.execute_batch("BEGIN IMMEDIATE").unwrap();
    write_record(
        client.get_mut(),
        &json!({"id":2,"op":"register","name":"retained","repository":state.0}),
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    owner.abort();
    assert!(owner.await.unwrap_err().is_cancelled());
    let directory = state.directory();
    assert!(matches!(
        DaemonLease::acquire(&directory),
        Err(LeaseError::AlreadyRunning)
    ));
    sql.execute_batch("ROLLBACK").unwrap();
    await_released(&directory).await;
    let bytes: Vec<u8> = sql
        .query_row("SELECT payload FROM registry", [], |row| row.get(0))
        .unwrap();
    let document: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        document["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == "retained")
    );
}

#[tokio::test]
async fn cancelled_initialization_retains_lease_until_sqlite_work_finishes() {
    let state = State::new();
    drop(Daemon::open(state.config()).await.unwrap());
    let sql = rusqlite::Connection::open(state.0.join("sessions.sqlite3")).unwrap();
    sql.execute_batch("BEGIN IMMEDIATE").unwrap();
    let config = state.config();
    let opening = tokio::spawn(Daemon::open(config));
    tokio::time::sleep(Duration::from_millis(300)).await;
    opening.abort();
    assert!(opening.await.err().unwrap().is_cancelled());
    let directory = state.directory();
    assert!(matches!(
        DaemonLease::acquire(&directory),
        Err(LeaseError::AlreadyRunning)
    ));
    sql.execute_batch("ROLLBACK").unwrap();
    await_released(&directory).await;
}

#[tokio::test]
async fn cancelled_long_poll_retains_lease_and_does_not_consume_after_cancellation() {
    let state = State::new();
    let daemon = Daemon::open(state.config()).await.unwrap();
    let path = daemon.path().to_owned();
    let owner = daemon.spawn();
    let mut client = BufReader::new(UnixStream::connect(path).await.unwrap());
    write_record(
        client.get_mut(),
        &json!({"id":1,"op":"register","name":"reader","repository":state.0}),
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    let reply = read_record::<Value>(&mut client, Duration::from_secs(2))
        .await
        .unwrap()
        .unwrap();
    let auth = reply["result"]["value"]["auth"].clone();
    write_record(client.get_mut(), &json!({"id":2,"op":"send","auth":auth,"message_id":"pending","to":{"name":"reader"},"body":"keep"}), Duration::from_secs(2)).await.unwrap();
    assert_eq!(
        read_record::<Value>(&mut client, Duration::from_secs(2))
            .await
            .unwrap()
            .unwrap()["result"]["status"],
        "ok"
    );
    let sql = rusqlite::Connection::open(state.0.join("mail.sqlite3")).unwrap();
    sql.execute_batch("BEGIN IMMEDIATE").unwrap();
    write_record(
        client.get_mut(),
        &json!({"id":3,"op":"inbox","auth":auth,"wait_secs":600}),
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    owner.abort();
    let directory = state.directory();
    assert!(matches!(
        DaemonLease::acquire(&directory),
        Err(LeaseError::AlreadyRunning)
    ));
    sql.execute_batch("ROLLBACK").unwrap();
    // Intentionally freeze this executor: deferred child destructors cannot run
    // while the independent blocking worker resumes against the unlocked database.
    std::thread::sleep(Duration::from_millis(200));
    let observed: Option<i64> = sql
        .query_row("SELECT read_ms FROM mail WHERE id='pending'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(
        observed.is_none(),
        "cancelled poll committed before child destruction"
    );
    assert!(owner.await.unwrap_err().is_cancelled());
    await_released(&directory).await;
    let (body, read): (Vec<u8>, Option<i64>) = sql
        .query_row(
            "SELECT body,read_ms FROM mail WHERE id='pending'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(body, b"keep");
    assert!(read.is_none());
}
