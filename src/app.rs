use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{
    actions, div, image_cache, prelude::*, px, rgb, App, AppContext, Context, Entity,
    ExternalPaths, IntoElement, KeyBinding, PromptButton, PromptLevel, Render, ScrollWheelEvent,
    SharedString, Subscription, Window,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::text::TextView;
use gpui_component::{Selectable as _, Sizable as _, StyledExt as _, Theme};
use rfd::AsyncFileDialog;

use crate::benchmark::BenchmarkScenario;
use crate::document::Document;
use crate::image_cache::{BudgetImageCache, WARNING_THRESHOLD_BYTES};
use crate::image_loader::DocumentImageRoot;
use crate::markdown::{self, Heading, SearchHit};
use crate::zoom::ZoomLevel;

const APP_CONTEXT: &str = "NativeMarkdown";
const BASE_FONT_SIZE: f32 = 16.0;
const BASE_MONO_FONT_SIZE: f32 = 13.0;

actions!(
    native_markdown,
    [
        NewDocument,
        OpenDocument,
        SaveDocument,
        SaveDocumentAs,
        FindDocument,
        TogglePreview,
        ShowPreview,
        ShowSplit,
        ShowSource,
        ZoomIn,
        ZoomOut,
        ResetZoom,
    ]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-n", NewDocument, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-o", OpenDocument, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-s", SaveDocument, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-shift-s", SaveDocumentAs, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-f", FindDocument, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-e", TogglePreview, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-1", ShowPreview, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-2", ShowSplit, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-3", ShowSource, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-+", ZoomIn, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-=", ZoomIn, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl--", ZoomOut, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-0", ResetZoom, Some(APP_CONTEXT)),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewMode {
    Preview,
    Split,
    Source,
}

impl ViewMode {
    fn label(self) -> &'static str {
        match self {
            Self::Preview => "Preview",
            Self::Split => "Split",
            Self::Source => "Source",
        }
    }
}

struct Notice {
    text: String,
    is_error: bool,
    created_at: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DocumentAction {
    New,
    OpenDialog,
    OpenPath(PathBuf),
    Recover,
    CloseWindow,
}

pub struct NativeMarkdownApp {
    document: Document,
    editor: Entity<InputState>,
    search_input: Entity<InputState>,
    markdown: SharedString,
    outline: Vec<Heading>,
    word_count: usize,
    reading_minutes: usize,
    search_hits: Vec<SearchHit>,
    search_query: String,
    active_hit: usize,
    view_mode: ViewMode,
    outline_open: bool,
    search_open: bool,
    preview_section: Option<usize>,
    zoom: ZoomLevel,
    last_path: Option<PathBuf>,
    recovery_available: bool,
    notice: Option<Notice>,
    dialog_in_flight: bool,
    image_cache: Entity<BudgetImageCache>,
    image_root: DocumentImageRoot,
    _subscriptions: Vec<Subscription>,
}

impl NativeMarkdownApp {
    pub(crate) fn run_benchmark_step(
        &mut self,
        scenario: BenchmarkScenario,
        step: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match scenario {
            BenchmarkScenario::Idle | BenchmarkScenario::Scroll => {}
            BenchmarkScenario::ViewModes => {
                let mode = match step % 3 {
                    0 => ViewMode::Preview,
                    1 => ViewMode::Split,
                    _ => ViewMode::Source,
                };
                self.set_view_mode(mode, cx);
            }
            BenchmarkScenario::Zoom
            | BenchmarkScenario::ZoomSource
            | BenchmarkScenario::ZoomSplit => {
                match scenario {
                    BenchmarkScenario::ZoomSource => self.set_view_mode(ViewMode::Source, cx),
                    BenchmarkScenario::ZoomSplit => self.set_view_mode(ViewMode::Split, cx),
                    _ => self.set_view_mode(ViewMode::Preview, cx),
                }
                let phase = step % 30;
                let tenths = if phase <= 15 { 10 + phase } else { 40 - phase };
                self.zoom = ZoomLevel::from_factor(tenths as f32 / 10.0);
                self.apply_zoom(cx);
            }
            BenchmarkScenario::Reopen => {
                if let Some(path) = self.document.path.clone() {
                    self.open_path(path, window, cx);
                }
            }
        }
    }

