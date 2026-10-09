//! 端到端验收：subagent 子会话耐久化 + 父目录 catalog（上游
//! establishCatalogChild 语义）——
//!
//! 1. 子会话以真实会话文件落盘（标题=description，含完整轮次流），
//!    可被既有 load 路径回读（侧栏按普通会话回看的数据前提）；
//! 2. 父日志在子建立时落 `subagent/catalog` 发现事实（one-shot +
//!    label），childId 指向真实落盘子会话；
//! 3. 工具结果仍返回子任务末条报告（原有语义不变）。

use async_trait::async_trait;
use dsh_agent_loop::{AgentOptions, ReactLoopAgent};
use dsh_cordis::EventBus;
use dsh_llm::types::{ContentBlockType, SessionId as LlmSessionId};
use dsh_llm::{
    BoxStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, LlmError,
    LlmProviderInfo, LlmRuntime, StreamChunk,
};
use dsh_persist::SessionRecorder;
use dsh_session::SessionEvent;
use dsh_session_projection::SessionProjections;
use dsh_system_prompt::SystemPrompt;
use dsh_tools::ToolRegistry;
use futures::stream;
use std::sync::{Arc, Mutex};

/// 脚本化适配器：第 1 次请求（父）发 subagent 工具调用；子请求与父收尾
/// 请求返回文本 + stop。
#[derive(Default)]
struct ScriptedAdapter {
    calls: Mutex<Vec<GenerateOptions>>,
}

#[async_trait]
impl LlmAdapter for ScriptedAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "mock".into(), name: "Mock".into() }
    }

    async fn stream(&self, options: GenerateOptions) -> Result<BoxStream, LlmError> {
        let n = { let mut c = self.calls.lock().unwrap(); c.push(options); c.len() };
        let chunks: Vec<StreamChunk> = if n == 1 {
            vec![
                StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::ToolCall },
                StreamChunk::ToolCallDelta {
                    index: 0,
                    id: CallId("call-1".into()),
                    name: Some("subagent".into()),
                    arguments_delta: serde_json::json!(
                        {"description": "demo task", "prompt": "Say hello."}
                    )
                    .to_string(),
                },
                StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::ToolCall {
                        id: CallId("call-1".into()),
                        name: "subagent".into(),
                        arguments: serde_json::json!(
                            {"description": "demo task", "prompt": "Say hello."}
                        )
                        .to_string(),
                    },
                },
                StreamChunk::Finish { reason: FinishReason::ToolCalls, replay_state: None },
            ]
        } else {
            vec![
                StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text },
                StreamChunk::TextDelta {
                    index: 0,
                    text: if n == 2 { "child report".into() } else { "parent done".into() },
                },
                StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
            ]
        };
        Ok(Box::pin(stream::iter(chunks)))
    }
}

#[tokio::test]
async fn child_session_persists_and_parent_logs_catalog() {
    let dir = std::env::temp_dir().join(format!("dsh-subagent-persist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let recorder = Arc::new(SessionRecorder::new(dir.join("sessions")));
    let cwd = "/tmp/ws".to_string();

    let events = EventBus::new();
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let adapter = Arc::new(ScriptedAdapter::default());
    let _reg = llm
        .register_adapter(&["mock".to_string()], Arc::clone(&adapter) as Arc<dyn LlmAdapter>)
        .unwrap();

    let projections = Arc::new(SessionProjections::default());
    let subagent = dsh_subagent::SubagentTool::new(
        Arc::clone(&llm),
        Arc::new(ToolRegistry::new()),
        Arc::new(SystemPrompt::new()),
        "mock",
        "mock",
    )
    .with_projections(Arc::clone(&projections));
    let tools = Arc::new(ToolRegistry::new());
    let _t = tools
        .register(Arc::clone(&subagent) as Arc<dyn dsh_tools::Tool>)
        .unwrap();

    let parent = Arc::new(ReactLoopAgent::new(
        LlmSessionId::new("session-parent"),
        AgentOptions {
            provider: "mock".into(),
            model: "mock".into(),
            max_tokens: None,
            system_prompt: None,
            compaction: Default::default(),
            workdir: Default::default(),
            attachments_root: None,
        },
        Arc::clone(&llm),
        Arc::clone(&tools),
        Arc::new(SystemPrompt::new()),
        Arc::clone(&projections),
        events,
    ));

    // 持久化缝：子会话与父会话共用 recorder（cwd 固定）；记录见过的子 id
    let seen_child_ids: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_recorder = Arc::clone(&recorder);
    let seen = Arc::clone(&seen_child_ids);
    subagent.set_child_sink(Arc::new(move |id: &LlmSessionId, event: &SessionEvent| {
        seen.lock().unwrap().push(id.as_str().to_string());
        let _ = sink_recorder.append(id, &cwd, event);
    }));
    subagent.set_parent_link(Arc::clone(&parent));

    // 父事件也落盘（catalog 行随之持久化）
    let parent_recorder = Arc::clone(&recorder);
    let parent_sink_agent = Arc::clone(&parent);
    parent.set_event_sink(move |event: SessionEvent| {
        let id = parent_sink_agent.session().lock().unwrap().id.clone();
        let _ = parent_recorder.append(&id, "/tmp/ws", &event);
    });

    parent.spawn();
    parent.followup("delegate a task");
    parent.when_idle().await;

    // 3 次请求：父（工具调用）→ 子（报告）→ 父（收尾）
    assert_eq!(adapter.calls.lock().unwrap().len(), 3, "parent→child→parent request script");

    // 工具结果带回子报告
    let parent_session = parent.session();
    let parent_session = parent_session.lock().unwrap();
    let tool_result = parent_session.entries().iter().find_map(|e| match &e.event {
        SessionEvent::ToolResult { message, .. } => Some(message.clone()),
        _ => None,
    });
    let tool_text = tool_result
        .and_then(|m| m.content.iter().find_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => content.iter().find_map(|c| match c {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            }),
            _ => None,
        }))
        .expect("tool result row");
    assert!(
        tool_text.contains("Subagent (demo task) final report:")
            && tool_text.contains("child report"),
        "{tool_text}"
    );

    // 父日志：subagent/catalog 发现事实，childId 指向真实落盘子会话
    let catalog = parent_session
        .entries()
        .iter()
        .find_map(|e| match &e.event {
            SessionEvent::SubagentCatalog { child_id, mode, label, .. } => {
                Some((child_id.clone(), mode.clone(), label.clone()))
            }
            _ => None,
        })
        .expect("parent log carries subagent/catalog");
    drop(parent_session);
    assert_eq!(catalog.1, "one-shot");
    assert_eq!(catalog.2.as_deref(), Some("demo task"));

    // 子会话：真实会话文件可回读（标题=description + 完整轮次流）
    let child_id = catalog.0;
    assert!(
        seen_child_ids.lock().unwrap().iter().any(|id| *id == child_id),
        "sink saw the child session id"
    );
    let (child_loaded, _) = recorder
        .load(&LlmSessionId::new(child_id.clone()), Some("/tmp/ws"))
        .expect("child session should load from disk");
    assert!(
        matches!(
            child_loaded.entries().first().map(|e| &e.event),
            Some(SessionEvent::SessionTitle { title }) if title == "demo task"
        ),
        "first durable row is the description title"
    );
    assert!(
        child_loaded
            .entries()
            .iter()
            .any(|e| matches!(&e.event, SessionEvent::TurnStart { .. })),
        "child log carries its own turn flow"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// 恒文本适配器（settlement e2e 的子 agent 驱动：首请求即终报）。
#[derive(Default)]
struct PlainAdapter {
    calls: Mutex<Vec<GenerateOptions>>,
}

#[async_trait]
impl LlmAdapter for PlainAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "mock".into(), name: "Mock".into() }
    }

    async fn stream(&self, options: GenerateOptions) -> Result<BoxStream, LlmError> {
        self.calls.lock().unwrap().push(options);
        let chunks = vec![
            StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text },
            StreamChunk::TextDelta { index: 0, text: "child report".into() },
            StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
        ];
        Ok(Box::pin(stream::iter(chunks)))
    }
}

