//! 目标域折叠的严格契约：解码器逐字错误、状态机转移、承认轮、clear 墓碑、
//! id 复用拒绝。对齐上游 `goal.spec.ts`/`projection.spec.ts` 的关键用例。

use dsh_session::goal::{
    GoalChange, GoalPhase, GoalSnapshot, GoalSnapshotChange, apply_goal_change, apply_goal_event,
    decode_goal_change, fold_goal,
};
use serde_json::json;

fn snapshot(id: &str, revision: u64, phase: GoalPhase, objective: &str, max_rounds: u64) -> GoalSnapshot {
    GoalSnapshot {
        id: id.into(),
        revision,
        objective: objective.into(),
        phase,
        blocked_reason: None,
        max_goal_rounds: max_rounds,
    }
}

/// 与 `create_wire` 同一 objective 的便捷构造（`requireSameDefinition` 要求
/// 非 create 变更不得改 objective，测试数据必须与 create 一致）。
fn same_objective() -> &'static str {
    "ship the goal surface"
}

fn snapshot_change(goal: GoalSnapshot, rounds: u64, created: u64, updated: u64) -> GoalSnapshotChange {
    GoalSnapshotChange { goal, rounds_started: rounds, created_at: created, updated_at: updated }
}

/// 一个合法 create 变更的 wire JSON。
fn create_wire(id: &str) -> serde_json::Value {
    json!({
        "kind": "goal/change",
        "version": 1,
        "operation": "create",
        "goal": {
            "id": id,
            "revision": 1,
            "objective": "ship the goal surface",
            "phase": "active",
            "maxGoalRounds": 8,
        },
        "roundsStarted": 0,
        "createdAt": 1000,
        "updatedAt": 1000,
    })
}

#[test]
fn create_decodes_and_folds() {
    let change = decode_goal_change(&create_wire("goal-1")).expect("合法 create");
    let Some(GoalChange::Create(change)) = change else {
        panic!("create 变体");
    };
    assert_eq!(change.goal.revision, 1);
    assert_eq!(change.goal.phase, GoalPhase::Active);
    assert_eq!(change.goal.max_goal_rounds, 8);

    let folded = fold_goal(vec![("goal/change", &create_wire("goal-1"))]).expect("fold");
    assert_eq!(folded.goal.as_ref().map(|g| g.id.as_str()), Some("goal-1"));
    assert_eq!(folded.rounds_started, 0);
    assert_eq!(folded.created_at, Some(1000));
    assert_eq!(folded.updated_at, Some(1000));
}

#[test]
fn unrelated_events_are_ignored() {
    let folded = fold_goal(vec![
        ("turn/start", &json!({ "turn": 1 })),
        ("user/message", &json!({ "source": { "kind": "user" } })),
        ("assistant/message", &json!({ "turn": 1, "step": 1 })),
    ])
    .expect("无关事件不参与折叠");
    assert!(folded.goal.is_none());
    assert_eq!(folded.rounds_started, 0);
}

#[test]
fn decoder_rejects_wrong_version_and_extra_fields() {
    let wrong_version = json!({
        "kind": "goal/change",
        "version": 2,
        "operation": "create",
    });
    let error = decode_goal_change(&wrong_version).expect_err("版本不符要拒绝");
    assert!(error.contains("unsupported goal change version 2"), "{error}");

    let extra = json!({
        "kind": "goal/change", "version": 1, "operation": "create",
        "extra": true,
    });
    let error = decode_goal_change(&extra).expect_err("多余字段要拒绝");
    assert!(error.contains("must have exactly"), "{error}");

    let bad_operation = json!({
        "kind": "goal/change", "version": 1, "operation": "explode",
    });
    let error = decode_goal_change(&bad_operation).expect_err("未知操作要拒绝");
    assert_eq!(error, "goal change operation is invalid");
}

