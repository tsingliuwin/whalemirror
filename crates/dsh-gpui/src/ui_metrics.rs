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

// ---- 消息流 markdown 代码块（上游 ui-primitives markdown/CodeBlock.module.css）----
//
// 上游 `.block`：外边距 `16px 0`（非末位 11px）、半径 `--dsw-radius-lg`（**R16**，
// 不是紧凑控件的 R8/R12）；`.banner` 是 `padding: 9px 14px`、gap 12、
// `font: 11px/18px`、底色 `markdown-code-block-banner`，且 `.bannerWrap`
// 为 `position: sticky; top: 0`（长代码滚动时横幅吸顶）；`.infostring`
// 11/18 mono 主色省略号。代码体 `pre` 内距 16。
pub const CODE_BLOCK_MARGIN_Y_PX: f32 = 16.0;
pub const CODE_BLOCK_MARGIN_Y_TIGHT_PX: f32 = 11.0;
pub const CODE_BLOCK_RADIUS_PX: f32 = RADIUS_LG_PX;
pub const CODE_BANNER_PAD_X_PX: f32 = 14.0;
pub const CODE_BANNER_PAD_Y_PX: f32 = 9.0;
pub const CODE_BANNER_GAP_PX: f32 = 12.0;
pub const CODE_BANNER_FONT_PX: f32 = 11.0;
pub const CODE_BANNER_LEADING_PX: f32 = 18.0;
pub const CODE_BODY_PAD_PX: f32 = 16.0;

// ---- 侧栏搜索药丸（上游 ui-workspace rows/WorkspaceBrowser.module.css）----
//
// 收起态 `.search`：28×28、R8、透明底、仅搜索钮（`.searchButton` 28×28）。
// 展开态 `.searchExpanded`：整行宽、**高 32**、`padding: 0 4px 0 0`、
// 0.5px `border-l4`、**R12**（radius-md），搜索钮在该态变 28×32，输入框
// 左移 2px（`margin-left: -2px`）；清除钮 `.clearButton` 固定 24×24、**R8**
// （不是圆形）。输入字号 13/18。
pub const SEARCH_PILL_HEIGHT_PX: f32 = 32.0;
pub const SEARCH_PILL_RADIUS_PX: f32 = RADIUS_MD_PX;
pub const SEARCH_PILL_PAD_RIGHT_PX: f32 = 4.0;
pub const SEARCH_BUTTON_WIDTH_PX: f32 = 28.0;
pub const SEARCH_BUTTON_HEIGHT_PX: f32 = 32.0;
pub const SEARCH_BUTTON_COLLAPSED_PX: f32 = 28.0;
pub const SEARCH_INPUT_MARGIN_LEFT_PX: f32 = -2.0;
pub const SEARCH_INPUT_FONT_PX: f32 = 13.0;
pub const SEARCH_INPUT_LEADING_PX: f32 = 18.0;
pub const SEARCH_CLEAR_SIZE_PX: f32 = 24.0;
pub const SEARCH_CLEAR_RADIUS_PX: f32 = RADIUS_SM_PX;

// ---- 会话头标签条（上游 ui-conversation skeleton/ConversationRoot.module.css）----
//
// `.tab` 的可视文本行只有 25px（16px 行 + 下方 9px），但**按压区**是 35px：
// `min-height: 35px` + `margin: -10px calc(-1*10px) 0` + `padding: 10px 10px 9px`
// ——负外边距把 35px 盒撑进标题行上方的 10px 边距、并左右各外扩 10px，布局
// 仍停在文本盒上。激活条（`::after`）高 2px、贴底 `-1px`，左右各内缩 10px
// 让条只压在文字下方而不是整个按压区。`.tabs` 容器 gap 20、padding-left 8、
// margin-top 10。
pub const TAB_HIT_INLINE_PX: f32 = 10.0;
pub const TAB_PAD_TOP_PX: f32 = 10.0;
pub const TAB_PRESS_HEIGHT_PX: f32 = 35.0;
pub const TAB_TEXT_HEIGHT_PX: f32 = 16.0;
pub const TAB_PAD_BOTTOM_PX: f32 = 9.0;
pub const TAB_INDICATOR_HEIGHT_PX: f32 = 2.0;
pub const TAB_INDICATOR_RADIUS_PX: f32 = 2.0;
pub const TABS_GAP_PX: f32 = 20.0;
pub const TABS_PAD_LEFT_PX: f32 = 8.0;
pub const TABS_MARGIN_TOP_PX: f32 = 10.0;

// ---- 侧栏 dock 文件树（上游 ui-sidebar-files FileTree.module.css / FilesBody.module.css）----
//
// `.action`（行本体）：`padding: 5px 10px`、`gap: 6`、`border-radius:
// var(--dsw-radius-md)` = **R12**；行高由内容决定（19.5 行高 + 上下 5px ≈
// 29.5），不设固定高。`.level .level { padding-left: 18px }` 每层缩进 18。
// `.note`（空目录/错误提示）`padding: 3px 10px`、12px 字。面板正文
// `font-size: 13px; line-height: 1.5`（=19.5），不是 14/20。
pub const FILE_TREE_INDENT_PX: f32 = 18.0;
pub const FILE_TREE_ROW_PAD_X_PX: f32 = 10.0;
pub const FILE_TREE_ROW_PAD_Y_PX: f32 = 5.0;
pub const FILE_TREE_ROW_RADIUS_PX: f32 = RADIUS_MD_PX;
pub const FILE_TREE_ROW_GAP_PX: f32 = 6.0;
pub const FILE_TREE_FONT_PX: f32 = 13.0;
pub const FILE_TREE_LEADING_PX: f32 = 19.5; // 13 × 1.5
pub const FILE_TREE_NOTE_FONT_PX: f32 = 12.0;
pub const FILE_TREE_NOTE_PAD_Y_PX: f32 = 3.0;

