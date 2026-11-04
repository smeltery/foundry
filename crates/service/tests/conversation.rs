#![cfg(unix)]
use foundry_contracts::{CreateSession, MessageState, Outcome};
use foundry_runtime::AdapterSpec;
use foundry_service::{
    Service, ServiceOptions,
    conversation::{ConversationContent, ConversationEntry},
};
use foundry_store::Store;
use serde_json::json;
use std::{path::Path, process::Command, time::Duration};
use tempfile::tempdir;

fn setup(path: &Path, directory: &Path) -> (Service, String) {
    let options = ServiceOptions {
        lease_duration: Duration::from_secs(5),
        poll_interval: Duration::from_millis(10),
    };
    let mut service = Service::open_with_options(path, options).unwrap();
    let session = service
        .create(CreateSession {
            prompt: "opening prompt".into(),
            directory: directory.to_string_lossy().into(),
            model: None,
            idempotency_key: None,
        })
        .unwrap();
    (service, session.id)
}

fn ack_adapter(marker: &Path) -> AdapterSpec {
    let output = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let mut spec = AdapterSpec::new(String::from_utf8(output.stdout).unwrap().trim());
    spec.args = ["-u", "-c", include_str!("message_adapter.py"), "ack"]
        .into_iter()
        .map(Into::into)
        .collect();
    spec.args.push(marker.as_os_str().into());
    spec.run_timeout = Duration::from_secs(5);
    spec.cancel_timeout = Duration::from_millis(50);
    spec
}

fn prompt(text: &str) -> ConversationEntry {
    ConversationEntry {
        position: 0,
        content: ConversationContent::Prompt { text: text.into() },
    }
}

#[test]
fn executed_session_projects_prompt_follow_up_and_result_across_restart() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let (mut service, id) = setup(&path, directory.path());
    service.enqueue_message(&id, "m", "follow-up").unwrap();
    service.enqueue_message(&id, "w", "withdrawn").unwrap();
    service.withdraw_message(&id, "w").unwrap();
    let marker = directory.path().join("ready");
    service.run(&id, 0, &ack_adapter(&marker)).unwrap();
    let full = service.conversation(&id, None, 10).unwrap();
    assert!(!full.has_more);
    let attempt = service.messages(&id, 0, 1).unwrap()[0].attempt_id.clone();
    let events = service.events(&id, 0, 100).unwrap();
    let event = |kind: &str| events.iter().find(|e| e.kind == kind).unwrap();
    assert!(events.iter().any(|event| event.kind == "adapter_ready"));
    assert_eq!(
        full.entries,
        vec![
            prompt("opening prompt"),
            ConversationEntry {
                position: event("message_accepted").sequence,
                content: ConversationContent::FollowUp {
                    message_id: "m".into(),
                    attempt_id: attempt.clone(),
                    text: "follow-up".into(),
                },
            },
            ConversationEntry {
                position: event("adapter_settled").sequence,
                content: ConversationContent::Result {
                    event_id: event("adapter_settled").id.clone(),
                    attempt_id: attempt.unwrap(),
                    outcome: Outcome::Success,
                    text: "fixture finished".into(),
                },
            },
        ]
    );
    assert_eq!(full.end_cursor, Some(full.entries[2].position));
    let first = service.conversation(&id, None, 1).unwrap();
    assert_eq!(
        (first.entries.clone(), first.has_more),
        (vec![prompt("opening prompt")], true)
    );
    assert_eq!(first.end_cursor, Some(0));
    drop(service);
    let service = Service::open(&path).unwrap();
    let second = service.conversation(&id, first.end_cursor, 1).unwrap();
    assert_eq!(second.entries, full.entries[1..2]);
    assert!(second.has_more);
    let third = service.conversation(&id, second.end_cursor, 5).unwrap();
    assert_eq!(
        (third.entries.as_slice(), third.has_more),
        (&full.entries[2..], false)
    );
    let empty = service.conversation(&id, third.end_cursor, 5).unwrap();
    assert!(empty.entries.is_empty() && !empty.has_more);
    assert_eq!(empty.end_cursor, third.end_cursor);
    assert!(service.conversation(&id, None, u32::MAX).is_err());
    assert!(service.conversation("missing", None, 1).is_err());
}

#[test]
fn message_page_excludes_conversation_and_continues_from_end_cursor() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let (mut service, id) = setup(&path, directory.path());
    for (message, text) in [("a", "one"), ("b", "two"), ("c", "three")] {
        service.enqueue_message(&id, message, text).unwrap();
    }
    service.withdraw_message(&id, "b").unwrap();
    let first = service.message_page(&id, None, 2).unwrap();
    let ids: Vec<_> = first.messages.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        (ids, first.end_cursor, first.has_more),
        (vec!["a", "b"], Some(2), true)
    );
    assert_eq!(first.messages[1].state, MessageState::Withdrawn);
    drop(service);
    let service = Service::open(&path).unwrap();
    let rest = service.message_page(&id, first.end_cursor, 2).unwrap();
    assert_eq!(rest.messages, service.messages(&id, 2, 10).unwrap());
    assert_eq!((rest.end_cursor, rest.has_more), (Some(3), false));
    let empty = service.message_page(&id, rest.end_cursor, 2).unwrap();
    assert_eq!((empty.messages.len(), empty.end_cursor), (0, Some(3)));
    let none = service.message_page(&id, None, 0).unwrap();
    assert_eq!(
        (none.messages.len(), none.end_cursor, none.has_more),
        (0, None, true)
    );
}

#[test]
fn direct_revisions_project_latest_reply_and_reject_malformed_content() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let (service, id) = setup(&path, directory.path());
    drop(service);
    let mut store = Store::open(&path).unwrap();
    let fence = store.begin_attempt(&id, 0).unwrap();
    let reply = |text: &str| json!({"type":"text","attempt_id":fence.attempt_id,"text":text});
    store
        .append_event(&fence, "r", 0, "text", reply("draft"))
        .unwrap();
    store
        .append_event(
            &fence,
            "tool",
            0,
            "tool_request",
            json!({"type":"tool_call"}),
        )
        .unwrap();
    store
        .append_event(&fence, "r", 3, "text", reply("final"))
        .unwrap();
    let service = Service::open(&path).unwrap();
    let page = service.conversation(&id, Some(0), 10).unwrap();
    let expected = ConversationEntry {
        position: 1,
        content: ConversationContent::Reply {
            event_id: "r".into(),
            attempt_id: fence.attempt_id.clone(),
            revision: 3,
            text: "final".into(),
        },
    };
    assert_eq!((page.entries, page.end_cursor), (vec![expected], Some(1)));
    store
        .append_event(&fence, "bad", 0, "text", json!("not an adapter event"))
        .unwrap();
    assert!(service.conversation(&id, None, 10).is_err());
}
