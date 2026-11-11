//! 摘录面（上游 sqlite `makeSnippet` 语义）：空白归一 → 首个匹配定位 →
//! 约 1/3 锚定的定长码点窗 + `…` 端标记。

/// 摘录默认上限（上游 `SESSION_SEARCH_SNIPPET_MAX_CODE_POINTS` / sqlite
/// `snippetChars` 默认 240）。
pub const SNIPPET_MAX_CHARS: usize = 240;

/// 空白归一（上游 makeSnippet：连续空白折叠为单空格、trim）。
pub fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 从归一文本产摘录：含首个匹配的窗（锚定约 1/3 处起），超限截断加 `…`。
/// 无匹配回退头部窗（侧栏摘要语义：命中驱动，未命中取开头）。
pub fn make_snippet(normalized: &str, matcher: &regex::Regex, max_chars: usize) -> String {
    let chars: Vec<char> = normalized.chars().collect();
    let total = chars.len();
    if total <= max_chars {
        return normalized.to_string();
    }
    let match_start = matcher
        .find(normalized)
        .map(|m| normalized[..m.start()].chars().count())
        .unwrap_or(0);
    // 锚定：匹配点尽量落在窗内 1/3 处；窗向前扩不越 0、向后截到 max_chars
    let anchor = match_start.min(total.saturating_sub(1));
    let desired_start = anchor.saturating_sub(max_chars / 3);
    let start = desired_start;
    let end = (start + max_chars).min(total);
    let mut snippet: String = chars[start..end].iter().collect();
    if end < total {
        snippet.push('…');
    }
    if start > 0 {
        snippet.insert(0, '…');
    }
    snippet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_whitespace_runs() {
        assert_eq!(normalize_whitespace("a  \n b\t\tc"), "a b c");
    }

    #[test]
    fn short_text_passes_through() {
        assert_eq!(make_snippet("短文本", &compile("短"), 240), "短文本");
    }

    #[test]
    fn long_text_truncates_with_markers_and_anchors_match() {
        let filler = "x".repeat(300);
        let text = format!("{filler} needle {filler}");
        let normalized = normalize_whitespace(&text);
        let snippet = make_snippet(&normalized, &compile("needle"), 50);
        assert!(snippet.contains("needle"));
        assert!(snippet.starts_with('…') || snippet.ends_with('…') || normalized.chars().count() <= 50);
        assert!(snippet.chars().count() <= 52, "窗 + 至多两端标记");
    }

    #[test]
    fn no_match_falls_back_to_head_window() {
        let text = "y".repeat(400);
        let snippet = make_snippet(&text, &compile("needle"), 50);
        assert!(snippet.starts_with('y'));
        assert!(snippet.ends_with('…'));
    }

    fn compile(q: &str) -> regex::Regex {
        crate::filters::compile_text_filter(q).unwrap()
    }
}
