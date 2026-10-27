#[path = "../src/watch.rs"]
mod watch;

use foundry_contracts::{CreateSession, Lifecycle, Outcome, Session};
use foundry_store::Store;
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};
use tempfile::{TempDir, tempdir};
use watch::{WatchError, WatchFormat, WatchOptions, watch};

struct Fixture {
    _directory: TempDir,
    database: PathBuf,
    session: Session,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempdir().unwrap();
        let database = directory.path().join("sessions.db");
        let session = Store::open(&database)
            .unwrap()
            .create(CreateSession {
                prompt: "offline task".into(),
                directory: directory.path().display().to_string(),
                model: None,
                idempotency_key: None,
            })
            .unwrap();
        Self {
            _directory: directory,
            database,
            session,
        }
    }
}

fn options(format: WatchFormat) -> WatchOptions {
    WatchOptions {
        format,
        interval: Duration::from_millis(2),
        timeout: Some(Duration::from_secs(5)),
        ..WatchOptions::default()
    }
}
fn follow(
    fixture: &Fixture,
    config: WatchOptions,
    stdout: &mut impl Write,
) -> Result<u8, WatchError> {
    watch(
        &fixture.database,
        &fixture.session.id,
        config,
        stdout,
        &mut Vec::new(),
    )
}
fn lines(output: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(output)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

struct ObservedWriter {
    bytes: Vec<u8>,
    flushed: Sender<()>,
}
impl Write for ObservedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        let _ = self.flushed.send(());
        Ok(())
    }
}
fn received(receiver: &Receiver<()>) {
    receiver.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[test]
fn follows_concurrent_commits_and_streams_each_revision_once_before_terminal() {
    let fixture = Fixture::new();
    let (sender, receiver) = mpsc::channel();
    let mut output = ObservedWriter {
        bytes: Vec::new(),
        flushed: sender,
    };
    let database = fixture.database.clone();
    let id = fixture.session.id.clone();
    let producer = thread::spawn(move || {
        let mut store = Store::open(database).unwrap();
        let fence = store.begin_attempt(&id, 0).unwrap();
        let first = store
            .append_event(&fence, "text", 0, "text", json!({"text":"first"}))
            .unwrap();
        received(&receiver);
        // Idempotent submission is not a second delivery; a revision is.
        assert_eq!(
            store
                .append_event(&fence, "text", 0, "text", json!({"text":"first"}))
                .unwrap(),
            first
        );
        store
            .append_event(&fence, "text", 1, "text", json!({"text":"revised"}))
            .unwrap();
        received(&receiver);
        store
            .append_event(&fence, "last", 0, "text", json!({"text":"last"}))
            .unwrap();
        store
            .finish_attempt(
                &fence,
                Lifecycle::Exit,
                Some(Outcome::Success),
                Some("complete".into()),
            )
            .unwrap();
    });
    let code = follow(&fixture, options(WatchFormat::JsonLines), &mut output).unwrap();
    producer.join().unwrap();
    assert_eq!(code, 0);
    let records = lines(&output.bytes);
    assert_eq!(records.len(), 4);
    assert_eq!(
        records[..3]
            .iter()
            .map(|v| v["event"]["sequence"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(records[1]["event"]["revision"], 1);
    assert_eq!(records[3]["type"], "result");
    assert_eq!(records[3]["after_sequence"], 3);
    assert_eq!(records[3]["session"]["result"], "complete");
}

#[test]
fn remains_queued_until_a_producer_starts_and_finishes() {
    let fixture = Fixture::new();
    let database = fixture.database.clone();
    let id = fixture.session.id.clone();
    let (done, completion) = mpsc::channel();
    let follower = thread::spawn(move || {
        let mut stdout = Vec::new();
        let result = watch(
            database,
            &id,
            options(WatchFormat::Text),
            &mut stdout,
            &mut Vec::new(),
        );
        done.send((result, stdout)).unwrap();
    });
    assert!(completion.recv_timeout(Duration::from_millis(40)).is_err());
    let mut store = Store::open(&fixture.database).unwrap();
    let fence = store.begin_attempt(&fixture.session.id, 0).unwrap();
    assert!(completion.recv_timeout(Duration::from_millis(40)).is_err());
    store
        .finish_attempt(
            &fence,
            Lifecycle::Exit,
            Some(Outcome::Success),
            Some("finished".into()),
        )
        .unwrap();
    let (result, output) = completion.recv_timeout(Duration::from_secs(5)).unwrap();
    follower.join().unwrap();
    assert_eq!(result.unwrap(), 0);
    assert_eq!(String::from_utf8(output).unwrap(), "finished\n");
}

#[test]
fn drains_multiple_pages_and_honors_exclusive_cursor() {
    let fixture = Fixture::new();
    let mut store = Store::open(&fixture.database).unwrap();
    let fence = store.begin_attempt(&fixture.session.id, 0).unwrap();
    for index in 0..520 {
        store
            .append_event(
                &fence,
                &format!("event-{index}"),
                0,
                "text",
                json!({"text":index.to_string()}),
            )
            .unwrap();
    }
    store
        .finish_attempt(&fence, Lifecycle::Exit, Some(Outcome::Success), None)
        .unwrap();
    let mut config = options(WatchFormat::JsonLines);
    config.after_sequence = 10;
    let mut output = Vec::new();
    assert_eq!(follow(&fixture, config, &mut output).unwrap(), 0);
    let records = lines(&output);
    assert_eq!(records.len(), 511);
    assert_eq!(records[0]["event"]["sequence"], 11);
    assert_eq!(records[509]["event"]["sequence"], 520);
    assert_eq!(records[510]["after_sequence"], 520);
}

#[test]
fn text_progress_and_terminal_output_use_separate_sinks() {
    let fixture = Fixture::new();
    let mut store = Store::open(&fixture.database).unwrap();
    let fence = store.begin_attempt(&fixture.session.id, 0).unwrap();
    store
        .append_event(&fence, "part", 0, "text", json!({"text":"working"}))
        .unwrap();
    store
        .finish_attempt(
            &fence,
            Lifecycle::Exit,
            Some(Outcome::Success),
            Some("answer".into()),
        )
        .unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        watch(
            &fixture.database,
            &fixture.session.id,
            options(WatchFormat::Text),
            &mut stdout,
            &mut stderr
        )
        .unwrap(),
        0
    );
    assert_eq!(String::from_utf8(stdout).unwrap(), "answer\n");
    assert_eq!(String::from_utf8(stderr).unwrap(), "[1] text: working\n");
}

#[test]
fn concurrent_terminal_failures_never_return_success() {
    for (lifecycle, outcome, code) in [
        (Lifecycle::Exit, Some(Outcome::Failure), 1),
        (Lifecycle::Exit, Some(Outcome::ActionRequired), 3),
        (Lifecycle::Error, None, 2),
        (Lifecycle::Suspended, None, 130),
    ] {
        let fixture = Fixture::new();
        let database = fixture.database.clone();
        let id = fixture.session.id.clone();
        let (sender, receiver) = mpsc::channel();
        let mut output = ObservedWriter {
            bytes: Vec::new(),
            flushed: sender,
        };
        let producer = thread::spawn(move || {
            let mut store = Store::open(database).unwrap();
            let fence = store.begin_attempt(&id, 0).unwrap();
            store
                .append_event(&fence, "progress", 0, "text", json!({"text":"pending"}))
                .unwrap();
            received(&receiver);
            store
                .finish_attempt(&fence, lifecycle, outcome, Some("terminal detail".into()))
                .unwrap();
        });
        assert_eq!(
            follow(&fixture, options(WatchFormat::JsonLines), &mut output).unwrap(),
            code
        );
        producer.join().unwrap();
        let records = lines(&output.bytes);
        assert_eq!(records.last().unwrap()["exit_code"], code);
        assert_eq!(
            records.last().unwrap()["session"]["result"],
            "terminal detail"
        );
    }
}
