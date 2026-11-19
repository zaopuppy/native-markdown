//! Application-owned search labels. No component-library patches.
use gpui::SharedString;
use std::ops::Range;

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
        gpui::div().w_full().min_w_0().child(inline::Inline::new(
            "search-text",
            self.text,
            self.highlights,
        ))
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

mod inline {
    //! Native search-result text with shaped highlights and painted backgrounds.
    use gpui::{
        point, px, quad, App, BorderStyle, Bounds, Edges, Element, ElementId, GlobalElementId,
        HighlightStyle, InspectorElementId, IntoElement, LayoutId, Pixels, SharedString,
        StyledText, Window,
    };
    use std::ops::Range;

    pub(super) struct Inline {
        id: ElementId,
        text: SharedString,
        highlights: Vec<(Range<usize>, HighlightStyle)>,
        styled_text: StyledText,
    }
    impl Inline {
        pub(super) fn new(
            id: impl Into<ElementId>,
            text: SharedString,
            highlights: Vec<(Range<usize>, HighlightStyle)>,
        ) -> Self {
            Self {
                id: id.into(),
                text: text.clone(),
                highlights,
                styled_text: StyledText::new(text),
            }
        }
    }
    impl IntoElement for Inline {
        type Element = Self;
        fn into_element(self) -> Self {
            self
        }
    }
    impl Element for Inline {
        type RequestLayoutState = ();
        type PrepaintState = ();
        fn id(&self) -> Option<ElementId> {
            Some(self.id.clone())
        }
        fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
            None
        }
        fn request_layout(
            &mut self,
            global_element_id: Option<&GlobalElementId>,
            inspector_id: Option<&InspectorElementId>,
            window: &mut Window,
            cx: &mut App,
        ) -> (LayoutId, ()) {
            let text_style = window.text_style();
            let mut runs = Vec::new();
            let mut ix = 0;
            for (range, highlight) in self.highlights.iter() {
                if ix < range.start {
                    runs.push(text_style.clone().to_run(range.start - ix));
                }
                runs.push(text_style.clone().highlight(*highlight).to_run(range.len()));
                ix = range.end;
            }
            if ix < self.text.len() {
                runs.push(text_style.to_run(self.text.len() - ix));
            }

            self.styled_text = StyledText::new(self.text.clone()).with_runs(runs);
            let (layout_id, _) =
                self.styled_text
                    .request_layout(global_element_id, inspector_id, window, cx);

            (layout_id, ())
        }

        fn prepaint(
            &mut self,
            id: Option<&GlobalElementId>,
            inspector_id: Option<&InspectorElementId>,
            bounds: Bounds<Pixels>,
            _: &mut (),
            window: &mut Window,
            cx: &mut App,
        ) {
            self.styled_text
                .prepaint(id, inspector_id, bounds, &mut (), window, cx);
        }
        fn paint(
            &mut self,
            global_id: Option<&GlobalElementId>,
            _: Option<&InspectorElementId>,
            bounds: Bounds<Pixels>,
            _: &mut (),
            _: &mut (),
            window: &mut Window,
            cx: &mut App,
        ) {
            let text_layout = self.styled_text.layout().clone();
            for (range, highlight) in &self.highlights {
                let Some(color) = highlight.background_color else {
                    continue;
                };
                let mut cursor = range.start;
                while cursor < range.end {
                    let next = cursor + self.text[cursor..].chars().next().unwrap().len_utf8();
                    if let (Some(start), Some(end)) = (
                        text_layout.position_for_index(cursor),
                        text_layout.position_for_index(next),
                    ) {
                        let right = if end.y == start.y {
                            end.x
                        } else {
                            bounds.right()
                        };
                        let rect = Bounds::from_corners(
                            start,
                            point(
                                right.max(start.x + px(2.)),
                                start.y + text_layout.line_height(),
                            ),
                        );
                        window.paint_quad(quad(
                            rect,
                            px(1.),
                            color,
                            Edges::default(),
                            gpui::transparent_black(),
                            BorderStyle::default(),
                        ));
                    }
                    cursor = next;
                }
            }
            self.styled_text
                .paint(global_id, None, bounds, &mut (), &mut (), window, cx);
        }
    }
}
