//! 同会话目标轮驱动（上游 `packages/goal/goal-round-driver`）：armed 目标在
//! agent 静默时自动注入续跑轮。
//!
//! 上游是 cordis 插件（多 agent 状态表 + effect 卸载语义）；rustdsh 的等价
//! 面是**单 agent 内存驱动器** [`GoalRoundDriver`]，挂在 `EventBus` 上：
//! - `agent/status` → idle：静默检查（预算尽 block、预留轮兑现/撤回、竞争
//!   排队撤回 + 暂停 fence）→ 请求驱动。
//! - `goal/changed`（本地 `GoalEventBus`）：目标变更 → 请求驱动。
//! - `agent/inbox/inserted|claimed|discarded`：预留轮的阶段与竞争标记。
//! - `turn/end` 的 `max-tokens`/`aborted`：解除武装或标记取消（上游
//!   `session/event` 的 turn/end 分支）。
//! - `agent/pre-step` waterfall：预留有效性 fail-closed 校验（exact revision
//!   + armed + 恰为下一轮），无效拒绝该步。
//!
//! 轮预算尽时的自动 block 用 `round-limit` code（上游逐字）。

use std::sync::{Arc, Mutex};

use dsh_agent::{AgentInboxClaimed, AgentInboxDiscarded, AgentInboxInserted, AgentPreStep, AgentStatus, AgentStatusChanged, InboxClaimedPayload, PreStepDecision, PreStepInput};
use dsh_cordis::{BoxFuture, EventBus};
use dsh_llm::{ContentBlock, Message, MessageId, MessageSource, Role};
use dsh_session::TurnEndReason;

use crate::prompt::render_goal_round_prompt;
use crate::store::{GoalBlockReason, GoalPhase, GoalRef, GoalStore, GoalView};

/// 目标变更事件（宿主在 GoalStore 提交后发；上游 `goal/changed` 的单进程面）。
#[derive(Clone, Debug)]
pub struct GoalChangedEvent {
    pub operation: &'static str,
}

/// 目标事件键（`EventBus` 上的本地事件）。
pub struct GoalChanged;
impl dsh_cordis::EmitEvent for GoalChanged {
    type Payload = GoalChangedEvent;
}

/// 一条预留轮的身份（上游 `RoundIdentity`）。
#[derive(Clone, Debug, PartialEq, Eq)]
struct RoundIdentity {
    goal_id: String,
    revision: u64,
    round: u64,
}

/// 一条排队/认领/承认的续跑消息（上游 `RoundAttempt`）。
#[derive(Clone, Debug)]
struct RoundAttempt {
    identity: RoundIdentity,
    message_id: MessageId,
    content: Vec<ContentBlock>,
    phase: RoundPhase,
    cancelled: bool,
    stale: bool,
}

/// 预留轮阶段（上游 `'queued' | 'claimed' | 'admitted'`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RoundPhase {
    Queued,
    Claimed,
    Admitted,
}

/// 驱动器内部状态（上游 `DriverState` 的单 agent 形）。
#[derive(Default)]
struct DriverState {
    attempt: Option<RoundAttempt>,
    /// 有竞争性排队输入（非本驱动注入的 next-turn 消息）。
    competing_queued: bool,
    /// 有待检查点（上游 needsCheckpoint——rustdsh 的提交缝是同步的，恒 false，
    /// 保留字段为后续异步落盘留位）。
    needs_checkpoint: bool,
    stopping: bool,
}

/// 消息源是否为正轮数的 goal 续跑（上游 `isGoalRoundSource`）。
fn is_goal_round_source(source: &MessageSource) -> bool {
    matches!(source, MessageSource::Goal { round, .. } if *round > 0)
}

/// 源与预留身份匹配（上游 `sameRound`）。
fn same_round(source: &MessageSource, identity: &RoundIdentity) -> bool {
    match source {
        MessageSource::Goal { goal_id, revision, round } => {
            *goal_id == identity.goal_id && *revision == identity.revision && *round == identity.round
        }
        _ => false,
    }
}

/// 消息与预留的完整比较（上游 `sameQueued`：内容深度相等）。
fn same_queued(message: &Message, attempt: &RoundAttempt) -> bool {
    is_goal_round_source(&message.source)
        && same_round(&message.source, &attempt.identity)
        && message.content == attempt.content
}

/// 目标是否可驱动（上游 `drivable`：active + armed）。
fn drivable(view: &Option<GoalView>) -> bool {
    view.as_ref().is_some_and(|g| g.phase == GoalPhase::Active && g.activation.is_armed())
}

