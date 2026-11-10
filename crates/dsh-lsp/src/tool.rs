//! 模型面的 `lsp` 工具：`ctx.lsp` 之上的只读工具。
//!
//! 对齐 [`packages/lsp/tool-lsp/src/index.ts`]：四个操作
//! （`goToDefinition`/`findReferences`/`goToImplementation`/`hover`）；把模型
//! 给的一基 UTF-16 光标坐标换算成缝的零基坐标；**要求会话工作区、无回退**；
//! 截断并渲染结果；带可配置的超时预算（`dsh-tool-call-timeout-policy` 强制执行）。
//! 运行时只注入 `tools`、`lsp`、`systemPrompt`，不导入任何 provider。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dsh_tools::{Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult};
use serde_json::{json, Value};

use crate::render::{
    format_hover, format_locations, parse_lsp_args, present_lsp_call, DEFAULT_MAX_LOCATIONS,
    DEFAULT_MAX_RESULT_CHARS,
};
use crate::types::LspQueryRequest;
use crate::{CancelToken, LspErrorCode, LspRegistry};

/// 默认工具调用超时预算（毫秒），覆盖排队中的 open/query/close 全生命周期。
pub const DEFAULT_LSP_TOOL_TIMEOUT_MS: u64 = 60_000;

/// 系统提示词里给 LSP 的定位文案（上游 `LSP_PROMPT_TEXT` 逐字）。
pub const LSP_PROMPT_TEXT: &str = "Use search/read for ordinary navigation. Use lsp when textual matches are ambiguous or before a change requires precise definitions, implementations, or references. Positions are one-based line and character (UTF-16) at the cursor; an off-symbol position may return no results. findReferences always includes the declaration.";

/// 工具配置：结果上限与超时预算（上游 `tool-lsp` 的 `Config`）。
#[derive(Clone, Debug)]
pub struct LspToolConfig {
    /// 渲染地点数上限，超出追加省略标记（默认 100）。
    pub max_locations: usize,
    /// 完整渲染结果的字符上限，含截断元数据（默认 16000）。
    pub max_result_chars: usize,
    /// 工具调用超时预算（毫秒；默认 60000）。
    pub timeout_ms: u64,
}

impl Default for LspToolConfig {
    fn default() -> Self {
        Self {
            max_locations: DEFAULT_MAX_LOCATIONS,
            max_result_chars: DEFAULT_MAX_RESULT_CHARS,
            timeout_ms: DEFAULT_LSP_TOOL_TIMEOUT_MS,
        }
    }
}

/// 模型面的 `lsp` 工具。
pub struct LspTool {
    registry: Arc<LspRegistry>,
    config: LspToolConfig,
    /// 会话工作区（上游 `workingDirectory.ensure(agent)`）：空 = 无会话
    /// 工作区，工具以 `LSP_WORKSPACE_REQUIRED` 失败（无回退）。
    workspace_root: Option<String>,
}

impl LspTool {
    /// 用缝注册表与配置构造。
    pub fn new(registry: Arc<LspRegistry>, config: LspToolConfig) -> Self {
        Self { registry, config, workspace_root: None }
    }

    /// 会话工作区（上游 `workingDirectory.ensure`）。
    pub fn with_workspace_root(mut self, root: Option<String>) -> Self {
        self.workspace_root = root;
        self
    }

    /// 工具描述（上游逐字）。
    pub fn description() -> &'static str {
        "Query a language server for precise code navigation. operation is one of goToDefinition, findReferences, goToImplementation, hover. line and character are one-based UTF-16 cursor coordinates. findReferences includes the declaration."
    }

    /// 参数 JSON schema（上游逐字段）。
    pub fn parameters() -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "operation": {
                    "type": "string",
                    "description": "goToDefinition, findReferences, goToImplementation, or hover.",
                    "enum": crate::render::LSP_OPERATIONS
                        .iter()
                        .map(|op| op.as_str())
                        .collect::<Vec<_>>(),
                },
                "file_path": {
                    "type": "string",
                    "description": "The source file to query, relative to the workspace or absolute.",
                },
                "line": {
                    "type": "number",
                    "description": "One-based line of the cursor.",
                },
                "character": {
                    "type": "number",
                    "description": "One-based UTF-16 column of the cursor.",
                },
            },
            "required": ["operation", "file_path", "line", "character"],
        })
    }
}

