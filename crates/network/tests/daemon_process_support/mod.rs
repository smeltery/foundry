use foundry_network::ipc_codec::{read_reply, write_record};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::UnixStream,
    process::{Child, Command},
};
pub(crate) const DEADLINE: Duration = Duration::from_secs(10);

pub(crate) struct State(pub(crate) PathBuf);
impl State {
    pub(crate) fn new() -> Self {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path = PathBuf::from("/tmp").join(format!("f5yd-{:016x}", u64::from_le_bytes(nonce)));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let path = path.canonicalize().unwrap();
        let config = path.join("config.json");
        fs::write(&config, serde_json::to_vec(&json!({"state_directory":path,"socket_directory":path,"person":"alice","machine":"laptop","network":"disabled","folders":[]})).unwrap()).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        Self(path)
    }
    pub(crate) async fn start(&self) -> Child {
        let mut child = Command::new(env!("CARGO_BIN_EXE_f5y-daemon"))
            .arg("--config")
            .arg(self.0.join("config.json"))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut line = String::new();
        tokio::time::timeout(
            DEADLINE,
            BufReader::new(child.stdout.take().unwrap()).read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["event"], "ready");
        assert_eq!(
            ready["socket"],
            self.0.join("daemon.sock").to_str().unwrap()
        );
        child
    }
    pub(crate) async fn connect(&self) -> Client {
        Client(BufReader::new(
            UnixStream::connect(self.0.join("daemon.sock"))
                .await
                .unwrap(),
        ))
    }
}
impl Drop for State {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
pub(crate) struct Client(pub(crate) BufReader<UnixStream>);
impl Client {
    pub(crate) async fn request(&mut self, value: Value) -> Value {
        self.send(value.clone()).await;
        let reply = self.receive().await;
        assert_eq!(reply["id"], value["id"]);
        reply
    }
    pub(crate) async fn register(&mut self, name: &str, repository: &Path) -> Value {
        let reply = self
            .request(json!({"id":1,"op":"register","name":name,"repository":repository}))
            .await;
        assert_eq!(reply["result"]["status"], "ok");
        reply["result"]["value"]["auth"].clone()
    }
}

impl Client {
    pub(crate) async fn send(&mut self, value: Value) {
        write_record(self.0.get_mut(), &value, DEADLINE)
            .await
            .unwrap();
    }
    pub(crate) async fn receive(&mut self) -> Value {
        read_reply(&mut self.0, DEADLINE).await.unwrap().unwrap()
    }
}
pub(crate) async fn exited(child: &mut Child) {
    assert!(
        tokio::time::timeout(DEADLINE, child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}
