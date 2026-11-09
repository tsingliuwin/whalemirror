//! 设置面板 —— 1:1 对齐 web `ui-settings-general/SettingsRoot`：
//! 遮罩 + 800px r24 面板（188px 左导航 + 54px 头 + 滚动内容区）。
//! 页：通用设置（外观/语言/Enter 行为）、模型（DeepSeek key + 模型）、
//! 插件 / Agent 预设（空态占位，agent 层尚未接入这两个系统）。

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{Icon, IconName, StyledExt, input::{Input, InputState}};

use crate::{AppView, AppearanceMode, AddingMode, EnterBehavior, FetchTarget, SettingsTab, TranscriptView, PROVIDER_CATALOG, valid_route_id};
use crate::theme;

/// 设置弹层（挂在根视图最上层）。
pub(crate) fn render_settings(app: &AppView, this: Entity<AppView>, window: &mut Window, cx: &mut App) -> Div {
    let tk = theme::t();
    let t_close = this.clone();

    // 导航轨（上游 `.nav { padding: 10px 12px 0 }` + `.navTitle { padding:
    // 22px 12px 8px 24px; 16/24/500 }`）：标题占一行 18+24+12 = 54，与内容区
    // `.header` 的 h54 同线，第一个导航格从 y=54 起；格子左缘落在面板左侧
    // 12px（`.nav` 的左右内距），标题文字左缘 24（12 + 标题自身 12）。
    //
    // 实现取「字面量 + 显式行盒」而不是四个常量相加：nightly 1.101 在本仓
    // 会把这类多常量连加折叠成 64（旧实现的 22+24+18），且写成分段表达式
    // 也不可靠（见 ui_metrics 里 SETTINGS_NAV_LIST_TOP_PX 的注释）。
    // 标题行高写成定值，不依赖字体行盒的取整。
    use dsh_gpui::ui_metrics as sm;
    let tk_nav = tk;
    let nav = div()
        .w(px(188.0))
        .flex_none()
        .v_flex()
        .pt(px(sm::SETTINGS_NAV_PAD_TOP_PX))
        .px(px(sm::SETTINGS_NAV_PAD_X_PX))
        .child(
            div()
                .id("set-nav-title-row")
                .flex_none()
                .h(px(sm::SETTINGS_NAV_TITLE_LEADING_PX))
                .mb(px(sm::SETTINGS_NAV_TITLE_MARGIN_BOTTOM_PX))
                .pl(px(sm::SETTINGS_NAV_TITLE_INSET_PX))
                // 不设 line_height：GPUI 的显式行高会把单行文字顶到盒顶
                // 绘制（其余地方只用在多行文本框），标题用字体自然行盒，
                // 由 `.h(24)` + 8px 外边距定几何。
                .text_size(px(sm::SETTINGS_NAV_TITLE_FONT_PX))
                .font_weight(FontWeight::MEDIUM)
                .text_color(tk_nav.text)
                .child("设置"),
        )
        .child(
            div()
                // 上游 `.navList { gap: 4px }`（曾写 8）
                .v_flex()
                .gap(px(dsh_gpui::ui_metrics::SETTINGS_NAV_GAP_PX))
                .child(nav_cell("set-nav-general", IconName::Settings, "通用设置", app.settings_tab == SettingsTab::General, {
                    let t = this.clone();
                    move |_, _, cx| t.update(cx, |v, cx| { v.settings_tab = SettingsTab::General; cx.notify(); })
                }))
                .child(nav_cell("set-nav-models", IconName::Bot, "模型", app.settings_tab == SettingsTab::Models, {
                    let t = this.clone();
                    move |_, _, cx| t.update(cx, |v, cx| { v.settings_tab = SettingsTab::Models; cx.notify(); })
                }))
                .child(nav_cell("set-nav-plugins", IconName::Palette, "插件", app.settings_tab == SettingsTab::Plugins, {
                    let t = this.clone();
                    move |_, _, cx| t.update(cx, |v, cx| { v.settings_tab = SettingsTab::Plugins; cx.notify(); })
                }))
                .child(nav_cell("set-nav-presets", IconName::SquareTerminal, "Agent 预设", app.settings_tab == SettingsTab::Presets, {
                    let t = this.clone();
                    move |_, _, cx| t.update(cx, |v, cx| { v.settings_tab = SettingsTab::Presets; cx.notify(); })
                })),
        );

    // 头（web .header：54px、pad(20,14,8,10)、右侧动作）
    let t_open = this.clone();
    let header = div()
        .flex_none()
        .h(px(54.0))
        .flex()
        .items_center()
        .pt_5()
        .pb_2()
        .pl(px(10.0))
        .pr(px(14.0))
        .child(div().flex_1())
        .child(
            div()
                .id("set-open-config")
                .h(px(28.0))
                .px_3()
                .flex()
                .items_center()
                .rounded(px(14.0))
                .text_size(px(theme::FONT_TAB))
                .line_height(px(20.0))
                .text_color(tk.text_2)
                .cursor_pointer()
                .hover(|s| s.bg(tk.hover))
                .on_click(move |_, _, _cx| {
                    // 直接打开 settings.yaml（系统默认关联程序）
                    crate::open_settings_file();
                    let _ = &t_open;
                })
                .child("打开配置文件"),
        )
        .child(
            div()
                .id("set-close")
                .size(px(28.0))
                .ml_2()
                .flex()
                .items_center()
                .justify_center()
                // 上游 SettingsRoot `.close`：28px + `--dsw-radius-sm` = R8（曾画成圆）
                .rounded(px(dsh_gpui::ui_metrics::ICON_BUTTON_RADIUS_PX))
                .text_color(tk.text)
                .cursor_pointer()
                .hover(|s| s.bg(tk.hover))
                .on_click(move |_, _, cx| {
                    t_close.update(cx, |v, cx| { v.settings_open = false; cx.notify(); });
                })
                .child(Icon::new(IconName::Close).size(px(14.0))),
        );

    // 内容区（web .options：pad(0,24,24,24)、滚动）
    let content = match app.settings_tab {
        SettingsTab::General => general_page(app, &this, cx),
        SettingsTab::Models => models_page(app, &this, window, cx),
        SettingsTab::Plugins => plugins_page(app, &this),
        SettingsTab::Presets => placeholder_page("Agent 预设", "预设（标准/计划/只读等模式）尚未在 Rust 版接入；接入后将在此选择。"),
    };
    let options = div()
        .id("set-options")
        .flex_1()
        .min_w_0()
        .overflow_y_scroll()
        .pr_6()
        .pl_6()
        .pb_6()
        .child(content);

    let confirm = app.confirm_delete.clone();
    let panel = div()
        .absolute()
        .size_full()
        .top_0()
        .left_0()
        .bg(tk.mask)
        .flex()
        .items_center()
        .justify_center()
        .child(
            // 上游 SettingsRoot `.panel`：宽 800、高 min(800, vh−48)（高度取
            // 视口而非内容——切换分节时面板不随内容抖动）、半径 R28
            //（--dsw-radius-panel）、底色 bg-layer-2。曾用固定 700 高 + R24
            //（24 不在标尺上，面板档是 R28）。
            div()
                .relative()
                .w(px(dsh_gpui::ui_metrics::SETTINGS_PANEL_WIDTH_PX))
                .h(px(dsh_gpui::ui_metrics::settings_panel_height(
                    window.viewport_size().height.into(),
                )))
                .flex()
                .rounded(px(dsh_gpui::ui_metrics::SETTINGS_PANEL_RADIUS_PX))
                .overflow_hidden()
                .bg(tk.surface)
                // web Modal 卡（alpha.4）：描边 l1 画进 elevation-prominent
                .shadow(theme::elevation_prominent())
                .child(nav)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .v_flex()
                        .child(header)
                        .child(options),
                )
                .when_some(confirm.clone(), |col, id| {
                    col.child(
                        // 删除确认 Modal（web deleteDialog：遮罩 + 小卡）
                        div()
                            .absolute()
                            .size_full()
                            .top_0()
                            .left_0()
                            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.3))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .w(px(420.0))
                                    .v_flex()
                                    .gap_3()
                                    .p_4()
                                    // web Modal 卡（alpha.4）：r24、border 撤
                                    // 掉、描边 l1 画进 elevation-prominent
                                    .rounded(px(24.0))
                                    .bg(tk.surface)
                                    .shadow(theme::elevation_prominent())
                                    .child(
                                        div()
                                            .text_size(px(theme::FONT_ROW))
                                            .line_height(px(22.0))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(tk.text)
                                            .child(format!("删除 {id}？")),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(theme::FONT_CAPTION))
                                            .line_height(px(theme::FONT_CAPTION_LEADING))
                                            .text_color(tk.text_3)
                                            .child("删除 {id} 会移除其配置；其使用的凭证（如有）由其他位置管理，将会保留。".replace("{id}", &id)),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .justify_end()
                                            .gap_2()
                                            .child({
                                                let t = this.clone();
                                                action_button("del-cancel", "取消", false, move |_, _, cx| {
                                                    t.update(cx, |v, cx| {
                                                        v.confirm_delete = None;
                                                        cx.notify();
                                                    });
                                                })
                                            })
                                            .child({
                                                let t = this.clone();
                                                let id2 = id.clone();
                                                div()
                                                    .id("del-confirm")
                                                    .h(px(36.0))
                                                    .px(px(14.0))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .rounded(px(18.0))
                                                    .border_1()
                                                    .border_color(tk.error)
                                                    .text_size(px(theme::FONT_ROW))
                                                    .line_height(px(22.0))
                                                    .text_color(tk.error)
                                                    .cursor_pointer()
                                                    .hover(|st| st.bg(gpui::hsla(0.0, 0.9, 0.55, 0.12)))
                                                    .on_click(move |_, _, cx| {
                                                        let id = id2.clone();
                                                        t.update(cx, |v, cx| {
                                                            v.remove_provider(&id);
                                                            v.confirm_delete = None;
                                                            cx.notify();
                                                        });
                                                    })
                                                    .child(format!("删除 {id}"))
                                            }),
                                    ),
                            ),
                    )
                })
                .when_some(app.fetch_target.clone(), |col, target| {
                    col.child(fetch_modal(app, &this, &target, cx))
                }),
        );

    panel
                .flex()
                .rounded(px(24.0))
                .overflow_hidden()
                .bg(tk.surface)
                // web Modal 卡（alpha.4）：描边 l1 画进 elevation-prominent
                .shadow(theme::elevation_prominent())
}

