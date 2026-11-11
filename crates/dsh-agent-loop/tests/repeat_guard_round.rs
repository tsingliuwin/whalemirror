//! repeat-tool-reminder 回路：一步内三次同参调用（未注册工具 = 未知拒绝
//! 路径——上游语义被拒调用同样计数）恰在阈值 3 注入温和提醒；用户插话
//! 重置连击链，后续轮不再追加。

use async_trait::async_trait;
use dsh_agent_loop::{AgentOptions, ReactLoopAgent};
use dsh_cordis::EventBus;
use dsh_llm::{
    BoxStream, CallId, ContentBlockType, FinishReason, GenerateOptions, LlmAdapter, LlmError,
    LlmProviderInfo, LlmRuntime, MessageSource, SessionId, StreamChunk,
};
use dsh_session::SessionEvent;
use dsh_session_projection::SessionProjections;
use dsh_system_prompt::SystemPrompt;
use dsh_tools::ToolRegistry;
use futures::stream;
use std::sync::{Arc, Mutex};

/// 脚本化适配器：第 1 步回三次同参工具调用（echo，未注册），其后文本收尾。
struct ScriptedAdapter {
    requests: Mutex<Vec<GenerateOptions>>,
}

#[async_trait]
impl LlmAdapter for ScriptedAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "mock".into(), name: "Mock".into() }
    }

    async fn stream(&self, options: GenerateOptions) -> Result<BoxStream, LlmError> {
        let index = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(options);
            requests.len() - 1
        };
        let chunks = if index == 0 {
            let mut chunks = Vec::new();
            for i in 0..3 {
                chunks.push(StreamChunk::BlockStart { index: i, block_type: ContentBlockType::ToolCall });
                chunks.push(StreamChunk::ToolCallDelta {
                    index: i,
                    id: CallId::new(format!("call_repeat_{i}")),
                    name: Some("echo".into()),
                    arguments_delta: r#"{"x":1}"#.into(),
                });
            }
            chunks.push(StreamChunk::Finish { reason: FinishReason::ToolCalls, replay_state: None });
            chunks
        } else {
            vec![
                StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text },
                StreamChunk::TextDelta { index: 0, text: "done".into() },
                StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
            ]
        };
        Ok(Box::pin(stream::iter(chunks)))
    }
}

fn reminders(agent: &Arc<ReactLoopAgent>) -> Vec<(String, String)> {
    let session = agent.session();
    let session = session.lock().unwrap();
    session
        .entries()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::UserMessage(m) => match &m.source {
                MessageSource::Plugin { plugin, form, summary, .. } if plugin == "repeat-tool-reminder" => {
                    let text = m.content.first().and_then(|b| match b {
                        dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })?;
                    assert_eq!(form.as_deref(), Some("notice"));
                    Some((text, summary.clone().unwrap_or_default()))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn third_identical_call_injects_gentle_notice_and_user_turn_resets() {
    let events = EventBus::new();
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let adapter = Arc::new(ScriptedAdapter { requests: Mutex::new(Vec::new()) });
    let _reg = llm
        .register_adapter(&["mock".to_string()], Arc::clone(&adapter) as Arc<dyn LlmAdapter>)
        .unwrap();
    let agent = ReactLoopAgent::new(
        SessionId::new(format!("session-repeat-guard-{}", std::process::id())),
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
        Arc::new(SessionProjections::default()),
        events,
    );
    agent.spawn();
    agent.send(dsh_llm::Message::user_text("go"), dsh_agent_loop::InboxTarget::NextTurn);
    agent.when_idle().await;

    // 阈值 3 恰命中一次：温和文案 + summary "echo × 3"
    let hits = reminders(&agent);
    assert_eq!(hits.len(), 1, "exactly one reminder on the threshold");
    assert_eq!(hits[0].1, "echo × 3");
    assert_eq!(
        hits[0].0,
        "You are repeating the exact same tool call with identical arguments. Carefully analyze the previous result before calling again: if the task is not complete, try a different approach or different arguments instead of repeating the call."
    );

    // 用户插话（user 源输入）重置连击链：下一轮文本收尾不再注入
    agent.send(dsh_llm::Message::user_text("stop"), dsh_agent_loop::InboxTarget::NextTurn);
    agent.when_idle().await;
    assert_eq!(reminders(&agent).len(), 1, "user interjection resets the chain");
}
