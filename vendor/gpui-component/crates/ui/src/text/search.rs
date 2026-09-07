use gpui::{Bounds, Hsla, Pixels, SharedString};
use std::{
    ops::Range,
    sync::{Arc, Mutex},
};

/// Inline text with painted backgrounds, preserving shaping and wrapping across marks.
#[derive(gpui::IntoElement)]
pub struct SearchText {
    text: SharedString,
    highlights: Vec<(Range<usize>, gpui::HighlightStyle)>,
}
impl SearchText {
    pub fn new(
        text: impl Into<SharedString>,
        highlights: Vec<(Range<usize>, gpui::HighlightStyle)>,
    ) -> Self {
        Self {
            text: text.into(),
            highlights,
        }
    }
}
impl gpui::RenderOnce for SearchText {
    fn render(self, _: &mut gpui::Window, _: &mut gpui::App) -> impl gpui::IntoElement {
        use gpui::{ParentElement, Styled};
        let state = Arc::new(Mutex::new(super::inline::InlineState::default()));
        state.lock().unwrap().set_text(self.text);
        gpui::div()
            .w_full()
            .min_w_0()
            .child(super::inline::Inline::new(
                "search-text",
                state,
                vec![],
                self.highlights,
            ))
    }
}

/// Geometry of the painted search occurrence, for accessibility and rendering tests.
#[derive(Clone, Debug, Default)]
pub struct SearchGeometry {
    pub text: SharedString,
    pub bounds: Option<Bounds<Pixels>>,
    pub viewport: Option<Bounds<Pixels>>,
    pub background_color: Option<Hsla>,
    pub painted: bool,
    pub(crate) centered_request: Option<u64>,
    pub(crate) reveal_request: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct SearchHandle(pub(crate) Arc<Mutex<SearchGeometry>>);
impl SearchHandle {
    pub fn geometry(&self) -> SearchGeometry {
        self.0.lock().unwrap().clone()
    }
    pub fn clear(&self) {
        *self.0.lock().unwrap() = SearchGeometry::default();
    }
}

/// Original byte ranges, even when Unicode lowercase changes byte length.
pub fn search_ranges(text: &str, query: &str) -> Vec<Range<usize>> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return vec![];
    }
    let mut folded = String::new();
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    for (start, ch) in text.char_indices() {
        for c in ch.to_lowercase() {
            folded.push(c);
            starts.extend(std::iter::repeat_n(start, c.len_utf8()));
            ends.extend(std::iter::repeat_n(start + ch.len_utf8(), c.len_utf8()));
        }
    }
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for (start, _) in folded.match_indices(&query) {
        let range = starts[start]..ends[start + query.len() - 1];
        if ranges.last() != Some(&range) {
            ranges.push(range);
        }
    }
    ranges
}

#[derive(Clone, Debug)]
pub(crate) struct SearchRequest {
    pub query: SharedString,
    pub index: usize,
    pub request_id: u64,
    pub handle: SearchHandle,
}

#[derive(Clone, Debug)]
pub(crate) struct SearchPaint {
    pub range: Range<usize>,
    pub request: SearchRequest,
}
