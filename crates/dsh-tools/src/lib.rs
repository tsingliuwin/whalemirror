//! dsh-tools — the scoped tool registry and guarded execution pipeline.
//!
//! Mirrors [`packages/core/tools`](https://github.com/deepseek-ai/deepseek-harness/blob/main/packages/core/tools):
//! a tool is a JSON-schema `parameters` + async `execute`; the registry assembles
//! model-facing `ToolSchema`s and runs the guarded execution pipeline.

use async_trait::async_trait;
use dsh_llm::{CallId, ContentBlock, Disposer, ToolSchema};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

/// One tool invocation ready to execute.
#[derive(Clone, Debug)]
pub struct ToolExecutionInput {
    pub call_id: CallId,
    pub name: String,
    /// Parsed JSON arguments; preserved as the raw string body when the model
    /// emitted invalid JSON (the reference keeps invalid JSON as text).
    pub arguments: Value,
}

impl ToolExecutionInput {
    /// Parse model arguments, mapping empty input to `{}` and invalid JSON to
    /// the raw string (mirrors the reference `parseArguments`).
    pub fn with_raw_arguments(call_id: CallId, name: String, raw: String) -> Self {
        let arguments = if raw.is_empty() {
            Value::Object(Default::default())
        } else {
            serde_json::from_str(&raw).unwrap_or(Value::String(raw))
        };
        Self { call_id, name, arguments }
    }
}

/// 会话工作目录（web `session.header.cwd` 的共享句柄）：宿主在切换
/// 工作区/会话时更新，工具据此取执行目录与相对路径基准。None = 未设置，
/// 工具回退进程 cwd（与 web 的 `header.cwd ?? process.cwd()` 同语义）。
#[derive(Clone, Debug, Default)]
pub struct Workdir(Arc<std::sync::RwLock<Option<std::path::PathBuf>>>);

impl Workdir {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_value(path: impl Into<std::path::PathBuf>) -> Self {
        Self(Arc::new(std::sync::RwLock::new(Some(path.into()))))
    }

    pub fn set(&self, path: impl Into<std::path::PathBuf>) {
        *self.0.write().unwrap() = Some(path.into());
    }

    pub fn get(&self) -> Option<std::path::PathBuf> {
        self.0.read().unwrap().clone()
    }

    /// 相对路径按工作目录展开；绝对路径原样返回。
    pub fn resolve(&self, path: &std::path::Path) -> std::path::PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            match self.get() {
                Some(wd) => wd.join(path),
                None => path.to_path_buf(),
            }
        }
    }
}

/// The result of one tool invocation.
#[derive(Clone, Debug)]
pub struct ToolExecutionResult {
    pub content: Vec<ContentBlock>,
    pub is_error: bool,
    /// When true, the turn concludes after this step (the model is not asked
    /// for another step).
    pub concludes_turn: bool,
    /// UI 元数据缝（上游工具 `output.presentationMeta` 投影等价）：随 tool/result
    /// 持久化、不进模型可见文本；形态由各工具自定（如 diff 卡的
    /// `{card:"diff", diffs:[{path, oldText, newText}]}`，FileDiff 照上游
    /// presentation.ts——oldText 为 null 表示新文件/无先前内容）。
    pub presentation: Option<serde_json::Value>,
}

impl ToolExecutionResult {
    pub fn text(text: impl Into<String>) -> Self {
        Self { content: vec![ContentBlock::text(text)], is_error: false, concludes_turn: false, presentation: None }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self { content: vec![ContentBlock::text(text)], is_error: true, concludes_turn: false, presentation: None }
    }
}

/// A model-facing tool: declares a JSON-schema definition and executes calls.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The tool's name, description, and JSON-schema `parameters`.
    fn definition(&self) -> ToolDefinition;

    /// Execute one invocation.
    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult;
}

/// 审批结果闭集（上游 `interaction/user-approval` 的 `ApprovalOutcome`）。
///
/// 一次批准只作用于这一次调用（`allowed-once`），不改变会话常驻策略；
/// 无答案通道时必须 fail-closed 到 `unavailable`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// 允许这一次。
    AllowedOnce,
    /// 用户明确拒绝。
    Rejected,
    /// 请求被撤回（轮次中止 / 等待超时）。
    Cancelled,
    /// 没有可用的答案通道（fail-closed）。
    Unavailable,
}

impl ApprovalOutcome {
    /// 审计行形（上游 `approval/decided.outcome` 闭集原文）。
    pub fn as_wire(&self) -> &'static str {
        match self {
            Self::AllowedOnce => "allowed-once",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
            Self::Unavailable => "unavailable",
        }
    }
}

/// 一次审批请求（上游 `ApprovalRequestEvent` 的 rustdsh 子集）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalRequest {
    /// 请求审批的工具名（审计 `approval/asked.toolName`）。
    pub tool_name: String,
    /// 该次工具调用的 id（与 `approval/decided` 配对）。
    pub call_id: String,
    /// 审计 reason（上游 `approval/asked.reason`，如
    /// `escalate sandbox to workspace-write: <justification>`）。
    pub reason: String,
    /// 卡片正文：模型给的 justification 原文（面向用户）。
    pub display: String,
}

/// 审批回调：宿主注入（弹卡片、等用户决定），阻塞到有答案为止。
/// 工具执行线程内调用——宿主用 `spawn_blocking` 承载，避免堵住异步工作线程。
pub type ApprovalHook = Arc<dyn Fn(&ApprovalRequest) -> ApprovalOutcome + Send + Sync>;

/// 可后置注入的审批槽：工具构造时先持槽，宿主在 UI 就绪后 `set`。
/// 未接线时 `request` 返回 `Unavailable`（上游 fail-closed 同语义）。
#[derive(Clone, Default)]
pub struct ApprovalSlot(Arc<RwLock<Option<ApprovalHook>>>);

impl ApprovalSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// 宿主接线（UI 就绪后调用；重复调用以后者为准）。
    pub fn set(&self, hook: ApprovalHook) {
        *self.0.write().unwrap() = Some(hook);
    }

    /// 断开接线（宿主退出 / 会话销毁）。
    pub fn clear(&self) {
        *self.0.write().unwrap() = None;
    }

    /// 是否已接线。
    pub fn is_set(&self) -> bool {
        self.0.read().unwrap().is_some()
    }

    /// 请求一次审批；未接线 = `Unavailable`（fail-closed）。
    pub fn request(&self, req: &ApprovalRequest) -> ApprovalOutcome {
        let hook = self.0.read().unwrap().clone();
        match hook {
            Some(hook) => hook(req),
            None => ApprovalOutcome::Unavailable,
        }
    }
}

// ---- ask_user_question（上游 `interaction/tool-ask-user` legacy 阻塞形 +
// `interaction/user-questions` 服务词表的 rustdsh 等价）----

