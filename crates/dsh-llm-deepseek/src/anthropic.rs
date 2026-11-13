//! The [`AnthropicAdapter`] transport and its SSE → [`StreamChunk`] mapper.
//!
//! Wire: Anthropic Messages（`POST {base}/v1/messages`，`x-api-key` +
//! `anthropic-version`）。与 [`crate::adapter::DeepSeekAdapter`]（OpenAI
//! Chat Completions 形）共享 provider-neutral 词汇（[`GenerateOptions`] →
//! [`StreamChunk`]）；system 走顶层 `system` 字段，tool-result 以
//! `tool_result` 块挂在 user 消息上（anthropic 原生形态——OpenAI 形下的
//! 「合成 user 图片消息」等价承载在此天然成立）。流式映射：text_delta →
//! [`StreamChunk::TextDelta`]，thinking_delta → [`StreamChunk::ReasoningDelta`]，
//! tool_use 块 → [`StreamChunk::ToolCallDelta`]（input_json_delta 聚参），
//! message_start/message_delta 的 usage → [`StreamChunk::Usage`]，
//! stop_reason → [`StreamChunk::Finish`]。

use crate::adapter::take_event;
use async_stream::stream;
use bytes::Bytes;
use dsh_llm::{
    AbortSignal, BoxStream, CallId, ContentBlock, ContentBlockType, FinishReason, GenerateOptions,
    LlmAdapter, LlmError, LlmFailure, LlmProviderInfo, StreamChunk, TokenUsage,
};
use futures::{Stream, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use std::pin::Pin;
type ByteStream = Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>>;

const ANTHROPIC_VERSION: &str = "2023-06-01";
/// anthropic `max_tokens` 是必填字段；路由元数据缺失时的安全下限。
const DEFAULT_MAX_TOKENS: u32 = 8192;

pub struct AnthropicAdapter {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    image_fetcher: Option<std::sync::Arc<dyn Fn(&dsh_llm::ImageAttachmentRef) -> Option<Vec<u8>> + Send + Sync>>,
}

impl AnthropicAdapter {
    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(90))
            .user_agent(concat!("dsh-rust/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client");
        Self {
            client,
            base_url: base_url.into(),
            api_key: api_key.into(),
            image_fetcher: None,
        }
    }

    pub fn with_image_fetcher(
        mut self,
        fetcher: std::sync::Arc<dyn Fn(&dsh_llm::ImageAttachmentRef) -> Option<Vec<u8>> + Send + Sync>,
    ) -> Self {
        self.image_fetcher = Some(fetcher);
        self
    }

    /// `{base}/v1/messages`；base 已带 `/v1/messages` 时按原样使用（聚合
    /// 代理常直接给全路径）。
    fn endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        if base.ends_with("/v1/messages") {
            base.to_string()
        } else {
            format!("{base}/v1/messages")
        }
    }

    fn classify_http_failure(status: u16, detail: &str) -> LlmFailure {
        let code = if dsh_llm::is_quota_exceeded(detail) {
            dsh_llm::QUOTA
        } else if dsh_llm::is_context_window_exceeded(detail) {
            dsh_llm::CONTEXT_WINDOW_EXCEEDED
        } else if status == 401 || status == 403 {
            dsh_llm::INVALID_CREDENTIAL
        } else if status == 429 {
            "RATE_LIMIT"
        } else if status == 408 {
            dsh_llm::TIMEOUT
        } else if (400..500).contains(&status) {
            dsh_llm::INVALID_REQUEST
        } else {
            dsh_llm::UNKNOWN
        };
        LlmFailure {
            message: detail.to_string(),
            code: code.to_string(),
            status: Some(status),
            provider_retry_after_ms: None,
            request_id: None,
            offload_images: None,
        }
    }

    fn finish_stream(failure: LlmFailure) -> BoxStream {
        Box::pin(stream! {
            yield StreamChunk::finish_error(failure);
        })
    }

    /// 请求体（anthropic wire）：system 顶层；tool-result 以 tool_result 块
    /// 挂 user 消息；assistant 的 tool_use 带 `input` 对象；tools 用
    /// `input_schema`。连续同角色消息合并（anthropic 要求交替轮）。
    fn build_body(&self, options: &GenerateOptions) -> Value {
        let mut system_parts: Vec<String> = Vec::new();
        if let Some(system) = &options.system {
            system_parts.push(system.clone());
        }

        let messages_in = dsh_llm::image_offload::project_offloaded_images(
            &options.messages,
            |a| dsh_llm::image_offload::offloaded_image_text(a, None),
        );

        let mut messages: Vec<Value> = Vec::new();
        for m in &messages_in {
            match m.role {
                dsh_llm::Role::System => {
                    if let Ok(text) = serde_json::to_string(&json!(Self::blocks_to_text(&m.content))) {
                        let _ = text;
                    }
                    system_parts.push(Self::blocks_to_text(&m.content));
                }
                dsh_llm::Role::User => {
                    let mut content = self.user_blocks(&m.content);
                    if content.is_empty() {
                        continue;
                    }
                    Self::push_message(&mut messages, "user", content);
                }
                dsh_llm::Role::Assistant => {
                    let mut content = self.assistant_blocks(&m.content);
                    if content.is_empty() {
                        continue;
                    }
                    Self::push_message(&mut messages, "assistant", content);
                }
            }
        }

        let mut body = json!({
            "model": options.model,
            "max_tokens": options.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            "stream": true,
            "messages": messages,
        });
        if !system_parts.is_empty() {
            body["system"] = Value::String(system_parts.join("\n\n"));
        }
        if let Some(tools) = &options.tools
            && !tools.is_empty()
        {
            let tools_json: Vec<Value> = tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.parameters,
                    })
                })
                .collect();
            body["tools"] = Value::Array(tools_json);
        }
        if let Some(temp) = options.temperature {
            body["temperature"] = Value::from(temp);
        }
        if let Some(stop) = &options.stop
            && let Some(first) = stop.first()
        {
            // anthropic 只收单个 stop_sequence
            body["stop_sequences"] = json!([first]);
        }
        body
    }

    /// 连续同角色消息合并：anthropic 严格要求交替轮，否则 400。
    fn push_message(messages: &mut Vec<Value>, role: &str, content: Vec<Value>) {
        if let Some(last) = messages.last_mut()
            && last.get("role").and_then(|r| r.as_str()) == Some(role)
            && let (Some(last_content), Some(new_content)) =
                (last.get_mut("content").and_then(|c| c.as_array_mut()), Some(&content))
        {
            let _ = new_content;
            for block in &content {
                last_content.push(block.clone());
            }
            return;
        }
        messages.push(json!({ "role": role, "content": content }));
    }

    fn blocks_to_text(blocks: &[ContentBlock]) -> String {
        blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn user_blocks(&self, content: &[ContentBlock]) -> Vec<Value> {
        let mut out = Vec::new();
        for b in content {
            match b {
                ContentBlock::Text { text } => out.push(json!({ "type": "text", "text": text })),
                ContentBlock::Image { attachment, .. } => {
                    self.push_image_block(&mut out, attachment);
                }
                ContentBlock::ToolResult { tool_call_id, content, is_error, .. } => {
                    let inner: Vec<Value> = content
                        .iter()
                        .filter_map(|c| match c {
                            ContentBlock::Text { text } => {
                                Some(json!({ "type": "text", "text": text }))
                            }
                            _ => None,
                        })
                        .collect();
                    let mut tr = json!({
                        "type": "tool_result",
                        "tool_use_id": tool_call_id.as_str(),
                        "content": inner,
                    });
                    if *is_error == Some(true) {
                        tr["is_error"] = json!(true);
                    }
                    out.push(tr);
                }
                _ => {}
            }
        }
        out
    }

    fn assistant_blocks(&self, content: &[ContentBlock]) -> Vec<Value> {
        let mut out = Vec::new();
        for b in content {
            match b {
                ContentBlock::Text { text } if !text.is_empty() => {
                    out.push(json!({ "type": "text", "text": text }))
                }
                ContentBlock::ToolCall { id, name, arguments } => {
                    let input: Value = serde_json::from_str(arguments)
                        .unwrap_or_else(|_| json!({}));
                    out.push(json!({
                        "type": "tool_use",
                        "id": id.as_str(),
                        "name": name,
                        "input": input,
                    }));
                }
                // reasoning 块不回传：thinking 回显要求在 thinking 开启时
                // 原样带签名回传——rustdsh 未持久化签名，v1 不开 thinking
                //（登记 anthropic thinking 支持面）。
                _ => {}
            }
        }
        out
    }

    fn push_image_block(&self, parts: &mut Vec<Value>, attachment: &dsh_llm::ImageAttachmentRef) {
        parts.push(json!({
            "type": "text",
            "text": format!(
                "{}Image {}; request preview {}x{}px.",
                if parts.is_empty() { "" } else { "\n" },
                attachment.attachment_id,
                attachment.width,
                attachment.height,
            ),
        }));
        let bytes = self.image_fetcher.as_ref().and_then(|f| f(attachment));
        match bytes {
            Some(data) => {
                use base64::Engine as _;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
                parts.push(json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": attachment.media_type,
                        "data": b64,
                    },
                }))
            }
            None => parts.push(json!({
                "type": "text",
                "text": format!("[image {} unavailable]", attachment.attachment_id),
            })),
        }
    }

    fn chunk_stream(bytes: ByteStream, signal: AbortSignal) -> BoxStream {
        Box::pin(stream! {
            let mut bytes = bytes;
            let mut buffer: Vec<u8> = Vec::new();
            let mut ctx = StreamCtx::default();
            loop {
                // Drain every complete SSE event buffered so far.
                while let Some(event) = take_event(&mut buffer) {
                    if signal.aborted() {
                        yield StreamChunk::stream_aborted();
                        return;
                    }
                    for chunk in ctx.handle_event(&event) {
                        let terminal = matches!(&chunk, StreamChunk::Finish { .. });
                        yield chunk;
                        if terminal {
                            return;
                        }
                    }
                }

                // Refill the buffer.
                match bytes.next().await {
                    Some(Ok(b)) => buffer.extend_from_slice(&b),
                    Some(Err(e)) => {
                        yield StreamChunk::finish_error(LlmFailure {
                            message: format!("transport failure: {e}"),
                            code: dsh_llm::UNKNOWN.to_string(),
                            status: None,
                            provider_retry_after_ms: None,
                            request_id: None,
                            offload_images: None,
                        });
                        return;
                    },
                    None => {
                        // EOF: finalize a trailing event that lacked a blank line.
                        if !buffer.is_empty() {
                            let rest = std::mem::take(&mut buffer);
                            for chunk in ctx.handle_event(&String::from_utf8_lossy(&rest)) {
                                let terminal = matches!(&chunk, StreamChunk::Finish { .. });
                                yield chunk;
                                if terminal {
                                    return;
                                }
                            }
                        }
                        yield ctx.final_finish();
                        return;
                    }
                }
            }
        })
    }
}