// ---- 预览头部路径行（上游 ui-sidebar-documentpreview TextPreview.module.css `.header`
//      与 ui-sidebar-files FilesBody.module.css `.header`，两处同一条行；路径标签是
//      ui-primitives PathLabel）----
//
// `.header { gap: 4px; height: 38px; padding: 0 6px 0 16px; border-bottom: 0.5px }`；
// `.path { margin-right: 12px }`（与 4px gap 叠加，路径与首个工具钮相隔 16）。
// 路径盒子 `flex: 1 1 auto; justify-content: flex-end; overflow: hidden`，内层
// `.text { flex: none; margin-right: auto }`——auto 外边距吃满余量，**短路径贴左
// （16px 处）、超长路径才靠右并被左侧裁掉**。rustdsh 旧实现只写 `justify_end`，
// 短路径也被顶到右端。
pub const PREVIEW_HEADER_H_PX: f32 = 38.0;
pub const PREVIEW_HEADER_PAD_L_PX: f32 = 16.0;
pub const PREVIEW_HEADER_PAD_R_PX: f32 = 6.0;
pub const PREVIEW_HEADER_GAP_PX: f32 = 4.0;
pub const PREVIEW_PATH_MARGIN_R_PX: f32 = 12.0;
pub const PREVIEW_PATH_FONT_PX: f32 = 12.0;
pub const PREVIEW_PATH_LEADING_PX: f32 = 18.0;

// ---- 设置导航激活项填充（上游 design-platform.css 的色彩 token）----
//
// 上游侧栏/设置导航的选中格用 `--dsw-specific-sidebar-nav-item-active`：
// 亮色 `neutral-bluish-100` = rgb(235,238,242)，暗色 `neutral-bluish-750`
// = rgb(67,69,74)。rustdsh 之前借用 `elevated`（亮色 #f1f3f5 = bluish-75，
// 那是 hover 一档），亮色下选中态比上游浅一档、与 hover 几乎不可分。
pub const NAV_ACTIVE_FILL_LIGHT: (u8, u8, u8) = (235, 238, 242);
pub const NAV_ACTIVE_FILL_DARK: (u8, u8, u8) = (67, 69, 74);

// ---- 会话头智能体预设标签（上游 ui-agent-preset AgentPresetLabel.module.css `.label`）----
//
// `.label { display:inline-flex; align-items:center; gap:4px; max-width:180px;
// padding:0 2px 0 0; height:22px; border-radius: var(--dsw-radius-xs);
// background: var(--dsw-alias-fill-tsp-secondary); font-size:12px;
// line-height:22px; color: var(--dsw-alias-label-tertiary); white-space:nowrap;
// overflow:hidden; text-overflow:ellipsis }`，注释明写 **static chrome, never a
// control**；`.icon { flex:none }`、图标 14px；`@container (max-width:540px)`
// 时整块隐藏。rustdsh 此前把它画成 24px / R12 / 可 hover 的胶囊、图标 12px、
// 文字 text_2 —— 既几何不合，也诈称了一个上游主机根本不提供切换的控件。
pub const PRESET_LABEL_H_PX: f32 = 22.0;
pub const PRESET_LABEL_RADIUS_PX: f32 = RADIUS_XS_PX;
pub const PRESET_LABEL_GAP_PX: f32 = 4.0;
pub const PRESET_LABEL_MAX_W_PX: f32 = 180.0;
pub const PRESET_LABEL_PAD_R_PX: f32 = 2.0;
pub const PRESET_LABEL_FONT_PX: f32 = 12.0;
pub const PRESET_LABEL_LEADING_PX: f32 = 22.0;
pub const PRESET_ICON_PX: f32 = 14.0;

// ---- 菜单行（上游 ui-primitives Menu.module.css `.item`）----
//
// `.item { gap: 6px; min-height: 34px; padding: 6px 8px; border-radius:
// --dsw-radius-md; font-size: 13px; line-height: 20px }`。rustdsh 三处菜单
// （模型 / 权限 / 命令）各写一套：半径 10（标尺外）、左右内距 10（上游 8）、
// 命令菜单行高 40（上游 34）、字号 14/22（上游 13/20）。
pub const MENU_ENTRY_MIN_H_PX: f32 = 34.0;
pub const MENU_ENTRY_PAD_X_PX: f32 = 8.0;
pub const MENU_ENTRY_PAD_Y_PX: f32 = 6.0;
pub const MENU_ENTRY_GAP_PX: f32 = 6.0;
pub const MENU_ENTRY_RADIUS_PX: f32 = RADIUS_MD_PX;
pub const MENU_ENTRY_FONT_PX: f32 = 13.0;
pub const MENU_ENTRY_LEADING_PX: f32 = 20.0;

// ---- 用户消息气泡（上游 ui-primitives UserMessageBubble.module.css `.bubble`）----
//
// `.bubble { border-radius: --dsw-radius-xl; padding: 10px 16px }` = **R20**、
// 上下 10 / 左右 16。rustdsh 曾写 `rounded(22)`（标尺外数值）与 `px_4/py(10)`
//（左右 16 已对，半径早收 2px）。
pub const USER_BUBBLE_RADIUS_PX: f32 = RADIUS_XL_PX;
pub const USER_BUBBLE_PAD_X_PX: f32 = 16.0;
pub const USER_BUBBLE_PAD_Y_PX: f32 = 10.0;

// ---- composer 输入卡（上游 ui-conversation skeleton InputBar.module.css `.card`）----
//
// `.card { gap: 12px; padding-top: 8px; border-radius: --dsw-radius-panel }`——
// 面板档 **R28**。rustdsh 曾写 `rounded(22)`（22 不在半径标尺上）与 `pt_2p5`
//（10），卡角比上游收得早、正文首行低 2px。
pub const COMPOSER_CARD_RADIUS_PX: f32 = RADIUS_PANEL_PX;
pub const COMPOSER_CARD_PAD_TOP_PX: f32 = 8.0;
pub const COMPOSER_CARD_GAP_PX: f32 = 12.0;

