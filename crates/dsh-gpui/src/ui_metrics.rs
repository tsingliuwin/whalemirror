//! 界面几何常量（上游 `docs/ui-radius.md` + ui-theme `base.css` 的
//! `--dsw-radius-*` 与组件 CSS 实值的 rustdsh 副本）。
//!
//! 放 lib 而不是 bin：bin 靶的 `#[test]` 会让 rustc 宏展开爆栈，几何常量
//! 的锁定测试必须住在 lib（theme.rs 再导出，调用点路径不变）。

/// 圆角标尺（上游）：R4 细节（H<20）、R8 紧凑控件（H20–28）、R12 标准
/// 控件（H32–40）、R16 代码块/改动卡、R20 内容卡、R28 面板/宿主面。
/// 上游明令普通控件与卡片不得引入 10/14/18/24 这类局部值。
pub const RADIUS_XS_PX: f32 = 4.0;
pub const RADIUS_SM_PX: f32 = 8.0;
pub const RADIUS_MD_PX: f32 = 12.0;
pub const RADIUS_LG_PX: f32 = 16.0;
pub const RADIUS_XL_PX: f32 = 20.0;
pub const RADIUS_PANEL_PX: f32 = 28.0;

// ---- 侧栏行与弹出菜单（上游 ui-sidebar / ui-workspace / ui-dockkit）----
//
// 上游 SidebarRoot.module.css 向子树注入四个可继承量：
// `--dsh-sidebar-inline-padding: 8`、`--dsh-sidebar-row-padding: 12`、
// `--dsh-sidebar-label-gap: 6`、`--dsh-sidebar-paint-inset: 3`。
// 行的左右内边距 = 行内距 − 底色内缩（`padding-inline-start: calc(12px -
// 3px)`），底色靠等宽透明边框内缩——内容位置不动、底色比内容窄 3px。
pub const SIDEBAR_INLINE_PADDING_PX: f32 = 8.0;
pub const SIDEBAR_ROW_PADDING_PX: f32 = 12.0;
pub const SIDEBAR_LABEL_GAP_PX: f32 = 6.0;
pub const SIDEBAR_PAINT_INSET_PX: f32 = 3.0;
/// 行的实际内容起点（上游 `padding-inline-start` 的 calc 结果 = 9px）。
pub const SIDEBAR_ROW_INSET_PX: f32 = SIDEBAR_ROW_PADDING_PX - SIDEBAR_PAINT_INSET_PX;
/// 会话行/工作区行：H32 + R12（上游 Rows.module.css `border-radius:
/// var(--dsw-radius-md)`，圆角标准「标准控件或单行 cell H32–40 → R12」）。
/// 曾误用 R8 且内边距写死 16px。
pub const SIDEBAR_ROW_HEIGHT_PX: f32 = 32.0;
pub const SIDEBAR_ROW_RADIUS_PX: f32 = RADIUS_MD_PX;
/// 行尾「…」菜单钮：16px 方形热区 + R4（上游行内小控件走 R4 档）。
pub const SIDEBAR_ROW_ACTION_PX: f32 = 16.0;
pub const SIDEBAR_ROW_ACTION_RADIUS_PX: f32 = RADIUS_XS_PX;

/// 弹出菜单（上游 ui-dockkit `.menu` / `.menuItem`）：条目 padding 4/7、
/// R12、字号 13-1=12；菜单本体 padding 4、最小宽 96、0.5px 发丝边。
/// 曾用 34px 高 + padding 10 + R10（R10 不在标尺上）。
pub const MENU_ITEM_PAD_X_PX: f32 = 7.0;
pub const MENU_ITEM_PAD_Y_PX: f32 = 4.0;
pub const MENU_ITEM_RADIUS_PX: f32 = RADIUS_MD_PX;
pub const MENU_ITEM_FONT_PX: f32 = 12.0;
pub const MENU_PAD_PX: f32 = 4.0;
pub const MENU_MIN_WIDTH_PX: f32 = 96.0;

