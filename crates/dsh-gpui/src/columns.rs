//! 三栏列宽求解（上游 `ui-layout/src/client/columns.ts` 的等价实现）。
//!
//! 上游契约：**右侧栏先让步**（先缩、缩到 `RIGHTBAR_MIN` 以下就整条让掉），
//! 只有右栏完全让掉之后，中间列才可以掉到 `CENTER_MIN` 以下；侧栏在这一层
//! 永不让步（它的响应式收起由 `SIDEBAR_AUTO_COLLAPSE` 在更外层决定）。
//!
//! 数值逐项对齐上游常量：`CENTER_MIN = 400`、`SIDEBAR_MIN/MAX/DEFAULT =
//! 264/420/280`、`SIDEBAR_COLLAPSED = 56`、`RIGHTBAR_MIN = 300`、
//! `RIGHTBAR_MAX_RATIO = 0.7`。rustdsh 旧实现把 `CENTER_MIN` 写成 640（比
//! 上游大 240），导致窄窗口下右栏被过早让掉、中间列过度挤压。
//!
//! 放在 lib（不是 bin）以便单测：bin 靶的 `#[test]` 会让 rustc 宏展开爆栈。

/// 右栏打开时受保护的中间列最小宽度（上游 `CENTER_MIN`）。
pub const CENTER_MIN_PX: f32 = 400.0;
/// 侧栏拖拽下限（上游 `SIDEBAR_MIN`）。
pub const SIDEBAR_MIN_PX: f32 = 264.0;
/// 侧栏拖拽上限（上游 `SIDEBAR_MAX`）。
pub const SIDEBAR_MAX_PX: f32 = 420.0;
/// 侧栏未拖拽时的默认宽（上游 `SIDEBAR_DEFAULT`）。
pub const SIDEBAR_DEFAULT_PX: f32 = 280.0;
/// 收起侧栏的图标道宽（上游 `SIDEBAR_COLLAPSED`：24px 图标 + 两侧 16px）。
pub const SIDEBAR_COLLAPSED_PX: f32 = 56.0;
/// 右栏拖拽下限（上游 `RIGHTBAR_MIN`）。
pub const RIGHTBAR_MIN_PX: f32 = 300.0;
/// 右栏最大宽占整框比例（上游 `RIGHTBAR_MAX_RATIO`）。
pub const RIGHTBAR_MAX_RATIO: f32 = 0.7;
/// 低于该视口宽度侧栏自动收成图标道（上游 `SIDEBAR_AUTO_COLLAPSE`）。
pub const SIDEBAR_AUTO_COLLAPSE_PX: f32 = 1024.0;
/// 低于该视口宽度侧栏改为抽屉式覆盖（上游 `SIDEBAR_DRAWER_BREAKPOINT`）。
pub const SIDEBAR_DRAWER_BREAKPOINT_PX: f32 = 768.0;

/// 一帧的三栏宽度。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Columns {
    pub sidebar: f32,
    pub center: f32,
    pub rightbar: f32,
}

/// 把面板宽度夹到契约区间（上游 `clampWidth`：四舍五入后再夹）。
fn clamp_width(px: f32, min: f32, max: f32) -> f32 {
    px.round().clamp(min, max)
}