/// anthropic SSE → chunk：按 `data:` JSON 的 `type` 字段分派（event: 行
/// 与 data 行冗余，以 JSON 为准）。
#[derive(Default)]
struct StreamCtx {
    /// anthropic content_block index → (块类型, tool_use id/name 是否已发)
    blocks: HashMap<usize, (ContentBlockType, Option<CallId>, bool)>,
    input_tokens: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    output_tokens: u64,
    any_content: bool,
    stop_reason: Option<String>,
    finished: bool,
}

impl StreamCtx {
    fn handle_event(&mut self, event: &str) -> Vec<StreamChunk> {
        let mut datas: Vec<&str> = Vec::new();
        for line in event.lines() {
            let line = line.strip_prefix('\r').unwrap_or(line);
            if let Some(rest) = line.strip_prefix("data:") {
                datas.push(rest.trim_start());
            }
        }
        if datas.is_empty() {
            return Vec::new();
        }
        let data = datas.join("\n");
        let Ok(v) = serde_json::from_str::<Value>(&data) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        self.handle_chunk(&v, &mut out);
        out
    }

    fn handle_chunk(&mut self, v: &Value, out: &mut Vec<StreamChunk>) {
        match v.get("type").and_then(|t| t.as_str()) {
            Some("message_start") => {
                let message = v.get("message");
                let usage = message.and_then(|m| m.get("usage"));
                if let Some(u) = usage {
                    self.input_tokens = u.get("input_tokens").and_then(|x| x.as_u64());
                    self.cache_read = u.get("cache_read_input_tokens").and_then(|x| x.as_u64());
                    self.cache_write = u.get("cache_creation_input_tokens").and_then(|x| x.as_u64());
                    let input = self.input_tokens.unwrap_or(0);
                    out.push(StreamChunk::Usage {
                        usage: Self::map_usage(input, 0, self.cache_read, self.cache_write),
                    });
                }
            }
            Some("content_block_start") => {
                let index = v.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                let block_type = v
                    .get("content_block")
                    .and_then(|b| b.get("type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("text");
                let (kind, call_id) = match block_type {
                    "tool_use" => {
                        let id = v
                            .get("content_block")
                            .and_then(|b| b.get("id"))
                            .and_then(|x| x.as_str())
                            .map(CallId::new);
                        (ContentBlockType::ToolCall, id)
                    }
                    "thinking" => (ContentBlockType::Reasoning, None),
                    _ => (ContentBlockType::Text, None),
                };
                self.any_content = true;
                out.push(StreamChunk::BlockStart { index, block_type: kind.clone() });
                if let Some(id) = &call_id {
                    out.push(StreamChunk::ToolCallDelta {
                        index,
                        id: id.clone(),
                        name: v
                            .get("content_block")
                            .and_then(|b| b.get("name"))
                            .and_then(|x| x.as_str())
                            .map(|n| n.to_string()),
                        arguments_delta: String::new(),
                    });
                }
                self.blocks.insert(index, (kind, call_id, true));
            }
            Some("content_block_delta") => {
                let index = v.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                let delta = v.get("delta");
                let delta_type = delta.and_then(|d| d.get("type")).and_then(|t| t.as_str());
                match delta_type {
                    Some("text_delta") => {
                        if let Some(text) = delta.and_then(|d| d.get("text")).and_then(|t| t.as_str()) {
                            self.any_content = true;
                            out.push(StreamChunk::TextDelta { index, text: text.to_string() });
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(text) = delta.and_then(|d| d.get("thinking")).and_then(|t| t.as_str()) {
                            self.any_content = true;
                            out.push(StreamChunk::ReasoningDelta { index, text: text.to_string() });
                        }
                    }
                    Some("input_json_delta") => {
                        let partial = delta
                            .and_then(|d| d.get("partial_json"))
                            .and_then(|t| t.as_str())
                            .unwrap_or_default();
                        let (_, call_id, _) = self.blocks.get(&index).cloned().unwrap_or((
                            ContentBlockType::ToolCall,
                            None,
                            true,
                        ));
                        self.any_content = true;
                        out.push(StreamChunk::ToolCallDelta {
                            index,
                            id: call_id.unwrap_or_else(|| CallId::new(format!("toolu-{index}"))),
                            name: None,
                            arguments_delta: partial.to_string(),
                        });
                    }
                    _ => {}
                }
            }
            Some("content_block_stop") => {}
            Some("message_delta") => {
                if let Some(u) = v.get("usage") {
                    if let Some(o) = u.get("output_tokens").and_then(|x| x.as_u64()) {
                        self.output_tokens = o;
                    }
                }
                if let Some(reason) = v.get("delta").and_then(|d| d.get("stop_reason")).and_then(|x| x.as_str())
                    && self.stop_reason.is_none()
                {
                    self.stop_reason = Some(reason.to_string());
                }
            }
            Some("error") => {
                let message = v
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("anthropic stream error")
                    .to_string();
                if !self.finished {
                    self.finished = true;
                    out.push(StreamChunk::Finish {
                        reason: FinishReason::Error {
                            failure: LlmFailure {
                                message,
                                code: dsh_llm::UNKNOWN.to_string(),
                                status: None,
                                provider_retry_after_ms: None,
                                request_id: None,
                                offload_images: None,
                            },
                        },
                        replay_state: None,
                    });
                }
            }
            _ => {}
        }
    }

    fn map_usage(input: u64, output: u64, cache_read: Option<u64>, cache_write: Option<u64>) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
            reasoning_tokens: None,
        }
    }

    fn final_finish(&self) -> StreamChunk {
        let reason = match self.stop_reason.as_deref() {
            Some("tool_use") => FinishReason::ToolCalls,
            Some("max_tokens") => FinishReason::MaxTokens,
            _ => {
                if !self.any_content {
                    FinishReason::Error {
                        failure: LlmFailure {
                            message: "empty model response".to_string(),
                            code: dsh_llm::EMPTY_RESPONSE.to_string(),
                            status: None,
                            provider_retry_after_ms: None,
                            request_id: None,
                            offload_images: None,
                        },
                    }
                } else {
                    FinishReason::Stop
                }
            }
        };
        StreamChunk::Finish { reason, replay_state: None }
    }
}

#[async_trait::async_trait]
impl dsh_llm::LlmAdapter for AnthropicAdapter {
    fn provider_info(&self, provider: &str) -> LlmProviderInfo {
        LlmProviderInfo {
            id: provider.to_string(),
            name: provider.to_string(),
        }
    }

