//! 压缩分隔行（#94）的端到端回归：
//!
//! 1. `SessionEvent::Compaction` 在日志中是独立行（compaction/summary），
//!    读回后 `derive_messages` 以检查点消息替换影子区——事件本身不被吞掉；
//! 2. 由事件派生的展示事实（`CompactionRowFacts`）取对数与 token 估算，
//!    摘要为空时退化为「压缩摘要不可用」且不可展开——UI 行只消费这些事实。

use dsh_gpui::compaction_row::CompactionRowFacts;
use dsh_llm::SessionId;
use dsh_session::{Session, SessionEvent};

fn log_with_compaction(shadowed: Vec<u64>, tokens: u64, summary: &str) -> Vec<SessionEvent> {
    let mut events = vec![
        SessionEvent::UserMessage(dsh_llm::Message::user_text("earlier question")),
        SessionEvent::UserMessage(dsh_llm::Message::user_text("later question")),
    ];
    events.push(SessionEvent::compaction(
        1,
        summary.to_string(),
        "cp-test".into(),
        0,
        shadowed,
        tokens,
        "mock".into(),
        "mock".into(),
    ));
    events
}

#[test]
fn compaction_event_survives_replay_and_shadows_the_prefix() {
    let events = log_with_compaction(vec![0, 1], 1_234, "condensed earlier turns");
    let session = Session::from_events(SessionId::new("session-compaction"), events);

    let rows: Vec<_> = session
        .entries()
        .iter()
        .filter(|e| matches!(e.event, SessionEvent::Compaction { .. }))
        .collect();
    assert_eq!(rows.len(), 1, "compaction row must stay addressable in the log");

    // 检查点替换影子区前缀，成为历史首条（影子区覆盖两条问答）
    let derived = session.derive_messages();
    assert_eq!(derived.len(), 1, "only the checkpoint remains");
    let text = match &derived[0].content[0] {
        dsh_llm::ContentBlock::Text { text } => text.clone(),
        other => panic!("checkpoint must be text, got {other:?}"),
    };
    assert!(text.contains("<compacted-summary>"));
    assert!(text.contains("condensed earlier turns"));
}

#[test]
fn compaction_replaces_the_shadowed_prefix_with_the_checkpoint() {
    let events = log_with_compaction(vec![0], 1_234, "condensed earlier turns");
    let session = Session::from_events(SessionId::new("session-compaction-prefix"), events);

    // before_seq=1：影子区 [0] 被替换，seq 1 的消息与检查点同在前缀里
    // （derive 以 before_seq 为界保留，seq > before_seq 才留下）
    let derived = session.derive_messages();
    assert_eq!(derived.len(), 1, "checkpoint replaces the shadowed prefix");
    let text = match &derived[0].content[0] {
        dsh_llm::ContentBlock::Text { text } => text.clone(),
        other => panic!("checkpoint must be text, got {other:?}"),
    };
    assert!(text.contains("<compacted-summary>"));
}

#[test]
fn compaction_row_facts_come_from_the_logged_event() {
    let mut session = Session::new(SessionId::new("session-compaction-facts"));
    session.append(SessionEvent::UserMessage(dsh_llm::Message::user_text("a")));
    session.append(SessionEvent::UserMessage(dsh_llm::Message::user_text("b")));
    session.append(SessionEvent::compaction(
        1,
        "summary body".into(),
        "cp-1".into(),
        0,
        vec![0, 1],
        4_096,
        "mock".into(),
        "mock".into(),
    ));

    let (shadowed_seqs, shadowed_token_count, summary) = session
        .entries()
        .iter()
        .find_map(|e| match &e.event {
            SessionEvent::Compaction { shadowed_seqs, shadowed_token_count, summary, .. } => {
                Some((shadowed_seqs.clone(), *shadowed_token_count, summary.clone()))
            }
            _ => None,
        })
        .expect("compaction row present");

    let facts = CompactionRowFacts {
        shadowed_items: shadowed_seqs.len() as u64,
        shadowed_tokens: shadowed_token_count,
        summary,
    };
    assert_eq!(facts.title(), "上下文已压缩");
    assert_eq!(facts.summary_text(), "已压缩 2 条历史记录（约 4096 tokens）");
    assert!(facts.expandable());
}

#[test]
fn empty_summary_degrades_to_unavailable() {
    let events = log_with_compaction(vec![0], 10, "   ");
    let session = Session::from_events(SessionId::new("session-compaction-empty"), events);
    let summary = session
        .entries()
        .iter()
        .find_map(|e| match &e.event {
            SessionEvent::Compaction { summary, .. } => Some(summary.clone()),
            _ => None,
        })
        .unwrap();
    let facts = CompactionRowFacts { shadowed_items: 1, shadowed_tokens: 10, summary };
    assert!(!facts.expandable());
    assert_eq!(facts.summary_text(), "压缩摘要不可用");
}
