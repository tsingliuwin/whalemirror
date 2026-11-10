//! 模型面目标工具（上游 `tool-goal/src/index.ts`）：`get_goal` / `create_goal`
//! / `update_goal` 三件套 + `tool:goal` 提示词节。
//!
//! rustdsh 工具缝没有 exec.agent —— 权限材料（打开轮次消息、根会话标记）由
//! 宿主经 [`AuthorityProbeSlot`] 每轮注入；wrapup 上下文（自主轮 complete/
//! blocked 后的收尾指令）经 [`DeferContextSlot`] 回给宿主注入。

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use dsh_llm::ContentBlock;
use dsh_tools::{Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult};
use serde_json::{json, Value};

use crate::authority::{completion_authority, require_direct_human, AuthorityProbe};
use crate::store::{
    CreateGoalRequest, EditGoalRequest, GoalBlockReason, GoalError, GoalPhase, GoalRef,
    GoalStore, GoalView, codes,
};
use crate::wrapup::render_wrapup_context;

/// 模型自报 blocked 前的最少连续轮数（上游默认 3）。
pub const DEFAULT_BLOCKED_AFTER_CONSECUTIVE_ROUNDS: u64 = 3;

/// 上游 CREATE_DESCRIPTION 逐字。
const CREATE_DESCRIPTION: &str = "Create a persisted goal that keeps this session working across automatic continuation rounds. Use it when the direct human request is a long-running objective, even if the user did not say \"goal\"; not for single-turn work.";

/// 上游 GET_DESCRIPTION 逐字。
const GET_DESCRIPTION: &str =
    "Read the current session goal, including the id and revision that update_goal requires.";

/// 权限材料注入缝（宿主每轮更新；无材料 = 无打开轮次，变更操作拒绝）。
pub type AuthorityProbeSlot = Arc<dyn Fn() -> AuthorityProbe + Send + Sync>;

/// wrapup 上下文回注缝（上游 exec.deferContext）。
pub type DeferContextSlot = Arc<dyn Fn(String) + Send + Sync>;

/// 目标工具集合（共享 store、权限探针与 wrapup 缝）。
pub struct GoalTools {
    store: Arc<GoalStore>,
    blocked_after: u64,
    default_max_goal_rounds: u64,
    probe: RwLock<Option<AuthorityProbeSlot>>,
    defer: RwLock<Option<DeferContextSlot>>,
}

impl GoalTools {
    /// 默认策略（blockedAfter=3；defaultMaxGoalRounds 上游部署默认 256）。
    pub fn new(store: Arc<GoalStore>) -> Self {
        Self {
            store,
            blocked_after: DEFAULT_BLOCKED_AFTER_CONSECUTIVE_ROUNDS,
            default_max_goal_rounds: 256,
            probe: RwLock::new(None),
            defer: RwLock::new(None),
        }
    }

    /// 覆盖策略（部署配置）。
    pub fn with_policy(mut self, blocked_after: u64, default_max_goal_rounds: u64) -> Self {
        self.blocked_after = blocked_after;
        self.default_max_goal_rounds = default_max_goal_rounds;
        self
    }

    /// 注入权限材料缝。
    pub fn set_probe(&self, probe: AuthorityProbeSlot) {
        *self.probe.write().unwrap() = Some(probe);
    }

    /// 注入 wrapup 回注缝。
    pub fn set_defer_context(&self, defer: DeferContextSlot) {
        *self.defer.write().unwrap() = Some(defer);
    }

    /// tool:goal 提示词节文本（上游 guidance 逐字，带部署阈值插值）。
    pub fn guidance(&self) -> String {
        format!(
            "create_goal may infer goal intent from a direct human request in any language. \
After session resume or fork, an active goal is disarmed: when \
a human asks to continue or resume in any wording or language, use update_goal action \
resume to rearm it. Mark complete only when the objective is actually achieved. Mark \
blocked only after the same blocking condition persists for at least {} \
consecutive goal rounds, and report that concrete condition in blocked_reason; difficulty, uncertainty, \
or useful remaining work is not blocked.",
            self.blocked_after
        )
    }

