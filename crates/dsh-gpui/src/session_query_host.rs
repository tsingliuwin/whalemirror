//! session_query 宿主面（上游 `experimental/tool-session-query` 的 rustdsh
//! 等价）：五工具（session_search / session_event_search / session_trace /
//! session_event_trace / session_event_read）+ 语料物化 + 工作区授权。
//!
//! 授权语义（上游 operations.ts）：跨会话检索强制 cwd = 调用者工作区并
//! 剔除调用者自身；单会话面缺省当前会话（事件检索封顶当前步起点之前），
//! 显式会话必须 cwd 可见。结果文本走 presentation 模板（逐字）。

use std::sync::{Arc, Mutex};

use dsh_persist::SessionRecorder;
use dsh_session::{Session, SessionEvent};
use dsh_tools::{Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult};
use serde_json::{json, Value};

use dsh_session_query::{
    availability_text, event_trace, format_event_read, format_event_search, format_event_trace,
    format_session_search, format_session_trace, read_event, search_events, search_sessions,
    session_lineage, CorpusSession, EventFilters, EventSearchRequest, SessionFilters,
    SessionSearchRequest, Surface, MAX_SEARCH_RESULTS,
};

/// 调用者材料（上游 exec.agent 的宿主等价：会话 id / cwd / 当前步起点 +
/// 当前会话的 live 语料克隆——corpus 的 live-wins 覆盖源）。
#[derive(Clone, Debug, Default)]
pub struct QueryActor {
    pub session_id: String,
    pub cwd: Option<String>,
    /// 当前 step 起点 seq（当前会话事件检索封顶在其之前——上游
    /// stepStartSeq 同语义）。
    pub step_start_seq: Option<u64>,
    /// 调用者会话的 live 语料（耐久副本可能滞后——上游 live 会话直读）。
    pub live: Option<CorpusSession>,
}

pub type QueryActorProbe = Arc<dyn Fn() -> Option<QueryActor> + Send + Sync>;

/// 共享态：耐久记录器 + 调用者探针。
pub struct QueryState {
    pub recorder: Arc<SessionRecorder>,
    actor: Mutex<Option<QueryActorProbe>>,
}

impl QueryState {
    pub fn new(recorder: Arc<SessionRecorder>) -> Self {
        Self { recorder, actor: Mutex::new(None) }
    }

    pub fn set_actor_probe(&self, probe: QueryActorProbe) {
        *self.actor.lock().unwrap() = Some(probe);
    }

    fn actor(&self) -> Result<QueryActor, String> {
        let probe = self.actor.lock().unwrap().clone();
        let probe = probe.as_ref().ok_or("session query requires a live calling agent")?;
        probe().ok_or_else(|| "session query requires a live calling agent".to_string())
    }

    /// 语料物化（上游 corpus.load 的冷读等价）：全量耐久会话 → 检索语料。
    /// 标题取 latest SessionTitle，回退首条 user 文本；created 取会话文件
    /// mtime（偏差：行级信封时间 rustdsh 内存面不承载，见 crate 注）。
    fn corpus(&self) -> Vec<CorpusSession> {
        let Ok(entries) = self.recorder.list() else {
            return Vec::new();
        };
        let mut corpus = Vec::new();
        for entry in entries {
            let Ok((session, _)) = self.recorder.load(&entry.id, entry.cwd.as_deref()) else {
                continue;
            };
            corpus.push(materialize(&entry.id.as_str().to_string(), entry.cwd.clone(), &session, entry.modified));
        }
        corpus
    }

    fn corpus_with_live(&self, live: Option<&CorpusSession>) -> Vec<CorpusSession> {
        let mut corpus = self.corpus();
        if let Some(live) = live {
            // live 当前会话覆盖耐久副本（上游 live-wins 同语义）
            corpus.retain(|s| s.id != live.id);
            corpus.insert(0, live.clone());
        }
        corpus
    }
}

pub fn materialize(id: &str, cwd: Option<String>, session: &Session, modified: std::time::SystemTime) -> CorpusSession {
    let events: Vec<(u64, SessionEvent, Option<i64>)> = session
        .entries()
        .iter()
        .map(|e| (e.seq, e.event.clone(), event_time_ms(&e.event)))
        .collect();
    let mut title = None;
    for e in session.entries() {
        if let SessionEvent::SessionTitle { title: t, .. } = &e.event {
            title = Some(t.clone());
        }
    }
    if title.is_none() {
        title = session.first_user_text();
    }
    let created_at_ms = Some(
        modified
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
    );
    CorpusSession { id: id.to_string(), cwd, title, parent_id: None, created_at_ms, events }
}

