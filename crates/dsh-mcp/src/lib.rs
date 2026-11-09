//! dsh-mcp — MCP（Model Context Protocol）客户端的 rustdsh 实现。
//!
//! 上游用官方 `@modelcontextprotocol/client` SDK；rustdsh 没有 JS 运行时，
//! 这里直接实现 **stdio 传输 + JSON-RPC 2.0**（MCP stdio 的帧格式是
//! newline-delimited JSON，每条消息一行、**不得含未转义的换行**）。
//!
//! 覆盖上游 `mcp-client` 的模型可见面：
//! - `initialize` 协商（协议版本 / capabilities / serverInfo / instructions）；
//! - `tools/list`（含 cursor 分页）；
//! - `tools/call`（每调用超时默认 60s）；
//! - 公开名 `mcp__<server>__<raw>`（按 DeepSeek 函数名契约归一，损失性归一
//!   时追加 12 位 sha256，保证不同身份不塌陷）；
//! - 结果映射：按块序转文本、resource_link 转带名称与 URI 的文本、
//!   `isError: true` → 工具错误（模型看到真失败，不是假的成功）。

pub mod bridge;
pub use bridge::{McpServer, instruction_message};

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

/// 客户端默认协议版本（上游 SDK 首选 2026-07-28，回退到受支持的旧版本）。
pub const PROTOCOL_VERSION: &str = "2026-07-28";
/// 受支持的协议版本（按新到旧；协商时取服务端回的版本，回不适应即失败）。
pub const SUPPORTED_VERSIONS: &[&str] = &["2026-07-28", "2025-06-18", "2025-03-26", "2024-11-05"];
/// 单次 `tools/call` 的默认超时（上游 `toolCallTimeoutMs` 默认 60s）。
pub const DEFAULT_TOOL_CALL_TIMEOUT_MS: u64 = 60_000;
/// 服务端 instructions 的字节上限（上游 `maxInstructionBytes` 默认 32KiB）。
pub const DEFAULT_MAX_INSTRUCTION_BYTES: usize = 32_768;

/// DeepSeek 函数名上限（64 字符）。
const MAX_PUBLIC_NAME_LENGTH: usize = 64;
/// 损失性归一后追加的 sha256 hex 长度。
const HASH_LENGTH: usize = 12;

/// 服务端暴露的一个工具（原始名 + 描述 + JSON Schema）。
#[derive(Clone, Debug, PartialEq)]
pub struct McpTool {
    /// 服务端自己的名字（只在 `tools/call` 上线时使用）。
    pub raw_name: String,
    /// 模型可见的公开名 `mcp__<server>__<raw>`。
    pub public_name: String,
    pub description: String,
    pub input_schema: Value,
}

/// 一次 `tools/call` 的模型可见结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpCallResult {
    pub text: String,
    /// 服务端报错（`isError: true`）→ 调用方按工具错误返回。
    pub is_error: bool,
}

/// 初始化结果。
#[derive(Clone, Debug, PartialEq)]
pub struct InitializeResult {
    /// 服务端回的协议版本（必须在 [`SUPPORTED_VERSIONS`] 内）。
    pub protocol_version: String,
    pub server_name: String,
    pub server_version: String,
    /// 服务端 instructions（可选；超限即拒绝连接）。
    pub instructions: Option<String>,
    /// 服务端是否声明 `tools` capability。
    pub has_tools: bool,
    /// 服务端是否声明 `resources` capability。
    pub has_resources: bool,
}

/// JSON-RPC 错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MCP error {}: {}", self.code, self.message)
    }
}

/// 公开名派生（上游 `publicToolName`）：`mcp__<server>__<raw>`，把非
/// `[A-Za-z0-9_-]` 换成 `_`；若发生替换或超出 64 字符，追加 12 位
/// `sha256("<server>\0<raw>")`——保证不同身份永不塌陷成同一个名字。
pub fn public_tool_name(server: &str, raw: &str) -> String {
    let joined = format!("mcp__{server}__{raw}");
    let normalized: String = joined
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    if normalized == joined && normalized.len() <= MAX_PUBLIC_NAME_LENGTH {
        return normalized;
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("{server}\u{0}{raw}").as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let keep = MAX_PUBLIC_NAME_LENGTH - HASH_LENGTH - 1;
    let head: String = normalized.chars().take(keep).collect();
    format!("{head}_{}", &hex[..HASH_LENGTH])
}

/// 一次 `tools/list` 的解析（含分页游标）。
fn parse_tool_list(server: &str, value: &Value) -> Result<(Vec<McpTool>, Option<String>), String> {
    let tools = value
        .get("tools")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "invalid tools/list result: missing `tools` array".to_string())?;
    let mut out: Vec<McpTool> = Vec::new();
    for tool in tools {
        let raw = tool
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "invalid tools/list result: tool without `name`".to_string())?;
        let public_name = public_tool_name(server, raw);
        if out.iter().any(|t| t.public_name == public_name) {
            // 上游：服务端列出同名工具两次 → 整个列表判为无效
            return Err(format!(
                "mcp-client({server}): server listed tool \"{raw}\" more than once — invalid tool list"
            ));
        }
        out.push(McpTool {
            raw_name: raw.to_string(),
            public_name,
            description: tool
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            input_schema: tool
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
        });
    }
    let cursor = value
        .get("nextCursor")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok((out, cursor))
}

