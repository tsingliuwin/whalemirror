mod document;
mod format;
mod inline;
mod inline_element;
mod inline_flow;
mod inline_object;
#[cfg(test)]
mod inline_virtual_tests;
mod markdown_ext;
mod node;
mod range_highlight;
pub(crate) mod selection;
mod selection_adapter;
mod state;
mod stream_fade;
mod style;
mod text_view;
mod utils;

use gpui::{App, ElementId, IntoElement, RenderOnce, SharedString, Window};
pub use inline_element::*;
pub use markdown_ext::*;
pub use node::{CodeBlock, TableData};
pub use range_highlight::{RangeHighlight, RangeHighlightError, RenderedText};
pub use state::*;
pub use stream_fade::TextViewMotion;
pub use style::*;
pub use text_view::*;

// [dsh] 相对/绝对文件路径的打开钩子：宿主（桌面壳）注入，把聊天流里指向
// 真实文件的路径路由进侧栏文件预览，而非系统默认程序。仅对无 scheme 的
// 路径生效；http(s)/mailto 等仍走系统打开。None = 回落系统打开。
static RELATIVE_FILE_OPENER: std::sync::RwLock<
    Option<std::sync::Arc<dyn Fn(&str, &mut gpui::App) + Send + Sync>>,
> = std::sync::RwLock::new(None);

/// [dsh] 注入/移除文件路径打开回调。
pub fn set_relative_file_opener(
    opener: Option<std::sync::Arc<dyn Fn(&str, &mut gpui::App) + Send + Sync>>,
) {
    *RELATIVE_FILE_OPENER.write().unwrap() = opener;
}

/// [dsh] 无 scheme 的真实路径经钩子打开。返回 true = 已消费。
pub fn try_relative_file_opener(url: &str, cx: &mut gpui::App) -> bool {
    let is_path = !url.is_empty()
        && !url.contains("://")
        && !url.starts_with("mailto:")
        && std::path::Path::new(url).exists();
    if is_path {
        if let Some(opener) = RELATIVE_FILE_OPENER.read().unwrap().as_ref() {
            opener(url, cx);
            return true;
        }
    }
    false
}

/// [dsh] 空 href 或不存在路径不交给系统打开：LLM 常输出引用式链接
/// （[text][1] 无定义 → url 为空）与相对路径——Windows ShellExecute
/// 会弹「找不到文件」错误框。有 scheme 的 URL 放行。
pub fn dsh_openable(url: &str) -> bool {
    let u = url.trim();
    if u.is_empty() {
        return false;
    }
    if u.contains("://") || u.starts_with("mailto:") {
        return true;
    }
    std::path::Path::new(u).exists()
}

pub(crate) fn init(cx: &mut App) {
    state::init(cx);
}

/// Create a new markdown text view with code location as id.
#[track_caller]
pub fn markdown(source: impl Into<SharedString>) -> TextView {
    let id: ElementId = ElementId::CodeLocation(*std::panic::Location::caller());
    TextView::markdown(id, source)
}

/// Create a new html text view with code location as id.
#[track_caller]
pub fn html(source: impl Into<SharedString>) -> TextView {
    let id: ElementId = ElementId::CodeLocation(*std::panic::Location::caller());
    TextView::html(id, source)
}

#[derive(IntoElement, Clone)]
pub enum Text {
    String(SharedString),
    TextView(Box<TextView>),
}

impl From<SharedString> for Text {
    fn from(s: SharedString) -> Self {
        Self::String(s)
    }
}

impl From<&str> for Text {
    fn from(s: &str) -> Self {
        Self::String(SharedString::from(s.to_string()))
    }
}

impl From<String> for Text {
    fn from(s: String) -> Self {
        Self::String(s.into())
    }
}

impl From<TextView> for Text {
    fn from(e: TextView) -> Self {
        Self::TextView(Box::new(e))
    }
}

impl Text {
    /// Set the style for [`TextView`].
    ///
    /// Do nothing if this is `String`.
    pub fn style(self, style: TextViewStyle) -> Self {
        match self {
            Self::String(s) => Self::String(s),
            Self::TextView(e) => Self::TextView(Box::new(e.style(style))),
        }
    }

    /// Get the text content.
    #[doc(hidden)]
    pub fn get_text(&self, cx: &App) -> SharedString {
        match self {
            Self::String(s) => s.clone(),
            Self::TextView(view) => {
                if let Some(state) = &view.state {
                    state.read(cx).source()
                } else {
                    SharedString::default()
                }
            }
        }
    }
}

impl RenderOnce for Text {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        match self {
            Self::String(s) => s.into_any_element(),
            Self::TextView(e) => e.into_any_element(),
        }
    }
}