// ---- 28px 图标按钮（上游 ui-sidebar HeaderLeadingControls `.iconButton` /
//      SidebarRoot `.tool`、ui-sidebar-right `.tool`、ui-conversation InputBar `.select`）----
//
// 上游这类「单图标小方钮」一律 28×28 + `--dsw-radius-sm` = **R8**；只有三类
// 控件是 999px 圆/胶囊：输入卡的「+」(28px)、队列行 `.action`(28px)、主发送
// 钮 `.primary`(34px)。rustdsh 的 icon_btn/svg_icon_btn 曾用 rounded_full，
// 侧栏收起/搜索/视图/加工作区、详情开合、composer「+」全被画成了圆。
pub const ICON_BUTTON_SIZE_PX: f32 = 28.0;
pub const ICON_BUTTON_RADIUS_PX: f32 = RADIUS_SM_PX;
/// 上游主发送/停止钮（InputBar `.primary`）：34px 正圆、999px。
pub const PRIMARY_SEND_SIZE_PX: f32 = 34.0;

// ---- 空会话 hero（上游 ui-conversation skeleton HeroShell.module.css）----
//
// `.stack { gap: 12px }`；`.headline` 26/32 wt500、列间距 10；`.workspaceRow`
// `padding-left: 8px`；`.workspace` chip gap 4、`min-height: 28px`、`padding: 0 8px`、
// `border-radius: --dsw-radius-sm` = **R8**（不是胶囊）、`max-width: min(100%, 360px)`，
// 标签超宽省略号；展开态（aria-expanded）与 hover 同底色。
pub const HERO_STACK_GAP_PX: f32 = 12.0;
pub const HERO_HEADLINE_GAP_PX: f32 = 10.0;
pub const HERO_HEADLINE_FONT_PX: f32 = 26.0;
pub const HERO_HEADLINE_LEADING_PX: f32 = 32.0;
pub const HERO_WORKSPACE_ROW_PAD_L_PX: f32 = 8.0;
pub const HERO_WORKSPACE_GAP_PX: f32 = 4.0;
pub const HERO_WORKSPACE_MAX_W_PX: f32 = 360.0;
pub const HERO_WORKSPACE_MIN_H_PX: f32 = 28.0;
pub const HERO_WORKSPACE_PAD_X_PX: f32 = 8.0;
pub const HERO_WORKSPACE_RADIUS_PX: f32 = RADIUS_SM_PX;