/// `tools/call` 结果 → 模型文本（上游 `tools.ts` 的块映射）。
///
/// - `text` 块原样拼接；
/// - `resource_link` → `<name>: <uri>` 形文本；
/// - `image` → 诊断文本（rustdsh 无路由图片准入判定，先如实告知而不是静默丢）；
/// - `audio` / `resource`（内嵌资源）→ 诊断文本；
/// - 未知块 → 跳过。
pub fn render_call_content(content: &[Value]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for block in content {
        match block.get("type").and_then(|v| v.as_str()) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    parts.push(text.to_string());
                }
            }
            Some("resource_link") => {
                let name = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .or_else(|| block.get("uri").and_then(|v| v.as_str()))
                    .unwrap_or("resource");
                let uri = block.get("uri").and_then(|v| v.as_str()).unwrap_or("");
                parts.push(format!("{name}: {uri}"));
            }
            Some("image") => {
                let mime = block.get("mimeType").and_then(|v| v.as_str()).unwrap_or("image");
                parts.push(format!(
                    "[image content omitted: {mime} — this build cannot deliver MCP images to the model]"
                ));
            }
            Some("audio") => {
                let mime = block.get("mimeType").and_then(|v| v.as_str()).unwrap_or("audio");
                parts.push(format!("[audio content omitted: {mime}]"));
            }
            Some("resource") => {
                let uri = block
                    .pointer("/resource/uri")
                    .and_then(|v| v.as_str())
                    .unwrap_or("resource");
                parts.push(format!("[embedded resource omitted: {uri}]"));
            }
            _ => {}
        }
    }
    parts.join("\n")
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>>;

/// 一个 MCP 客户端连接：请求/响应按 JSON-RPC id 配对，通知单向丢弃。
pub struct McpClient {
    writer: Arc<tokio::sync::Mutex<Box<dyn AsyncWrite + Unpin + Send>>>,
    pending: Pending,
    next_id: AtomicU64,
    /// 后台读循环句柄（drop 即中止）。
    reader_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    closed: Arc<AtomicU64>,
}