/// 同会话目标轮驱动器。
pub struct GoalRoundDriver {
    store: Arc<GoalStore>,
    state: Mutex<DriverState>,
    /// followup 注入缝（宿主接 agent.send / inbox.append(NextTurn)）。
    followup: Mutex<Option<Arc<dyn Fn(&Message) -> bool + Send + Sync>>>,
    /// inbox 移除缝（上游 agent.inbox.remove）。
    remove: Mutex<Option<Arc<dyn Fn(&str) -> bool + Send + Sync>>>,
}

impl GoalRoundDriver {
    pub fn new(store: Arc<GoalStore>) -> Self {
        Self {
            store,
            state: Mutex::new(DriverState::default()),
            followup: Mutex::new(None),
            remove: Mutex::new(None),
        }
    }

    /// 注入 followup 缝（宿主接 agent-loop）。
    pub fn set_followup(&self, followup: Arc<dyn Fn(&Message) -> bool + Send + Sync>) {
        *self.followup.lock().unwrap() = Some(followup);
    }

    /// 注入 inbox 移除缝。
    pub fn set_remove(&self, remove: Arc<dyn Fn(&str) -> bool + Send + Sync>) {
        *self.remove.lock().unwrap() = Some(remove);
    }

    /// 驱动到稳定（上游 `requestDrive` 的 while 循环语义：每趟处理一个
    /// 状态转换——先清已处理的预留，再做下一轮决策）。
    ///
    /// 返回最后一趟是否发起了预留。
    pub fn pump(&self) -> bool {
        // 第一趟：若还有已处理（admitted）的预留，清掉它。
        if self.state.lock().unwrap().attempt.is_some() {
            let _ = self.drive_pass();
        }
        // 第二趟：真正的决策趟。
        self.drive_pass()
    }

    /// 目标静默检查 + 至多预留一个下一轮（上游 `drive`）。
    ///
    /// 返回是否发起了预留（测试与宿主观察用）。
    pub fn drive_pass(&self) -> bool {
        {
            let mut state = self.state.lock().unwrap();
            if state.stopping {
                return false;
            }
            // 已有预留：兑现它（交给 followup 的消息已在队列）——本 pass 让位。
            if state.attempt.is_some() {
                let attempt = state.attempt.take().expect("已判 Some");
                state.needs_checkpoint = true;
                let _ = attempt;
                return false;
            }
        }
        let goal = match self.store.get() {
            Ok(view) => view,
            Err(_) => return false,
        };
        if !drivable(&goal) {
            return false;
        }
        let goal = goal.expect("drivable 已判 Some");
        // 预算尽：自动 block（上游 round-limit code 逐字）。
        if goal.rounds_started >= goal.max_goal_rounds {
            let _ = self.store.block(
                &GoalRef { id: goal.id.clone(), revision: goal.revision },
                GoalBlockReason {
                    code: "round-limit".into(),
                    message: format!("Goal reached its configured limit of {} rounds.", goal.max_goal_rounds),
                },
            );
            return false;
        }

        let round = goal.rounds_started + 1;
        let content = render_goal_round_prompt(&goal, round);
        let message = Message {
            id: MessageId::generate(),
            role: Role::User,
            content: content.clone(),
            source: MessageSource::Goal {
                goal_id: goal.id.clone(),
                revision: goal.revision,
                round,
            },
        };
        let attempt = RoundAttempt {
            identity: RoundIdentity { goal_id: goal.id.clone(), revision: goal.revision, round },
            message_id: message.id.clone(),
            content,
            phase: RoundPhase::Queued,
            cancelled: false,
            stale: false,
        };
        {
            let mut state = self.state.lock().unwrap();
            state.attempt = Some(attempt);
        }
        let queued = match self.followup.lock().unwrap().as_ref() {
            Some(followup) => followup(&message),
            None => false,
        };
        if !queued {
            // 排队失败：清预留并按上游 queue-failed 语义 block。
            let mut state = self.state.lock().unwrap();
            state.attempt = None;
            if let Ok(Some(latest)) = self.store.get() {
                if latest.id == goal.id
                    && latest.revision == goal.revision
                    && latest.phase == GoalPhase::Active
                    && latest.activation.is_armed()
                {
                    let _ = self.store.block(
                        &GoalRef { id: latest.id.clone(), revision: latest.revision },
                        GoalBlockReason {
                            code: "queue-failed".into(),
                            message: format!("Could not queue goal round {round}: followup slot is not installed"),
                        },
                    );
                }
            }
        }
        queued
    }

