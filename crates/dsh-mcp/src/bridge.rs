//! 工具桥（上游 `mcp-client` 的 `tools.ts`）：把一个 MCP 服务端的工具表
//! 注册成 rustdsh 的原生工具。
//!
//! 命名契约（同上游）：每个 MCP 工具的身份是 `(serverName, rawName)`，模型
//! 可见名是 `mcp__<server>__<raw>`（按 DeepSeek 函数名契约归一）；**原始名只
//! 出现在 wire 上的 `tools/call`**，公开名从不反解。
//!
//! 两阶段同步（上游同构）：先取全量并按公开名建好定义，任一失败整代作废、
//! 旧工具保持有效；成功后再换掉旧注册。注册冲突（别的插件占了这个命名空间）
//! 会回滚整代，绝不留下半套工具。

use std::sync::Arc;

use async_trait::async_trait;
use dsh_llm::{ContentBlock, ToolSchema};
use dsh_tools::{Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult};
use serde_json::json;

use crate::{DEFAULT_TOOL_CALL_TIMEOUT_MS, McpClient, McpTool};

/// 一个已连接服务端的工具桥：持有客户端与它的工具表。
pub struct McpServer {
    /// 服务端名（命名空间）。
    pub name: String,
    pub client: Arc<McpClient>,
    pub tools: Vec<McpTool>,
    pub instructions: Option<String>,
    /// 每次 `tools/call` 的超时。
    pub tool_call_timeout_ms: u64,
}

impl McpServer {
    pub fn new(
        name: impl Into<String>,
        client: Arc<McpClient>,
        tools: Vec<McpTool>,
        instructions: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            client,
            tools,
            instructions,
            tool_call_timeout_ms: DEFAULT_TOOL_CALL_TIMEOUT_MS,
        }
    }

    /// 把本服务端的工具全部注册到注册表（返回 disposer 集合；调用方持有，
    /// 断开时 drop 即整代下线）。
    pub fn register(
        &self,
        registry: &dsh_tools::ToolRegistry,
    ) -> Result<Vec<dsh_llm::Disposer>, String> {
        let mut disposers: Vec<dsh_llm::Disposer> = Vec::new();
        for tool in &self.tools {
            let wrapper = Arc::new(McpToolWrapper {
                server: self.name.clone(),
                raw_name: tool.raw_name.clone(),
                definition: ToolDefinition {
                    name: tool.public_name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.input_schema.clone(),
                },
                client: Arc::clone(&self.client),
                timeout_ms: self.tool_call_timeout_ms,
            });
            match registry.register(wrapper) {
                Ok(disposer) => disposers.push(disposer),
                Err(error) => {
                    // 换代的第二阶段失败：回滚本代已注册的部分，整代作废
                    for disposer in disposers.drain(..) {
                        disposer();
                    }
                    return Err(format!(
                        "mcp-client({}): failed to register \"{}\": {error}",
                        self.name, tool.public_name
                    ));
                }
            }
        }
        Ok(disposers)
    }

    /// 本服务端的工具 schema（不含注册）。
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools
            .iter()
            .map(|tool| ToolSchema {
                name: tool.public_name.clone(),
                description: tool.description.clone(),
                parameters: tool.input_schema.clone(),
            })
            .collect()
    }
}

/// 单个 MCP 工具的原生包装。
struct McpToolWrapper {
    server: String,
    /// 服务端自己的名字（上线时用）。
    raw_name: String,
    definition: ToolDefinition,
    client: Arc<McpClient>,
    timeout_ms: u64,
}

#[async_trait]
impl Tool for McpToolWrapper {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        // 参数原样透传（模型给的就是 JSON；非法 JSON 时按空对象送出并如实报错）
        let arguments = if input.arguments.is_object() {
            input.arguments.clone()
        } else {
            return ToolExecutionResult::error(format!(
                "mcp__{}__{}: arguments must be a JSON object",
                self.server, self.raw_name
            ));
        };
        match self
            .client
            .call_tool(
                &self.raw_name,
                arguments,
                std::time::Duration::from_millis(self.timeout_ms),
            )
            .await
        {
            Ok(result) => {
                let text = if result.text.is_empty() {
                    "(no content)".to_string()
                } else {
                    result.text
                };
                if result.is_error {
                    // 服务端报错要**可见地**失败：模型不该看到假的成功
                    ToolExecutionResult::error(text)
                } else {
                    ToolExecutionResult {
                        content: vec![ContentBlock::Text { text }],
                        is_error: false,
                        concludes_turn: false,
                        presentation: Some(json!({
                            "card": "read",
                            "mcp": { "server": self.server, "tool": self.raw_name },
                        })),
                    }
                }
            }
            Err(error) => ToolExecutionResult::error(format!(
                "mcp__{}__{}: {error}",
                self.server, self.raw_name
            )),
        }
    }
}

