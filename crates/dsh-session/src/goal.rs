//! 目标域（上游 `packages/goal/goal`）：事件溯源状态、CAS 变更与进程本地
//! 续跑武装。本模块是纯面——类型（`types.ts`）、严格解码器与回放折叠
//! （`fold.ts`）、错误码（`runtime.ts`）；服务面（cordis GoalService）由
//! 宿主在批②起按等价缝落。
//!
//! 对齐要点：
//! - `GoalSnapshot`：目标 id + revision + objective + phase（active/paused/
//!   blocked/complete）+ maxGoalRounds；blocked 时必带 blockedReason。
//! - `goal/change` 事件：整快照变更或 clear 墓碑，字段集**逐字闭集**（解码器
//!   拒绝多余字段——上游 `Object.keys().sort().join(',')` 判定的 Rust 等价）。
//! - 严格回放：非 create 操作必须恰好推进当前目标一个 revision；edit 不许改
//!   phase；pause 只能 active→paused；resume 只能 active/paused/blocked→
//!   active 且预算有余；complete 只能非 complete→complete；block 只能
//!   active→blocked；clear 墓碑 revision +1 且时间戳不早于当前 updatedAt。
//! - 承认轮（`roundsStarted`）：`user/message` 事件带 goal 消息源时校验
//!   「恰好是当前 active 目标的下一轮、不超预算」——严格拒绝。
//! - `seenGoalIds`：拒绝目标 id 复用（防回放歧义）。

use serde::{Deserialize, Serialize};

/// 目标变更事件的 wire 版本（上游 `GOAL_CHANGE_VERSION`）。
pub const GOAL_CHANGE_VERSION: u64 = 1;

/// 目标 id（上游 `GoalId` 品牌；wire 上就是字符串）。
pub type GoalId = String;

/// 一个目标的一次确切修订的 CAS 身份（上游 `GoalRef`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalRef {
    pub id: GoalId,
    /// 正整数；每个耐久变更推进一位。
    pub revision: u64,
}

/// 目标的耐久生命周期阶段（上游 `GoalPhase`；wire 上是小写字面量）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalPhase {
    Active,
    Paused,
    Blocked,
    Complete,
}

impl GoalPhase {
    /// wire 名（上游小写字面量）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
            Self::Complete => "complete",
        }
    }
}

/// 阻塞原因（上游 `GoalBlockReason`）：机器可路由的稳定 code + 人类可读说明。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalBlockReason {
    /// 策略选择的 lower-kebab-case 稳定分类。
    pub code: String,
    /// 给人与模型看的非空说明。
    pub message: String,
}

/// 全量耐久快照（上游 `GoalSnapshot`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalSnapshot {
    pub id: GoalId,
    pub revision: u64,
    /// 人类请求的完成目标。
    pub objective: String,
    pub phase: GoalPhase,
    /// blocked 时必带。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<GoalBlockReason>,
    /// 承认轮上限。
    pub max_goal_rounds: u64,
}

/// 一次目标状态变更（上游 `GoalSnapshotChangeMeta`，`goal/change` 整快照支）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalSnapshotChange {
    /// 变更后目标的全量快照。
    pub goal: GoalSnapshot,
    /// 变更时已承认的轮数。
    pub rounds_started: u64,
    /// 目标创建时间（epoch ms）。
    pub created_at: u64,
    /// 本变更时间（epoch ms）。
    pub updated_at: u64,
}

/// 目标清除墓碑（上游 `GoalClearChangeMeta`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalClear {
    /// 被清除目标的最后身份（revision + 1）。
    pub cleared: GoalRef,
    /// 清除时间（epoch ms）。
    pub cleared_at: u64,
}

/// `goal/change` 事件载荷（上游 `GoalChangeMeta` 闭集联合）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "lowercase")]
pub enum GoalChange {
    Create(GoalSnapshotChange),
    Edit(GoalSnapshotChange),
    Pause(GoalSnapshotChange),
    Resume(GoalSnapshotChange),
    Complete(GoalSnapshotChange),
    Block(GoalSnapshotChange),
    Clear(GoalClear),
}

