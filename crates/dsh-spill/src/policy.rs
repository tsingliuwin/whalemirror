//! 工具结果外置策略（上游 `spill-policy` 的装配面）：
//! 判定 → 存全文 → 头尾保留 + 省略通知；任何一步失败都**保留原文**。

use dsh_llm::ContentBlock;
use dsh_tools::ToolExecutionResult;

use crate::retention::{estimate_tokens, retain};
use crate::local::save_text;
use crate::{SaveTextSpill, SpillSource};

/// 策略配置（上游 `maxInlineTokens`：估计 token 上限）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpillPolicy {
    pub max_inline_tokens: usize,
}

impl SpillPolicy {
    /// 上游：负数/非整数在装载期拒绝；0 合法（永不内联）。
    pub fn new(max_inline_tokens: usize) -> Result<Self, String> {
        Ok(Self { max_inline_tokens })
    }

    /// 对一次工具结果施加策略。返回是否真的外置了。
    ///
    /// 失败即保留原文（策略永不吞结果）：无属主、存储失败、通知放不下都
    /// 走这条路，只留下内部提示。
    pub fn apply(
        &self,
        session_id: &str,
        tool_name: &str,
        call_id: &str,
        result: &mut ToolExecutionResult,
    ) -> bool {
        if self.max_inline_tokens == 0 {
            return false;
        }
        // 只处理文本/图片块
        if result
            .content
            .iter()
            .any(|b| !matches!(b, ContentBlock::Text { .. } | ContentBlock::Image { .. }))
        {
            return false;
        }
        let joined = result_text(&result.content);
        if estimate_tokens(&joined) <= self.max_inline_tokens {
            return false;
        }
        if session_id.is_empty() {
            return false;
        }
        let source = SpillSource::Tool {
            tool_name: tool_name.to_string(),
            call_id: call_id.to_string(),
            label: "result".into(),
        };
        let probe_spill = match save_text(&SaveTextSpill {
            session_id: session_id.to_string(),
            source,
            suggested_name: format!("{tool_name}.txt"),
            content: joined.clone(),
        }) {
            Ok(spill) => spill,
            Err(_) => return false,
        };
        let total_bytes = joined.len();
        eprintln!("DBG joined_tokens={} notice_tokens={} total_bytes={}", estimate_tokens(&joined), estimate_tokens(&crate::retention::format_spill_notice(total_bytes, 0, &probe_spill)) + estimate_tokens(crate::retention::GAP), total_bytes);
        let _ = &probe_spill;
        match retain(&result.content, self.max_inline_tokens, &probe_spill, total_bytes) {
            Ok(Some(content)) => {
                result.content = content;
                true
            }
            // 预算内（理论上不会到这里）或通知放不下：保留原文
            Ok(None) | Err(_) => false,
        }
    }
}

/// 块拼接文本（图片渲染成可取回的路径行，与上游 fullText 同形）。
pub fn result_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.clone(),
            ContentBlock::Image { attachment, .. } => format!(
                "\n[Image: {:?}; {}; {}x{}. Use read_image to view it.]\n",
                attachment.attachment_id, attachment.media_type, attachment.width, attachment.height
            ),
            _ => String::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(text: String) -> ToolExecutionResult {
        ToolExecutionResult::text(text)
    }

    #[test]
    fn within_budget_is_untouched() {
        let policy = SpillPolicy::new(12_500).unwrap();
        let mut r = result("a".repeat(100));
        assert!(!policy.apply("session-a", "web_fetch", "call-1", &mut r));
        assert_eq!(r.content.len(), 1);
    }

    #[test]
    fn oversized_result_is_spilled_and_points_at_the_file() {
        let policy = SpillPolicy::new(200).unwrap();
        let mut r = result("x".repeat(10_000));
        assert!(policy.apply("session-a", "web_fetch", "call-1", &mut r));
        let text = result_text(&r.content);
        assert!(text.contains("Full formatted result stored at:"), "{text}");
        assert!(text.ends_with(')'));
        assert!(text.len() < 10_000);
        // 定位符可读回全文
        let locator = text
            .split("stored at: ")
            .nth(1)
            .unwrap()
            .split(". Use read")
            .next()
            .unwrap()
            .to_string();
        let full = std::fs::read_to_string(&locator).unwrap();
        assert_eq!(full.len(), 10_000);
    }

    #[test]
    fn zero_budget_disables_the_policy() {
        let policy = SpillPolicy::new(0).unwrap();
        let mut r = result("x".repeat(10_000));
        assert!(!policy.apply("session-a", "bash", "call-1", &mut r));
    }

    #[test]
    fn missing_owner_keeps_the_original() {
        let policy = SpillPolicy::new(200).unwrap();
        let mut r = result("x".repeat(10_000));
        assert!(!policy.apply("", "web_fetch", "call-1", &mut r));
        assert_eq!(result_text(&r.content).len(), 10_000);
    }

    #[test]
    fn mixed_blocks_are_never_rewritten() {
        let policy = SpillPolicy::new(200).unwrap();
        let mut r = ToolExecutionResult {
            content: vec![
                ContentBlock::text("x".repeat(10_000)),
                ContentBlock::ToolCall {
                    id: dsh_llm::CallId("c".into()),
                    name: "bash".into(),
                    arguments: "{}".into(),
                },
            ],
            is_error: false,
            concludes_turn: false,
            presentation: None,
        };
        assert!(!policy.apply("session-a", "bash", "call-1", &mut r));
    }
}