/// 服务端 instructions 的模型面块（上游把 server instructions 作为上下文注入，
/// 带归属信息）。空 instructions 返回 None。
pub fn instruction_message(server: &str, instructions: &str) -> Option<dsh_llm::Message> {
    if instructions.trim().is_empty() {
        return None;
    }
    let text = format!(
        "<mcp_server_instructions server=\"{server}\">\n{instructions}\n</mcp_server_instructions>"
    );
    Some(dsh_llm::Message::new(
        dsh_llm::Role::User,
        vec![ContentBlock::text(text)],
        dsh_llm::MessageSource::Context {
            context_kind: "mcp-instructions".into(),
            plugin: None,
            form: Some("instructions".into()),
            summary: Some(format!("instructions from MCP server {server}")),
            changes_paths: Vec::new(),
            reference_labels: Vec::new(),
            name: Some(server.to_string()),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_message_is_attributed_and_skips_empty() {
        assert!(instruction_message("github", "   ").is_none());
        let message = instruction_message("github", "prefer small commits").unwrap();
        match &message.source {
            dsh_llm::MessageSource::Context { context_kind, name, form, .. } => {
                assert_eq!(context_kind, "mcp-instructions");
                assert_eq!(name.as_deref(), Some("github"));
                assert_eq!(form.as_deref(), Some("instructions"));
            }
            other => panic!("expected Context source, got {other:?}"),
        }
        let text = match &message.content[0] {
            ContentBlock::Text { text } => text.clone(),
            other => panic!("expected text, got {other:?}"),
        };
        assert!(text.contains("<mcp_server_instructions server=\"github\">"), "{text}");
        assert!(text.contains("prefer small commits"), "{text}");
    }

    #[tokio::test]
    async fn registry_rolls_back_the_whole_generation_on_conflict() {
        // 先占用一个公开名，再让服务端发布两个工具：整代必须作废
        let registry = dsh_tools::ToolRegistry::new();
        let squatter = registry
            .register(Arc::new(Squatter))
            .expect("squatter registers");
        let (client_side, _server_side) = tokio::io::duplex(1024);
        let (read, write) = tokio::io::split(client_side);
        let client = Arc::new(McpClient::new(read, write));
        let server = McpServer::new(
            "web",
            client,
            vec![
                McpTool {
                    raw_name: "first".into(),
                    public_name: "mcp__web__first".into(),
                    description: "ok".into(),
                    input_schema: json!({ "type": "object" }),
                },
                McpTool {
                    raw_name: "conflict".into(),
                    public_name: "squatted".into(), // 与已有注册同名
                    description: "clash".into(),
                    input_schema: json!({ "type": "object" }),
                },
            ],
            None,
        );
        // `register` 成功时返回 Disposer 集合（非 Debug），这里用 match 取错误
        let err = match server.register(&registry) {
            Ok(_) => panic!("整代应当作废"),
            Err(error) => error,
        };
        assert!(err.contains("failed to register"), "{err}");
        // 第一个工具也必须被回滚（不能留半套）
        assert!(registry.get("mcp__web__first").is_none(), "部分注册必须回滚");
        assert!(registry.get("squatted").is_some(), "原有注册不受影响");
        drop(squatter);
    }

    struct Squatter;

    #[async_trait]
    impl Tool for Squatter {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "squatted".into(),
                description: "existing".into(),
                parameters: json!({ "type": "object" }),
            }
        }

        async fn execute(&self, _input: &ToolExecutionInput) -> ToolExecutionResult {
            ToolExecutionResult::text("native")
        }
    }
}