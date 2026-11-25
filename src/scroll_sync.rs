//! Split scroll mapping. Retains source ranges and ownership only in memory.
//! Observes the upstream editor after layout; never changes its caret or text.
use gpui::{canvas, point, prelude::*, px, App, Entity, Focusable, SharedString, Window};
use gpui_component::input::{BufferPoint, EditorState, RopeExt};
use std::{cell::RefCell, ops::Range, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    pub start: usize,
    pub fraction: f32,
    pub edge: i8,
}

#[derive(Default)]
struct State {
    enabled: bool,
    source: SharedString,
    dialect: crate::markdown::Dialect,
    blocks: Vec<Range<usize>>,
    scope: Range<usize>,
    revision: u64,
    pending: Option<Position>,
    following_preview: bool,
    settle: bool,
    last_y: Option<f32>,
}

#[derive(Clone, Default)]
pub struct ScrollSync(Rc<RefCell<State>>);

impl ScrollSync {
    #[cfg(test)]
    pub fn configure(&self, enabled: bool, source: SharedString, scope: Range<usize>) {
        self.configure_with_dialect(enabled, source, scope, crate::markdown::Dialect::default());
    }

    pub fn configure_with_dialect(
        &self,
        enabled: bool,
        source: SharedString,
        scope: Range<usize>,
        dialect: crate::markdown::Dialect,
    ) {
        let mut state = self.0.borrow_mut();
        let source_changed = state.source != source || state.dialect != dialect;
        if source_changed {
            state.blocks = crate::markdown::source_blocks_with_dialect(&source, dialect);
            state.source = source;
            state.dialect = dialect;
            state.revision = state.revision.wrapping_add(1);
            state.pending = None;
            state.following_preview = false;
            state.last_y = None;
        }
        if state.scope != scope || state.enabled != enabled {
            state.scope = scope;
            state.enabled = enabled;
            state.revision = state.revision.wrapping_add(1);
            state.pending = None;
            state.settle = false;
            // On entering Split or changing Focus, align the newly shown source
            // to the preview. Edits retain the editor as the scroll leader.
            state.following_preview = enabled && !source_changed;
            state.last_y = None;
        }
    }

    pub fn revision(&self) -> u64 {
        self.0.borrow().revision
    }

    pub fn realign(&self) {
        self.0.borrow_mut().last_y = None;
    }

    pub fn receive(&self, position: Position, revision: u64) -> bool {
        let mut state = self.0.borrow_mut();
        if !state.enabled
            || revision != state.revision
            || !position.fraction.is_finite()
            || !(0.0..=1.0).contains(&position.fraction)
            || !(-1..=1).contains(&position.edge)
            || !state.scope.contains(&position.start)
            || state
                .blocks
                .binary_search_by_key(&position.start, |block| block.start)
                .is_err()
        {
            return false;
        }
        state.following_preview = true;
        state.pending = Some(position);
        true
    }

    fn source_intent(&self) {
        let mut state = self.0.borrow_mut();
        if state.following_preview {
            state.following_preview = false;
            state.pending = None;
            state.settle = false;
            state.last_y = None;
        }
    }