#[test]
fn decoder_rejects_bad_snapshots_with_exact_messages() {
    // objective 未归一（带尾空白）。
    let trailing = json!({
        "kind": "goal/change", "version": 1, "operation": "create",
        "goal": {
            "id": "goal-1", "revision": 1, "objective": " ship it ",
            "phase": "active", "maxGoalRounds": 8,
        },
        "roundsStarted": 0, "createdAt": 1000, "updatedAt": 1000,
    });
    let error = decode_goal_change(&trailing).expect_err("objective 要归一");
    assert_eq!(error, "goal change goal.objective must be non-empty and normalized");

    // phase 非法。
    let bad_phase = json!({
        "kind": "goal/change", "version": 1, "operation": "create",
        "goal": {
            "id": "goal-1", "revision": 1, "objective": "ship it",
            "phase": "limbo", "maxGoalRounds": 8,
        },
        "roundsStarted": 0, "createdAt": 1000, "updatedAt": 1000,
    });
    let error = decode_goal_change(&bad_phase).expect_err("phase 要闭集");
    assert_eq!(error, "goal change goal.phase is invalid");

    // blocked 缺 blockedReason（键集不匹配）。
    let blocked_without_reason = json!({
        "kind": "goal/change", "version": 1, "operation": "block",
        "goal": {
            "id": "goal-1", "revision": 2, "objective": "ship it",
            "phase": "blocked", "maxGoalRounds": 8,
        },
        "roundsStarted": 0, "createdAt": 1000, "updatedAt": 1001,
    });
    let error = decode_goal_change(&blocked_without_reason).expect_err("blocked 必带原因");
    assert!(
        error.contains("goal for phase blocked must have exactly blockedReason,id,maxGoalRounds,objective,phase,revision"),
        "{error}"
    );

    // blockedReason code 非 kebab。
    let bad_code = json!({
        "kind": "goal/change", "version": 1, "operation": "block",
        "goal": {
            "id": "goal-1", "revision": 2, "objective": "ship it",
            "phase": "blocked", "maxGoalRounds": 8,
            "blockedReason": { "code": "Bad_Code", "message": "stuck" },
        },
        "roundsStarted": 0, "createdAt": 1000, "updatedAt": 1001,
    });
    let error = decode_goal_change(&bad_code).expect_err("code 要 kebab");
    assert_eq!(error, "goal change goal.blockedReason.code must be lower-kebab-case");

    // 非法 blockedReason 消息（尾空白未归一）。
    let bad_message = json!({
        "kind": "goal/change", "version": 1, "operation": "block",
        "goal": {
            "id": "goal-1", "revision": 2, "objective": "ship it",
            "phase": "blocked", "maxGoalRounds": 8,
            "blockedReason": { "code": "missing-input", "message": " stuck " },
        },
        "roundsStarted": 0, "createdAt": 1000, "updatedAt": 1001,
    });
    let error = decode_goal_change(&bad_message).expect_err("message 要归一");
    assert_eq!(
        error,
        "goal change goal.blockedReason.message must be non-empty and normalized"
    );

    // updatedAt 早于 createdAt。
    let backwards = json!({
        "kind": "goal/change", "version": 1, "operation": "create",
        "goal": {
            "id": "goal-1", "revision": 1, "objective": "ship it",
            "phase": "active", "maxGoalRounds": 8,
        },
        "roundsStarted": 0, "createdAt": 2000, "updatedAt": 1000,
    });
    let error = decode_goal_change(&backwards).expect_err("时间戳要单调");
    assert_eq!(error, "goal change updatedAt cannot precede createdAt");
}

#[test]
fn full_lifecycle_folds() {
    let mut events = vec![("goal/change", create_wire("goal-1"))];

    let edit = json!({
        "kind": "goal/change", "version": 1, "operation": "edit",
        "goal": {
            "id": "goal-1", "revision": 2, "objective": "ship it wider",
            "phase": "active", "maxGoalRounds": 12,
        },
        "roundsStarted": 1, "createdAt": 1000, "updatedAt": 2000,
    });
    // 先承认第 1 轮再 edit（rounds 保存约束）。
    let round = json!({
        "id": "m-1", "role": "user",
        "content": [{ "type": "text", "text": "continue" }],
        "source": { "kind": "goal", "goalId": "goal-1", "revision": 1, "round": 1 },
    });
    events.push(("user/message", round));
    events.push(("goal/change", edit));

    let folded = fold_goal(events.iter().map(|(t, d)| (*t, d)).collect::<Vec<_>>()).expect("fold");
    let goal = folded.goal.expect("目标还在");
    assert_eq!(goal.revision, 2);
    assert_eq!(goal.objective, "ship it wider");
    assert_eq!(goal.max_goal_rounds, 12);
    assert_eq!(folded.rounds_started, 1);
    assert_eq!(folded.updated_at, Some(2000));
}