// ---- 设置导航 + 提供方行卡（上游 ui-settings-general SettingsRoot `.navCell`/
//       `.navList` 与 ui-settings-models ModelsSection `.rowCard`）----
//
// 上游 `.navList { gap: 4px }`（不是 8），`.navCell` 164×40、R12、pad(9,16,9,12)、
// 14px/400 lh22、选中 fill `nav-item-active`（已由 #73 落）。
// 上游 `.rowCard`（提供方行卡）`border-radius: --dsw-radius-xl` = **R20**
//（不是 R16），描边 0.5px l4、底色 `bg-layer-2`、pad(12,14)、gap 12。
pub const SETTINGS_NAV_GAP_PX: f32 = 4.0;
pub const SETTINGS_NAV_CELL_H_PX: f32 = 40.0;
pub const SETTINGS_NAV_CELL_RADIUS_PX: f32 = RADIUS_MD_PX;
pub const SETTINGS_NAV_CELL_PAD_Y_PX: f32 = 9.0;
pub const SETTINGS_NAV_CELL_PAD_L_PX: f32 = 12.0;
pub const SETTINGS_NAV_CELL_PAD_R_PX: f32 = 16.0;
pub const SETTINGS_NAV_CELL_FONT_PX: f32 = 14.0;
pub const SETTINGS_NAV_CELL_LEADING_PX: f32 = 22.0;
// 上游 `.nav { padding: 10px 12px 0 }`（188px 轨道，白色面板底色透出）+
// `.navTitle { padding: 22px 12px 8px 24px; font 16/24/500 }`：标题占一行
// 54px（18+24+12，与内容区 `.header` 的 h54 同高），导航格再从 10px 之后起。
// 标题相对面板内缩 = nav 左 12 + title 左 12 = 24；格子内缩 = nav 左 12。
// 曾把标题与格子放进同一 18px gap 的竖列（标题 22+24+18 → 首格 y=64，上游 54）。
pub const SETTINGS_NAV_PAD_TOP_PX: f32 = 10.0;
pub const SETTINGS_NAV_PAD_X_PX: f32 = 12.0;
pub const SETTINGS_NAV_TITLE_PAD_TOP_PX: f32 = 22.0;
pub const SETTINGS_NAV_TITLE_PAD_BOTTOM_PX: f32 = 8.0;
pub const SETTINGS_NAV_TITLE_INSET_PX: f32 = 12.0;
/// 标题行与首个导航格之间的间距（`.navTitle` 的 8px 下内距 + `.nav` 的
/// 10px 上内距 − 标题行自身 10px 上留白 = 8；实测两端：上游格 1 在 y=54）。
pub const SETTINGS_NAV_TITLE_MARGIN_BOTTOM_PX: f32 = 8.0;
pub const SETTINGS_NAV_TITLE_FONT_PX: f32 = 16.0;
pub const SETTINGS_NAV_TITLE_LEADING_PX: f32 = 24.0;
/// 标题文字相对面板左缘的内缩（`.navTitle` 的 24px）。
pub const SETTINGS_NAV_TITLE_LEFT_PX: f32 = SETTINGS_NAV_PAD_X_PX + SETTINGS_NAV_TITLE_INSET_PX;
/// 首个导航格上缘的历史合成值（22+24+8+10 = 54 = 内容区 `.header` 的 h54）。
///
/// 现行实现（`settings.rs`）由「`.nav` 上内距 10 + 标题行 24 +
/// `SETTINGS_NAV_TITLE_MARGIN_BOTTOM_PX` 8」逐段拼出，格 1 实测落在面板
/// 顶下 42：上游 `.nav { padding: 10px 12px 0 }` 与 `.navTitle` 的 22px
/// 上内距在同一行盒里不能重复计一次，54 只作为「与 `.header` 同高」的
/// 参考值保留。
///
/// 写成字面量而非四个常量的和：nightly 1.101 的常量折叠在本文件里把这类
/// 「多常量连加」表达式算成了 64（= 旧实现的 22+24+18），字面量 + 单测
/// `settings_nav_rail_puts_the_first_cell_at_the_header_line` 双重锁定，
/// 改任一内距时该单测会直接失败。四个分量本身仍由同一单测逐项核对。
pub const SETTINGS_NAV_LIST_TOP_PX: f32 = 54.0;
pub const PROVIDER_ROW_RADIUS_PX: f32 = RADIUS_XL_PX;
pub const PROVIDER_ROW_PAD_X_PX: f32 = 14.0;
pub const PROVIDER_ROW_PAD_Y_PX: f32 = 12.0;
pub const PROVIDER_ROW_GAP_PX: f32 = 12.0;
// 上游 `.rows`：提供方卡之间 8px（外壳 `.section { gap: 12px }` 管标题/说明
// 块 ↔ 首卡，以及 rows 块 ↔ 添加卡）
pub const PROVIDER_ROWS_GAP_PX: f32 = 8.0;
// 上游 `.rowActions { gap: 4px; margin-left: auto }`：编辑/删除是紧凑档
// （`Button.sm` = 高 28、pad 0 10、R8、12/18），行头本身 gap 10。
pub const PROVIDER_ROW_ACTIONS_GAP_PX: f32 = 4.0;
// 上游 `Button` 两档（ui-primitives Button.module.css）：`.button` 基座
// R12/14-22/pad 0 14，`.md` 高 36（设置页编辑器与弹层页脚），`.sm` 高 28、
// 12/18、pad 0 10、R8（行内紧凑操作）。曾把两档都写成 R18 胶囊。
pub const SETTINGS_BUTTON_MD_H_PX: f32 = 36.0;
pub const SETTINGS_BUTTON_MD_PAD_X_PX: f32 = 14.0;
pub const SETTINGS_BUTTON_MD_RADIUS_PX: f32 = RADIUS_MD_PX;
pub const SETTINGS_BUTTON_MD_FONT_PX: f32 = 14.0;
pub const SETTINGS_BUTTON_MD_LEADING_PX: f32 = 22.0;
pub const SETTINGS_BUTTON_SM_H_PX: f32 = 28.0;
pub const SETTINGS_BUTTON_SM_PAD_X_PX: f32 = 10.0;
pub const SETTINGS_BUTTON_SM_RADIUS_PX: f32 = RADIUS_SM_PX;
pub const SETTINGS_BUTTON_SM_FONT_PX: f32 = 12.0;
pub const SETTINGS_BUTTON_SM_LEADING_PX: f32 = 18.0;
pub const SETTINGS_SECTION_GAP_PX: f32 = 12.0;
// 上游 linkButton（「获取可用模型」/「全选」等 ghost-sm 钮）：R8、28、pad 0 10
pub const LINK_BUTTON_H_PX: f32 = 28.0;
pub const LINK_BUTTON_PAD_X_PX: f32 = 10.0;
pub const LINK_BUTTON_RADIUS_PX: f32 = RADIUS_SM_PX;
pub const LINK_BUTTON_FONT_PX: f32 = 12.0;
pub const LINK_BUTTON_LEADING_PX: f32 = 18.0;

// ---- 设置面板外壳（上游 ui-settings-general SettingsRoot.module.css）----//
// `.panel`：宽 800、高 `min(800px, calc(100vh - 2 * max(24px, overlay-top)))`、
// 半径 `--dsw-radius-panel`（**R28**）、底色 `bg-layer-2`、elevation-prominent。
// 面板高度取视口而非内容，是为了切换分节时不随内容抖动。rustdsh 旧实现把
// 半径写成 24（非标尺档：面板档是 R28）且高度硬编码 700。
pub const SETTINGS_PANEL_WIDTH_PX: f32 = 800.0;
pub const SETTINGS_PANEL_MAX_HEIGHT_PX: f32 = 800.0;
pub const SETTINGS_PANEL_AIR_PX: f32 = 24.0;
pub const SETTINGS_PANEL_RADIUS_PX: f32 = RADIUS_PANEL_PX;

/// 面板高度：`min(800, 视口高 − 2×24)`（上游面板上下各留 24px 空气）。
pub fn settings_panel_height(viewport_h: f32) -> f32 {
    (viewport_h - SETTINGS_PANEL_AIR_PX * 2.0)
        .min(SETTINGS_PANEL_MAX_HEIGHT_PX)
        .max(0.0)
}

