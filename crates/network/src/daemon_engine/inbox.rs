use super::*;
use std::io::{self, Write};
impl Engine {
    pub(super) fn inbox_checked(
        &mut self,
        connection: ConnectionId,
        auth: Auth,
        peek: bool,
        active: impl Fn() -> bool,
    ) -> Result<Outcome, EngineError> {
        self.authorize(connection, &auth)?;
        let recipient = mailbox_address(&auth.conversation);
        let now = self.now()?;
        let live = self
            .connections
            .get_mut(&connection)
            .ok_or(EngineError::Unauthorized)?;
        live.attempts
            .retain(|_, pending| pending.token.expires_ms() > now);
        let result = self.mail.poll_batch_checked(
            &recipient,
            &live.scope,
            now,
            peek,
            |reads| {
                let additional = reads
                    .iter()
                    .filter_map(|r| r.attempt.as_ref())
                    .filter(|a| !live.attempts.contains_key(&a.id()))
                    .count();
                if live.attempts.len() + additional > MAX_ATTEMPTS {
                    return Err(mail::MailError::Capacity);
                }
                prepare(reads)
            },
            active,
        )?;
        let Some((reads, outcome)) = result else {
            return Ok(Outcome::Inbox {
                messages: Vec::new(),
                text: String::new(),
            });
        };
        for read in reads {
            if let Some(token) = read.attempt {
                live.attempts.insert(
                    token.id(),
                    PendingAttempt {
                        conversation: auth.conversation.clone(),
                        token,
                    },
                );
            }
        }
        Ok(outcome)
    }
}
/// Neutral sender/body rendering. Exact client-specific presentation remains an
/// external compatibility boundary; nonempty mail never returns blank stub text.
fn prepare(reads: &[mail::PollRead]) -> mail::Result<Outcome> {
    let mut messages = Vec::with_capacity(reads.len());
    let mut text = String::new();
    let mut display_bytes = 0usize;
    for read in reads {
        let body = read.message.display_body()?;
        display_bytes = display_bytes
            .checked_add(body.len())
            .ok_or(mail::MailError::Capacity)?;
        // Base64 alone exceeds the frame if decoded bodies exceed this bound.
        if display_bytes > crate::ipc_codec::MAX_IPC_BYTES {
            return Err(mail::MailError::Capacity);
        }
        let body = std::str::from_utf8(&body).map_err(|_| mail::MailError::InvalidPayload)?;
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&read.message.sender);
        text.push_str(":\n");
        text.push_str(body);
        messages.push(Mail::from_message(&read.message, read.attempt.as_ref())?);
    }
    let outcome = Outcome::Inbox { messages, text };
    // Largest legal correlation ID reserves worst-case envelope overhead. Count
    // exact JSON escaping, structured bodies, rendering and terminating newline.
    let preview = Reply {
        id: u64::MAX,
        result: ReplyResult::Ok(outcome),
    };
    serde_json::to_writer(FrameBudget(1), &preview).map_err(|_| mail::MailError::Capacity)?;
    match preview.result {
        ReplyResult::Ok(outcome) => Ok(outcome),
        _ => Err(mail::MailError::InvalidInput),
    }
}
struct FrameBudget(usize);
impl Write for FrameBudget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > crate::ipc_codec::MAX_IPC_BYTES.saturating_sub(self.0) {
            return Err(io::Error::other("inbox batch exceeds frame limit"));
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