#[test]
fn pause_resume_block_complete_transitions() {
    // create → pause → resume → block → resume → complete 全链。
    // objective 与 create_wire 保持一致（requireSameDefinition 的约束）。
    let mut state = dsh_session::goal::GoalFoldState::default();
    let create = decode_goal_change(&create_wire("goal-1")).unwrap().unwrap();
    apply_goal_change(&mut state, &create).expect("create");

    let base = snapshot("goal-1", 2, GoalPhase::Paused, same_objective(), 8);
    let pause = GoalChange::Pause(snapshot_change(base, 0, 1000, 1100));
    apply_goal_change(&mut state, &pause).expect("pause");
    assert_eq!(state.goal.as_ref().map(|g| g.phase), Some(GoalPhase::Paused));

    // resume：paused → active（预算有余）。
    let resume_goal = snapshot("goal-1", 3, GoalPhase::Active, same_objective(), 8);
    let resume = GoalChange::Resume(snapshot_change(resume_goal, 0, 1000, 1200));
    apply_goal_change(&mut state, &resume).expect("resume");

    // block：active → blocked，带原因。
    let blocked = GoalSnapshot {
        blocked_reason: Some(dsh_session::goal::GoalBlockReason {
            code: "missing-input".into(),
            message: "需要用户输入".into(),
        }),
        ..snapshot("goal-1", 4, GoalPhase::Blocked, same_objective(), 8)
    };
    let block = GoalChange::Block(snapshot_change(blocked, 0, 1000, 1300));
    apply_goal_change(&mut state, &block).expect("block");

    // resume：blocked → active。
    let resume2 = GoalChange::Resume(snapshot_change(
        snapshot("goal-1", 5, GoalPhase::Active, same_objective(), 8),
        0,
        1000,
        1400,
    ));
    apply_goal_change(&mut state, &resume2).expect("resume after block");

    // complete：active → complete。
    let complete = GoalChange::Complete(snapshot_change(
        snapshot("goal-1", 6, GoalPhase::Complete, same_objective(), 8),
        0,
        1000,
        1500,
    ));
    apply_goal_change(&mut state, &complete).expect("complete");
    assert_eq!(state.goal.as_ref().map(|g| g.phase), Some(GoalPhase::Complete));
}

#[test]
fn invalid_transitions_fail_loudly() {
    let mut state = dsh_session::goal::GoalFoldState::default();
    let create = decode_goal_change(&create_wire("goal-1")).unwrap().unwrap();
    apply_goal_change(&mut state, &create).expect("create");

    // active → complete：合法。
    let skip = GoalChange::Complete(snapshot_change(
        snapshot("goal-1", 2, GoalPhase::Complete, same_objective(), 8),
        0,
        1000,
        1100,
    ));
    apply_goal_change(&mut state, &skip).expect("active → complete 合法");

    // complete 后再 complete：非法（current 已 complete）。
    let again = GoalChange::Complete(snapshot_change(
        snapshot("goal-1", 3, GoalPhase::Complete, same_objective(), 8),
        0,
        1000,
        1200,
    ));
    let error = apply_goal_change(&mut state, &again).expect_err("complete 后不能再 complete");
    assert_eq!(error, "goal complete has an invalid phase transition");

    // pause 已 complete 的目标：非法。
    let pause = GoalChange::Pause(snapshot_change(
        snapshot("goal-1", 3, GoalPhase::Paused, same_objective(), 8),
        0,
        1000,
        1200,
    ));
    let error = apply_goal_change(&mut state, &pause).expect_err("complete 不能 pause");
    assert_eq!(error, "goal pause has an invalid phase transition");
}

