//! round driver 的契约：静默预留、竞争撤回、预算尽自动 block、fail-closed
//! 校验、事件接线全链（DoD：一个跨 3 轮的目标自动推进）。

use std::sync::{Arc, Mutex};

use dsh_agent::{AgentInboxClaimed, AgentInboxDiscarded, AgentInboxInserted, AgentPreStep, AgentStatus, AgentStatusChanged, InboxClaimedPayload, PreStepDecision, PreStepInput};
use dsh_cordis::EventBus;
use dsh_goal::driver::{GoalRoundDriver, attach_goal_round_driver};
use dsh_goal::store::{CreateGoalRequest, GoalActivation, GoalPhase, GoalRef, GoalStore};
use dsh_llm::{AbortSignal, ContentBlock, Message, MessageId, MessageSource, Role};
use dsh_session::TurnEndReason;

/// 假 inbox：followup/remove 的可观察后端。
#[derive(Default)]
struct FakeInbox {
    queued: Mutex<Vec<Message>>,
    removed: Mutex<Vec<String>>,
}

impl FakeInbox {
    fn followup(&self, message: &Message) -> bool {
        self.queued.lock().unwrap().push(message.clone());
        true
    }
}

fn driver_with_inbox() -> (Arc<GoalRoundDriver>, Arc<GoalStore>, Arc<FakeInbox>) {
    let store = Arc::new(GoalStore::new());
    let driver = Arc::new(GoalRoundDriver::new(Arc::clone(&store)));
    let inbox = Arc::new(FakeInbox::default());
    let inbox_followup = Arc::clone(&inbox);
    driver.set_followup(Arc::new(move |message: &Message| inbox_followup.followup(message)));
    let inbox_remove = Arc::clone(&inbox);
    driver.set_remove(Arc::new(move |id: &str| {
        let mut queued = inbox_remove.queued.lock().unwrap();
        let before = queued.len();
        queued.retain(|m| m.id.as_str() != id);
        inbox_remove.removed.lock().unwrap().push(id.to_string());
        queued.len() != before
    }));
    (driver, store, inbox)
}

fn create_goal(store: &GoalStore, objective: &str, max_rounds: u64) -> dsh_goal::store::GoalView {
    store
        .create(
            CreateGoalRequest { objective: objective.into(), max_goal_rounds: Some(max_rounds) },
            256,
        )
        .expect("create")
}

fn goal_round_message(goal_id: &str, revision: u64, round: u64) -> Message {
    Message {
        id: MessageId::new(format!("m-{goal_id}-{round}")),
        role: Role::User,
        content: vec![ContentBlock::text("continue")],
        source: MessageSource::Goal {
            goal_id: goal_id.into(),
            revision,
            round,
        },
    }
}

#[test]
fn drive_pass_reserves_next_round_when_armed_and_active() {
    let (driver, store, inbox) = driver_with_inbox();
    let view = create_goal(&store, "obj", 5);
    assert!(driver.drive_pass(), "armed + active + 预算有余 → 预留下一轮");
    let queued = inbox.queued.lock().unwrap();
    assert_eq!(queued.len(), 1);
    // 预留消息的源：goal-1 的 revision 1、round 1。
    assert!(matches!(
        &queued[0].source,
        MessageSource::Goal { goal_id, revision, round }
            if *goal_id == view.id && *revision == 1 && *round == 1
    ));
    // 提示词形态。
    assert!(matches!(&queued[0].content[0], ContentBlock::Text { text } if text.starts_with("<goal_round>")));
}