    pub fn new(
        initial_path: Option<PathBuf>,
        image_root: DocumentImageRoot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (document, notice) = match initial_path.as_ref() {
            Some(path) => match Document::open(path.clone()) {
                Ok(document) => (document, None),
                Err(error) => (
                    Document::default(),
                    Some(Notice {
                        text: format!("Could not open document: {error}"),
                        is_error: true,
                        created_at: Instant::now(),
                    }),
                ),
            },
            None => (Document::default(), None),
        };
        image_root.set_document_path(document.path.as_deref());

        let editor = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor("markdown")
                .line_number(true)
                .default_value(document.content.clone())
        });
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Find in document")
                .clean_on_escape()
        });
        let image_cache = BudgetImageCache::new(cx);
        let subscriptions = vec![
            cx.subscribe_in(
                &editor,
                window,
                |this: &mut Self, editor, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.document.content = editor.read(cx).value().to_string();
                        this.refresh_analysis();
                        if let Err(error) = this.document.maybe_write_recovery() {
                            this.set_notice(format!("Recovery copy failed: {error}"), true);
                        }
                        cx.notify();
                    }
                },
            ),
            cx.subscribe_in(
                &search_input,
                window,
                |this: &mut Self, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.search_query = input.read(cx).value().to_string();
                        this.refresh_search();
                        cx.notify();
                    }
                },
            ),
        ];

        cx.spawn(async move |this, cx| loop {
            smol::Timer::after(Duration::from_secs(1)).await;
            if this
                .update(cx, |this, cx| {
                    if this.run_background_maintenance() {
                        cx.notify();
                    }
                })
                .is_err()
            {
                break;
            }
        })
        .detach();

        let markdown: SharedString = document.content.clone().into();
        let outline = markdown::headings(&document.content);
        let word_count = markdown::word_count(&document.content);
        let reading_minutes = markdown::reading_minutes(word_count);
        let last_path = document.path.clone().or(initial_path);

        Self {
            document,
            editor,
            search_input,
            markdown,
            outline,
            word_count,
            reading_minutes,
            search_hits: Vec::new(),
            search_query: String::new(),
            active_hit: 0,
            view_mode: ViewMode::Preview,
            outline_open: false,
            search_open: false,
            preview_section: None,
            zoom: ZoomLevel::default(),
            last_path,
            recovery_available: Document::recovery_exists(),
            notice,
            dialog_in_flight: false,
            image_cache,
            image_root,
            _subscriptions: subscriptions,
        }
    }

    pub fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.document.is_dirty() || self.dialog_in_flight {
            self.request_document_action(DocumentAction::CloseWindow, window, cx);
            false
        } else {
            Document::clear_recovery();
            true
        }
    }

    fn refresh_analysis(&mut self) {
        self.markdown = self.document.content.clone().into();
        self.outline = markdown::headings(&self.document.content);
        self.word_count = markdown::word_count(&self.document.content);
        self.reading_minutes = markdown::reading_minutes(self.word_count);
        self.refresh_search();
        self.preview_section = self.preview_section.filter(|section| {
            *section < markdown::sections(&self.document.content, &self.outline).len()
        });
    }

    fn run_background_maintenance(&mut self) -> bool {
        let mut visible_change = false;
        if let Err(error) = self.document.maybe_write_recovery() {
            self.set_notice(format!("Recovery copy failed: {error}"), true);
            visible_change = true;
        }

        let recovery_available = Document::recovery_exists();
        if self.recovery_available != recovery_available {
            self.recovery_available = recovery_available;
            visible_change = true;
        }

        if self
            .notice
            .as_ref()
            .is_some_and(|notice| notice.created_at.elapsed() > Duration::from_secs(5))
        {
            self.notice = None;
            visible_change = true;
        }
        visible_change
    }

    fn refresh_search(&mut self) {
        self.search_hits =
            markdown::search(&self.document.content, &self.search_query, &self.outline);
        self.active_hit = self
            .active_hit
            .min(self.search_hits.len().saturating_sub(1));
    }

    fn replace_document(
        &mut self,
        document: Document,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.document = document;
        self.last_path.clone_from(&self.document.path);
        self.image_root
            .set_document_path(self.document.path.as_deref());
        self.image_cache
            .update(cx, |cache, cx| cache.clear(window, cx));
        self.preview_section = None;
        self.search_open = false;
        self.outline_open = false;
        self.view_mode = ViewMode::Preview;
        self.editor.update(cx, |editor, cx| {
            editor.set_value(self.document.content.clone(), window, cx)
        });
        self.refresh_analysis();
        self.recovery_available = Document::recovery_exists();
        cx.notify();
    }

    /// The only entry point for actions that can replace the current document.
    ///
    /// GPUI owns the application state through a `RefCell`. A blocking native dialog would run a
    /// nested Windows message loop while that state is mutably borrowed, allowing cursor and timer
    /// tasks to re-enter GPUI and panic. Keep every prompt launched from this workflow asynchronous.
    fn request_document_action(
        &mut self,
        action: DocumentAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dialog_in_flight {
            return;
        }
        if !self.document.is_dirty() {
            self.perform_document_action(action, window, cx);
            return;
        }

        self.dialog_in_flight = true;
        let answer = window.prompt(
            PromptLevel::Warning,
            "Unsaved changes",
            Some("Keep the changes to this document?"),
            &[
                PromptButton::ok("Save"),
                PromptButton::new("Don't Save"),
                PromptButton::cancel("Cancel"),
            ],
            cx,
        );
        let app = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let answer = answer.await.ok();
                cx.update(|window, cx| {
                    app.update(cx, |app, cx| {
                        app.dialog_in_flight = false;
                        match answer {
                            Some(0) => app.save_before(action, window, cx),
                            Some(1) => app.perform_document_action(action, window, cx),
                            _ => cx.notify(),
                        }
                    })
                    .ok();
                })
                .ok();
            })
            .detach();
    }

    fn perform_document_action(
        &mut self,
        action: DocumentAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            DocumentAction::New => {
                Document::clear_recovery();
                self.replace_document(Document::new_document(), window, cx);
                self.view_mode = ViewMode::Split;
                self.set_notice("New document", false);
            }
            DocumentAction::OpenDialog => self.open_dialog(window, cx),
            DocumentAction::OpenPath(path) => self.open_path(path, window, cx),
            DocumentAction::Recover => self.recover(window, cx),
            DocumentAction::CloseWindow => {
                Document::clear_recovery();
                window.remove_window();
            }
        }
    }

    fn open_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog_in_flight {
            return;
        }
        self.dialog_in_flight = true;
        // `AsyncFileDialog` runs the Windows dialog without retaining GPUI's application borrow.
        let selection = AsyncFileDialog::new()
            .add_filter("Markdown", &["md", "markdown", "mdown", "mkd"])
            .add_filter("Text", &["txt"])
            .pick_file();
        let app = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let path = selection.await.map(|file| file.path().to_path_buf());
                cx.update(|window, cx| {
                    app.update(cx, |app, cx| {
                        app.dialog_in_flight = false;
                        if let Some(path) = path {
                            app.open_path(path, window, cx);
                        } else {
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .ok();
            })
            .detach();
    }

    fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        match Document::open(path) {
            Ok(document) => {
                Document::clear_recovery();
                self.replace_document(document, window, cx);
                self.set_notice("Document opened", false);
            }
            Err(error) => self.set_notice(format!("Could not open document: {error}"), true),
        }
    }

    fn recover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match Document::recover() {
            Ok(document) => {
                self.replace_document(document, window, cx);
                self.view_mode = ViewMode::Split;
                self.recovery_available = false;
                self.set_notice("Recovered unsaved draft", false);
            }
            Err(error) => self.set_notice(format!("Could not recover draft: {error}"), true),
        }
    }

    fn save_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog_in_flight {
            return;
        }
        if self.document.path.is_none() {
            self.save_as(None, window, cx);
            return;
        }
        self.save_existing();
        cx.notify();
    }

    fn save_existing(&mut self) -> bool {
        match self.document.save() {
            Ok(()) => {
                self.last_path.clone_from(&self.document.path);
                self.image_root
                    .set_document_path(self.document.path.as_deref());
                self.set_notice("Saved", false);
                true
            }
            Err(error) => {
                self.set_notice(format!("Could not save document: {error}"), true);
                false
            }
        }
    }

    fn save_before(&mut self, action: DocumentAction, window: &mut Window, cx: &mut Context<Self>) {
        if self.document.path.is_some() {
            if self.save_existing() {
                self.perform_document_action(action, window, cx);
            } else {
                cx.notify();
            }
        } else {
            self.save_as(Some(action), window, cx);
        }
    }

    fn save_as(
        &mut self,
        continuation: Option<DocumentAction>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dialog_in_flight {
            return;
        }
        let mut dialog = AsyncFileDialog::new()
            .add_filter("Markdown", &["md", "markdown"])
            .set_file_name(self.document.display_name());
        if let Some(parent) = self.document.path.as_deref().and_then(Path::parent) {
            dialog = dialog.set_directory(parent);
        }
        self.dialog_in_flight = true;
        // Saving must use the same non-blocking boundary as opening and confirmation prompts.
        let selection = dialog.save_file();
        let app = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let path = selection.await.map(|file| file.path().to_path_buf());
                cx.update(|window, cx| {
                    app.update(cx, |app, cx| {
                        app.dialog_in_flight = false;
                        let Some(mut path) = path else {
                            cx.notify();
                            return;
                        };
                        if path.extension().is_none() {
                            path.set_extension("md");
                        }
                        match app.document.save_as(path.clone()) {
                            Ok(()) => {
                                app.last_path = Some(path);
                                app.image_root
                                    .set_document_path(app.document.path.as_deref());
                                app.set_notice("Saved", false);
                                if let Some(action) = continuation {
                                    app.perform_document_action(action, window, cx);
                                } else {
                                    cx.notify();
                                }
                            }
                            Err(error) => {
                                app.set_notice(format!("Could not save document: {error}"), true);
                                cx.notify();
                            }
                        }
                    })
                    .ok();
                })
                .ok();
            })
            .detach();
    }

    fn set_notice(&mut self, text: impl Into<String>, is_error: bool) {
        self.notice = Some(Notice {
            text: text.into(),
            is_error,
            created_at: Instant::now(),
        });
    }

    fn show_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = true;
        self.search_input.update(cx, |input, cx| {
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn next_hit(&mut self, cx: &mut Context<Self>) {
        if !self.search_hits.is_empty() {
            self.active_hit = (self.active_hit + 1) % self.search_hits.len();
            self.preview_section = Some(self.search_hits[self.active_hit].section_index);
            self.view_mode = ViewMode::Preview;
            cx.notify();
        }
    }

    fn previous_hit(&mut self, cx: &mut Context<Self>) {
        if !self.search_hits.is_empty() {
            self.active_hit = if self.active_hit == 0 {
                self.search_hits.len() - 1
            } else {
                self.active_hit - 1
            };
            self.preview_section = Some(self.search_hits[self.active_hit].section_index);
            self.view_mode = ViewMode::Preview;
            cx.notify();
        }
    }

    fn apply_zoom(&self, cx: &mut Context<Self>) {
        let factor = self.zoom.factor();
        let theme = Theme::global_mut(cx);
        theme.font_size = px(BASE_FONT_SIZE * factor);
        theme.mono_font_size = px(BASE_MONO_FONT_SIZE * factor);
        cx.notify();
    }

    fn zoom_in(&mut self, cx: &mut Context<Self>) {
        if self.zoom.zoom_in() {
            self.apply_zoom(cx);
        }
    }

    fn zoom_out(&mut self, cx: &mut Context<Self>) {
        if self.zoom.zoom_out() {
            self.apply_zoom(cx);
        }
    }

    fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        if self.zoom.reset() {
            self.apply_zoom(cx);
        }
    }

    fn handle_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.modifiers.control {
            return;
        }

        let delta_y: f32 = event.delta.pixel_delta(px(20.0)).y.into();
        let factor = (delta_y * 0.0025).exp();
        if self.zoom.apply_gesture(factor) {
            self.apply_zoom(cx);
        }
        cx.stop_propagation();
    }

    fn set_view_mode(&mut self, mode: ViewMode, cx: &mut Context<Self>) {
        self.view_mode = mode;
        cx.notify();
    }

    fn on_new(&mut self, _: &NewDocument, window: &mut Window, cx: &mut Context<Self>) {
        self.request_document_action(DocumentAction::New, window, cx);
    }

    fn on_open(&mut self, _: &OpenDocument, window: &mut Window, cx: &mut Context<Self>) {
        self.request_document_action(DocumentAction::OpenDialog, window, cx);
    }

    fn on_save(&mut self, _: &SaveDocument, window: &mut Window, cx: &mut Context<Self>) {
        self.save_current(window, cx);
    }

    fn on_save_as(&mut self, _: &SaveDocumentAs, window: &mut Window, cx: &mut Context<Self>) {
        self.save_as(None, window, cx);
    }

    fn on_find(&mut self, _: &FindDocument, window: &mut Window, cx: &mut Context<Self>) {
        self.show_search(window, cx);
    }

    fn on_toggle_preview(&mut self, _: &TogglePreview, _: &mut Window, cx: &mut Context<Self>) {
        let mode = if self.view_mode == ViewMode::Preview {
            ViewMode::Split
        } else {
            ViewMode::Preview
        };
        self.set_view_mode(mode, cx);
    }

    fn toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let dirty = if self.document.is_dirty() { " •" } else { "" };
        div()
            .h_flex()
            .h(px(72.0))
            .flex_none()
            .items_center()
            .gap_2()
            .px_4()
            .border_b_1()
            .border_color(rgb(0xd3c8b5))
            .bg(rgb(0xf6f1e7))
            .child(
                div()
                    .mr_3()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x96692b))
                            .child("NATIVE / MARKDOWN"),
                    )
                    .child(
                        div()
                            .text_lg()
                            .font_semibold()
                            .child(format!("{}{dirty}", self.document.display_name())),
                    ),
            )
            .child(
                Button::new("new-document")
                    .label("New")
                    .small()
                    .ghost()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.request_document_action(DocumentAction::New, window, cx)
                    })),
            )
            .child(
                Button::new("open-document")
                    .label("Open")
                    .small()
                    .ghost()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.request_document_action(DocumentAction::OpenDialog, window, cx)
                    })),
            )
            .child(
                Button::new("save-document")
                    .label("Save")
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| this.save_current(window, cx))),
            )
            .child(div().w(px(1.0)).h(px(24.0)).mx_2().bg(rgb(0xd3c8b5)))
            .child(
                Button::new("preview-mode")
                    .label("Preview")
                    .small()
                    .selected(self.view_mode == ViewMode::Preview)
                    .on_click(
                        cx.listener(|this, _, _, cx| this.set_view_mode(ViewMode::Preview, cx)),
                    ),
            )
            .child(
                Button::new("split-mode")
                    .label("Split")
                    .small()
                    .selected(self.view_mode == ViewMode::Split)
                    .on_click(
                        cx.listener(|this, _, _, cx| this.set_view_mode(ViewMode::Split, cx)),
                    ),
            )
            .child(
                Button::new("source-mode")
                    .label("Source")
                    .small()
                    .selected(self.view_mode == ViewMode::Source)
                    .on_click(
                        cx.listener(|this, _, _, cx| this.set_view_mode(ViewMode::Source, cx)),
                    ),
            )
            .child(div().flex_1())
            .child(
                Button::new("toggle-outline")
                    .label("Outline")
                    .small()
                    .selected(self.outline_open)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.outline_open = !this.outline_open;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("find-document")
                    .label("Find")
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| this.show_search(window, cx))),
            )
            .child(div().w(px(1.0)).h(px(24.0)).mx_2().bg(rgb(0xd3c8b5)))
            .child(
                Button::new("zoom-out")
                    .label("−")
                    .small()
                    .ghost()
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_out(cx))),
            )
            .child(
                Button::new("reset-zoom")
                    .label(format!("{}%", self.zoom.percent()))
                    .small()
                    .ghost()
                    .on_click(cx.listener(|this, _, _, cx| this.reset_zoom(cx))),
            )
            .child(
                Button::new("zoom-in")
                    .label("+")
                    .small()
                    .ghost()
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_in(cx))),
            )
    }

    fn search_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.search_hits.len();
        let current = if count == 0 { 0 } else { self.active_hit + 1 };
        let snippet = self
            .search_hits
            .get(self.active_hit)
            .map(|hit| hit.snippet.clone())
            .unwrap_or_default();
        div()
            .h_flex()
            .flex_none()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(rgb(0xd3c8b5))
            .bg(rgb(0xebe3d4))
            .child(div().w(px(320.0)).child(Input::new(&self.search_input)))
            .child(format!("{current} / {count}"))
            .child(
                div()
                    .max_w(px(360.0))
                    .truncate()
                    .text_color(rgb(0x665b4d))
                    .child(snippet),
            )
            .child(
                Button::new("previous-hit")
                    .label("Previous")
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.previous_hit(cx))),
            )
            .child(
                Button::new("next-hit")
                    .label("Next")
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.next_hit(cx))),
            )
            .child(div().flex_1())
            .child(
                Button::new("close-search")
                    .label("Close")
                    .small()
                    .ghost()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.search_open = false;
                        cx.notify();
                    })),
            )
    }

    fn outline_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let sections = markdown::sections(&self.document.content, &self.outline);
        let mut panel = div()
            .v_flex()
            .w(px(270.0))
            .min_w(px(220.0))
            .h_full()
            .flex_none()
            .overflow_y_scrollbar()
            .p_3()
            .gap_1()
            .border_r_1()
            .border_color(rgb(0xd3c8b5))
            .bg(rgb(0xebe3d4))
            .child(
                Button::new("show-full-document")
                    .label("Full document")
                    .small()
                    .selected(self.preview_section.is_none())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.preview_section = None;
                        this.view_mode = ViewMode::Preview;
                        cx.notify();
                    })),
            );

        for (heading_index, heading) in self.outline.iter().enumerate() {
            let section_index = sections
                .iter()
                .position(|section| section.heading_index == Some(heading_index));
            let label = format!(
                "{}{}",
                "  ".repeat(heading.level.saturating_sub(1) as usize),
                heading.title
            );
            panel = panel.child(
                Button::new(("outline-heading", heading_index))
                    .label(label)
                    .small()
                    .ghost()
                    .selected(section_index == self.preview_section)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.preview_section = section_index;
                        this.view_mode = ViewMode::Preview;
                        cx.notify();
                    })),
            );
        }
        panel
    }

    fn preview_source(&self) -> SharedString {
        let Some(section_index) = self.preview_section else {
            return self.markdown.clone();
        };
        markdown::sections(&self.document.content, &self.outline)
            .get(section_index)
            .map(|section| {
                self.document.content[section.range.clone()]
                    .to_owned()
                    .into()
            })
            .unwrap_or_else(|| self.markdown.clone())
    }

    fn preview_panel(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .debug_selector(|| "preview-panel".into())
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(rgb(0xfaf7f0))
            .child(
                image_cache(self.image_cache.clone()).size_full().child(
                    div().size_full().px_5().child(
                        TextView::markdown(
                            "native-markdown-preview",
                            self.preview_source(),
                            window,
                            cx,
                        )
                        .selectable(true)
                        .scrollable(true),
                    ),
                ),
            )
    }

    fn editor_panel(&self) -> impl IntoElement {
        div()
            .debug_selector(|| "editor-panel".into())
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(rgb(0xeee9df))
            .child(Input::new(&self.editor).h_full().appearance(false))
    }

    fn welcome(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .bg(rgb(0xfaf7f0))
            .child(
                div()
                    .text_3xl()
                    .font_semibold()
                    .child("Read Markdown without the machinery."),
            )
            .child(
                div()
                    .text_color(rgb(0x70685b))
                    .child("A quiet native reader powered by GPUI Component."),
            )
            .child(
                Button::new("welcome-open")
                    .label("Open document")
                    .primary()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.request_document_action(DocumentAction::OpenDialog, window, cx)
                    })),
            )
            .when_some(
                self.last_path.clone().filter(|path| path.is_file()),
                |view, path| {
                    let name = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("last document")
                        .to_owned();
                    view.child(
                        Button::new("welcome-reopen")
                            .label(format!("Reopen {name}"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.request_document_action(
                                    DocumentAction::OpenPath(path.clone()),
                                    window,
                                    cx,
                                );
                            })),
                    )
                },
            )
            .when(self.recovery_available, |view| {
                view.child(
                    Button::new("welcome-recover")
                        .label("Recover unsaved draft")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.request_document_action(DocumentAction::Recover, window, cx)
                        })),
                )
            })
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0x70685b))
                    .child("Drop a .md file anywhere · Ctrl+O to open"),
            )
    }

    fn workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.document.is_empty() {
            return self.welcome(cx).into_any_element();
        }

        let content = match self.view_mode {
            ViewMode::Preview => self.preview_panel(window, cx).into_any_element(),
            ViewMode::Source => self.editor_panel().into_any_element(),
            ViewMode::Split => div()
                .h_flex()
                .size_full()
                .child(self.editor_panel())
                .child(div().w(px(1.0)).h_full().bg(rgb(0xd3c8b5)))
                .child(self.preview_panel(window, cx))
                .into_any_element(),
        };

        div()
            .debug_selector(|| "document-workspace".into())
            .h_flex()
            .flex_1()
            .min_h_0()
            .when(self.outline_open, |view| view.child(self.outline_panel(cx)))
            .child(content)
            .into_any_element()
    }

    fn status_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        let image_cache = self.image_cache.read(cx).status();
        let image_cache_mib = image_cache.estimated_bytes as f64 / 1024.0 / 1024.0;
        let image_status = if image_cache.over_warning_threshold {
            format!(
                " · image cache {image_cache_mib:.1} MiB (above {} MiB; retained for smooth scrolling)",
                WARNING_THRESHOLD_BYTES / 1024 / 1024
            )
        } else if image_cache.estimated_bytes > 0 {
            format!(" · image cache {image_cache_mib:.1} MiB")
        } else {
            String::new()
        };
        let left = self.notice.as_ref().map_or_else(
            || {
                if self.document.is_dirty() {
                    "Unsaved changes · recovery on".to_owned()
                } else {
                    "Saved".to_owned()
                }
            },
            |notice| notice.text.clone(),
        );
        let left_color = if self.notice.as_ref().is_some_and(|notice| notice.is_error) {
            rgb(0x9b392a)
        } else {
            rgb(0x70685b)
        };

        div()
            .h_flex()
            .h(px(30.0))
            .flex_none()
            .items_center()
            .justify_between()
            .px_4()
            .border_t_1()
            .border_color(rgb(0xd3c8b5))
            .bg(rgb(0xebe3d4))
            .text_sm()
            .text_color(rgb(0x70685b))
            .child(div().text_color(left_color).child(left))
            .child(format!(
                "{} words · {} min read · {} · {}% · {} local images{image_status}",
                self.word_count,
                self.reading_minutes,
                self.view_mode.label(),
                self.zoom.percent(),
                self.image_root.load_count(),
            ))
    }

    fn handle_drop(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = paths.paths().iter().find(|path| path.is_file()).cloned() {
            self.request_document_action(DocumentAction::OpenPath(path), window, cx);
        }
    }
}

