use super::*;
use crate::daemon_engine::{InboxSummary, Reply};
use std::io::{self, Read};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    sync::{OwnedSemaphorePermit, mpsc},
};

pub(super) enum Response {
    Small(Reply),
    Inbox(PreparedReply),
}

#[cfg(test)]
#[path = "response_tests.rs"]
pub(super) mod tests;
impl Response {
    pub fn should_shutdown(&self) -> bool {
        matches!(self, Self::Small(reply) if reply.should_shutdown())
    }
    pub fn storage_uncertain(&self) -> bool {
        matches!(
            self,
            Self::Small(Reply {
                result: ReplyResult::Error(EngineError::StorageUncertain),
                ..
            })
        )
    }
    pub async fn write(self, writer: &mut (impl AsyncWrite + Unpin)) -> crate::Result<()> {
        match self {
            Self::Small(reply) => write_record(writer, &reply, WRITE_TIMEOUT).await,
            Self::Inbox(reply) => reply.write(writer).await,
        }
    }
}
pub(super) struct PreparedReply {
    file: std::fs::File,
    bytes: u64,
    ownership: Arc<ReplyOwnership>,
}
struct ReplyOwnership {
    _engine: Arc<OwnedEngine>,
    _slot: OwnedSemaphorePermit,
}
impl PreparedReply {
    pub fn new(
        spool: spool::Spool,
        summary: InboxSummary,
        slot: OwnedSemaphorePermit,
        engine: Arc<OwnedEngine>,
    ) -> io::Result<Self> {
        Ok(Self {
            file: spool.into_file(summary.bytes)?,
            bytes: summary.bytes,
            ownership: Arc::new(ReplyOwnership {
                _slot: slot,
                _engine: engine,
            }),
        })
    }
    async fn write(self, writer: &mut (impl AsyncWrite + Unpin)) -> crate::Result<()> {
        let _ownership = self.ownership.clone();
        let (chunks, mut receive) = mpsc::channel(2);
        // One off-executor reader owns the file, slot and actual lease. Dropping
        // the async writer closes its channel; an in-flight file read keeps its
        // ownership until it returns, including after timeout or task abort.
        let reading = tokio::task::spawn_blocking(move || -> io::Result<()> {
            let mut reply = self;
            let mut remaining = reply.bytes;
            while remaining != 0 {
                let mut buffer = vec![0; remaining.min(64 * 1024) as usize];
                if let Err(error) = reply.file.read_exact(&mut buffer) {
                    let _ = chunks.blocking_send(Err(error));
                    return Ok(());
                }
                remaining -= buffer.len() as u64;
                if chunks.blocking_send(Ok(buffer)).is_err() {
                    return Ok(());
                }
            }
            Ok(())
        });
        tokio::time::timeout(WRITE_TIMEOUT, async {
            while let Some(chunk) = receive.recv().await {
                writer.write_all(&chunk?).await?;
            }
            reading
                .await
                .map_err(|_| io::Error::other("reply reader failed"))??;
            writer.flush().await?;
            Ok(())
        })
        .await
        .map_err(|_| crate::NetworkError::TimedOut)?
    }
}
