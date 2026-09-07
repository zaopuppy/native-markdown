mod format;
mod inline;
mod node;
mod style;
mod search;
mod text_view;
mod utils;

use gpui::App;
pub use style::*;
pub use search::{SearchHandle, SearchGeometry, SearchText, search_ranges};
pub use text_view::*;

pub(crate) fn init(cx: &mut App) {
    text_view::init(cx);
}
