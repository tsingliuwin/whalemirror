//! 工具结果的 token 预算保留（上游 `spill-policy` 的等价面）。
//!
//! 判定顺序与上游一致：
//! 1. 内容全部是文本/图片块才算「可保留」；
//! 2. 预算内 → 原样返回（不产生任何改动）；
//! 3. 无属主 / 无后端 / 存储失败 → **保留原文**（loga 提示，绝不吞结果）；
//! 4. 省略通知本身放不进预算 → 保留原文；
//! 5. 否则头尾各保留一半预算、中间以 `\n\n[...]\n\n` 断开、尾部接省略通知。

use dsh_llm::ContentBlock;

use crate::SpillRef;

/// 中间断开占位（上游 `GAP`）。
pub const GAP: &str = "\n\n[...]\n\n";

/// 省略通知（上游 `formatSpillNotice`）：`(Omitted N bytes.{ Omitted M images.} Full formatted
/// result stored at: <locator>. <hint>)`。
pub fn format_spill_notice(omitted_bytes: usize, images: usize, spill: &SpillRef) -> String {
    let image_notice = if images > 0 {
        format!(" Omitted {images} images.")
    } else {
        String::new()
    };
    format!(
        "(Omitted {omitted_bytes} bytes.{image_notice} Full formatted result stored at: {}. {})",
        spill.locator.as_str(),
        spill.retrieval_hint
    )
}

/// token 估算（上游 `estimateContent` 的 chars/4 近似）。
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// 保留结果：头尾预览 + 通知。`Ok(None)` = 未超预算（原样）。
pub fn retain(
    content: &[ContentBlock],
    max_inline_tokens: usize,
    spill: &SpillRef,
    total_bytes: usize,
) -> Result<Option<Vec<ContentBlock>>, String> {
    // 1) 只处理文本/图片块
    if content
        .iter()
        .any(|b| !matches!(b, ContentBlock::Text { .. } | ContentBlock::Image { .. }))
    {
        return Ok(None);
    }
    let text_of = |b: &ContentBlock| match b {
        ContentBlock::Text { text } => text.clone(),
        _ => String::new(),
    };
    let joined: String = content.iter().map(text_of).collect();
    let images = content
        .iter()
        .filter(|b| matches!(b, ContentBlock::Image { .. }))
        .count();
    if estimate_tokens(&joined) <= max_inline_tokens {
        return Ok(None);
    }

    // 4) 通知预算（最坏情况：整段被省略）
    let worst_notice = format_spill_notice(total_bytes, images, spill);
    let notice_tokens = estimate_tokens(&worst_notice) + estimate_tokens(GAP);
    if notice_tokens > max_inline_tokens {
        return Err("spill notice exceeds maxInlineTokens".into());
    }
    let budget = max_inline_tokens.saturating_sub(notice_tokens);

    // 5) 头尾各一半（按字符而非字节，避免切断码点）
    let chars: Vec<char> = joined.chars().collect();
    let head_len = chars.len().min(budget.div_ceil(2).saturating_mul(4));
    let tail_len = chars
        .len()
        .saturating_sub(head_len)
        .min((budget / 2).saturating_mul(4));
    let head: String = chars[..head_len].iter().collect();
    let tail: String = chars[chars.len() - tail_len..].iter().collect();
    let omitted_bytes = total_bytes.saturating_sub(head.len() + tail.len());
    let notice = format_spill_notice(omitted_bytes, images, spill);

    let mut out: Vec<ContentBlock> = Vec::new();
    if !head.is_empty() {
        out.push(ContentBlock::text(head));
    }
    if !tail.is_empty() {
        out.push(ContentBlock::text(GAP));
        out.push(ContentBlock::text(tail));
    }
    if out.is_empty() {
        out.push(ContentBlock::text(notice));
    } else {
        out.push(ContentBlock::text(format!("\n\n{notice}")));
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SpillLocator, RETRIEVAL_HINT};

    fn spill() -> SpillRef {
        SpillRef {
            locator: SpillLocator("/tmp/dsh-spill-x/session-abc/1a2b-web_fetch.txt".into()),
            bytes: 10_000,
            retrieval_hint: RETRIEVAL_HINT.to_string(),
        }
    }

    #[test]
    fn notice_shape_matches_upstream() {
        assert_eq!(
            format_spill_notice(4096, 0, &spill()),
            "(Omitted 4096 bytes. Full formatted result stored at: /tmp/dsh-spill-x/session-abc/1a2b-web_fetch.txt. Use read with offset/limit, or grep this path to search within it.)"
        );
        assert!(format_spill_notice(4096, 2, &spill()).contains(" Omitted 2 images."));
    }

    #[test]
    fn within_budget_passes_through() {
        let content = vec![ContentBlock::text("short")];
        assert!(retain(&content, 12500, &spill(), 5).unwrap().is_none());
    }

    #[test]
    fn non_text_blocks_are_never_rewritten() {
        let content = vec![
            ContentBlock::text("x".repeat(10_000)),
            ContentBlock::ToolResult {
                tool_call_id: dsh_llm::CallId("c".into()),
                content: vec![],
                is_error: Some(false),
            },
        ];
        assert!(retain(&content, 100, &spill(), 10_000).unwrap().is_none());
    }

    #[test]
    fn oversized_text_keeps_head_and_tail_with_notice() {
        let big = "abcdefghij".repeat(2000); // 20k chars ≈ 5k tokens
        let content = vec![ContentBlock::text(big.clone())];
        let retained = retain(&content, 200, &spill(), big.len()).unwrap().unwrap();
        let joined: String = retained
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text } => text.clone(),
                _ => String::new(),
            })
            .collect();
        assert!(joined.starts_with("abcdefghij"), "保留头部");
        assert!(joined.contains(GAP), "中间断开");
        assert!(joined.contains("Full formatted result stored at:"), "带定位符");
        assert!(joined.ends_with(')'));
        assert!(joined.len() < big.len(), "结果必须变小");
    }

    #[test]
    fn notice_that_cannot_fit_keeps_the_original() {
        let big = "x".repeat(4000);
        let content = vec![ContentBlock::text(big)];
        assert!(retain(&content, 4, &spill(), 4000).is_err());
    }
}
