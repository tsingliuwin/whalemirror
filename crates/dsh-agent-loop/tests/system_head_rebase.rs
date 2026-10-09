//! 端到端验收：`set_session` 后 v3 system head 必须重定基到日志末条
//! system/message 行。回归背景：set_session 从日志算出 head 却丢弃——
//! 续跑（runtime_for_viewed_session / 宿主启动装载）走 None 分支对已有
//! head 重复 append（旧行残留）；主 agent 跨会话切换则残留旧会话 seq，
//! 写出悬空 replace 端点（上游语义要求换端点恰为当前 head）。

use async_trait::async_trait;
use dsh_agent_loop::{AgentOptions, ReactLoopAgent};
use dsh_cordis::EventBus;
use dsh_llm::{
    BoxStream, ContentBlockType, FinishReason, GenerateOptions, LlmAdapter, LlmError,
    LlmProviderInfo, LlmRuntime, SessionId, StreamChunk,
};
use dsh_persist::SessionRecorder;
use dsh_session::SessionEvent;
use dsh_system_prompt::{PromptSection, SystemPrompt};
use std::sync::Arc;

struct OkAdapter;

#[async_trait]
impl LlmAdapter for OkAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "mock".into(), name: "Mock".into() }
    }

    async fn stream(&self, _options: GenerateOptions) -> Result<BoxStream, LlmError> {
        let chunks = vec![
            StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text },
            StreamChunk::TextDelta { index: 0, text: "done".into() },
            StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
        ];
        Ok(Box::pin(futures::stream::iter(chunks)))
    }
}

fn build_agent(
    id: &str,
    llm: &Arc<LlmRuntime>,
    prompt: Arc<SystemPrompt>,
) -> Arc<ReactLoopAgent> {
    ReactLoopAgent::new(
        SessionId::new(id),
        AgentOptions {
            provider: "mock".into(),
            model: "mock".into(),
            max_tokens: None,
            system_prompt: Some("base prompt".into()),
            compaction: Default::default(),
            workdir: Default::default(),
            attachments_root: None,
        },
        Arc::clone(llm),
        Arc::new(dsh_tools::ToolRegistry::new()),
        prompt,
        Arc::new(dsh_session_projection::SessionProjections::default()),
        EventBus::new(),
    )
}

fn wire_sink(agent: &Arc<ReactLoopAgent>, recorder: &Arc<SessionRecorder>) {
    let sink_agent = Arc::clone(agent);
    let sink_recorder = Arc::clone(recorder);
    agent.set_event_sink(move |event| {
        let id = sink_agent.session().lock().unwrap().id.clone();
        let _ = sink_recorder.append(&id, "/tmp/ws", &event);
    });
}

/// 末条 system/message 行的 (seq, replace)——replace 形态即 v3 head 语义
/// 的落盘证据。
fn last_system_row(agent: &Arc<ReactLoopAgent>) -> Option<(u64, Option<u64>)> {
    agent.session().lock().unwrap().entries().iter().rev().find_map(|e| match &e.event {
        SessionEvent::SystemMessage { message: _, replace, .. } => Some((e.seq, *replace)),
        _ => None,
    })
}