impl GoalChange {
    /// 操作名（上游 `GoalOperation`）。
    pub fn operation(&self) -> &'static str {
        match self {
            Self::Create(_) => "create",
            Self::Edit(_) => "edit",
            Self::Pause(_) => "pause",
            Self::Resume(_) => "resume",
            Self::Complete(_) => "complete",
            Self::Block(_) => "block",
            Self::Clear(_) => "clear",
        }
    }

    /// 编码为 web `goal/change` 的完整 data 形（`kind`/`version` 补齐；与
    /// [`decode_goal_change`] 互逆）。
    ///
    /// 注意 `GoalChange` 自身的 serde 形（`#[serde(tag = "operation")]`）只覆盖
    /// 操作支，没有 `kind`/`version`——那是信封层的字段；落盘/回放都走本函数
    /// 与 `decode_goal_change` 这对边界。
    pub fn to_wire(&self) -> serde_json::Value {
        match self {
            Self::Clear(clear) => serde_json::json!({
                "kind": "goal/change",
                "version": GOAL_CHANGE_VERSION,
                "operation": "clear",
                "cleared": serde_json::to_value(&clear.cleared).unwrap_or_default(),
                "clearedAt": clear.cleared_at,
            }),
            _ => {
                let change = self.snapshot_change().expect("非 clear 必带快照变更");
                serde_json::json!({
                    "kind": "goal/change",
                    "version": GOAL_CHANGE_VERSION,
                    "operation": self.operation(),
                    "goal": serde_json::to_value(&change.goal).unwrap_or_default(),
                    "roundsStarted": change.rounds_started,
                    "createdAt": change.created_at,
                    "updatedAt": change.updated_at,
                })
            }
        }
    }

    /// 本次变更携带的修订身份（上游 `goalChangeRef`）。
    pub fn ref_of(&self) -> GoalRef {
        match self {
            Self::Clear(clear) => clear.cleared.clone(),
            Self::Create(change)
            | Self::Edit(change)
            | Self::Pause(change)
            | Self::Resume(change)
            | Self::Complete(change)
            | Self::Block(change) => GoalRef {
                id: change.goal.id.clone(),
                revision: change.goal.revision,
            },
        }
    }

    /// 整快照支的载荷（clear 为 `None`）。
    pub fn snapshot_change(&self) -> Option<&GoalSnapshotChange> {
        match self {
            Self::Create(change)
            | Self::Edit(change)
            | Self::Pause(change)
            | Self::Resume(change)
            | Self::Complete(change)
            | Self::Block(change) => Some(change),
            Self::Clear(_) => None,
        }
    }
}

/// 目标消息源（上游 `GoalMessageSource`，`user/message` 的归属）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalMessageSource {
    pub goal_id: GoalId,
    pub revision: u64,
    /// 已承认的续跑轮次（正整数）。
    pub round: u64,
}

/// 纯回放折叠的累积器（上游 `GoalFoldState`）。
#[derive(Clone, Debug, Default)]
pub struct GoalFoldState {
    /// 当前目标；clear 后、首个 create 前为 `None`。
    pub goal: Option<GoalSnapshot>,
    /// 当前目标已承认的最高轮次。
    pub rounds_started: u64,
    /// 当前目标创建时间。
    pub created_at: Option<u64>,
    /// 当前目标最近变更时间。
    pub updated_at: Option<u64>,
    /// 最近一次变更的身份（含 clear 墓碑）。
    pub last_ref: Option<GoalRef>,
    /// 本会话已创建过的目标 id（拒绝复用）。
    pub seen_goal_ids: std::collections::HashSet<GoalId>,
}

/// 折叠结果（上游 `FoldedGoal`；无目标时只有字段缺省）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FoldedGoal {
    pub goal: Option<GoalSnapshot>,
    pub rounds_started: u64,
    pub created_at: Option<u64>,
    pub updated_at: Option<u64>,
    pub last_ref: Option<GoalRef>,
}

/// 校验一个正安全整数（上游 `positiveInteger`）。
fn positive_integer(value: &serde_json::Value, field: &str) -> Result<u64, String> {
    match value.as_u64() {
        Some(number) if number >= 1 => Ok(number),
        _ => Err(format!("goal change {field} must be a positive safe integer")),
    }
}

/// 校验一个非负安全整数（上游 `nonNegativeInteger`）。
fn non_negative_integer(value: &serde_json::Value, field: &str) -> Result<u64, String> {
    match value.as_u64() {
        Some(number) => Ok(number),
        _ => Err(format!("goal change {field} must be a non-negative safe integer")),
    }
}