    /// 当前探针材料（无缝 = 空材料）。
    fn probe(&self) -> AuthorityProbe {
        self.probe
            .read()
            .unwrap()
            .as_ref()
            .map(|slot| slot())
            .unwrap_or_default()
    }

    /// 模型参数 → CAS 身份（上游 goalRef 逐字）。
    fn goal_ref(goal_id: &str, revision: &Value) -> Result<GoalRef, GoalError> {
        let revision = revision.as_u64().unwrap_or(0);
        if goal_id.is_empty()
            || goal_id != goal_id.trim()
            || revision < 1
        {
            return Err(GoalError::new(
                codes::TOOL_INVALID_UPDATE,
                "goal_id must be non-empty and revision must be a positive safe integer",
            ));
        }
        Ok(GoalRef { id: goal_id.to_string(), revision })
    }

    /// 紧凑模型结果（上游 goalValue：activation 是观察值；JSON 序列化呈现）。
    fn goal_value(view: Option<GoalView>) -> String {
        let value = match view {
            None => json!({ "goal": null }),
            Some(view) => {
                let mut goal = json!({
                    "id": view.id,
                    "revision": view.revision,
                    "objective": view.objective,
                    "phase": view.phase.as_str(),
                    "roundsStarted": view.rounds_started,
                    "maxGoalRounds": view.max_goal_rounds,
                });
                if let Some(reason) = &view.blocked_reason {
                    goal["blockedReason"] = json!({
                        "code": reason.code,
                        "message": reason.message,
                    });
                }
                json!({ "goal": goal, "activation": view.activation.as_str() })
            }
        };
        serde_json::to_string(&value).unwrap_or_else(|_| "{\"goal\":null}".into())
    }

    fn present(title: &str, kind: &str, raw: Option<&str>) -> Value {
        let mut card = json!({ "card": "generic", "title": title, "kind": kind });
        if let Some(raw) = raw {
            card["rawInput"] = json!(raw);
        }
        card
    }
}

/// 把 GoalError 落成工具错误结果。
fn error_result(error: GoalError) -> ToolExecutionResult {
    ToolExecutionResult::error(format!("{} ({})", error.message, error.code))
}

impl GoalTools {
    /// get_goal 执行体。
    async fn execute_get(&self) -> ToolExecutionResult {
        match self.store.get() {
            Ok(view) => ToolExecutionResult::text(Self::goal_value(view)),
            Err(error) => error_result(error),
        }
    }

    /// create_goal 执行体。
    async fn execute_create(&self, arguments: &Value) -> ToolExecutionResult {
        let probe = self.probe();
        if let Err(error) = require_direct_human(&probe) {
            return error_result(error);
        }
        let objective = arguments.get("objective").and_then(Value::as_str).unwrap_or_default();
        let max_goal_rounds = arguments.get("max_goal_rounds").and_then(Value::as_u64);
        match self.store.create(
            CreateGoalRequest { objective: objective.to_string(), max_goal_rounds },
            self.default_max_goal_rounds,
        ) {
            Ok(view) => ToolExecutionResult::text(Self::goal_value(Some(view))),
            Err(error) => error_result(error),
        }
    }

