//! markdown 表格几何契约（对齐 DSH web `MarkdownText.module.css`
//! `.tableScroll th/td`）：
//!
//! - `padding: 10px 16px`，首列去掉左内距、末列去掉右内距——表格左右缘
//!   与正文对齐（单列表格因此两侧都贴边）。
//! - 字号 13 / 行高 22（`--dsw-font-markdown-table`），表头 500 字重
//!   （`--dsw-font-markdown-table-head`），正文 400。
//! - 表格无竖分隔线、无外框、无表头底色；行与行之间只有 0.5px 底发丝。
//!
//! 这些值由 vendor `gpui-base` 的表格渲染器在 `decorate_table_cell` /
//! `decorate_table_row` 里消费，单测锁住纯函数契约，防止后续升级组件时
//! 又退回「带竖线的表格盒」。

use gpui_base::text::{
    TABLE_CELL_MIN_WIDTH_PX, TABLE_CELL_PAD_X_PX, TABLE_CELL_PAD_Y_PX, TABLE_FONT_SIZE_PX,
    TABLE_FONT_WEIGHT_BODY, TABLE_FONT_WEIGHT_HEAD, TABLE_LINE_HEIGHT_PX, TABLE_ROW_HAIRLINE_PX,
    table_cell_font_weight, table_cell_pad_left, table_cell_pad_right, table_cell_pad_x,
};

#[test]
fn cells_pad_16_horizontally_and_10_vertically_like_the_web_table() {
    assert_eq!(TABLE_CELL_PAD_X_PX, 16.0, "web `padding: 10px 16px`");
    assert_eq!(TABLE_CELL_PAD_Y_PX, 10.0, "web `padding: 10px 16px`");
}

#[test]
fn first_and_last_columns_sit_flush_with_the_text_column() {
    // 三列表：首列左贴边、末列右贴边、中间列两侧各 16。
    assert_eq!(table_cell_pad_left(0, 3), 0.0);
    assert_eq!(table_cell_pad_right(0, 3), 16.0);
    assert_eq!(table_cell_pad_left(1, 3), 16.0);
    assert_eq!(table_cell_pad_right(1, 3), 16.0);
    assert_eq!(table_cell_pad_left(2, 3), 16.0);
    assert_eq!(table_cell_pad_right(2, 3), 0.0);

    // 两列表：两侧贴边，中间一条 16。
    assert_eq!(table_cell_pad_x(0, 2), 16.0);
    assert_eq!(table_cell_pad_x(1, 2), 16.0);

    // 单列表：首列即末列，两侧都贴边 → 零内距。
    assert_eq!(table_cell_pad_x(0, 1), 0.0);
}

#[test]
fn table_typography_uses_the_markdown_table_tokens() {
    assert_eq!(TABLE_FONT_SIZE_PX, 13.0);
    assert_eq!(TABLE_LINE_HEIGHT_PX, 22.0);
    assert_eq!(TABLE_FONT_WEIGHT_BODY, 400);
    assert_eq!(TABLE_FONT_WEIGHT_HEAD, 500);
    assert_eq!(table_cell_font_weight(false), 400);
    assert_eq!(table_cell_font_weight(true), 500);
}

#[test]
fn tables_only_draw_a_half_pixel_row_hairline() {
    // 唯一的分隔线：行底 0.5px（web `border-bottom: 0.5px`）。竖线与外框
    // 在渲染器里已删除；这个常量是那唯一一条发丝的宽度。
    assert_eq!(TABLE_ROW_HAIRLINE_PX, 0.5);
}

#[test]
fn columns_keep_the_web_minimum_readable_width() {
    // web `th/td { min-width: 100px }`：短列（如 `crates/vision` 的代码 chip）
    // 不会因为按字符数分配而被压到折行。
    assert_eq!(TABLE_CELL_MIN_WIDTH_PX, 100.0);
}
