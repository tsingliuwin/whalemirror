//! 目标服务（上游 `packages/goal/goal/src/index.ts` 的 GoalService 等价面）：
//! 单会话目标的 CAS 变更、激活（armed/disarmed）、`goal/change` 事件提交。
//!
//! 上游是 cordis 服务（跨进程 Remote）；rustdsh 单进程内存态等价——变更走
//! [`GoalStore`]，耐久事实经 [`CommitSink`] 落会话日志（`goal/change` 事件），
//! 折叠回放用 `dsh-session::goal` 的严格 fold。
//!
//! 对齐要点：
//! - 七操作闭集：create/edit/pause/resume/complete/block/clear。
//! - CAS：非 create 操作带 [`GoalRef`]，与当前不符报 `GOAL_STALE_REVISION`。
//! - 激活是**进程本地**事实（armed/disarmed），绝不落盘：create/resume 后
//!   armed，pause/complete/block/clear 后 disarmed；提交时刻的激活经
//!   `pendingActivation` 跨「同步 append 边界」（上游 commit 的 seq 配对语义）。
//! - `nextMutationTime` 钳制墙钟倒退：`max(now, current.updatedAt)`。
//! - 换代失败整表拒绝（上游 resolveCreateGoal/resolveBlockReason 的校验文案逐字）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dsh_llm::LlmFailure;

use dsh_session::goal::{
    GoalChange, GoalClear, GoalFoldState, GoalId, GoalSnapshot, GoalSnapshotChange,
    apply_goal_change, apply_goal_event,
};
pub use dsh_session::goal::{GoalBlockReason, GoalPhase, GoalRef};

/// 目标域边界错误：稳定 code（上游 `GoalErrorCode`）+ 人类可读 message。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoalError {
    pub code: &'static str,
    pub message: String,
}

impl GoalError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

impl std::fmt::Display for GoalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for GoalError {}

/// 稳定错误码（上游 `GoalErrorCode` 逐字）。
pub mod codes {
    pub const AGENT_NOT_LIVE: &str = "GOAL_AGENT_NOT_LIVE";
    pub const NOT_FOUND: &str = "GOAL_NOT_FOUND";
    pub const ALREADY_EXISTS: &str = "GOAL_ALREADY_EXISTS";
    pub const STALE_REVISION: &str = "GOAL_STALE_REVISION";
    pub const INVALID_OBJECTIVE: &str = "GOAL_INVALID_OBJECTIVE";
    pub const INVALID_MAX_ROUNDS: &str = "GOAL_INVALID_MAX_ROUNDS";
    pub const INVALID_BLOCK_REASON: &str = "GOAL_INVALID_BLOCK_REASON";
    pub const INVALID_EDIT: &str = "GOAL_INVALID_EDIT";
    pub const INVALID_TRANSITION: &str = "GOAL_INVALID_TRANSITION";
    /// 工具面（上游 tool-goal 的 HarnessError code）。
    pub const TOOL_INVALID_UPDATE: &str = "GOAL_TOOL_INVALID_UPDATE";
    pub const TOOL_RESUME_PAUSED: &str = "GOAL_TOOL_RESUME_PAUSED";
    pub const TOOL_BLOCK_THRESHOLD: &str = "GOAL_TOOL_BLOCK_THRESHOLD";
}

/// 耐久提交缝：宿主把 `goal/change` 事件落会话日志（todo/write 同款注入）。
pub type CommitSink = Arc<dyn Fn(&GoalChange) + Send + Sync>;

/// 目标创建请求（上游 `CreateGoalRequest`）。
#[derive(Clone, Debug)]
pub struct CreateGoalRequest {
    pub objective: String,
    /// 缺省用部署默认（`defaultMaxGoalRounds`，上游默认 256）。
    pub max_goal_rounds: Option<u64>,
}

/// 目标编辑请求（上游 `EditGoalRequest`；至少一个字段）。
#[derive(Clone, Debug, Default)]
pub struct EditGoalRequest {
    pub objective: Option<String>,
    pub max_goal_rounds: Option<u64>,
}

/// 目标活体视图（上游 `GoalView`；activation 是观察值不落盘）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoalView {
    pub id: GoalId,
    pub revision: u64,
    pub objective: String,
    pub phase: GoalPhase,
    pub blocked_reason: Option<GoalBlockReason>,
    pub max_goal_rounds: u64,
    pub rounds_started: u64,
    pub created_at: u64,
    pub updated_at: u64,
    pub activation: GoalActivation,
}