/// 一次提问里给用户的选择项（上游 `AskUserQuestionOption`）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AskUserOption {
    pub label: String,
    pub description: Option<String>,
}

/// 一次提问的单个问题（上游 `AskUserQuestionItem` 的模型面：`multi_select`
/// 蛇形——服务词表的 multiSelect 驼峰只在 timed 变体出现，rustdsh 走 legacy）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AskUserQuestion {
    pub id: String,
    pub question: String,
    pub header: Option<String>,
    pub options: Option<Vec<AskUserOption>>,
    pub multi_select: Option<bool>,
}

/// 单题答案（上游 `AskUserQuestionAnswerItem`）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AskUserAnswerItem {
    pub id: String,
    pub selected: Vec<String>,
    pub custom: Option<String>,
}

/// 一次提问请求（工具线程借出，宿主 UI 读）。
#[derive(Clone, Debug)]
pub struct AskUserRequest {
    pub call_id: String,
    pub questions: Vec<AskUserQuestion>,
}

/// 一次提问的归宿（上游 ask()/askTimed() 结果闭集的 rustdsh 等价）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AskUserResolution {
    /// 用户提交了完整答案批（每题恰一项）。
    Answered(Vec<AskUserAnswerItem>),
    /// 前台等待到期、问题仍可答（上游 `ASK_TIMED_OUT` → `{pending:true}`
    /// 映射在工具层；宿主把问题移入仍可答注册表）。
    TimedOut,
    /// 请求被撤回（轮次中止）——上游 `ASK_ABORTED`。
    Cancelled,
    /// 无可用答案通道（fail-closed）——上游 `NO_PROVIDER`。
    Unavailable,
}

/// 提问回调：宿主注入（弹提问卡、等用户提交），阻塞到有答案为止。
/// 工具执行线程内调用——宿主用独立通道 + 轮询承载（审批槽同形）。
/// `timeout_ms` = 前台等待毫秒（None = 无限等——legacy 阻塞形；到期由
/// 宿主返回 `TimedOut` 并把问题移入仍可答注册表）。
pub type AskUserHook = Arc<dyn Fn(&AskUserRequest, Option<u64>) -> AskUserResolution + Send + Sync>;

/// 可后置注入的提问槽：工具构造时先持槽，宿主 UI 就绪后 `set`。
/// 未接线时 `request` 返回 `Unavailable`（上游 NO_PROVIDER fail-closed 同语义）。
#[derive(Clone, Default)]
pub struct AskUserSlot(Arc<RwLock<Option<AskUserHook>>>);

impl AskUserSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// 宿主接线（UI 就绪后调用；重复调用以后者为准）。
    pub fn set(&self, hook: AskUserHook) {
        *self.0.write().unwrap() = Some(hook);
    }

    /// 断开接线（宿主退出 / 会话销毁）。
    pub fn clear(&self) {
        *self.0.write().unwrap() = None;
    }

    /// 是否已接线。
    pub fn is_set(&self) -> bool {
        self.0.read().unwrap().is_some()
    }

    /// 请求一次提问；未接线 = `Unavailable`（fail-closed）。
    /// `timeout_ms` 透传宿主钩子（legacy 阻塞形传 None）。
    pub fn request(&self, req: &AskUserRequest, timeout_ms: Option<u64>) -> AskUserResolution {
        let hook = self.0.read().unwrap().clone();
        match hook {
            Some(hook) => hook(req, timeout_ms),
            None => AskUserResolution::Unavailable,
        }
    }
}

/// 跳答语义说明（上游 `skippedAnswerGuidance`，逐字；legacy 与 timed 描述共享）。
pub const SKIPPED_ANSWER_GUIDANCE: &str = "An answer item with empty selected and no custom means the user skipped that question: continue without it, do not ask it again, and do not treat the skip as permission.";

/// timed 工具描述（上游 tool-ask-user/timed `description`，逐字拼接）。
pub fn ask_user_tool_description() -> String {
    format!(
        "Ask brief, direct, self-contained questions about missing information, preferences, or decisions. \
         Use user-facing terms; assume no knowledge of background work or internal names. \
         Use distinct stable question IDs. {SKIPPED_ANSWER_GUIDANCE} \
         A pending result instead means no answer batch arrived before the timeout and the user can still answer."
    )
}

/// pending 结果携带的指引（上游 `pendingNotice`，逐字；是结果 JSON 的
/// message 字段而非旁白——记录的结果按一个 JSON 对象整体读回）。
pub const ASK_USER_PENDING_NOTICE: &str = "No answer batch arrived before the timeout. This is pending, not a skipped answer. Continue useful independent work. The user can still answer; their reply will be a user message identified as answer_to_pending_question with this callId and the original questions. Do not treat this as permission.";

/// 上游错误/状态文案（`user-questions` 服务词表，逐字）。
pub const ASK_USER_ABORTED_MESSAGE: &str = "ask_user_question was aborted before the user answered";
pub const ASK_USER_EMPTY_QUESTIONS_MESSAGE: &str = "ask_user_question requires at least one question";
pub const ASK_USER_NO_PROVIDER_MESSAGE: &str = "no user-questions answerer accepted the request";
pub const ASK_USER_DELEGATED_CALLER_MESSAGE: &str = "human interaction is unavailable while the calling agent is owned by another live agent; include the unresolved question or decision in the child agent's final result";
pub const ASK_USER_TIMED_OUT_MESSAGE: &str = "ask_user_question timed out before the user answered";

/// timeout 参数上限（上游 validateTimeout：-1 或 1..=2147483 秒）。
pub const ASK_USER_TIMEOUT_MAX_SECS: i64 = 2_147_483;

/// timeout 校验（上游 `validateTimeout`，文案逐字）：`None` = -1（无限
/// 等、卡 keyed by callId），`Some(ms)` = 前台等待毫秒。
pub fn validate_timeout(timeout: i64) -> Result<Option<u64>, String> {
    if timeout != -1 && (timeout < 1 || timeout > ASK_USER_TIMEOUT_MAX_SECS) {
        return Err(
            "timeout must be -1 or a positive integer up to 2147483 seconds".into(),
        );
    }
    Ok((timeout != -1).then(|| (timeout as u64) * 1000))
}

/// 问题 id 唯一性校验（上游 `validateQuestionIds`，文案逐字）。
pub fn validate_question_ids(questions: &[AskUserQuestion]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for question in questions {
        if !seen.insert(question.id.as_str()) {
            return Err(format!(
                "question id {} must be unique within this call",
                serde_json::to_string(&question.id).unwrap_or_else(|_| "\"?\"".into())
            ));
        }
    }
    Ok(())
}

