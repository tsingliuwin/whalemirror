//! GoalStore 的服务面契约：七操作闭集、CAS、激活流转、耐久缝、回放。

use std::sync::{Arc, Mutex};

use dsh_goal::store::{CreateGoalRequest, EditGoalRequest, GoalActivation, GoalBlockReason, GoalPhase, GoalRef, GoalStore};
use dsh_session::goal::GoalChange;

/// 收集提交缝落下的变更。
#[derive(Default)]
struct Recorder {
    changes: Mutex<Vec<GoalChange>>,
}

fn store_with_recorder() -> (Arc<GoalStore>, Arc<Recorder>) {
    let store = Arc::new(GoalStore::new());
    let recorder = Arc::new(Recorder::default());
    let sink_recorder = Arc::clone(&recorder);
    store.set_sink(Arc::new(move |change: &GoalChange| {
        sink_recorder.changes.lock().unwrap().push(change.clone());
    }));
    (store, recorder)
}

fn create(store: &GoalStore, objective: &str, rounds: Option<u64>) -> dsh_goal::store::GoalView {
    store
        .create(
            CreateGoalRequest {
                objective: objective.into(),
                max_goal_rounds: rounds,
            },
            256,
        )
        .expect("create")
}

fn reference(view: &dsh_goal::store::GoalView) -> GoalRef {
    GoalRef { id: view.id.clone(), revision: view.revision }
}

#[test]
fn create_then_get_returns_armed_active_goal() {
    let (store, recorder) = store_with_recorder();
    let view = create(&store, "ship the goal surface", Some(8));
    assert_eq!(view.revision, 1);
    assert_eq!(view.phase, GoalPhase::Active);
    assert_eq!(view.max_goal_rounds, 8);
    assert_eq!(view.activation, GoalActivation::Armed);
    // 落了 create 变更。
    assert_eq!(recorder.changes.lock().unwrap().len(), 1);
    assert_eq!(recorder.changes.lock().unwrap()[0].operation(), "create");
    // get 一致。
    assert_eq!(store.get().unwrap(), Some(view));
}

#[test]
fn create_rejects_bad_objective_and_rounds_with_exact_messages() {
    let (store, _) = store_with_recorder();
    let error = store
        .create(
            CreateGoalRequest { objective: "  ".into(), max_goal_rounds: None },
            256,
        )
        .expect_err("空 objective");
    assert_eq!(error.message, "goal objective must be a non-empty string");
    assert_eq!(error.code, "GOAL_INVALID_OBJECTIVE");

    let error = store
        .create(
            CreateGoalRequest { objective: "obj".into(), max_goal_rounds: Some(0) },
            256,
        )
        .expect_err("零轮上限");
    assert_eq!(error.message, "maxGoalRounds must be a positive safe integer");
    assert_eq!(error.code, "GOAL_INVALID_MAX_ROUNDS");
}

#[test]
fn create_rejects_existing_non_complete_goal() {
    let (store, _) = store_with_recorder();
    let first = create(&store, "one", None);
    let error = store
        .create(CreateGoalRequest { objective: "two".into(), max_goal_rounds: None }, 256)
        .expect_err("active 目标不能被覆盖");
    assert_eq!(error.code, "GOAL_ALREADY_EXISTS");
    assert_eq!(
        error.message,
        format!("goal \"{}\" already exists with phase \"active\"", first.id)
    );

    // complete 后可覆盖。
    store.complete(&reference(&first)).expect("complete");
    let second = create(&store, "two", None);
    assert_ne!(second.id, first.id);
}