/// 进程本地续跑武装（上游 `GoalActivation`；绝不落盘）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalActivation {
    Armed,
    Disarmed,
}

impl GoalActivation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Armed => "armed",
            Self::Disarmed => "disarmed",
        }
    }
}

/// 一次变更后的通知（上游 `GoalChanged`）。
#[derive(Clone, Debug)]
pub struct GoalChanged {
    pub operation: &'static str,
    pub ref_of: GoalRef,
    /// clear 时 None。
    pub goal: Option<GoalView>,
}

/// 单会话目标服务：内存态 + 耐久提交缝。
pub struct GoalStore {
    inner: Mutex<GoalFoldState>,
    sink: Mutex<Option<CommitSink>>,
    activation: AtomicBool,
    /// 耐久流是否曾报错（上游 projection failure：拒绝后续访问）。
    failure: Mutex<Option<String>>,
}

impl Default for GoalStore {
    fn default() -> Self {
        Self::new()
    }
}

impl GoalStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(GoalFoldState::default()),
            sink: Mutex::new(None),
            activation: AtomicBool::new(false),
            failure: Mutex::new(None),
        }
    }

    /// 注入耐久提交缝（宿主在 agent 就绪后接线）。
    pub fn set_sink(&self, sink: CommitSink) {
        *self.sink.lock().unwrap() = Some(sink);
    }

    /// 从耐久流回放（启动/重载时重建内存态）。
    ///
    /// 严格 fold：任何 goal 相关事件非法都记录 failure 并拒绝后续访问
    /// （上游 projection failure 语义：客户端视图停在最后合法目标）。
    pub fn replay<I>(&self, events: I) -> Result<(), String>
    where
        I: IntoIterator<Item = (String, serde_json::Value)>,
    {
        let mut state = GoalFoldState::default();
        for (index, (event_type, event_data)) in events.into_iter().enumerate() {
            if let Err(message) =
                apply_goal_event(&mut state, &event_type, &event_data, index as u64 + 1)
            {
                let failure = format!("goal replay failed at session event {}: {}", index + 1, message);
                *self.failure.lock().unwrap() = Some(failure.clone());
                *self.inner.lock().unwrap() = state;
                return Err(failure);
            }
        }
        *self.inner.lock().unwrap() = state;
        self.activation.store(false, Ordering::SeqCst);
        *self.failure.lock().unwrap() = None;
        Ok(())
    }

    /// 当前活体视图（无目标 None；耐久流失败时报错——上游 state() 抛 failure）。
    pub fn get(&self) -> Result<Option<GoalView>, GoalError> {
        if let Some(failure) = self.failure.lock().unwrap().clone() {
            return Err(GoalError::new(codes::NOT_FOUND, failure));
        }
        Ok(self.view())
    }

    /// 撤销进程本地续跑武装（不改耐久 phase/revision；上游 disarm）。
    pub fn disarm(&self) -> Option<GoalView> {
        self.activation.store(false, Ordering::SeqCst);
        self.view()
    }

    /// 创建并武装一个目标（上游 create）。
    ///
    /// 已完成的目标可被替换；其它当前 phase 必须先 clear。
    pub fn create(&self, request: CreateGoalRequest, default_max_goal_rounds: u64) -> Result<GoalView, GoalError> {
        let objective = resolve_objective(&request.objective)?;
        let max_goal_rounds =
            resolve_max_goal_rounds(request.max_goal_rounds.unwrap_or(default_max_goal_rounds))?;
        {
            let inner = self.inner.lock().unwrap();
            if let Some(current) = &inner.goal {
                if current.phase != GoalPhase::Complete {
                    return Err(GoalError::new(
                        codes::ALREADY_EXISTS,
                        format!(
                            "goal \"{}\" already exists with phase \"{}\"",
                            current.id,
                            current.phase.as_str()
                        ),
                    ));
                }
            }
        }
        let now = now_ms();
        let goal = GoalSnapshot {
            id: format!("goal-{}", uuid::Uuid::new_v4()),
            revision: 1,
            objective,
            phase: GoalPhase::Active,
            blocked_reason: None,
            max_goal_rounds,
        };
        self.commit_snapshot("create", goal, 0, now, now, true)
    }

    /// 编辑 objective 与/或 maxGoalRounds，不改 phase（上游 edit）。
    pub fn edit(&self, reference: &GoalRef, request: EditGoalRequest) -> Result<GoalView, GoalError> {
        let current_state = self.expect_current(reference)?;
        let current = current_state.goal.clone();
        if request.objective.is_none() && request.max_goal_rounds.is_none() {
            return Err(GoalError::new(
                codes::INVALID_EDIT,
                "goal edit requires objective and/or maxGoalRounds",
            ));
        }
        let goal = GoalSnapshot {
            id: current.id.clone(),
            revision: current.revision + 1,
            objective: match &request.objective {
                Some(objective) => resolve_objective(objective)?,
                None => current.objective.clone(),
            },
            phase: current.phase,
            blocked_reason: current.blocked_reason.clone(),
            max_goal_rounds: match request.max_goal_rounds {
                Some(rounds) => resolve_max_goal_rounds(rounds)?,
                None => current.max_goal_rounds,
            },
        };
        self.commit_current("edit", current_state, goal, self.activation.load(Ordering::SeqCst))
    }

    /// 暂停 active 目标并解除武装（上游 pause）。
    pub fn pause(&self, reference: &GoalRef) -> Result<GoalView, GoalError> {
        self.transition(reference, "pause", &[GoalPhase::Active], GoalPhase::Paused, false)
    }

    /// 恢复/重新武装目标（上游 resume）。
    ///
    /// resume 的合法前置含 active（重武装场景——上游 resume 在「active 且已
    /// disarmed」时允许再武装）；预算耗尽拒绝。
    pub fn resume(&self, reference: &GoalRef) -> Result<GoalView, GoalError> {
        let current_state = self.expect_current(reference)?;
        let current = current_state.goal.clone();
        let resumable = [GoalPhase::Active, GoalPhase::Paused, GoalPhase::Blocked];
        if !resumable.contains(&current.phase) {
            return Err(self.transition_error(&current, "resume", &resumable));
        }
        if current.phase == GoalPhase::Active && self.activation.load(Ordering::SeqCst) {
            return Err(GoalError::new(
                codes::INVALID_TRANSITION,
                format!("goal \"{}\" is already active and armed", current.id),
            ));
        }
        if current_state.rounds_started >= current.max_goal_rounds {
            return Err(GoalError::new(
                codes::INVALID_TRANSITION,
                format!(
                    "goal \"{}\" exhausted {} goal rounds; increase maxGoalRounds before resuming",
                    current.id, current.max_goal_rounds
                ),
            ));
        }
        let goal = with_phase(&current, GoalPhase::Active);
        self.commit_current("resume", current_state, goal, true)
    }

    /// 标记当前非 complete 目标完成并解除武装（上游 complete）。
    pub fn complete(&self, reference: &GoalRef) -> Result<GoalView, GoalError> {
        self.transition(
            reference,
            "complete",
            &[GoalPhase::Active, GoalPhase::Paused, GoalPhase::Blocked],
            GoalPhase::Complete,
            false,
        )
    }

    /// 标记 active 目标阻塞并解除武装（上游 block）。
    pub fn block(&self, reference: &GoalRef, reason: GoalBlockReason) -> Result<GoalView, GoalError> {
        let reason = resolve_block_reason(&reason)?;
        let current_state = self.expect_current(reference)?;
        let current = current_state.goal.clone();
        if current.phase != GoalPhase::Active {
            return Err(self.transition_error(&current, "block", &[GoalPhase::Active]));
        }
        let mut goal = with_phase(&current, GoalPhase::Blocked);
        goal.blocked_reason = Some(reason);
        self.commit_current("block", current_state, goal, false)
    }

    /// 清除当前目标，保留墓碑（上游 clear）。
    pub fn clear(&self, reference: &GoalRef) -> Result<GoalRef, GoalError> {
        let current_state = self.expect_current(reference)?;
        let current = current_state.goal.clone();
        let tombstone = GoalRef {
            id: current.id.clone(),
            revision: current.revision + 1,
        };
        let change = GoalChange::Clear(GoalClear {
            cleared: tombstone.clone(),
            cleared_at: self.next_mutation_time(&current_state),
        });
        self.commit(change, false);
        Ok(tombstone)
    }

    /// 内部：CAS 期望当前目标（上游 expectCurrent）。
    fn expect_current(&self, reference: &GoalRef) -> Result<CurrentGoal, GoalError> {
        if let Some(failure) = self.failure.lock().unwrap().clone() {
            return Err(GoalError::new(codes::NOT_FOUND, failure));
        }
        let inner = self.inner.lock().unwrap();
        let Some(current) = &inner.goal else {
            return Err(GoalError::new(codes::NOT_FOUND, "no current goal"));
        };
        if reference.id != current.id || reference.revision != current.revision {
            return Err(GoalError::new(
                codes::STALE_REVISION,
                format!(
                    "stale goal ref \"{}\" revision {}; current is \"{}\" revision {}",
                    reference.id, reference.revision, current.id, current.revision
                ),
            ));
        }
        Ok(CurrentGoal {
            goal: current.clone(),
            rounds_started: inner.rounds_started,
            created_at: inner.created_at.unwrap_or(0),
            updated_at: inner.updated_at.unwrap_or(0),
        })
    }

    /// 内部：view（从内存态 + activation 组装）。
    fn view(&self) -> Option<GoalView> {
        let inner = self.inner.lock().unwrap();
        let goal = inner.goal.as_ref()?;
        Some(GoalView {
            id: goal.id.clone(),
            revision: goal.revision,
            objective: goal.objective.clone(),
            phase: goal.phase,
            blocked_reason: goal.blocked_reason.clone(),
            max_goal_rounds: goal.max_goal_rounds,
            rounds_started: inner.rounds_started,
            created_at: inner.created_at.unwrap_or(0),
            updated_at: inner.updated_at.unwrap_or(0),
            activation: if self.activation.load(Ordering::SeqCst) {
                GoalActivation::Armed
            } else {
                GoalActivation::Disarmed
            },
        })
    }

    /// 内部：共享的相位转移（上游 transition）。
    fn transition(
        &self,
        reference: &GoalRef,
        operation: &'static str,
        allowed: &[GoalPhase],
        phase: GoalPhase,
        armed: bool,
    ) -> Result<GoalView, GoalError> {
        let current_state = self.expect_current(reference)?;
        let current = current_state.goal.clone();
        if !allowed.contains(&current.phase) {
            return Err(self.transition_error(&current, operation, allowed));
        }
        let goal = with_phase(&current, phase);
        self.commit_current(operation, current_state, goal, armed)
    }

    /// 内部：稳定非法转移错误（上游 transitionError 逐字）。
    fn transition_error(&self, current: &GoalSnapshot, operation: &str, allowed: &[GoalPhase]) -> GoalError {
        let expected: Vec<&str> = allowed.iter().map(|p| p.as_str()).collect();
        GoalError::new(
            codes::INVALID_TRANSITION,
            format!(
                "cannot {} goal \"{}\" from phase \"{}\"; expected {}",
                operation,
                current.id,
                current.phase.as_str(),
                expected.join(" or ")
            ),
        )
    }

    /// 内部：提交保留当前计数/时间的变更（上游 commitCurrent）。
    fn commit_current(
        &self,
        operation: &'static str,
        current: CurrentGoal,
        goal: GoalSnapshot,
        armed: bool,
    ) -> Result<GoalView, GoalError> {
        let updated_at = self.next_mutation_time(&current);
        self.commit_snapshot(
            operation,
            goal,
            current.rounds_started,
            current.created_at,
            updated_at,
            armed,
        )
    }

    /// 内部：钳制墙钟倒退（上游 nextMutationTime）。
    fn next_mutation_time(&self, current: &CurrentGoal) -> u64 {
        now_ms().max(current.updated_at)
    }

    /// 内部：构造并提交整快照变更（上游 commitSnapshot）。
    fn commit_snapshot(
        &self,
        operation: &'static str,
        goal: GoalSnapshot,
        rounds_started: u64,
        created_at: u64,
        updated_at: u64,
        armed: bool,
    ) -> Result<GoalView, GoalError> {
        let change = goal_change_from_parts(operation, goal, rounds_started, created_at, updated_at);
        self.commit(change, armed);
        Ok(self.view().expect("提交后必有当前目标"))
    }

    /// 内部：落内存态 + 走耐久缝 + 同步 activation（上游 commit）。
    fn commit(&self, change: GoalChange, armed: bool) {
        // 先落耐久缝（失败=宿主日志错误，耐久流的严格 fold 会在回放时报；
        // 内存态照常推进——上游 Session.append 失败同样抛出但内存投影已更新）。
        if let Some(sink) = self.sink.lock().unwrap().as_ref() {
            sink(&change);
        }
        {
            // 内存态推进走严格 apply（与回放同一条校验路径）。
            let mut inner = self.inner.lock().unwrap();
            let mut scratch = inner.clone();
            if let Err(message) = apply_goal_change(&mut scratch, &change) {
                // 服务层构造的变更不该被 fold 拒——这是内部不变量破坏，响亮报错。
                panic!("goal store committed an invalid change: {message}");
            }
            *inner = scratch;
        }
        self.activation.store(armed, Ordering::SeqCst);
    }
}