/// 对象键集逐字判定：排序逗连后必须恰为期望集（上游 `Object.keys().sort().join(',')`）。
fn expect_keys(
    record: &serde_json::Map<String, serde_json::Value>,
    expected: &[&str],
) -> Result<(), String> {
    let mut keys: Vec<&str> = record.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut wanted: Vec<&str> = expected.to_vec();
    wanted.sort_unstable();
    if keys == wanted {
        Ok(())
    } else {
        Err(format!("goal change must have exactly {} fields", wanted.join(",")))
    }
}

/// 校验 lower-kebab-case code（上游 `^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$`）。
///
/// 状态机：首字母必须小写（0→1）；段内接受小写/数字；连字符开新段（1→2），
/// 新段首字符必须小写字母（2→1）。数字开头、连续连字符、尾连字符、
/// 非小写字母数字一律拒绝。
fn is_kebab_code(code: &str) -> bool {
    let mut state = 0;
    for byte in code.bytes() {
        match (state, byte) {
            (0, b'a'..=b'z') | (2, b'a'..=b'z') => state = 1,
            (1, b'a'..=b'z' | b'0'..=b'9') => {}
            (1, b'-') => state = 2,
            _ => return false,
        }
    }
    state == 1
}

/// 解码一个 blockedReason（上游 `decodeBlockReason`）。
fn decode_block_reason(value: &serde_json::Value) -> Result<GoalBlockReason, String> {
    let Some(record) = value.as_object() else {
        return Err(
            "goal change goal.blockedReason must have exactly code and message fields".into(),
        );
    };
    expect_keys(record, &["code", "message"])?;
    let code = record.get("code").and_then(serde_json::Value::as_str).unwrap_or_default();
    if !is_kebab_code(code) {
        return Err("goal change goal.blockedReason.code must be lower-kebab-case".into());
    }
    let message = record.get("message").and_then(serde_json::Value::as_str).unwrap_or_default();
    if message.trim().is_empty() || message != message.trim() {
        return Err("goal change goal.blockedReason.message must be non-empty and normalized".into());
    }
    Ok(GoalBlockReason { code: code.to_string(), message: message.to_string() })
}

/// 解码一个快照（上游 `decodeSnapshot`）。
fn decode_snapshot(value: &serde_json::Value) -> Result<GoalSnapshot, String> {
    let Some(record) = value.as_object() else {
        return Err("goal change goal must be a record".into());
    };
    let id = record.get("id").and_then(serde_json::Value::as_str).unwrap_or_default();
    if id.is_empty() {
        return Err("goal change goal.id must be a non-empty string".into());
    }
    let objective = record
        .get("objective")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if objective.trim().is_empty() || objective != objective.trim() {
        return Err("goal change goal.objective must be non-empty and normalized".into());
    }
    let phase = match record.get("phase").and_then(serde_json::Value::as_str) {
        Some("active") => GoalPhase::Active,
        Some("paused") => GoalPhase::Paused,
        Some("blocked") => GoalPhase::Blocked,
        Some("complete") => GoalPhase::Complete,
        _ => return Err("goal change goal.phase is invalid".into()),
    };
    let expected_keys: &[&str] = match phase {
        GoalPhase::Blocked => &["blockedReason", "id", "maxGoalRounds", "objective", "phase", "revision"],
        _ => &["id", "maxGoalRounds", "objective", "phase", "revision"],
    };
    if let Err(_message) = expect_keys(record, expected_keys) {
        // 错误文案带 phase 与确切期望集（上游逐字）。
        let expected_text = match phase {
            GoalPhase::Blocked => "blockedReason,id,maxGoalRounds,objective,phase,revision",
            _ => "id,maxGoalRounds,objective,phase,revision",
        };
        return Err(format!(
            "goal change goal for phase {} must have exactly {} fields",
            phase.as_str(),
            expected_text
        ));
    }
    let blocked_reason = match phase {
        GoalPhase::Blocked => Some(decode_block_reason(
            record.get("blockedReason").unwrap_or(&serde_json::Value::Null),
        )?),
        _ => None,
    };
    let revision = positive_integer(
        record.get("revision").unwrap_or(&serde_json::Value::Null),
        "goal.revision",
    )?;
    let max_goal_rounds = positive_integer(
        record.get("maxGoalRounds").unwrap_or(&serde_json::Value::Null),
        "goal.maxGoalRounds",
    )?;
    Ok(GoalSnapshot {
        id: id.to_string(),
        revision,
        objective: objective.to_string(),
        phase,
        blocked_reason,
        max_goal_rounds,
    })
}