/// 事件体携带的行时间（rustdsh 仅部分行型承载；用户消息等由耐久信封
/// 承载——内存面 None，偏差已登记）。
fn event_time_ms(event: &SessionEvent) -> Option<i64> {
    // 仅四类行型承载 time_ms（turn/step 边界等由信封 time 承载——内存面
    // 丢弃，偏差登记见 crate 注）
    let ms = match event {
        SessionEvent::StepStart { time_ms, .. }
        | SessionEvent::ToolCall { time_ms, .. }
        | SessionEvent::ToolResult { time_ms, .. }
        | SessionEvent::AssistantMessage { time_ms, .. } => time_ms.as_ref()?,
        _ => return None,
    };
    Some(*ms as i64)
}

fn wire_to_surface(value: &Value) -> Option<Surface> {
    Surface::from_wire(value.as_str()?)
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn u64_bound(value: Option<&Value>) -> Option<u64> {
    value.and_then(|v| v.as_u64())
}

fn result_text(text: String) -> ToolExecutionResult {
    ToolExecutionResult::text(text)
}

fn result_error(message: String) -> ToolExecutionResult {
    ToolExecutionResult::error(message)
}

/// cwd 可见校验（上游 workspace-access：显式会话必须与调用者同工作区）。
fn ensure_cwd_visible(corpus: &[CorpusSession], session_id: &str, caller_cwd: Option<&str>) -> Result<(), String> {
    let session = corpus
        .iter()
        .find(|s| s.id == session_id)
        .ok_or_else(|| "session target is outside the caller workspace".to_string())?;
    match (session.cwd.as_deref(), caller_cwd) {
        (Some(a), Some(b)) if a == b => Ok(()),
        (None, _) => Ok(()),
        _ => Err("session target is outside the caller workspace".into()),
    }
}

/// session_search（上游 operations.searchSessions）：强制 cwd 过滤 + 剔除
/// 调用者自身；达上限附 cap 行。
pub fn op_session_search(state: &QueryState, actor: &QueryActor, args: &Value) -> ToolExecutionResult {
    let corpus = state.corpus_with_live(actor.live.as_ref());
    let filters = SessionFilters {
        session_ids: strings(args.get("session_ids")),
        cwds: actor.cwd.clone().into_iter().collect(),
        parent_session_ids: strings(args.get("parent_session_ids")),
        include_root_sessions: args
            .get("include_root_sessions")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        ..Default::default()
    };
    let request = SessionSearchRequest {
        query: args.get("query").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        session_filters: filters,
        event_filters: EventFilters {
            seq_from: u64_bound(args.get("event_seq_from")),
            seq_to: u64_bound(args.get("event_seq_to")),
            event_types: strings(args.get("event_types")),
            surfaces: strings(args.get("event_surfaces"))
                .iter()
                .filter_map(|s| wire_to_surface(&Value::from(s.as_str())))
                .collect(),
            ..Default::default()
        },
        limit: Some(MAX_SEARCH_RESULTS + 1),
    };
    let hits = match search_sessions(&corpus, &request) {
        Ok(hits) => hits,
        Err(error) => return result_error(error.message),
    };
    // 剔除调用者自身（上游 exclude caller）
    let visible: Vec<_> = hits.into_iter().filter(|h| h.session_id != actor.session_id).collect();
    let capped = visible.len() > MAX_SEARCH_RESULTS;
    let page: Vec<_> = visible.into_iter().take(MAX_SEARCH_RESULTS).collect();
    result_text(format_session_search(&page, capped))
}

/// session_event_search（上游 operations.searchEvents）：缺省当前会话且
/// 封顶当前步之前；显式会话须 cwd 可见。
pub fn op_session_event_search(state: &QueryState, actor: &QueryActor, args: &Value) -> ToolExecutionResult {
    let explicit = args.get("session_id").and_then(|v| v.as_str()).map(str::to_string);
    let (session_id, seq_cap) = match &explicit {
        Some(id) => (id.clone(), None),
        None => (actor.session_id.clone(), actor.step_start_seq),
    };
    let corpus = state.corpus_with_live(actor.live.as_ref());
    if let Err(message) = ensure_cwd_visible(&corpus, &session_id, actor.cwd.as_deref()) {
        return result_error(message);
    }
    let title = corpus.iter().find(|s| s.id == session_id).and_then(|s| s.title.clone());
    let request = EventSearchRequest {
        session_id: session_id.clone(),
        query: args.get("query").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        filters: EventFilters {
            seq_from: u64_bound(args.get("seq_from")),
            // 当前会话封顶：seq_to = min(用户上界, stepStartSeq - 1)（上游同序）
            seq_to: match (u64_bound(args.get("seq_to")), seq_cap) {
                (Some(to), Some(cap)) => Some(cap.saturating_sub(1).min(to)),
                (Some(to), None) => Some(to),
                (None, Some(cap)) => Some(cap.saturating_sub(1)),
                (None, None) => None,
            },
            event_types: strings(args.get("event_types")),
            surfaces: strings(args.get("surfaces"))
                .iter()
                .filter_map(|s| wire_to_surface(&Value::from(s.as_str())))
                .collect(),
            ..Default::default()
        },
        limit: Some(MAX_SEARCH_RESULTS + 1),
    };
    let hits = match search_events(&corpus, &request) {
        Ok(hits) => hits,
        Err(error) => return result_error(error.message),
    };
    let capped = hits.len() > MAX_SEARCH_RESULTS;
    let page: Vec<_> = hits.into_iter().take(MAX_SEARCH_RESULTS).collect();
    result_text(format_event_search(&session_id, title.as_deref(), &page, capped))
}

/// session_trace（上游 traceSession 面缺省当前）。
pub fn op_session_trace(state: &QueryState, actor: &QueryActor, args: &Value) -> ToolExecutionResult {
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| actor.session_id.clone());
    let corpus = state.corpus_with_live(actor.live.as_ref());
    if let Err(message) = ensure_cwd_visible(&corpus, &session_id, actor.cwd.as_deref()) {
        return result_error(message);
    }
    match session_lineage(&corpus, &session_id) {
        Some(lineage) => {
            let title = corpus.iter().find(|s| s.id == session_id).and_then(|s| s.title.clone());
            let _ = title;
            result_text(format_session_trace(&lineage, false))
        }
        None => result_error("session target is outside the caller workspace".into()),
    }
}

