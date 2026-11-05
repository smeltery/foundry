//! Adapter fixture compiled by the entrypoint watch test: it holds its attempt
//! running until the test creates `release`.
use std::{
    io::{self, Write},
    time::Duration,
};
fn main() {
    println!("{{\"type\":\"ready\",\"version\":1}}");
    io::stdout().flush().unwrap();
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    let attempt = input.split("\"attempt_id\":\"").nth(1).unwrap().split('"').next().unwrap();
    let id = input.split("\"id\":\"").nth(1).unwrap().split('"').next().unwrap();
    println!("{{\"type\":\"text\",\"attempt_id\":\"{attempt}\",\"text\":\"progress\"}}");
    io::stdout().flush().unwrap();
    // Publish by rename: a polling reader must never see an empty id.
    std::fs::write("session-id.partial", id).unwrap();
    std::fs::rename("session-id.partial", "session-id").unwrap();
    // Self-release after 30s, well past the test's 15s deadline.
    for _ in 0..3000 {
        if std::path::Path::new("release").exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    println!(
        "{{\"type\":\"settled\",\"attempt_id\":\"{attempt}\",\"outcome\":\"success\",\"result\":\"done\"}}"
    );
}
