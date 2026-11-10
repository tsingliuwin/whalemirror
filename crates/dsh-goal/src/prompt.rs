//! 续跑轮提示词（上游 `goal-round-driver/src/prompt.ts` 逐字）。

use dsh_llm::ContentBlock;

use crate::store::GoalView;

/// 渲染一条同会话续跑轮的完整指令（`<goal_round>` 形态）。
///
/// 上游保留在会话历史里的一条单块提示：目标（JSON 引号化）+ 轮次进度 +
/// 「继续工作、验证结果、完成前取证、按策略报 blocker」的纪律。
pub fn render_goal_round_prompt(goal: &GoalView, round: u64) -> Vec<ContentBlock> {
    let text = format!(
        "<goal_round>\nObjective: {}\nRound: {}/{}\n\nContinue working toward the objective in this same session. Treat the current workspace, tool results, and durable session state as authoritative; inspect them instead of assuming earlier narration is still current. Make concrete progress and verify the result. Before claiming completion, gather evidence that the whole objective is achieved, read the current goal, and mark it complete. If work remains, leave the goal active for the next round. Follow the configured goal-tool policy before reporting a blocker.\n</goal_round>",
        serde_json::json!(goal.objective),
        round,
        goal.max_goal_rounds
    );
    vec![ContentBlock::text(text)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{GoalActivation, GoalPhase};

    #[test]
    fn round_prompt_is_verbatim() {
        let view = GoalView {
            id: "goal-1".into(),
            revision: 1,
            objective: "ship the goal surface".into(),
            phase: GoalPhase::Active,
            blocked_reason: None,
            max_goal_rounds: 5,
            rounds_started: 2,
            created_at: 1,
            updated_at: 1,
            activation: GoalActivation::Armed,
        };
        let blocks = render_goal_round_prompt(&view, 3);
        assert_eq!(blocks.len(), 1);
        let text = match &blocks[0] {
            ContentBlock::Text { text } => text.clone(),
            _ => panic!("单文本块"),
        };
        assert!(text.starts_with("<goal_round>\nObjective: \"ship the goal surface\"\nRound: 3/5\n\n"));
        assert!(text.contains("Treat the current workspace, tool results, and durable session state as authoritative"));
        assert!(text.ends_with("</goal_round>"));
    }
}