/// 解码一个墓碑身份（上游 `decodeRef`）。
fn decode_ref(value: &serde_json::Value) -> Result<GoalRef, String> {
    let Some(record) = value.as_object() else {
        return Err("goal clear tombstone must have exactly id and revision fields".into());
    };
    expect_keys(record, &["id", "revision"])?;
    let id = record.get("id").and_then(serde_json::Value::as_str).unwrap_or_default();
    if id.is_empty() {
        return Err("goal clear tombstone id must be a non-empty string".into());
    }
    let revision = positive_integer(
        record.get("revision").unwrap_or(&serde_json::Value::Null),
        "cleared.revision",
    )?;
    Ok(GoalRef { id: id.to_string(), revision })
}

/// 版本字段的显示文本（缺失时给 'undefined'）。
fn version_text(record: &serde_json::Map<String, serde_json::Value>) -> String {
    match record.get("version") {
        Some(value) => value.to_string(),
        None => "undefined".to_string(),
    }
}

/// 解码一个自称目标变更的值（上游 `decodeGoalChange`）。
///
/// 非目标变更的值返回 `Ok(None)`；格式非法的目标变更**响亮失败**。
pub fn decode_goal_change(value: &serde_json::Value) -> Result<Option<GoalChange>, String> {
    let Some(record) = value.as_object() else {
        return Ok(None);
    };
    if record.get("kind").and_then(serde_json::Value::as_str) != Some("goal/change") {
        return Ok(None);
    }
    match record.get("version").and_then(serde_json::Value::as_u64) {
        Some(GOAL_CHANGE_VERSION) => {}
        _ => return Err(format!("unsupported goal change version {}", version_text(record))),
    }
    let operation = record
        .get("operation")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if operation == "clear" {
        let allowed = ["cleared", "clearedAt", "kind", "operation", "version"];
        expect_keys(record, &allowed)?;
        let cleared = decode_ref(record.get("cleared").unwrap_or(&serde_json::Value::Null))?;
        let cleared_at = non_negative_integer(
            record.get("clearedAt").unwrap_or(&serde_json::Value::Null),
            "clearedAt",
        )?;
        return Ok(Some(GoalChange::Clear(GoalClear { cleared, cleared_at })));
    }
    let operation = match operation {
        "create" | "edit" | "pause" | "resume" | "complete" | "block" => operation,
        _ => return Err("goal change operation is invalid".into()),
    };
    let allowed = ["createdAt", "goal", "kind", "operation", "roundsStarted", "updatedAt", "version"];
    expect_keys(record, &allowed)?;
    let created_at = non_negative_integer(
        record.get("createdAt").unwrap_or(&serde_json::Value::Null),
        "createdAt",
    )?;
    let updated_at = non_negative_integer(
        record.get("updatedAt").unwrap_or(&serde_json::Value::Null),
        "updatedAt",
    )?;
    if updated_at < created_at {
        return Err("goal change updatedAt cannot precede createdAt".into());
    }
    let rounds_started = non_negative_integer(
        record.get("roundsStarted").unwrap_or(&serde_json::Value::Null),
        "roundsStarted",
    )?;
    let goal = decode_snapshot(record.get("goal").unwrap_or(&serde_json::Value::Null))?;
    let change = GoalSnapshotChange { goal, rounds_started, created_at, updated_at };
    Ok(Some(match operation {
        "create" => GoalChange::Create(change),
        "edit" => GoalChange::Edit(change),
        "pause" => GoalChange::Pause(change),
        "resume" => GoalChange::Resume(change),
        "complete" => GoalChange::Complete(change),
        // 上面 match 已闭集校验，这里只剩 block。
        _ => GoalChange::Block(change),
    }))
}