#[test]
fn drive_pass_blocks_on_round_limit() {
    let (driver, store, inbox) = driver_with_inbox();
    // max=1：第 1 轮预留后承认，下一轮驱动时预算尽 → 自动 block。
    let view = create_goal(&store, "obj", 1);
    // 第 1 轮预留。
    assert!(driver.drive_pass());
    let queued_id = inbox.queued.lock().unwrap()[0].id.0.clone();
    // 兑现轮 1（承认）。
    driver.on_message_admitted(&inbox.queued.lock().unwrap()[0].clone());
    // 轮结束 → idle → 驱动到稳定（先清已承认预留，再决策：预算尽）。
    driver.on_turn_end(&TurnEndReason::Completed);
    assert!(driver.on_idle());
    let blocked = driver.pump();
    assert!(!blocked, "预算尽不再预留");
    let goal = store.get().unwrap().expect("有目标");
    assert_eq!(goal.phase, GoalPhase::Blocked);
    let reason = goal.blocked_reason.expect("必带原因");
    assert_eq!(reason.code, "round-limit");
    assert_eq!(reason.message, "Goal reached its configured limit of 1 rounds.");
    let _ = view;
}

#[test]
fn on_idle_withdraws_stale_queued_reservation_and_pauses() {
    let (driver, store, inbox) = driver_with_inbox();
    create_goal(&store, "obj", 5);
    assert!(driver.drive_pass());
    let queued_id = inbox.queued.lock().unwrap()[0].id.0.clone();
    // 静默时预留仍排队：撤回 + 暂停（上游 pause 分支：排队中的预留到不了
    // 静默，既撤回也别困住后面的人类输入；目标被暂停以等待人类）。
    driver.on_idle();
    assert!(inbox.removed.lock().unwrap().iter().any(|id| *id == queued_id));
    let goal = store.get().unwrap().expect("有目标");
    assert_eq!(goal.phase, GoalPhase::Paused, "排队预留到静默 → 上游 pause 语义");
    assert_eq!(goal.activation, GoalActivation::Disarmed);
}

#[test]
fn competing_queue_marks_reservation_stale() {
    let (driver, store, inbox) = driver_with_inbox();
    create_goal(&store, "obj", 5);
    assert!(driver.drive_pass());
    // 人类插队：预留标记 stale。
    let human = Message {
        id: MessageId::new("human-1"),
        role: Role::User,
        content: vec![ContentBlock::text("wait, do this instead")],
        source: MessageSource::User,
    };
    driver.on_inbox_inserted(&human);
    // 预留消息本身不算竞争（sameQueued 跳过）。
    let reserved = inbox.queued.lock().unwrap()[0].clone();
    driver.on_inbox_claimed(&reserved);
    // 竞争后的 pre-step 校验：stale 预留 fail-closed。
    assert!(!driver.validate_reservation(&reserved));
}

/// Message → InboxClaimedPayload 助手。
trait IntoClaimed {
    fn into_claimed(self) -> InboxClaimedPayload;
}

impl IntoClaimed for Message {
    fn into_claimed(self) -> InboxClaimedPayload {
        InboxClaimedPayload { message: self, turn: 1 }
    }
}

#[test]
fn turn_end_max_tokens_disarms_durable_phase_intact() {
    let (driver, store, _) = driver_with_inbox();
    create_goal(&store, "obj", 5);
    assert!(driver.drive_pass());
    driver.on_turn_end(&TurnEndReason::MaxTokens);
    let goal = store.get().unwrap().expect("有目标");
    assert_eq!(goal.phase, GoalPhase::Active, "耐久相位不动");
    assert_eq!(goal.activation, GoalActivation::Disarmed, "武装解除");
    // 解除后不再预留。
    assert!(!driver.drive_pass());
}