    pub fn observer(
        &self,
        editor: Entity<EditorState>,
        send: impl Fn(Position, u64) + 'static,
    ) -> impl IntoElement {
        let sync = self.clone();
        let events = self.clone();
        let input = editor.clone();
        canvas(
            move |_, window, cx| sync.update(&editor, window, cx, &send),
            move |bounds, _, window, _| {
                let sync = events.clone();
                window.on_mouse_event(move |event: &gpui::ScrollEvent, phase, _, _| {
                    if phase.capture()
                        && bounds.contains(&event.position)
                        && !event.modifiers.control
                    {
                        sync.source_intent();
                    }
                });
                let sync = events.clone();
                window.on_mouse_event(move |event: &gpui::MouseDownEvent, phase, _, _| {
                    if phase.capture() && bounds.contains(&event.position) {
                        sync.source_intent();
                    }
                });
                let sync = events.clone();
                let editor = input.clone();
                window.on_key_event(move |_: &gpui::KeyDownEvent, phase, window, cx| {
                    if phase.capture() && editor.read(cx).focus_handle(cx).is_focused(window) {
                        sync.source_intent();
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    fn update(
        &self,
        editor: &Entity<EditorState>,
        window: &mut Window,
        cx: &mut App,
        send: &impl Fn(Position, u64),
    ) {
        let mut state = self.0.borrow_mut();
        if !state.enabled || state.blocks.is_empty() {
            return;
        }
        let input = editor.read(cx);
        let Some(line_height) = input
            .line_height()
            .map(|height| height.as_f32())
            .filter(|height| *height > 0.)
        else {
            return;
        };
        let viewport = input.input_bounds();
        if viewport.size.height <= px(0.) {
            return;
        }
        let offset = input.scroll_offset();
        let y = -offset.y.as_f32();
        let max_y = (input.display_map().display_row_count() as f32 * line_height
            - viewport.size.height.as_f32())
        .max(0.);
        let block_y = |offset: usize| {
            let position = input.text().offset_to_point(offset.min(input.text().len()));
            input
                .display_map()
                .buffer_pos_to_display_pos(BufferPoint::new(position.row, position.column))
                .row as f32
                * line_height
        };
        if let Some(position) = state.pending.take() {
            let index = state
                .blocks
                .binary_search_by_key(&position.start, |block| block.start)
                .unwrap();
            let start = block_y(position.start);
            let end = block_y(
                state
                    .blocks
                    .get(index + 1)
                    .map_or(state.source.len(), |block| block.start),
            );
            let target = match position.edge {
                -1 if state.scope.start == 0 => 0.,
                1 if state.scope.end == state.source.len() => max_y,
                _ => (start + (end - start) * position.fraction).clamp(0., max_y),
            };
            let revision = state.revision;
            let sync = self.clone();
            let editor = editor.clone();
            drop(state);
            window.defer(cx, move |_, cx| {
                let mut state = sync.0.borrow_mut();
                if state.enabled && state.revision == revision && state.following_preview {
                    state.settle = true;
                    drop(state);
                    editor.update(cx, |input, cx| {
                        input.set_scroll_offset(point(offset.x, px(-target)), cx)
                    });
                }
            });
            return;
        }
        if state.settle || state.following_preview {
            state.settle = false;
            state.last_y = Some(y);
            return;
        }
        if state.last_y.is_some_and(|last| (last - y).abs() < 0.5) {
            return;
        }
        state.last_y = Some(y);
        let index = state
            .blocks
            .partition_point(|block| block_y(block.start) <= y + 0.5)
            .saturating_sub(1);
        let block = &state.blocks[index];
        if !state.scope.contains(&block.start) {
            return;
        }
        let start = block_y(block.start);
        let end = block_y(
            state
                .blocks
                .get(index + 1)
                .map_or(state.source.len(), |block| block.start),
        );
        let position = Position {
            start: block.start,
            fraction: if end > start {
                ((y - start) / (end - start)).clamp(0., 1.)
            } else {
                0.
            },
            edge: if y <= 0.5 {
                -1
            } else if y >= max_y - 0.5 {
                1
            } else {
                0
            },
        };
        let revision = state.revision;
        drop(state);
        send(position, revision);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{size, Context, Render, TestAppContext};
    use gpui_component::{input::Editor, Root};

    struct Harness {
        editor: Entity<EditorState>,
        sync: ScrollSync,
        sent: Rc<RefCell<Vec<Position>>>,
    }
    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let sent = self.sent.clone();
            gpui::div()
                .relative()
                .size_full()
                .child(Editor::new(&self.editor).size_full())
                .child(self.sync.observer(self.editor.clone(), move |position, _| {
                    sent.borrow_mut().push(position)
                }))
        }
    }
    fn draw(cx: &mut gpui::VisualTestContext) {
        for _ in 0..4 {
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    fn wrapped_source_and_preview_scroll_without_echo_or_selection_changes(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let source: SharedString = (0..80)
            .map(|i| {
                format!(
                    "## Section {i}\n\n{}\n\n",
                    "中文 soft wrapped text. ".repeat(12)
                )
            })
            .collect::<String>()
            .into();
        let sync = ScrollSync::default();
        sync.configure(true, source.clone(), 0..source.len());
        let sent = Rc::new(RefCell::new(Vec::new()));
        let mut input = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let editor = cx.new(|cx| EditorState::new(window, cx).default_value(source.clone()));
            input = Some(editor.clone());
            let view = cx.new(|_| Harness {
                editor,
                sync: sync.clone(),
                sent: sent.clone(),
            });
            Root::new(view, window, cx)
        });
        let editor = input.unwrap();
        cx.simulate_resize(size(px(500.), px(600.)));
        draw(cx);
        let target = crate::markdown::source_blocks(&source)[40].start;
        cx.update(|_, cx| editor.update(cx, |input, cx| input.set_selected_range(0..5, cx)));
        assert!(sync.receive(
            Position {
                start: target,
                fraction: 0.,
                edge: 0
            },
            sync.revision()
        ));
        let before = sent.borrow().len();
        draw(cx);
        cx.update(|_, cx| {
            let input = editor.read(cx);
            let position = input.text().offset_to_point(target);
            let row = input
                .display_map()
                .buffer_pos_to_display_pos(BufferPoint::new(position.row, position.column))
                .row;
            let expected = row as f32 * input.line_height().unwrap().as_f32();
            assert!(
                (-input.scroll_offset().y.as_f32() - expected).abs() < 2.,
                "wrapped target must align to the source viewport"
            );
            assert_eq!(input.selected_range(), 0..5);
            assert_eq!(input.value().as_ref(), source.as_ref());
        });
        assert_eq!(
            sent.borrow().len(),
            before,
            "following the preview must not send a feedback scroll"
        );
        let position = cx.update(|_, cx| editor.read(cx).input_bounds().center());
        cx.simulate_event(gpui::ScrollEvent {
            position,
            delta: gpui::ScrollDelta::Pixels(point_delta()),
            modifiers: gpui::Modifiers::default(),
            touch_phase: gpui::TouchPhase::Moved,
            source: gpui::ScrollSource::Pointer,
        });
        draw(cx);
        assert!(
            sent.borrow().len() > before,
            "a source wheel must regain scroll ownership"
        );
        assert!(sent.borrow().last().unwrap().start >= target);
        let revision = sync.revision();
        sync.configure(false, source.clone(), 0..source.len());
        assert!(!sync.receive(
            Position {
                start: target,
                fraction: 0.,
                edge: 0
            },
            revision
        ));
        let count = sent.borrow().len();
        cx.update(|_, cx| {
            editor.update(cx, |input, cx| {
                input.set_scroll_offset(point(px(0.), px(-100.)), cx)
            })
        });
        draw(cx);
        assert_eq!(
            sent.borrow().len(),
            count,
            "disabled sync must not publish any position"
        );
    }
    fn point_delta() -> gpui::Point<gpui::Pixels> {
        point(px(0.), px(-120.))
    }

    #[test]
    fn rejects_stale_invalid_and_out_of_focus_scroll_messages() {
        let sync = ScrollSync::default();
        let source: SharedString = "# First\n\nbody\n\n# Second\n\n末尾".into();
        let second = source.find("# Second").unwrap();
        sync.configure(true, source.clone(), second..source.len());
        let revision = sync.revision();
        for position in [
            Position {
                start: 0,
                fraction: 0.,
                edge: 0,
            },
            Position {
                start: second,
                fraction: f32::NAN,
                edge: 0,
            },
            Position {
                start: second + 1,
                fraction: 0.,
                edge: 0,
            },
            Position {
                start: second,
                fraction: 2.,
                edge: 0,
            },
        ] {
            assert!(!sync.receive(position, revision));
        }
        assert!(sync.receive(
            Position {
                start: second,
                fraction: 0.5,
                edge: 0
            },
            revision
        ));
        sync.configure(true, "# Changed".into(), 0..9);
        assert!(!sync.receive(
            Position {
                start: 0,
                fraction: 0.,
                edge: 0
            },
            revision
        ));
    }
}
