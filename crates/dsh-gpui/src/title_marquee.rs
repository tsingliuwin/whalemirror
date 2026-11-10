//! 侧栏会话标题 hover 跑马灯（上游 `ui-workspace/rows/Rows.tsx`
//! `useTitleMarquee` 的 1:1 纯策略部分 + 渐隐遮罩判据）。
//!
//! 上游语义：
//! - 标题溢出 ≤ {@link MIN_REVEAL_PX}（8px）**不动**——移动几像素读起来像抖动；
//! - 否则 hover 进入以 `TITLE_MARQUEE_PX_PER_MS`（0.03 px/ms，匀速慢行）横移到
//!   右缘，到头后停住；
//! - 指针离开**一步瞬回**起点（回程中窄格会与文字相遇）；
//! - 两端 12px 渐隐遮罩：左遮罩仅在已离开起点（`data-scrolled`）时出现，
//!   右遮罩仅在右侧仍有文字（`data-clipped`）时出现，到头时右遮罩撤除，
//!   末字符以全强度读出；
//! - 触屏（pointerType === 'touch'）不进入；减少动态时直接跳到右缘。

/// 溢出小于等于此值不跑马灯（上游 `MIN_TITLE_REVEAL_PX`）。
pub const MIN_REVEAL_PX: f32 = 8.0;
/// 匀速横移速度（px 每毫秒；上游 `TITLE_MARQUEE_PX_PER_MS = 0.03`）。
pub const MARQUEE_PX_PER_MS: f32 = 0.03;
/// 两端渐隐带宽（上游 CSS `mask-image` 的 12px）。
pub const FADE_PX: f32 = 12.0;

/// 一次 hover 跑马灯的走位策略。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MarqueeMotion {
    /// 保持静止（溢出不足以值得移动，或根本没有溢出）。
    Rest,
    /// 跳到右缘并停住（减少动态）。
    Jump { range: f32 },
    /// 匀速横移到 `range` 后停住。
    Crawl { range: f32 },
}

/// 可滚动范围：内容宽 − 可视宽，负值归零。
pub fn marquee_range(scroll_w: f32, client_w: f32) -> f32 {
    (scroll_w - client_w).max(0.0)
}

/// 进入时的走位决策。
pub fn marquee_enter(range: f32, reduced_motion: bool, touch: bool) -> MarqueeMotion {
    if touch || range <= MIN_REVEAL_PX {
        return MarqueeMotion::Rest;
    }
    if reduced_motion {
        MarqueeMotion::Jump { range }
    } else {
        MarqueeMotion::Crawl { range }
    }
}

/// 按经过的毫秒推进横移位置（匀速、到 `range` 夹住）。`elapsed_ms` 为
/// 自进入起的累计毫秒。
pub fn marquee_position(elapsed_ms: f32, range: f32) -> f32 {
    (elapsed_ms * MARQUEE_PX_PER_MS).clamp(0.0, range.max(0.0))
}

/// 两端渐隐遮罩状态：`(左侧渐隐, 右侧渐隐)`（上游 `data-scrolled`/`data-clipped`）。
pub fn marquee_fades(left: f32, range: f32) -> (bool, bool) {
    (left > 0.0, left < range)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_overflow_stays_put() {
        // 溢出 ≤ 8px：移动几像素像抖动，上游明确不动
        assert_eq!(marquee_enter(8.0, false, false), MarqueeMotion::Rest);
        assert_eq!(marquee_enter(3.0, false, false), MarqueeMotion::Rest);
        assert_eq!(marquee_enter(0.0, false, false), MarqueeMotion::Rest);
        // 刚好超过阈值才动
        assert_eq!(
            marquee_enter(8.1, false, false),
            MarqueeMotion::Crawl { range: 8.1 }
        );
    }

    #[test]
    fn touch_never_marquees_and_reduced_motion_jumps() {
        assert_eq!(marquee_enter(200.0, false, true), MarqueeMotion::Rest);
        assert_eq!(
            marquee_enter(200.0, true, false),
            MarqueeMotion::Jump { range: 200.0 }
        );
    }

    #[test]
    fn range_never_negative() {
        assert_eq!(marquee_range(100.0, 120.0), 0.0);
        assert_eq!(marquee_range(300.0, 100.0), 200.0);
    }

    #[test]
    fn crawl_advances_at_upstream_speed_and_clamps() {
        // 0.03 px/ms：1000ms 走 30px
        assert!((marquee_position(1000.0, 200.0) - 30.0).abs() < 1e-4);
        assert!((marquee_position(0.0, 200.0)).abs() < 1e-4);
        // 到头后停住，不越过右缘
        assert!((marquee_position(100_000.0, 40.0) - 40.0).abs() < 1e-4);
    }

    #[test]
    fn fades_track_resting_scrolled_and_arrived() {
        // 起点：无左遮罩；右侧仍有文字 → 右遮罩
        assert_eq!(marquee_fades(0.0, 50.0), (false, true));
        // 行进中：两端都有
        assert_eq!(marquee_fades(20.0, 50.0), (true, true));
        // 到头：只剩左遮罩（末字符全强度读出）
        assert_eq!(marquee_fades(50.0, 50.0), (true, false));
        // 无溢出：两端都不出现
        assert_eq!(marquee_fades(0.0, 0.0), (false, false));
    }
}
