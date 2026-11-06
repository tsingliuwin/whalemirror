//! prompt 重试去重（上游 8706daa679 `fix(web): deduplicate prompt retries
//! after input acceptance`）：
//!
//! 1. 同一 `MessageId` 的重复投递只接纳一次——不重复入列、不重复唤醒；
//! 2. 接纳以「首次插入」为准：消息已被认领落盘或已从队列移除后，重试仍去重；
//! 3. 日志是收据的恢复源：换入的既有会话按 `user/message` 行重建收据；
//! 4. 收据按确切的会话缓存（上游 WeakMap<Session, …>），换会话即重置。

use async_trait::async_trait;
use dsh_agent::InboxTarget;
use dsh_agent_loop::{AgentOptions, ReactLoopAgent};
use dsh_cordis::EventBus;
use dsh_llm::{
    BoxStream, ContentBlockType, FinishReason, GenerateOptions, LlmAdapter, LlmError,
    LlmProviderInfo, LlmRuntime, Message, MessageId, SessionId, StreamChunk,
};
use dsh_session::{Session, SessionEvent};
use dsh_system_prompt::SystemPrompt;
use dsh_tools::ToolRegistry;
use futures::stream;
use std::sync::Arc;

struct EchoAdapter;

#[async_trait]
impl LlmAdapter for EchoAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "mock".into(), name: "Mock".into() }
    }

    async fn stream(&self, _options: GenerateOptions) -> Result<BoxStream, LlmError> {
        let chunks = vec![
            StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text },
            StreamChunk::TextDelta { index: 0, text: "ok".into() },
            StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
        ];
        Ok(Box::pin(stream::iter(chunks)))
    }
}

fn new_agent(id: &str) -> Arc<ReactLoopAgent> {
    let events = EventBus::new();
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let adapter = Arc::new(EchoAdapter);
    let _reg = llm
        .register_adapter(&["mock".to_string()], Arc::clone(&adapter) as Arc<dyn LlmAdapter>)
        .unwrap();
    ReactLoopAgent::new(
        SessionId::new(id),
        AgentOptions {
            provider: "mock".into(),
            model: "mock".into(),
            max_tokens: None,
            system_prompt: None,
            compaction: Default::default(),
            workdir: Default::default(),
            attachments_root: None,
        },
        llm,
        Arc::new(ToolRegistry::new()),
        Arc::new(SystemPrompt::new()),
        Arc::new(dsh_session_projection::SessionProjections::default()),
        events,
    )
}

fn user_message(id: &str, text: &str) -> Message {
    let mut message = Message::user_text(text);
    message.id = MessageId(id.into());
    message
}

/// 已落盘的该 id 用户行条数。
fn committed_user_rows(agent: &ReactLoopAgent, id: &str) -> usize {
    let session = agent.session();
    let guard = session.lock().unwrap();
    guard
        .entries()
        .iter()
        .filter(|e| matches!(&e.event, SessionEvent::UserMessage(m) if m.id.0 == id))
        .count()
}

#[tokio::test]
async fn repeated_prompt_identity_is_accepted_once() {
    let agent = new_agent("session-dedup-repeat");
    let message = user_message("req-1", "hello");

    assert!(agent.send(message.clone(), InboxTarget::NextTurn));
    assert!(!agent.send(message, InboxTarget::NextTurn));

    assert_eq!(agent.queued_next_turn().len(), 1);
    assert_eq!(committed_user_rows(&agent, "req-1"), 0);
}

#[tokio::test]
async fn distinct_prompt_identities_are_all_admitted() {
    let agent = new_agent("session-dedup-distinct");

    assert!(agent.send(user_message("req-1", "a"), InboxTarget::NextTurn));
    assert!(agent.send(user_message("req-2", "b"), InboxTarget::NextTurn));

    assert_eq!(agent.queued_next_turn().len(), 2);
}

#[tokio::test]
async fn retry_after_queue_removal_is_still_deduplicated() {
    let agent = new_agent("session-dedup-removed");
    let message = user_message("req-1", "hello");

    assert!(agent.send(message.clone(), InboxTarget::NextTurn));
    assert!(agent.remove_queued("req-1"));
    // 移除不撤销接纳（上游：收据来自首次插入，认领/移除都不撤销）。
    assert!(!agent.send(message, InboxTarget::NextTurn));
    assert!(agent.queued_next_turn().is_empty());
}

#[tokio::test]
async fn retry_after_turn_committed_is_deduplicated() {
    let agent = new_agent("session-dedup-committed");
    agent.spawn();
    let message = user_message("req-1", "hello");

    assert!(agent.send(message.clone(), InboxTarget::NextTurn));
    agent.when_idle().await;
    assert_eq!(committed_user_rows(&agent, "req-1"), 1);

    // 已认领落盘：日志收据命中，重试不再开轮。
    assert!(!agent.send(message, InboxTarget::NextTurn));
    agent.when_idle().await;
    assert_eq!(committed_user_rows(&agent, "req-1"), 1);
}

#[tokio::test]
async fn receipts_are_recovered_from_the_session_log() {
    let agent = new_agent("session-dedup-recover");
    let mut session = Session::new(SessionId::new("session-dedup-recover"));
    session.append(SessionEvent::UserMessage(user_message("req-1", "hello")));
    agent.set_session(session);

    assert!(!agent.send(user_message("req-1", "hello"), InboxTarget::NextTurn));
    assert!(agent.queued_next_turn().is_empty());
}

#[tokio::test]
async fn receipts_belong_to_one_exact_session() {
    let agent = new_agent("session-dedup-switch");
    assert!(agent.send(user_message("req-1", "a"), InboxTarget::NextTurn));

    // 换入一个日志里没有该 id 的会话：收据按确切会话缓存，旧收据不得误挡。
    agent.set_session(Session::new(SessionId::new("session-dedup-other")));
    assert!(agent.send(user_message("req-1", "a"), InboxTarget::NextTurn));
    assert_eq!(agent.queued_next_turn().len(), 1);
}
