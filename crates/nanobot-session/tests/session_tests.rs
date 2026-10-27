//! Integration tests for ocad-session: Session append, derive_messages, persistence.

use nanobot_session::{ProjectedMessage, Session, SessionEvent, TurnEndReason};

#[test]
fn append_assigns_monotonic_seq() {
    let s = Session::new("test");
    let seq1 = s.append(SessionEvent::TurnStart { turn: 0 });
    let seq2 = s.append(SessionEvent::TurnEnd {
        turn: 0,
        reason: TurnEndReason::Completed,
    });
    assert_eq!(seq1, 0);
    assert_eq!(seq2, 1);
}

#[test]
fn derive_messages_projects_model_visible() {
    let s = Session::new("test");
    s.append(SessionEvent::UserMessage {
        content: "hello".into(),
    });
    s.append(SessionEvent::AssistantMessage {
        content: "hi".into(),
    });
    s.append(SessionEvent::PlanRejected {
        plan_id: "p1".into(),
        reason: "bad".into(),
    });
    // Not model-visible:
    s.append(SessionEvent::StepStart { turn: 0, step: 0 });
    s.append(SessionEvent::CheckRun {
        check_id: "c1".into(),
    });

    let messages = s.derive_messages();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0], ProjectedMessage::User("hello".into()));
    assert_eq!(messages[1], ProjectedMessage::Assistant("hi".into()));
    assert_eq!(
        messages[2],
        ProjectedMessage::System("Plan rejected: bad".into())
    );
}

#[test]
fn truncate_to_rewinds_the_log_and_resumes_numbering() {
    let s = Session::new("truncate_test");
    s.append(SessionEvent::TurnStart { turn: 0 }); // seq 0
    s.append(SessionEvent::UserMessage {
        content: "keep me".into(),
    }); // seq 1
    s.append(SessionEvent::ToolCall {
        id: "c1".into(),
        name: "add_feature".into(),
        arguments: serde_json::json!({}),
        seq: 0,
    }); // seq 2
    s.append(SessionEvent::ToolResult {
        id: "c1".into(),
        success: true,
        output: "{}".into(),
    }); // seq 3

    s.truncate_to(1);

    // Only the events up to and including seq 1 survive.
    let entries = s.events();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries.last().unwrap().seq, 1);

    // Numbering resumes right after the rewind point, never reusing a seq.
    let next = s.append(SessionEvent::AssistantMessage {
        content: "after rewind".into(),
    });
    assert_eq!(next, 2);

    // The rewound tool call is gone from the model's projected history.
    let messages = s.derive_messages();
    assert!(
        messages
            .iter()
            .all(|m| !matches!(m, ProjectedMessage::ToolCall { .. })),
        "the rolled-back tool call must leave the projection"
    );
}

#[test]
fn truncate_past_the_end_is_a_noop() {
    let s = Session::new("truncate_noop");
    s.append(SessionEvent::TurnStart { turn: 0 });
    s.truncate_to(99);
    assert_eq!(s.len(), 1);
}

#[test]
fn persist_to_writes_the_log_as_it_grows() {
    let path = std::env::temp_dir().join("ocad_persist_to_test.jsonl");
    std::fs::remove_file(&path).ok();

    let session = Session::new("persist_to");
    let _guard = session.persist_to(&path);

    assert!(
        path.exists(),
        "the session has a file from the moment it is bound"
    );

    session.append(SessionEvent::TurnStart { turn: 0 });
    session.append(SessionEvent::UserMessage {
        content: "hello".into(),
    });

    let loaded = nanobot_session::load(&path).expect("the log is readable");
    assert_eq!(loaded.len(), 2, "every append must reach the file");
    match &loaded[1].event {
        SessionEvent::UserMessage { content, .. } => assert_eq!(content, "hello"),
        other => panic!("unexpected event: {other:?}"),
    }

    std::fs::remove_file(&path).ok();
}

