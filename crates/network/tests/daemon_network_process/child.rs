use super::{DEADLINE, client::Client, config::Config};
use foundry_network::ipc_codec::read_record;
use serde_json::{Value, json};
use std::process::Stdio;
use tokio::{
    io::{AsyncRead, AsyncReadExt, BufReader},
    process::{Child, Command},
    task::JoinHandle,
};

pub struct Node {
    pub config: Config,
    child: Child,
    output: JoinHandle<Vec<u8>>,
    error: JoinHandle<Vec<u8>>,
    ready: Value,
}
fn command(config: &Config) -> Command {
    let executable = std::env::var_os("FOUNDRY_TEST_DAEMON_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_f5y-daemon").into());
    let mut command = Command::new(executable);
    command.env_remove("FOUNDRY_TEST_DAEMON_BINARY");
    command
        .arg("--config")
        .arg(&config.path)
        .current_dir(&config.cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    super::clock::clear(command.as_std_mut());
    if let Some(clock) = &config.clock {
        clock.apply(command.as_std_mut());
    }
    command
}
fn capture(reader: impl AsyncRead + Unpin + Send + 'static) -> JoinHandle<Vec<u8>> {
    tokio::spawn(async move {
        let mut bytes = Vec::new();
        reader.take(65537).read_to_end(&mut bytes).await.unwrap();
        assert!(bytes.len() <= 65536, "fixture log ceiling exceeded");
        bytes
    })
}
impl Node {
    pub fn diagnostic_pid(&self) -> Option<u32> {
        self.child.id()
    }
    pub fn diagnostic_running(&mut self) -> bool {
        self.child
            .try_wait()
            .expect("diagnostic child status")
            .is_none()
    }
    pub async fn start(config: Config) -> Self {
        let mut node = Self::start_or_rejected(config)
            .await
            .expect("daemon exited before readiness");
        if std::env::var_os("FOUNDRY_ACK_DIAGNOSTIC").is_some() {
            eprintln!(
                "fixture child pid={:?} running={}",
                node.diagnostic_pid(),
                node.diagnostic_running()
            );
        }
        node
    }
    pub async fn start_or_rejected(config: Config) -> Option<Self> {
        config.save();
        let mut child = command(&config).spawn().unwrap();
        let error = capture(child.stderr.take().unwrap());
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let ready: Option<Value> = read_record(&mut output, DEADLINE).await.unwrap();
        let Some(ready) = ready else {
            assert!(
                !tokio::time::timeout(DEADLINE, child.wait())
                    .await
                    .unwrap()
                    .unwrap()
                    .success()
            );
            error.await.unwrap();
            return None;
        };
        assert_eq!(ready["event"], "ready");
        assert_eq!(
            ready["socket"],
            config.socket.join("daemon.sock").to_str().unwrap()
        );
        let output = capture(output);
        Some(Self {
            config,
            child,
            output,
            error,
            ready,
        })
    }
    pub async fn client(&self) -> (Client, Value) {
        let mut client = Client::connect(&self.config).await;
        let auth = client.register(&self.config).await;
        (client, auth)
    }
    pub async fn stop(mut self, client: &mut Client, auth: &Value, secrets: &[&str]) {
        if let Some(status) = self.child.try_wait().unwrap() {
            let bytes = (&mut self.error).await.unwrap();
            let category = if bytes
                .windows(b"invalid private fixture clock input".len())
                .any(|part| part == b"invalid private fixture clock input")
            {
                "fixture_clock_input"
            } else if bytes
                .windows(b"storage".len())
                .any(|part| part == b"storage")
            {
                "storage"
            } else if bytes.windows(b"worker".len()).any(|part| part == b"worker") {
                "worker"
            } else {
                "unclassified"
            };
            panic!("daemon exited before shutdown: status={status} category={category}");
        }
        client.ok(json!({"op":"shutdown","auth":auth})).await;
        assert!(
            tokio::time::timeout(DEADLINE, self.child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        self.check_logs(secrets).await;
    }
    pub async fn kill(mut self) -> Config {
        self.child.kill().await.unwrap();
        self.child.wait().await.unwrap();
        self.check_logs(&[]).await;
        self.config
    }
    async fn check_logs(&mut self, secrets: &[&str]) {
        let stdout = (&mut self.output).await.unwrap();
        let stderr = (&mut self.error).await.unwrap();
        let ready = serde_json::to_vec(&self.ready).unwrap();
        for secret in secrets {
            assert!(!secret.is_empty());
            for bytes in [&stdout, &stderr, &ready] {
                assert!(
                    !bytes
                        .windows(secret.len())
                        .any(|part| part == secret.as_bytes()),
                    "secret leaked through daemon output"
                );
            }
        }
    }
}
pub async fn fails_to_start(config: &Config) {
    if let Some(node) = Node::start_or_rejected(config.clone()).await {
        node.kill().await;
        panic!("invalid startup unexpectedly reached readiness");
    }
}
