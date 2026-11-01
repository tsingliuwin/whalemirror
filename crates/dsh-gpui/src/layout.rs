//! 外部框架层：三栏列宽契约 + 拖拽框架。
//!
//! 列宽数值对齐参考 `packages/client/ui-layout/src/client/columns.ts`；
//! 拖拽沿用 Zed redistributable_columns 的 on_drag/on_drag_move 模式
//! （起拖在把手，跟手在根容器，指针越出把手不丢事件）。

use gpui::*;

use crate::theme;


// 数值全部来自 lib 的 columns 契约（与上游 columns.ts 逐项对齐）：
// 曾把 CENTER_MIN 写成 640（上游 400）且 DETAILS_MAX 520（上游按
// viewport×0.7 算上限），窄窗口下右栏被过早让掉、中间列被压穿。
pub(crate) use dsh_gpui::columns::{
    RIGHTBAR_MIN_PX as DETAILS_MIN, SIDEBAR_AUTO_COLLAPSE_PX as SIDEBAR_AUTO_COLLAPSE,
    SIDEBAR_COLLAPSED_PX as SIDEBAR_COLLAPSED, SIDEBAR_DEFAULT_PX as SIDEBAR_DEFAULT, SIDEBAR_MAX_PX as SIDEBAR_MAX,
    SIDEBAR_MIN_PX as SIDEBAR_MIN,
};
/// 右栏首开偏好（上游 `RIGHTBAR_DEFAULT_RATIO = 0.45 × frame`，在首次打开时
/// 由调用方按视口换算；这里保留 360 作为宽屏下的落点）。
pub(crate) const DETAILS_DEFAULT: f32 = 360.0;
/// 右栏拖拽上限在宽屏下的实际夹点由 `viewport × 0.7` 决定（上游
/// RIGHTBAR_MAX_RATIO），因此不再有固定 DETAILS_MAX 常量。

// 内容列与输入卡宽度实现迁 dsh-gpui lib（纯函数，可测；bin 测试会
// 使 rustc 爆栈）：此处保持 layout:: 调用路径不变。
pub(crate) use dsh_gpui::{chat_content_width, composer_card_width};

/// 用户气泡宽度上限（web MessageItem：min(W×0.702, 82%)，0.702 < 0.82 恒成立）。
pub(crate) fn user_bubble_max(column_w: f32) -> f32 {
    chat_content_width(column_w) * 0.702
}

/// columns.ts 的列宽求解（实现迁到 lib 的 `dsh_gpui::columns` 以便单测，
/// 这里保持 `layout::compute_columns` 的元组返回形状）。
pub(crate) fn compute_columns(viewport: f32, sidebar_pref: f32, details_pref: f32) -> (f32, f32, f32) {
    let c = dsh_gpui::columns::compute_columns(
        viewport,
        sidebar_pref,
        details_pref,
        dsh_gpui::columns::SIDEBAR_COLLAPSED_PX,
    );
    (c.sidebar, c.center, c.rightbar)
}
#[derive(Clone, Copy)]
pub(crate) enum DragSide {
    Sidebar,
    Details,
}

/// 列宽拖拽载荷（借鉴 Zed redistributable_columns 的 on_drag/on_drag_move
/// 模式：拖起后由整窗 on_drag_move 跟手，指针越出把手也不丢事件）。
#[derive(Clone, Copy)]
pub(crate) struct ColumnDrag {
    pub(crate) side: DragSide,
}
// --- 小组件 / 帮手 -----------------------------------------------------------

/// 列宽把手：`on_drag` 起拖（借鉴 Zed render_column_resize_divider），
/// 宽度由根容器上的 `on_drag_move::<ColumnDrag>` 按指针绝对位置跟手更新。
pub(crate) fn drag_handle(side: DragSide) -> Stateful<Div> {
    // 16px 命中带骑缝（±8px，起拖容错）；悬浮高亮画在中间的窄带
    // 上，恢复早期 8px 细高亮的观感。
    let group: SharedString = match side {
        DragSide::Sidebar => "drag-sidebar".into(),
        DragSide::Details => "drag-details".into(),
    };
    let group_h = group.clone();
    div()
        .id(group.clone())
        .w(px(16.0))
        .h_full()
        .flex_none()
        .mx(px(-8.0))
        .cursor_col_resize()
        .group(group_h)
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_1()
                .right_1()
                .opacity(0.0)
                .group_hover(group.clone(), |s| s.opacity(1.0))
                .bg(theme::t().border_l2),
        )
        .on_drag(
            ColumnDrag { side },
            |_drag, _offset, _window, cx| cx.new(|_| gpui::Empty),
        )
}