    async fn stream(&self, options: GenerateOptions) -> Result<BoxStream, LlmError> {
        let signal = options.signal.clone();
        let body = self.build_body(&options);
        let resp = self
            .client
            .post(self.endpoint())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::new(format!("request failed: {e}"), dsh_llm::UNKNOWN))?;

        let status = resp.status();
        if !status.is_success() {
            let status_code = status.as_u16();
            let detail = resp.text().await.unwrap_or_else(|_| format!("HTTP {status_code}"));
            return Ok(Self::finish_stream(Self::classify_http_failure(status_code, &detail)));
        }

        let bytes = Box::pin(resp.bytes_stream()) as ByteStream;
        Ok(Self::chunk_stream(bytes, signal))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_llm::{ContentBlock as CB, ImageAttachmentRef, Message, ToolSchema};

    fn adapter() -> AnthropicAdapter {
        AnthropicAdapter::with_base_url("sk-test", "https://api.anthropic.com")
    }

    fn opts(messages: Vec<Message>) -> GenerateOptions {
        GenerateOptions {
            provider: "test".into(),
            model: "claude-x".into(),
            reasoning_effort: None,
            messages,
            system: Some("You are a helpful assistant.".into()),
            tools: Some(vec![ToolSchema {
                name: "read".into(),
                description: "Read a file".into(),
                parameters: serde_json::json!({"type": "object"}),
            }]),
            temperature: None,
            max_tokens: None,
            stop: None,
            signal: dsh_llm::types::AbortSignal::new(),
            purpose: None,
            session_id: None,
        }
    }