#[tokio::test]
async fn settlement_notice_lands_in_parent_log() {
    // #4 settlement：子任务正常完成后父日志落 subagent-settled
    // user/message（notifySettlement 语义——与 subagent/catalog 并存）
    let dir = std::env::temp_dir().join(format!("dsh-subagent-settle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let events = EventBus::new();
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let adapter = Arc::new(PlainAdapter::default());
    let _reg = llm
        .register_adapter(&["mock".to_string()], Arc::clone(&adapter) as Arc<dyn LlmAdapter>)
        .unwrap();
    let subagent = dsh_subagent::SubagentTool::new(
        Arc::clone(&llm),
        Arc::new(ToolRegistry::new()),
        Arc::new(dsh_system_prompt::SystemPrompt::new()),
        "mock",
        "mock",
    )
    .with_projections(Arc::new(dsh_session_projection::SessionProjections::default()));
    let parent = Arc::new(ReactLoopAgent::new(
        dsh_llm::types::SessionId::new("session-parent"),
        AgentOptions {
            provider: "mock".into(),
            model: "mock".into(),
            max_tokens: None,
            system_prompt: None,
            compaction: Default::default(),
            workdir: Default::default(),
            attachments_root: None,
        },
        Arc::clone(&llm),
        Arc::new(ToolRegistry::new()),
        Arc::new(dsh_system_prompt::SystemPrompt::new()),
        Arc::new(dsh_session_projection::SessionProjections::default()),
        events,
    ));
    subagent.set_parent_link(Arc::clone(&parent));
    // child_sink 在场才启用子会话耐久化+catalog 面（与宿主接线同条件）
    let sink_dir = dir.clone();
    subagent.set_child_sink(Arc::new(move |id: &LlmSessionId, event: &SessionEvent| {
        let _ = &sink_dir;
        let _ = id;
        let _ = event;
    }));

    use dsh_tools::Tool as _;
    let input = dsh_tools::ToolExecutionInput::with_raw_arguments(
        CallId("s1".into()),
        "subagent".into(),
        r#"{"description": "demo task", "prompt": "Say hello."}"#.to_string(),
    );
    let r = subagent.execute(&input).await;
    let _ = r;
    let _ = &dir;

    // 父日志（内存会话）同时含 subagent/catalog 与 subagent-settled notice
    let session = parent.session();
    let session = session.lock().unwrap();
    let has_catalog = session
        .entries()
        .iter()
        .any(|e| matches!(&e.event, SessionEvent::SubagentCatalog { .. }));
    assert!(has_catalog, "catalog event in parent log");
    let notice = session
        .entries()
        .iter()
        .find_map(|e| match &e.event {
            SessionEvent::UserMessage(m) if matches!(&m.source, dsh_llm::MessageSource::Context { context_kind, form, .. } if context_kind == "subagent-settled" && form.as_deref() == Some("notice")) => Some(m.clone()),
            _ => None,
        })
        .expect("settlement notice user/message in parent log");
    let notice_text: String = notice
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        notice_text.contains("Background subagent ") && notice_text.contains(" finished. It cannot receive follow-up messages."),
        "{notice_text}"
    );
    assert!(
        notice_text.contains("It left no closing message.") || notice_text.contains("Its closing message:"),
        "{notice_text}"
    );
}