/// 求解一帧的三栏宽度（上游 `computeColumns`）。
///
/// `sidebar == 0` 表示用户收起侧栏 → 占 `collapsed_width`（默认图标道 56）；
/// `rightbar == 0` 表示没有右栏轨道。右栏先让步：可用宽不足 `RIGHTBAR_MIN`
/// 时整条让掉；否则夹在 `[RIGHTBAR_MIN, viewport × 0.7]`。中间列 = 其余全给，
/// 因此只有右栏让掉之后才可能低于 `CENTER_MIN`。
pub fn compute_columns(
    viewport: f32,
    sidebar: f32,
    rightbar: f32,
    collapsed_width: f32,
) -> Columns {
    let s = if sidebar <= 0.0 {
        collapsed_width
    } else {
        clamp_width(sidebar, SIDEBAR_MIN_PX, SIDEBAR_MAX_PX)
    };
    let available = viewport - s - CENTER_MIN_PX;
    let r = if rightbar <= 0.0 || available < RIGHTBAR_MIN_PX {
        0.0
    } else {
        available.min(clamp_width(rightbar, RIGHTBAR_MIN_PX, viewport * RIGHTBAR_MAX_RATIO))
    };
    Columns { sidebar: s, center: (viewport - s - r).max(0.0), rightbar: r }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_constants_are_pinned() {
        // 逐项对齐 upstream columns.ts：曾把 CENTER_MIN 写成 640（大 240）
        assert_eq!(CENTER_MIN_PX, 400.0);
        assert_eq!(SIDEBAR_MIN_PX, 264.0);
        assert_eq!(SIDEBAR_MAX_PX, 420.0);
        assert_eq!(SIDEBAR_DEFAULT_PX, 280.0);
        assert_eq!(SIDEBAR_COLLAPSED_PX, 56.0);
        assert_eq!(RIGHTBAR_MIN_PX, 300.0);
        assert_eq!(RIGHTBAR_MAX_RATIO, 0.7);
        assert_eq!(SIDEBAR_AUTO_COLLAPSE_PX, 1024.0);
        assert_eq!(SIDEBAR_DRAWER_BREAKPOINT_PX, 768.0);
    }

    #[test]
    fn wide_frame_keeps_requested_right_panel() {
        // 1600 - 280 - 400 = 920 可用 > 300 → 右栏按请求（夹到 ≤1600×0.7=1120）
        let c = compute_columns(1600.0, 280.0, 360.0, SIDEBAR_COLLAPSED_PX);
        assert_eq!(c.sidebar, 280.0);
        assert_eq!(c.rightbar, 360.0);
        assert_eq!(c.center, 1600.0 - 280.0 - 360.0);
    }

    #[test]
    fn right_panel_concedes_before_the_center_shrinks() {
        // 980-280-400 = 300 恰好等于 RIGHTBAR_MIN → 仍可开（让栏条件是 < ）
        let c = compute_columns(980.0, 280.0, 360.0, SIDEBAR_COLLAPSED_PX);
        assert_eq!(c.rightbar, 300.0, "可用宽恰等于下限时右栏保留并夹到 300");
        assert_eq!(c.center, 400.0);
        // 少 1px：可用 < RIGHTBAR_MIN → 右栏整条让掉，中间列拿走全部余宽
        let c = compute_columns(979.0, 280.0, 300.0, SIDEBAR_COLLAPSED_PX);
        assert_eq!(c.rightbar, 0.0);
        assert_eq!(c.center, 699.0);
        let c = compute_columns(979.0, 280.0, 300.0, SIDEBAR_COLLAPSED_PX);
        assert_eq!(c.rightbar, 0.0);
        assert_eq!(c.center, 979.0 - 280.0);
    }

    #[test]
    fn center_may_drop_below_min_only_without_rightbar() {
        // 没有右栏时中间列可以一路缩到 0（上游同语义）
        let c = compute_columns(500.0, 280.0, 0.0, SIDEBAR_COLLAPSED_PX);
        assert_eq!(c.rightbar, 0.0);
        assert_eq!(c.center, 220.0);
        let c = compute_columns(300.0, 280.0, 0.0, SIDEBAR_COLLAPSED_PX);
        assert_eq!(c.center, 20.0);
        let c = compute_columns(200.0, 280.0, 0.0, SIDEBAR_COLLAPSED_PX);
        assert_eq!(c.center, 0.0, "不足时夹到 0，不出负宽");
    }

    #[test]
    fn collapsed_sidebar_uses_the_rail_width() {
        let c = compute_columns(900.0, 0.0, 0.0, SIDEBAR_COLLAPSED_PX);
        assert_eq!(c.sidebar, 56.0);
        assert_eq!(c.center, 844.0);
        // macOS 桌面可把图标道也隐去（collapsed_width = 0）
        let c = compute_columns(900.0, 0.0, 0.0, 0.0);
        assert_eq!(c.sidebar, 0.0);
    }

    #[test]
    fn sidebar_preference_is_clamped_to_the_drag_range() {
        assert_eq!(compute_columns(1600.0, 100.0, 0.0, 56.0).sidebar, 264.0);
        assert_eq!(compute_columns(1600.0, 999.0, 0.0, 56.0).sidebar, 420.0);
        // 小数宽度四舍五入（上游 Math.round）
        assert_eq!(compute_columns(1600.0, 300.6, 0.0, 56.0).sidebar, 301.0);
    }

    #[test]
    fn right_panel_is_capped_at_seventy_percent() {
        // 1000 - 264 - 400 = 336 可用；请求 900 夹到上限 1000×0.7 = 700，
        // 再与可用宽 336 取小 → 336（右栏永远先让步给中间列的 400 保底）
        let c = compute_columns(1000.0, 264.0, 900.0, 56.0);
        assert_eq!(c.rightbar, 336.0);
        assert_eq!(c.center, 400.0);
        // 视口足够大时 0.7 上限才真正生效：2000×0.7 = 1400 > 可用 1336
        let c = compute_columns(2000.0, 264.0, 1400.0, 56.0);
        assert_eq!(c.rightbar, 1336.0);
        assert_eq!(c.center, 400.0);
    }
}
