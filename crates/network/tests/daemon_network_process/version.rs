use super::{child::Node, config::Root, flow::*};
use serde_json::json;

fn warned(stderr: &[u8], expected: u32, observed: u32) -> bool {
    let line =
        format!("network protocol version mismatch: expected {expected}, observed {observed}");
    stderr
        .windows(line.len())
        .any(|part| part == line.as_bytes())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mismatched_protocol_versions_warn_while_join_presence_and_mail_work() {
    let root = Root::new();
    let a = Node::start(root.config("a", "alex")).await;
    let (mut ca, aa) = a.client().await;
    let status = form(&mut ca, &aa).await;
    let invitation = invite(&mut ca, &aa, false).await;
    let mut config = root.config("b", "blair");
    // Only the numeric Hello version differs; ALPNs and signing domains match.
    config.value["network"]["profile"]["protocol"] = json!(20);
    seed(&mut config, &invitation.endpoint);
    let b = Node::start(config).await;
    let (mut cb, ab) = b.client().await;
    join(&mut cb, &ab, &invitation).await;
    assert_eq!(cb.status(&ab).await["team"], status["team"]);
    wait_remote(&mut ca, &b.config, true).await;
    wait_remote(&mut cb, &a.config, true).await;
    send(&mut ca, &aa, &b.config, "older-to-newer", "across versions").await;
    peek(&mut cb, &ab, &[("older-to-newer", "across versions")]).await;
    send(&mut cb, &ab, &a.config, "newer-to-older", "and back again").await;
    peek(&mut ca, &aa, &[("newer-to-older", "and back again")]).await;
    consume(&mut ca, &aa, &[("newer-to-older", "and back again")]).await;
    consume(&mut cb, &ab, &[("older-to-newer", "across versions")]).await;
    let newer = b.stop_logged(&mut cb, &ab).await;
    let older = a.stop_logged(&mut ca, &aa).await;
    assert!(
        warned(&newer, 20, 19),
        "newer daemon did not log the mismatch"
    );
    assert!(
        warned(&older, 19, 20),
        "older daemon did not log the mismatch"
    );
}