#[test]
fn a_rewind_reaches_the_file_on_the_next_append() {
    // The plan-reject path: rewind the log, then append the rejection. The
    // file must not keep the rolled-back work.
    let path = std::env::temp_dir().join("ocad_persist_rewind_test.jsonl");
    std::fs::remove_file(&path).ok();

    let session = Session::new("rewind");
    let _guard = session.persist_to(&path);

    session.append(SessionEvent::TurnStart { turn: 0 });
    session.append(SessionEvent::UserMessage {
        content: "make a plate".into(),
    });
    session.append(SessionEvent::ToolCall {
        id: "call_1".into(),
        name: "add_feature".into(),
        arguments: serde_json::json!({}),
        seq: 0,
    });

    session.truncate_to(1);
    session.append(SessionEvent::PlanRejected {
        plan_id: "p1".into(),
        reason: "not what I asked for".into(),
    });

    let loaded = nanobot_session::load(&path).expect("the log is readable");
    assert!(
        !loaded
            .iter()
            .any(|e| matches!(e.event, SessionEvent::ToolCall { .. })),
        "the rolled-back tool call must leave the file"
    );
    assert!(loaded
        .iter()
        .any(|e| matches!(e.event, SessionEvent::PlanRejected { .. })));

    std::fs::remove_file(&path).ok();
}

#[test]
fn audit_to_records_every_tool_call_with_its_outcome() {
    // The audit trail is the record of what was attempted: a call that failed
    // (or was refused) must be in it even though the document never moved.
    let path = std::env::temp_dir().join("ocad_audit_test.jsonl");
    std::fs::remove_file(&path).ok();

    let session = Session::new("audit");
    let _guard = session.audit_to(&path);

    session.append(SessionEvent::ToolCall {
        id: "call_1".into(),
        name: "add_feature".into(),
        arguments: serde_json::json!({"name": "plate"}),
        seq: 0,
    });
    session.append(SessionEvent::ToolResult {
        id: "call_1".into(),
        success: true,
        output: r#"{"ok":true}"#.into(),
    });
    session.append(SessionEvent::ToolCall {
        id: "call_2".into(),
        name: "set_parameter".into(),
        arguments: serde_json::json!({"name": "nosuch"}),
        seq: 0,
    });
    session.append(SessionEvent::ToolResult {
        id: "call_2".into(),
        success: false,
        output: r#"{"ok":false}"#.into(),
    });

    let text = std::fs::read_to_string(&path).expect("the audit log exists");
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "one record per call: {text}");

    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["tool"], "add_feature");
    assert_eq!(first["success"], true);
    assert_eq!(first["arguments"]["name"], "plate");

    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second["tool"], "set_parameter");
    assert_eq!(second["success"], false, "a failed call is recorded too");

    std::fs::remove_file(&path).ok();
}

#[test]
fn persistence_roundtrip() {
    let s = Session::new("persist_test");
    s.append(SessionEvent::UserMessage {
        content: "test".into(),
    });

    let path = std::env::temp_dir().join("nanobot_session_test.jsonl");
    let entries = s.events();
    nanobot_session::save(&path, &entries).unwrap();
    let loaded = nanobot_session::load(&path).unwrap();
    assert_eq!(loaded.len(), 1);
    std::fs::remove_file(&path).ok();
}

#[test]
fn revert_records_the_rollback_on_the_log() {
    let s = Session::new("revert");
    s.append(SessionEvent::TurnStart { turn: 0 }); // seq 0
    s.append(SessionEvent::UserMessage {
        content: "keep".into(),
    }); // seq 1
    s.append(SessionEvent::ToolCall {
        id: "c1".into(),
        name: "x".into(),
        arguments: serde_json::json!({}),
        seq: 0,
    }); // seq 2

    let reverted = s.revert_to(1);
    assert_eq!(reverted, 1);
    let events = s.events();
    assert_eq!(events.len(), 3, "prefix + the revert marker: {events:?}");
    assert!(
        matches!(
            events.last().unwrap().event,
            SessionEvent::SessionReverted {
                to_seq: 1,
                reverted: 1
            }
        ),
        "the revert is on the record: {:?}",
        events.last().unwrap().event
    );
    // The rolled-back tool call leaves the projected model history too.
    assert!(
        s.derive_messages()
            .iter()
            .all(|m| !matches!(m, ProjectedMessage::ToolCall { .. })),
        "a reverted tool call must not stay in the model's history"
    );
}

#[test]
fn revert_past_the_end_is_a_noop() {
    let s = Session::new("revert_noop");
    s.append(SessionEvent::TurnStart { turn: 0 });
    assert_eq!(s.revert_to(99), 0);
    assert_eq!(s.len(), 1);
}