/// 导航格（web .navCell：40px、r12、pad(9,16,9,12)、14/22、active 填充）。
fn nav_cell(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    use dsh_gpui::ui_metrics as fm;
    let tk = theme::t();
    div()
        .id(id)
        // 上游 `.navCell`：高 40、R12、pad(9,16,9,12)、14px/400 lh22
        .h(px(fm::SETTINGS_NAV_CELL_H_PX))
        .flex()
        .items_center()
        .gap_2()
        .pl(px(fm::SETTINGS_NAV_CELL_PAD_L_PX))
        .pr(px(fm::SETTINGS_NAV_CELL_PAD_R_PX))
        .py(px(fm::SETTINGS_NAV_CELL_PAD_Y_PX))
        .rounded(px(fm::SETTINGS_NAV_CELL_RADIUS_PX))
        .text_size(px(fm::SETTINGS_NAV_CELL_FONT_PX))
        .line_height(px(fm::SETTINGS_NAV_CELL_LEADING_PX))
        .text_color(tk.text)
        // 上游 .navCell.active 用 --dsw-specific-sidebar-nav-item-active
        // （亮 bluish-100 / 暗 bluish-750），与 hover 一档分离；曾借 elevated，
        // 亮色下比上游浅一档、与 hover 几乎不可分。
        .map(|d| if active { d.bg(tk.nav_active) } else { d })
        .when(!active, |d| d.hover(|s| s.bg(tk.hover)))
        .cursor_pointer()
        .on_click(on_click)
        .child(Icon::new(icon).size(px(16.0)).text_color(if active { tk.accent } else { tk.text_2 }))
        .child(label)
}

/// 设置行（web 通用页行：label + 说明 + 右控件，行间 l2 发丝线）。
fn settings_row(label: &str, desc: &str, control: impl IntoElement, last: bool) -> Div {
    let tk = theme::t();
    // 上游 settings-form/fields.module.css `.field`：padding 12px 0、
    // gap 6px、标签 13px/1.5、说明 12px/1.6；行间是 `.field + .field` 的
    // 0.5px border-top 发丝（不是 border-bottom）。此前 gap 16 + 上下
    // padding 16 + 标签 14px/22，行比上游高约 8px、字号大一档。
    use dsh_gpui::ui_metrics::*;
    let mut row = div()
        .w_full()
        .flex()
        .items_center()
        .gap(px(16.0))
        .py(px(FIELD_PAD_Y_PX))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .v_flex()
                .gap(px(FIELD_GAP_PX))
                .child(
                    div()
                        .text_size(px(FIELD_LABEL_FONT_PX))
                        .line_height(px(FIELD_LABEL_LEADING_PX))
                        .text_color(tk.text)
                        .child(label.to_string()),
                )
                .child(
                    div()
                        .text_size(px(FIELD_HELP_FONT_PX))
                        .line_height(px(FIELD_HELP_LEADING_PX))
                        .text_color(tk.text_3)
                        .child(desc.to_string()),
                ),
        )
        .child(control);
    if !last {
        // `.field + .field { border-top: 0.5px }`——画在上边缘
        row = row.border_t(px(FIELD_RULE_PX)).border_color(tk.border_l2);
    }
    row
}

