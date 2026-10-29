use serde_json::{Value, json};
use std::path::Path;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

pub(super) use crate::support::{DEADLINE, State, exited};

// A fixture ceiling, independent of the daemon's request or response policy.
const RESPONSE_CEILING: usize = 8 * 1024 * 1024;
pub(super) const REQUEST_CEILING: usize = 1024 * 1024;

pub(super) struct Client {
    stream: BufReader<UnixStream>,
    next_id: u64,
    pub(super) last_record_bytes: usize,
}

impl Client {
    pub(super) async fn connect(state: &State) -> Self {
        Self {
            stream: BufReader::new(
                UnixStream::connect(state.0.join("daemon.sock"))
                    .await
                    .unwrap(),
            ),
            next_id: 1,
            last_record_bytes: 0,
        }
    }

    pub(super) async fn request(&mut self, mut request: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        request["id"] = json!(id);
        let mut bytes = serde_json::to_vec(&request).unwrap();
        bytes.push(b'\n');
        assert!(bytes.len() <= REQUEST_CEILING);
        tokio::time::timeout(DEADLINE, self.stream.get_mut().write_all(&bytes))
            .await
            .unwrap()
            .unwrap();
        let (reply, length) = read_response(&mut self.stream).await;
        self.last_record_bytes = length;
        assert_eq!(reply["id"], id, "reply correlation: {reply}");
        reply
    }

    pub(super) async fn ok(&mut self, request: Value) -> Value {
        let reply = self.request(request).await;
        assert_eq!(reply["result"]["status"], "ok", "daemon reply: {reply}");
        reply["result"]["value"].clone()
    }

    pub(super) async fn register(&mut self, name: &str, root: &Path) -> Value {
        self.ok(json!({"op":"register","name":name,"repository":root}))
            .await["auth"]
            .clone()
    }

    pub(super) async fn unread(&mut self, auth: &Value, expected: usize) {
        let result = self.ok(json!({"op":"unread","auth":auth})).await;
        assert_eq!(result["count"], expected);
    }

    pub(super) async fn enqueue(&mut self, auth: &Value, count: usize, body: &str) {
        for index in 0..count {
            let result = self
                .ok(json!({"op":"send","auth":auth,
                "message_id":format!("ordered-{index:04}"),
                "to":{"name":"reader"},"body":body}))
                .await;
            assert_eq!(result["duplicate"], false);
        }
    }
}

async fn read_response(stream: &mut BufReader<UnixStream>) -> (Value, usize) {
    tokio::time::timeout(DEADLINE, async {
        let mut record = Vec::new();
        loop {
            let available = stream.fill_buf().await.expect("response read failed");
            assert!(!available.is_empty(), "EOF before complete JSON record");
            let count = available
                .iter()
                .position(|b| *b == b'\n')
                .map_or(available.len(), |position| position + 1);
            assert!(
                record.len() + count <= RESPONSE_CEILING,
                "fixture response ceiling exceeded"
            );
            let complete = available[count - 1] == b'\n';
            record.extend_from_slice(&available[..count]);
            stream.consume(count);
            if complete {
                let length = record.len();
                // Parse the entire record: concatenated JSON and truncated prefixes fail.
                return (
                    serde_json::from_slice(&record).expect("one complete JSON record"),
                    length,
                );
            }
        }
    })
    .await
    .expect("response deadline")
}

pub(super) fn check_messages(value: &Value, indices: &[usize], body: &str, peek: bool) -> Vec<i64> {
    assert_eq!(value["kind"], "inbox");
    let messages = value["messages"].as_array().unwrap();
    assert_eq!(messages.len(), indices.len(), "complete inbox required");
    let mut attempts = Vec::new();
    for (message, index) in messages.iter().zip(indices) {
        assert_eq!(message["message_id"], format!("ordered-{index:04}"));
        assert_eq!(
            data_encoding::BASE64
                .decode(message["body_base64"].as_str().unwrap().as_bytes())
                .unwrap(),
            body.as_bytes()
        );
        assert_eq!(message["state"], if peek { "pending" } else { "consumed" });
        if peek {
            attempts.push(
                message["attempt_id"]
                    .as_i64()
                    .expect("individual ACK token"),
            );
        } else {
            assert!(message["attempt_id"].is_null());
        }
    }
    if indices.is_empty() {
        assert_eq!(value["text"], "");
    } else {
        assert_eq!(
            value["text"].as_str().unwrap().matches(body).count(),
            indices.len()
        );
    }
    let unique: std::collections::BTreeSet<_> = attempts.iter().collect();
    assert_eq!(unique.len(), attempts.len(), "ACK tokens must be distinct");
    attempts
}

pub(super) async fn reject_oversized_request(state: &State, auth: &Value) {
    let mut client = Client::connect(state).await;
    client.ok(json!({"op":"attach","auth":auth})).await;
    let (input, mut output) = client.stream.into_inner().into_split();
    // Valid JSON whitespace grows only the frame, not the individual text.
    // Payload validation must not accidentally satisfy the framing assertion.
    let mut bytes = vec![b' '; REQUEST_CEILING];
    bytes.extend(
        serde_json::to_vec(&json!({"id":900,"op":"send","auth":auth,
        "message_id":"oversized-request","to":{"name":"reader"},
        "body":"valid small request body"}))
        .unwrap(),
    );
    bytes.push(b'\n');
    assert!(bytes.len() > REQUEST_CEILING);
    // Rejection can close the socket before the writer finishes transmitting.
    let writer = tokio::spawn(async move { output.write_all(&bytes).await });
    let mut input = BufReader::new(input);
    let reply = tokio::time::timeout(DEADLINE, async {
        let mut record = Vec::new();
        // Restrict even the rejection fixture's input before allocating it.
        loop {
            let available = input.fill_buf().await.unwrap();
            if available.is_empty() {
                return None;
            }
            let count = available
                .iter()
                .position(|b| *b == b'\n')
                .map_or(available.len(), |position| position + 1);
            assert!(record.len() + count <= 4096);
            let complete = available[count - 1] == b'\n';
            record.extend_from_slice(&available[..count]);
            input.consume(count);
            if complete {
                return Some(serde_json::from_slice::<Value>(&record).unwrap());
            }
        }
    })
    .await
    .expect("oversized request rejection deadline");
    drop(input);
    tokio::time::timeout(DEADLINE, writer)
        .await
        .unwrap()
        .unwrap()
        .ok();
    if let Some(reply) = reply {
        assert_eq!(
            reply["result"]["status"], "error",
            "oversized request accepted: {reply}"
        );
        assert_eq!(reply["result"]["value"], "invalid_input");
    }
}