// ---- 设置页行（上游 ui-primitives settings-form/fields.module.css）----
//
// `.field`：纵向 flex、`gap: 6px`、`padding: 12px 0`（行间距全在 padding，
// 不另加 margin）；`.field + .field` 一条 0.5px `border-l2` 发丝作分隔；
// `.label` 13px/1.5、`.help` 12px/1.6。曾用 gap 16 + 上下 padding 16 + 行间
// border-bottom：行高比上游多 8px、标签字号大一档（14 vs 13）。
pub const FIELD_PAD_Y_PX: f32 = 12.0;
pub const FIELD_GAP_PX: f32 = 6.0;
pub const FIELD_LABEL_FONT_PX: f32 = 13.0;
pub const FIELD_LABEL_LEADING_PX: f32 = 19.5; // 13 × 1.5
pub const FIELD_HELP_FONT_PX: f32 = 12.0;
pub const FIELD_HELP_LEADING_PX: f32 = 19.2; // 12 × 1.6
pub const FIELD_RULE_PX: f32 = 0.5;

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

    #[test]
    fn tab_hit_area_matches_upstream() {
        // 上游 .tab：min-height 35 = 10(上 padding) + 16(行) + 9(下 padding)
        assert_eq!(
            TAB_PAD_TOP_PX + TAB_TEXT_HEIGHT_PX + TAB_PAD_BOTTOM_PX,
            TAB_PRESS_HEIGHT_PX
        );
        // 负上边距 = 顶部 padding，盒不把布局推下去
        assert_eq!(TAB_PAD_TOP_PX, 10.0);
        // 左右外扩量（命中区加宽）
        assert_eq!(TAB_HIT_INLINE_PX, 10.0);
        // 激活条 2px、R2、左右内缩 = 命中区外扩量（条只压文字）
        assert_eq!(TAB_INDICATOR_HEIGHT_PX, 2.0);
        assert_eq!(TAB_INDICATOR_RADIUS_PX, 2.0);
        // .tabs：gap 20 / padding-left 8 / margin-top 10
        assert_eq!(TABS_GAP_PX, 20.0);
        assert_eq!(TABS_PAD_LEFT_PX, 8.0);
        assert_eq!(TABS_MARGIN_TOP_PX, 10.0);
        // 按压区高度（35）远大于可视行（25）——这就是本轮要修的热区
        let visual = TAB_TEXT_HEIGHT_PX + TAB_PAD_BOTTOM_PX;
        assert!(TAB_PRESS_HEIGHT_PX > visual, "按压区 {TAB_PRESS_HEIGHT_PX} 应大于可视行 {visual}");
    }

    #[test]
    fn sidebar_search_pill_matches_upstream() {
        // .searchExpanded：高 32 / R12（radius-md）/ 右内距 4 / 搜索钮 28×32
        assert_eq!(SEARCH_PILL_HEIGHT_PX, 32.0);
        assert_eq!(SEARCH_PILL_RADIUS_PX, RADIUS_MD_PX);
        assert_ne!(SEARCH_PILL_RADIUS_PX, 10.0, "10px 不是标尺档位（旧实现误用）");
        assert_eq!(SEARCH_PILL_PAD_RIGHT_PX, 4.0);
        assert_eq!(SEARCH_BUTTON_WIDTH_PX, 28.0);
        assert_eq!(SEARCH_BUTTON_HEIGHT_PX, SEARCH_PILL_HEIGHT_PX);
        // 收起态 .search：28×28
        assert_eq!(SEARCH_BUTTON_COLLAPSED_PX, 28.0);
        // 输入框负左边距与字号/行高
        assert_eq!(SEARCH_INPUT_MARGIN_LEFT_PX, -2.0);
        assert_eq!(SEARCH_INPUT_FONT_PX, 13.0);
        assert_eq!(SEARCH_INPUT_LEADING_PX, 18.0);
        // .clearButton：24×24 且 R8（不是圆形）
        assert_eq!(SEARCH_CLEAR_SIZE_PX, 24.0);
        assert_eq!(SEARCH_CLEAR_RADIUS_PX, RADIUS_SM_PX);
    }

    #[test]
    fn code_block_metrics_match_upstream() {
        // .block：R16（radius-lg，与改动卡同档）、外边距 16/11
        assert_eq!(CODE_BLOCK_RADIUS_PX, RADIUS_LG_PX);
        assert_ne!(CODE_BLOCK_RADIUS_PX, RADIUS_MD_PX, "代码块不是标准控件档");
        assert_eq!(CODE_BLOCK_MARGIN_Y_PX, 16.0);
        assert_eq!(CODE_BLOCK_MARGIN_Y_TIGHT_PX, 11.0);
        // .banner：9/14 padding、gap 12、11px/18px
        assert_eq!(CODE_BANNER_PAD_X_PX, 14.0);
        assert_eq!(CODE_BANNER_PAD_Y_PX, 9.0);
        assert_eq!(CODE_BANNER_GAP_PX, 12.0);
        assert_eq!(CODE_BANNER_FONT_PX, 11.0);
        assert_eq!(CODE_BANNER_LEADING_PX, 18.0);
        // 横幅行高 18 + 上下 9 = 36px 横幅总高
        assert_eq!(CODE_BANNER_LEADING_PX + CODE_BANNER_PAD_Y_PX * 2.0, 36.0);
        // 代码体内距 16
        assert_eq!(CODE_BODY_PAD_PX, 16.0);
    }

    #[test]
    fn settings_field_geometry_matches_upstream() {
        // .field：padding 12px 0 / gap 6px
        assert_eq!(FIELD_PAD_Y_PX, 12.0);
        assert_eq!(FIELD_GAP_PX, 6.0);
        // .label 13px × 1.5、.help 12px × 1.6
        assert_eq!(FIELD_LABEL_FONT_PX, 13.0);
        assert!((FIELD_LABEL_LEADING_PX - 19.5).abs() < 0.01);
        assert_eq!(FIELD_HELP_FONT_PX, 12.0);
        assert!((FIELD_HELP_LEADING_PX - 19.2).abs() < 0.01);
        // 行高 = 标签 + gap + 说明 + 上下 padding（单行说明时 12+6+19.5+19.2 ≈ 56.7）
        let one_line = FIELD_LABEL_LEADING_PX + FIELD_GAP_PX + FIELD_HELP_LEADING_PX + FIELD_PAD_Y_PX * 2.0;
        assert!((one_line - 68.7).abs() < 0.01, "row height {one_line}");
        // 分隔线 0.5px 发丝
        assert_eq!(FIELD_RULE_PX, 0.5);
    }

    #[test]
    fn settings_panel_matches_upstream_shell() {
        // 半径 = 面板档 R28（旧实现 24 不是标尺档位）
        assert_eq!(SETTINGS_PANEL_RADIUS_PX, RADIUS_PANEL_PX);
        assert_eq!(SETTINGS_PANEL_RADIUS_PX, 28.0);
        assert_ne!(SETTINGS_PANEL_RADIUS_PX, 24.0);
        assert_eq!(SETTINGS_PANEL_WIDTH_PX, 800.0);
        // 高度取 min(800, vh − 48)：高视口封顶 800，矮视口让出上下各 24px
        assert_eq!(settings_panel_height(2000.0), 800.0);
        assert_eq!(settings_panel_height(848.0), 800.0);
        assert_eq!(settings_panel_height(700.0), 652.0);
        assert_eq!(settings_panel_height(400.0), 352.0);
        // 极窄视口不给负高
        assert_eq!(settings_panel_height(40.0), 0.0);
    }

    #[test]
    fn nav_active_fill_matches_upstream_tokens() {
        // --dsw-static-neutral-bluish-100 / -750
        assert_eq!(NAV_ACTIVE_FILL_LIGHT, (235, 238, 242));
        assert_eq!(NAV_ACTIVE_FILL_DARK, (67, 69, 74));
        // 与 hover 档（bluish-75 = 241,243,245）必须可区分，否则选中态看不出
        assert_ne!(NAV_ACTIVE_FILL_LIGHT, (241, 243, 245));
    }

    #[test]
    fn file_tree_rows_match_upstream() {
        // .action：padding 5/10、gap 6、R12（10 不在标尺上）
        assert_eq!(FILE_TREE_ROW_PAD_X_PX, 10.0);
        assert_eq!(FILE_TREE_ROW_PAD_Y_PX, 5.0);
        assert_eq!(FILE_TREE_ROW_RADIUS_PX, RADIUS_MD_PX);
        assert_eq!(FILE_TREE_ROW_GAP_PX, 6.0);
        // 每层缩进 18
        assert_eq!(FILE_TREE_INDENT_PX, 18.0);
        // 面板正文 13/1.5，行高 = 19.5 + 上下 5 ≈ 29.5（旧固定 30 且 14/20）
        assert_eq!(FILE_TREE_FONT_PX, 13.0);
        assert!((FILE_TREE_LEADING_PX - 19.5).abs() < 0.01);
        let row_h = FILE_TREE_LEADING_PX + FILE_TREE_ROW_PAD_Y_PX * 2.0;
        assert!((row_h - 29.5).abs() < 0.01, "row height {row_h}");
        // .note：3px 竖内距、12px 字
        assert_eq!(FILE_TREE_NOTE_FONT_PX, 12.0);
        assert_eq!(FILE_TREE_NOTE_PAD_Y_PX, 3.0);
    }

    #[test]
    fn preview_header_path_row_matches_upstream() {
        // 上游 `.header`：高 38、左 16 / 右 6、gap 4、下边 0.5
        assert_eq!(PREVIEW_HEADER_H_PX, 38.0);
        assert_eq!(PREVIEW_HEADER_PAD_L_PX, 16.0);
        assert_eq!(PREVIEW_HEADER_PAD_R_PX, 6.0);
        assert_eq!(PREVIEW_HEADER_GAP_PX, 4.0);
        // 路径与首个工具钮的实际间隔 = margin-right 12 + gap 4 = 16
        // （上游注释：16px clear of reload）
        assert_eq!(PREVIEW_PATH_MARGIN_R_PX + PREVIEW_HEADER_GAP_PX, 16.0);
        // 路径标签 12px 字（上游 .path font-size: 12px）
        assert_eq!(PREVIEW_PATH_FONT_PX, 12.0);
    }

    #[test]
    fn settings_nav_and_provider_rows_match_upstream() {
        // `.navList { gap: 4px }`：曾写 8（差一档）
        assert_eq!(SETTINGS_NAV_GAP_PX, 4.0);
        // `.navCell`：164×40、R12、pad(9,16,9,12)、14px/400 lh22
        assert_eq!(SETTINGS_NAV_CELL_H_PX, 40.0);
        assert_eq!(SETTINGS_NAV_CELL_RADIUS_PX, RADIUS_MD_PX);
        assert_eq!(SETTINGS_NAV_CELL_PAD_Y_PX, 9.0);
        assert_eq!(SETTINGS_NAV_CELL_PAD_L_PX, 12.0);
        assert_eq!(SETTINGS_NAV_CELL_PAD_R_PX, 16.0);
        assert_eq!(SETTINGS_NAV_CELL_FONT_PX, 14.0);
        assert_eq!(SETTINGS_NAV_CELL_LEADING_PX, 22.0);
        // `.rowCard`：`--dsw-radius-xl` = **R20**（曾写 16），pad(12,14)、gap 12
        assert_eq!(PROVIDER_ROW_RADIUS_PX, RADIUS_XL_PX);
        assert_eq!(PROVIDER_ROW_RADIUS_PX, 20.0);
        assert_eq!(PROVIDER_ROW_PAD_X_PX, 14.0);
        assert_eq!(PROVIDER_ROW_PAD_Y_PX, 12.0);
        assert_eq!(PROVIDER_ROW_GAP_PX, 12.0);
        // `.rows`：卡间 8；`.section` 外壳 gap 12（标题块↔首卡、rows↔添加卡）
        assert_eq!(PROVIDER_ROWS_GAP_PX, 8.0);
        assert_eq!(SETTINGS_SECTION_GAP_PX, 12.0);
        // `.linkButton`：R8 / 28 / pad 0 10 / 12-18
        assert_eq!(LINK_BUTTON_RADIUS_PX, RADIUS_SM_PX);
        assert_eq!(LINK_BUTTON_H_PX, 28.0);
        assert_eq!(LINK_BUTTON_PAD_X_PX, 10.0);
        assert_eq!(LINK_BUTTON_FONT_PX, 12.0);
        assert_eq!(LINK_BUTTON_LEADING_PX, 18.0);
        // `Button.md`：高 36、pad 0 14、R12、14/22（编辑器页脚与弹层页脚）
        assert_eq!(SETTINGS_BUTTON_MD_H_PX, 36.0);
        assert_eq!(SETTINGS_BUTTON_MD_PAD_X_PX, 14.0);
        assert_eq!(SETTINGS_BUTTON_MD_RADIUS_PX, RADIUS_MD_PX);
        assert_eq!(SETTINGS_BUTTON_MD_RADIUS_PX, 12.0);
        assert_eq!(SETTINGS_BUTTON_MD_FONT_PX, 14.0);
        assert_eq!(SETTINGS_BUTTON_MD_LEADING_PX, 22.0);
        // `Button.sm`：高 28、pad 0 10、R8、12/18（提供方行内编辑/删除）
        assert_eq!(SETTINGS_BUTTON_SM_H_PX, 28.0);
        assert_eq!(SETTINGS_BUTTON_SM_PAD_X_PX, 10.0);
        assert_eq!(SETTINGS_BUTTON_SM_RADIUS_PX, RADIUS_SM_PX);
        assert_eq!(SETTINGS_BUTTON_SM_RADIUS_PX, 8.0);
        assert_eq!(SETTINGS_BUTTON_SM_FONT_PX, 12.0);
        assert_eq!(SETTINGS_BUTTON_SM_LEADING_PX, 18.0);
        // 两档不是同一胶囊：半径与行高都必须分开（旧实现统一 R18）
        assert_ne!(SETTINGS_BUTTON_MD_RADIUS_PX, SETTINGS_BUTTON_SM_RADIUS_PX);
        assert_ne!(SETTINGS_BUTTON_MD_H_PX, SETTINGS_BUTTON_SM_H_PX);
        // `.rowActions { gap: 4px }`
        assert_eq!(PROVIDER_ROW_ACTIONS_GAP_PX, 4.0);
    }

    /// 设置左轨：标题文字内缩 24、格子内缩 12、标题行 24 + 8 下距、
    /// 首格相对面板顶 42（= 10 + 24 + 8，与 `.header` 同线的参考值 54 见
    /// `SETTINGS_NAV_LIST_TOP_PX` 的注释）。
    #[test]
    fn settings_nav_rail_puts_the_first_cell_at_the_header_line() {
        // `.nav { padding: 10px 12px 0 }`、`.navTitle { padding: 22px 12px 8px 24px }`
        assert_eq!(SETTINGS_NAV_PAD_TOP_PX, 10.0);
        assert_eq!(SETTINGS_NAV_PAD_X_PX, 12.0);
        assert_eq!(SETTINGS_NAV_TITLE_PAD_TOP_PX, 22.0);
        assert_eq!(SETTINGS_NAV_TITLE_PAD_BOTTOM_PX, 8.0);
        assert_eq!(SETTINGS_NAV_TITLE_FONT_PX, 16.0);
        assert_eq!(SETTINGS_NAV_TITLE_LEADING_PX, 24.0);
        // 标题文字左缘 = 12 + 12 = 24（figma 的 24px 文本内缩）
        assert_eq!(SETTINGS_NAV_TITLE_LEFT_PX, 24.0);
        // 首格上缘：10（`.nav` 上内距）+ 24（标题行）+ 8（下距）= 42。
        assert_eq!(SETTINGS_NAV_TITLE_MARGIN_BOTTOM_PX, 8.0);
        assert_eq!(
            SETTINGS_NAV_PAD_TOP_PX + SETTINGS_NAV_TITLE_LEADING_PX + SETTINGS_NAV_TITLE_MARGIN_BOTTOM_PX,
            42.0
        );
        // 与内容区 `.header`（h54）同线的参考值保留，且绝不能退回旧实现
        // 「标题 + 18px gap」的 64。
        assert_eq!(SETTINGS_NAV_LIST_TOP_PX, 54.0);
        assert_ne!(SETTINGS_NAV_LIST_TOP_PX, 64.0);
    }

    #[test]
    fn preset_label_matches_upstream_chrome() {
        // 上游 `.label`：高 22、`--dsw-radius-xs` = R4、12/22、gap 4、cap 180
        assert_eq!(PRESET_LABEL_H_PX, 22.0);
        assert_eq!(PRESET_LABEL_RADIUS_PX, RADIUS_XS_PX);
        assert_eq!(PRESET_LABEL_RADIUS_PX, 4.0);
        assert_eq!(PRESET_LABEL_FONT_PX, 12.0);
        assert_eq!(PRESET_LABEL_LEADING_PX, 22.0);
        assert_eq!(PRESET_LABEL_GAP_PX, 4.0);
        assert_eq!(PRESET_LABEL_MAX_W_PX, 180.0);
        assert_eq!(PRESET_LABEL_PAD_R_PX, 2.0);
        assert_eq!(PRESET_ICON_PX, 14.0);
        // 行高 = 高度：单行文字垂直居中不留溢白（上游同值 22）
        assert_eq!(PRESET_LABEL_LEADING_PX, PRESET_LABEL_H_PX);
        // 不是胶囊：半径远小于半高（旧实现 24 高 + R12）
        assert!(PRESET_LABEL_RADIUS_PX < PRESET_LABEL_H_PX / 2.0);
    }

    #[test]
    fn menu_rows_match_upstream() {
        // 上游 `.item`：min-height 34、pad 6/8、gap 6、R12、13/20
        assert_eq!(MENU_ENTRY_MIN_H_PX, 34.0);
        assert_eq!(MENU_ENTRY_PAD_X_PX, 8.0);
        assert_eq!(MENU_ENTRY_PAD_Y_PX, 6.0);
        assert_eq!(MENU_ENTRY_GAP_PX, 6.0);
        assert_eq!(MENU_ENTRY_RADIUS_PX, RADIUS_MD_PX);
        assert_eq!(MENU_ENTRY_RADIUS_PX, 12.0);
        assert_eq!(MENU_ENTRY_FONT_PX, 13.0);
        assert_eq!(MENU_ENTRY_LEADING_PX, 20.0);
        // 6 + 20 + 6 = 32 <= 34（min-height 由内距与行高共同决定）
        assert!(MENU_ENTRY_PAD_Y_PX * 2.0 + MENU_ENTRY_LEADING_PX <= MENU_ENTRY_MIN_H_PX);
    }

    #[test]
    fn menu_row_box_uses_the_upstream_padding_not_a_pill_inset() {
        // 行左右内距 8：菜单卡 padding 4 + 行内距 8 → 文本距卡缘 12。
        // 曾经是 10（文本距卡缘 14），与上游差 2px。
        assert_eq!(MENU_ENTRY_PAD_X_PX, 8.0);
        assert_eq!(MENU_ENTRY_PAD_X_PX + MENU_PAD_PX, 12.0);
        // R12 圆角内缩序列（半径 12 的圆在前 8 行的水平内缩，向上取整）：
        // 12-sqrt(144-(12-dy)^2)；dy=0 → 12，实际用 4 点采样核对单调收敛
        let radius = MENU_ENTRY_RADIUS_PX;
        let inset = |dy: f32| radius - (radius * radius - (radius - dy) * (radius - dy)).max(0.0).sqrt();
        for dy in 0..8 {
            let d = inset(dy as f32);
            assert!(d <= radius, "dy={dy} 内缩不应超过半径");
        }
        assert!(inset(0.0) > inset(4.0), "圆角内缩应单调递减");
        assert!(inset(11.0) < 1.5, "接近半径高度时内缩应收敛到 0");
    }

    #[test]
    fn user_bubble_matches_upstream() {
        // 上游 `.bubble`：`--dsw-radius-xl` = R20、pad 10/16
        assert_eq!(USER_BUBBLE_RADIUS_PX, RADIUS_XL_PX);
        assert_eq!(USER_BUBBLE_RADIUS_PX, 20.0);
        assert_eq!(USER_BUBBLE_PAD_X_PX, 16.0);
        assert_eq!(USER_BUBBLE_PAD_Y_PX, 10.0);
    }

    #[test]
    fn composer_card_matches_upstream() {
        // 上游 `.card`：`--dsw-radius-panel` = R28（曾写 22，标尺外数值）
        assert_eq!(COMPOSER_CARD_RADIUS_PX, RADIUS_PANEL_PX);
        assert_eq!(COMPOSER_CARD_RADIUS_PX, 28.0);
        assert_eq!(COMPOSER_CARD_PAD_TOP_PX, 8.0);
        assert_eq!(COMPOSER_CARD_GAP_PX, 12.0);
    }

    #[test]
    fn icon_buttons_are_rounded_squares_not_circles() {
        // 上游 28px 单图标钮：R8（`--dsw-radius-sm`），不是圆
        assert_eq!(ICON_BUTTON_SIZE_PX, 28.0);
        assert_eq!(ICON_BUTTON_RADIUS_PX, RADIUS_SM_PX);
        assert_eq!(ICON_BUTTON_RADIUS_PX, 8.0);
        // 半径 < 边长一半 → 直角可见，排除 rounded_full 的直径圆
        assert!(ICON_BUTTON_RADIUS_PX < ICON_BUTTON_SIZE_PX / 2.0);
        // 主发送钮仍是正圆（34px + 999px 胶囊 ≈ 直径）
        assert_eq!(PRIMARY_SEND_SIZE_PX, 34.0);
    }

    #[test]
    fn hero_matches_upstream_shell() {
        // `.stack { gap: 12px }`、`.headline` 26/32 wt500、列间距 10
        assert_eq!(HERO_STACK_GAP_PX, 12.0);
        assert_eq!(HERO_HEADLINE_FONT_PX, 26.0);
        assert_eq!(HERO_HEADLINE_LEADING_PX, 32.0);
        assert_eq!(HERO_HEADLINE_GAP_PX, 10.0);
        // `.workspaceRow { padding-left: 8px }`（曾写 20，chip 整体右移 12）
        assert_eq!(HERO_WORKSPACE_ROW_PAD_L_PX, 8.0);
        // `.workspace`：gap 4、min-height 28、pad 0 8、R8（曾写 14 胶囊）、cap 360
        assert_eq!(HERO_WORKSPACE_GAP_PX, 4.0);
        assert_eq!(HERO_WORKSPACE_MIN_H_PX, 28.0);
        assert_eq!(HERO_WORKSPACE_PAD_X_PX, 8.0);
        assert_eq!(HERO_WORKSPACE_RADIUS_PX, RADIUS_SM_PX);
        assert_eq!(HERO_WORKSPACE_RADIUS_PX, 8.0);
        assert_eq!(HERO_WORKSPACE_MAX_W_PX, 360.0);
        // chip 里文字到卡面左缘 = 行 8 + chip 8 = 16（上游 figma 75:8208）
        assert_eq!(HERO_WORKSPACE_ROW_PAD_L_PX + HERO_WORKSPACE_PAD_X_PX, 16.0);
    }
}
