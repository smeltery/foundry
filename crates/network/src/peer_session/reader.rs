use super::*;
use tokio::io::AsyncReadExt;

/// Opaque complete frame from one actual authenticated connection. The binding
/// cannot be asserted through JSON or manufactured by a caller of receive_frame.
/// No Debug/Serialize: a frame may contain team bearer secrets or message text.
pub struct ReceivedFrame {
    binding: Arc<()>,
    frame: PeerFrame,
}

/// Dedicated reader for a finite parent actor. Keep next() running independently
/// of outbound commands; cancelling it invalidates the stream boundary. Use a
/// small bounded channel (for example capacity two) for ReceivedFrame. This type
/// decodes framing only; the session revalidates durable authority on dispatch.
pub struct PeerReader {
    connection: Connection,
    receive: RecvStream,
    binding: Arc<()>,
    cancel: watch::Receiver<bool>,
    deadline: Duration,
    usable: bool,
}
impl PeerReader {
    pub async fn next(&mut self) -> Result<ReceivedFrame> {
        if !self.usable {
            return Err(PeerError::Closed);
        }
        self.usable = false;
        let result = async {
            // Idle connections have no partially read record to time out. Once
            // the first byte arrives, one deadline covers the remaining frame.
            let first = tokio::select! {
                _ = async { loop {
                    if *self.cancel.borrow() || self.cancel.changed().await.is_err() { break; }
                }} => return Err(PeerError::Closed),
                first = self.receive.read_u8() => first.map_err(|_| PeerError::Closed)?,
            };
            let mut record = std::io::Cursor::new([first]).chain(&mut self.receive);
            let frame = io::bounded(
                &mut self.cancel,
                self.deadline,
                crate::read_frame(&mut record, self.deadline),
            )
            .await??
            .ok_or(PeerError::Closed)?;
            Ok(ReceivedFrame {
                binding: self.binding.clone(),
                frame,
            })
        }
        .await;
        self.usable = result.is_ok();
        if result.is_err() {
            self.connection.close(1u32.into(), b"reader closed");
        }
        result
    }
}
impl Drop for PeerReader {
    fn drop(&mut self) {
        self.connection.close(0u32.into(), b"reader dropped");
    }
}
impl PeerSession {
    /// Parent actor retains this session as sole writer/ACK owner, and gives this
    /// reader to one dedicated task. No read is cancelled for an outbound command.
    /// Drop either part on shutdown; close QUIC and cancel/reap the reader task.
    pub fn take_reader(&mut self) -> Result<PeerReader> {
        if !self.usable {
            return Err(PeerError::Closed);
        }
        let receive = self.receive.take().ok_or(PeerError::Closed)?;
        Ok(PeerReader {
            connection: self.connection.clone(),
            receive,
            binding: self.binding.clone(),
            cancel: self.options.cancel.clone(),
            deadline: self.options.deadline,
            usable: true,
        })
    }
    /// Consume only this connection's complete token. All effects and ACK
    /// correlation use the same implementations as pull-driven receive().
    pub async fn receive_frame(&mut self, token: ReceivedFrame) -> Result<PeerEvent> {
        self.begin()?;
        let result = if Arc::ptr_eq(&self.binding, &token.binding) {
            self.handle_frame(token.frame).await
        } else {
            Err(PeerError::Unauthorized)
        };
        self.completed(result)
    }
}
