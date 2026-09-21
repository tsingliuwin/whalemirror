mod format;
mod inline;
#[allow(unused_imports)]
pub use inline::set_relative_file_opener;
mod node;
mod style;
mod text_view;
mod utils;

use gpui::App;
pub use style::*;
pub use text_view::*;

pub(crate) fn init(cx: &mut App) {
    text_view::init(cx);
}