    /// `agent/status` → idle 的静默处理（上游 `agent/status` 分支）。
    ///
    /// 返回「是否应请求驱动」（宿主据此调 [`Self::drive_pass`]）。
    pub fn on_idle(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        state.competing_queued = false;
        let attempt = state.attempt.clone();
        let goal = self.store.get().ok().flatten();
        // 暂停 fence：精确对齐被放弃的预留 ref（上游注释：host pause 后立即
        // resume 会 bump revision，不得再暂停已恢复的目标）。
        let pause = attempt.is_some()
            && attempt.as_ref().is_some_and(|a| {
                matches!(a.phase, RoundPhase::Queued | RoundPhase::Claimed) || a.cancelled
            })
            && goal.as_ref().is_some_and(|g| {
                g.phase == GoalPhase::Active
                    && g.activation.is_armed()
                    && attempt
                        .as_ref()
                        .is_some_and(|a| a.identity.goal_id == g.id && a.identity.revision == g.revision)
            });
        // 仍在排队的预留到不了静默：撤回（别把排在后面的人类输入困住）。
        let withdraw = attempt.as_ref().is_some_and(|a| a.phase == RoundPhase::Queued);
        if pause || withdraw {
            state.attempt = None;
            drop(state);
            if let Some(attempt) = &attempt {
                if attempt.phase == RoundPhase::Queued {
                    if let Some(remove) = self.remove.lock().unwrap().as_ref() {
                        remove(attempt.message_id.as_str());
                    }
                }
            }
            if pause {
                if let Some(goal) = &goal {
                    let _ = self.store.pause(&GoalRef {
                        id: goal.id.clone(),
                        revision: goal.revision,
                    });
                }
            }
            return true;
        }
        true
    }

    /// `agent/inbox/inserted`：竞争性排队标记（上游分支）。
    pub fn on_inbox_inserted(&self, message: &Message) {
        let mut state = self.state.lock().unwrap();
        if let Some(attempt) = state.attempt.as_mut() {
            if same_queued(message, attempt) {
                return;
            }
        }
        state.competing_queued = true;
        if let Some(attempt) = state.attempt.as_mut() {
            if attempt.phase == RoundPhase::Queued {
                attempt.stale = true;
            }
        }
    }

    /// `agent/inbox/claimed`：预留进入 claimed 阶段。
    pub fn on_inbox_claimed(&self, message: &Message) {
        let mut state = self.state.lock().unwrap();
        if let Some(attempt) = state.attempt.as_mut() {
            if same_queued(message, attempt) {
                attempt.phase = RoundPhase::Claimed;
            }
        }
    }

    /// `agent/inbox/discarded`：预留标记取消。
    pub fn on_inbox_discarded(&self, message: &Message) {
        let mut state = self.state.lock().unwrap();
        if let Some(attempt) = state.attempt.as_mut() {
            if same_queued(message, attempt) {
                attempt.cancelled = true;
            }
        }
    }

    /// `user/message` 落盘（预留被承认 + 轮数推进）。
    ///
    /// 上游是 `applyGoalEvent` 的 user/message 分支；宿主在消息落盘钩子里
    /// 调这里（一轮一次）。
    pub fn on_message_admitted(&self, message: &Message) -> bool {
        let message_id = message.id.0.clone();
        {
            let mut state = self.state.lock().unwrap();
            if let Some(attempt) = state.attempt.as_mut() {
                if attempt.message_id.as_str() == message_id {
                    attempt.phase = RoundPhase::Admitted;
                }
            }
        }
        // 轮数推进走 store 的严格 admit 路径。
        if let MessageSource::Goal { goal_id, revision, round } = &message.source {
            self.store.admit_round(goal_id, *revision, *round).is_ok()
        } else {
            false
        }
    }

    /// `turn/end`（上游 session/event 的 turn/end 分支）。
    ///
    /// 返回 true = 应请求驱动。
    pub fn on_turn_end(&self, reason: &TurnEndReason) -> bool {
        match reason {
            // max-tokens：解除武装（耐久 phase 不动）。
            TurnEndReason::MaxTokens => {
                self.store.disarm();
                true
            }
            TurnEndReason::Aborted { .. } => {
                let mut state = self.state.lock().unwrap();
                if state
                    .attempt
                    .as_ref()
                    .is_some_and(|a| matches!(a.phase, RoundPhase::Claimed | RoundPhase::Admitted))
                {
                    if let Some(attempt) = state.attempt.as_mut() {
                        attempt.cancelled = true;
                    }
                    true
                } else {
                    drop(state);
                    self.store.disarm();
                    true
                }
            }
            _ => true,
        }
    }

    /// pre-step 预留有效性校验（上游 `validReservation` 的 fail-closed）。
    pub fn validate_reservation(&self, message: &Message) -> bool {
        let state = self.state.lock().unwrap();
        let Some(attempt) = state.attempt.as_ref() else {
            return false;
        };
        if state.stopping || attempt.phase != RoundPhase::Claimed || attempt.stale {
            return false;
        }
        if !same_queued(message, attempt) {
            return false;
        }
        drop(state);
        let goal = self.store.get().ok().flatten();
        let MessageSource::Goal { goal_id, revision, round } = &message.source else {
            return false;
        };
        goal.as_ref().is_some_and(|g| {
            g.id == *goal_id
                && g.revision == *revision
                && g.phase == GoalPhase::Active
                && g.activation.is_armed()
                && *round == g.rounds_started + 1
        })
    }