/// 通用提问工具（legacy 阻塞形）：execute 阻塞到宿主 UI 给出答案批，
/// 答案批作为普通工具结果回流模型（上游 execute 同语义）。
pub struct AskUserTool {
    slot: AskUserSlot,
    /// 子代理拒答探针（本仓约定：子会话 id 无 `session-` 前缀；上游
    /// `assertLiveRoot` 的 DELEGATED_CALLER 同语义）。None = 不检查。
    root_probe: Mutex<Option<Arc<dyn Fn() -> bool + Send + Sync>>>,
}

impl AskUserTool {
    pub fn new(slot: AskUserSlot) -> Self {
        Self { slot, root_probe: Mutex::new(None) }
    }

    /// 宿主接线根会话探针（构造后调用；接线即启用 DELEGATED_CALLER 检查）。
    pub fn set_root_probe(&self, probe: Arc<dyn Fn() -> bool + Send + Sync>) {
        *self.root_probe.lock().unwrap() = Some(probe);
    }

    pub fn slot(&self) -> &AskUserSlot {
        &self.slot
    }

    /// 从模型入参解析问题批（上游 schemastery required 面的 rustdsh 等价：
    /// 形不合法报 invalid arguments）。
    pub fn parse_questions(arguments: &Value) -> Result<Vec<AskUserQuestion>, String> {
        let Some(items) = arguments.get("questions").and_then(|q| q.as_array()) else {
            return Err("invalid arguments: questions is required".into());
        };
        items
            .iter()
            .map(|item| {
                let id = item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or("invalid arguments: questions[].id is required")?;
                let question = item
                    .get("question")
                    .and_then(|v| v.as_str())
                    .ok_or("invalid arguments: questions[].question is required")?;
                let header = item.get("header").and_then(|v| v.as_str()).map(str::to_string);
                let options = item.get("options").and_then(|v| v.as_array()).map(|opts| {
                    opts.iter()
                        .map(|o| AskUserOption {
                            label: o.get("label").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                            description: o.get("description").and_then(|v| v.as_str()).map(str::to_string),
                        })
                        .collect()
                });
                let multi_select = item.get("multi_select").and_then(|v| v.as_bool());
                Ok(AskUserQuestion {
                    id: id.to_string(),
                    question: question.to_string(),
                    header,
                    options,
                    multi_select,
                })
            })
            .collect()
    }

    /// 答案批的模型面 JSON（上游 `answerResult`：custom 缺省省略）。
    pub fn answer_result_json(items: &[AskUserAnswerItem]) -> Value {
        let answers: Vec<Value> = items
            .iter()
            .map(|a| {
                let mut o = serde_json::Map::new();
                o.insert("id".into(), json!(a.id));
                o.insert("selected".into(), json!(a.selected));
                if let Some(custom) = &a.custom {
                    o.insert("custom".into(), json!(custom));
                }
                Value::Object(o)
            })
            .collect();
        json!({ "answers": answers })
    }
}

#[async_trait]
impl Tool for AskUserTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "ask_user_question".into(),
            description: ask_user_tool_description(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "description": "Questions to ask the user.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "Stable id for this question; echoed in the answer." },
                                "question": { "type": "string", "description": "The specific question to ask the user." },
                                "header": { "type": "string", "description": "Optional short heading for the question, such as \"Confirm\" or \"Choose Mode\"." },
                                "options": {
                                    "type": "array",
                                    "description": "Optional choices to show the user. If you recommend one, put it first and append \"(Recommended)\" to that label.",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "label": { "type": "string", "description": "Short user-facing option label." },
                                            "description": { "type": "string", "description": "One sentence explaining the tradeoff or impact." }
                                        }
                                    }
                                },
                                "multi_select": { "type": "boolean", "description": "Whether the user may select more than one option. Defaults to false." }
                            }
                        }
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Wait seconds for the entire batch (default 120); omit unless the user specifies a duration. Use -1 only when an answer is required before proceeding."
                    }
                }
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let questions = match Self::parse_questions(&input.arguments) {
            Ok(q) => q,
            Err(message) => return ToolExecutionResult::error(message),
        };
        if questions.is_empty() {
            return ToolExecutionResult::error(ASK_USER_EMPTY_QUESTIONS_MESSAGE);
        }
        if let Err(message) = validate_question_ids(&questions) {
            return ToolExecutionResult::error(message);
        }
        // 子代理拒答（上游 assertLiveRoot 的 DELEGATED_CALLER；探针缺省不查）
        {
            let probe = self.root_probe.lock().unwrap().clone();
            if let Some(probe) = probe {
                if !probe() {
                    return ToolExecutionResult::error(ASK_USER_DELEGATED_CALLER_MESSAGE);
                }
            }
        }
        // timeout（上游：默认 120s；-1 = 无限等且卡 keyed by callId）
        let timeout_secs = input
            .arguments
            .get("timeout")
            .and_then(|v| v.as_i64())
            .unwrap_or(120);
        let timeout_ms = match validate_timeout(timeout_secs) {
            Ok(ms) => ms,
            Err(message) => return ToolExecutionResult::error(message),
        };
        let request = AskUserRequest {
            call_id: input.call_id.as_str().to_string(),
            questions,
        };
        match self.slot.request(&request, timeout_ms) {
            AskUserResolution::Answered(items) => {
                ToolExecutionResult::text(Self::answer_result_json(&items).to_string())
            }
            // 前台等待到期：pending 结果（问题仍可答——不是跳答；上游
            // `{pending:true, callId, message}` 逐字段）
            AskUserResolution::TimedOut => ToolExecutionResult::text(
                json!({
                    "pending": true,
                    "callId": input.call_id.as_str(),
                    "message": ASK_USER_PENDING_NOTICE,
                })
                .to_string(),
            ),
            AskUserResolution::Cancelled => ToolExecutionResult::error(ASK_USER_ABORTED_MESSAGE),
            AskUserResolution::Unavailable => {
                ToolExecutionResult::error(ASK_USER_NO_PROVIDER_MESSAGE)
            }
        }
    }
}

// ---- todo_write（上游 `todo/tool-todo`）----

/// 任务状态闭集（web wire 形 = snake_case）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    /// wire 形取值（pending / in_progress / completed）。
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
        }
    }

    /// 解析 wire 取值。
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "in_progress" => Some(Self::InProgress),
            "completed" => Some(Self::Completed),
            _ => None,
        }
    }
}

/// 一条任务（整表快照的一项）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

/// 校验并规范化整表（上游 `toTodoList` 文案逐字）：内容 trim 后非空且唯一；
/// 未开并行时至多一个 `in_progress`。
pub fn validate_todos(
    raw: &[(String, String)],
    allow_parallel: bool,
) -> Result<Vec<TodoItem>, String> {
    let mut todos: Vec<TodoItem> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut active = 0usize;
    for (content, status) in raw {
        let content = content.trim();
        if content.is_empty() {
            return Err("invalid todo: `content` must be a non-empty string".into());
        }
        if !seen.insert(content.to_string()) {
            return Err(format!("invalid todos: duplicate content {content:?}"));
        }
        let Some(status) = TodoStatus::from_wire(status) else {
            return Err(format!("invalid todos: unknown status {status:?}"));
        };
        if matches!(status, TodoStatus::InProgress) {
            active += 1;
        }
        todos.push(TodoItem { content: content.to_string(), status });
    }
    if !allow_parallel && active > 1 {
        return Err(format!(
            "invalid todos: at most one task may be in_progress (got {active})"
        ));
    }
    Ok(todos)
}

