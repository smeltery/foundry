use super::*;
impl Engine {
    pub(super) fn inbox(
        &mut self,
        connection: ConnectionId,
        auth: Auth,
        peek: bool,
    ) -> Result<Outcome, EngineError> {
        self.inbox_checked(connection, auth, peek, || true)
    }
    pub(super) fn finish(
        &mut self,
        connection: ConnectionId,
        auth: Auth,
        attempt_id: i64,
        delivered: bool,
    ) -> Result<Outcome, EngineError> {
        self.authorize(connection, &auth)?;
        let now = self.now()?;
        let live = self
            .connections
            .get_mut(&connection)
            .ok_or(EngineError::Unauthorized)?;
        self.mail.finish_issued_peek(
            attempt_id,
            &live.scope,
            &mailbox_address(&auth.conversation),
            now,
            delivered,
        )?;
        Ok(if delivered {
            Outcome::Delivered
        } else {
            Outcome::PushFailed
        })
    }
}