/// composer 统计 pill 圆角：上游 `ui-chat/chat/StatsPills.module.css`
/// `.pill` 的 `border-radius: var(--dsw-radius-sm)`（R8）；圆角标准点名
/// 「composer statistics 与 context-occupancy trigger 用紧凑控件 R8，
/// 含 hover/展开/焦点态」。曾误用 24px 全圆胶囊（非标尺值）。
pub const STATS_PILL_RADIUS_PX: f32 = RADIUS_SM_PX;
/// composer 统计 pill 字号：上游 `.anchor` 的
/// `calc(--dsh-content-font-size-secondary 13px - 1px)` = 12px。
pub const STATS_PILL_FONT_PX: f32 = 12.0;
/// composer 统计 pill 行高：上游 `.anchor` 的 `20px + delta`。
pub const STATS_PILL_LEADING_PX: f32 = 20.0;
/// composer 统计 pill 的内边距与内容间隔（上游 `.pill`：padding 1px 8px、
/// gap 6px、内含 14px 图标）。
pub const STATS_PILL_PAD_X_PX: f32 = 8.0;
pub const STATS_PILL_PAD_Y_PX: f32 = 1.0;
pub const STATS_PILL_GAP_PX: f32 = 6.0;
pub const STATS_PILL_ICON_PX: f32 = 14.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radius_scale_matches_the_upstream_tokens() {
        // base.css：--dsw-radius-xs 4 / -sm 8；其余档由 docs/ui-radius.md 表定
        assert_eq!(RADIUS_XS_PX, 4.0);
        assert_eq!(RADIUS_SM_PX, 8.0);
        assert_eq!(RADIUS_MD_PX, 12.0);
        assert_eq!(RADIUS_LG_PX, 16.0);
        assert_eq!(RADIUS_XL_PX, 20.0);
        assert_eq!(RADIUS_PANEL_PX, 28.0);
        // 标尺单调递增（选档靠角色，不靠观感）
        let scale = [
            RADIUS_XS_PX,
            RADIUS_SM_PX,
            RADIUS_MD_PX,
            RADIUS_LG_PX,
            RADIUS_XL_PX,
            RADIUS_PANEL_PX,
        ];
        assert!(scale.windows(2).all(|w| w[0] < w[1]), "{scale:?}");
    }

    #[test]
    fn stats_pill_uses_the_compact_control_radius() {
        // 上游 .pill：--dsw-radius-sm；圆角标准：composer statistics = R8
        assert_eq!(STATS_PILL_RADIUS_PX, RADIUS_SM_PX);
        assert_ne!(STATS_PILL_RADIUS_PX, 24.0, "24px 不是标尺档位（旧实现误用）");
        // .anchor：13px - 1px / 20px
        assert_eq!(STATS_PILL_FONT_PX, 12.0);
        assert_eq!(STATS_PILL_LEADING_PX, 20.0);
        // .pill：padding 1px 8px、gap 6px、svg 14px
        assert_eq!(STATS_PILL_PAD_X_PX, 8.0);
        assert_eq!(STATS_PILL_PAD_Y_PX, 1.0);
        assert_eq!(STATS_PILL_GAP_PX, 6.0);
        assert_eq!(STATS_PILL_ICON_PX, 14.0);
        // pill 总高 = 行高 + 上下 padding = 22px（H20–28 的紧凑控件带内 → R8 成立）
        let height = STATS_PILL_LEADING_PX + STATS_PILL_PAD_Y_PX * 2.0;
        assert!((20.0..=28.0).contains(&height), "pill 高度 {height} 应落在 R8 档");
    }

    #[test]
    fn sidebar_row_geometry_matches_upstream_tokens() {
        // SidebarRoot：row-padding 12 − paint-inset 3 = 内容起点 9
        assert_eq!(SIDEBAR_ROW_INSET_PX, 9.0);
        assert_eq!(SIDEBAR_LABEL_GAP_PX, 6.0);
        assert_eq!(SIDEBAR_INLINE_PADDING_PX, 8.0);
        // Rows.module.css：会话行 H32 / R12（标准控件档），不是 R8
        assert_eq!(SIDEBAR_ROW_HEIGHT_PX, 32.0);
        assert_eq!(SIDEBAR_ROW_RADIUS_PX, RADIUS_MD_PX);
        assert_ne!(SIDEBAR_ROW_RADIUS_PX, RADIUS_SM_PX);
        // 行内 16px 小按钮走 R4 档
        assert_eq!(SIDEBAR_ROW_ACTION_PX, 16.0);
        assert_eq!(SIDEBAR_ROW_ACTION_RADIUS_PX, RADIUS_XS_PX);
        // 行高落在 R12 档的 H32–40 带内（选档自洽）
        assert!((32.0..=40.0).contains(&SIDEBAR_ROW_HEIGHT_PX));
    }

    #[test]
    fn menu_item_geometry_matches_upstream_tokens() {
        // dockkit .menuItem：padding 4px 7px / R12 / 字号 13-1=12
        assert_eq!(MENU_ITEM_PAD_X_PX, 7.0);
        assert_eq!(MENU_ITEM_PAD_Y_PX, 4.0);
        assert_eq!(MENU_ITEM_RADIUS_PX, RADIUS_MD_PX);
        assert_eq!(MENU_ITEM_FONT_PX, 12.0);
        // .menu：padding 4 / min-width 96
        assert_eq!(MENU_PAD_PX, 4.0);
        assert_eq!(MENU_MIN_WIDTH_PX, 96.0);
        // 条目自然高 = 行高 20（12 字号配 20 行高）+ 上下 padding = 28px，
        // 恰好落在 R8–R12 的紧凑/标准控件交界，上游取 R12 → 我们同取 R12
        let item_height = 20.0 + MENU_ITEM_PAD_Y_PX * 2.0;
        assert_eq!(item_height, 28.0);
    }
}
