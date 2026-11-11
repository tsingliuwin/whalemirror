//! 语料文档面（上游 `session-query/src/documents.ts` + `extraction.ts`）：
//! 每个有非空提取文本的事件构成一份可检索文档；surface 三态
//! （current/shadowed/log-only）由压缩影子区折叠。

use dsh_session::SessionEvent;
use std::collections::HashSet;

/// 一个会话的检索语料（宿主从耐久日志物化；(seq, 事件, 可选行时间)）。
#[derive(Clone, Debug)]
pub struct CorpusSession {
    pub id: String,
    pub cwd: Option<String>,
    /// 标题（web SessionTitle latest-wins；缺 = 首条 user 文本兜底由宿主做）。
    pub title: Option<String>,
    /// 父会话 id（rustdsh 仅子代理目录承载血缘；普通会话 None）。
    pub parent_id: Option<String>,
    pub created_at_ms: Option<i64>,
    /// (seq, 事件, 行时间 unix ms——耐久信封承载；内存面缺省 None)。
    pub events: Vec<(u64, SessionEvent, Option<i64>)>,
}

/// surface 三态（上游 `classifySurface`：current | shadowed | log-only）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Current,
    Shadowed,
    LogOnly,
}

impl Surface {
    /// wire 形（上游闭集小写）。
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Shadowed => "shadowed",
            Self::LogOnly => "log-only",
        }
    }

    /// wire 形解析。
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "current" => Some(Self::Current),
            "shadowed" => Some(Self::Shadowed),
            "log-only" => Some(Self::LogOnly),
            _ => None,
        }
    }
}

/// 事件 wire 型名（与 persist KNOWN 行型同名）。
pub fn event_type_name(event: &SessionEvent) -> &'static str {
    match event {
        SessionEvent::UserMessage(_) => "user/message",
        SessionEvent::AssistantMessage { .. } => "assistant/message",
        SessionEvent::AssistantChunk { .. } => "assistant/attempt",
        SessionEvent::SystemMessage { .. } => "system/message",
        SessionEvent::ToolCall { .. } => "tool/call",
        SessionEvent::ToolResult { .. } => "tool/result",
        SessionEvent::TurnStart { .. } => "turn/start",
        SessionEvent::TurnEnd { .. } => "turn/end",
        SessionEvent::StepStart { .. } => "step/start",
        SessionEvent::StepEnd { .. } => "step/end",
        SessionEvent::RequestHeader { .. } => "request/header",
        SessionEvent::RequestContext(_) => "request/context",
        SessionEvent::SessionTitle { .. } => "session/title",
        SessionEvent::Compaction { .. } => "compaction/summary",
        SessionEvent::TodoWrite { .. } => "todo/write",
        SessionEvent::PlanMode { .. } => "plan/mode",
        SessionEvent::PermissionPreset { .. } => "permission/preset",
        SessionEvent::SandboxModeSwitch { .. } => "sandbox/mode",
        SessionEvent::ApprovalPolicy { .. } => "approval/policy",
        SessionEvent::ApprovalAsked { .. } => "approval/asked",
        SessionEvent::ApprovalDecided { .. } => "approval/decided",
        SessionEvent::JobRegistered { .. } => "job/registered",
        SessionEvent::JobSettled { .. } => "job/settled",
        SessionEvent::GoalChange { .. } => "goal/change",
        _ => "other",
    }
}

/// surface 事件闭集（上游 foldSurface 的四类 surface 行型）。
fn is_surface_type(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::UserMessage(_)
            | SessionEvent::AssistantMessage { .. }
            | SessionEvent::SystemMessage { .. }
            | SessionEvent::ToolResult { .. }
    )
}