/// 期望当前目标的内部快照。
struct CurrentGoal {
    goal: GoalSnapshot,
    rounds_started: u64,
    created_at: u64,
    updated_at: u64,
}

/// 换相位并推进一档（上游 withPhase）。
fn with_phase(current: &GoalSnapshot, phase: GoalPhase) -> GoalSnapshot {
    GoalSnapshot {
        id: current.id.clone(),
        revision: current.revision + 1,
        objective: current.objective.clone(),
        phase,
        blocked_reason: None,
        max_goal_rounds: current.max_goal_rounds,
    }
}

/// 校验并归一 objective（上游 resolveObjective 逐字）。
fn resolve_objective(value: &str) -> Result<String, GoalError> {
    if value.trim().is_empty() {
        return Err(GoalError::new(
            codes::INVALID_OBJECTIVE,
            "goal objective must be a non-empty string",
        ));
    }
    Ok(value.trim().to_string())
}

/// 校验轮数上限（上游 resolveMaxGoalRounds 逐字）。
fn resolve_max_goal_rounds(value: u64) -> Result<u64, GoalError> {
    if value < 1 {
        return Err(GoalError::new(
            codes::INVALID_MAX_ROUNDS,
            "maxGoalRounds must be a positive safe integer",
        ));
    }
    Ok(value)
}

