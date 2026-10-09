//! 端到端验收：plan-mode 通知走收件箱（上游 agent.inject 语义）——
//!
//! 1. 空闲期 set_plan_mode 只落 plan/mode 事件，notice 不直写日志
//!    （直写会让它游离在轮间、被记到上一轮名下）；
//! 2. 下一条用户消息开轮时，notice 在 step 1 随批认领并落盘，且先于
//!    用户文本（claim 序：next-step 先于 next-turn）；
//! 3. 落盘形为 producer-owned source kind（v4 退役 plugin 包装），
//!    重载回放还原为 context 行原料；
//! 4. 请求历史包含 notice（模型可见）。

use async_trait::async_trait;
use dsh_agent_loop::{AgentOptions, ReactLoopAgent};
use dsh_cordis::EventBus;
use dsh_llm::{
    BoxStream, ContentBlock, ContentBlockType, FinishReason, GenerateOptions, LlmAdapter, LlmError,
    LlmProviderInfo, LlmRuntime, MessageSource, SessionId, StreamChunk,
};
use dsh_persist::SessionRecorder;
use dsh_session::SessionEvent;
use dsh_system_prompt::SystemPrompt;
use dsh_tools::ToolRegistry;
use futures::stream;
use std::sync::{Arc, Mutex};

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
        let chunks = vec![
            StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text },
            StreamChunk::TextDelta { index: 0, text: "ok".into() },
            StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
        ];
        Ok(Box::pin(stream::iter(chunks)))
    }
}

const NOTICE_TEXT: &str = "The user switched this session to plan mode.";

fn spawn_agent(dir: &std::path::Path) -> (Arc<ReactLoopAgent>, Arc<SessionRecorder>, Arc<CapturingAdapter>) {
    let recorder = Arc::new(SessionRecorder::new(dir.join("sessions")));
    let events = EventBus::new();
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let adapter = Arc::new(CapturingAdapter::default());
    let _reg = llm
        .register_adapter(&["mock".to_string()], Arc::clone(&adapter) as Arc<dyn LlmAdapter>)
        .unwrap();
    let agent = ReactLoopAgent::new(
        SessionId::new("session-plan-notice"),
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
    let sink_agent = Arc::clone(&agent);
    let sink_recorder = Arc::clone(&recorder);
    agent.set_event_sink(move |event| {
        let id = sink_agent.session().lock().unwrap().id.clone();
        let _ = sink_recorder.append(&id, "/tmp/ws", &event);
    });
    agent.spawn();
    (agent, recorder, adapter)
}

#[tokio::test]
async fn plan_mode_notice_rides_inbox_and_lands_at_next_turn_step_one() {
    let dir = std::env::temp_dir().join(format!("dsh-loop-plan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (agent, recorder, adapter) = spawn_agent(&dir);

    // 空闲期切换 plan 模式：plan/mode 立即落盘，notice 进收件箱不落日志
    agent.set_plan_mode(true);
    assert!(agent.plan_mode_active(), "plan mode folded on");
    {
        let session = agent.session();
        let session = session.lock().unwrap();
        assert!(
            session.entries().iter().any(|e| matches!(&e.event, SessionEvent::PlanMode { active: true, .. })),
            "plan/mode event must be logged immediately"
        );
        assert!(
            !session.entries().iter().any(|e| matches!(&e.event, SessionEvent::UserMessage(_))),
            "notice must not be written to the log while idle"
        );
    }

    // 下一条用户消息开轮：notice 在 step 1 随批认领、先于用户文本落盘
    agent.followup("hi");
    agent.when_idle().await;

    let logged: Vec<SessionEvent> = {
        let session = agent.session();
        let session = session.lock().unwrap();
        session.entries().iter().map(|e| e.event.clone()).collect()
    };
    let turn1 = logged
        .iter()
        .position(|e| matches!(e, SessionEvent::TurnStart { turn: 1 }))
        .expect("turn 1 started");
    let notice_ix = logged[turn1..]
        .iter()
        .position(|e| matches!(e, SessionEvent::UserMessage(m) if matches!(&m.source, MessageSource::Plugin { plugin, .. } if plugin == "plan-mode")))
        .map(|i| i + turn1)
        .expect("claimed plan-mode notice logged");
    let user_ix = logged[turn1..]
        .iter()
        .position(|e| matches!(e, SessionEvent::UserMessage(m) if matches!(&m.source, MessageSource::User)))
        .map(|i| i + turn1)
        .expect("user text logged");
    assert!(notice_ix < user_ix, "notice (next-step claim) precedes user text (next-turn claim)");
    assert!(
        matches!(&logged[notice_ix], SessionEvent::UserMessage(m)
            if matches!(&m.source, MessageSource::Plugin { form, summary, .. }
                if form.as_deref() == Some("notice") && summary.as_deref() == Some(NOTICE_TEXT))),
        "notice carries form=notice and its own summary"
    );

    // 请求历史包含 notice（模型可见）——derive_messages 收认领后的落盘消息
    let requests = adapter.requests.lock().unwrap();
    assert!(!requests.is_empty(), "adapter must be called");
    assert!(
        requests[0].messages.iter().any(|m| m
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text == NOTICE_TEXT))),
        "notice must reach the request history"
    );
    drop(requests);

    // 落盘形：producer-owned source kind（v4 退役 plugin 包装）——重载后
    // 非(user|system-prompt) kind 还原为 context 行原料
    let (loaded, _) = recorder
        .load(&SessionId::new("session-plan-notice"), Some("/tmp/ws"))
        .expect("session should load");
    let reloaded_notice = loaded.entries().iter().find_map(|e| match &e.event {
        SessionEvent::UserMessage(m) if matches!(m.content.first(), Some(ContentBlock::Text { text }) if text == NOTICE_TEXT) => {
            Some(m)
        }
        _ => None,
    });
    let reloaded_notice = reloaded_notice.expect("notice reloaded");
    match &reloaded_notice.source {
        MessageSource::Context { context_kind, form, summary, .. } => {
            assert_eq!(context_kind, "plan-mode", "v4 source kind is producer-owned");
            assert_eq!(form.as_deref(), Some("notice"));
            assert_eq!(summary.as_deref(), Some(NOTICE_TEXT));
        }
        other => panic!("owned kind expected after reload, got {other:?}"),
    }

    // 空闲期再次切换回默认模式：第二次 notice 也进收件箱不直写
    agent.set_plan_mode(false);
    {
        let session = agent.session();
        let session = session.lock().unwrap();
        let entries = session.entries();
        let second_plan = entries
            .iter()
            .rposition(|e| matches!(&e.event, SessionEvent::PlanMode { active: false, .. }))
            .expect("second plan/mode event logged");
        assert!(
            !entries[second_plan..]
                .iter()
                .any(|e| matches!(&e.event, SessionEvent::UserMessage(_))),
            "second notice must also ride the inbox, not the log"
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}