#[test]
fn cas_rejects_stale_revision_with_exact_message() {
    let (store, _) = store_with_recorder();
    let view = create(&store, "obj", None);
    let stale = GoalRef { id: view.id.clone(), revision: 9 };
    let error = store.pause(&stale).expect_err("旧 revision");
    assert_eq!(error.code, "GOAL_STALE_REVISION");
    assert_eq!(
        error.message,
        format!(
            "stale goal ref \"{}\" revision 9; current is \"{}\" revision 1",
            view.id, view.id
        )
    );
    // 无目标时 NOT_FOUND。
    let error = store.pause(&GoalRef { id: "goal-x".into(), revision: 1 }).expect_err("id 不匹配当前目标");
    assert_eq!(error.code, "GOAL_STALE_REVISION");
    // 真正无目标（clear 后）：NOT_FOUND。
    store.clear(&GoalRef { id: view.id.clone(), revision: view.revision }).expect("clear");
    let error = store.pause(&GoalRef { id: "goal-x".into(), revision: 1 }).expect_err("无目标");
    assert_eq!(error.code, "GOAL_NOT_FOUND");
    assert_eq!(error.message, "no current goal");
}

#[test]
fn pause_resume_complete_block_clear_flow_with_activation() {
    let (store, recorder) = store_with_recorder();
    let view = create(&store, "obj", Some(8));

    // pause：armed → disarmed。
    let paused = store.pause(&reference(&view)).expect("pause");
    assert_eq!(paused.phase, GoalPhase::Paused);
    assert_eq!(paused.activation, GoalActivation::Disarmed);
    assert_eq!(paused.revision, 2);

    // resume：disarmed → armed（paused → active）。
    let resumed = store.resume(&reference(&paused)).expect("resume");
    assert_eq!(resumed.phase, GoalPhase::Active);
    assert_eq!(resumed.activation, GoalActivation::Armed);

    // block：active → blocked（带原因），disarmed。
    let blocked = store
        .block(
            &reference(&resumed),
            GoalBlockReason { code: "needs-input".into(), message: "等待输入".into() },
        )
        .expect("block");
    assert_eq!(blocked.phase, GoalPhase::Blocked);
    assert_eq!(
        blocked.blocked_reason.as_ref().map(|r| r.code.as_str()),
        Some("needs-input")
    );
    assert_eq!(blocked.activation, GoalActivation::Disarmed);

    // complete：blocked → complete。
    let complete = store.complete(&reference(&blocked)).expect("complete");
    assert_eq!(complete.phase, GoalPhase::Complete);

    // 重建 → clear。
    let second = create(&store, "obj2", None);
    let tombstone = store.clear(&reference(&second)).expect("clear");
    assert_eq!(tombstone.id, second.id);
    assert_eq!(tombstone.revision, second.revision + 1);
    assert!(store.get().unwrap().is_none());

    // 全部落了缝：create/pause/resume/block/complete/create/clear = 7。
    assert_eq!(recorder.changes.lock().unwrap().len(), 7);
}

#[test]
fn edit_requires_at_least_one_field_and_cannot_change_phase() {
    let (store, _) = store_with_recorder();
    let view = create(&store, "obj", Some(8));
    let error = store
        .edit(
            &reference(&view),
            EditGoalRequest { objective: None, max_goal_rounds: None },
        )
        .expect_err("空编辑");
    assert_eq!(error.code, "GOAL_INVALID_EDIT");
    assert_eq!(error.message, "goal edit requires objective and/or maxGoalRounds");

    let edited = store
        .edit(
            &reference(&view),
            EditGoalRequest { objective: Some("wider".into()), max_goal_rounds: Some(12) },
        )
        .expect("edit");
    assert_eq!(edited.objective, "wider");
    assert_eq!(edited.max_goal_rounds, 12);
    assert_eq!(edited.phase, GoalPhase::Active);
}