    /// update_goal 执行体（上游 execute 的五动作闭集）。
    async fn execute_update(&self, arguments: &Value) -> ToolExecutionResult {
        let goal_id = arguments
            .get("goal_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let reference = match Self::goal_ref(goal_id, arguments.get("revision").unwrap_or(&Value::Null)) {
            Ok(reference) => reference,
            Err(error) => return error_result(error),
        };
        let action = arguments.get("action").and_then(Value::as_str).unwrap_or_default();
        let objective = arguments
            .get("objective")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty());
        let max_goal_rounds = arguments
            .get("max_goal_rounds")
            .and_then(Value::as_u64)
            .filter(|rounds| *rounds != 0);
        let blocked_reason = arguments
            .get("blocked_reason")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty());

        match action {
            "edit" => {
                let probe = self.probe();
                if let Err(error) = require_direct_human(&probe) {
                    return error_result(error);
                }
                let request = EditGoalRequest {
                    objective: objective.map(str::to_string),
                    max_goal_rounds,
                };
                match self.store.edit(&reference, request) {
                    Ok(view) => ToolExecutionResult::text(Self::goal_value(Some(view))),
                    Err(error) => error_result(error),
                }
            }
            "pause" | "resume" => {
                let probe = self.probe();
                if let Err(error) = require_direct_human(&probe) {
                    return error_result(error);
                }
                // 上游：模型不能恢复 paused 目标——用户必须亲自恢复。
                if action == "resume" {
                    if let Ok(Some(current)) = self.store.get() {
                        if current.id == reference.id
                            && current.revision == reference.revision
                            && current.phase == GoalPhase::Paused
                        {
                            return error_result(GoalError::new(
                                codes::TOOL_RESUME_PAUSED,
                                "the model cannot resume a paused goal; the user must resume it",
                            ));
                        }
                    }
                }
                let outcome = if action == "pause" {
                    self.store.pause(&reference)
                } else {
                    self.store.resume(&reference)
                };
                match outcome {
                    Ok(view) => ToolExecutionResult::text(Self::goal_value(Some(view))),
                    Err(error) => error_result(error),
                }
            }
            "complete" | "blocked" => {
                let probe = self.probe();
                let current = self.store.get().unwrap_or(None);
                let authority = match completion_authority(&probe, &current) {
                    Ok(authority) => authority,
                    Err(error) => return error_result(error),
                };
                if action == "blocked" && blocked_reason.is_none() {
                    return error_result(GoalError::new(
                        codes::TOOL_INVALID_UPDATE,
                        "blocked_reason is required with action blocked",
                    ));
                }
                // 上游：blocked 在自主轮要过最少连续轮数门槛。
                if action == "blocked" {
                    if let crate::authority::CompletionAuthority::GoalRound { view } = &authority {
                        if view.rounds_started < self.blocked_after {
                            return error_result(GoalError::new(
                                codes::TOOL_BLOCK_THRESHOLD,
                                format!(
                                    "blocked requires at least {} consecutive goal rounds; current round is {}",
                                    self.blocked_after, view.rounds_started
                                ),
                            ));
                        }
                    }
                }
                let outcome = if action == "complete" {
                    self.store.complete(&reference)
                } else {
                    self.store.block(
                        &reference,
                        GoalBlockReason {
                            code: "model-reported".into(),
                            message: blocked_reason.unwrap_or_default().to_string(),
                        },
                    )
                };
                let view = match outcome {
                    Ok(view) => view,
                    Err(error) => return error_result(error),
                };
                // 自主轮终局：注入收尾上下文（上游 deferContext）。
                if matches!(
                    authority,
                    crate::authority::CompletionAuthority::GoalRound { .. }
                ) {
                    let text = render_wrapup_context(
                        &view.objective,
                        if action == "complete" {
                            None
                        } else {
                            blocked_reason
                        },
                    );
                    if let Some(defer) = self.defer.read().unwrap().as_ref() {
                        defer(text);
                    }
                }
                ToolExecutionResult::text(Self::goal_value(Some(view)))
            }
            _ => error_result(GoalError::new(
                codes::TOOL_INVALID_UPDATE,
                "action must be one of edit, pause, resume, complete, blocked",
            )),
        }
    }
}

/// get_goal 工具（上游 get_goal）。
pub struct GetGoalTool {
    tools: Arc<GoalTools>,
}

/// create_goal 工具（上游 create_goal）。
pub struct CreateGoalTool {
    tools: Arc<GoalTools>,
}

/// update_goal 工具（上游 update_goal）。
pub struct UpdateGoalTool {
    tools: Arc<GoalTools>,
}