/// 工具描述（上游 describe 三段式：head + 活跃策略 + tail）。
pub fn todo_description(allow_parallel: bool) -> String {
    const HEAD: &str = "Record and update a task list to plan multi-step work and show progress; skip it for trivial single-step tasks. Add one todo per concrete step before you start. ";
    const PARALLEL: &str = "While work remains, keep the todos being worked on `in_progress`, several only when work runs in parallel. ";
    const SINGLE: &str = "While work remains, keep exactly one todo `in_progress`. ";
    const TAIL: &str = "Mark each todo `completed` as soon as it is done.";
    format!(
        "{HEAD}{}{TAIL}",
        if allow_parallel { PARALLEL } else { SINGLE }
    )
}

/// 整表持久化缝（宿主注入：把快照追加成会话的 `todo/write` 事件）。
pub type TodoSink = Arc<dyn Fn(&[TodoItem]) + Send + Sync>;

/// 模型面向的任务清单工具：每次调用整表替换，快照经 sink 落会话日志。
pub struct TodoTool {
    allow_parallel: bool,
    sink: RwLock<Option<TodoSink>>,
}

impl TodoTool {
    /// `allow_parallel` = 部署是否允许同时多个 in_progress（上游必填配置）。
    pub fn new(allow_parallel: bool) -> Self {
        Self { allow_parallel, sink: RwLock::new(None) }
    }

    /// 注入持久化缝（宿主在 agent 就绪后接线）。
    pub fn set_sink(&self, sink: TodoSink) {
        *self.sink.write().unwrap() = Some(sink);
    }
}

#[async_trait]
impl Tool for TodoTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "todo_write".into(),
            description: todo_description(self.allow_parallel),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "The COMPLETE task list, replacing any previous list.",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "content": { "type": "string", "description": "What the task is — a short imperative line." },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"],
                                    "description": "pending (not started) | in_progress (now) | completed (done)."
                                }
                            },
                            "required": ["content", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let Some(items) = input.arguments.get("todos").and_then(|v| v.as_array()) else {
            return ToolExecutionResult::error("todos must be an array");
        };
        let raw: Vec<(String, String)> = items
            .iter()
            .map(|item| {
                (
                    item.get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    item.get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                )
            })
            .collect();
        let todos = match validate_todos(&raw, self.allow_parallel) {
            Ok(todos) => todos,
            Err(text) => return ToolExecutionResult::error(text),
        };
        if let Some(sink) = self.sink.read().unwrap().as_ref() {
            sink(&todos);
        }
        let count = |status: TodoStatus| todos.iter().filter(|t| t.status == status).count();
        let (pending, in_progress, completed) = (
            count(TodoStatus::Pending),
            count(TodoStatus::InProgress),
            count(TodoStatus::Completed),
        );
        ToolExecutionResult::text(format!(
            "Updated todo list: {pending} pending, {in_progress} in progress, {completed} completed."
        ))
    }
}

/// The static part of a tool: name, description, JSON-schema parameters.
#[derive(Clone, Debug)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl ToolDefinition {
    pub fn to_schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }
}

/// 步骤级工具准入上下文（上游条件注册 `when: ({ agent, modelSelection })`
/// 的 rustdsh 等价子集：plan 折叠态 + 本步模型选择 + agent 预设）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolAdmission {
    pub plan_active: bool,
    pub plan_entered: bool,
    pub provider: String,
    pub model: String,
    /// 会话绑定的 agent 预设 id（上游 agentPreset；空 = 部署默认 standard）。
    pub preset: String,
}

/// 工具激活条件：为 false 的步骤不声明、不派发该工具（上游 conditional
/// tool 语义；调用按未知工具失败）。
pub type ToolCondition = Arc<dyn Fn(&ToolAdmission) -> bool + Send + Sync>;

/// 极简模式允许的工具名（上游 minimal 预设组合：持久终端 + read_image；
/// persona 换工程师助手句、无检索/web/子代理/计划/技能/交付）。
pub const MINIMAL_PRESET_TOOLS: &[&str] = &["bash", "read_image"];

/// agent 预设 → 工具准入策略（`agent_presets` 目录的判据源；dsh-tools
/// 不依赖上层，策略住在这里让条件注册与测试共用一份）。
pub fn preset_admits(preset: &str, tool: &str) -> bool {
    if preset == "minimal" {
        return MINIMAL_PRESET_TOOLS.contains(&tool);
    }
    true
}

impl ToolAdmission {
    /// 本步准入上下文的预设是否声明该工具（空/未知预设 = 部署默认，全放行）。
    pub fn preset_admits(&self, tool: &str) -> bool {
        preset_admits(&self.preset, tool)
    }
}

struct Registered {
    tool: Arc<dyn Tool>,
    condition: Option<ToolCondition>,
}

/// The scoped tool registry.
#[derive(Default)]
pub struct ToolRegistry {
    tools: Arc<RwLock<HashMap<String, Registered>>>,
    order: Arc<RwLock<Vec<String>>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool under its name, returning its disposer.
    pub fn register(&self, tool: Arc<dyn Tool>) -> Result<Disposer, dsh_llm::LlmError> {
        self.register_inner(tool, None)
    }

    /// 以激活条件注册（上游条件工具）：条件不满足的步骤不声明、不派发。
    pub fn register_when(
        &self,
        tool: Arc<dyn Tool>,
        condition: ToolCondition,
    ) -> Result<Disposer, dsh_llm::LlmError> {
        self.register_inner(tool, Some(condition))
    }

    fn register_inner(
        &self,
        tool: Arc<dyn Tool>,
        condition: Option<ToolCondition>,
    ) -> Result<Disposer, dsh_llm::LlmError> {
        let name = tool.definition().name.clone();
        {
            let map = self.tools.read().unwrap();
            if map.contains_key(&name) {
                return Err(dsh_llm::LlmError::new(
                    format!("tool \"{name}\" is already registered"),
                    "DUPLICATE_TOOL",
                ));
            }
        }
        self.tools
            .write()
            .unwrap()
            .insert(name.clone(), Registered { tool, condition });
        self.order.write().unwrap().push(name.clone());

        let tools = Arc::clone(&self.tools);
        let order = Arc::clone(&self.order);
        Ok(Box::new(move || {
            tools.write().unwrap().remove(&name);
            order.write().unwrap().retain(|n| n != &name);
        }))
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .read()
            .unwrap()
            .get(name)
            .map(|registered| Arc::clone(&registered.tool))
    }

