use super::*;
use crate::daemon_engine::{Auth, RequestEnvelope};
use crate::daemon_host::{connection, response::tests::State};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

#[derive(Clone, Copy)]
enum Block {
    Sqlite,
    Slot,
    Engine,
}
fn dispatch(engine: &OwnedEngine, connection: u64, value: Value) -> Value {
    let request: RequestEnvelope = serde_json::from_value(value).unwrap();
    serde_json::to_value(
        engine
            .inner
            .lock()
            .unwrap()
            .handle(ConnectionId(connection), request),
    )
    .unwrap()
}
async fn until(mut condition: impl FnMut() -> bool) -> bool {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok()
}
async fn disconnected_read(seconds: u64, block: Block) {
    let state = State::new();
    let engine = state.engine();
    let registered = dispatch(
        &engine,
        1,
        json!({
            "id":1,"op":"register","name":"reader","repository":state.0
        }),
    );
    let auth = registered["result"]["value"]["auth"].clone();
    let native: Auth = serde_json::from_value(auth.clone()).unwrap();
    let sent = dispatch(
        &engine,
        1,
        json!({"id":2,"op":"send","auth":auth,
        "message_id":"pending","to":{"name":"reader"},"body":"retained"}),
    );
    assert_eq!(sent["result"]["status"], "ok");
    assert_eq!(
        dispatch(&engine, 2, json!({"id":3,"op":"attach","auth":auth}))["result"]["status"],
        "ok"
    );
    let registry = Arc::new(WaitRegistry::default());
    let fence = registry.connection(ConnectionId(1));
    let _other = registry.connection(ConnectionId(2));
    let control = Arc::new(
        engine
            .inner
            .lock()
            .unwrap()
            .begin_inbox_read(ConnectionId(2), &native)
            .unwrap(),
    );
    registry.register(ConnectionId(2), &control);
    let sql = rusqlite::Connection::open(state.0.join("mail.sqlite3")).unwrap();
    let mut held_slot = None;
    let mut engine_release = None;
    let mut engine_worker = None;
    match block {
        Block::Sqlite => sql.execute_batch("BEGIN IMMEDIATE").unwrap(),
        Block::Slot => held_slot = Some(engine.replies.clone().acquire_owned().await.unwrap()),
        Block::Engine => {
            let (started, ready) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            let owner = engine.clone();
            engine_worker = Some(tokio::task::spawn_blocking(move || {
                let _guard = owner.inner.lock().unwrap();
                let _ = started.send(());
                wait.recv().unwrap();
            }));
            ready.await.unwrap();
            engine_release = Some(release);
        }
    }
    let (server, mut client) = UnixStream::pair().unwrap();
    let (shutdown, _) = watch::channel(false);
    let running = tokio::spawn(connection::run(
        server,
        engine.clone(),
        ConnectionId(1),
        shutdown,
        fence,
    ));
    write_record(
        &mut client,
        &json!({"id":4,"op":"inbox","auth":auth,"wait_secs":seconds}),
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    if !matches!(block, Block::Engine) {
        assert!(
            until(
                || registry.state.lock().unwrap().connections[&ConnectionId(1)]
                    .iter()
                    .any(|w| w.upgrade().is_some())
            )
            .await
        );
    }
    if matches!(block, Block::Sqlite) {
        assert!(until(|| engine.inner.try_lock().is_err()).await);
    }
    client.shutdown().await.unwrap();
    drop(client);
    // This observes the reader's publication directly. No Engine access or
    // ancestor cleanup can establish it while the selected gate is blocked.
    let fenced = until(|| {
        !registry
            .state
            .lock()
            .unwrap()
            .connections
            .contains_key(&ConnectionId(1))
    })
    .await;
    let control_kept = !control.cancelled();
    let before: Option<i64> = sql
        .query_row("SELECT read_ms FROM mail WHERE id='pending'", [], |row| {
            row.get(0)
        })
        .unwrap();
    match block {
        Block::Sqlite => sql.execute_batch("ROLLBACK").unwrap(),
        Block::Slot => drop(held_slot.take()),
        Block::Engine => engine_release.take().unwrap().send(()).unwrap(),
    }
    // Freeze this current-thread executor after unlocking SQLite: a blocking
    // worker may resume, but deferred connection/child destructors cannot help.
    if matches!(block, Block::Sqlite) {
        std::thread::sleep(Duration::from_millis(200));
    }
    let after_unlock: Option<i64> = sql
        .query_row("SELECT read_ms FROM mail WHERE id='pending'", [], |row| {
            row.get(0)
        })
        .unwrap();
    if let Some(worker) = engine_worker {
        worker.await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(3), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(until(|| engine.replies.available_permits() == 1).await);
    drop(engine);
    let reopened = crate::mail_store::MailStore::open(&state.0).unwrap();
    let row = reopened.get("pending").unwrap();
    let after: Option<i64> = sql
        .query_row("SELECT read_ms FROM mail WHERE id='pending'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(fenced, "reader did not fence disconnected connection");
    assert!(control_kept, "disconnect cancelled another connection");
    assert_eq!(before, None);
    assert_eq!(
        after_unlock, None,
        "read committed without ancestor cleanup yielding"
    );
    assert_eq!(after, None, "disconnected read consumed pending mail");
    assert_eq!(row.message.body, b"retained");
    assert_eq!(row.state, crate::mail_store::State::Pending);
}

#[tokio::test]
async fn eof_fences_immediate_sqlite_read_before_unlock_without_cleanup_yield() {
    disconnected_read(0, Block::Sqlite).await;
}
#[tokio::test]
async fn eof_fences_positive_sqlite_wait_before_unlock_without_cleanup_yield() {
    disconnected_read(600, Block::Sqlite).await;
}
#[tokio::test]
async fn eof_fences_immediate_read_waiting_for_the_reply_slot() {
    disconnected_read(0, Block::Slot).await;
}
#[tokio::test]
async fn eof_rejects_registration_after_engine_mutex_unblocks() {
    disconnected_read(0, Block::Engine).await;
}
