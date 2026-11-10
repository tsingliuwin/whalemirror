//! 终局目标变更后的模型可见收尾指令（上游 `tool-goal/src/wrapup.ts` 逐字）。
//!
//! 自主轮报告 complete/blocked 后注入：取代旧的硬停轮，让模型在轮结束前
//! 仍然对用户说一次话。

/// 渲染收尾上下文（`<goal_complete>` / `<goal_blocked>` 两种形态）。
pub fn render_wrapup_context(objective: &str, blocked_reason: Option<&str>) -> String {
    let heading = format!("Objective: {}\n", serde_json::json!(objective));
    let grounding = "Report only what earlier rounds and tool results in this session actually establish; \
when a detail is not in the session, say so instead of inventing it. ";
    match blocked_reason {
        None => format!(
            "<goal_complete>\n{heading}\
The goal is marked complete and this autonomous run is ending. Write the closing \
message to the user now: state the outcome, summarize what was done and how it was \
verified, and point to the concrete results (files, commits, or other artifacts). {grounding}\
Note anything the user should review or do next. Address the user directly. Do not \
call any more tools in this run; further work waits for the user's next instruction.\n</goal_complete>"
        ),
        Some(reason) => format!(
            "<goal_blocked>\n{heading}\
Blocked: {}\n\
The goal is marked blocked and this autonomous run is ending. Write the closing \
message to the user now: state what has been completed so far, describe the concrete \
blocking condition and what you tried, and say exactly what you need from the user to \
continue. {grounding}\
Address the user directly. Do not call any more tools in this run; further work \
waits for the user's next instruction.\n</goal_blocked>",
            serde_json::json!(reason)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_form_carries_objective_and_grounding() {
        let text = render_wrapup_context("ship the goal surface", None);
        assert!(text.starts_with("<goal_complete>\nObjective: \"ship the goal surface\"\n"));
        assert!(text.contains("state the outcome, summarize what was done"));
        assert!(text.contains("say so instead of inventing it"));
        assert!(text.ends_with("</goal_complete>"));
        // 不带 blocked 行。
        assert!(!text.contains("Blocked:"));
    }

    #[test]
    fn blocked_form_carries_quoted_reason() {
        let text = render_wrapup_context("ship it", Some("需要用户输入"));
        assert!(text.starts_with("<goal_blocked>\nObjective: \"ship it\"\n"));
        assert!(text.contains("Blocked: \"需要用户输入\"\n"));
        assert!(text.contains("say exactly what you need from the user"));
        assert!(text.ends_with("</goal_blocked>"));
    }
}