//! 端到端验收：plan-mode 渐进披露 + 按步工具准入（上游 feat(plan)
//! 752cc5c0c0 + feat(tools)「admit conditional tools per step」的等价面）。
//!
//! 1. 会话首次进入 plan 模式之前：`exit_plan_mode` 不出现在请求工具面；
//! 2. 进入后：随声明一起出现；
//! 3. 离开 plan 模式：声明保留（addition-only 路由不再丢前缀缓存），
//!    仅指引变化（rustdsh 无 plan 指引段，指引面无变化）;
//! 4. 未准入时模型硬调用：派发侧按未知工具拒绝（上游 UNKNOWN_TOOL）；
//! 5. 离开 plan 模式后的调用：工具侧失败「unavailable outside plan mode」。

use async_trait::async_trait;
use dsh_agent_loop::{AgentOptions, ReactLoopAgent};
use dsh_cordis::EventBus;
use dsh_llm::{
    BoxStream, CallId, ContentBlock, ContentBlockType, FinishReason, GenerateOptions, LlmAdapter,
    LlmError, LlmProviderInfo, LlmRuntime, SessionId, StreamChunk,
};
use dsh_session::SessionEvent;
use dsh_system_prompt::SystemPrompt;
use dsh_tools::{ExitPlanModeTool, Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult, ToolRegistry};
use futures::stream;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct CapturingAdapter {
    requests: Mutex<Vec<GenerateOptions>>,
    /// true：首个请求回一个 exit_plan_mode 工具调用，之后回文本。
    script_plan_call: Mutex<bool>,
}

#[async_trait]
impl LlmAdapter for CapturingAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "mock".into(), name: "Mock".into() }
    }

    async fn stream(&self, options: GenerateOptions) -> Result<BoxStream, LlmError> {
        let index = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(options);
            requests.len() - 1
        };
        let scripted = index == 0 && *self.script_plan_call.lock().unwrap();
        let chunks = if scripted {
            vec![
                StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::ToolCall },
                StreamChunk::ToolCallDelta {
                    index: 0,
                    id: CallId::new("call_plan"),
                    name: Some("exit_plan_mode".into()),
                    arguments_delta: "{\"plan\":\"# Do it\"}".into(),
                },
                StreamChunk::Finish { reason: FinishReason::ToolCalls, replay_state: None },
            ]
        } else {
            vec![
                StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text },
                StreamChunk::TextDelta { index: 0, text: "ok".into() },
                StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
            ]
        };
        Ok(Box::pin(stream::iter(chunks)))
    }
}

/// 一个总是成功的哑工具：证明非条件工具无条件出现在工具面上。
struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "echo".into(),
            description: "echo".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    async fn execute(&self, _input: &ToolExecutionInput) -> ToolExecutionResult {
        ToolExecutionResult::text("echo")
    }
}

/// 仅在 `mock-pro` 路由声明的工具：证条件读到本步模型选择。
struct RouteGatedTool;

#[async_trait]
impl Tool for RouteGatedTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "route-gated".into(),
            description: "route gated".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    async fn execute(&self, _input: &ToolExecutionInput) -> ToolExecutionResult {
        ToolExecutionResult::text("ok")
    }
}

