//! External source search implemented exclusively through upstream editor APIs.
use gpui::{canvas, point, prelude::*, px, rgb, App, Entity, Window};
#[cfg(test)]
use gpui::{Bounds, Pixels};
use gpui_component::input::{
    BufferPoint, EditorState, RopeExt, TextDecoration, TextDecorationCollection,
};
use std::{cell::RefCell, ops::Range, rc::Rc};

#[cfg(test)]
#[derive(Clone, Debug, Default)]
pub(crate) struct SearchGeometry {
    pub text: gpui::SharedString,
    pub bounds: Option<Bounds<Pixels>>,
    pub viewport: Option<Bounds<Pixels>>,
    pub painted: bool,
}

#[derive(Default)]
struct Marker {
    target: Option<(Range<usize>, u64)>,
    dirty: bool,
    centered_request: Option<u64>,
    decorations: Option<TextDecorationCollection>,
    #[cfg(test)]
    geometry: SearchGeometry,
}

#[derive(Clone, Default)]
pub(crate) struct SourceSearch(Rc<RefCell<Marker>>);

impl SourceSearch {
    pub fn dismiss(&self, cx: &mut App) {
        self.clear();
        // Clear before editor prepaint: changing decorations from the observer's canvas
        // can leave the editor's cached text painted with the previous highlight.
        if let Some(decorations) = self.0.borrow().decorations.clone() {
            decorations.clear(cx);
        }
    }

    pub fn clear(&self) {
        let mut state = self.0.borrow_mut();
        state.target = None;
        state.dirty = true;
        state.centered_request = None;
        #[cfg(test)]
        {
            state.geometry = SearchGeometry::default();
        }
    }

    pub fn set_match(&self, range: Option<Range<usize>>, request: u64) {
        self.clear();
        self.0.borrow_mut().target = range.map(|range| (range, request));
    }

    #[cfg(test)]
    pub fn geometry(&self) -> SearchGeometry {
        self.0.borrow().geometry.clone()
    }

    #[cfg(test)]
    pub fn highlight_ranges(&self, cx: &App) -> Vec<Range<usize>> {
        self.0
            .borrow()
            .decorations
            .as_ref()
            .map_or_else(Vec::new, |marks| marks.get_ranges(cx))
    }

    /// Runs after the editor's layout without adding a second input or hit target.
    pub fn observer(&self, editor: Entity<EditorState>) -> impl IntoElement {
        let marker = self.clone();
        canvas(
            move |_, window, cx| marker.update(&editor, window, cx),
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
    }

    fn update(&self, editor: &Entity<EditorState>, window: &mut Window, cx: &mut App) {
        let (target, dirty, decorations, centered) = {
            let mut state = self.0.borrow_mut();
            let dirty = std::mem::take(&mut state.dirty);
            (
                state.target.clone(),
                dirty,
                state.decorations.clone(),
                state.centered_request,
            )
        };
        if dirty {
            let marks = target
                .as_ref()
                .filter(|(range, _)| {
                    range.start < range.end && range.end <= editor.read(cx).text().len()
                })
                .map(|(range, _)| {
                    vec![TextDecoration::new(
                        range.clone(),
                        gpui::HighlightStyle {
                            background_color: Some(rgb(0x3730a3).into()),
                            color: Some(rgb(0xffffff).into()),
                            ..Default::default()
                        },
                    )]
                })
                .unwrap_or_default();
            if let Some(decorations) = decorations {
                decorations.set(marks, cx);
            } else if !marks.is_empty() {
                let decorations = editor.update(cx, |editor, cx| {
                    editor.create_decorations_collection(marks, cx)
                });
                self.0.borrow_mut().decorations = Some(decorations);
            }
            if let Some((range, _)) = &target {
                editor.update(cx, |editor, cx| {
                    if range.start < editor.text().len() {
                        editor.unfold_at(editor.text().offset_to_position(range.start), cx);
                    }
                });
            }
        }
        let Some((range, request)) = target else {
            return;
        };
        let input = editor.read(cx);
        if range.start >= range.end || range.end > input.text().len() {
            self.clear();
            return;
        }
        let viewport = input.input_bounds();
        if viewport.size.height <= px(0.) {
            return;
        }
        // Center the first character, including when a match spans formatted source lines.
        let text = input.text().slice(range.clone());
        let first_end = range.start + text.chars().next().map_or(0, char::len_utf8);
        let bounds = input.range_to_bounds(&(range.start..first_end));
        #[cfg(test)]
        {
            self.0.borrow_mut().geometry = SearchGeometry {
                text: text.to_string().into(),
                bounds,
                viewport: Some(viewport),
                painted: bounds.is_some_and(|bounds| viewport.intersects(&bounds)),
            };
        }
        if centered == Some(request) {
            return;
        }
        let Some(line_height) = input.line_height() else {
            return;
        };
        let mut offset = input.scroll_offset();
        if let Some(bounds) = bounds {
            offset.y -= bounds.center().y - viewport.center().y;
            // Reveal horizontally without using the cursor or changing the selection.
            if bounds.left() < viewport.left() {
                offset.x += viewport.left() - bounds.left();
            } else if bounds.right() > viewport.right() {
                offset.x -= bounds.right() - viewport.right();
            }
            self.0.borrow_mut().centered_request = Some(request);
        } else {
            // DisplayMap accounts for soft wraps and folds even for offscreen targets.
            let position = input.text().offset_to_point(range.start);
            let display = input
                .display_map()
                .buffer_pos_to_display_pos(BufferPoint::new(position.row, position.column));
            offset = point(
                offset.x,
                viewport.size.height / 2. - line_height * (display.row as f32 + 0.5),
            );
        }
        let editor = editor.clone();
        let marker = self.clone();
        window.defer(cx, move |_, cx| {
            if marker.0.borrow().target.as_ref() == Some(&(range, request)) {
                editor.update(cx, |editor, cx| editor.set_scroll_offset(offset, cx));
            }
        });
    }
}
