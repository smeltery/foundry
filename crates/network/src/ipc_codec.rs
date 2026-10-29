//! Bounded JSON-line transport for local daemon connections.
//! After any error, discard the connection: a partial frame may be consumed.
use crate::{NetworkError, Result};
use serde::{Serialize, de::DeserializeOwned};
use std::{io, time::Duration};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_IPC_BYTES: usize = 1024 * 1024;

/// EOF is clean only between records. The deadline covers the entire record,
/// so a client cannot keep a connection alive by slowly dripping bytes.
pub async fn read_record<T: DeserializeOwned>(
    reader: &mut (impl AsyncBufRead + Unpin),
    timeout: Duration,
) -> Result<Option<T>> {
    read_bounded_record(reader, timeout, MAX_IPC_BYTES).await
}

/// Decode a complete inbox response. Requests retain their independent 1 MiB
/// bound; this ceiling derives from the mailbox's admitted rows and body bytes.
pub async fn read_reply<T: DeserializeOwned>(
    reader: &mut (impl AsyncBufRead + Unpin),
    timeout: Duration,
) -> Result<Option<T>> {
    read_bounded_record(
        reader,
        timeout,
        crate::daemon_engine::MAX_INBOX_REPLY_BYTES - 1,
    )
    .await
}

async fn read_bounded_record<T: DeserializeOwned>(
    reader: &mut (impl AsyncBufRead + Unpin),
    timeout: Duration,
    maximum: usize,
) -> Result<Option<T>> {
    if timeout.is_zero() {
        return Err(NetworkError::Invalid("positive IPC timeout required"));
    }
    tokio::time::timeout(timeout, async {
        let mut record = Vec::new();
        loop {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                return if record.is_empty() {
                    Ok(None)
                } else {
                    Err(NetworkError::Io(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "incomplete IPC record",
                    )))
                };
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let length = newline.unwrap_or(available.len());
            if length > maximum.saturating_sub(record.len()) {
                return Err(NetworkError::Invalid("IPC record exceeds limit"));
            }
            record.extend_from_slice(&available[..length]);
            reader.consume(length + usize::from(newline.is_some()));
            if newline.is_some() {
                return Ok(Some(serde_json::from_slice(&record)?));
            }
        }
    })
    .await
    .map_err(|_| NetworkError::TimedOut)?
}

struct Record(Vec<u8>);
impl io::Write for Record {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_IPC_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::other("IPC record exceeds limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Serialization and size validation finish before any bytes escape. A write
/// failure can still leave a partial record, requiring connection disposal.
pub async fn write_record<T: Serialize>(
    writer: &mut (impl AsyncWrite + Unpin),
    value: &T,
    timeout: Duration,
) -> Result<()> {
    if timeout.is_zero() {
        return Err(NetworkError::Invalid("positive IPC timeout required"));
    }
    let mut record = Record(Vec::new());
    serde_json::to_writer(&mut record, value)?;
    record.0.push(b'\n');
    tokio::time::timeout(timeout, async {
        writer.write_all(&record.0).await?;
        writer.flush().await?;
        Ok(())
    })
    .await
    .map_err(|_| NetworkError::TimedOut)?
}