/// 校验阻塞原因（上游 resolveBlockReason 逐字）。
fn resolve_block_reason(reason: &GoalBlockReason) -> Result<GoalBlockReason, GoalError> {
    if reason.code.is_empty()
        || !is_lower_kebab(&reason.code)
        || reason.message.trim().is_empty()
    {
        return Err(GoalError::new(
            codes::INVALID_BLOCK_REASON,
            "goal block reason requires a lower-kebab-case code and a non-empty message",
        ));
    }
    Ok(GoalBlockReason {
        code: reason.code.clone(),
        message: reason.message.trim().to_string(),
    })
}

/// lower-kebab-case 判定（与 dsh-session::goal 的折叠校验同一规则）。
fn is_lower_kebab(code: &str) -> bool {
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

/// epoch 毫秒。
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 服务层构造：按操作名组装变体（上游 commitSnapshot 的字面量对应）。
fn goal_change_from_parts(
    operation: &str,
    goal: GoalSnapshot,
    rounds_started: u64,
    created_at: u64,
    updated_at: u64,
) -> GoalChange {
    let change = GoalSnapshotChange {
        goal,
        rounds_started,
        created_at,
        updated_at,
    };
    match operation {
        "create" => GoalChange::Create(change),
        "edit" => GoalChange::Edit(change),
        "pause" => GoalChange::Pause(change),
        "resume" => GoalChange::Resume(change),
        "complete" => GoalChange::Complete(change),
        "block" => GoalChange::Block(change),
        _ => unreachable!("服务层操作闭集"),
    }
}

/// 兼容 dsh-llm 失败形态的转换（工具面把 GoalError 落成 LLM 工具错误）。
impl From<GoalError> for LlmFailure {
    fn from(error: GoalError) -> Self {
        LlmFailure {
            message: error.message,
            code: error.code.to_string(),
            status: None,
            provider_retry_after_ms: None,
            request_id: None,
            offload_images: None,
        }
    }
}