impl McpClient {
    /// 用一对读写端构造（stdio 子进程，或测试里的内存对）。
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicU64::new(0));
        let reader_pending = Arc::clone(&pending);
        let reader_closed = Arc::clone(&closed);
        let task = tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let Ok(value) = serde_json::from_str::<Value>(trimmed) else { continue };
                        // 只处理响应（有 id 且有 result/error）；服务端请求与通知忽略
                        let Some(id) = value.get("id").and_then(|v| v.as_u64()) else { continue };
                        let sender = reader_pending.lock().unwrap().remove(&id);
                        let Some(sender) = sender else { continue };
                        let outcome = if let Some(error) = value.get("error") {
                            Err(RpcError {
                                code: error.get("code").and_then(|v| v.as_i64()).unwrap_or(-32603),
                                message: error
                                    .get("message")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown error")
                                    .to_string(),
                            })
                        } else {
                            Ok(value.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let _ = sender.send(outcome);
                    }
                    Ok(None) | Err(_) => {
                        reader_closed.store(1, Ordering::Relaxed);
                        // 连接断了：所有在途请求立即失败，而不是等各自的超时
                        let drained: Vec<_> = reader_pending.lock().unwrap().drain().collect();
                        for (_, sender) in drained {
                            let _ = sender.send(Err(RpcError {
                                code: -32000,
                                message: "MCP connection closed".into(),
                            }));
                        }
                        break;
                    }
                }
            }
        });
        Self {
            writer: Arc::new(tokio::sync::Mutex::new(Box::new(writer))),
            pending,
            next_id: AtomicU64::new(1),
            reader_task: Mutex::new(Some(task)),
            closed,
        }
    }

    /// 连接是否已关闭（读循环退出）。
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed) != 0
    }

    /// 发一条请求并等响应（`timeout` 到点即返回超时错误，但**不**取消对端）。
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: std::time::Duration,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let line = format!("{frame}\n");
        {
            let mut writer = self.writer.lock().await;
            if let Err(error) = writer.write_all(line.as_bytes()).await {
                // 写失败 = 连接已不可用：标记关闭，后续请求也不会白等
                self.closed.store(1, Ordering::Relaxed);
                self.pending.lock().unwrap().remove(&id);
                return Err(format!("MCP write failed: {error}"));
            }
            if let Err(error) = writer.flush().await {
                self.closed.store(1, Ordering::Relaxed);
                self.pending.lock().unwrap().remove(&id);
                return Err(format!("MCP flush failed: {error}"));
            }
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(error))) => Err(error.to_string()),
            Ok(Err(_)) => {
                self.pending.lock().unwrap().remove(&id);
                Err("MCP connection closed".into())
            }
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!("MCP request `{method}` timed out after {}ms", timeout.as_millis()))
            }
        }
    }

    /// 发一条通知（无 id、不等响应）。
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let frame = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let line = format!("{frame}\n");
        let mut writer = self.writer.lock().await;
        writer
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("MCP write failed: {e}"))?;
        writer.flush().await.map_err(|e| format!("MCP flush failed: {e}"))
    }

    /// `initialize` 握手（含 `notifications/initialized`）。
    pub async fn initialize(
        &self,
        client_name: &str,
        client_version: &str,
        max_instruction_bytes: usize,
        timeout: std::time::Duration,
    ) -> Result<InitializeResult, String> {
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "roots": { "listChanged": false } },
                    "clientInfo": { "name": client_name, "version": client_version },
                }),
                timeout,
            )
            .await?;
        let protocol_version = result
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "invalid initialize result: missing protocolVersion".to_string())?
            .to_string();
        if !SUPPORTED_VERSIONS.contains(&protocol_version.as_str()) {
            return Err(format!(
                "unsupported MCP protocol version \"{protocol_version}\" (supported: {})",
                SUPPORTED_VERSIONS.join(", ")
            ));
        }
        let instructions = result
            .get("instructions")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if let Some(text) = &instructions {
            if text.len() > max_instruction_bytes {
                return Err(format!(
                    "MCP server instructions are {} bytes, over the {max_instruction_bytes}-byte limit",
                    text.len()
                ));
            }
        }
        self.notify("notifications/initialized", json!({})).await?;
        Ok(InitializeResult {
            protocol_version,
            server_name: result
                .pointer("/serverInfo/name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            server_version: result
                .pointer("/serverInfo/version")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            instructions,
            has_tools: result.pointer("/capabilities/tools").is_some(),
            has_resources: result.pointer("/capabilities/resources").is_some(),
        })
    }

    /// `tools/list`（跟 cursor 翻页直到没有下一页）。
    pub async fn list_tools(
        &self,
        server: &str,
        timeout: std::time::Duration,
    ) -> Result<Vec<McpTool>, String> {
        let mut out: Vec<McpTool> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let result = self.request("tools/list", params, timeout).await?;
            let (mut page, next) = parse_tool_list(server, &result)?;
            for tool in page.drain(..) {
                if out.iter().any(|t| t.public_name == tool.public_name) {
                    return Err(format!(
                        "mcp-client({server}): server listed tool \"{}\" more than once — invalid tool list",
                        tool.raw_name
                    ));
                }
                out.push(tool);
            }
            match next {
                Some(next) if !next.is_empty() => cursor = Some(next),
                _ => break,
            }
        }
        Ok(out)
    }

    /// `tools/call`（原始名上线，参数原样透传）。
    pub async fn call_tool(
        &self,
        raw_name: &str,
        arguments: Value,
        timeout: std::time::Duration,
    ) -> Result<McpCallResult, String> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": raw_name, "arguments": arguments }),
                timeout,
            )
            .await?;
        let is_error = result.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);
        let content = result
            .get("content")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let text = render_call_content(&content);
        Ok(McpCallResult { text, is_error })
    }
}

impl McpClient {
    /// 立即断开（关 stdin 并中止读循环）。
    pub fn shutdown(&self) {
        if let Some(task) = self.reader_task.lock().unwrap().take() {
            task.abort();
        }
        self.closed.store(1, Ordering::Relaxed);
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        if let Some(task) = self.reader_task.lock().unwrap().take() {
            task.abort();
        }
    }
}