#[async_trait]
impl Tool for LspTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "lsp".to_string(),
            description: Self::description().to_string(),
            parameters: Self::parameters(),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        // 参数校验与一基→零基换算（上游 parseLspArgs；错误文案逐字）。
        let operation = input
            .arguments
            .get("operation")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let file_path = input
            .arguments
            .get("file_path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let line = input
            .arguments
            .get("line")
            .and_then(Value::as_f64)
            .unwrap_or_default();
        let character = input
            .arguments
            .get("character")
            .and_then(Value::as_f64)
            .unwrap_or_default();
        let parsed = match parse_lsp_args(operation, file_path, line, character) {
            Ok(parsed) => parsed,
            Err(message) => return ToolExecutionResult::error(message),
        };

        // 上游：exec.agent === undefined → LSP_WORKSPACE_REQUIRED（无回退）。
        let Some(workspace_root) = self.workspace_root.as_ref().filter(|root| !root.trim().is_empty()) else {
            return ToolExecutionResult::error(
                "the lsp tool requires a session workspace cwd",
            );
        };

        // 超时预算：预算用满时取消信号触发，查询以 'LSP query aborted' 失败。
        let signal = CancelToken::new();
        let request = LspQueryRequest {
            operation: parsed.operation,
            file_path: parsed.file_path.clone(),
            position: parsed.position,
            workspace_root: workspace_root.clone(),
            signal: Some(signal.clone()),
        };
        let mut query = Box::pin(self.registry.query(request));
        let budget = Duration::from_millis(self.config.timeout_ms.max(1));
        let outcome = tokio::select! {
            result = &mut query => result,
            _ = tokio::time::sleep(budget) => {
                signal.abort();
                // 给取消路径一点时间自然结算（拆卸会杀进程）；预算已尽，上限等它。
                match tokio::time::timeout(Duration::from_secs(2), &mut query).await {
                    Ok(result) => result,
                    Err(_) => Err(crate::LspError::new(
                        LspErrorCode::Timeout,
                        "LSP query aborted",
                    )),
                }
            }
        };
        let result = match outcome {
            Ok(result) => result,
            Err(error) => {
                // 中止与失败都以工具错误透出（模型可见、可换路）。
                return ToolExecutionResult::error(error.message);
            }
        };

        // 渲染：上游 render 回调（locations / hover 两支）。
        let (text, present) = match result {
            crate::LspQueryResult::Locations { locations, resolved_workspace_uri } => {
                let text = format_locations(
                    &locations,
                    &resolved_workspace_uri,
                    self.config.max_locations,
                    self.config.max_result_chars,
                );
                (
                    text,
                    present_lsp_call(
                        operation,
                        file_path,
                        line,
                        character,
                    ),
                )
            }
            crate::LspQueryResult::Hover { hover } => {
                let text = format_hover(hover.as_ref(), self.config.max_result_chars);
                (
                    text,
                    present_lsp_call(operation, file_path, line, character),
                )
            }
        };
        ToolExecutionResult {
            content: vec![dsh_llm::ContentBlock::text(text)],
            is_error: false,
            concludes_turn: false,
            presentation: Some(present),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_text_is_verbatim() {
        assert!(LSP_PROMPT_TEXT.starts_with("Use search/read for ordinary navigation."));
        assert!(LSP_PROMPT_TEXT.ends_with("findReferences always includes the declaration."));
    }

    #[test]
    fn defaults_match_upstream() {
        assert_eq!(DEFAULT_MAX_LOCATIONS, 100);
        assert_eq!(DEFAULT_MAX_RESULT_CHARS, 16_000);
        assert_eq!(DEFAULT_LSP_TOOL_TIMEOUT_MS, 60_000);
    }

    #[test]
    fn parameters_schema_matches_upstream() {
        let schema = LspTool::parameters();
        assert_eq!(schema["properties"]["operation"]["enum"][0], "goToDefinition");
        assert_eq!(schema["properties"]["line"]["description"], "One-based line of the cursor.");
        assert_eq!(
            schema["properties"]["character"]["description"],
            "One-based UTF-16 column of the cursor."
        );
        assert_eq!(schema["required"], json!(["operation", "file_path", "line", "character"]));
    }
}