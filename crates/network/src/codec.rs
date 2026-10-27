use crate::{NetworkError, PeerFrame, Result};
use std::{io, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

struct BoundedPayload(Vec<u8>);
impl io::Write for BoundedPayload {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FRAME_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::other("frame exceeds limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A timeout or read error makes this stream unusable: discard it rather than
/// retrying from an unknown byte boundary. None denotes EOF before any prefix.
pub async fn read_frame(
    reader: &mut (impl AsyncRead + Unpin),
    timeout: Duration,
) -> Result<Option<PeerFrame>> {
    if timeout.is_zero() {
        return Err(NetworkError::Invalid("positive frame timeout required"));
    }
    tokio::time::timeout(timeout, async {
        let mut prefix = [0; 4];
        if reader.read(&mut prefix[..1]).await? == 0 {
            return Ok(None);
        }
        reader.read_exact(&mut prefix[1..]).await?;
        let length = u32::from_be_bytes(prefix) as usize;
        if length == 0 || length > MAX_FRAME_BYTES {
            return Err(NetworkError::Invalid("frame length outside allowed range"));
        }
        let mut payload = vec![0; length];
        reader.read_exact(&mut payload).await?;
        Ok(Some(serde_json::from_slice(&payload)?))
    })
    .await
    .map_err(|_| NetworkError::TimedOut)?
}

/// Serialize into a bounded buffer before writing the first byte. On output
/// failure or timeout discard the stream; an incomplete frame may have escaped.
pub async fn write_frame(
    writer: &mut (impl AsyncWrite + Unpin),
    frame: &PeerFrame,
    timeout: Duration,
) -> Result<()> {
    if timeout.is_zero() {
        return Err(NetworkError::Invalid("positive frame timeout required"));
    }
    let mut payload = BoundedPayload(Vec::new());
    serde_json::to_writer(&mut payload, frame)?;
    tokio::time::timeout(timeout, async {
        writer
            .write_all(&(payload.0.len() as u32).to_be_bytes())
            .await?;
        writer.write_all(&payload.0).await?;
        writer.flush().await?;
        Ok(())
    })
    .await
    .map_err(|_| NetworkError::TimedOut)?
}