/// session_event_trace（上游 traceEvent）。
pub fn op_session_event_trace(state: &QueryState, actor: &QueryActor, args: &Value) -> ToolExecutionResult {
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| actor.session_id.clone());
    let Some(seq) = u64_bound(args.get("seq")) else {
        return result_error("invalid arguments: seq is required".into());
    };
    let corpus = state.corpus_with_live(actor.live.as_ref());
    if let Err(message) = ensure_cwd_visible(&corpus, &session_id, actor.cwd.as_deref()) {
        return result_error(message);
    }
    let Some(session) = corpus.iter().find(|s| s.id == session_id) else {
        return result_error("session target is outside the caller workspace".into());
    };
    match event_trace(session, seq) {
        Some(trace) => result_text(format_event_trace(&session_id, session.title.as_deref(), &trace)),
        None => result_error(format!("event seq {seq} not found in session {session_id}")),
    }
}

/// session_event_read（上游 readEvent）。
pub fn op_session_event_read(state: &QueryState, actor: &QueryActor, args: &Value) -> ToolExecutionResult {
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| actor.session_id.clone());
    let Some(seq) = u64_bound(args.get("seq")) else {
        return result_error("invalid arguments: seq is required".into());
    };
    let before = args.get("before").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let after = args.get("after").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let corpus = state.corpus_with_live(actor.live.as_ref());
    if let Err(message) = ensure_cwd_visible(&corpus, &session_id, actor.cwd.as_deref()) {
        return result_error(message);
    }
    let title = corpus.iter().find(|s| s.id == session_id).and_then(|s| s.title.clone());
    match read_event(&corpus, &session_id, seq, before, after) {
        Some((target_json, before_lines, after_lines)) => {
            result_text(format_event_read(&session_id, title.as_deref(), seq, &target_json, &before_lines, &after_lines))
        }
        None => result_error(format!("event seq {seq} not found in session {session_id}")),
    }
}

// ---- 五工具壳（描述逐字；schema 逐字段）----

const SESSION_SEARCH_DESCRIPTION: &str =
    "Search prior sessions in the caller workspace and return the strongest matching event from each session.";
const SESSION_EVENT_SEARCH_DESCRIPTION: &str =
    "Search prior events in one authorized session; the current session excludes the step performing this call.";
const SESSION_TRACE_DESCRIPTION: &str =
    "Read the authorized session lineage around one session, including complete visible ancestor and descendant relationships.";
const SESSION_EVENT_TRACE_DESCRIPTION: &str =
    "Read every direct replacement and relationship to a cited source event for one event in an authorized session.";
const SESSION_EVENT_READ_DESCRIPTION: &str =
    "Read one full unabridged event and optional neighboring raw-event summaries from an authorized session.";

