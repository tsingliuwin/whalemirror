//! 端到端验收：todo_write 整表快照（上游 `todo/tool-todo` 的等价面）。
//!
//! 1. 工具出现在请求工具面；
//! 2. 一次调用把整表经 sink 落成会话 `todo/write` 事件（模型看到计数回执）；
//! 3. 回放 fold：last-write-wins，遇 `turn/start` 清空（轮末保留清单可见）；
//! 4. 校验失败（两个 in_progress）不落盘，按上游文案回错误。

use async_trait::async_trait;
use dsh_agent_loop::{AgentOptions, ReactLoopAgent};
use dsh_cordis::EventBus;
use dsh_llm::{
    BoxStream, CallId, ContentBlock, ContentBlockType, FinishReason, GenerateOptions, LlmAdapter,
    LlmError, LlmProviderInfo, LlmRuntime, SessionId, StreamChunk,
};
use dsh_session::SessionEvent;
use dsh_system_prompt::SystemPrompt;
use dsh_tools::{TodoTool, Tool, ToolAdmission, ToolRegistry};
use futures::stream;
use std::sync::{Arc, Mutex};

/// 首请求回一个 todo_write 调用（载荷可切换），之后回文本。
struct ScriptedAdapter {
    requests: Mutex<Vec<GenerateOptions>>,
    call_args: Mutex<String>,
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
            let args = self.call_args.lock().unwrap().clone();
            vec![
                StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::ToolCall },
                StreamChunk::ToolCallDelta {
                    index: 0,
                    id: CallId::new("call_todo"),
                    name: Some("todo_write".into()),
                    arguments_delta: args,
                },
                StreamChunk::Finish { reason: FinishReason::ToolCalls, replay_state: None },
            ]
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

fn spawn_agent(call_args: &str) -> (Arc<ReactLoopAgent>, Arc<ScriptedAdapter>, Arc<TodoTool>) {
    let events = EventBus::new();
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let adapter = Arc::new(ScriptedAdapter {
        requests: Mutex::new(Vec::new()),
        call_args: Mutex::new(call_args.to_string()),
    });
    let _reg = llm
        .register_adapter(&["mock".to_string()], Arc::clone(&adapter) as Arc<dyn LlmAdapter>)
        .unwrap();
    let tools = Arc::new(ToolRegistry::new());
    let todo_tool = Arc::new(TodoTool::new(false));
    // 宿主同款条件注册（标准预设准入）
    let _todo = tools
        .register_when(
            Arc::clone(&todo_tool) as Arc<dyn Tool>,
            Arc::new(|ctx: &ToolAdmission| ctx.preset_admits("todo_write")),
        )
        .unwrap();
    let agent = ReactLoopAgent::new(
        SessionId::new(format!("session-todo-{}", std::process::id())),
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
        Arc::clone(&tools),
        Arc::new(SystemPrompt::new()),
        Arc::new(dsh_session_projection::SessionProjections::default()),
        events,
    );
    // 宿主同款 sink：整表快照落会话日志
    {
        let agent_sink = Arc::clone(&agent);
        todo_tool.set_sink(Arc::new(move |items: &[dsh_tools::TodoItem]| {
            let entries: Vec<dsh_session::TodoEntry> = items
                .iter()
                .map(|t| dsh_session::TodoEntry {
                    content: t.content.clone(),
                    status: t.status.as_wire().to_string(),
                })
                .collect();
            agent_sink.append_session_event(SessionEvent::TodoWrite { todos: entries });
        }));
    }
    agent.spawn();
    (agent, adapter, todo_tool)
}

fn tool_results(agent: &Arc<ReactLoopAgent>) -> Vec<String> {
    let session = agent.session();
    let session = session.lock().unwrap();
    session
        .entries()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::ToolResult { message, .. } => Some(
                message
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolResult { content, .. } => Some(
                            content
                                .iter()
                                .filter_map(|c| match c {
                                    ContentBlock::Text { text } => Some(text.as_str()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("\n"),
                        ),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .collect()
}

fn todo_events(agent: &Arc<ReactLoopAgent>) -> Vec<Vec<dsh_session::TodoEntry>> {
    let session = agent.session();
    let session = session.lock().unwrap();
    session
        .entries()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::TodoWrite { todos } => Some(todos.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn todo_write_records_the_whole_list_and_reports_counts() {
    let (agent, adapter, _tool) = spawn_agent(
        r#"{"todos":[{"content":"读代码","status":"completed"},{"content":"写实现","status":"in_progress"},{"content":"跑测试","status":"pending"}]}"#,
    );

    agent.followup("plan the work");
    agent.when_idle().await;

    // 1) 工具在声明面上
    {
        let requests = adapter.requests.lock().unwrap();
        let names: Vec<String> = requests[0]
            .tools
            .as_ref()
            .map(|tools| tools.iter().map(|t| t.name.clone()).collect())
            .unwrap_or_default();
        assert!(names.iter().any(|n| n == "todo_write"), "{names:?}");
    }
    // 2) 模型看到计数回执
    let results = tool_results(&agent);
    assert!(
        results.iter().any(|t| t.contains("Updated todo list: 1 pending, 1 in progress, 1 completed.")),
        "{results:?}"
    );
    // 3) 会话日志落整表快照
    let snapshots = todo_events(&agent);
    assert_eq!(snapshots.len(), 1, "{snapshots:?}");
    assert_eq!(snapshots[0].len(), 3);
    assert_eq!(snapshots[0][1].content, "写实现");
    assert_eq!(snapshots[0][1].status, "in_progress");
}

#[tokio::test]
async fn single_active_discipline_rejects_two_in_progress() {
    let (agent, _adapter, _tool) = spawn_agent(
        r#"{"todos":[{"content":"a","status":"in_progress"},{"content":"b","status":"in_progress"}]}"#,
    );

    agent.followup("go");
    agent.when_idle().await;

    let results = tool_results(&agent);
    assert!(
        results
            .iter()
            .any(|t| t.contains("invalid todos: at most one task may be in_progress (got 2)")),
        "{results:?}"
    );
    // 校验失败不落盘
    assert!(todo_events(&agent).is_empty());
}