#[test]
fn invalid_transitions_use_exact_messages() {
    let (store, _) = store_with_recorder();
    let view = create(&store, "obj", None);

    // paused 后再 pause：非法。
    let paused = store.pause(&reference(&view)).expect("pause");
    let error = store.pause(&reference(&paused)).expect_err("paused 不能再 pause");
    assert_eq!(
        error.message,
        format!("cannot pause goal \"{}\" from phase \"paused\"; expected active", view.id)
    );

    // 非 active 不能 block（在 complete 前测，用 paused 的当前 ref）。
    let error = store
        .block(
            &reference(&paused),
            GoalBlockReason { code: "x".into(), message: "m".into() },
        )
        .expect_err("paused 不能 block");
    assert!(error.message.contains("cannot block goal"), "{}", error.message);

    // complete 后 resume：非法（complete 不在 resumable）。
    let complete = store.complete(&reference(&paused)).expect("complete");
    let error = store.resume(&reference(&complete)).expect_err("complete 不能 resume");
    assert_eq!(
        error.message,
        format!(
            "cannot resume goal \"{}\" from phase \"complete\"; expected active or paused or blocked",
            view.id
        )
    );
}

#[test]
fn resume_checks_armed_state_and_budget() {
    let (store, _) = store_with_recorder();
    let view = create(&store, "obj", None);
    // active 且 armed：resume 报 already。
    let error = store.resume(&reference(&view)).expect_err("armed 不能再 resume");
    assert_eq!(
        error.message,
        format!("goal \"{}\" is already active and armed", view.id)
    );

    // disarm 后 resume 允许（重武装）。
    store.disarm();
    let resumed = store.resume(&reference(&view)).expect("disarm 后可 resume");
    assert_eq!(resumed.activation, GoalActivation::Armed);
}

#[test]
fn block_reason_validation_is_verbatim() {
    let (store, _) = store_with_recorder();
    let view = create(&store, "obj", None);
    let error = store
        .block(
            &reference(&view),
            GoalBlockReason { code: "Bad_Code".into(), message: "m".into() },
        )
        .expect_err("非法 code");
    assert_eq!(
        error.message,
        "goal block reason requires a lower-kebab-case code and a non-empty message"
    );
    assert_eq!(error.code, "GOAL_INVALID_BLOCK_REASON");
}

#[test]
fn replay_restores_state_and_disarms() {
    let (store, _) = store_with_recorder();
    let view = create(&store, "obj", None);
    store.pause(&reference(&view)).expect("pause");

    // 回放：从落下的变更重建（armed 状态不落盘 → 回放后 disarmed）。
    let events: Vec<(String, serde_json::Value)> = vec![(
        "goal/change".into(),
        dsh_session::goal::GoalChange::Create(dsh_session::goal::GoalSnapshotChange {
            goal: dsh_session::goal::GoalSnapshot {
                id: view.id.clone(),
                revision: 1,
                objective: "obj".into(),
                phase: GoalPhase::Active,
                blocked_reason: None,
                max_goal_rounds: 256,
            },
            rounds_started: 0,
            created_at: view.created_at,
            updated_at: view.updated_at,
        })
        .to_wire(),
    )];
    store.replay(events).expect("replay");
    let restored = store.get().unwrap().expect("有目标");
    assert_eq!(restored.id, view.id);
    // 激活是进程本地事实：回放后 disarmed（上游 resume/fork 边界同语义）。
    assert_eq!(restored.activation, GoalActivation::Disarmed);
}

#[test]
fn replay_failure_blocks_access() {
    let store = GoalStore::new();
    // 非法流：无目标的 pause。
    let bad = GoalChange::Pause(dsh_session::goal::GoalSnapshotChange {
        goal: dsh_session::goal::GoalSnapshot {
            id: "goal-1".into(),
            revision: 1,
            objective: "obj".into(),
            phase: GoalPhase::Paused,
            blocked_reason: None,
            max_goal_rounds: 4,
        },
        rounds_started: 0,
        created_at: 1000,
        updated_at: 1100,
    })
    .to_wire();
    let error = store.replay(vec![("goal/change".to_string(), bad)]).expect_err("非法流");
    assert!(error.contains("goal replay failed at session event 1"), "{error}");
    // 失败后 get 拒绝。
    let access = store.get().expect_err("失败后拒绝访问");
    assert!(access.message.contains("goal replay failed"), "{}", access.message);
}