    #[test]
    fn body_has_system_top_level_and_anthropic_tools() {
        let body = adapter().build_body(&opts(vec![]));
        assert_eq!(body["system"], "You are a helpful assistant.");
        assert_eq!(body["max_tokens"], 8192, "anthropic max_tokens 必填，缺省安全下限");
        assert_eq!(body["stream"], true);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools[0]["name"], "read");
        assert_eq!(tools[0]["input_schema"]["type"], "object", "工具 schema 落 input_schema");
        assert!(body["messages"].as_array().unwrap().is_empty());
    }

    #[test]
    fn tool_result_rides_user_message_and_tool_use_carries_object_input() {
        let messages = vec![
            Message::user(vec![CB::Text { text: "读一下".into() }]),
            Message::assistant(
                vec![CB::ToolCall {
                    id: dsh_llm::CallId::new("toolu_1"),
                    name: "read".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                }],
                "test",
                "claude-x",
            ),
            Message::user(vec![CB::ToolResult {
                tool_call_id: dsh_llm::CallId::new("toolu_1"),
                content: vec![CB::Text { text: "file body".into() }],
                is_error: Some(false),
            }]),
        ];
        let body = adapter().build_body(&opts(messages));
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3, "user → assistant(tool_use) → user(tool_result) 交替轮");
        assert_eq!(msgs[1]["content"][0]["type"], "tool_use");
        assert_eq!(msgs[1]["content"][0]["input"]["path"], "a.txt", "arguments 反序列化为 input 对象");
        assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
        assert_eq!(msgs[2]["content"][0]["tool_use_id"], "toolu_1");
        assert_eq!(msgs[2]["content"][0]["content"][0]["text"], "file body");
    }

    #[test]
    fn consecutive_same_role_messages_merge() {
        let messages = vec![
            Message::user(vec![CB::Text { text: "a".into() }]),
            Message::user(vec![CB::Text { text: "b".into() }]),
        ];
        let body = adapter().build_body(&opts(messages));
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1, "anthropic 严格交替轮——连续 user 合并");
        assert_eq!(msgs[0]["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn image_block_uses_base64_source() {
        let adapter = adapter().with_image_fetcher(std::sync::Arc::new(
            |_| Some(vec![1u8, 2, 3]),
        ));
        let attachment = ImageAttachmentRef {
            attachment_id: "img1".into(),
            name: None,
            media_type: "image/png".into(),
            bytes: 128,
            width: 10,
            height: 8,
            original_dimensions: None,
        };
        let messages = vec![Message::user(vec![CB::Image {
            attachment,
            offloaded: false,
        }])];
        let body = adapter.build_body(&opts(messages));
        let content = body["messages"][0]["content"].as_array().unwrap();
        let image = content.iter().find(|b| b["type"] == "image").unwrap();
        assert_eq!(image["source"]["type"], "base64");
        assert_eq!(image["source"]["media_type"], "image/png");
    }

    #[test]
    fn sse_maps_tool_use_block_and_stop_reason() {
        let mut ctx = StreamCtx::default();
        let events = [
            r#"data: {"type":"message_start","message":{"usage":{"input_tokens":21,"output_tokens":1}}}"#,
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_9","name":"read"}}"#,
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"pa"}}"#,
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"th\":\"a\"}"}}"#,
            r#"data: {"type":"content_block_stop","index":0}"#,
            r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":40}}"#,
        ];
        let mut chunks = Vec::new();
        for e in events {
            chunks.extend(ctx.handle_event(e));
        }
        chunks.push(ctx.final_finish());
        let block_start = chunks
            .iter()
            .position(|c| matches!(c, StreamChunk::BlockStart { block_type: ContentBlockType::ToolCall, .. }))
            .expect("tool_use BlockStart expected");
        assert_eq!(block_start, 1, "message_start 的 Usage 先于内容块");
        match &chunks[2] {
            StreamChunk::ToolCallDelta { id, name, .. } => {
                assert_eq!(id.as_str(), "toolu_9");
                assert_eq!(name.as_deref(), Some("read"));
            }
            other => panic!("expected tool call delta, got {other:?}"),
        }
        let args: String = chunks
            .iter()
            .filter_map(|c| match c {
                StreamChunk::ToolCallDelta { arguments_delta, .. } => Some(arguments_delta.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(args, r#"{"path":"a"}"#);
        assert!(chunks.iter().any(|c| matches!(c, StreamChunk::Usage { .. })));
        assert!(matches!(
            chunks.last(),
            Some(StreamChunk::Finish { reason: FinishReason::ToolCalls, .. })
        ));
    }

    #[test]
    fn sse_maps_text_thinking_and_max_tokens() {
        let mut ctx = StreamCtx::default();
        let events = [
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}"#,
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
            r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"text"}}"#,
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"hi"}}"#,
            r#"data: {"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":9}}"#,
        ];
        let mut chunks = Vec::new();
        for e in events {
            chunks.extend(ctx.handle_event(e));
        }
        chunks.push(ctx.final_finish());
        assert!(matches!(chunks[0], StreamChunk::BlockStart { block_type: ContentBlockType::Reasoning, .. }));
        assert!(chunks.iter().any(|c| matches!(c, StreamChunk::ReasoningDelta { text, .. } if text == "hmm")));
        assert!(chunks.iter().any(|c| matches!(c, StreamChunk::TextDelta { text, .. } if text == "hi")));
        assert!(matches!(
            chunks.last(),
            Some(StreamChunk::Finish { reason: FinishReason::MaxTokens, .. })
        ));
    }

    #[test]
    fn sse_error_event_yields_error_finish() {
        let mut ctx = StreamCtx::default();
        let chunks = ctx.handle_event(
            r#"data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        );
        assert!(matches!(
            chunks.first(),
            Some(StreamChunk::Finish { reason: FinishReason::Error { .. }, .. })
        ));
    }

    #[test]
    fn endpoint_appends_v1_messages_but_keeps_full_path() {
        let a = AnthropicAdapter::with_base_url("k", "https://api.anthropic.com");
        assert_eq!(a.endpoint(), "https://api.anthropic.com/v1/messages");
        let b = AnthropicAdapter::with_base_url("k", "https://proxy.example.com/v1/messages");
        assert_eq!(b.endpoint(), "https://proxy.example.com/v1/messages");
    }
}