#[test]
fn revision_must_advance_by_exactly_one() {
    let mut state = dsh_session::goal::GoalFoldState::default();
    let create = decode_goal_change(&create_wire("goal-1")).unwrap().unwrap();
    apply_goal_change(&mut state, &create).expect("create");

    // 跳两档：拒绝。
    let jump = GoalChange::Pause(snapshot_change(
        snapshot("goal-1", 3, GoalPhase::Paused, same_objective(), 8),
        0,
        1000,
        1100,
    ));
    let error = apply_goal_change(&mut state, &jump).expect_err("跳档要拒绝");
    assert_eq!(error, "goal pause must advance the current goal by one revision");

    // 换 id：拒绝。
    let other = GoalChange::Pause(snapshot_change(
        snapshot("goal-2", 2, GoalPhase::Paused, same_objective(), 8),
        0,
        1000,
        1100,
    ));
    let error = apply_goal_change(&mut state, &other).expect_err("换 id 要拒绝");
    assert_eq!(error, "goal pause must advance the current goal by one revision");
}

#[test]
fn non_create_counters_must_be_preserved() {
    let mut state = dsh_session::goal::GoalFoldState::default();
    let create = decode_goal_change(&create_wire("goal-1")).unwrap().unwrap();
    apply_goal_change(&mut state, &create).expect("create");

    // roundsStarted 与当前不一致：拒绝。
    let inconsistent = GoalChange::Pause(snapshot_change(
        snapshot("goal-1", 2, GoalPhase::Paused, same_objective(), 8),
        5,
        1000,
        1100,
    ));
    let error = apply_goal_change(&mut state, &inconsistent).expect_err("计数要保存");
    assert_eq!(
        error,
        "goal pause does not preserve the current counters and timestamps"
    );

    // updatedAt 倒退：拒绝。
    let backwards = GoalChange::Pause(snapshot_change(
        snapshot("goal-1", 2, GoalPhase::Paused, same_objective(), 8),
        0,
        1000,
        900,
    ));
    let error = apply_goal_change(&mut state, &backwards).expect_err("时间戳要单调");
    assert_eq!(
        error,
        "goal pause does not preserve the current counters and timestamps"
    );
}

#[test]
fn goal_rounds_admit_strictly() {
    let mut state = dsh_session::goal::GoalFoldState::default();
    let create = decode_goal_change(&create_wire("goal-1")).unwrap().unwrap();
    apply_goal_change(&mut state, &create).expect("create");

    // 第 1 轮：合法。
    let round_one = json!({
        "source": { "kind": "goal", "goalId": "goal-1", "revision": 1, "round": 1 },
    });
    apply_goal_event(&mut state, "user/message", &round_one, 5).expect("第 1 轮合法");
    assert_eq!(state.rounds_started, 1);

    // 跳轮（round 3）：拒绝。
    let round_three = json!({
        "source": { "kind": "goal", "goalId": "goal-1", "revision": 1, "round": 3 },
    });
    let error = apply_goal_event(&mut state, "user/message", &round_three, 6)
        .expect_err("跳轮要拒绝");
    assert!(
        error.contains("goal round at session event 6 is not the next admitted round"),
        "{error}"
    );

    // 旧 revision：goalSource 层就拒绝（上游 revision < 1 报 source invalid，
    // 而不是 next-round 校验）。
    let stale = json!({
        "source": { "kind": "goal", "goalId": "goal-1", "revision": 0, "round": 2 },
    });
    let error = apply_goal_event(&mut state, "user/message", &stale, 7).expect_err("旧 revision 拒绝");
    assert_eq!(error, "goal message source is invalid");

    // 无 goal 源的 user/message：不参与折叠。
    let plain = json!({ "source": { "kind": "user" } });
    apply_goal_event(&mut state, "user/message", &plain, 8).expect("普通消息忽略");
    assert_eq!(state.rounds_started, 1);
}