/// 拉起一个 stdio MCP 服务端子进程。
///
/// `env` 是**额外**环境变量；其余继承宿主环境（上游会对继承面做凭据清洗，
/// 见 backlog 偏差登记）。
/// 测试辅助：拉起一个 stdio 服务端子进程（arg 列表形式，便于 fixture 脚本）。
pub fn spawn_stdio(
    command: &str,
    args: &[String],
    env: &[(String, String)],
    cwd: Option<&std::path::Path>,
) -> Result<McpClient, String> {
    use tokio::process::Command;
    let mut cmd = Command::new(command);
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    for (key, value) in env {
        cmd.env(key, value);
    }
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    #[cfg(windows)]
    {
        // GUI 宿主无控制台可继承：不设此标志每个 MCP 服务端都会弹黑窗
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| format!("failed to spawn MCP server: {e}"))?;
    let stdout = child.stdout.take().ok_or("MCP server stdout unavailable")?;
    let stdin = child.stdin.take().ok_or("MCP server stdin unavailable")?;
    // 子进程随客户端一起活；这里把 child 挂到读循环的 drop 上
    let client = McpClient::new(stdout, stdin);
    // 保住 child：drop client 时 kill_on_drop 生效
    std::mem::forget(child);
    Ok(client)
}

/// 测试/联调辅助：`spawn_stdio(..., &[], None)` 的简写。
pub fn spawn_for_test(command: &str, args: &[String]) -> Result<McpClient, String> {
    spawn_stdio(command, args, &[], None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_name_matches_upstream_shape() {
        assert_eq!(public_tool_name("github", "create_issue"), "mcp__github__create_issue");
        // 非法字符 → 归一 + 哈希后缀（短名字不补齐到 64，只在超长时截断）
        let lossy = public_tool_name("web", "search things/v2");
        assert!(lossy.starts_with("mcp__web__search_things_v2_"), "{lossy}");
        assert_eq!(lossy.len(), "mcp__web__search_things_v2".len() + 1 + HASH_LENGTH);
        assert_eq!(public_tool_name("web", "search things/v2"), lossy, "确定性");
        // 超长 → 截断 + 哈希后缀
        let long = "x".repeat(120);
        let truncated = public_tool_name("srv", &long);
        assert_eq!(truncated.len(), MAX_PUBLIC_NAME_LENGTH);
        // 不同身份不塌陷
        assert_ne!(
            public_tool_name("a", "b/c"),
            public_tool_name("a", "b_c"),
        );
        // 两个服务端的同名工具共存
        assert_ne!(public_tool_name("github", "search"), public_tool_name("web", "search"));
    }

    #[test]
    fn tool_list_parses_and_rejects_duplicates() {
        let value = json!({
            "tools": [
                { "name": "search", "description": "find things", "inputSchema": { "type": "object" } },
                { "name": "fetch" }
            ]
        });
        let (tools, cursor) = parse_tool_list("web", &value).unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].public_name, "mcp__web__search");
        assert_eq!(tools[1].description, "");
        assert!(tools[1].input_schema.is_object(), "缺 schema 时给空对象兜底");
        assert!(cursor.is_none());

        let dup = json!({ "tools": [ { "name": "a" }, { "name": "a" } ] });
        let err = parse_tool_list("web", &dup).unwrap_err();
        assert!(err.contains("more than once — invalid tool list"), "{err}");

        let with_cursor = json!({ "tools": [], "nextCursor": "page-2" });
        assert_eq!(parse_tool_list("web", &with_cursor).unwrap().1.as_deref(), Some("page-2"));
    }

    #[test]
    fn call_content_renders_blocks_in_order() {
        let content = vec![
            json!({ "type": "text", "text": "first" }),
            json!({ "type": "resource_link", "name": "README", "uri": "file:///a.md" }),
            json!({ "type": "text", "text": "last" }),
        ];
        assert_eq!(render_call_content(&content), "first\nREADME: file:///a.md\nlast");
        // 图片/音频/内嵌资源给诊断文本而不是静默丢
        assert!(render_call_content(&[json!({ "type": "image", "mimeType": "image/png" })])
            .contains("cannot deliver MCP images"));
        assert!(render_call_content(&[json!({ "type": "audio", "mimeType": "audio/wav" })])
            .contains("audio content omitted"));
        assert!(render_call_content(&[json!({ "type": "resource", "resource": { "uri": "x://y" } })])
            .contains("embedded resource omitted"));
        // 未知块跳过
        assert_eq!(render_call_content(&[json!({ "type": "weird" })]), "");
    }
}