    /// pre-step 拒绝后的预留清理（上游 invalid 分支的 attempt 清理）。
    pub fn on_reservation_rejected(&self, message: &Message) {
        let mut state = self.state.lock().unwrap();
        if let Some(attempt) = state.attempt.as_ref() {
            if same_round(&message.source, &attempt.identity) {
                state.attempt = None;
            }
        }
    }

    /// 停止驱动（上游卸载分支：disarm + 撤回预留）。
    pub fn stop(&self) {
        let attempt = {
            let mut state = self.state.lock().unwrap();
            state.stopping = true;
            state.attempt.take()
        };
        // 撤回仍在排队的预留消息（上游 inbox.remove）。
        if let Some(attempt) = attempt {
            if attempt.phase == RoundPhase::Queued {
                if let Some(remove) = self.remove.lock().unwrap().as_ref() {
                    remove(attempt.message_id.as_str());
                }
            }
        }
        self.store.disarm();
    }
}

impl GoalActivationExt for crate::store::GoalActivation {
    fn is_armed(&self) -> bool {
        matches!(self, crate::store::GoalActivation::Armed)
    }
}

/// activation 便捷判定。
trait GoalActivationExt {
    fn is_armed(&self) -> bool;
}

/// 订阅 agent 事件并把驱动器接到 agent-loop（宿主接线用）。
///
/// 返回 disposers（宿主持有；drop 即全部摘除）。宿主还要：
/// - `GoalStore` 的提交缝里发 [`GoalChanged`]（请求驱动）；
/// - `turn/end` 与 `user/message` 落盘时调 [`GoalRoundDriver::on_turn_end`] /
///   [`GoalRoundDriver::on_message_admitted`]（宿主的事件 sink 里接线）。
pub fn attach_goal_round_driver(
    driver: Arc<GoalRoundDriver>,
    events: &EventBus,
) -> Vec<dsh_cordis::Disposer> {
    let mut disposers = Vec::new();
    // agent/status → idle：静默处理 + 请求驱动。
    {
        let driver = Arc::clone(&driver);
        disposers.push(events.on_emit::<AgentStatusChanged>(move |status: &AgentStatus| {
            if matches!(status, AgentStatus::Idle) {
                if driver.on_idle() {
                    driver.drive_pass();
                }
            }
        }));
    }
    // agent/inbox/inserted：竞争性排队标记。
    {
        let driver = Arc::clone(&driver);
        disposers.push(events.on_emit::<AgentInboxInserted>(move |message: &Message| {
            driver.on_inbox_inserted(message);
        }));
    }
    // agent/inbox/claimed：预留进入 claimed 阶段。
    {
        let driver = Arc::clone(&driver);
        disposers.push(events.on_emit::<AgentInboxClaimed>(move |payload: &InboxClaimedPayload| {
            driver.on_inbox_claimed(&payload.message);
        }));
    }
    // agent/inbox/discarded：预留标记取消。
    {
        let driver = Arc::clone(&driver);
        disposers.push(events.on_emit::<AgentInboxDiscarded>(move |message: &Message| {
            driver.on_inbox_discarded(message);
        }));
    }
    // agent/pre-step waterfall：预留 fail-closed 校验（无效即拒绝该步）。
    {
        let driver = Arc::clone(&driver);
        disposers.push(events.on_waterfall::<AgentPreStep>(
            move |input: PreStepInput, next: BoxFuture<PreStepDecision>| {
                let driver = Arc::clone(&driver);
                Box::pin(async move {
                    let submitted = input
                        .messages
                        .iter()
                        .find(|m| is_goal_round_source(&m.source))
                        .cloned();
                    let Some(submitted) = submitted else {
                        return next.await;
                    };
                    if !driver.validate_reservation(&submitted) {
                        driver.on_reservation_rejected(&submitted);
                        return PreStepDecision::Reject;
                    }
                    let decision = next.await;
                    match decision {
                        PreStepDecision::Enter { messages } => {
                            // 落盘前的最终复核（上游 post-decision check）。
                            if messages
                                .iter()
                                .any(|m| m.id == submitted.id && !driver.validate_reservation(m))
                            {
                                driver.on_reservation_rejected(&submitted);
                                return PreStepDecision::Reject;
                            }
                            PreStepDecision::Enter { messages }
                        }
                        reject @ PreStepDecision::Reject => {
                            driver.on_reservation_rejected(&submitted);
                            reject
                        }
                    }
                })
            },
        ));
    }
    disposers
}