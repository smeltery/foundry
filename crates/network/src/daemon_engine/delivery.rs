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
        let pending = live
            .attempts
            .get(&attempt_id)
            .ok_or(EngineError::StaleAttempt)?;
        if pending.conversation != auth.conversation {
            return Err(EngineError::Unauthorized);
        }
        let result = if delivered {
            self.mail.acknowledge(&pending.token, &live.scope, now)
        } else {
            self.mail.fail(&pending.token, &live.scope, now)
        };
        match result {
            Ok(()) => {
                live.attempts.remove(&attempt_id);
            }
            Err(mail::MailError::StaleAttempt) => {
                live.attempts.remove(&attempt_id);
                return Err(EngineError::StaleAttempt);
            }
            Err(error) => return Err(error.into()),
        }
        Ok(if delivered {
            Outcome::Delivered
        } else {
            Outcome::PushFailed
        })
    }
}