#[test]
fn full_three_round_autonomous_progression() {
    // DoD：跨 3 轮的目标自动推进到完成。
    // 每轮的真实序列：idle → 预留 → 认领 → 承认（落盘）→ 轮末 → idle。
    let (driver, store, inbox) = driver_with_inbox();
    let view = create_goal(&store, "ship three rounds", 3);
    let goal_id = view.id.clone();

    for round in 1..=3u64 {
        // 预留本轮（pump 先清前一轮的已承认预留，再决策）。
        assert!(driver.pump(), "第 {round} 轮预留");
        let queued = inbox.queued.lock().unwrap();
        let message = queued
            .iter()
            .find(|m| matches!(&m.source, MessageSource::Goal { round: r, .. } if *r == round))
            .cloned()
            .expect("本轮消息");
        drop(queued);
        // 认领（claimed）→ 有效性通过。
        driver.on_inbox_claimed(&message);
        assert!(driver.validate_reservation(&message), "第 {round} 轮预留有效");
        // 承认（落盘）→ 轮完成。
        driver.on_message_admitted(&message);
        if round == 3 {
            // 最后一轮模型报告完成（update_goal complete 走 GoalRound 权限）。
            let reference = GoalRef {
                id: goal_id.clone(),
                revision: store.get().unwrap().unwrap().revision,
            };
            store.complete(&reference).expect("complete");
        }
        driver.on_turn_end(&TurnEndReason::Completed);
    }

    let goal = store.get().unwrap().expect("有目标");
    assert_eq!(goal.phase, GoalPhase::Complete, "三轮后自动推进到完成");
    assert_eq!(goal.rounds_started, 3);
    assert_eq!(goal.activation, GoalActivation::Disarmed);
    // 完成后不再预留。
    assert!(!driver.drive_pass());
}

#[test]
fn stop_disarms_and_withdraws() {
    let (driver, store, inbox) = driver_with_inbox();
    create_goal(&store, "obj", 5);
    assert!(driver.drive_pass());
    driver.stop();
    let goal = store.get().unwrap().expect("有目标");
    assert_eq!(goal.activation, GoalActivation::Disarmed);
    assert!(inbox.queued.lock().unwrap().is_empty(), "停止清空预留");
    assert!(!driver.drive_pass());
}

#[tokio::test]
async fn event_bus_attachment_routes_inbox_events() {
    // 事件总线接线全链：status → idle 触发驱动；inserted 标竞争。
    let store = Arc::new(GoalStore::new());
    let driver = Arc::new(GoalRoundDriver::new(Arc::clone(&store)));
    let inbox = Arc::new(FakeInbox::default());
    let inbox_followup = Arc::clone(&inbox);
    driver.set_followup(Arc::new(move |message: &Message| inbox_followup.followup(message)));

    let events = EventBus::default();
    let _disposers = attach_goal_round_driver(Arc::clone(&driver), &events);

    create_goal(&store, "obj", 5);
    // idle 状态广播 → 驱动器自动预留。
    events.emit::<AgentStatusChanged>(AgentStatus::Idle);
    assert_eq!(inbox.queued.lock().unwrap().len(), 1);

    // 人类插队 → 竞争标记。
    let human = Message {
        id: MessageId::new("human-2"),
        role: Role::User,
        content: vec![ContentBlock::text("stop")],
        source: MessageSource::User,
    };
    events.emit::<AgentInboxInserted>(human);
    // 预留消息被认领（claimed 广播走 inbox 事件）。
    let reserved = inbox.queued.lock().unwrap()[0].clone();
    events.emit::<AgentInboxClaimed>(InboxClaimedPayload { message: reserved.clone(), turn: 1 });
    // pre-step waterfall：预留被标 stale（竞争插队）→ 拒绝。
    let decision = events
        .waterfall::<AgentPreStep>(
            PreStepInput {
                messages: vec![reserved],
                turn: 1,
                step: 1,
                signal: AbortSignal::new(),
            },
            |input| {
                Box::pin(async move { PreStepDecision::Enter { messages: input.messages } })
            },
        )
        .await;
    assert!(matches!(decision, PreStepDecision::Reject), "竞争后的预留要被拒");

    // discarded：预留取消标记。
    let discarded = Message {
        id: MessageId::new("m-x"),
        role: Role::User,
        content: vec![ContentBlock::text("x")],
        source: MessageSource::User,
    };
    let _ = events.emit::<AgentInboxDiscarded>(discarded);
    let _ = dsh_agent::AgentStatus::Idle;
}