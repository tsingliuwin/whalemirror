//! 执行期权限检查（上游 `tool-goal/src/authority.ts`）：目标工具的武装门。
//!
//! rustdsh 的等价面：上游从 `exec.agent` + `ctx.agents`（活体注册表）拿执行
//! 上下文；rustdsh 工具缝没有 agent 句柄，改为**显式提供判定材料**——宿主
//! 在调用侧收集「打开轮次内的事件 + 会话头来源 + 会话条目数」，打包成
//! [`AuthorityProbe`] 交给工具。判定逻辑本身是纯函数，与上游逐条对齐：
//!
//! - `require_direct_human`：创建/编辑/暂停/恢复需要**直接人类轮**——当前根
//!   agent 的打开轮次内出现 `kind=user` 的 `user/message`（子会话头不算）。
//! - `completion_authority`：complete/blocked 接受直接人类轮**或**恰好是当前
//!   目标的承认轮（goal 源、同 id、同 revision、round == roundsStarted）。

use dsh_llm::Message;

use crate::store::{GoalError, GoalView, codes};

/// 权限判定的输入材料（宿主在调用侧从 agent-loop 收集）。
#[derive(Clone, Debug, Default)]
pub struct AuthorityProbe {
    /// 打开轮次内（`open_turn_start_seq+1` 之后）的消息快照。
    ///
    /// 上游遍历不可变事件切面找 `user/message`；rustdsh 直接给消息列表
    /// （宿主从会话条目截取）。
    pub open_turn_messages: Vec<Message>,
    /// 本会话是否根 agent（子会话头 origin=subagent 不给人类权限）。
    pub is_root_session: bool,
}

/// 一次调用被授予的硬权限（上游 `GoalToolAuthority`）。
#[derive(Clone, Debug)]
pub enum CompletionAuthority {
    /// 直接人类输入。
    DirectHuman,
    /// 恰好是当前目标的承认轮。
    GoalRound { view: GoalView },
}

/// 打开轮次内是否有 host 证明的人类输入（上游 `hasDirectHumanInput`）。
fn has_direct_human_input(probe: &AuthorityProbe) -> bool {
    if !probe.is_root_session {
        return false;
    }
    probe.open_turn_messages.iter().any(|message| {
        matches!(&message.source, dsh_llm::MessageSource::User)
            && message.role == dsh_llm::Role::User
    })
}

/// 判定一条消息是否为恰好匹配当前目标承认轮的 goal 续跑消息
/// （上游 `isMatchingGoalRound` 的事件等价：消息源 + 轮次匹配）。
pub fn is_matching_goal_round(probe: &AuthorityProbe, view: &GoalView) -> bool {
    probe.open_turn_messages.iter().any(|message| {
        let dsh_llm::MessageSource::Goal { goal_id, revision, round } = &message.source else {
            return false;
        };
        *goal_id == view.id
            && *revision == view.revision
            && *round == view.rounds_started
            && message.role == dsh_llm::Role::User
    })
}

/// 要求直接人类轮（上游 `requireDirectHuman`；错误文案逐字）。
pub fn require_direct_human(probe: &AuthorityProbe) -> Result<(), GoalError> {
    if has_direct_human_input(probe) {
        return Ok(());
    }
    Err(GoalError::new(
        codes::TOOL_INVALID_UPDATE,
        "this goal operation requires a direct human turn on a top-level agent",
    ))
}

/// 解析 complete/blocked 的权限（上游 `completionAuthority`；错误文案逐字）。
pub fn completion_authority(
    probe: &AuthorityProbe,
    current: &Option<GoalView>,
) -> Result<CompletionAuthority, GoalError> {
    if has_direct_human_input(probe) {
        return Ok(CompletionAuthority::DirectHuman);
    }
    if let Some(view) = current {
        if is_matching_goal_round(probe, view) {
            return Ok(CompletionAuthority::GoalRound { view: view.clone() });
        }
    }
    Err(GoalError::new(
        codes::TOOL_INVALID_UPDATE,
        "complete and blocked require a direct human turn or the current goal round",
    ))
}

/// 上游 goalToolExecution 的 driver 边界检查等价：无打开轮次时的错误。
pub fn require_open_turn(probe: &AuthorityProbe) -> Result<(), GoalError> {
    // rustdsh 的探针由宿主收集：无打开轮次时宿主给空列表——与上游
    // openTurnStartSeq === null 的区分不可判，改为宿主不注册调用（工具面
    // 在无探针时拒绝）。保留此函数作为语义占位，供批③ round driver 接线时
    // 决定是否需要显式边界。
    let _ = probe;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::GoalPhase;
    use dsh_llm::{ContentBlock, Message, MessageId, MessageSource, Role};

    fn user_message(source: MessageSource) -> Message {
        Message {
            id: MessageId::new("m"),
            role: Role::User,
            content: vec![ContentBlock::text("hi")],
            source,
        }
    }

    fn view(rounds: u64) -> GoalView {
        GoalView {
            id: "goal-1".into(),
            revision: 1,
            objective: "obj".into(),
            phase: GoalPhase::Active,
            blocked_reason: None,
            max_goal_rounds: 5,
            rounds_started: rounds,
            created_at: 1,
            updated_at: 1,
            activation: crate::GoalActivation::Armed,
        }
    }

    #[test]
    fn direct_human_requires_root_and_user_source() {
        let probe = AuthorityProbe {
            open_turn_messages: vec![user_message(MessageSource::User)],
            is_root_session: true,
        };
        assert!(require_direct_human(&probe).is_ok());

        // 子会话不给权限。
        let sub = AuthorityProbe {
            open_turn_messages: probe.open_turn_messages.clone(),
            is_root_session: false,
        };
        assert!(require_direct_human(&sub).is_err());

        // 非 user 源不算人类输入。
        let tool = AuthorityProbe {
            open_turn_messages: vec![user_message(MessageSource::Tool {
                call_id: dsh_llm::CallId::new("c"),
            })],
            is_root_session: true,
        };
        assert!(require_direct_human(&tool).is_err());
    }

    #[test]
    fn goal_round_matches_exact_revision_and_round() {
        let probe = AuthorityProbe {
            open_turn_messages: vec![user_message(MessageSource::Goal {
                goal_id: "goal-1".into(),
                revision: 1,
                round: 2,
            })],
            is_root_session: true,
        };
        // roundsStarted=2 → 匹配。
        assert!(is_matching_goal_round(&probe, &view(2)));
        // roundsStarted=3 → 轮次不匹配。
        assert!(!is_matching_goal_round(&probe, &view(3)));

        // 旧 revision 不匹配。
        let stale = AuthorityProbe {
            open_turn_messages: vec![user_message(MessageSource::Goal {
                goal_id: "goal-1".into(),
                revision: 0,
                round: 2,
            })],
            is_root_session: true,
        };
        assert!(!is_matching_goal_round(&stale, &view(2)));
    }

    #[test]
    fn completion_authority_prefers_direct_human() {
        let probe = AuthorityProbe {
            open_turn_messages: vec![user_message(MessageSource::User)],
            is_root_session: true,
        };
        assert!(matches!(
            completion_authority(&probe, &None),
            Ok(CompletionAuthority::DirectHuman)
        ));

        // 无人类、无匹配轮：拒绝（文案逐字）。
        let empty = AuthorityProbe::default();
        let error = completion_authority(&empty, &None).expect_err("无权限要拒绝");
        assert_eq!(
            error.message,
            "complete and blocked require a direct human turn or the current goal round"
        );
    }
}