/// 要求两快照保持只有 edit 可换的字段（上游 `requireSameDefinition`）。
fn require_same_definition(
    current: &GoalSnapshot,
    next: &GoalSnapshot,
    operation: &str,
) -> Result<(), String> {
    if next.objective != current.objective || next.max_goal_rounds != current.max_goal_rounds {
        return Err(format!("goal {operation} cannot change objective or maxGoalRounds"));
    }
    Ok(())
}

/// 要求目标恰好推进一个 revision（上游 `requireNextRevision`）。
fn require_next_revision(current: &GoalSnapshot, next: &GoalRef, operation: &str) -> Result<(), String> {
    if next.id != current.id || next.revision != current.revision + 1 {
        return Err(format!("goal {operation} must advance the current goal by one revision"));
    }
    Ok(())
}

/// 校验非 create 快照操作对前一投影的合法性（上游 `validateSnapshotTransition`）。
fn validate_snapshot_transition(
    state: &GoalFoldState,
    change: &GoalSnapshotChange,
    operation: &str,
    current: &GoalSnapshot,
) -> Result<(), String> {
    let next = &change.goal;
    require_next_revision(
        current,
        &GoalRef { id: next.id.clone(), revision: next.revision },
        operation,
    )?;
    let Some(updated_at) = state.updated_at else {
        return Err("current goal fold lacks updatedAt".into());
    };
    if change.created_at != state.created_at.unwrap_or(0)
        || change.updated_at < updated_at
        || change.rounds_started != state.rounds_started {
        return Err(format!(
            "goal {operation} does not preserve the current counters and timestamps"
        ));
    }
    match operation {
        "edit" => {
            if next.phase != current.phase || next.blocked_reason != current.blocked_reason {
                return Err("goal edit cannot change phase or blocked reason".into());
            }
            Ok(())
        }
        "pause" => {
            require_same_definition(current, next, operation)?;
            if current.phase != GoalPhase::Active || next.phase != GoalPhase::Paused {
                return Err("goal pause has an invalid phase transition".into());
            }
            Ok(())
        }
        "resume" => {
            require_same_definition(current, next, operation)?;
            let resumable = [GoalPhase::Active, GoalPhase::Paused, GoalPhase::Blocked];
            if !resumable.contains(&current.phase)
                || next.phase != GoalPhase::Active
                || state.rounds_started >= next.max_goal_rounds {
                return Err(
                    "goal resume has an invalid phase transition or exhausted round budget".into(),
                );
            }
            Ok(())
        }
        "complete" => {
            require_same_definition(current, next, operation)?;
            if current.phase == GoalPhase::Complete || next.phase != GoalPhase::Complete {
                return Err("goal complete has an invalid phase transition".into());
            }
            Ok(())
        }
        "block" => {
            require_same_definition(current, next, operation)?;
            if current.phase != GoalPhase::Active || next.phase != GoalPhase::Blocked {
                return Err("goal block has an invalid phase transition".into());
            }
            Ok(())
        }
        _ => Err(format!(
            "goal {operation} cannot be validated as a current-goal transition"
        )),
    }
}

/// 应用一个解码后的变更到累积器（上游 `applyGoalChange`）。
pub fn apply_goal_change(state: &mut GoalFoldState, change: &GoalChange) -> Result<(), String> {
    let reference = change.ref_of();
    match change {
        GoalChange::Clear(clear) => {
            let Some(current) = state.goal.clone() else {
                return Err("goal clear requires a current goal".into());
            };
            require_next_revision(&current, &clear.cleared, "clear")?;
            let Some(updated_at) = state.updated_at else {
                return Err("current goal fold lacks updatedAt".into());
            };
            if clear.cleared_at < updated_at {
                return Err(
                    "goal clear timestamp cannot precede the current goal update".into()
                );
            }
            state.goal = None;
            state.rounds_started = 0;
            state.created_at = None;
            state.updated_at = None;
            state.last_ref = Some(reference);
            Ok(())
        }
        GoalChange::Create(change) => {
            if change.goal.revision != 1
                || change.goal.phase != GoalPhase::Active
                || change.rounds_started != 0
                || (state.goal.is_some()
                    && state.goal.as_ref().map(|g| g.phase) != Some(GoalPhase::Complete))
                || state.seen_goal_ids.contains(&change.goal.id) {
                return Err(
                    "goal create requires a fresh active revision-one goal with zero rounds"
                        .into(),
                );
            }
            state.seen_goal_ids.insert(change.goal.id.clone());
            let goal = change.goal.clone();
            state.goal = Some(goal);
            state.rounds_started = change.rounds_started;
            state.created_at = Some(change.created_at);
            state.updated_at = Some(change.updated_at);
            state.last_ref = Some(reference);
            Ok(())
        }
        GoalChange::Edit(change) => {
            apply_non_create(state, change, "edit", reference)
        }
        GoalChange::Pause(change) => {
            apply_non_create(state, change, "pause", reference)
        }
        GoalChange::Resume(change) => {
            apply_non_create(state, change, "resume", reference)
        }
        GoalChange::Complete(change) => {
            apply_non_create(state, change, "complete", reference)
        }
        GoalChange::Block(change) => {
            apply_non_create(state, change, "block", reference)
        }
    }
}

