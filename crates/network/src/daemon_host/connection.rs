use super::*;
use crate::daemon_engine::{Reply, Request};
use tokio::{io::AsyncBufReadExt, sync::mpsc, task::JoinHandle};
struct ReaderTask(JoinHandle<()>);
impl Drop for ReaderTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}
/// A dedicated bounded reader prevents a wait completion from cancelling a partly
/// consumed record. Its idle wait is governed by the owner; record bytes get 60s.
fn reader(
    stream: tokio::net::unix::OwnedReadHalf,
) -> (
    ReaderTask,
    mpsc::Receiver<crate::Result<Option<serde_json::Value>>>,
) {
    let (send, receive) = mpsc::channel(8);
    let task = tokio::spawn(async move {
        let mut reader = BufReader::new(stream);
        loop {
            let record = match reader.fill_buf().await {
                Ok([]) => Ok(None),
                Ok(_) => read_record(&mut reader, REQUEST_TIMEOUT).await,
                Err(error) => Err(error.into()),
            };
            let end = !matches!(record, Ok(Some(_)));
            if send.send(record).await.is_err() || end {
                break;
            }
        }
    });
    (ReaderTask(task), receive)
}
async fn dispatch(
    engine: Arc<OwnedEngine>,
    connection: ConnectionId,
    request: RequestEnvelope,
) -> Result<Reply, HostError> {
    tokio::task::spawn_blocking(move || {
        Ok(engine
            .inner
            .lock()
            .map_err(|_| HostError::Storage)?
            .handle(connection, request))
    })
    .await
    .map_err(|_| HostError::Task)?
}
pub(super) async fn run(
    stream: UnixStream,
    engine: Arc<OwnedEngine>,
    connection: ConnectionId,
    shutdown: watch::Sender<bool>,
    fence: cancellation::ConnectionFence,
) -> Result<(), HostError> {
    let mut stopping = shutdown.subscribe();
    let (input, mut writer) = stream.into_split();
    let (_reader, mut records) = reader(input);
    struct Work {
        fence: cancellation::ConnectionFence,
        waits: JoinSet<Result<Reply, HostError>>,
    }
    let mut work = Work {
        fence,
        waits: JoinSet::new(),
    };
    let mut result = Ok(());
    loop {
        if *stopping.borrow() {
            break;
        }
        let reply = tokio::select! {
            _ = stopping.changed() => break,
            _ = tokio::time::sleep(REQUEST_TIMEOUT), if work.waits.is_empty() => break,
            finished = work.waits.join_next(), if !work.waits.is_empty() => {
                match finished { Some(Ok(Ok(reply))) => reply, _ => { result = Err(HostError::Task); break; } }
            }
            record = records.recv() => {
                let request = match record {
                    Some(Ok(Some(value))) => {
                        let id = value.get("id").and_then(serde_json::Value::as_u64);
                        match serde_json::from_value::<RequestEnvelope>(value) {
                            Ok(request) => request,
                            Err(_) => {
                                let error = serde_json::json!({"id":id,"result":{"status":"error","value":"invalid_input"}});
                                if write_record(&mut writer, &error, WRITE_TIMEOUT).await.is_err() { break; }
                                continue;
                            }
                        }
                    }
                    Some(Ok(None)) | None => break,
                    Some(Err(_)) => {
                        let error = serde_json::json!({"id":null,"result":{"status":"error","value":"invalid_input"}});
                        let _ = write_record(&mut writer, &error, WRITE_TIMEOUT).await;
                        break;
                    }
                };
                if let Request::Inbox { auth, peek, wait_secs } = &request.request
                    && *wait_secs > 0 {
                    if work.waits.len() >= 8 {
                        Reply { id: request.id, result: ReplyResult::Error(EngineError::Limit) }
                    } else {
                        match waiting::prepare(engine.clone(), connection, auth.clone(), &work.fence).await {
                            Ok(wait) => {
                                let engine = engine.clone(); let peek = *peek; let seconds = *wait_secs; let id = request.id;
                                work.waits.spawn(async move { waiting::poll(engine, wait, id, peek, seconds).await });
                                continue;
                            }
                            Err(error) => Reply { id: request.id, result: ReplyResult::Error(error) },
                        }
                    }
                } else {
                    match dispatch(engine.clone(), connection, request).await {
                        Ok(reply) => reply,
                        Err(error) => { result = Err(error); break; }
                    }
                }
            }
        };
        let stop = reply.should_shutdown();
        let uncertain = matches!(
            reply.result,
            ReplyResult::Error(EngineError::StorageUncertain)
        );
        let written = write_record(&mut writer, &reply, WRITE_TIMEOUT).await;
        if stop || uncertain {
            if uncertain {
                result = Err(HostError::Storage);
            }
            let _ = shutdown.send(true);
            break;
        }
        if written.is_err() {
            break;
        }
    }
    work.fence.cancel();
    work.waits.abort_all();
    while work.waits.join_next().await.is_some() {}
    let cleanup = tokio::task::spawn_blocking(move || {
        engine
            .inner
            .lock()
            .map_err(|_| HostError::Storage)?
            .disconnect(connection)
            .map_err(|_| HostError::Storage)
    })
    .await
    .map_err(|_| HostError::Task)?;
    result.and(cleanup)
}