/// 分段控件（h28 r14 胶囊；选中段 elevated 底 + primary 字）。
fn segmented(
    id: &'static str,
    options: &[(&'static str, bool, bool)], // (label, selected, enabled)
    on_pick: impl Fn(usize, &ClickEvent, &mut Window, &mut App) + 'static + Clone,
) -> Div {
    let tk = theme::t();
    let mut seg = div()
        .h(px(28.0))
        .flex()
        .items_center()
        .p(px(2.0))
        .gap(px(2.0))
        .rounded(px(14.0))
        .bg(tk.layer1);
    for (i, (label, selected, enabled)) in options.iter().enumerate() {
        let on = on_pick.clone();
        let cell = div()
            .id(SharedString::from(format!("{id}-{i}")))
            .h(px(24.0))
            .px_3()
            .flex()
            .items_center()
            .rounded(px(12.0))
            .text_size(px(theme::FONT_TAB))
            .line_height(px(20.0))
            .text_color(if *selected { tk.text } else { tk.text_3 })
            .map(|d| if *selected { d.bg(tk.elevated) } else { d })
            .when(*enabled, |d| {
                d.cursor_pointer()
                    .when(!selected, |d| d.hover(|s| s.bg(tk.hover)))
                    .on_click(move |e, w, cx| on(i, e, w, cx))
            })
            .when(!*enabled, |d| d.opacity(0.5))
            .child(*label);
        seg = seg.child(cell);
    }
    seg
}

/// 通用设置页。
fn general_page(app: &AppView, this: &Entity<AppView>, cx: &mut App) -> Div {
    let tk = theme::t();
    let t = this.clone();
    let t_ws_switch = t.clone();
    let web_search_switch = segmented(
        "set-web-search",
        &[
            ("启用", app.settings.web_search.enabled, true),
            ("停用", !app.settings.web_search.enabled, true),
        ],
        move |i, _, _, cx| {
            let enabled = i == 0;
            t_ws_switch.update(cx, |v, cx| {
                if v.settings.web_search.enabled != enabled {
                    v.settings.web_search.enabled = enabled;
                    v.persist_settings();
                    cx.notify();
                }
            });
        },
    );
    let web_search_key_input = Input::new(&app.web_search_key_input)
        .appearance(false)
        .w(px(220.0));
    let web_search_base_url_input = Input::new(&app.web_search_base_url_input)
        .appearance(false)
        .w(px(220.0));
    let web_search_count_input = Input::new(&app.web_search_count_input)
        .appearance(false)
        .w(px(60.0));
    let t_ws = this.clone();
    let web_search_save_button = div()
        .id("set-web-search-save")
        .h(px(28.0))
        .px(px(12.0))
        .flex()
        .items_center()
        .rounded(px(8.0))
        .border(px(0.5))
        .border_color(tk.border_l3)
        .text_size(px(theme::FONT_TAB))
        .text_color(tk.text)
        .cursor_pointer()
        .hover(|st| st.bg(tk.hover))
        .child("保存搜索设置")
        .on_click(move |_, window, cx| {
            t_ws.update(cx, |v, cx| v.apply_web_search_settings(window, cx));
        });
    let appearance = segmented(
        "set-appearance",
        &[
            ("浅色", app.settings.appearance == AppearanceMode::Light, true),
            ("深色", app.settings.appearance == AppearanceMode::Dark, true),
            ("跟随系统", app.settings.appearance == AppearanceMode::System, true),
        ],
        move |i, _, _, cx| {
            let mode = match i {
                0 => AppearanceMode::Light,
                1 => AppearanceMode::Dark,
                _ => AppearanceMode::System,
            };
            t.update(cx, |v, cx| v.set_appearance(mode, cx));
        },
    );

    let t2 = this.clone();
    let language = segmented(
        "set-language",
        &[
            ("中文", true, true),
            ("English", false, false),
        ],
        move |_, _, _, _cx| {
            let _ = &t2;
        },
    );

    // 权限默认预设行（上游 settings.permission defaultPreset）：三段选择
    // 写 settings.yaml，供新会话 pin 消费
    let t_perm = this.clone();
    let default_perm = crate::default_permission_preset();
    let permission = segmented(
        "set-permission",
        &[
            ("仅可查看", default_perm == "read-only", true),
            ("工作区内修改", default_perm == "workspace-write", true),
            ("完全权限", default_perm == "danger-full-access", true),
        ],
        move |i, _, _, cx| {
            let key = ["read-only", "workspace-write", "danger-full-access"][i];
            t_perm.update(cx, |v, _| v.set_default_permission_preset(key));
        },
    );

    let t3 = this.clone();
    let enter = segmented(
        "set-enter",
        &[
            ("排队发送", app.settings.enter == EnterBehavior::Queue, true),
            ("打断", app.settings.enter == EnterBehavior::Interrupt, true),
        ],
        move |i, _, _, cx| {
            let behavior = if i == 0 { EnterBehavior::Queue } else { EnterBehavior::Interrupt };
            t3.update(cx, |v, cx| v.set_enter_behavior(behavior, cx));
        },
    );

    let t_transcript = this.clone();
    let transcript = segmented(
        "set-transcript",
        &[
            ("标准", app.settings.transcript_view == TranscriptView::Normal, true),
            ("紧凑", app.settings.transcript_view == TranscriptView::Compact, true),
        ],
        move |i, _, _, cx| {
            let mode = if i == 0 { TranscriptView::Normal } else { TranscriptView::Compact };
            t_transcript.update(cx, |v, cx| {
                if v.settings.transcript_view != mode {
                    v.settings.transcript_view = mode;
                    v.persist_settings();
                    // 折叠派生在 ChatView 内，镜像须同步（否则 compact 切换
                    // 不生效直到下一次会话切换）
                    v.chat.update(cx, |c, _| c.transcript_view = mode);
                    cx.notify();
                }
            });
        },
    );

    // 工作步骤收起时机（上游 settings.collapse.*，CollapseTiming）：
    // Browser-local 内存偏好（客户端生命周期，不写 settings.yaml——上游
    // 同「without changing Host settings」）
    let timing_now = app.chat.read_with(cx, |c, _| c.collapse_timing);
    let t_collapse = this.clone();
    let collapse = segmented(
        "set-collapse",
        &[
            ("回答结束后", timing_now == dsh_gpui::CollapseTiming::Completion, true),
            ("下次有新消息时", timing_now == dsh_gpui::CollapseTiming::NextInput, true),
        ],
        move |i, _, _, cx| {
            let timing = if i == 0 {
                dsh_gpui::CollapseTiming::Completion
            } else {
                dsh_gpui::CollapseTiming::NextInput
            };
            t_collapse.update(cx, |v, cx| {
                v.chat.update(cx, |c, _| c.collapse_timing = timing);
                cx.notify();
            });
        },
    );

    div()
        .v_flex()
        .pt_2()
        .child(
            div()
                .text_size(px(16.0))
                .line_height(px(24.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(tk.text)
                .child("通用设置"),
        )
        .child(
            div()
                .mt_1()
                .text_size(px(theme::FONT_CAPTION))
                .line_height(px(theme::FONT_CAPTION_LEADING))
                .text_color(tk.text_3)
                .child("设置保存在本地配置文件中，立即生效。"),
        )
    // web 通用页顺序：Agent 预设 → 权限 → 语言 → 外观 → Enter 行为
        .child(settings_row(
            "Agent 预设",
            "对此后新建的会话生效。运行中的会话保持它开始时的预设。",
            select_chip("set-preset", "标准模式"),
            false,
        ))
        .child(settings_row(
            "权限",
            "选择新会话的默认权限模式",
            permission,
            false,
        ))
        .child(settings_row("语言", "界面语言", language, false))
        .child(settings_row("外观", "选择应用的外观主题", appearance, false))
        .child(settings_row(
            "繁忙时 Enter 键行为",
            "仅在智能体运行时生效；Shift+Enter 始终换行",
            enter,
            false,
        ))
        .child(settings_row("对话视图", "已完成轮次的过程折叠显示", transcript, false))
        .child(settings_row(
            "工作步骤收起时机",
            "选择何时自动收起工作步骤",
            collapse,
            true,
        ))
        // DeepSeek 搜索设置卡（上游 web-search-deepseek 设置卡：开关/key/
        // base URL/count；保存后重启生效——工具在启动时注册）
        .child(settings_row(
            "DeepSeek 搜索",
            "启用后注册 web_search 工具（key 留空用 DEEPSEEK_API_KEY；保存后重启生效）",
            web_search_switch,
            false,
        ))
        .when(app.settings.web_search.enabled, |page| {
            page.child(settings_row(
                "搜索 API key",
                "留空时回退 DEEPSEEK_API_KEY 凭据",
                web_search_key_input,
                false,
            ))
            .child(settings_row(
                "搜索端点",
                "留空用官方默认端点",
                web_search_base_url_input,
                false,
            ))
            .child(settings_row(
                "单请求搜索上限",
                "1-50，默认 10",
                web_search_count_input,
                false,
            ))
            .child(settings_row(
                "",
                "",
                web_search_save_button,
                true,
            ))
        })
}

/// select 形态的选项 chip（web InputBar .select：h28 r8、13/20 medium
/// secondary、右 12px chevron、透明底 hover 白 8%）。当前版本对应系统
/// 只有单一模式，chip 为静态展示。
fn select_chip(id: &'static str, label: &'static str) -> Stateful<Div> {
    let tk = theme::t();
    div()
        .id(id)
        .h(px(28.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .gap_1()
        .rounded(px(8.0))
        .text_size(px(theme::FONT_TAB))
        .line_height(px(20.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(tk.text_2)
        .hover(|s| s.bg(tk.hover))
        .child(label)
        .child(Icon::new(IconName::ChevronDown).size(px(12.0)).text_color(tk.caption))
}

/// 模型页（web ModelsSection）：标题/说明 + 已配置提供方 rowCard 列表 +
/// addBlock（两枚虚线按钮 / adopt 卡 / declare 卡）。
fn models_page(app: &AppView, this: &Entity<AppView>, _window: &mut Window, cx: &mut App) -> Div {

    // --- 已配置提供方：DeepSeek rowCard + 自定义 rowCards ---
    let deepseek_row = provider_row(
        app,
        this,
        "deepseek",
        "DeepSeek",
        false, // 自定义 tag
        app.llm_configured,
        Some(()), // 可编辑，不可删除（内置路由）
        cx,
    );

    let custom_rows: Vec<Div> = app
        .settings
        .providers
        .iter()
        .map(|p| {
            // web：只有 hand-declared（目录外）的 route 才打「自定义」tag；
            // 「添加提供方」采用的目录 route 不算自定义。
            let custom = !PROVIDER_CATALOG.iter().any(|e| e.id == p.id);
            provider_row(
                app,
                this,
                &p.id,
                if p.name.is_empty() { &p.id } else { &p.name },
                custom,
                !p.api_key.is_empty(),
                None, // 可编辑 + 可删除
                cx,
            )
        })
        .collect();

    // --- addBlock ---
    let add_block = match app.adding {
        AddingMode::Adopt => adopt_card(app, this, cx),
        AddingMode::Declare => declare_card(app, this, cx),
        AddingMode::None => {
            let t1 = this.clone();
            let t2 = this.clone();
            div()
                .flex()
                .flex_wrap()
                .gap(px(10.0))
                .child(add_button("set-add-known", "添加提供方", move |_, _, cx| {
                    t1.update(cx, |v, cx| {
                        let focus = dsh_gpui::provider_cards::focus_after_add(&form_focus(v));
                        v.editing_provider = focus.editing;
                        v.adding = AddingMode::Adopt;
                        cx.notify();
                    });
                }))
                .child(add_button("set-add-custom", "添加自定义提供方", move |_, _, cx| {
                    t2.update(cx, |v, cx| {
                        let focus = dsh_gpui::provider_cards::focus_after_add(&form_focus(v));
                        v.editing_provider = focus.editing;
                        v.adding = AddingMode::Declare;
                        cx.notify();
                    });
                }))
        }
    };
    // 上游 `.section { gap: 12px }` + `.rows { margin: 12px 0 0; gap: 8px }`：
    // 外壳 gap 12、说明块与首卡 12、卡之间 8（原实现 pt_2/gap_3 = 8/12，
    // 两处都错位）。添加卡是 `</ul>` 之后的兄弟节点，不是行卡，故单独落
    // 在 gap 12 的外壳层。
    div()
        .v_flex()
        .pt_2()
        .gap(px(dsh_gpui::ui_metrics::SETTINGS_SECTION_GAP_PX))
        .child(page_title("模型"))
        .child(page_intro("填入各提供方的 API 密钥即可使用其模型。"))
        .child(
            div()
                .v_flex()
                .gap(px(dsh_gpui::ui_metrics::PROVIDER_ROWS_GAP_PX))
                .child(deepseek_row)
                .children(custom_rows),
        )
        .child(add_block)
}

/// 页标题 / 说明（web .title 16/24 wt500、.intro 14/22 tertiary）。
fn page_title(text: &str) -> Div {
    let tk = theme::t();
    div()
        .text_size(px(16.0))
        .line_height(px(24.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(tk.text)
        .child(text.to_string())
}

fn page_intro(text: &str) -> Div {
    let tk = theme::t();
    div()
        .text_size(px(theme::FONT_ROW))
        .line_height(px(22.0))
        .text_color(tk.text_3)
        .child(text.to_string())
}

/// 插件页：内置插件清单 + 开关（上游 OPTIONAL_BUNDLES 的管理面；
/// 外部 npm/pnpm 包安装与 Discover 市场依赖 JS 运行时，面外——backlog）。
fn plugins_page(app: &AppView, this: &Entity<AppView>) -> Div {
    use dsh_gpui::ui_metrics::*;
    let tk = theme::t();
    let plugins = dsh_plugin::OFFICIAL_PLUGINS;
    let mut rows = div().v_flex().mt(px(12.0));
    for (i, p) in plugins.iter().enumerate() {
        let on = app.settings.plugins_enabled.iter().any(|e| e == p.id);
        let t = this.clone();
        let pid: &'static str = p.id;
        let switch = gpui_component::switch::Switch::new(SharedString::from(format!(
            "plugin-switch-{}",
            p.id
        )))
        .checked(on)
        .on_change(move |checked: &bool, _w, cx| {
            t.update(cx, |v, cx| v.set_plugin_enabled(pid, *checked, cx));
        });
        // 名称行 + Experimental 徽标（上游实验包标记）
        let mut name_row = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .text_size(px(FIELD_LABEL_FONT_PX))
                    .line_height(px(FIELD_LABEL_LEADING_PX))
                    .text_color(tk.text)
                    .child(p.name),
            );
        if p.experimental {
            name_row = name_row.child(
                div()
                    .h(px(18.0))
                    .px(px(6.0))
                    .flex()
                    .items_center()
                    .rounded(px(4.0))
                    .bg(tk.layer1)
                    .text_size(px(theme::FONT_CAPTION))
                    .line_height(px(theme::FONT_CAPTION_LEADING))
                    .text_color(tk.text_3)
                    .child("实验性"),
            );
        }
        let last = i + 1 == plugins.len();
        let mut row = div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(16.0))
            .py(px(FIELD_PAD_Y_PX))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .v_flex()
                    .gap(px(FIELD_GAP_PX))
                    .child(name_row)
                    .child(
                        div()
                            .text_size(px(FIELD_HELP_FONT_PX))
                            .line_height(px(FIELD_HELP_LEADING_PX))
                            .text_color(tk.text_3)
                            .child(p.description),
                    ),
            )
            .child(switch);
        if !last {
            // `.field + .field { border-top: 0.5px }`——画在上边缘
            row = row.border_t(px(FIELD_RULE_PX)).border_color(tk.border_l2);
        }
        rows = rows.child(row);
    }
    div()
        .w_full()
        .v_flex()
        .child(page_title("插件"))
        .child(page_intro(
            "内置插件随应用发行、默认关闭；开关对新提问即时生效，用户改名的会话标题不会被自动改写。外部插件安装与插件市场暂未支持。",
        ))
        .child(rows)
}

/// 当前提供方表单焦点（行编辑 + 添加卡），喂给 `provider_cards` 的纯函数。
fn form_focus(app: &AppView) -> dsh_gpui::provider_cards::ProviderFormFocus {
    dsh_gpui::provider_cards::ProviderFormFocus {
        editing: app.editing_provider.clone(),
        add_open: !matches!(app.adding, AddingMode::None),
    }
}

/// 已配置提供方行卡（web .rowCard：l2 描边 r12 pad 12/14 gap 12）。
/// `deletable`=None 表示内置路由（DeepSeek）只可编辑。
#[allow(clippy::too_many_arguments)]
fn provider_row(
    app: &AppView,
    this: &Entity<AppView>,
    id: &str,
    name: &str,
    custom: bool,
    has_key: bool,
    _builtin: Option<()>,
    cx: &App,
) -> Div {
    let tk = theme::t();
    let is_deepseek = id == "deepseek";
    let _active = app.active_provider == id;
    let editing = app.editing_provider.as_deref() == Some(id);

    // 行头：凭据点 + 名称 + route 标注 + 自定义 tag + 操作
    let t_edit = this.clone();
    let edit_id = id.to_string();
    let mut head = div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(
            // web 顺序：名称 → [route 标注] → [自定义 tag] → 凭据点
            div()
                .text_size(px(theme::FONT_ROW))
                .line_height(px(22.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(tk.text)
                .child(name.to_string()),
        );
    if name != id {
        head = head.child(
            div()
                .text_size(px(theme::FONT_CAPTION))
                .line_height(px(theme::FONT_CAPTION_LEADING))
                .text_color(tk.text_3)
                .child(id.to_string()),
        );
    }
    if custom {
        head = head.child(
            // 「自定义」tag（web .rowTag：11/16、l3 描边、r4）
            div()
                .px(px(6.0))
                .py(px(1.0))
                // web .rowTag（alpha.4）：0.5px 发丝
                .border(px(0.5))
                .border_color(tk.border_l3)
                .rounded(px(4.0))
                .text_size(px(11.0))
                .line_height(px(16.0))
                .text_color(tk.text_2)
                .child("自定义"),
        );
    }
    head = head.child(
        // 凭据点（web .credentialDot：8px 圆，绿=已配置 红=缺失）
        div()
            .size(px(8.0))
            .rounded_full()
            .flex_none()
            .bg(if has_key { tk.green } else { tk.error }),
    );
    // web 的 rowCard 只有 编辑/删除，没有启用动作（当前模型由
    // composer 的模型选择器管理，不是行卡职责）。行操作是 `Button.sm`
    // 紧凑档，`rowActions` 自带右对齐与 4px 间距（行头 gap 10 不参与）。
    let mut actions = div()
        .flex()
        .items_center()
        .gap(px(dsh_gpui::ui_metrics::PROVIDER_ROW_ACTIONS_GAP_PX))
        .ml_auto()
        .child(compact_action_button(
            SharedString::from(format!("prov-edit-{id}")),
            "编辑",
            false,
            move |_, window, cx| {
                let id = edit_id.clone();
                t_edit.update(cx, |v, cx| {
                    let focus = dsh_gpui::provider_cards::focus_after_edit(&form_focus(v), &id);
                    let opening = focus.editing.is_some();
                    v.editing_provider = focus.editing;
                    // 上游「一次只开一张卡」：行编辑展开时收添加卡
                    v.adding = AddingMode::None;
                    // 展开时把「路由默认模型」的容量填进输入框（#96 的编辑
                    // 入口在自定义设置里，与 API 地址同一套控件形态）
                    if opening {
                        let seed = v
                            .settings
                            .providers
                            .iter()
                            .find(|p| p.id == id)
                            .and_then(|p| p.models.first())
                            .map(|m| m.context_window.clone())
                            .unwrap_or_default();
                        v.model_capacity_input.update(cx, |state, cx| {
                            state.set_value(seed, window, cx);
                        });
                        // reasoning 档位：预填当前声明（空 = 未声明）
                        let declared = v
                            .settings
                            .providers
                            .iter()
                            .find(|p| p.id == id)
                            .and_then(|p| p.models.first())
                            .and_then(|m| m.reasoning_efforts.clone())
                            .map(|e| dsh_gpui::reasoning_efforts::format_declaration(&e))
                            .unwrap_or_default();
                        v.reasoning_wire_input.update(cx, |state, cx| {
                            state.set_value(declared, window, cx);
                        });
                    }
                    cx.notify();
                });
            },
        ));
    if !is_deepseek {
        let t_del = this.clone();
        let del_id = id.to_string();
        actions = actions.child(compact_action_button(
            SharedString::from(format!("prov-del-{id}")),
            "删除",
            true,
            move |_, _, cx| {
                let id = del_id.clone();
                t_del.update(cx, |v, cx| {
                    v.confirm_delete = Some(id);
                    cx.notify();
                });
            },
        ));
    }
    head = head.child(actions);

    let mut card = div()
        .v_flex()
        .gap(px(dsh_gpui::ui_metrics::PROVIDER_ROW_GAP_PX))
        // web ModelsSection .rowCard（alpha.4）：`--dsw-radius-xl` = R20、
        // 0.5px 发丝 l4、pad(12,14)、gap 12（曾写 R16）
        .rounded(px(dsh_gpui::ui_metrics::PROVIDER_ROW_RADIUS_PX))
        .border(px(0.5))
        .border_color(tk.border_l4)
        .px(px(dsh_gpui::ui_metrics::PROVIDER_ROW_PAD_X_PX))
        .py(px(dsh_gpui::ui_metrics::PROVIDER_ROW_PAD_Y_PX))
        .child(head);

    // 展开的编辑卡（web rowCard 内嵌 ProviderEditor）
    if editing {
        card = card.child(edit_card(app, this, id, name, cx));
    }
    card
}

/// 编辑卡（web ProviderEditor：密钥 + 自定义设置折叠 + 保存/取消）。
fn edit_card(app: &AppView, this: &Entity<AppView>, id: &str, name: &str, cx: &App) -> Div {
    let tk = theme::t();
    let t_save = this.clone();
    let models: Vec<String> = if id == "deepseek" {
        // 上游 llm-deepseek DEFAULT_MODELS（0.1.5-rc.2：V41 Flash 首项默认）
        vec![
            "deepseek-flash".into(),
            "deepseek-v4-flash".into(),
            "deepseek-v4-pro".into(),
            "deepseek-v4-flash-vision-exp".into(),
        ]
    } else {
        app.settings
            .providers
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.models.iter().map(|m| m.id.clone()).collect())
            .unwrap_or_default()
    };

    div()
        .v_flex()
        .gap(px(14.0))
        .rounded(px(12.0))
        .bg(tk.surface_2)
        .px(px(16.0))
        .py(px(14.0))
        // editorHeader：标题 + route 标注
        .child(
            div()
                .flex()
                .items_baseline()
                .gap_2()
                .child(
                    div()
                        .text_size(px(theme::FONT_ROW))
                        .line_height(px(22.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(tk.text)
                        .child(name.to_string()),
                )
                .child(
                    div()
                        .text_size(px(theme::FONT_CAPTION))
                        .line_height(px(theme::FONT_CAPTION_LEADING))
                        .text_color(tk.text_3)
                        .child(id.to_string()),
                ),
        )
        .child(field(
            "API 密钥",
            div()
                .when(id == "deepseek" && app.env_key_locked, |d| {
                    d.child(
                        div()
                            .w_full()
                            .h(px(32.0))
                            .flex()
                            .items_center()
                            .px(px(10.0))
                            .rounded(px(8.0))
                            // web Input 原语（alpha.4）：0.5px，色阶 l2→l4
                            .border(px(0.5))
                            .border_color(tk.border_l4)
                            .bg(tk.layer1)
                            .text_size(px(theme::FONT_ROW))
                            .line_height(px(22.0))
                            .text_color(tk.text_3)
                            .child("由启动环境提供（只读）"),
                    )
                })
                .when(id != "deepseek" || !app.env_key_locked, |d| {
                    d.child(Input::new(&app.edit_key).w_full())
                }),
        ))
        .child(if id == "deepseek" {
            hint(if app.env_key_locked {
                "DeepSeek 密钥来自 DEEPSEEK_API_KEY 环境变量，此处不可编辑。"
            } else {
                "编辑卡默认 API 地址 https://api.deepseek.com；自定义提供方的 API 地址在折叠区内。"
            })
        } else {
            hint("选择模型目录中的首行作为路由默认模型。")
        })
        // 上下文容量（#96）：常显在折叠区之前——设置面板在本环境无法用
        // 合成滚轮滚动，放到折叠区里会让页脚「保存」永远移出视口。形态
        // 与 API 密钥同款 `field()`+`Input`（同卡内实测可点）。
        .when(id != "deepseek" || !app.env_key_locked, |d| {
            d.child(field(
                "上下文窗口",
                Input::new(&app.model_capacity_input).w_full(),
            ))
            .child(hint(
                "作用于路由默认模型（模型目录首行）；接受 128K / 131072，留空按目录兜底。",
            ))
        })
        // reasoning 档位（上游 ModelReasoningEfforts）：首屏可见。
        //
        // 卡内的开关/增删按钮在本环境点击不可达（与容量 chip 同类），故用
        // **一行声明**代替点选区：`off,low,medium,high` 或
        // `off=,low=low_v2` 这种显式值写法；打开编辑卡时预填当前声明。
        .when(id != "deepseek", |d| {
            let current = app
                .settings
                .providers
                .iter()
                .find(|p| p.id == id)
                .and_then(|p| p.models.first())
                .and_then(|m| m.reasoning_efforts.clone());
            let invalid = current
                .as_deref()
                .is_some_and(|entries| dsh_gpui::reasoning_efforts::validate(entries).is_some());
            let mut col = d.child(field(
                "推理档位",
                Input::new(&app.reasoning_wire_input).w_full(),
            ))
            .child(hint(
                "逗号分隔档位：off,low,medium,high；自定义发送值写作 low=low_v2。留空 = 沿用目录能力。",
            ));
            if invalid {
                col = col.child(err_hint("至少一个思考档，且非 off 档都要有发送值。"));
            }
            col
        })
        .child(
            // 自定义设置折叠（web details.customized：12/18 wt500 secondary + 旋转 chevron）
            disclosure(
                "edit-customized",
                "自定义设置",
                app.edit_customized_open,
                div()
                    .v_flex()
                    .gap(px(12.0))
                    .pt_3()
                    .child(field("API 地址", Input::new(&app.edit_base).w_full()))
                    .child(
                        div()
                            .v_flex()
                            .gap_1()
                            .child(
                                // web ModelListEditor 的 modelListHead：标题 +
                                // 获取按钮（pi-ai 家族才有端点探测；DeepSeek
                                // 用内置目录编辑器，无此动作）
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_size(px(theme::FONT_CAPTION))
                                            .line_height(px(theme::FONT_CAPTION_LEADING))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(tk.text_2)
                                            .child("模型目录"),
                                    )
                                    .when(id != "deepseek", |d| {
                                        let target = FetchTarget::Edit(id.to_string());
                                        d.child(fetch_link(app, this, &target, cx))
                                    }),
                            )
                            // 模型行：名称 + 容量读数（点击就地改容量）。
                            // 上游把 contextWindow 当 provider 元数据展示；
                            // rustdsh 的探测可能拿不到，故给手填入口。
                            .child({
                                let mut rows = div().v_flex().gap(px(2.0));
                                // 自定义路由读已存模型行；内置 DeepSeek 读目录
                                // 列表（同样允许就地改容量）。
                                let list: Vec<(String, String)> = app
                                    .settings
                                    .providers
                                    .iter()
                                    .find(|p| p.id == id)
                                    .map(|p| {
                                        p.models
                                            .iter()
                                            .map(|m| (m.id.clone(), m.context_window.clone()))
                                            .collect()
                                    })
                                    .unwrap_or_else(|| {
                                        models.iter().map(|m| (m.clone(), String::new())).collect()
                                    });
                                for (model_id, capacity) in list {
                                    rows = rows.child(model_capacity_row(
                                        app, this, id, &model_id, &capacity, cx,
                                    ));
                                }
                                rows
                            }),
                    )
                    .when_some(
                        app.fetch_error.clone().filter(|(t, _)| {
                            matches!(t, FetchTarget::Edit(owner) if owner == id)
                        }),
                        |d, (_, message)| d.child(fetch_error_line(&message)),
                    )
                    .child(hint("其余字段在 settings.json 中，请直接编辑对应段。")),
                {
                    let t = this.clone();
                    move |cx: &mut App| {
                        t.update(cx, |v, cx| {
                            v.edit_customized_open = !v.edit_customized_open;
                            cx.notify();
                        });
                    }
                },
            ),
        )
        .child(
            // editorActions：取消 + 保存
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child({
                    let t = this.clone();
                    action_button("edit-cancel", "取消", false, move |_, _, cx| {
                        t.update(cx, |v, cx| {
                            v.editing_provider = None;
                            cx.notify();
                        });
                    })
                })
                .child(action_button("edit-save", "保存", true, move |_, _, cx| {
                    t_save.update(cx, |v, cx| {
                        v.save_edit(cx);
                        cx.notify();
                    });
                })),
        )
}

/// 「添加提供方」卡（web addCard = 提供方选择 + ProviderEditor hideTitle）。
fn adopt_card(app: &AppView, this: &Entity<AppView>, _cx: &App) -> Div {
    let tk = theme::t();
    let t_save = this.clone();
    let t_cancel = this.clone();

    // 目录里未被 adopt 的提供方（DeepSeek 已内置）
    let candidates: Vec<usize> = (0..PROVIDER_CATALOG.len())
        .filter(|i| !app.settings.providers.iter().any(|p| p.id == PROVIDER_CATALOG[*i].id))
        .collect();
    let pick = candidates.iter().position(|i| *i == app.adopt_pick).map(|p| candidates[p]).unwrap_or(0);
    let entry = &PROVIDER_CATALOG[pick];

    // 提供方选择（web <select class="input selectInput">：240px、h32、r8、l2 描边、
    // layer-1 底、14/22、右侧 12px chevron #81858C；点击就地展开选项列表）
    let t_toggle = this.clone();
    let entry_name = entry.name;
    let dropdown_open = app.adopt_dropdown_open;
    let chooser = div()
        .w(px(240.0))
        .child(
            div()
                .id("adopt-select")
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .pr(px(30.0))
                .pl(px(10.0))
                .h(px(32.0))
                .rounded(px(8.0))
                // web Input 原语（alpha.4）：0.5px，色阶 l2→l4
                .border(px(0.5))
                .border_color(tk.border_l4)
                .bg(tk.layer1)
                .text_size(px(theme::FONT_ROW))
                .line_height(px(22.0))
                .text_color(tk.text)
                .cursor_pointer()
                .hover(|s| s.border_color(tk.border_l3))
                .on_click(move |_, _, cx| {
                    t_toggle.update(cx, |v, cx| {
                        // 只开不关：开着时点触发器的"关"由下拉壳的窗口级
                        // mousedown 处理（先于本 click 到达），双路都动会
                        // toggle 回弹
                        if !v.adopt_dropdown_open {
                            v.adopt_dropdown_open = true;
                            cx.notify();
                        }
                    });
                })
                .child(entry_name)
                .child(
                    Icon::new(IconName::ChevronDown)
                        .absolute()
                        .right(px(12.0))
                        .size(px(12.0))
                        .text_color(tk.caption),
                ),
        );

    div()
        .relative()
        .v_flex()
        .gap(px(14.0))
        .rounded(px(12.0))
        .bg(tk.surface_2)
        .px(px(16.0))
        .py(px(14.0))
        .child(field("提供方", chooser))
        .child(field("API 密钥", Input::new(&app.adopt_key).w_full()))
        .child(
            disclosure(
                "adopt-customized",
                "自定义设置",
                app.adopt_customized_open,
                div()
                    .v_flex()
                    .gap(px(12.0))
                    .pt_3()
                    .child(field("API 地址", Input::new(&app.adopt_base).w_full()))
                    .child(
                        div()
                            .v_flex()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_size(px(theme::FONT_CAPTION))
                                    .line_height(px(theme::FONT_CAPTION_LEADING))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(tk.text_2)
                                    .child("模型目录"),
                            )
                            .child(hint(&format!("正在使用适配器默认模型（{}）", entry.model))),
                    ),
                {
                    let t = this.clone();
                    move |cx: &mut App| {
                        t.update(cx, |v, cx| {
                            v.adopt_customized_open = !v.adopt_customized_open;
                            cx.notify();
                        });
                    }
                },
            ),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(action_button("adopt-cancel", "取消", false, move |_, _, cx| {
                    t_cancel.update(cx, |v, cx| {
                        v.adding = AddingMode::None;
                        cx.notify();
                    });
                }))
                .child(action_button("adopt-save", "保存", true, move |_, window, cx| {
                    let (key, base) = t_save.read_with(cx, |v, _| (v.adopt_key.clone(), v.adopt_base.clone()));
                    let outcome = t_save.update(cx, |v, cx| {
                        let r = v.adopt_provider(cx);
                        if r.is_ok() {
                            v.adding = AddingMode::None;
                        }
                        cx.notify();
                        r
                    });
                    if outcome.is_ok() {
                        key.update(cx, |s: &mut InputState, cx| s.set_value("", window, cx));
                        base.update(cx, |s: &mut InputState, cx| s.set_value("", window, cx));
                    }
                })),
        )
        .when(dropdown_open, |card| {
            // 菜单画在卡根的最后 child：GPUI 前序绘制，后画的盖住先画的，
            // 放触发器内会被下方字段透叠。top=pad14+label18+gap6+trigger32+2；left=16 对齐触发器。
            let mut menu = div()
                .id("adopt-menu")
                .absolute()
                .top(px(72.0))
                .left(px(16.0))
                .w(px(240.0))
                .max_h(px(320.0))
                .overflow_y_scroll()
                .v_flex()
                .p(px(4.0))
                // web MenuDropdown 卡（alpha.4）：r20、border 撤掉、menu
                // 底、描边重绑 l1 画进 elevation-prominent
                .rounded(px(20.0))
                .bg(tk.menu)
                .shadow(theme::elevation_prominent());
            for ci in &candidates {
                let t = this.clone();
                let idx = *ci;
                let e = &PROVIDER_CATALOG[idx];
                menu = menu.child(
                    div()
                        .id(SharedString::from(format!("adopt-pick-{idx}")))
                        .w_full()
                        .h(px(32.0))
                        .flex()
                        .items_center()
                        .px(px(10.0))
                        .rounded(px(6.0))
                        .text_size(px(theme::FONT_ROW))
                        .line_height(px(22.0))
                        .text_color(tk.text)
                        .map(|d| if idx == pick { d.bg(tk.hover) } else { d })
                        .when(idx != pick, |d| d.hover(|s| s.bg(tk.hover)))
                        .cursor_pointer()
                        .on_click(move |_, _, cx| {
                            let idx = idx;
                            t.update(cx, |v, cx| {
                                v.adopt_pick = idx;
                                v.adopt_dropdown_open = false;
                                cx.notify();
                            });
                        })
                        .child(e.name),
                );
            }
            // 点击外部关闭：定位壳捕获菜单 bounds，canvas（paint 阶段）注册
            // 窗口级 mousedown——只认 Bubble 相，落点在菜单 bounds 外即收起
            let menu_bounds = std::sync::Arc::new(std::sync::Mutex::new(None::<Bounds<Pixels>>));
            let t_dismiss = this.clone();
            let mut overlay = div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .right_0()
                .on_children_prepainted({
                    let mb = menu_bounds.clone();
                    move |children, _, _| {
                        *mb.lock().unwrap() = children.first().cloned();
                    }
                })
                .child(menu);
            overlay = overlay.child(
                // canvas 必须脱流：文档流里的 0 高子元素也会多出一条 gap
                div().absolute().child(canvas(
                    move |_, _, _| {},
                    move |_, _, window, _| {
                        let bounds = menu_bounds
                            .lock()
                            .unwrap()
                            .clone()
                            .unwrap_or_default();
                        window.on_mouse_event(
                            move |event: &MouseDownEvent,
                                  phase: DispatchPhase,
                                  _,
                                  cx| {
                                if phase == DispatchPhase::Bubble
                                    && !bounds.contains(&event.position)
                                {
                                    t_dismiss.update(cx, |v, cx| {
                                        if v.adopt_dropdown_open {
                                            v.adopt_dropdown_open = false;
                                            cx.notify();
                                        }
                                    });
                                }
                            },
                        );
                    },
                )),
            );
            card.child(overlay)
        })
}

/// 「添加自定义提供方」卡（web CustomProviderCard：六字段 + 模型目录 + 创建）。
fn declare_card(app: &AppView, this: &Entity<AppView>, cx: &App) -> Div {
    let tk = theme::t();
    let t_create = this.clone();
    let t_cancel = this.clone();
    let t_add_model = this.clone();

    // route 校验提示（web：invalid/taken → error；否则 hint）
    let route_val = app.dc_route.read_with(cx, |s, _| s.value().trim().to_string());
    let route_msg = if !route_val.is_empty() && !valid_route_id(&route_val) {
        ("需以小写字母开头，之后可用小写字母、数字和短横线。", true)
    } else if app.settings.providers.iter().any(|p| p.id == route_val) || route_val == "deepseek" {
        ("已有提供方使用了这个 ID。", true)
    } else {
        ("以小写字母开头的标识，在请求中唯一标识该提供方，并用于派生凭据名。", false)
    };

    // 模型行列表（web .modelList/.modelEntry：l2 描边 r8 pad6 + 删除）
    let mut model_list = div().v_flex().gap_2();
    for (i, m) in app.dc_models.iter().enumerate() {
        let t = this.clone();
        model_list = model_list.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(8.0))
                // web ModelsSection .modelEntry（alpha.4）：0.5px，色阶 l2→l4
                .border(px(0.5))
                .border_color(tk.border_l4)
                .px(px(10.0))
                .py(px(6.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(theme::FONT_ROW))
                        .line_height(px(22.0))
                        .text_color(tk.text)
                        .child(m.clone()),
                )
                .child({
                    let t = t.clone();
                    div()
                        .id(SharedString::from(format!("dc-model-del-{i}")))
                        .text_size(px(theme::FONT_CAPTION))
                        .line_height(px(theme::FONT_CAPTION_LEADING))
                        .text_color(tk.text_3)
                        .cursor_pointer()
                        .hover(|s| s.text_color(tk.text_2).bg(tk.hover))
                        .on_click(move |_, _, cx| {
                            let i = i;
                            t.update(cx, |v, cx| {
                                v.dc_models.remove(i);
                                cx.notify();
                            });
                        })
                        .child("删除模型")
                }),
        );
    }

    let needs_hint = if app.dc_models.is_empty() {
        Some("自定义提供方至少需要一个模型。")
    } else {
        None
    };

    div()
        .v_flex()
        .gap(px(14.0))
        .rounded(px(12.0))
        .bg(tk.surface_2)
        .px(px(16.0))
        .py(px(14.0))
        // editorHeader
        .child(
            div()
                .flex()
                .items_baseline()
                .gap_2()
                .child(
                    div()
                        .text_size(px(theme::FONT_ROW))
                        .line_height(px(22.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(tk.text)
                        .child("自定义提供方"),
                ),
        )
        .child(field("Provider ID", Input::new(&app.dc_route).w_full()))
        .child(
            div()
                .text_size(px(theme::FONT_CAPTION))
                .line_height(px(theme::FONT_CAPTION_LEADING))
                .text_color(if route_msg.1 { tk.error } else { tk.text_3 })
                .child(route_msg.0),
        )
        .child(field("显示名称", Input::new(&app.dc_name).w_full()))
        .child(field("API 地址", Input::new(&app.dc_base).w_full()))
        .child(field("API 协议", protocol_field("openai")))
        .child(field("API 密钥", Input::new(&app.dc_key).w_full()))
        // 模型目录
                .child(
                    div()
                        .v_flex()
                        .gap(px(10.0))
                        .pt_3()
                        // web .customized（alpha.4）：0.5px 发丝
                        .border_t(px(0.5))
                        .border_color(tk.border_l2)
                        .child(
                            // web ModelListEditor 的 modelListHead：标题 + 获取按钮
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(theme::FONT_CAPTION))
                                        .line_height(px(theme::FONT_CAPTION_LEADING))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(tk.text_2)
                                        .child("模型目录"),
                                )
                                .child(fetch_link(app, this, &FetchTarget::Declare, cx)),
                        )
                        .child(model_list)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(Input::new(&app.dc_new_model).w(px(240.0)))
                                .child(link_button("dc-add-model", "添加模型", move |_, window, cx| {
                                    let entities = t_add_model.read_with(cx, |v, _| v.dc_new_model.clone());
                                    t_add_model.update(cx, |v, cx| {
                                        v.dc_push_model(cx);
                                        cx.notify();
                                    });
                                    entities.update(cx, |s: &mut InputState, cx| s.set_value("", window, cx));
                                })),
                        )
                        .children(needs_hint.map(hint)),
                )
                .when_some(
                    app.fetch_error.clone().filter(|(t, _)| matches!(t, FetchTarget::Declare)),
                    |d, (_, message)| d.child(fetch_error_line(&message)),
                )
                .when_some(app.declare_error.clone(), |d, e| {
            d.child(
                div()
                    .text_size(px(theme::FONT_CAPTION))
                    .line_height(px(theme::FONT_CAPTION_LEADING))
                    .text_color(tk.error)
                    .child(e),
            )
        })
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(action_button("declare-cancel", "取消", false, move |_, window, cx| {
                    let ents = t_cancel.read_with(cx, |v, _| {
                        (v.dc_route.clone(), v.dc_name.clone(), v.dc_base.clone(), v.dc_key.clone(), v.dc_new_model.clone())
                    });
                    t_cancel.update(cx, |v, cx| {
                        v.adding = AddingMode::None;
                        v.dc_models.clear();
                        cx.notify();
                    });
                    let (a, b, c, d, e) = ents;
                    for ent in [a, b, c, d, e] {
                        ent.update(cx, |s: &mut InputState, cx| s.set_value("", window, cx));
                    }
                }))
                .child(action_button("declare-create", "创建提供方", true, move |_, window, cx| {
                    let outcome = t_create.update(cx, |v, cx| {
                        match v.declare_provider(cx) {
                            Ok(()) => {
                                v.adding = AddingMode::None;
                                v.dc_models.clear();
                                v.declare_error = None;
                                true
                            }
                            Err(e) => {
                                v.declare_error = Some(e);
                                false
                            }
                        }
                    });
                    t_create.update(cx, |_, cx| cx.notify());
                    if outcome {
                        let ents = t_create.read_with(cx, |v, _| {
                            (v.dc_route.clone(), v.dc_name.clone(), v.dc_base.clone(), v.dc_key.clone(), v.dc_new_model.clone())
                        });
                        let (a, b, c, d, e) = ents;
                        for ent in [a, b, c, d, e] {
                            ent.update(cx, |s: &mut InputState, cx| s.set_value("", window, cx));
                        }
                    }
                })),
        )
}

/// 字段行（web .field：label 12/18 wt500 secondary + 控件 gap6）。
fn field(label: &str, control: impl IntoElement) -> Div {
    let tk = theme::t();
    div()
        .v_flex()
        .gap(px(6.0))
        .child(
            div()
                .text_size(px(theme::FONT_CAPTION))
                .line_height(px(theme::FONT_CAPTION_LEADING))
                .font_weight(FontWeight::MEDIUM)
                .text_color(tk.text_2)
                .child(label.to_string()),
        )
        .child(control)
}

/// 12/18 tertiary 提示行（web .advancedHint）。
/// 表单行内错误提示（上游 `.error`：12/18、state-error）。
fn err_hint(text: &str) -> Div {
    div()
        .text_size(px(theme::FONT_CAPTION))
        .line_height(px(theme::FONT_CAPTION_LEADING))
        .text_color(theme::t().error)
        .child(text.to_string())
}

fn hint(text: &str) -> Div {
    let tk = theme::t();
    div()
        .text_size(px(theme::FONT_CAPTION))
        .line_height(px(theme::FONT_CAPTION_LEADING))
        .text_color(tk.text_3)
        .child(text.to_string())
}

/// 协议字段（web select：右侧 12px chevron 的胶囊框，恒 openai）。
fn protocol_field(value: &'static str) -> Div {
    let tk = theme::t();
    div()
        .w(px(240.0))
        .h(px(32.0))
        .flex()
        .items_center()
        .justify_between()
        .px(px(10.0))
        .rounded(px(8.0))
        // web Input 原语（alpha.4）：0.5px，色阶 l2→l4
        .border(px(0.5))
        .border_color(tk.border_l4)
        .bg(tk.layer1)
        .text_size(px(theme::FONT_ROW))
        .line_height(px(22.0))
        .text_color(tk.text)
        .child(value)
        .child(Icon::new(IconName::ChevronDown).size(px(12.0)).text_color(tk.caption))
}

/// 折叠区（web details.customized：summary 12/18 wt500 secondary + 旋转 chevron）。
fn disclosure(id: &'static str, label: &'static str, open: bool, body: Div, on_toggle: impl Fn(&mut App) + 'static) -> Div {
    let tk = theme::t();
    let mut col = div().v_flex().child(
        div()
            .id(id)
                        .flex()
            .items_center()
            .gap(px(6.0))
            .px_1()
            .text_size(px(theme::FONT_CAPTION))
            .line_height(px(theme::FONT_CAPTION_LEADING))
            .font_weight(FontWeight::MEDIUM)
            .text_color(tk.text_2)
            .cursor_pointer()
            .hover(|s| s.text_color(tk.text))
            .on_click(move |_, _, cx| on_toggle(cx))
            .child(if open { "▾" } else { "▸" })
            .child(label),
    );
    if open {
        col = col.child(body);
    }
    col
}

/// h36 r18 动作按钮（web .primaryButton/.secondaryButton）。
/// primary = 品牌填充（暗色主题为浅底深字的反转设计）。
fn action_button(
    id: impl Into<ElementId>,
    label: &'static str,
    primary: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let tk = theme::t();
    div()
        .id(id)
        .h(px(dsh_gpui::ui_metrics::SETTINGS_BUTTON_MD_H_PX))
        .px(px(dsh_gpui::ui_metrics::SETTINGS_BUTTON_MD_PAD_X_PX))
        .flex()
        .items_center()
        .justify_center()
        // web Button 基座 `--dsw-radius-md` = R12（曾写 R18 胶囊）
        .rounded(px(dsh_gpui::ui_metrics::SETTINGS_BUTTON_MD_RADIUS_PX))
        .map(|d| {
            if primary {
                // web button-primary：fill=brand-primary（暗=近白/亮=墨），字=foreground 反色
                d.bg(tk.text)
                    .text_color(tk.bg_base)
                    .hover(|s| s.opacity(0.9))
            } else {
                // web Button .outline（alpha.4）：0.5px，色阶 l2→l3
                d.border(px(0.5))
                    .border_color(tk.border_l3)
                    .text_color(tk.text)
                    .hover(|s| s.bg(tk.hover))
            }
        })
        .text_size(px(dsh_gpui::ui_metrics::SETTINGS_BUTTON_MD_FONT_PX))
        .line_height(px(dsh_gpui::ui_metrics::SETTINGS_BUTTON_MD_LEADING_PX))
        .cursor_pointer()
        .on_click(on_click)
        .child(label)
}

/// 模型行的容量读数 + 就地编辑（#95 选项③）。
///
/// 上游把 `contextWindow` 当 provider 声明的元数据（模型页只读展示）；
/// rustdsh 的目录探测可能拿不到该字段（如火山 ark 的 openai-compatible
/// 列表），此时占用表整枚不渲染。这里给用户一个手填入口：点容量 chip 展开
/// 输入框，接受 `128K` / `131072`；空值 = 清除声明。
fn model_capacity_row(
    app: &AppView,
    this: &Entity<AppView>,
    provider: &str,
    model: &str,
    declared: &str,
    _cx: &App,
) -> Div {
    let tk = theme::t();
    let editing = app
        .model_capacity_edit
        .as_ref()
        .is_some_and(|(p, m)| p == provider && m == model);
    let resolved = dsh_gpui::capacity::resolve_capacity(declared, provider, model);
    let label = match (declared.trim().is_empty(), resolved) {
        (false, Some(_)) => declared.to_string(),
        // 没声明但目录能兜底：显示兜底值并标注来源，用户才知道真值从哪来
        (true, Some(v)) => format!("{}(目录)", dsh_gpui::capacity::format_capacity(v)),
        (true, None) => "未声明".to_string(),
        (false, None) => declared.to_string(),
    };
    let mut row = div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(px(theme::FONT_CAPTION))
                .line_height(px(theme::FONT_CAPTION_LEADING))
                .text_color(tk.text_2)
                .child(model.to_string()),
        );
    // chip id 固定（同一时刻只编辑一行，不再按 provider/model 分 id——
    // 之前每次渲染 id 都变，点击态会丢）
    let chip_id = SharedString::from("model-capacity-chip");
    if editing {
        let t_save = this.clone();
        let t_cancel = this.clone();
        row = row
            .child(
                // 输入框要够宽（占位「如 128K 或 131072」不截断）；展开时
                // 由 chip 的点击处 focus，否则键盘输入落空。
                div().w(px(200.0)).child(Input::new(&app.model_capacity_input)),
            )
            .child(compact_action_button(
                SharedString::from(format!("cap-save-{provider}-{model}")),
                "保存",
                false,
                move |_, _, cx| {
                    t_save.update(cx, |v, cx| {
                        v.commit_model_capacity(cx);
                        cx.notify();
                    });
                },
            ))
            .child(compact_action_button(
                SharedString::from(format!("cap-cancel-{provider}-{model}")),
                "取消",
                false,
                move |_, _, cx| {
                    t_cancel.update(cx, |v, cx| {
                        v.model_capacity_edit = None;
                        cx.notify();
                    });
                },
            ));
    } else {
        let t = this.clone();
        let provider_owned = provider.to_string();
        let model_owned = model.to_string();
        let current = declared.to_string();
        row = row.child(
            div()
                .id(chip_id.clone())
                .flex_none()
                .px(px(6.0))
                .py(px(1.0))
                .rounded(px(dsh_gpui::ui_metrics::RADIUS_XS_PX))
                .border(px(0.5))
                .border_color(tk.border_l3)
                .text_size(px(11.0))
                .line_height(px(16.0))
                .text_color(tk.text_3)
                .cursor_pointer()
                .hover(|s| s.bg(tk.hover))
                .child(label)
                .on_click({
                    let t = t.clone();
                    move |_, window, cx| {
                        let (p, m, cur) =
                            (provider_owned.clone(), model_owned.clone(), current.clone());
                        t.update(cx, |v, cx| {
                            v.model_capacity_edit = Some((p, m));
                            cx.notify();
                            // 与侧栏搜索展开同序：先 notify 让输入框挂载，
                            // 再 focus（挂载前 focus 会被丢弃）。
                            let input = v.model_capacity_input.clone();
                            input.update(cx, |state, cx| {
                                state.set_value(cur, window, cx);
                                state.focus(window, cx);
                            });
                        });
                    }
                }),
        );
    }
    row
}

/// 紧凑档行内按钮（web `Button.sm`：高 28、pad 0 10、R8、12/18）——
/// 提供方行卡的编辑/删除，不占页脚 md 档的尺寸。`danger` 走 web
/// `.dangerButton`：无描边、错误色文字、红色 hover 底。
fn compact_action_button(
    id: impl Into<ElementId>,
    label: &'static str,
    danger: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let tk = theme::t();
    let mut button = div()
        .id(id)
        .h(px(dsh_gpui::ui_metrics::SETTINGS_BUTTON_SM_H_PX))
        .px(px(dsh_gpui::ui_metrics::SETTINGS_BUTTON_SM_PAD_X_PX))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(dsh_gpui::ui_metrics::SETTINGS_BUTTON_SM_RADIUS_PX))
        .text_size(px(dsh_gpui::ui_metrics::SETTINGS_BUTTON_SM_FONT_PX))
        .line_height(px(dsh_gpui::ui_metrics::SETTINGS_BUTTON_SM_LEADING_PX))
        .cursor_pointer();
    button = if danger {
        button
            .text_color(tk.error)
            .hover(|s| s.bg(tk.danger_hover))
    } else {
        button
            .border(px(0.5))
            .border_color(tk.border_l3)
            .text_color(tk.text)
            .hover(|s| s.bg(tk.hover))
    };
    button.on_click(on_click).child(label)
}

/// h28 linkButton（web .linkButton：透明、12/18 tertiary、hover 白8%）。
fn link_button(
    id: impl Into<ElementId>,
    label: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let tk = theme::t();
    div()
        .id(id)
        .h(px(28.0))
        .px(px(10.0))
        .flex()
        .items_center()
        .rounded(px(14.0))
        .text_size(px(theme::FONT_CAPTION))
        .line_height(px(theme::FONT_CAPTION_LEADING))
        .text_color(tk.text_3)
        .cursor_pointer()
        .hover(|s| s.bg(tk.hover).text_color(tk.text_2))
        .on_click(on_click)
        .child(label)
}

/// 添加区虚线按钮（web .addButton：等宽、h44、r12、l3 虚线、+ 图标）。
fn add_button(
    id: &'static str,
    label: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let tk = theme::t();
    div()
        .id(id)
        .flex_1()
        .min_w(px(180.0))
        .h(px(44.0))
        .flex()
        .items_center()
        .justify_center()
        .gap_1p5()
        // web .addCard（alpha.4）：r12→16（虚线描边保持 1px l3）
        .rounded(px(16.0))
        .border_1()
        .border_dashed()
        .border_color(tk.border_l3)
        .text_size(px(theme::FONT_ROW))
        .line_height(px(22.0))
        .text_color(tk.text)
        .cursor_pointer()
        .hover(|s| s.bg(tk.hover))
        .on_click(on_click)
        .child(Icon::new(IconName::Plus).size(px(14.0)).text_color(tk.text_2))
        .child(label)
}



/// 空态页（插件 / Agent 预设）。
fn placeholder_page(title: &str, desc: &str) -> Div {
    let tk = theme::t();
    div()
        .v_flex()
        .pt_2()
        .gap_2()
        .child(
            div()
                .text_size(px(16.0))
                .line_height(px(24.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(tk.text)
                .child(title.to_string()),
        )
        .child(
            div()
                .text_size(px(theme::FONT_ROW))
                .line_height(px(22.0))
                .text_color(tk.text_3)
                .child(desc.to_string()),
        )
}

// --- 获取可用模型（web ModelListEditor 的 fetch 流程） -----------------------

/// 卡内失败行（web ModelListEditor 的 p.error）：Host 诊断按原样展示。
fn fetch_error_line(message: &str) -> Div {
    let tk = theme::t();
    div()
        .text_size(px(theme::FONT_CAPTION))
        .line_height(px(theme::FONT_CAPTION_LEADING))
        .text_color(tk.error)
        .child(message.to_string())
}

/// web ModelListEditor 的 fetch linkButton：busy 显「正在询问提供方…」；
/// 不可探测（API 地址为空）时禁用——web 以 title 提示 fetchNeedsBaseUrl，
/// 此处无 tooltip 通道，禁用态同款置灰。几何照抄上游 ModelsSection
/// `.linkButton`：高 28、pad 0 10px、`--dsw-radius-sm`（R8）、12px 字、
/// hover 变 label-secondary + hover 底。
fn fetch_link(app: &AppView, this: &Entity<AppView>, target: &FetchTarget, cx: &App) -> Stateful<Div> {
    use dsh_gpui::ui_metrics as fm;
    let tk = theme::t();
    let busy = app.fetch_pending.as_ref() == Some(target);
    let askable = app.fetch_askable(target, cx);
    let key = match target {
        FetchTarget::Declare => "declare".to_string(),
        FetchTarget::Edit(id) => format!("edit-{id}"),
    };
    let base = div()
        .id(SharedString::from(format!("fetch-models-{key}")))
        .h(px(fm::LINK_BUTTON_H_PX))
        .px(px(fm::LINK_BUTTON_PAD_X_PX))
        .flex()
        .items_center()
        .rounded(px(fm::LINK_BUTTON_RADIUS_PX))
        .text_size(px(fm::LINK_BUTTON_FONT_PX))
        .line_height(px(fm::LINK_BUTTON_LEADING_PX))
        .child(if busy { "正在询问提供方…" } else { "获取可用模型" });
    if busy || !askable {
        base.text_color(tk.text_3)
    } else {
        let t = this.clone();
        let target = target.clone();
        base.text_color(tk.text_3)
            .cursor_pointer()
            .hover(|s| s.bg(tk.hover).text_color(tk.text_2))
            .on_click(move |_, window, cx| {
                t.update(cx, |v, cx| v.fetch_models(target.clone(), cx));
                // web fetchModels 前 setCandidateQuery('')：搜索词清空。
                let query = t.read_with(cx, |v, _| v.fetch_query.clone());
                query.update(cx, |s: &mut InputState, cx| s.set_value("", window, cx));
            })
    }
}

/// 候选行勾选框（web 原生 checkbox 的等价物：14px 圆角方，勾选填充反色）。
fn fetch_check(picked: bool) -> Div {
    let tk = theme::t();
    let base = div()
        .flex_none()
        .w(px(14.0))
        .h(px(14.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.0));
    if picked {
        base.bg(tk.text)
            .child(Icon::new(IconName::Check).size(px(10.0)).text_color(tk.bg_base))
    } else {
        base.border(px(0.5)).border_color(tk.border_l4)
    }
}

/// 候选弹层（web ModelListEditor 的 fetchDialog Modal）：标题/描述/搜索 +
/// 全选切换/勾选列表（上限 320px 滚动）/取消 + 添加所选。
fn fetch_modal(app: &AppView, this: &Entity<AppView>, _target: &FetchTarget, cx: &App) -> Div {
    let tk = theme::t();
    let visible = app.fetch_visible(cx);
    let any_visible = !visible.is_empty();
    let all_picked = any_visible
        && visible
            .iter()
            .all(|i| app.fetch_picked.contains(&app.fetch_candidates[*i].id));

    // 候选行（web .candidate：r6 行 + 原生 checkbox + id）
    let mut list = div().v_flex().gap(px(2.0));
    for i in visible {
        let model = app.fetch_candidates[i].clone();
        let picked = app.fetch_picked.contains(&model.id);
        let t = this.clone();
        let cid = model.id.clone();
        list = list.child(
            div()
                .id(SharedString::from(format!("fetch-cand-{i}")))
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(8.0))
                .py(px(6.0))
                .rounded(px(6.0))
                .cursor_pointer()
                .hover(|s| s.bg(tk.hover))
                .on_click(move |_, _, cx| {
                    t.update(cx, |v, cx| v.fetch_toggle(&cid, cx));
                })
                .child(fetch_check(picked))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(px(theme::FONT_ROW))
                        .line_height(px(20.0))
                        .text_color(tk.text)
                        .child(model.id.clone()),
                ),
        );
    }

    div()
        .absolute()
        .size_full()
        .top_0()
        .left_0()
        .bg(gpui::hsla(0.0, 0.0, 0.0, 0.3))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(520.0))
                .v_flex()
                .gap(px(10.0))
                .p(px(20.0))
                // web Modal 卡（alpha.4）：r24、描边 l1 画进 elevation-prominent
                .rounded(px(24.0))
                .bg(tk.surface)
                .shadow(theme::elevation_prominent())
                .child(
                    div()
                        .text_size(px(theme::FONT_ROW))
                        .line_height(px(22.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(tk.text)
                        .child("选择要添加的模型"),
                )
                .child(
                    div()
                        .text_size(px(theme::FONT_CAPTION))
                        .line_height(px(theme::FONT_CAPTION_LEADING))
                        .text_color(tk.text_3)
                        .child("以下是模型提供方的可用模型，勾选要添加的模型。"),
                )
                .child(
                    // web .candidateToolbar：搜索框（flex-1）+ 全选切换
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .mb_1()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&app.fetch_query).w_full()),
                        )
                        .child({
                            let t = this.clone();
                            let toggle = div()
                                .id("fetch-toggle-all")
                                .h(px(28.0))
                                .px(px(10.0))
                                .flex()
                                .items_center()
                                .rounded(px(14.0))
                                .text_size(px(theme::FONT_CAPTION))
                                .line_height(px(theme::FONT_CAPTION_LEADING))
                                .child(if all_picked { "取消全选" } else { "全选" });
                            if !any_visible {
                                toggle.text_color(tk.text_3)
                            } else {
                                toggle
                                    .text_color(tk.text_2)
                                    .cursor_pointer()
                                    .hover(|s| s.bg(tk.hover).text_color(tk.text))
                                    .on_click(move |_, _, cx| {
                                        t.update(cx, |v, cx| v.fetch_toggle_visible(cx));
                                    })
                            }
                        }),
                )
                .child(if !any_visible {
                    div()
                        .id("fetch-empty")
                        .text_size(px(theme::FONT_CAPTION))
                        .line_height(px(theme::FONT_CAPTION_LEADING))
                        .text_color(tk.text_3)
                        .child("没有匹配的模型。")
                } else {
                    div()
                        .id("fetch-cand-list")
                        .max_h(px(320.0))
                        .overflow_y_scroll()
                        .child(list)
                })
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child({
                            let t = this.clone();
                            action_button("fetch-cancel", "取消", false, move |_, window, cx| {
                                let query = t.read_with(cx, |v, _| v.fetch_query.clone());
                                t.update(cx, |v, cx| v.fetch_close(cx));
                                query.update(cx, |s: &mut InputState, cx| s.set_value("", window, cx));
                            })
                        })
                        .child({
                            let t = this.clone();
                            action_button("fetch-adopt", "添加所选", false, move |_, window, cx| {
                                let query = t.read_with(cx, |v, _| v.fetch_query.clone());
                                t.update(cx, |v, cx| v.fetch_adopt(cx));
                                query.update(cx, |s: &mut InputState, cx| s.set_value("", window, cx));
                            })
                        }),
                ),
        )
}
