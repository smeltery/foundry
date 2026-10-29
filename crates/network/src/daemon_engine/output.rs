use super::*;
use std::io::{self, Write};

/// Host-private output stays unpublished until both sealing and durable commit
/// succeed. Sealing alone must never promote the builder to a readable reply.
pub trait InboxOutput: Write {
    fn seal(&mut self) -> io::Result<()>;
}
pub struct InboxSummary {
    pub messages: usize,
    pub bytes: u64,
}
/// JSON escaping, independent base64 rounding, duplicated identifiers, scalar
/// fields and the actual envelope/newline are bounded by durable admission.
pub const MAX_INBOX_REPLY_BYTES: usize = 6 * mail::MAX_MAIL_BODY_BYTES
    + 4 * mail::MAX_MAIL_BODY_BYTES.div_ceil(3)
    + (30 * mail::MAX_MAIL_IDENTIFIER + 512) * mail::MAX_MAIL_ROWS
    + 1024;

impl Engine {
    pub fn inbox_to_output(
        &mut self,
        connection: ConnectionId,
        auth: &Auth,
        id: u64,
        peek: bool,
        output: &mut impl InboxOutput,
    ) -> Result<Option<InboxSummary>, EngineError> {
        self.output_checked(connection, auth, id, peek, output, || true)
    }
    pub fn poll_inbox_wait_to_output(
        &mut self,
        wait: &InboxWait,
        id: u64,
        peek: bool,
        output: &mut impl InboxOutput,
    ) -> Result<Option<InboxSummary>, EngineError> {
        self.output_checked(wait.connection, &wait.auth, id, peek, output, || {
            !wait.cancelled()
        })
    }
    fn output_checked(
        &mut self,
        connection: ConnectionId,
        auth: &Auth,
        id: u64,
        peek: bool,
        output: &mut impl InboxOutput,
        active: impl Fn() -> bool,
    ) -> Result<Option<InboxSummary>, EngineError> {
        if self.uncertain {
            return Err(EngineError::StorageUncertain);
        }
        self.authorize(connection, auth)?;
        let now = self.now()?;
        let scope = &self
            .connections
            .get(&connection)
            .ok_or(EngineError::Unauthorized)?
            .scope;
        let result = self
            .mail
            .poll_incremental_checked(
                &mailbox_address(&auth.conversation),
                scope,
                now,
                peek,
                |batch| serialize(batch, id, output),
                active,
            )
            .map_err(EngineError::from);
        if matches!(result, Err(EngineError::StorageUncertain)) {
            self.uncertain = true;
        }
        result
    }
}
fn serialize(
    batch: &mail::InboxBatch<'_>,
    id: u64,
    output: &mut impl InboxOutput,
) -> mail::Result<InboxSummary> {
    let mut writer = Budget {
        inner: output,
        bytes: 0,
    };
    let result = (|| -> mail::Result<()> {
        write!(writer, "{{\"id\":{id},\"result\":{{\"status\":\"ok\",\"value\":{{\"kind\":\"inbox\",\"messages\":[")
            .map_err(|_| mail::MailError::Output)?;
        let mut first = true;
        batch.for_each(|read| {
            if !first {
                writer
                    .write_all(b",")
                    .map_err(|_| mail::MailError::Output)?;
            }
            first = false;
            let message = Mail::from_message(&read.message, read.attempt.as_ref())?;
            serde_json::to_writer(&mut writer, &message).map_err(|_| mail::MailError::Output)
        })?;
        writer
            .write_all(b"],\"text\":\"")
            .map_err(|_| mail::MailError::Output)?;
        let mut first = true;
        batch.for_each(|read| {
            let body = read.message.display_body()?;
            let body = std::str::from_utf8(&body).map_err(|_| mail::MailError::InvalidPayload)?;
            if !first {
                fragment(&mut writer, "\n\n")?;
            }
            first = false;
            fragment(&mut writer, &read.message.sender)?;
            fragment(&mut writer, ":\n")?;
            fragment(&mut writer, body)
        })?;
        writer
            .write_all(b"\"}}}\n")
            .map_err(|_| mail::MailError::Output)?;
        writer.flush().map_err(|_| mail::MailError::Output)?;
        writer.inner.seal().map_err(|_| mail::MailError::Output)?;
        Ok(())
    })();
    result?;
    Ok(InboxSummary {
        messages: batch.count,
        bytes: writer.bytes as u64,
    })
}
struct Budget<'a, W> {
    inner: &'a mut W,
    bytes: usize,
}
impl<W: Write> Write for Budget<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_INBOX_REPLY_BYTES.saturating_sub(self.bytes) {
            return Err(io::Error::other("inconsistent inbox budget"));
        }
        let count = self.inner.write(bytes)?;
        self.bytes += count;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
/// Strip only serde's surrounding quotes, retaining its tested JSON escaping.
/// A one-byte delay lets this work without assumptions about write boundaries.
struct Fragment<'a, W> {
    inner: &'a mut W,
    started: bool,
    last: Option<u8>,
}
impl<W: Write> Write for Fragment<'_, W> {
    fn write(&mut self, mut bytes: &[u8]) -> io::Result<usize> {
        let length = bytes.len();
        if bytes.is_empty() {
            return Ok(0);
        }
        if !self.started {
            if bytes[0] != b'"' {
                return Err(io::Error::other("invalid JSON fragment"));
            }
            self.started = true;
            bytes = &bytes[1..];
        }
        if !bytes.is_empty() {
            if let Some(last) = self.last.take() {
                self.inner.write_all(&[last])?;
            }
            self.inner.write_all(&bytes[..bytes.len() - 1])?;
            self.last = bytes.last().copied();
        }
        Ok(length)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
fn fragment(writer: &mut impl Write, value: &str) -> mail::Result<()> {
    let mut fragment = Fragment {
        inner: writer,
        started: false,
        last: None,
    };
    serde_json::to_writer(&mut fragment, value).map_err(|_| mail::MailError::Output)?;
    if fragment.last != Some(b'"') {
        return Err(mail::MailError::Output);
    }
    Ok(())
}