/// 非 create 快照支的公共应用：先校验转移，再整快照覆盖（上游 `applyGoalChange`
/// 的 else 分支 + `state.goal = change.goal …` 尾部）。
fn apply_non_create(
    state: &mut GoalFoldState,
    change: &GoalSnapshotChange,
    operation: &str,
    reference: GoalRef,
) -> Result<(), String> {
    let Some(current) = state.goal.clone() else {
        return Err(format!("goal {operation} requires a current goal"));
    };
    validate_snapshot_transition(state, change, operation, &current)?;
    let goal = change.goal.clone();
    state.goal = Some(goal);
    state.rounds_started = change.rounds_started;
    state.created_at = Some(change.created_at);
    state.updated_at = Some(change.updated_at);
    state.last_ref = Some(reference);
    Ok(())
}

/// 应用一条会话事件到严格耐久折叠（上游 `applyGoalEvent`）。
///
/// `event_data` 是事件的完整 wire 载荷。`user/message` 支从 `data.source`
/// 里读 goal 消息源。
pub fn apply_goal_event(
    state: &mut GoalFoldState,
    event_type: &str,
    event_data: &serde_json::Value,
    event_seq: u64,
) -> Result<(), String> {
    if event_type == "goal/change" {
        let Some(change) = decode_goal_change(event_data)? else {
            return Err(format!(
                "goal change at session event {event_seq} has an invalid kind"
            ));
        };
        return apply_goal_change(state, &change);
    }
    if event_type == "user/message" {
        let Some(source) = event_data.get("source") else {
            return Ok(());
        };
        if source.get("kind").and_then(serde_json::Value::as_str) != Some("goal") {
            return Ok(());
        }
        let goal_id = source
            .get("goalId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let revision = source.get("revision").and_then(serde_json::Value::as_u64);
        let round = source.get("round").and_then(serde_json::Value::as_u64);
        let (Some(revision), Some(round)) = (revision, round) else {
            return Err("goal message source is invalid".into());
        };
        if goal_id.is_empty() || revision < 1 || round < 1 {
            return Err("goal message source is invalid".into());
        }
        let Some(current) = state.goal.clone() else {
            return Err(format!(
                "goal round at session event {event_seq} is not the next admitted round of the active goal"
            ));
        };
        if current.phase != GoalPhase::Active
            || goal_id != current.id
            || revision != current.revision
            || round != state.rounds_started + 1
            || round > current.max_goal_rounds {
            return Err(format!(
                "goal round at session event {event_seq} is not the next admitted round of the active goal"
            ));
        }
        state.rounds_started = round;
    }
    Ok(())
}

/// 从连续会话事件流折叠当前目标状态（上游 `foldGoal`）。
///
/// `events`：(类型名, 载荷) 序列；任何 goal 相关事件非法都会响亮失败。
pub fn fold_goal<'a, I>(events: I) -> Result<FoldedGoal, String>
where
    I: IntoIterator<Item = (&'a str, &'a serde_json::Value)>,
{
    let mut state = GoalFoldState::default();
    for (index, (event_type, event_data)) in events.into_iter().enumerate() {
        apply_goal_event(&mut state, event_type, event_data, index as u64 + 1)?;
    }
    Ok(FoldedGoal {
        goal: state.goal,
        rounds_started: state.rounds_started,
        created_at: state.created_at,
        updated_at: state.updated_at,
        last_ref: state.last_ref,
    })
}