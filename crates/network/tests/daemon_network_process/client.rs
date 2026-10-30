use super::{DEADLINE, config::Config};
use foundry_network::ipc_codec::{read_reply, write_record};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    net::UnixStream,
};

pub struct Client {
    stream: BufReader<UnixStream>,
    next: u64,
}
impl Client {
    pub async fn connect(config: &Config) -> Self {
        Self {
            stream: BufReader::new(
                UnixStream::connect(config.socket.join("daemon.sock"))
                    .await
                    .unwrap(),
            ),
            next: 1,
        }
    }
    pub async fn send(&mut self, mut value: Value) -> u64 {
        let id = self.next;
        self.next += 1;
        value["id"] = json!(id);
        write_record(self.stream.get_mut(), &value, DEADLINE)
            .await
            .unwrap();
        id
    }
    pub async fn receive(&mut self) -> Option<Value> {
        read_reply(&mut self.stream, DEADLINE).await.unwrap()
    }
    pub async fn request(&mut self, value: Value) -> Value {
        let id = self.send(value).await;
        let reply = self
            .receive()
            .await
            .expect("daemon closed before correlated reply");
        assert_eq!(reply["id"], id);
        reply
    }
    pub async fn ok(&mut self, value: Value) -> Value {
        let operation = match value["op"].as_str() {
            Some("network") => "network",
            Some("register") => "register",
            Some("attach") => "attach",
            Some("who") => "who",
            Some("send") => "send",
            Some("inbox") => "inbox",
            Some("unread") => "unread",
            Some("shutdown") => "shutdown",
            _ => "unknown",
        };
        let command = match value["command"]["kind"].as_str() {
            Some("status") => "status",
            Some("form") => "form",
            Some("invite") => "invite",
            Some("join") => "join",
            Some("join_code") => "join_code",
            Some("connect") => "connect",
            Some("remove") => "remove",
            Some("rotate") => "rotate",
            Some("depart") => "depart",
            Some("code_publications") => "code_publications",
            _ => "none",
        };
        let reply = self.request(value).await;
        let error = match reply["result"]["value"].as_str() {
            Some("disabled") => "disabled",
            Some("unauthorized") => "unauthorized",
            Some("invalid") => "invalid",
            Some("busy") => "busy",
            Some("storage") => "storage",
            Some("transport") => "transport",
            Some("rejected") => "rejected",
            Some("invalid_input") => "invalid_input",
            Some("invalid_payload") => "invalid_payload",
            Some("limit") => "limit",
            Some("storage_uncertain") => "storage_uncertain",
            Some("not_found") => "not_found",
            Some("ambiguous") => "ambiguous",
            _ => "unrecognized_error",
        };
        assert!(
            reply["result"]["status"] == "ok",
            "daemon request failed: op={operation} command={command} error={error}"
        );
        reply["result"]["value"].clone()
    }
    pub async fn control(&mut self, auth: &Value, command: Value) -> Value {
        self.ok(json!({"op":"network","auth":auth,"command":command}))
            .await
    }
    pub async fn register(&mut self, config: &Config) -> Value {
        self.ok(json!({"op":"register","name":config.name,"repository":config.repository}))
            .await["auth"]
            .clone()
    }
    pub async fn attach(&mut self, auth: &Value) {
        self.ok(json!({"op":"attach","auth":auth})).await;
    }
    pub async fn status(&mut self, auth: &Value) -> Value {
        let value = self.control(auth, json!({"kind":"status"})).await;
        assert_eq!(value["kind"], "network_status");
        assert!(value["status"].is_object(), "network status contract");
        value["status"].clone()
    }
    pub async fn shutdown_write(&mut self) {
        self.stream.get_mut().shutdown().await.unwrap();
    }
}
