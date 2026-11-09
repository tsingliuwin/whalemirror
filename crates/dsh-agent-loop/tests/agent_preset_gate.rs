//! agent 预设 → 工具面门控（上游 preset 组合的 rustdsh 等价）。
//!
//! 会话绑定的预设（会话头 `agentPreset`）经 `ToolAdmission.preset` 进入
//! 条件注册：`minimal` 只声明终端（bash）与图片读取（read_image），其余
//! 工具不声明、不派发；standard/ptc/cordis 全放行；空/未知 id 按部署默认
//! （standard）语义。

use async_trait::async_trait;
use dsh_agent_loop::{AgentOptions, ReactLoopAgent};
use dsh_cordis::EventBus;
use dsh_llm::{
    BoxStream, ContentBlockType, FinishReason, GenerateOptions, LlmAdapter, LlmError,
    LlmProviderInfo, LlmRuntime, SessionId, StreamChunk,
};
use dsh_session_projection::SessionProjections;
use dsh_system_prompt::SystemPrompt;
use dsh_tools::{Tool, ToolDefinition, ToolRegistry};
use futures::stream;
use std::sync::{Arc, Mutex};

/// 记录请求并回一段文本的假适配器（工具声明在请求里，无需真回复）。
struct CapturingAdapter {
    requests: Mutex<Vec<GenerateOptions>>,
}

#[async_trait]
impl LlmAdapter for CapturingAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "mock".into(), name: "Mock".into() }
    }

    async fn stream(&self, options: GenerateOptions) -> Result<BoxStream, LlmError> {
        {
            let mut requests = self.requests.lock().unwrap();
            requests.push(options);
        }
        let chunks = vec![
            StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text },
            StreamChunk::TextDelta { index: 0, text: "ok".into() },
            StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
        ];
        Ok(Box::pin(stream::iter(chunks)))
    }
}

/// 空实现探针工具（名字即身份——只看声明与否）。
struct ProbeTool(&'static str);

#[async_trait]
impl Tool for ProbeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.0.into(),
            description: "probe".into(),
            parameters: serde_json::json!({}),
        }
    }

    async fn execute(&self, _input: &dsh_tools::ToolExecutionInput) -> dsh_tools::ToolExecutionResult {
        dsh_tools::ToolExecutionResult::text("probe")
    }
}

fn spawn_agent(preset: &str) -> (Arc<ReactLoopAgent>, Arc<CapturingAdapter>) {
    let events = EventBus::new();
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let adapter = Arc::new(CapturingAdapter { requests: Mutex::new(Vec::new()) });
    let _reg = llm
        .register_adapter(&["mock".to_string()], Arc::clone(&adapter) as Arc<dyn LlmAdapter>)
        .unwrap();

    let tools = Arc::new(ToolRegistry::new());
    // 宿主同款门控注册：条件按步骤准入的 preset 求值
    for name in ["bash", "read_image", "read", "write", "grep", "subagent"] {
        let gate_name = name;
        let _ = tools
            .register_when(
                Arc::new(ProbeTool(name)) as Arc<dyn Tool>,
                Arc::new(move |ctx: &dsh_tools::ToolAdmission| {
                    dsh_tools::preset_admits(&ctx.preset, gate_name)
                }),
            )
            .unwrap();
    }
    let agent = ReactLoopAgent::new(
        SessionId::new(format!("session-preset-gate-{preset}-{}", std::process::id())),
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
        Arc::clone(&tools),
        Arc::new(SystemPrompt::new()),
        Arc::new(SessionProjections::default()),
        events,
    );
    agent.set_agent_preset(preset);
    agent.spawn();
    (agent, adapter)
}

fn declared_tools(requests: &[GenerateOptions], index: usize) -> Vec<String> {
    requests[index]
        .tools
        .as_ref()
        .map(|tools| tools.iter().map(|t| t.name.clone()).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn minimal_declares_only_shell_and_image_read() {
    let (agent, adapter) = spawn_agent("minimal");
    agent.followup("hi");
    agent.when_idle().await;
    let requests = adapter.requests.lock().unwrap();
    assert!(!requests.is_empty(), "mock adapter captured a request");
    let names = declared_tools(&requests, requests.len() - 1);
    assert!(names.iter().any(|n| n == "bash"), "{names:?}");
    assert!(names.iter().any(|n| n == "read_image"), "{names:?}");
    for gated in ["read", "write", "grep", "subagent"] {
        assert!(!names.iter().any(|n| n == gated), "minimal 不应声明 {gated}: {names:?}");
    }
}

#[tokio::test]
async fn standard_declares_everything() {
    let (agent, adapter) = spawn_agent("standard");
    agent.followup("hi");
    agent.when_idle().await;
    let requests = adapter.requests.lock().unwrap();
    assert!(!requests.is_empty());
    let names = declared_tools(&requests, requests.len() - 1);
    for expected in ["bash", "read_image", "read", "write", "grep", "subagent"] {
        assert!(names.iter().any(|n| n == expected), "standard 应声明 {expected}: {names:?}");
    }
}

#[tokio::test]
async fn empty_preset_falls_back_to_standard_semantics() {
    let (agent, adapter) = spawn_agent("");
    agent.followup("hi");
    agent.when_idle().await;
    let requests = adapter.requests.lock().unwrap();
    assert!(!requests.is_empty());
    let names = declared_tools(&requests, requests.len() - 1);
    assert!(names.iter().any(|n| n == "write"), "空预设 = 部署默认（全放行）: {names:?}");
    assert!(names.iter().any(|n| n == "read"), "{names:?}");
}
