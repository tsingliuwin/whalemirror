//! 工具结果剪枝（上游 `compaction/compaction-tool-result-pruner` 的等价面）。
//!
//! 在**压缩触发确认之后、挑选要压缩的区间之前**跑：把每个超出阈值的历史
//! 工具结果替换成「头 headChars + 标记 + 尾 tailChars」。字符计数按 Unicode
//! 码点（不切断代理对），非文本块原样保留并维持顺序。整段替换必须比原文短
//! 且落在阈值内——不满足即报错而不是产生更大的结果。

use dsh_llm::ContentBlock;

/// 固定标记（上游 `PRUNE_MARKER` 逐字）。
pub const PRUNE_MARKER: &str = "\n\n[... tool result middle pruned ...]\n\n";

/// 默认阈值 / 头部 / 尾部（上游 `DEFAULTS`）。
pub const DEFAULT_THRESHOLD_CHARS: usize = 8192;
pub const DEFAULT_HEAD_CHARS: usize = 4096;
pub const DEFAULT_TAIL_CHARS: usize = 1024;

/// 剪枝预算。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PruneConfig {
    pub threshold_chars: usize,
    pub head_chars: usize,
    pub tail_chars: usize,
}

impl Default for PruneConfig {
    fn default() -> Self {
        Self {
            threshold_chars: DEFAULT_THRESHOLD_CHARS,
            head_chars: DEFAULT_HEAD_CHARS,
            tail_chars: DEFAULT_TAIL_CHARS,
        }
    }
}

impl PruneConfig {
    /// 解析并校验（上游 `resolveConfig` 语义：head + marker + tail ≤ threshold）。
    pub fn new(threshold_chars: usize, head_chars: usize, tail_chars: usize) -> Result<Self, String> {
        if threshold_chars == 0 {
            return Err("ToolResultPruneConfig: thresholdChars must be a positive integer".into());
        }
        let emitted = head_chars + PRUNE_MARKER.chars().count() + tail_chars;
        if emitted > threshold_chars {
            return Err(format!(
                "ToolResultPruneConfig: headChars + marker + tailChars ({emitted}) must be at most thresholdChars ({threshold_chars})"
            ));
        }
        Ok(Self { threshold_chars, head_chars, tail_chars })
    }
}

/// 文本内容的总码点数（非文本块计 0）。
pub fn measure_content(blocks: &[ContentBlock]) -> usize {
    blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.chars().count(),
            _ => 0,
        })
        .sum()
}

/// 替换超预算的文本中段；`None` = 未超阈值（调用方保留原文）。
pub fn prune_content(blocks: &[ContentBlock], config: PruneConfig) -> Result<Option<Vec<ContentBlock>>, String> {
    let total = measure_content(blocks);
    if total <= config.threshold_chars {
        return Ok(None);
    }
    let removed_start = config.head_chars;
    let removed_end = total.saturating_sub(config.tail_chars);

    let mut pruned: Vec<ContentBlock> = Vec::new();
    let mut consumed = 0usize;
    let mut marker_inserted = false;

    for block in blocks {
        let ContentBlock::Text { text } = block else {
            pruned.push(block.clone());
            continue;
        };
        let points: Vec<char> = text.chars().collect();
        let block_start = consumed;
        let block_end = block_start + points.len();
        let head_end = points.len().min(removed_start.saturating_sub(block_start));
        let tail_start = points.len().min(removed_end.saturating_sub(block_start));
        let intersects = block_start < removed_end && block_end > removed_start;
        let marker = if intersects && !marker_inserted {
            marker_inserted = true;
            PRUNE_MARKER
        } else {
            ""
        };
        let mut out = String::new();
        out.extend(points[..head_end].iter());
        out.push_str(marker);
        out.extend(points[tail_start..].iter());
        if !out.is_empty() {
            pruned.push(ContentBlock::text(out));
        }
        consumed = block_end;
    }
    if !marker_inserted {
        return Err("tool-result prune: failed to locate the removed text span".into());
    }
    let after = measure_content(&pruned);
    if after > config.threshold_chars || after >= total {
        return Err("tool-result prune: replacement must be smaller and within threshold".into());
    }
    Ok(Some(pruned))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_validation_matches_upstream() {
        assert!(PruneConfig::new(8192, 4096, 1024).is_ok());
        assert_eq!(
            PruneConfig::new(0, 0, 0).unwrap_err(),
            "ToolResultPruneConfig: thresholdChars must be a positive integer"
        );
        let err = PruneConfig::new(100, 90, 20).unwrap_err();
        assert!(err.contains("must be at most thresholdChars"), "{err}");
    }

    #[test]
    fn under_threshold_is_untouched() {
        let blocks = vec![ContentBlock::text("short")];
        assert!(prune_content(&blocks, PruneConfig::default()).unwrap().is_none());
    }

    #[test]
    fn oversized_text_keeps_head_marker_and_tail() {
        let text = "a".repeat(5000) + &"b".repeat(5000);
        let blocks = vec![ContentBlock::text(text)];
        let pruned = prune_content(&blocks, PruneConfig::default()).unwrap().unwrap();
        let joined: String = pruned
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text } => text.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(joined.chars().count(), 4096 + PRUNE_MARKER.chars().count() + 1024);
        assert!(joined.starts_with("aaaa"));
        assert!(joined.contains("middle pruned"));
        assert!(joined.ends_with('b'));
    }

    #[test]
    fn non_text_blocks_survive_in_order() {
        let blocks = vec![
            ContentBlock::text("a".repeat(20_000)),
            ContentBlock::Image {
                attachment: dsh_llm::ImageAttachmentRef {
                    attachment_id: "att-1".into(),
                    name: None,
                    media_type: "image/png".into(),
                    bytes: 10,
                    width: 1,
                    height: 1,
                    original_dimensions: None,
                },
                offloaded: false,
            },
        ];
        let pruned = prune_content(&blocks, PruneConfig::default()).unwrap().unwrap();
        assert!(matches!(pruned.last(), Some(ContentBlock::Image { .. })), "图片块保留且在后");
    }

    #[test]
    fn code_point_counting_never_splits_surrogates() {
        let text = "🙂".repeat(3000); // 3000 码点
        let blocks = vec![ContentBlock::text(text)];
        let pruned = prune_content(&blocks, PruneConfig::new(300, 200, 50).unwrap()).unwrap().unwrap();
        let joined: String = pruned
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text } => text.clone(),
                _ => String::new(),
            })
            .collect();
        assert!(joined.chars().count() <= 300);
    }
}