/// 全会话 surface 折叠（上游 `foldSurface`）：压缩影子区 seq 记 shadowed；
/// 非 surface 行型一律 log-only；其余 current。
pub fn classify_surfaces(events: &[(u64, SessionEvent, Option<i64>)]) -> HashSet<u64> {
    let mut shadowed: HashSet<u64> = HashSet::new();
    for (_, event, _) in events {
        if let SessionEvent::Compaction { shadowed_seqs, .. } = event {
            shadowed.extend(shadowed_seqs.iter().copied());
        }
    }
    shadowed
}

/// 单事件的 surface（`classify_surfaces` 的查表形）。
pub fn surface_of(seq: u64, shadowed: &HashSet<u64>, event: &SessionEvent) -> Surface {
    if shadowed.contains(&seq) {
        Surface::Shadowed
    } else if is_surface_type(event) {
        Surface::Current
    } else {
        Surface::LogOnly
    }
}

/// 块级文本提取（上游块规则：text → 文本；reasoning 排除；tool-call →
/// [name, arguments]；未知块排除）。
fn block_texts(content: &[dsh_llm::ContentBlock], out: &mut Vec<String>) {
    for block in content {
        match block {
            dsh_llm::ContentBlock::Text { text } => out.push(text.clone()),
            dsh_llm::ContentBlock::ToolCall { name, arguments, .. } => {
                out.push(name.clone());
                out.push(arguments.clone());
            }
            _ => {}
        }
    }
}

/// 事件语义文本提取（上游 `extractSessionEventText` 逐形）：
/// user/assistant → 内容块文本；tool/call → [name, arguments]；
/// tool/result → 结果内容文本；todo/write → [status, content]×N；
/// turn/end → error→['error', message]、aborted→'aborted'、
/// max-tokens/interrupted→种类、completed→空；其余事件 → 空
/// （声明合并扩展行型不可检索，上游同语义）。
pub fn extract_event_text(event: &SessionEvent) -> String {
    let mut parts: Vec<String> = Vec::new();
    match event {
        SessionEvent::UserMessage(m) => block_texts(&m.content, &mut parts),
        SessionEvent::AssistantMessage { message, .. } => block_texts(&message.content, &mut parts),
        SessionEvent::ToolCall { name, arguments, .. } => {
            parts.push(name.clone());
            parts.push(arguments.clone());
        }
        SessionEvent::ToolResult { message, .. } => {
            for block in &message.content {
                if let dsh_llm::ContentBlock::Text { text } = block {
                    parts.push(text.clone());
                }
            }
        }
        SessionEvent::TodoWrite { todos } => {
            for todo in todos {
                parts.push(todo.status.clone());
                parts.push(todo.content.clone());
            }
        }
        SessionEvent::TurnEnd { reason, .. } => {
            use dsh_session::TurnEndReason as R;
            match reason {
                R::Error { failure } => {
                    parts.push("error".into());
                    parts.push(failure.message.clone());
                }
                R::Aborted { .. } => parts.push("aborted".into()),
                R::MaxTokens => parts.push("max-tokens".into()),
                R::Blocked => parts.push("blocked".into()),
                R::Completed => {}
            }
        }
        _ => {}
    }
    parts
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// 一份检索文档（上游 `SessionEventSearchDocument`：事件记录 + 提取文本）。
#[derive(Clone, Debug)]
pub struct SearchDocument {
    pub seq: u64,
    pub event_type: &'static str,
    pub time_ms: Option<i64>,
    pub surface: Surface,
    pub text: String,
}

/// 会话 → 检索文档集（上游 `buildSessionEventSearchDocuments`：空文本事件
/// 跳过——不可检索行型自然缺席）。
pub fn build_documents(session: &CorpusSession) -> Vec<SearchDocument> {
    let shadowed = classify_surfaces(&session.events);
    session
        .events
        .iter()
        .filter_map(|(seq, event, time)| {
            let text = extract_event_text(event);
            if text.is_empty() {
                return None;
            }
            Some(SearchDocument {
                seq: *seq,
                event_type: event_type_name(event),
                time_ms: *time,
                surface: surface_of(*seq, &shadowed, event),
                text,
            })
        })
        .collect()
}
