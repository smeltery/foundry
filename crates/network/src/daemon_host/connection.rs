use super::*;
use crate::daemon_engine::{Reply, Request};
use response::Response;
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
    fence: cancellation::ConnectionFence,
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
            if end {
                // Publish EOF/errors before queueing: the owner may be blocked
                // preparing an immediate read or registering its guard.
                fence.cancel();
            }
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
    network: Option<crate::daemon_network::NetworkHandle>,
) -> Result<Reply, HostError> {
    network::EngineAccess::new(engine)
        .dispatch(connection, request, network.map(|n| n.authority()))
        .await
}
pub(super) async fn run(
    stream: UnixStream,
    engine: Arc<OwnedEngine>,
    connection: ConnectionId,
    shutdown: watch::Sender<bool>,
    fence: cancellation::ConnectionFence,
    network: Option<crate::daemon_network::NetworkHandle>,
) -> Result<(), HostError> {
    let mut stopping = shutdown.subscribe();
    let (input, mut writer) = stream.into_split();
    let (_reader, mut records) = reader(input, fence.reader_guard());
    struct Work {
        fence: cancellation::ConnectionFence,
        waits: JoinSet<Result<Response, HostError>>,
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
                        if value.get("op").and_then(serde_json::Value::as_str)==Some("network") {
                            let response=match serde_json::from_value::<crate::daemon_network::NetworkEnvelope>(value) {
                                Ok(envelope)=>match &network {
                                    Some(network)=>network.request(connection,envelope,work.fence.signal()).await,
                                    None=>crate::daemon_network::NetworkResponse::error(envelope.id,crate::daemon_network::ControlError::Disabled),
                                },
                                Err(_)=>crate::daemon_network::NetworkResponse::error(id.unwrap_or(0),crate::daemon_network::ControlError::Invalid),
                            };
                            if response.write(&mut writer).await.is_err() {break;}
                            continue;
                        }
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
                if let Request::Inbox { auth, peek, wait_secs } = &request.request {
                    if *wait_secs > 0 && work.waits.len() >= 8 {
                        Response::Small(Reply { id: request.id, result: ReplyResult::Error(EngineError::Limit) })
                    } else {
                        match waiting::prepare(engine.clone(), connection, auth.clone(), &work.fence, *wait_secs > 0).await {
                            Ok(wait) if *wait_secs == 0 => {
                                match waiting::poll(engine.clone(), wait, request.id, *peek, 0).await {
                                    Ok(reply) => reply,
                                    Err(error) => { result = Err(error); break; }
                                }
                            }
                            Ok(wait) => {
                                let engine = engine.clone(); let peek = *peek; let seconds = *wait_secs; let id = request.id;
                                work.waits.spawn(async move { waiting::poll(engine, wait, id, peek, seconds).await });
                                continue;
                            }
                            Err(error) => Response::Small(Reply { id: request.id, result: ReplyResult::Error(error) }),
                        }
                    }
                } else {
                    match dispatch(engine.clone(), connection, request, network.clone()).await {
                        Ok(reply) => Response::Small(reply),
                        Err(error) => { result = Err(error); break; }
                    }
                }
            }
        };
        let stop = reply.should_shutdown();
        let uncertain = reply.storage_uncertain();
        let written = reply.write(&mut writer).await;
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
