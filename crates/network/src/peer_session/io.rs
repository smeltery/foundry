use super::*;
use std::future::Future;

pub(super) fn options(options: &SessionOptions) -> Result<()> {
    options.profile.validate().map_err(|_| PeerError::Invalid)?;
    if options.deadline.is_zero() || options.deadline > Duration::from_secs(60) {
        return Err(PeerError::Invalid);
    }
    Ok(())
}
pub(super) async fn bounded<T>(
    cancel: &mut watch::Receiver<bool>,
    deadline: Duration,
    work: impl Future<Output = T>,
) -> Result<T> {
    if *cancel.borrow() {
        return Err(PeerError::Closed);
    }
    tokio::select! {
        biased;
        _ = async { loop {
            if cancel.changed().await.is_err() || *cancel.borrow_and_update() { break; }
        }} => Err(PeerError::Closed),
        result = tokio::time::timeout(deadline, work) => result.map_err(|_| PeerError::TimedOut),
    }
}
pub(super) async fn write(
    send: &mut SendStream,
    frame: &PeerFrame,
    options: &mut SessionOptions,
) -> Result<()> {
    bounded(
        &mut options.cancel,
        options.deadline,
        crate::write_frame(send, frame, options.deadline),
    )
    .await??;
    Ok(())
}
pub(super) async fn read(
    receive: &mut RecvStream,
    options: &mut SessionOptions,
) -> Result<PeerFrame> {
    bounded(
        &mut options.cancel,
        options.deadline,
        crate::read_frame(receive, options.deadline),
    )
    .await??
    .ok_or(PeerError::Closed)
}
pub(super) async fn exchange(
    send: &mut SendStream,
    receive: &mut RecvStream,
    frame: &PeerFrame,
    options: &mut SessionOptions,
) -> Result<PeerFrame> {
    let deadline = options.deadline;
    let (_, incoming) = bounded(&mut options.cancel, deadline, async {
        tokio::try_join!(
            crate::write_frame(send, frame, deadline),
            crate::read_frame(receive, deadline)
        )
    })
    .await??;
    incoming.ok_or(PeerError::Closed)
}
pub(super) struct CloseGuard(pub Option<Connection>);
impl Drop for CloseGuard {
    fn drop(&mut self) {
        if let Some(connection) = &self.0 {
            connection.close(1u32.into(), b"session not established");
        }
    }
}
