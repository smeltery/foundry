use super::*;
use foundry_network::mail_store::{MAX_MAIL_BODY_BYTES, MAX_MAIL_ROWS};
use std::io::{self, Write};

#[derive(Default)]
struct Sink {
    bytes: u64,
    max_write: usize,
    sealed: bool,
}
impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes += bytes.len() as u64;
        self.max_write = self.max_write.max(bytes.len());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl InboxOutput for Sink {
    fn seal(&mut self) -> io::Result<()> {
        self.sealed = true;
        Ok(())
    }
}
#[test]
fn full_admitted_row_and_byte_quota_streams_per_row_and_multiple_scopes() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "a", &root.0);
    let recipient = mailbox_address(&auth.conversation);
    let mut raw = rusqlite::Connection::open(root.0.join("mail.sqlite3")).unwrap();
    let tx = raw.transaction().unwrap();
    let base = MAX_MAIL_BODY_BYTES / MAX_MAIL_ROWS;
    let extra = MAX_MAIL_BODY_BYTES % MAX_MAIL_ROWS;
    {
        let mut insert = tx.prepare("INSERT INTO mail(id,sender,recipient,body,reply_to,wake,notify,encoding,state) VALUES(?1,?2,?3,?4,?2,1,0,0,0)").unwrap();
        for n in 0..MAX_MAIL_ROWS {
            let id = format!("{n:05}{}", "\"\\".repeat(125));
            let body = vec![0_u8; base + usize::from(n < extra)];
            insert
                .execute(rusqlite::params![id, "\"\\".repeat(128), recipient, body])
                .unwrap();
        }
    }
    tx.commit().unwrap();
    assert_eq!(
        raw.query_row("SELECT sum(length(body)) FROM mail", [], |r| r
            .get::<_, usize>(0))
            .unwrap(),
        MAX_MAIL_BODY_BYTES
    );
    assert_eq!(MAX_INBOX_REPLY_BYTES, 574_052_696);
    for connection in 1..=2 {
        if connection == 2 {
            ok(&mut engine, 2, Request::Attach { auth: auth.clone() });
        }
        let mut sink = Sink::default();
        let summary = engine
            .inbox_to_output(ConnectionId(connection), &auth, u64::MAX, true, &mut sink)
            .unwrap()
            .unwrap();
        assert_eq!(summary.messages, MAX_MAIL_ROWS);
        assert_eq!(summary.bytes, sink.bytes);
        assert!(sink.sealed);
        assert!(summary.bytes <= MAX_INBOX_REPLY_BYTES as u64);
        assert!(summary.bytes > 64 * 1024 * 1024);
        assert!(sink.max_write <= 256 * 1024);
    }
    assert_eq!(
        raw.query_row("SELECT count(*) FROM poll_attempt", [], |r| r
            .get::<_, usize>(0))
            .unwrap(),
        2 * MAX_MAIL_ROWS
    );
    engine.disconnect(ConnectionId(1)).unwrap();
    assert_eq!(
        raw.query_row("SELECT count(*) FROM poll_attempt", [], |r| r
            .get::<_, usize>(0))
            .unwrap(),
        MAX_MAIL_ROWS
    );
    let mut sink = Sink::default();
    engine
        .inbox_to_output(ConnectionId(2), &auth, 99, false, &mut sink)
        .unwrap()
        .unwrap();
    assert_eq!(unread(&mut engine, 2, &auth), 0);
    assert_eq!(
        raw.query_row("SELECT count(*) FROM poll_attempt", [], |r| r
            .get::<_, usize>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        raw.query_row("SELECT count(*) FROM mail", [], |r| r.get::<_, usize>(0))
            .unwrap(),
        MAX_MAIL_ROWS
    );
}