/// 提示词节（上游 tool:session-query 逐字）。
pub const SESSION_QUERY_PROMPT_TEXT: &str = "Use session_search to find relevant work from prior sessions, or session_event_search to search earlier events in one session. Search results are cursor-free and workspace-scoped. Follow a useful hit with session_trace, session_event_trace, or session_event_read when you need lineage, relationships, or exact data.";

fn query_parameters(query_description: &str, extra: Value) -> Value {
    let mut params = json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "description": query_description },
        }
    });
    if let (Some(base), Some(extra_props)) = (params.get_mut("properties").and_then(|p| p.as_object_mut()), extra.as_object()) {
        for (key, value) in extra_props {
            base.insert(key.clone(), value.clone());
        }
    }
    params
}

macro_rules! query_tool {
    ($name:ident, $tool_name:literal, $description:expr, $op:ident, $params:expr) => {
        pub struct $name {
            state: Arc<QueryState>,
        }

        impl $name {
            pub fn new(state: Arc<QueryState>) -> Self {
                Self { state }
            }
        }

        #[async_trait::async_trait]
        impl Tool for $name {
            fn definition(&self) -> ToolDefinition {
                ToolDefinition {
                    name: $tool_name.into(),
                    description: $description.into(),
                    parameters: $params,
                }
            }

            async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
                let actor = match self.state.actor() {
                    Ok(actor) => actor,
                    Err(message) => return result_error(message),
                };
                if !actor.session_id.starts_with("session-") {
                    // 子代理拒答（本仓 id 前缀约定——跨会话检索放行与否跟随
                    // 宿主约定；子会话无 cwd 授权面）
                    return result_error(
                        "session query requires a root live calling agent".into(),
                    );
                }
                $op(&self.state, &actor, &input.arguments)
            }
        }
    };
}

query_tool!(
    SessionSearchTool,
    "session_search",
    SESSION_SEARCH_DESCRIPTION,
    op_session_search,
    query_parameters(
        "Literal full-text query over prior session history.",
        json!({
            "session_ids": { "type": "array", "items": { "type": "string" }, "description": "Optional session ids to include." },
            "parent_session_ids": { "type": "array", "items": { "type": "string" }, "description": "Optional direct parent session ids." },
            "event_seq_from": { "type": "integer", "description": "Inclusive event sequence lower bound." },
            "event_seq_to": { "type": "integer", "description": "Inclusive event sequence upper bound." },
            "event_types": { "type": "array", "items": { "type": "string" }, "description": "Event types to include." },
            "event_surfaces": { "type": "array", "items": { "type": "string" }, "description": "Event surfaces to include." },
        })
    )
);

query_tool!(
    SessionEventSearchTool,
    "session_event_search",
    SESSION_EVENT_SEARCH_DESCRIPTION,
    op_session_event_search,
    query_parameters(
        "Literal full-text query over the target session.",
        json!({
            "session_id": { "type": "string", "description": "Target session id. Omit for the current session." },
            "seq_from": { "type": "integer", "description": "Inclusive event sequence lower bound." },
            "seq_to": { "type": "integer", "description": "Inclusive event sequence upper bound." },
            "event_types": { "type": "array", "items": { "type": "string" }, "description": "Event types to include." },
            "surfaces": { "type": "array", "items": { "type": "string" }, "description": "Event surfaces to include." },
        })
    )
);

query_tool!(
    SessionTraceTool,
    "session_trace",
    SESSION_TRACE_DESCRIPTION,
    op_session_trace,
    json!({
        "type": "object",
        "properties": {
            "session_id": { "type": "string", "description": "Target session id. Omit for the current session." }
        }
    })
);

query_tool!(
    SessionEventTraceTool,
    "session_event_trace",
    SESSION_EVENT_TRACE_DESCRIPTION,
    op_session_event_trace,
    json!({
        "type": "object",
        "properties": {
            "session_id": { "type": "string", "description": "Target session id. Omit for the current session." },
            "seq": { "type": "integer", "description": "Target event sequence number." }
        }
    })
);

query_tool!(
    SessionEventReadTool,
    "session_event_read",
    SESSION_EVENT_READ_DESCRIPTION,
    op_session_event_read,
    json!({
        "type": "object",
        "properties": {
            "session_id": { "type": "string", "description": "Target session id. Omit for the current session." },
            "seq": { "type": "integer", "description": "Target event sequence number." },
            "before": { "type": "integer", "description": "Number of preceding raw events to summarize. Omit for none." },
            "after": { "type": "integer", "description": "Number of following raw events to summarize. Omit for none." }
        }
    })
);