/// 续跑：runtime 装载已有一轮的会话（日志带 system head）后，下一轮的
/// system commit 必须 replace 该行，而不是 None 分支重复 append。
#[tokio::test]
async fn resumed_session_replaces_logged_head_instead_of_appending() {
    let dir = std::env::temp_dir().join(format!("dsh-rebase-1-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let recorder = Arc::new(SessionRecorder::new(dir.join("sessions")));

    let events = EventBus::new();
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let _ = llm.register_adapter(&["mock".to_string()], Arc::new(OkAdapter)).unwrap();

    // 会话一：一轮对话，落 system head（append 形）
    let prompt_a = Arc::new(SystemPrompt::new());
    let agent_a = build_agent("session-rebase-a", &llm, Arc::clone(&prompt_a));
    wire_sink(&agent_a, &recorder);
    agent_a.spawn();
    agent_a.followup("hi");
    agent_a.when_idle().await;
    let (seq_a, replace_a) = last_system_row(&agent_a).expect("head row must exist");
    assert_eq!(replace_a, None, "first commit is append-form");

    // 续跑：从盘上装载同一会话给新 runtime（生产 runtime_for_viewed_session 路径）
    let (loaded, _) = recorder
        .load(&SessionId::new("session-rebase-a"), Some("/tmp/ws"))
        .expect("session should load");
    let loaded_head_seq = loaded
        .entries()
        .iter()
        .rev()
        .find_map(|e| match &e.event { SessionEvent::SystemMessage { .. } => Some(e.seq), _ => None })
        .expect("loaded session has head row");

    let prompt_b = Arc::new(SystemPrompt::new());
    let agent_b = build_agent("session-rebase-a", &llm, Arc::clone(&prompt_b));
    wire_sink(&agent_b, &recorder);
    agent_b.set_session(loaded);
    agent_b.spawn();
    // rendered 变化（新增 section）→ 必须走 replace 分支
    let _ = prompt_b.add_section(PromptSection {
        name: "workspace:instructions".into(),
        order: 400,
        text: "changed workspace rules".into(),
    });
    agent_b.followup("again");
    agent_b.when_idle().await;

    let (seq_b, replace_b) = last_system_row(&agent_b).expect("second head row must exist");
    assert!(
        seq_b > seq_a,
        "second system row comes after the first (seq_b={seq_b}, seq_a={seq_a})"
    );
    assert_eq!(
        replace_b,
        Some(loaded_head_seq),
        "resumed commit must replace the logged head (got replace={replace_b:?}, logged head={loaded_head_seq})"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// 跨会话切换：主 agent 先跑会话 A（内存 head=A 的 seq），切到装载的会话
/// B 后下一轮的 replace 端点必须是 B 日志里的 head seq，不得泄漏 A 的 seq。
#[tokio::test]
async fn switching_sessions_does_not_leak_prior_head_seq() {
    let dir = std::env::temp_dir().join(format!("dsh-rebase-2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let recorder = Arc::new(SessionRecorder::new(dir.join("sessions")));

    let events = EventBus::new();
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let _ = llm.register_adapter(&["mock".to_string()], Arc::new(OkAdapter)).unwrap();

    // 会话 A 先在同一 agent 上跑两轮，且轮间加 section 促使 A 的 head 被
    // replace 到更高 seq——保证 A 的内存 head seq 与 B 的 head seq 不同，
    // 泄漏时断言能区分。
    let prompt = Arc::new(SystemPrompt::new());
    let agent = build_agent("session-switch-a", &llm, Arc::clone(&prompt));
    wire_sink(&agent, &recorder);
    agent.spawn();
    agent.followup("hi");
    agent.when_idle().await;
    let _ = prompt.add_section(PromptSection {
        name: "workspace:instructions".into(),
        order: 400,
        text: "A extra rules".into(),
    });
    agent.followup("more");
    agent.when_idle().await;
    let (seq_a, replace_a) = last_system_row(&agent).expect("A head row must exist");
    assert!(replace_a.is_some(), "A's head was replaced mid-run");

    // 会话 B 独立跑一轮并落盘，然后装载给同一 agent（主 agent 切换路径）
    let agent_b = build_agent("session-switch-b", &llm, Arc::new(SystemPrompt::new()));
    wire_sink(&agent_b, &recorder);
    agent_b.spawn();
    agent_b.followup("hi");
    agent_b.when_idle().await;
    let (loaded_b, _) = recorder
        .load(&SessionId::new("session-switch-b"), Some("/tmp/ws"))
        .expect("session B should load");
    let b_head_seq = loaded_b
        .entries()
        .iter()
        .rev()
        .find_map(|e| match &e.event { SessionEvent::SystemMessage { .. } => Some(e.seq), _ => None })
        .expect("loaded B has head row");
    assert_ne!(seq_a, b_head_seq, "test construction: A and B head seqs must differ");

    agent.set_session(loaded_b);
    // rendered 变化 → replace 分支；端点必须是 B 的 head，不是 A 的 seq
    let _ = prompt.add_section(PromptSection {
        name: "workspace:instructions".into(),
        order: 400,
        text: "switched workspace rules".into(),
    });
    agent.followup("go on");
    agent.when_idle().await;

    let (seq_b, replace_b) = last_system_row(&agent).expect("B second head row must exist");
    assert!(seq_b > b_head_seq, "new row follows B's log (seq_b={seq_b}, b_head={b_head_seq})");
    assert_eq!(
        replace_b,
        Some(b_head_seq),
        "replace endpoint must be B's logged head, not the prior session's seq (replace={replace_b:?}, A seq={seq_a})"
    );

    std::fs::remove_dir_all(&dir).ok();
}
