//! 压缩分隔行的展示事实（上游 ui-chat `CompactionItem` + `compaction/summary`）。
//!
//! 上游把 `compaction/summary` 事件折成一行可展开的分隔条：
//! `上下文已压缩 · 已压缩 {items} 条历史记录（约 {tokens} tokens）`，
//! 有摘要时展开体呈现摘要原文（inline markdown），没有摘要则禁用展开。
//! rustdsh 此前只画一条固定文案的居中发丝，既没有条目/用量数字，也没有
//! 摘要可查。文案与数值口径照抄 web locale（`message.compaction*`）。

/// 一行压缩分隔条的展示事实。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompactionRowFacts {
    /// 被影子区替换的历史条目数（上游 `shadowedSeqs.length`）。
    pub shadowed_items: u64,
    /// 影子区估算 token 数（上游 `shadowedTokenCount`）。
    pub shadowed_tokens: u64,
    /// 摘要原文；空串 = 摘要不可用（上游 `summary === null`）。
    pub summary: String,
}

impl CompactionRowFacts {
    /// 是否可展开（上游 `expandable = node.summary !== null`）。
    pub fn expandable(&self) -> bool {
        !self.summary.trim().is_empty()
    }

    /// 分隔条右侧摘要文案（上游 `message.compaction.completed`）。
    pub fn summary_text(&self) -> String {
        if self.expandable() {
            format!(
                "已压缩 {} 条历史记录（约 {} tokens）",
                self.shadowed_items, self.shadowed_tokens
            )
        } else {
            "压缩摘要不可用".to_string()
        }
    }

    /// 分隔条标题（上游 `message.compaction`）。
    pub fn title(&self) -> &'static str {
        "上下文已压缩"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(items: u64, tokens: u64, summary: &str) -> CompactionRowFacts {
        CompactionRowFacts {
            shadowed_items: items,
            shadowed_tokens: tokens,
            summary: summary.to_string(),
        }
    }

    #[test]
    fn summary_line_carries_the_web_counts() {
        let f = facts(16, 18_432, "earlier turns condensed");
        assert_eq!(f.title(), "上下文已压缩");
        assert_eq!(f.summary_text(), "已压缩 16 条历史记录（约 18432 tokens）");
        assert!(f.expandable());
    }

    #[test]
    fn missing_summary_reports_unavailable_and_disables_expand() {
        let f = facts(3, 900, "   ");
        assert!(!f.expandable());
        assert_eq!(f.summary_text(), "压缩摘要不可用");
    }

    #[test]
    fn zero_counts_still_render_the_full_line() {
        let f = facts(0, 0, "x");
        assert_eq!(f.summary_text(), "已压缩 0 条历史记录（约 0 tokens）");
    }
}
