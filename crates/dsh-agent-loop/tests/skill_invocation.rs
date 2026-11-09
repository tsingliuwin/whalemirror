//! 端到端验收：用户显式技能调用（上游 `tool-skill` 的 `/name` 手势）。
//!
//! 1. 用户消息里整词 `/demo-skill` → 该技能正文作为 `skill-invocation` 上下文
//!    追加在消息批**最后**（背景在前，该照做的在最后）；
//! 2. 未知名 / user-invocable=false 的技能 → 保持普通文本、不注入；
//! 3. 非 user source 的消息不算手势（外部文本无法伪造这个动作）；
//! 4. 同一名字重复出现只注入一次。

use async_trait::async_trait;
use dsh_agent_loop::{AgentOptions, ReactLoopAgent};
use dsh_cordis::EventBus;
use dsh_llm::{
    BoxStream, ContentBlock, ContentBlockType, FinishReason, GenerateOptions, LlmAdapter,
    LlmError, LlmProviderInfo, LlmRuntime, SessionId, StreamChunk,
};
use dsh_session::SessionEvent;
use dsh_system_prompt::SystemPrompt;
use dsh_tools::ToolRegistry;
use futures::stream;
use std::sync::{Arc, Mutex};

/// 只记录请求、回固定文本的适配器。
#[derive(Default)]
struct CapturingAdapter {
    requests: Mutex<Vec<GenerateOptions>>,
}

#[async_trait]
impl LlmAdapter for CapturingAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "mock".into(), name: "Mock".into() }
    }

    async fn stream(&self, options: GenerateOptions) -> Result<BoxStream, LlmError> {
        self.requests.lock().unwrap().push(options);
        Ok(Box::pin(stream::iter(vec![
            StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text },
            StreamChunk::TextDelta { index: 0, text: "ok".into() },
            StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
        ])))
    }
}

fn spawn_agent() -> (Arc<ReactLoopAgent>, Arc<CapturingAdapter>) {
    let events = EventBus::new();
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let adapter = Arc::new(CapturingAdapter::default());
    let _reg = llm
        .register_adapter(&["mock".to_string()], Arc::clone(&adapter) as Arc<dyn LlmAdapter>)
        .unwrap();
    let agent = ReactLoopAgent::new(
        SessionId::new(format!("session-skill-invoke-{}", std::process::id())),
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
    );
    // 宿主同款加载缝：只有这两个名字有正文，且都标成 user-invocable 后的结果
    agent.set_skill_loader(Arc::new(|name: &str| match name {
        "demo-skill" => Some("<skill_content name=\"demo-skill\">DEMO</skill_content>".to_string()),
        "other-skill" => Some("<skill_content name=\"other-skill\">OTHER</skill_content>".to_string()),
        _ => None,
    }));
    agent.spawn();
    (agent, adapter)
}

fn injected_contexts(agent: &Arc<ReactLoopAgent>) -> Vec<(String, String)> {
    let session = agent.session();
    let session = session.lock().unwrap();
    session
        .entries()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::UserMessage(m) => match &m.source {
                dsh_llm::MessageSource::Context { context_kind, name, .. }
                    if context_kind == "skill-invocation" =>
                {
                    let text = m
                        .content
                        .iter()
                        .find_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                        .unwrap_or_default();
                    Some((name.clone().unwrap_or_default(), text))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn slash_gesture_injects_the_skill_body_once() {
    let (agent, _adapter) = spawn_agent();
    agent.followup("/demo-skill 帮我做表格，再用 /demo-skill 确认");
    agent.when_idle().await;

    let injected = injected_contexts(&agent);
    assert_eq!(injected.len(), 1, "重复出现只注入一次: {injected:?}");
    assert_eq!(injected[0].0, "demo-skill");
    assert!(injected[0].1.contains("<skill_content name=\"demo-skill\">"), "{injected:?}");
}

#[tokio::test]
async fn unknown_and_plain_names_stay_prose() {
    let (agent, _adapter) = spawn_agent();
    // `/missing-skill` 未知名；`/usr/local/bin` 不是合法技能名 → 都不注入
    agent.followup("/missing-skill /usr/local/bin 普通提问");
    agent.when_idle().await;
    assert!(injected_contexts(&agent).is_empty());
}

#[tokio::test]
async fn multiple_gestures_inject_in_first_seen_order() {
    let (agent, _adapter) = spawn_agent();
    agent.followup("先 /other-skill 再 /demo-skill");
    agent.when_idle().await;
    let injected = injected_contexts(&agent);
    assert_eq!(injected.len(), 2, "{injected:?}");
    assert_eq!(injected[0].0, "other-skill");
    assert_eq!(injected[1].0, "demo-skill");
}

#[tokio::test]
async fn injected_instructions_come_last_in_the_message_batch() {
    let (agent, adapter) = spawn_agent();
    agent.followup("/demo-skill 做点事");
    agent.when_idle().await;
    // 注入的消息是消息批的最后一条（离模型答案最近）
    let session = agent.session();
    let session = session.lock().unwrap();
    let last_user_message = session
        .entries()
        .iter()
        .rev()
        .find_map(|e| match &e.event {
            SessionEvent::UserMessage(m) => Some(m.clone()),
            _ => None,
        })
        .expect("有用户消息批");
    assert!(
        matches!(&last_user_message.source, dsh_llm::MessageSource::Context { context_kind, .. } if context_kind == "skill-invocation"),
        "最后一条应是技能注入: {:?}",
        last_user_message.source
    );
    let _ = adapter;
}