#[test]
fn clear_tombstone_resets_and_ids_are_never_reused() {
    let mut state = dsh_session::goal::GoalFoldState::default();
    let create = decode_goal_change(&create_wire("goal-1")).unwrap().unwrap();
    apply_goal_change(&mut state, &create).expect("create");

    // clear：墓碑 revision + 1、时间戳不早于当前 updatedAt。
    let clear = GoalChange::Clear(dsh_session::goal::GoalClear {
        cleared: dsh_session::goal::GoalRef {
            id: "goal-1".into(),
            revision: 2,
        },
        cleared_at: 1500,
    });
    apply_goal_change(&mut state, &clear).expect("clear");
    assert!(state.goal.is_none());
    assert_eq!(state.rounds_started, 0);
    assert!(state.created_at.is_none());

    // 复用同一 id create：拒绝（seenGoalIds）。
    let reuse = decode_goal_change(&create_wire("goal-1")).unwrap().unwrap();
    let error = apply_goal_change(&mut state, &reuse).expect_err("id 复用要拒绝");
    assert_eq!(
        error,
        "goal create requires a fresh active revision-one goal with zero rounds"
    );

    // clear 无当前目标：拒绝。
    let clear_again = GoalChange::Clear(dsh_session::goal::GoalClear {
        cleared: dsh_session::goal::GoalRef { id: "goal-1".into(), revision: 3 },
        cleared_at: 1600,
    });
    let error = apply_goal_change(&mut state, &clear_again).expect_err("clear 要当前目标");
    assert_eq!(error, "goal clear requires a current goal");

    // clear 时间戳早于 updatedAt：拒绝。
    let mut state2 = dsh_session::goal::GoalFoldState::default();
    let create2 = decode_goal_change(&create_wire("goal-9")).unwrap().unwrap();
    apply_goal_change(&mut state2, &create2).expect("create");
    let early = GoalChange::Clear(dsh_session::goal::GoalClear {
        cleared: dsh_session::goal::GoalRef { id: "goal-9".into(), revision: 2 },
        cleared_at: 999,
    });
    let error = apply_goal_change(&mut state2, &early).expect_err("倒退时间戳要拒绝");
    assert_eq!(
        error,
        "goal clear timestamp cannot precede the current goal update"
    );
}

#[test]
fn completed_goal_can_be_replaced() {
    let mut state = dsh_session::goal::GoalFoldState::default();
    let create = decode_goal_change(&create_wire("goal-1")).unwrap().unwrap();
    apply_goal_change(&mut state, &create).expect("create");
    let complete = GoalChange::Complete(snapshot_change(
        snapshot("goal-1", 2, GoalPhase::Complete, same_objective(), 8),
        0,
        1000,
        1100,
    ));
    apply_goal_change(&mut state, &complete).expect("complete");

    // 完成后可以再 create 新目标。
    let second = decode_goal_change(&create_wire("goal-2")).unwrap().unwrap();
    apply_goal_change(&mut state, &second).expect("完成后可再建");
    assert_eq!(state.goal.as_ref().map(|g| g.id.as_str()), Some("goal-2"));

    // 未完成时不能覆盖。
    let third = decode_goal_change(&create_wire("goal-3")).unwrap().unwrap();
    let error = apply_goal_change(&mut state, &third).expect_err("active 不能被覆盖");
    assert_eq!(
        error,
        "goal create requires a fresh active revision-one goal with zero rounds"
    );
}

#[test]
fn wire_round_trip_preserves_every_field() {
    let change = decode_goal_change(&create_wire("goal-rt")).unwrap().unwrap();
    let serialized = change.to_wire();
    let reparsed = decode_goal_change(&serialized).expect("round trip decode").expect("some");
    assert_eq!(reparsed, change);

    // blocked 形态的往返（带 blockedReason）。
    let blocked_wire = json!({
        "kind": "goal/change", "version": 1, "operation": "block",
        "goal": {
            "id": "goal-rt", "revision": 2, "objective": "ship it",
            "phase": "blocked", "maxGoalRounds": 8,
            "blockedReason": { "code": "needs-input", "message": "等待用户输入" },
        },
        "roundsStarted": 1, "createdAt": 1000, "updatedAt": 1100,
    });
    // blocked 变更不能凭空折叠（需要 active 前置），这里只测 wire 往返。
    let blocked = decode_goal_change(&blocked_wire).expect("decode").expect("some");
    let serialized = blocked.to_wire();
    let reparsed = decode_goal_change(&serialized).expect("decode").expect("some");
    assert_eq!(reparsed, blocked);
}