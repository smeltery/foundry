use super::*;
use crate::daemon_host::response::tests::State;
use crate::daemon_lease::LeaseError;
use serde_json::json;

fn registered(engine: &OwnedEngine, state: &State) -> Auth {
    let request = serde_json::from_value(json!({
        "id":1,"op":"register","name":"reader","repository":state.0
    }))
    .unwrap();
    let reply = engine
        .inner
        .lock()
        .unwrap()
        .handle(ConnectionId(1), request);
    let value = serde_json::to_value(reply).unwrap();
    serde_json::from_value(value["result"]["value"]["auth"].clone()).unwrap()
}
#[tokio::test]
async fn cancelled_slot_wait_does_not_allocate_or_consume() {
    let state = State::new();
    let engine = state.engine();
    let auth = registered(&engine, &state);
    let wait = Arc::new(
        engine
            .inner
            .lock()
            .unwrap()
            .begin_inbox_wait(ConnectionId(1), &auth)
            .unwrap(),
    );
    let slot = engine.replies.clone().acquire_owned().await.unwrap();
    let waiting_engine = engine.clone();
    let waiting_token = wait.clone();
    let blocked = tokio::spawn(async move { acquire(&waiting_engine, &waiting_token).await });
    wait.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), blocked)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .is_none()
    );
    assert_eq!(engine.replies.available_permits(), 0);
    drop(slot);
    assert_eq!(engine.replies.available_permits(), 1);
}
#[tokio::test]
async fn aborted_preparation_keeps_slot_and_lease_until_sqlite_work_finishes() {
    let state = State::new();
    let engine = state.engine();
    let auth = registered(&engine, &state);
    let request = serde_json::from_value(json!({
        "id":2,"op":"send","auth":auth,"message_id":"kept",
        "to":{"name":"reader"},"body":"payload"
    }))
    .unwrap();
    let reply = engine
        .inner
        .lock()
        .unwrap()
        .handle(ConnectionId(1), request);
    assert_eq!(
        serde_json::to_value(reply).unwrap()["result"]["status"],
        "ok"
    );
    let wait = Arc::new(
        engine
            .inner
            .lock()
            .unwrap()
            .begin_inbox_wait(ConnectionId(1), &auth)
            .unwrap(),
    );
    let sql = rusqlite::Connection::open(state.0.join("mail.sqlite3")).unwrap();
    sql.execute_batch("BEGIN IMMEDIATE").unwrap();
    let slots = engine.replies.clone();
    let slot = slots.clone().acquire_owned().await.unwrap();
    let observing = Arc::downgrade(&engine);
    let worker = tokio::spawn(prepare_output(engine, wait.clone(), 3, false, slot));
    // Observe the worker holding the actual Engine mutex, not just a queued task.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if observing
                .upgrade()
                .is_some_and(|engine| engine.inner.try_lock().is_err())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    wait.cancel();
    worker.abort();
    assert!(matches!(worker.await, Err(error) if error.is_cancelled()));
    assert_eq!(slots.available_permits(), 0);
    assert!(matches!(
        DaemonLease::acquire(&state.root()),
        Err(LeaseError::AlreadyRunning)
    ));
    sql.execute_batch("ROLLBACK").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while slots.available_permits() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let read: Option<i64> = sql
        .query_row("SELECT read_ms FROM mail WHERE id='kept'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(read, None);
    assert!(DaemonLease::acquire(&state.root()).is_ok());
}
#[tokio::test]
async fn empty_long_poll_releases_slot_before_idle_wait() {
    let state = State::new();
    let engine = state.engine();
    let auth = registered(&engine, &state);
    let wait = Arc::new(
        engine
            .inner
            .lock()
            .unwrap()
            .begin_inbox_wait(ConnectionId(1), &auth)
            .unwrap(),
    );
    let slot = engine.replies.clone().acquire_owned().await.unwrap();
    let polling = tokio::spawn(poll(engine.clone(), wait.clone(), 4, false, 600));
    tokio::time::sleep(Duration::from_millis(30)).await;
    drop(slot);
    let available = tokio::time::timeout(
        Duration::from_secs(2),
        engine.replies.clone().acquire_owned(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!polling.is_finished());
    // Cancellation must wake the idle waiter even while another reply owns the slot.
    wait.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), polling)
            .await
            .unwrap()
            .unwrap()
            .is_ok()
    );
    drop(available);
}