/// 三工具注册（上游 apply 的 tools.register 等价）。
pub fn register_goal_tools(
    registry: &dsh_tools::ToolRegistry,
    tools: Arc<GoalTools>,
) -> Result<Vec<dsh_llm::Disposer>, String> {
    let mut disposers = Vec::new();
    for tool in [
        Arc::new(GetGoalTool { tools: Arc::clone(&tools) }) as Arc<dyn Tool>,
        Arc::new(CreateGoalTool { tools: Arc::clone(&tools) }) as Arc<dyn Tool>,
        Arc::new(UpdateGoalTool { tools: Arc::clone(&tools) }) as Arc<dyn Tool>,
    ] {
        match registry.register(tool) {
            Ok(disposer) => disposers.push(disposer),
            Err(error) => {
                for disposer in disposers.drain(..) {
                    disposer();
                }
                return Err(format!("tool-goal: register failed: {error:?}"));
            }
        }
    }
    Ok(disposers)
}

#[async_trait]
impl Tool for GetGoalTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "get_goal".into(),
            description: GET_DESCRIPTION.into(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {},
            }),
        }
    }

    async fn execute(&self, _input: &ToolExecutionInput) -> ToolExecutionResult {
        self.tools.execute_get().await
    }
}

#[async_trait]
impl Tool for CreateGoalTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "create_goal".into(),
            description: CREATE_DESCRIPTION.into(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "objective": {
                        "type": "string",
                        "description": "The concrete completion objective inferred from the direct human request.",
                    },
                    "max_goal_rounds": {
                        "type": "number",
                        "description": "Optional positive safe-integer limit on automatic continuation rounds.",
                    },
                },
                "required": ["objective"],
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        self.tools.execute_create(&input.arguments).await
    }
}

#[async_trait]
impl Tool for UpdateGoalTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "update_goal".into(),
            description: "Update the current goal.".into(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "goal_id": {
                        "type": "string",
                        "description": "Exact id returned by get_goal.",
                    },
                    "revision": {
                        "type": "number",
                        "description": "Exact positive revision returned by get_goal.",
                    },
                    "action": {
                        "type": "string",
                        "enum": ["edit", "pause", "resume", "complete", "blocked"],
                        "description": "edit, pause, and resume require a direct top-level human request. complete and blocked are also allowed \
during an automatic continuation of this goal; blocked is rejected before the configured minimum round count.",
                    },
                    "objective": {
                        "type": "string",
                        "description": "Replacement objective; valid only with action edit.",
                    },
                    "max_goal_rounds": {
                        "type": "number",
                        "description": "Replacement cap; valid only with action edit.",
                    },
                    "blocked_reason": {
                        "type": "string",
                        "description": "Required only with action blocked: the concrete condition that persisted across rounds and blocks progress.",
                    },
                },
                "required": ["goal_id", "revision", "action"],
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let present = GoalTools::present(
            &match input.arguments.get("action").and_then(Value::as_str).unwrap_or_default() {
                "blocked" => "Mark goal".to_string(),
                other => {
                    let mut chars = other.chars();
                    match chars.next() {
                        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                        None => String::new(),
                    }
                }
            },
            "other",
            input
                .arguments
                .get("blocked_reason")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
                .or_else(|| {
                    input.arguments.get("objective").and_then(Value::as_str).filter(|t| !t.is_empty())
                })
                .or_else(|| input.arguments.get("goal_id").and_then(Value::as_str)),
        );
        let mut result = self.tools.execute_update(&input.arguments).await;
        if !result.is_error {
            result.content = vec![ContentBlock::text(
                // 重新渲染（上游 render 回调：JSON.stringify(value)——错误时
                // 文本已是错误信息，不覆盖）。
                result
                    .content
                    .first()
                    .map(|block| match block {
                        ContentBlock::Text { text } => text.clone(),
                        _ => String::new(),
                    })
                    .unwrap_or_default(),
            )];
            result.presentation = Some(present);
        }
        result
    }
}