impl Render for NativeMarkdownApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let marker = if self.document.is_dirty() { " •" } else { "" };
        window.set_window_title(&format!(
            "{}{marker} — Native Markdown",
            self.document.display_name()
        ));

        div()
            .key_context(APP_CONTEXT)
            .on_scroll_wheel(cx.listener(Self::handle_scroll_wheel))
            .on_action(cx.listener(Self::on_new))
            .on_action(cx.listener(Self::on_open))
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_save_as))
            .on_action(cx.listener(Self::on_find))
            .on_action(cx.listener(Self::on_toggle_preview))
            .on_action(
                cx.listener(|this, _: &ShowPreview, _, cx| {
                    this.set_view_mode(ViewMode::Preview, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &ShowSplit, _, cx| this.set_view_mode(ViewMode::Split, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ShowSource, _, cx| this.set_view_mode(ViewMode::Source, cx)),
            )
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.zoom_in(cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.zoom_out(cx)))
            .on_action(cx.listener(|this, _: &ResetZoom, _, cx| this.reset_zoom(cx)))
            .on_drop(cx.listener(Self::handle_drop))
            .v_flex()
            .size_full()
            .overflow_hidden()
            .bg(rgb(0xf6f1e7))
            .text_color(rgb(0x292723))
            .child(self.toolbar(cx))
            .when(self.search_open, |view| view.child(self.search_bar(cx)))
            .child(self.workspace(window, cx))
            .child(self.status_bar(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{size, TestAppContext};

    #[gpui::test]
    fn unsaved_document_action_does_not_block_gpui_tasks(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let image_root = DocumentImageRoot::default();
        let (app, cx) =
            cx.add_window_view(|window, cx| NativeMarkdownApp::new(None, image_root, window, cx));

        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.document.content = "changed while editing".to_owned();
                app.view_mode = ViewMode::Split;
                app.request_document_action(DocumentAction::OpenDialog, window, cx);
            });
        });

        assert!(cx.has_pending_prompt());
        cx.executor().advance_clock(Duration::from_millis(1_100));
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());

        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        app.read_with(cx, |app, _| {
            assert_eq!(app.document.content, "changed while editing");
            assert!(!app.dialog_in_flight);
        });
    }

    #[gpui::test]
    fn discarding_changes_continues_the_requested_action(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let image_root = DocumentImageRoot::default();
        let (app, cx) =
            cx.add_window_view(|window, cx| NativeMarkdownApp::new(None, image_root, window, cx));

        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.document.content = "discard me".to_owned();
                app.request_document_action(DocumentAction::New, window, cx);
            });
        });
        cx.simulate_prompt_answer("Don't Save");
        cx.run_until_parked();

        app.read_with(cx, |app, _| {
            assert_eq!(app.document.content, "# Untitled\n\n");
            assert_eq!(app.view_mode, ViewMode::Split);
            assert!(!app.dialog_in_flight);
        });
    }

    #[gpui::test]
    fn document_panels_fill_the_workspace(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("layout.md");
        std::fs::write(&path, "# Layout\n\nFirst line.\n\nSecond line.").unwrap();
        let image_root = DocumentImageRoot::default();

        let (app, cx) = cx.add_window_view(|window, cx| {
            let mut app = NativeMarkdownApp::new(Some(path), image_root, window, cx);
            app.view_mode = ViewMode::Split;
            app
        });
        cx.simulate_resize(size(px(1180.0), px(780.0)));
        app.update(cx, |_, cx| cx.notify());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let workspace = cx.debug_bounds("document-workspace").unwrap();
        let editor = cx.debug_bounds("editor-panel").unwrap();
        let preview = cx.debug_bounds("preview-panel").unwrap();
        assert!(
            workspace.size.height > px(500.0),
            "workspace height: {:?}",
            workspace.size.height
        );
        assert!(
            editor.size.height > workspace.size.height * 0.8
                && preview.size.height > workspace.size.height * 0.8,
            "editor height {:?}, preview height {:?}, workspace height {:?}",
            editor.size.height,
            preview.size.height,
            workspace.size.height
        );
    }

    #[test]
    fn view_mode_labels_are_stable() {
        assert_eq!(ViewMode::Preview.label(), "Preview");
        assert_eq!(ViewMode::Split.label(), "Split");
        assert_eq!(ViewMode::Source.label(), "Source");
    }

    #[gpui::test]
    fn background_maintenance_only_redraws_for_visible_changes(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let image_root = DocumentImageRoot::default();
        let (app, cx) =
            cx.add_window_view(|window, cx| NativeMarkdownApp::new(None, image_root, window, cx));

        app.update(cx, |app, _| {
            assert!(!app.run_background_maintenance());
            app.notice = Some(Notice {
                text: "Expired".to_owned(),
                is_error: false,
                created_at: Instant::now() - Duration::from_secs(6),
            });
            assert!(app.run_background_maintenance());
            assert!(app.notice.is_none());
        });
    }

    #[gpui::test]
    fn reading_metadata_is_cached_with_document_analysis(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let image_root = DocumentImageRoot::default();
        let (app, cx) =
            cx.add_window_view(|window, cx| NativeMarkdownApp::new(None, image_root, window, cx));

        app.update(cx, |app, _| {
            app.document.content = "# Title\n\none two three".to_owned();
            app.refresh_analysis();
            assert_eq!(app.word_count, 4);
            assert_eq!(app.reading_minutes, 1);

            app.document.content = "# Title\n\none two three four five".to_owned();
            app.refresh_analysis();
            assert_eq!(app.word_count, 6);
            assert_eq!(app.reading_minutes, 1);
        });
    }
}