fn spawn_agent(
    dir: &std::path::Path,
    script_plan_call: bool,
) -> (
    Arc<ReactLoopAgent>,
    Arc<CapturingAdapter>,
    Arc<ExitPlanModeTool>,
    Arc<ToolRegistry>,
) {
    let events = EventBus::new();
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let adapter = Arc::new(CapturingAdapter {
        requests: Mutex::new(Vec::new()),
        script_plan_call: Mutex::new(script_plan_call),
    });
    let _reg = llm
        .register_adapter(&["mock".to_string()], Arc::clone(&adapter) as Arc<dyn LlmAdapter>)
        .unwrap();
    let tools = Arc::new(ToolRegistry::new());
    let _echo = tools.register(Arc::new(EchoTool)).unwrap();
    // 路由条件工具（证条件读到本步模型选择）：仅 mock-pro 路由声明
    let _route_gated = tools
        .register_when(
            Arc::new(RouteGatedTool),
            Arc::new(|ctx: &dsh_tools::ToolAdmission| ctx.model == "mock-pro"),
        )
        .unwrap();
    let plan_tool = Arc::new(ExitPlanModeTool::new());
    // 宿主同款条件注册：首次进入 plan 前不声明，进入后保持
    let _exit = tools
        .register_when(
            Arc::clone(&plan_tool) as Arc<dyn Tool>,
            Arc::new(|ctx: &dsh_tools::ToolAdmission| ctx.plan_active || ctx.plan_entered),
        )
        .unwrap();
    let agent = ReactLoopAgent::new(
        SessionId::new(format!(
            "session-plan-admission-{}-{script_plan_call}",
            std::process::id()
        )),
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
    // 宿主同款接线：工具拿到 plan 激活态句柄
    plan_tool.set_plan_active(agent.plan_active_flag());
    agent.spawn();
    let _ = dir;
    (agent, adapter, plan_tool, tools)
}

fn request_tool_names(requests: &[GenerateOptions], index: usize) -> Vec<String> {
    requests[index]
        .tools
        .as_ref()
        .map(|tools| tools.iter().map(|t| t.name.clone()).collect())
        .unwrap_or_default()
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
                        // tool 角色消息：结果文本在 ToolResult 块的嵌套 content 里
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
                        ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn exit_plan_mode_is_admitted_progressively() {
    let dir = std::env::temp_dir().join(format!("dsh-plan-adm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (agent, adapter, _tool, _tools) = spawn_agent(&dir, false);

    // 1) 首次进入前：不声明
    agent.followup("hi");
    agent.when_idle().await;
    {
        let requests = adapter.requests.lock().unwrap();
        let names = request_tool_names(&requests, 0);
        assert!(!names.iter().any(|n| n == "exit_plan_mode"), "{names:?}");
        assert!(names.iter().any(|n| n == "echo"), "非条件工具应在场: {names:?}");
    }

    // 2) 进入 plan 模式：随声明一起出现
    agent.set_plan_mode(true);
    assert!(agent.plan_mode_active());
    agent.followup("plan");
    agent.when_idle().await;
    {
        let requests = adapter.requests.lock().unwrap();
        let names = request_tool_names(&requests, 1);
        assert!(names.iter().any(|n| n == "exit_plan_mode"), "{names:?}");
    }

    // 3) 离开 plan 模式：声明保留（渐进披露），工具仍在面上
    agent.set_plan_mode(false);
    assert!(!agent.plan_mode_active());
    agent.followup("go");
    agent.when_idle().await;
    {
        let requests = adapter.requests.lock().unwrap();
        let names = request_tool_names(&requests, 2);
        assert!(
            names.iter().any(|n| n == "exit_plan_mode"),
            "离开后仍声明（addition-only 前缀缓存语义）: {names:?}"
        );
    }
}

#[tokio::test]
async fn undelcared_plan_call_is_unknown_until_first_entry() {
    let dir = std::env::temp_dir().join(format!("dsh-plan-adm-u-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (agent, _adapter, _tool, _tools) = spawn_agent(&dir, true); // 首请求脚本化硬调用

    // 从未进入 plan 模式：工具不在声明面，硬调用按未知工具拒绝
    agent.followup("call it anyway");
    agent.when_idle().await;
    let results = tool_results(&agent);
    assert!(
        results.iter().any(|t| t.contains("unknown tool \"exit_plan_mode\"")),
        "{results:?}"
    );
}

#[tokio::test]
async fn call_after_leaving_plan_mode_fails_as_unavailable() {
    let dir = std::env::temp_dir().join(format!("dsh-plan-adm-l-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (agent, _adapter, _tool, _tools) = spawn_agent(&dir, true);

    // 进入后离开：工具保持声明（entered），调用由工具侧拒绝
    agent.set_plan_mode(true);
    agent.set_plan_mode(false);
    agent.followup("call outside plan mode");
    agent.when_idle().await;
    let results = tool_results(&agent);
    assert!(
        results.iter().any(|t| t.contains("unavailable outside plan mode")),
        "{results:?}"
    );
    assert!(
        !results.iter().any(|t| t.contains("unknown tool")),
        "已准入（entered）不应按未知工具拒绝: {results:?}"
    );
}

#[tokio::test]
async fn tool_admission_tracks_the_model_route() {
    let dir = std::env::temp_dir().join(format!("dsh-plan-adm-r-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (agent, adapter, _tool, _tools) = spawn_agent(&dir, false);

    // 默认路由（mock）：路由条件工具不声明
    agent.followup("hi");
    agent.when_idle().await;
    {
        let requests = adapter.requests.lock().unwrap();
        let names = request_tool_names(&requests, 0);
        assert!(!names.iter().any(|n| n == "route-gated"), "{names:?}");
        assert!(names.iter().any(|n| n == "echo"));
    }

    // 切到 mock-pro：下一步的声明面跟随路由条件
    agent.set_provider_and_model("mock", "mock-pro");
    agent.followup("pro");
    agent.when_idle().await;
    {
        let requests = adapter.requests.lock().unwrap();
        let names = request_tool_names(&requests, 1);
        assert!(
            names.iter().any(|n| n == "route-gated"),
            "条件按本步模型选择求值: {names:?}"
        );
    }
}
