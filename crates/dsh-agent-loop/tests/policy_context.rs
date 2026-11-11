//! 运行时策略语境（上游 `sandbox:policy` / `approval:policy`）：set_session
//! 折叠恢复三类权限旋钮事件，`prompt_assembly` 的系统提示随档位携带逐字
//! 语境句；策略切换通知落 user/plugin `user-approval` 裸源耐久消息。

use async_trait::async_trait;
use dsh_agent_loop::{AgentOptions, ReactLoopAgent};
use dsh_cordis::EventBus;
use dsh_llm::{
    BoxStream, ContentBlockType, FinishReason, GenerateOptions, LlmAdapter, LlmError,
    LlmProviderInfo, LlmRuntime, SessionId, StreamChunk,
};
use dsh_session::SessionEvent;
use dsh_session_projection::SessionProjections;
use dsh_system_prompt::SystemPrompt;
use std::sync::Arc;

/// 记录请求并回一段文本的假适配器（不发请求也能看组装——prompt_assembly
/// 直读；适配器只为构造 LlmRuntime 存在）。
struct NopAdapter;

#[async_trait]
impl LlmAdapter for NopAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "mock".into(), name: "Mock".into() }
    }

    async fn stream(&self, _options: GenerateOptions) -> Result<BoxStream, LlmError> {
        let chunks = vec![
            StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text },
            StreamChunk::TextDelta { index: 0, text: "ok".into() },
            StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
        ];
        Ok(Box::pin(futures::stream::iter(chunks)))
    }
}

fn spawn_agent(session_id: &str) -> Arc<ReactLoopAgent> {
    let events = EventBus::new();
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let _reg = llm
        .register_adapter(&["mock".to_string()], Arc::new(NopAdapter) as Arc<dyn LlmAdapter>)
        .unwrap();
    let agent = ReactLoopAgent::new(
        SessionId::new(session_id.to_string()),
        AgentOptions {
            provider: "mock".into(),
            model: "mock".into(),
            max_tokens: None,
            system_prompt: Some("base-persona".into()),
            compaction: Default::default(),
            workdir: Default::default(),
            attachments_root: None,
        },
        llm,
        Arc::new(dsh_tools::ToolRegistry::new()),
        Arc::new(SystemPrompt::new()),
        Arc::new(SessionProjections::default()),
        events,
    );
    agent
}

/// 有事件的会话：三类旋钮事件各一条（danger-full-access + never）。
fn session_with_knobs() -> dsh_session::Session {
    let mut session = dsh_session::Session::new(SessionId::new("session-policy-fold-test".to_string()));
    session.append(SessionEvent::PermissionPreset { preset: "danger-full-access".into() });
    session.append(SessionEvent::SandboxModeSwitch { mode: "danger-full-access".into() });
    session.append(SessionEvent::ApprovalPolicy { policy: "never".into() });
    session
}

#[test]
fn folded_knobs_drive_prompt_context_sentences() {
    let agent = spawn_agent("session-policy-fold-a");
    // 无事件会话 → 回落目录默认（workspace-write + ask）
    agent.set_session(dsh_session::Session::new(SessionId::new("session-policy-fold-b".to_string())));
    let system = agent.prompt_assembly().system;
    assert!(system.contains(
        "Current DSH file policy: workspace-write. Any available operation enforced by the DSH file sandbox may modify files under the session workspace:"
    ));
    assert!(system.contains(
        "Approval prompts are enabled in this session: operations that require approval may ask through the configured answerers; without an available answerer, the request fails closed."
    ));

    // 三类事件折叠 → danger + never 语境
    agent.set_session(session_with_knobs());
    let system = agent.prompt_assembly().system;
    assert!(system.contains(
        "Current DSH file policy: danger-full-access. The DSH file sandbox does not restrict file modifications by available operations."
    ));
    assert!(system.contains(
        "Approval prompts are disabled in this session: actions that require approval are rejected automatically — do not request sandbox escalation (do not set `sandbox_permissions`)."
    ));
    assert!(!system.contains("workspace:"));
}

#[test]
fn runtime_switch_updates_next_assembly_without_session() {
    let agent = spawn_agent("session-policy-fold-c");
    agent.set_session(session_with_knobs());
    // 宿主运行时切换（权限预设换挡）：显式更新，不改会话
    agent.policy().set_sandbox("read-only", Vec::new());
    agent.policy().set_approval_policy("ask");
    let system = agent.prompt_assembly().system;
    assert!(system.contains(
        "Current DSH file policy: read-only. Any available operation enforced by the DSH file sandbox cannot modify files in the standing mode."
    ));
}

#[test]
fn policy_notice_lands_as_user_approval_source_message() {
    let agent = spawn_agent("session-policy-notice");
    agent.set_session(dsh_session::Session::new(SessionId::new("session-policy-notice-b".to_string())));
    agent.append_policy_change_notice("ask", "never");
    let session_guard = agent.session();
    let session = session_guard.lock().unwrap();
    let last = session.entries().last().expect("notice appended").event.clone();
    let dsh_session::SessionEvent::UserMessage(message) = last else {
        panic!("expected UserMessage, got {last:?}");
    };
    let notice_text = message.content.first().and_then(|b| match b {
        dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
        _ => None,
    });
    assert_eq!(
        notice_text.as_deref(),
        Some(
            "The user changed the approval policy from \"ask the user for approval\" to \"never ask the user for approval\"."
        )
    );
    let dsh_llm::MessageSource::Plugin { plugin, form, summary, .. } = &message.source else {
        panic!("expected Plugin source, got {:?}", message.source);
    };
    assert_eq!(plugin, "user-approval");
    assert_eq!(form.as_deref(), None);
    assert_eq!(summary.as_deref(), None);
}