    /// Every registered tool in registration order.
    pub fn list(&self) -> Vec<Arc<dyn Tool>> {
        let order = self.order.read().unwrap();
        let map = self.tools.read().unwrap();
        order
            .iter()
            .filter_map(|n| map.get(n).map(|r| Arc::clone(&r.tool)))
            .collect()
    }

    /// 本步准入的工具（注册序；无条件工具恒在，条件工具按上下文过滤）。
    pub fn list_admitted(&self, admission: &ToolAdmission) -> Vec<Arc<dyn Tool>> {
        let order = self.order.read().unwrap();
        let map = self.tools.read().unwrap();
        order
            .iter()
            .filter_map(|n| {
                let registered = map.get(n)?;
                let admitted = registered
                    .condition
                    .as_ref()
                    .map(|c| c(admission))
                    .unwrap_or(true);
                admitted.then(|| Arc::clone(&registered.tool))
            })
            .collect()
    }

    /// 单个工具的准入判定（未注册 = false；条件缺省 = true）。
    pub fn admitted(&self, name: &str, admission: &ToolAdmission) -> bool {
        let map = self.tools.read().unwrap();
        let Some(registered) = map.get(name) else {
            return false;
        };
        registered
            .condition
            .as_ref()
            .map(|c| c(admission))
            .unwrap_or(true)
    }

    /// The model-facing schemas for every registered tool.
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.list().iter().map(|t| t.definition().to_schema()).collect()
    }

    /// Run the guarded execute pipeline for one call.
    pub async fn execute(&self, input: &ToolExecutionInput) -> Option<ToolExecutionResult> {
        let tool = self.get(&input.name)?;
        Some(tool.execute(input).await)
    }
}
// ---- 行 diff（上游 DiffBlock 语义的纯函数面）----

/// 一行补丁的种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLineKind {
    /// 上下文行（中性显示，不计入增删统计——上游 review 修正语义）。
    Context,
    /// 删除行（旧文件独有）。
    Del,
    /// 新增行（新文件独有）。
    Add,
}

/// 一行补丁：种类与文本（不含行尾换行）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

/// 一个 hunk：一处改动及其两侧最多 3 行上下文；远距改动分成多个 hunk。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffHunk {
    pub lines: Vec<DiffLine>,
}

/// 增删统计（上下文行不计入——上游 diffTotals 同语义）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct DiffTotals {
    pub added: u64,
    pub deleted: u64,
}

/// 旧文本按行切分（末尾换行为终止符——与上游一致：仅末尾换行有无不同不展示）。
fn split_lines(text: &str) -> Vec<&str> {
    let trimmed = text.strip_suffix('\n').unwrap_or(text);
    if trimmed.is_empty() { Vec::new() } else { trimmed.split('\n').collect() }
}

/// 行 LCS DP 表（O(n·m)；上游 note 接受大替换的同步计算成本）。
fn lcs_table(a: &[&str], b: &[&str]) -> Vec<Vec<u32>> {
    let mut t = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            t[i][j] = if a[i] == b[j] { t[i + 1][j + 1] + 1 } else { t[i + 1][j].max(t[i][j + 1]) };
        }
    }
    t
}

fn op_lines(kind: DiffLineKind, lines: &[&str], out: &mut Vec<DiffLine>) {
    out.extend(lines.iter().map(|l| DiffLine { kind, text: (*l).to_string() }));
}

/// 生成行补丁：LCS 对齐后，把相邻的改动行聚为一个 hunk，两侧保留至多
/// [`CONTEXT_LINES`] 行上下文；相距超过 `2 * CONTEXT_LINES` 的改动分属不同 hunk。
pub fn line_patch(old: &str, new: &str) -> Vec<DiffHunk> {
    pub const CONTEXT_LINES: usize = 3;
    let a = split_lines(old);
    let b = split_lines(new);
    let t = lcs_table(&a, &b);
    // 走 LCS 得到全量操作序列（context/del/add）
    let mut ops: Vec<DiffLine> = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            op_lines(DiffLineKind::Context, &a[i..i + 1], &mut ops);
            i += 1;
            j += 1;
        } else if t[i + 1][j] >= t[i][j + 1] {
            op_lines(DiffLineKind::Del, &a[i..i + 1], &mut ops);
            i += 1;
        } else {
            op_lines(DiffLineKind::Add, &b[j..j + 1], &mut ops);
            j += 1;
        }
    }
    op_lines(DiffLineKind::Del, &a[i..], &mut ops);
    op_lines(DiffLineKind::Add, &b[j..], &mut ops);

    // 聚 hunk：改动为中心，两侧扩 CONTEXT_LINES 上下文；相邻改动区重叠则并 hunk
    let is_change = |l: &DiffLine| l.kind != DiffLineKind::Context;
    let n = ops.len();
    let mut marked = vec![false; n];
    for k in 0..n {
        if is_change(&ops[k]) {
            for m in k.saturating_sub(CONTEXT_LINES)..=(k + CONTEXT_LINES).min(n - 1) {
                marked[m] = true;
            }
        }
    }
    let mut hunks: Vec<DiffHunk> = Vec::new();
    let mut k = 0usize;
    while k < n {
        if !marked[k] {
            k += 1;
            continue;
        }
        let start = k;
        while k < n && (marked[k] || (k + 1 < n && marked[k + 1] && is_change(&ops[k + 1])))
        {
            // 连续标记区吞并；紧邻下一行是改动且已标记也吞并
            k += 1;
        }
        hunks.push(DiffHunk { lines: ops[start..k].to_vec() });
    }
    hunks
}

/// 增删统计：非上下文行计数（上游 diffTotals 同语义）。
pub fn diff_totals(old: &str, new: &str) -> DiffTotals {
    let a = split_lines(old);
    let b = split_lines(new);
    let t = lcs_table(&a, &b);
    let lcs = t[0][0] as u64;
    DiffTotals {
        deleted: a.len() as u64 - lcs,
        added: b.len() as u64 - lcs,
    }
}

#[cfg(test)]
mod diff_tests {
    use super::*;

    #[test]
    fn insertion_deletion_replacement_totals() {
        assert_eq!(diff_totals("", "a
b"), DiffTotals { added: 2, deleted: 0 });
        assert_eq!(diff_totals("a
b", ""), DiffTotals { added: 0, deleted: 2 });
        assert_eq!(diff_totals("a
b", "a
c"), DiffTotals { added: 1, deleted: 1 });
        assert_eq!(diff_totals("a
b", "a
b"), DiffTotals::default());
        // 仅末尾换行有无不同不展示（上游 content-line 规则）
        assert_eq!(diff_totals("a
", "a"), DiffTotals::default());
    }

    #[test]
    fn hunks_carry_context_and_split_at_distance() {
        // 上下文 3 行：改动两侧保留 ≤3 行中性上下文
        let old = (1..=10).map(|i| format!("l{i}")).collect::<Vec<_>>().join("
");
        let new = old.replacen("l5", "L5", 1);
        let hunks = line_patch(&old, &new);
        assert_eq!(hunks.len(), 1, "close change: single hunk");
        let (ctx, del, add) = hunks[0]
            .lines
            .iter()
            .fold((0usize, 0usize, 0usize), |(c, d, a), l| match l.kind {
                DiffLineKind::Context => (c + 1, d, a),
                DiffLineKind::Del => (c, d + 1, a),
                DiffLineKind::Add => (c, d, a + 1),
            });
        assert_eq!((ctx, del, add), (6, 1, 1), "3 context on each side + del/add pair");
        // 相距 >6 行的两处改动 → 两个 hunk
        let old2 = (1..=20).map(|i| format!("l{i}")).collect::<Vec<_>>().join("
");
        let new2 = old2.replacen("l2", "L2", 1).replacen("l18", "L18", 1);
        let hunks2 = line_patch(&old2, &new2);
        assert_eq!(hunks2.len(), 2, "distant changes split");
        // 上下文行不计入统计
        assert_eq!(diff_totals(&old, &new), DiffTotals { added: 1, deleted: 1 });
    }

    #[test]
    fn new_file_whole_content_is_added() {
        let hunks = line_patch("", "x
y");
        assert!(hunks[0].lines.iter().all(|l| l.kind == DiffLineKind::Add));
    }
}


/// 审阅决定（上游 exit_plan_mode 输出 {approved}）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanReviewDecision {
    /// 同意执行（离开 plan 模式，下一步开始执行）。
    Approve,
    /// 要求修改（保持 plan 模式；feedback 原文回给模型——空串=无反馈）。
    RequestChanges { feedback: String },
    /// 用户自行回复（dismissed：结束本轮不打扰，{approved:false}；
    /// 上游 ASK_CANCELLED → concludeTurn 同语义）。
    Dismissed,
}

impl PlanReviewDecision {
    /// approved 输出值。
    pub fn approved(&self) -> bool {
        matches!(self, PlanReviewDecision::Approve)
    }
}

/// 审阅回调（宿主注入：弹审阅面板等用户决定；阻塞至有答案）。上游
/// `userQuestions.ask` 的单进程等价——交互面归宿主。
pub type PlanReviewHook = std::sync::Arc<
    dyn Fn(&str, &dsh_llm::CallId) -> PlanReviewDecision + Send + Sync,
>;

/// `exit_plan_mode`（上游 plan-mode 包同名工具的审阅流实装）：提交校验
/// 后经审阅回调请求用户决定——Approve 输出 {approved:true}（离开 plan
/// 模式）；RequestChanges 以错误结果回模型（文案同上游 keep planning
/// 反馈路径）；Dismissed 结束本轮（concludes_turn，不打扰模型等待）。
pub struct ExitPlanModeTool {
    review: RwLock<Option<PlanReviewHook>>,
    /// plan 激活态共享句柄（宿主在 agent 构造后接线）：离开 plan 模式后
    /// 工具仍保持声明（前缀缓存），但调用以「unavailable outside plan
    /// mode」失败（上游渐进披露语义）。None = 无校验（数据面测试缺省）。
    plan_active: RwLock<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
}

impl ExitPlanModeTool {
    pub fn new() -> Self {
        Self { review: RwLock::new(None), plan_active: RwLock::new(None) }
    }

    /// 注入审阅回调（宿主 UI；无回调时自动同意——数据面测试缺省）。
    pub fn set_review_hook(&self, hook: PlanReviewHook) {
        *self.review.write().unwrap() = Some(hook);
    }

    /// 注入 plan 激活态句柄（宿主在 agent 构造后调用）。
    pub fn set_plan_active(&self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        *self.plan_active.write().unwrap() = Some(flag);
    }
}

impl Default for ExitPlanModeTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for ExitPlanModeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "exit_plan_mode".to_string(),
            description: "Present the completed plan for user review. The complete plan, as markdown, starting with a # heading that names it.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan": {
                        "type": "string",
                        "description": "The complete plan, as markdown, starting with a # heading that names it."
                    }
                },
                "required": ["plan"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        // 渐进披露：声明保留（前缀缓存友好），但 plan 模式未激活（已
        // 离开）时调用失败——上游「unavailable outside plan mode」
        if let Some(flag) = self.plan_active.read().unwrap().as_ref()
            && !flag.load(std::sync::atomic::Ordering::Relaxed)
        {
            return ToolExecutionResult::error("exit_plan_mode is unavailable outside plan mode");
        }
        let plan = input
            .arguments
            .get("plan")
            .and_then(|p| p.as_str())
            .map(|s| s.to_string());
        let Some(plan) = plan else {
            return ToolExecutionResult::error("exit_plan_mode requires a plan argument");
        };
        // 上游标题校验：非空 markdown 且以 # 标题开头
        if !plan.trim_start().starts_with('#') || plan.trim().len() < 3 {
            return ToolExecutionResult::error(
                "exit_plan_mode requires a non-empty markdown plan starting with a # heading",
            );
        }
        // 审阅流（上游 userQuestions.ask）：回调在场等用户决定；
        // 无回调（数据面/测试）自动同意
        let decision = match self.review.read().unwrap().clone() {
            Some(hook) => hook(&plan, &input.call_id),
            None => PlanReviewDecision::Approve,
        };
        match decision {
            PlanReviewDecision::Approve => ToolExecutionResult::text(
                "Plan approved — plan mode exited; carry out the plan starting with your next step.",
            ),
            PlanReviewDecision::RequestChanges { feedback } if feedback.is_empty() => {
                ToolExecutionResult::error(
                    "The user chose to keep planning; revise the plan and present it again.",
                )
            }
            PlanReviewDecision::RequestChanges { feedback } => ToolExecutionResult::error(format!(
                "The user chose to keep planning; their feedback: {feedback}"
            )),
            // 上游 dismissed：结束本轮不打扰（concludeTurn）
            PlanReviewDecision::Dismissed => ToolExecutionResult {
                concludes_turn: true,
                ..ToolExecutionResult::text(
                    "The user dismissed the plan review to reply in their own words; plan mode remains active.",
                )
            },
        }
    }
}


#[cfg(test)]
mod exit_plan_tests {
    use super::*;

    fn run(plan: &str) -> ToolExecutionResult {
        run_with(plan, PlanReviewDecision::Approve)
    }

    fn run_with(plan: &str, decision: PlanReviewDecision) -> ToolExecutionResult {
        let tool = ExitPlanModeTool::new();
        tool.set_review_hook(std::sync::Arc::new(move |_plan: &str, _call: &CallId| {
            decision.clone()
        }));
        let input = ToolExecutionInput::with_raw_arguments(
            CallId("c1".to_string()),
            "exit_plan_mode".to_string(),
            serde_json::json!({"plan": plan}).to_string(),
        );
        futures::executor::block_on(tool.execute(&input))
    }

    fn text_of(r: &ToolExecutionResult) -> String {
        r.content
            .iter()
            .find_map(|b| match b {
                dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    #[test]
    fn rejects_plan_without_hash_heading() {
        let r = run("just some notes");
        assert!(r.is_error);
        assert!(text_of(&r).contains("# heading"));
    }

    #[test]
    fn accepts_hash_headed_plan() {
        let r = run("# My Plan

step one");
        assert!(!r.is_error);
        assert!(text_of(&r).contains("Plan approved"));
    }

    #[test]
    fn approve_returns_carried_out_text() {
        let r = run_with("# Plan
step", PlanReviewDecision::Approve);
        assert!(!r.is_error);
        assert!(!r.concludes_turn);
        assert!(text_of(&r).contains("Plan approved"));
    }

    #[test]
    fn request_changes_returns_error_with_feedback() {
        let r = run_with(
            "# Plan
step",
            PlanReviewDecision::RequestChanges { feedback: "make it smaller".into() },
        );
        assert!(r.is_error);
        assert!(!r.concludes_turn);
        let text = text_of(&r);
        assert!(text.contains("keep planning"), "{text}");
        assert!(text.contains("make it smaller"), "{text}");
        let r2 = run_with(
            "# Plan
step",
            PlanReviewDecision::RequestChanges { feedback: String::new() },
        );
        let text2 = text_of(&r2);
        assert!(text2.contains("revise the plan and present it again"), "{text2}");
        assert!(!text2.contains("their feedback"), "{text2}");
    }

    #[test]
    fn dismissed_concludes_turn_without_error() {
        let r = run_with("# Plan
step", PlanReviewDecision::Dismissed);
        assert!(!r.is_error);
        assert!(r.concludes_turn, "dismissed ends the turn (upstream concludeTurn)");
        let text = text_of(&r);
        assert!(text.contains("dismissed the plan review"), "{text}");
        assert!(text.contains("plan mode remains active"), "{text}");
    }
}

#[cfg(test)]
mod ask_user_tests {
    use super::*;

    fn run_with(raw: &str, hook: Option<AskUserHook>, root_ok: bool) -> ToolExecutionResult {
        let tool = AskUserTool::new(AskUserSlot::new());
        if let Some(hook) = hook {
            tool.slot().set(hook);
        }
        tool.set_root_probe(std::sync::Arc::new(move || root_ok));
        let input = ToolExecutionInput::with_raw_arguments(
            CallId("call_ask".to_string()),
            "ask_user_question".to_string(),
            raw.to_string(),
        );
        futures::executor::block_on(tool.execute(&input))
    }

    fn text_of(r: &ToolExecutionResult) -> String {
        r.content
            .iter()
            .find_map(|b| match b {
                dsh_llm::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    const QUESTION_ARGS: &str = r#"{"questions":[{"id":"q1","question":"用哪个方案？","header":"选择","options":[{"label":"方案 A","description":"快"},{"label":"方案 B"}],"multi_select":false}]}"#;

    #[test]
    fn description_carries_skipped_answer_guidance_verbatim() {
        let tool = AskUserTool::new(AskUserSlot::new());
        // timed 描述（上游 timed.ts 逐字三段）
        assert!(tool
            .definition()
            .description
            .starts_with("Ask brief, direct, self-contained questions about missing information, preferences, or decisions."));
        assert!(tool
            .definition()
            .description
            .contains("A pending result instead means no answer batch arrived before the timeout and the user can still answer."));
        assert!(tool
            .definition()
            .description
            .contains("continue without it, do not ask it again, and do not treat the skip as permission."));
    }

    #[test]
    fn unwired_slot_fails_closed_with_no_provider_message() {
        let r = run_with(QUESTION_ARGS, None, true);
        assert!(r.is_error);
        assert_eq!(text_of(&r), "no user-questions answerer accepted the request");
    }

    #[test]
    fn empty_questions_rejected_verbatim() {
        let r = run_with(r#"{"questions":[]}"#, None, true);
        assert!(r.is_error);
        assert_eq!(text_of(&r), "ask_user_question requires at least one question");
    }

    #[test]
    fn missing_questions_rejected_as_invalid_arguments() {
        let r = run_with("{}", None, true);
        assert!(r.is_error);
        assert!(text_of(&r).starts_with("invalid arguments: questions is required"));
    }

    #[test]
    fn delegated_caller_refused_verbatim() {
        let r = run_with(QUESTION_ARGS, None, false);
        assert!(r.is_error);
        assert!(text_of(&r).starts_with("human interaction is unavailable while the calling agent is owned by another live agent"));
    }

    #[test]
    fn answered_batch_flows_back_as_result_json() {
        let tool = AskUserTool::new(AskUserSlot::new());
        tool.slot().set(std::sync::Arc::new(|req: &AskUserRequest, _: Option<u64>| {
            // 请求形断言：call id + 问题字段原样借出
            assert_eq!(req.call_id, "call_ask");
            assert_eq!(req.questions.len(), 1);
            let q = &req.questions[0];
            assert_eq!(q.id, "q1");
            assert_eq!(q.question, "用哪个方案？");
            assert_eq!(q.header.as_deref(), Some("选择"));
            assert_eq!(q.multi_select, Some(false));
            let opts = q.options.as_ref().unwrap();
            assert_eq!(opts[0].label, "方案 A");
            assert_eq!(opts[0].description.as_deref(), Some("快"));
            assert_eq!(opts[1].label, "方案 B");
            assert_eq!(opts[1].description, None);
            AskUserResolution::Answered(vec![
                AskUserAnswerItem { id: "q1".into(), selected: vec!["方案 A".into()], custom: None },
            ])
        }));
        let input = ToolExecutionInput::with_raw_arguments(
            CallId("call_ask".to_string()),
            "ask_user_question".to_string(),
            QUESTION_ARGS.to_string(),
        );
        let r = futures::executor::block_on(tool.execute(&input));
        assert!(!r.is_error);
        assert_eq!(
            text_of(&r),
            r#"{"answers":[{"id":"q1","selected":["方案 A"]}]}"#
        );
    }

    #[test]
    fn cancelled_resolution_maps_to_aborted_message() {
        let tool = AskUserTool::new(AskUserSlot::new());
        tool.slot()
            .set(std::sync::Arc::new(|_: &AskUserRequest, _: Option<u64>| AskUserResolution::Cancelled));
        let input = ToolExecutionInput::with_raw_arguments(
            CallId("c".to_string()),
            "ask_user_question".to_string(),
            QUESTION_ARGS.to_string(),
        );
        let r = futures::executor::block_on(tool.execute(&input));
        assert!(r.is_error);
        assert_eq!(text_of(&r), "ask_user_question was aborted before the user answered");
    }

    #[test]
    fn custom_answer_rides_the_batch() {
        assert_eq!(
            AskUserTool::answer_result_json(&[AskUserAnswerItem {
                id: "q1".into(),
                selected: vec![],
                custom: Some("自己写".into()),
            }])
            .to_string(),
            r#"{"answers":[{"id":"q1","selected":[],"custom":"自己写"}]}"#
        );
    }
}

#[cfg(test)]
mod registry_admission_tests {
    use super::*;

    struct NamedTool(&'static str);

    #[async_trait::async_trait]
    impl Tool for NamedTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.0.into(),
                description: "test".into(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            }
        }

        async fn execute(&self, _input: &ToolExecutionInput) -> ToolExecutionResult {
            ToolExecutionResult::text("ok")
        }
    }

    fn names(tools: Vec<Arc<dyn Tool>>) -> Vec<String> {
        tools.iter().map(|t| t.definition().name.clone()).collect()
    }

    #[test]
    fn unconditional_tools_are_always_admitted() {
        let registry = ToolRegistry::new();
        let _a = registry.register(Arc::new(NamedTool("always"))).unwrap();
        let ctx = ToolAdmission::default();
        assert_eq!(names(registry.list_admitted(&ctx)), vec!["always"]);
        assert!(registry.admitted("always", &ctx));
        assert!(registry.schemas().iter().any(|s| s.name == "always"));
    }

    #[test]
    fn conditional_tools_follow_the_admission_context() {
        let registry = ToolRegistry::new();
        let _plain = registry.register(Arc::new(NamedTool("plain"))).unwrap();
        let _gated = registry
            .register_when(
                Arc::new(NamedTool("gated")),
                Arc::new(|ctx: &ToolAdmission| ctx.plan_active),
            )
            .unwrap();
        let _model_gated = registry
            .register_when(
                Arc::new(NamedTool("model-gated")),
                Arc::new(|ctx: &ToolAdmission| ctx.model == "pro"),
            )
            .unwrap();

        // 默认上下文：条件工具缺席
        let ctx = ToolAdmission::default();
        assert_eq!(names(registry.list_admitted(&ctx)), vec!["plain"]);
        assert!(!registry.admitted("gated", &ctx));
        assert!(!registry.admitted("model-gated", &ctx));
        // 未注册的名字不是准入
        assert!(!registry.admitted("missing", &ctx));

        // plan 激活：plan 条件工具入场（entered 单独不满足 active 条件）
        let ctx = ToolAdmission { plan_active: true, ..Default::default() };
        assert_eq!(names(registry.list_admitted(&ctx)), vec!["plain", "gated"]);
        let ctx = ToolAdmission { plan_entered: true, ..Default::default() };
        assert!(!registry.admitted("gated", &ctx), "context 只携带事实，条件自行判断");

        // 路由条件：模型名来自上下文
        let ctx = ToolAdmission { model: "pro".into(), provider: "p".into(), ..Default::default() };
        assert!(registry.admitted("model-gated", &ctx));
        assert_eq!(names(registry.list_admitted(&ctx)), vec!["plain", "model-gated"]);
    }

    #[test]
    fn disposer_removes_conditional_registrations_too() {
        let registry = ToolRegistry::new();
        let disposer = registry
            .register_when(
                Arc::new(NamedTool("temp")),
                Arc::new(|ctx: &ToolAdmission| ctx.plan_active),
            )
            .unwrap();
        let ctx = ToolAdmission { plan_active: true, ..Default::default() };
        assert_eq!(names(registry.list_admitted(&ctx)), vec!["temp"]);
        disposer();
        assert!(registry.list_admitted(&ctx).is_empty());
        assert!(registry.get("temp").is_none());
    }
}

#[cfg(test)]
mod todo_tests {
    use super::*;

    fn raw(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items.iter().map(|(c, s)| (c.to_string(), s.to_string())).collect()
    }

    #[test]
    fn validation_trims_dedupes_and_pins_one_active_task() {
        let ok = validate_todos(&raw(&[("  写测试  ", "in_progress"), ("跑回归", "pending")]), false).unwrap();
        assert_eq!(ok[0].content, "写测试");
        assert_eq!(ok[0].status, TodoStatus::InProgress);

        assert_eq!(
            validate_todos(&raw(&[("  ", "pending")]), false).unwrap_err(),
            "invalid todo: `content` must be a non-empty string"
        );
        assert_eq!(
            validate_todos(&raw(&[("a", "pending"), (" a ", "completed")]), false).unwrap_err(),
            "invalid todos: duplicate content \"a\""
        );
        assert_eq!(
            validate_todos(&raw(&[("a", "in_progress"), ("b", "in_progress")]), false).unwrap_err(),
            "invalid todos: at most one task may be in_progress (got 2)"
        );
        // 并行部署允许两个活跃
        assert!(validate_todos(&raw(&[("a", "in_progress"), ("b", "in_progress")]), true).is_ok());
        assert!(validate_todos(&raw(&[("a", "bogus")]), false).is_err());
    }

    #[test]
    fn description_switches_the_active_clause_only() {
        assert!(todo_description(false).contains("keep exactly one todo `in_progress`"));
        assert!(todo_description(true).contains("several only when work runs in parallel"));
        assert!(todo_description(false).starts_with("Record and update a task list"));
        assert!(todo_description(true).ends_with("Mark each todo `completed` as soon as it is done."));
    }

    #[tokio::test]
    async fn execute_persists_the_whole_list_and_reports_counts() {
        let tool = TodoTool::new(false);
        let seen = Arc::new(std::sync::Mutex::new(Vec::<Vec<TodoItem>>::new()));
        let seen_sink = Arc::clone(&seen);
        tool.set_sink(Arc::new(move |items: &[TodoItem]| {
            seen_sink.lock().unwrap().push(items.to_vec());
        }));

        let r = tool
            .execute(&ToolExecutionInput::with_raw_arguments(
                dsh_llm::CallId("c1".into()),
                "todo_write".into(),
                r#"{"todos":[{"content":"读代码","status":"completed"},{"content":"写实现","status":"in_progress"},{"content":"跑测试","status":"pending"}]}"#.into(),
            ))
            .await;
        assert!(!r.is_error);
        let text = r.content.iter().find_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        }).unwrap();
        assert_eq!(text, "Updated todo list: 1 pending, 1 in progress, 1 completed.");
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(seen.lock().unwrap()[0].len(), 3);

        // 校验失败不落盘
        let r = tool
            .execute(&ToolExecutionInput::with_raw_arguments(
                dsh_llm::CallId("c2".into()),
                "todo_write".into(),
                r#"{"todos":[{"content":"a","status":"in_progress"},{"content":"b","status":"in_progress"}]}"#.into(),
            ))
            .await;
        assert!(r.is_error);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
}
