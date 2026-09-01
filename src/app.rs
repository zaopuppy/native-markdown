use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{
    actions, div, image_cache, img, prelude::*, px, relative, rgb, AnyElement, App, AppContext,
    Context, Entity, ExternalPaths, FocusHandle, ImgResourceLoader, IntoElement, KeyBinding,
    ObjectFit, PromptButton, PromptLevel, Render, Resource, ScrollWheelEvent, SharedString,
    Subscription, WeakEntity, Window,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState, Position};
use gpui_component::menu::ContextMenuExt as _;
use gpui_component::resizable::{h_resizable, resizable_panel, ResizableState};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::text::TextView;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Disableable as _, Selectable as _, Sizable as _, StyledExt as _, Theme};
use rfd::AsyncFileDialog;

use crate::benchmark::BenchmarkScenario;
use crate::document::Document;
use crate::file_tree::{self, EntryKind, FileTree, VisibleRow};
use crate::image_cache::{BudgetImageCache, WARNING_THRESHOLD_BYTES};
use crate::image_loader::DocumentImageRoot;
use crate::layout_settings::LayoutSettings;
use crate::markdown::{self, Heading, SearchHit};
use crate::mermaid::{self, MermaidManager, MermaidPreview, MermaidPreviewStatus, OPEN_TIMEOUT};
use crate::zoom::ZoomLevel;

const APP_CONTEXT: &str = "NativeMarkdown";
const FILE_TREE_CONTEXT: &str = "NativeMarkdownFileTree";
const BASE_FONT_SIZE: f32 = 16.0;
const BASE_MONO_FONT_SIZE: f32 = 13.0;
const FILE_TREE_HIDE_BELOW: f32 = 760.0;
const OUTLINE_HIDE_BELOW: f32 = 1000.0;

fn state_button(button: Button, selected: bool) -> Button {
    button
        .selected(selected)
        .border_1()
        .when(selected, |button| {
            button
                .bg(rgb(0x81531d))
                .border_color(rgb(0x5f3a10))
                .text_color(rgb(0xfffbf3))
                .shadow_sm()
        })
        .when(!selected, |button| {
            button
                .bg(rgb(0xf6f1e7))
                .border_color(rgb(0xbbaa8e))
                .text_color(rgb(0x4f473d))
        })
}

fn outline_indent(level: u8) -> f32 {
    8.0 + level.saturating_sub(1) as f32 * 14.0
}

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
        FileTreeUp,
        FileTreeDown,
        FileTreeLeft,
        FileTreeRight,
        FileTreeOpen,
        OpenTreeContext,
        SetTreeRootContext,
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
        KeyBinding::new("up", FileTreeUp, Some(FILE_TREE_CONTEXT)),
        KeyBinding::new("down", FileTreeDown, Some(FILE_TREE_CONTEXT)),
        KeyBinding::new("left", FileTreeLeft, Some(FILE_TREE_CONTEXT)),
        KeyBinding::new("right", FileTreeRight, Some(FILE_TREE_CONTEXT)),
        KeyBinding::new("enter", FileTreeOpen, Some(FILE_TREE_CONTEXT)),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutlineMode {
    Jump,
    Focus,
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
    OpenTreePath(PathBuf),
    Recover,
    CloseWindow,
}

pub struct NativeMarkdownApp {
    document: Document,
    editor: Entity<InputState>,
    search_input: Entity<InputState>,
    outline: Vec<Heading>,
    word_count: usize,
    reading_minutes: usize,
    search_hits: Vec<SearchHit>,
    search_query: String,
    active_hit: usize,
    view_mode: ViewMode,
    file_tree: FileTree,
    file_tree_focus: FocusHandle,
    tree_context_path: Option<PathBuf>,
    file_tree_open: bool,
    outline_open: bool,
    file_tree_width: f32,
    outline_width: f32,
    file_tree_narrow_reveal: bool,
    outline_narrow_reveal: bool,
    workspace_resizable: Entity<ResizableState>,
    search_open: bool,
    preview_markdown: SharedString,
    focused_section: Option<usize>,
    selected_heading: Option<usize>,
    outline_mode: OutlineMode,
    outline_jump_request: u64,
    zoom: ZoomLevel,
    last_path: Option<PathBuf>,
    recovery_available: bool,
    notice: Option<Notice>,
    dialog_in_flight: bool,
    image_cache: Entity<BudgetImageCache>,
    image_root: DocumentImageRoot,
    mermaid: MermaidManager,
    _subscriptions: Vec<Subscription>,
}

impl NativeMarkdownApp {
    pub(crate) fn mermaid_benchmark_status(&self) -> (usize, usize, usize, usize) {
        (
            self.mermaid.block_count(),
            self.mermaid.ready_count(),
            self.mermaid.pending_count(),
            self.mermaid.error_count(),
        )
    }

    pub(crate) fn run_benchmark_step(
        &mut self,
        scenario: BenchmarkScenario,
        step: u64,
        switch_step: u64,
        secondary_document: Option<&Path>,
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
                self.apply_zoom(window, cx);
            }
            BenchmarkScenario::Reopen => {
                if let Some(path) = self.document.path.clone() {
                    self.open_path(path, window, cx);
                }
            }
            BenchmarkScenario::ImageRelease if step == switch_step => {
                let images_loaded = self.image_root.load_count();
                let estimated_image_mib =
                    self.image_cache.read(cx).status().estimated_bytes as f64 / (1024.0 * 1024.0);
                if let Some(path) = secondary_document {
                    self.open_path(path.to_path_buf(), window, cx);
                    println!(
                        "NATIVE_MARKDOWN_BENCHMARK event=document_switched images_loaded={images_loaded} estimated_image_mib={estimated_image_mib:.2}"
                    );
                }
            }
            BenchmarkScenario::ImageRelease => {}
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
        let file_tree_focus = cx.focus_handle().tab_stop(true);
        #[cfg(not(test))]
        let layout = LayoutSettings::load();
        #[cfg(test)]
        let layout = LayoutSettings::default();
        let tree_root = document
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf);
        let workspace_resizable = cx.new(|_| ResizableState::default());
        let image_cache = BudgetImageCache::new(cx);
        let subscriptions = vec![
            cx.subscribe_in(
                &editor,
                window,
                |this: &mut Self, editor, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.document.content = editor.read(cx).value().to_string();
                        this.refresh_analysis();
                        this.refresh_mermaid(mermaid::EDIT_TIMEOUT, window, cx);
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
            cx.observe_window_activation(window, |this: &mut Self, window, cx| {
                if window.is_window_active() {
                    this.refresh_file_tree(window, cx);
                }
            }),
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

        let outline = markdown::headings(&document.content);
        let word_count = markdown::word_count(&document.content);
        let reading_minutes = markdown::reading_minutes(word_count);
        let last_path = document.path.clone().or(initial_path);
        let preview_markdown = SharedString::from(document.content.clone());

        let mut app = Self {
            document,
            editor,
            search_input,
            outline,
            word_count,
            reading_minutes,
            search_hits: Vec::new(),
            search_query: String::new(),
            active_hit: 0,
            view_mode: ViewMode::Preview,
            file_tree: FileTree::new(tree_root),
            file_tree_focus,
            tree_context_path: None,
            file_tree_open: layout.file_tree_open,
            outline_open: layout.outline_open,
            file_tree_width: layout.file_tree_width,
            outline_width: layout.outline_width,
            file_tree_narrow_reveal: false,
            outline_narrow_reveal: false,
            workspace_resizable,
            search_open: false,
            preview_markdown,
            focused_section: None,
            selected_heading: None,
            outline_mode: OutlineMode::Focus,
            outline_jump_request: 0,
            zoom: ZoomLevel::default(),
            last_path,
            recovery_available: Document::recovery_exists(),
            notice,
            dialog_in_flight: false,
            image_cache,
            image_root,
            mermaid: MermaidManager::new(),
            _subscriptions: subscriptions,
        };
        if let Some(root) = app.file_tree.root().map(Path::to_path_buf) {
            app.load_tree_directory(root, window, cx);
        }
        app.refresh_mermaid(OPEN_TIMEOUT, window, cx);
        app
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
        self.preview_markdown = SharedString::from(self.document.content.clone());
        self.outline = markdown::headings(&self.document.content);
        self.word_count = markdown::word_count(&self.document.content);
        self.reading_minutes = markdown::reading_minutes(self.word_count);
        self.refresh_search();
        self.focused_section = self.focused_section.filter(|section| {
            *section < markdown::sections(&self.document.content, &self.outline).len()
        });
        self.selected_heading = self
            .selected_heading
            .filter(|heading| *heading < self.outline.len());
    }

    fn persist_layout(&self) {
        let settings = LayoutSettings {
            file_tree_open: self.file_tree_open,
            outline_open: self.outline_open,
            file_tree_width: self.file_tree_width,
            outline_width: self.outline_width,
        };
        let _ = settings.save();
    }

    fn set_file_tree_root(
        &mut self,
        root: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.file_tree.set_root(root);
        if let Some(root) = self.file_tree.root().map(Path::to_path_buf) {
            self.load_tree_directory(root, window, cx);
        }
        cx.notify();
    }

    fn reset_tree_to_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self
            .document
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf);
        self.set_file_tree_root(root, window, cx);
    }

    fn tree_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let parent = self
            .file_tree
            .root()
            .and_then(Path::parent)
            .map(Path::to_path_buf);
        if let Some(parent) = parent {
            self.set_file_tree_root(Some(parent), window, cx);
        }
    }

    fn choose_tree_root(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog_in_flight {
            return;
        }
        let mut dialog = AsyncFileDialog::new();
        if let Some(root) = self.file_tree.root() {
            dialog = dialog.set_directory(root);
        }
        self.dialog_in_flight = true;
        let selection = dialog.pick_folder();
        let app = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let path = selection.await.map(|folder| folder.path().to_path_buf());
                cx.update(|window, cx| {
                    app.update(cx, |app, cx| {
                        app.dialog_in_flight = false;
                        if let Some(path) = path {
                            app.set_file_tree_root(Some(path), window, cx);
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

    fn refresh_file_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self.file_tree.refresh_paths();
        for path in paths {
            self.load_tree_directory(path, window, cx);
        }
    }

    fn load_tree_directory(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let request = self.file_tree.begin_load(&path);
        let show_hidden = self.file_tree.show_hidden();
        let scan_path = path.clone();
        let scan = smol::unblock(move || file_tree::scan_directory(&scan_path, show_hidden));
        let app = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let result = scan.await;
                cx.update(|_, cx| {
                    app.update(cx, |app, cx| {
                        if app.file_tree.finish_load(&path, request, result) {
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .ok();
            })
            .detach();
    }

    fn toggle_tree_directory(
        &mut self,
        path: PathBuf,
        traversable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.file_tree.set_selected(Some(path.clone()));
        if !traversable {
            self.set_notice("Linked directories are not expanded", true);
            cx.notify();
            return;
        }
        if let Some(path) = self.file_tree.toggle_directory(&path) {
            self.load_tree_directory(path, window, cx);
        } else {
            cx.notify();
        }
    }

    fn move_tree_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let paths = self.file_tree.selectable_paths();
        if paths.is_empty() {
            return;
        }
        let current = self
            .file_tree
            .selected()
            .and_then(|selected| paths.iter().position(|path| path == selected));
        let next = match (current, delta.is_negative()) {
            (Some(index), true) => index.saturating_sub(delta.unsigned_abs()),
            (Some(index), false) => (index + delta as usize).min(paths.len() - 1),
            (None, true) => paths.len() - 1,
            (None, false) => 0,
        };
        self.file_tree.set_selected(Some(paths[next].clone()));
        cx.notify();
    }

    fn open_selected_tree_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected) = self.file_tree.selected().map(Path::to_path_buf) else {
            return;
        };
        let row =
            self.file_tree.visible_rows().into_iter().find(
                |row| matches!(row, VisibleRow::Entry { entry, .. } if entry.path == selected),
            );
        match row {
            Some(VisibleRow::Entry {
                entry:
                    file_tree::TreeEntry {
                        kind: EntryKind::Markdown,
                        ..
                    },
                ..
            }) => self.request_document_action(DocumentAction::OpenTreePath(selected), window, cx),
            Some(VisibleRow::Entry {
                entry:
                    file_tree::TreeEntry {
                        kind: EntryKind::Directory { traversable },
                        ..
                    },
                ..
            }) => self.toggle_tree_directory(selected, traversable, window, cx),
            _ => {}
        }
    }

    fn tree_selection_left(&mut self, cx: &mut Context<Self>) {
        if self.file_tree.collapse_selected() {
            cx.notify();
            return;
        }
        let parent = self
            .file_tree
            .selected()
            .and_then(Path::parent)
            .filter(|parent| Some(*parent) != self.file_tree.root())
            .map(Path::to_path_buf);
        if let Some(parent) = parent {
            self.file_tree.set_selected(Some(parent));
            cx.notify();
        }
    }

    fn tree_selection_right(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.file_tree.expand_selected() {
            self.load_tree_directory(path, window, cx);
        } else {
            cx.notify();
        }
    }

    fn refresh_mermaid(&mut self, timeout: Duration, window: &mut Window, cx: &mut Context<Self>) {
        let jobs = self.mermaid.refresh(&self.document.content, timeout);
        self.image_root
            .retain_mermaid_svgs(&self.mermaid.referenced_assets());
        for job in jobs {
            let worker = self.mermaid.worker();
            let app = cx.entity().downgrade();
            window
                .spawn(cx, async move |cx| {
                    smol::Timer::after(Duration::from_millis(200)).await;
                    let should_render = cx
                        .update(|_, cx| {
                            app.update(cx, |app, _| app.mermaid.needs_result(&job.source_key))
                                .unwrap_or(false)
                        })
                        .unwrap_or(false);
                    if !should_render {
                        let _ = cx.update(|_, cx| {
                            app.update(cx, |app, _| app.mermaid.cancel_pending(&job.source_key))
                                .ok();
                        });
                        return;
                    }

                    let result = worker
                        .render(job.source, job.timeout)
                        .await
                        .unwrap_or_else(|_| Err("Mermaid worker stopped unexpectedly".to_owned()));
                    let _ = cx.update(|window, cx| {
                        app.update(cx, |app, cx| {
                            app.complete_mermaid_render(&job.source_key, result, window, cx)
                        })
                        .ok();
                    });
                })
                .detach();
        }
    }

    fn complete_mermaid_render(
        &mut self,
        source_key: &str,
        result: Result<String, String>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(svg) = self.mermaid.apply_result(source_key, result) {
            if let Err(error) = self.image_root.insert_mermaid_svg(source_key, svg) {
                self.mermaid.apply_result(source_key, Err(error));
            }
        }
        self.image_root
            .retain_mermaid_svgs(&self.mermaid.referenced_assets());
        cx.notify();
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
        self.release_document_images(window, cx);
        self.mermaid.reset();
        self.document = document;
        self.last_path.clone_from(&self.document.path);
        self.image_root
            .set_document_path(self.document.path.as_deref());
        self.focused_section = None;
        self.selected_heading = None;
        self.search_open = false;
        self.view_mode = ViewMode::Preview;
        self.editor.update(cx, |editor, cx| {
            editor.set_value(self.document.content.clone(), window, cx)
        });
        self.refresh_analysis();
        self.refresh_mermaid(OPEN_TIMEOUT, window, cx);
        self.recovery_available = Document::recovery_exists();
        cx.notify();
    }

    fn release_document_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.image_cache
            .update(cx, |cache, cx| cache.clear(window, cx));
        for uri in self.image_root.take_requested_resources() {
            let resource = Resource::Uri(uri.into());
            if let Some(Ok(image)) = window.get_asset::<ImgResourceLoader>(&resource, cx) {
                cx.drop_image(image, Some(window));
            }
            cx.remove_asset::<ImgResourceLoader>(&resource);
        }
    }

    fn release_mermaid_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for uri in self.image_root.take_mermaid_requested_resources() {
            let resource = Resource::Uri(uri.into());
            self.image_cache
                .update(cx, |cache, cx| cache.remove_resource(&resource, window, cx));
            if let Some(Ok(image)) = window.get_asset::<ImgResourceLoader>(&resource, cx) {
                cx.drop_image(image, Some(window));
            }
            cx.remove_asset::<ImgResourceLoader>(&resource);
        }
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
        if matches!(
            &action,
            DocumentAction::OpenTreePath(path)
                if self.document.path.as_deref() == Some(path.as_path())
        ) {
            return;
        }
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
                self.set_file_tree_root(None, window, cx);
                self.view_mode = ViewMode::Split;
                self.set_notice("New document", false);
            }
            DocumentAction::OpenDialog => self.open_dialog(window, cx),
            DocumentAction::OpenPath(path) => self.open_path(path, window, cx),
            DocumentAction::OpenTreePath(path) => self.open_tree_path(path, window, cx),
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
                self.reset_tree_to_document(window, cx);
                self.set_notice("Document opened", false);
            }
            Err(error) => self.set_notice(format!("Could not open document: {error}"), true),
        }
    }

    fn open_tree_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.document.path.as_deref() == Some(path.as_path()) {
            return;
        }
        let view_mode = self.view_mode;
        match Document::open(path) {
            Ok(document) => {
                Document::clear_recovery();
                self.replace_document(document, window, cx);
                self.view_mode = view_mode;
                self.set_notice("Document opened", false);
            }
            Err(error) => self.set_notice(format!("Could not open document: {error}"), true),
        }
    }

    fn recover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match Document::recover() {
            Ok(document) => {
                self.replace_document(document, window, cx);
                self.set_file_tree_root(None, window, cx);
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
                                app.release_document_images(window, cx);
                                app.last_path = Some(path);
                                app.image_root
                                    .set_document_path(app.document.path.as_deref());
                                let should_reset_tree = app.file_tree.root().is_none_or(|root| {
                                    app.document
                                        .path
                                        .as_deref()
                                        .is_none_or(|document| !document.starts_with(root))
                                });
                                if should_reset_tree {
                                    app.reset_tree_to_document(window, cx);
                                }
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

    fn next_hit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.search_hits.is_empty() {
            self.active_hit = (self.active_hit + 1) % self.search_hits.len();
            self.activate_search_hit(window, cx);
        }
    }

    fn previous_hit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.search_hits.is_empty() {
            self.active_hit = if self.active_hit == 0 {
                self.search_hits.len() - 1
            } else {
                self.active_hit - 1
            };
            self.activate_search_hit(window, cx);
        }
    }

    fn activate_search_hit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hit) = self.search_hits.get(self.active_hit) else {
            return;
        };
        if let Some(offset) = hit.source_offset {
            let position = byte_offset_position(&self.document.content, offset);
            self.view_mode = ViewMode::Split;
            self.focused_section = None;
            self.selected_heading = None;
            self.editor.update(cx, |editor, cx| {
                editor.set_cursor_position(position, window, cx)
            });
        } else {
            self.focused_section = Some(hit.section_index);
            self.selected_heading = markdown::sections(&self.document.content, &self.outline)
                .get(hit.section_index)
                .and_then(|section| section.heading_index);
            self.view_mode = ViewMode::Preview;
        }
        cx.notify();
    }

    fn apply_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let factor = self.zoom.factor();
        let theme = Theme::global_mut(cx);
        theme.font_size = px(BASE_FONT_SIZE * factor);
        theme.mono_font_size = px(BASE_MONO_FONT_SIZE * factor);
        self.release_mermaid_images(window, cx);
        cx.notify();
    }

    fn zoom_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.zoom.zoom_in() {
            self.apply_zoom(window, cx);
        }
    }

    fn zoom_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.zoom.zoom_out() {
            self.apply_zoom(window, cx);
        }
    }

    fn reset_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.zoom.reset() {
            self.apply_zoom(window, cx);
        }
    }

    fn handle_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.modifiers.control {
            return;
        }

        let delta_y: f32 = event.delta.pixel_delta(px(20.0)).y.into();
        let factor = (delta_y * 0.0025).exp();
        if self.zoom.apply_gesture(factor) {
            self.apply_zoom(window, cx);
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

    fn responsive_panel_visibility(&self, width: f32) -> (bool, bool, bool, bool) {
        let regular_tree = self.file_tree_open && width >= FILE_TREE_HIDE_BELOW;
        let regular_outline = self.outline_open && width >= OUTLINE_HIDE_BELOW;
        let overlay_tree =
            self.file_tree_open && width < FILE_TREE_HIDE_BELOW && self.file_tree_narrow_reveal;
        let overlay_outline =
            self.outline_open && width < OUTLINE_HIDE_BELOW && self.outline_narrow_reveal;
        (regular_tree, regular_outline, overlay_tree, overlay_outline)
    }

    fn toggle_file_tree(&mut self, window: &Window, cx: &mut Context<Self>) {
        let width: f32 = window.viewport_size().width.into();
        if width < FILE_TREE_HIDE_BELOW {
            if self.file_tree_open {
                self.file_tree_narrow_reveal = !self.file_tree_narrow_reveal;
            } else {
                self.file_tree_open = true;
                self.file_tree_narrow_reveal = true;
                self.persist_layout();
            }
        } else {
            self.file_tree_open = !self.file_tree_open;
            self.file_tree_narrow_reveal = false;
            self.persist_layout();
        }
        cx.notify();
    }

    fn toggle_outline(&mut self, window: &Window, cx: &mut Context<Self>) {
        let width: f32 = window.viewport_size().width.into();
        if width < OUTLINE_HIDE_BELOW {
            if self.outline_open {
                self.outline_narrow_reveal = !self.outline_narrow_reveal;
            } else {
                self.outline_open = true;
                self.outline_narrow_reveal = true;
                self.persist_layout();
            }
        } else {
            self.outline_open = !self.outline_open;
            self.outline_narrow_reveal = false;
            self.persist_layout();
        }
        cx.notify();
    }

    fn toolbar(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dirty = if self.document.is_dirty() { " •" } else { "" };
        let width: f32 = window.viewport_size().width.into();
        let (regular_tree, regular_outline, overlay_tree, overlay_outline) =
            self.responsive_panel_visibility(width);
        let tree_visible = regular_tree || overlay_tree;
        let outline_visible = regular_outline || overlay_outline;
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
                state_button(
                    Button::new("preview-mode").label("Preview").small(),
                    self.view_mode == ViewMode::Preview,
                )
                .on_click(cx.listener(|this, _, _, cx| this.set_view_mode(ViewMode::Preview, cx))),
            )
            .child(
                state_button(
                    Button::new("split-mode").label("Split").small(),
                    self.view_mode == ViewMode::Split,
                )
                .on_click(cx.listener(|this, _, _, cx| this.set_view_mode(ViewMode::Split, cx))),
            )
            .child(
                state_button(
                    Button::new("source-mode").label("Source").small(),
                    self.view_mode == ViewMode::Source,
                )
                .on_click(cx.listener(|this, _, _, cx| this.set_view_mode(ViewMode::Source, cx))),
            )
            .child(div().flex_1())
            .child(
                state_button(
                    Button::new("toggle-file-tree").label("Files").small(),
                    tree_visible,
                )
                .on_click(cx.listener(|this, _, window, cx| this.toggle_file_tree(window, cx))),
            )
            .child(
                state_button(
                    Button::new("toggle-outline").label("Outline").small(),
                    outline_visible,
                )
                .on_click(cx.listener(|this, _, window, cx| this.toggle_outline(window, cx))),
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
                    .on_click(cx.listener(|this, _, window, cx| this.zoom_out(window, cx))),
            )
            .child(
                Button::new("reset-zoom")
                    .label(format!("{}%", self.zoom.percent()))
                    .small()
                    .ghost()
                    .on_click(cx.listener(|this, _, window, cx| this.reset_zoom(window, cx))),
            )
            .child(
                Button::new("zoom-in")
                    .label("+")
                    .small()
                    .ghost()
                    .on_click(cx.listener(|this, _, window, cx| this.zoom_in(window, cx))),
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
                    .on_click(cx.listener(|this, _, window, cx| this.previous_hit(window, cx))),
            )
            .child(
                Button::new("next-hit")
                    .label("Next")
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| this.next_hit(window, cx))),
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

    fn file_tree_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        use std::hash::{Hash as _, Hasher as _};

        let root = self.file_tree.root().map(Path::to_path_buf);
        let root_title = root
            .as_deref()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .or_else(|| root.as_deref().map(|path| path.display().to_string()))
            .unwrap_or_else(|| "No folder".to_owned());
        let full_path = root
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "Save or open a document to show its folder".to_owned());
        let root_tooltip = full_path.clone();
        let can_go_up = root.as_deref().and_then(Path::parent).is_some();
        let has_document_path = self.document.path.is_some();
        let active_not_shown = self.document.path.as_deref().is_some_and(|active| {
            !file_tree::is_markdown_path(active)
                || root.as_deref().is_none_or(|root| !active.starts_with(root))
        });
        let mut root_hasher = std::collections::hash_map::DefaultHasher::new();
        root.hash(&mut root_hasher);
        let root_id = root_hasher.finish();

        let header = div()
            .v_flex()
            .flex_none()
            .gap_2()
            .p_3()
            .border_b_1()
            .border_color(rgb(0xd3c8b5))
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .child(
                        div()
                            .id("file-tree-root-title")
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_semibold()
                            .child(root_title)
                            .tooltip(move |window, cx| {
                                Tooltip::new(root_tooltip.clone()).build(window, cx)
                            }),
                    )
                    .child(
                        Button::new("close-file-tree")
                            .label("×")
                            .small()
                            .ghost()
                            .tooltip("Close file tree")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.file_tree_open = false;
                                this.file_tree_narrow_reveal = false;
                                this.persist_layout();
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .gap_1()
                    .mb_2()
                    .pb_2()
                    .border_b_1()
                    .border_color(rgb(0xd3c8b5))
                    .child(
                        Button::new("tree-up")
                            .label("↑")
                            .small()
                            .ghost()
                            .disabled(!can_go_up)
                            .tooltip("Use the parent folder as root")
                            .on_click(cx.listener(|this, _, window, cx| this.tree_up(window, cx))),
                    )
                    .child(
                        Button::new("tree-document-root")
                            .label("⌂")
                            .small()
                            .ghost()
                            .disabled(!has_document_path)
                            .tooltip("Use the current document folder as root")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reset_tree_to_document(window, cx)
                            })),
                    )
                    .child(
                        Button::new("tree-refresh")
                            .label("↻")
                            .small()
                            .ghost()
                            .disabled(root.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.refresh_file_tree(window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .gap_1()
                    .child(
                        Button::new("tree-choose-root")
                            .label("…")
                            .small()
                            .ghost()
                            .tooltip("Choose a folder as root")
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.choose_tree_root(window, cx)
                                }),
                            ),
                    )
                    .child(
                        state_button(
                            Button::new("tree-show-hidden").label("Hidden").small(),
                            self.file_tree.show_hidden(),
                        )
                        .tooltip("Show hidden, system, and heavy folders")
                        .on_click(cx.listener(|this, _, window, cx| {
                            let show = !this.file_tree.show_hidden();
                            let paths = this.file_tree.set_show_hidden(show);
                            for path in paths {
                                this.load_tree_directory(path, window, cx);
                            }
                            cx.notify();
                        })),
                    ),
            )
            .when(active_not_shown, |view| {
                view.child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x96692b))
                        .child("Current file is not shown in this tree"),
                )
            });

        let mut body = div()
            .id(("file-tree-scroll", root_id))
            .v_flex()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .py_2();

        if root.is_none() {
            body = body.child(
                div()
                    .v_flex()
                    .gap_2()
                    .p_4()
                    .text_sm()
                    .text_color(rgb(0x70685b))
                    .child("Save the document to show its folder.")
                    .child(
                        Button::new("tree-empty-save")
                            .label("Save document")
                            .small()
                            .on_click(
                                cx.listener(|this, _, window, cx| this.save_current(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("tree-empty-open")
                            .label("Open document")
                            .small()
                            .ghost()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.request_document_action(DocumentAction::OpenDialog, window, cx)
                            })),
                    ),
            );
        } else {
            for (row_index, row) in self.file_tree.visible_rows().into_iter().enumerate() {
                match row {
                    VisibleRow::Entry {
                        entry,
                        depth,
                        expanded,
                    } => {
                        let path = entry.path.clone();
                        let is_active = self.document.path.as_deref() == Some(path.as_path());
                        let is_cursor = self.file_tree.selected() == Some(path.as_path());
                        let (prefix, is_directory, traversable) = match entry.kind {
                            EntryKind::Directory { traversable } => (
                                if !traversable {
                                    "↗"
                                } else if expanded {
                                    "▾"
                                } else {
                                    "▸"
                                },
                                true,
                                traversable,
                            ),
                            EntryKind::Markdown => ("◆", false, false),
                        };
                        let click_path = path.clone();
                        let path_tooltip = path.display().to_string();
                        let focus = self.file_tree_focus.clone();
                        let row_element = div()
                            .id(("file-tree-row", row_index))
                            .h_flex()
                            .h(px(30.0))
                            .flex_none()
                            .min_w_0()
                            .pl(px(8.0 + depth as f32 * 16.0))
                            .pr_2()
                            .gap_2()
                            .text_sm()
                            .cursor_pointer()
                            .when(is_active, |view| view.bg(rgb(0xd7c4a5)).font_semibold())
                            .when(is_cursor && !is_active, |view| view.bg(rgb(0xe3d8c6)))
                            .hover(|view| view.bg(rgb(0xe6dccb)))
                            .child(
                                div()
                                    .w(px(14.0))
                                    .flex_none()
                                    .text_color(if is_directory {
                                        rgb(0x96692b)
                                    } else {
                                        rgb(0x665b4d)
                                    })
                                    .child(prefix),
                            )
                            .child(div().min_w_0().truncate().child(entry.name.clone()))
                            .tooltip(move |window, cx| {
                                Tooltip::new(path_tooltip.clone()).build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                focus.focus(window);
                                if is_directory {
                                    this.toggle_tree_directory(
                                        click_path.clone(),
                                        traversable,
                                        window,
                                        cx,
                                    );
                                } else {
                                    this.request_document_action(
                                        DocumentAction::OpenTreePath(click_path.clone()),
                                        window,
                                        cx,
                                    );
                                }
                            }));

                        body = if is_directory {
                            let menu_path = path.clone();
                            let app = cx.entity().downgrade();
                            body.child(row_element.context_menu(move |menu, _, cx| {
                                app.update(cx, |app, _| {
                                    app.tree_context_path = Some(menu_path.clone())
                                })
                                .ok();
                                menu.menu("Set as root", Box::new(SetTreeRootContext))
                            }))
                        } else {
                            let menu_path = path.clone();
                            let app = cx.entity().downgrade();
                            body.child(row_element.context_menu(move |menu, _, cx| {
                                app.update(cx, |app, _| {
                                    app.tree_context_path = Some(menu_path.clone())
                                })
                                .ok();
                                menu.menu("Open", Box::new(OpenTreeContext))
                            }))
                        };
                    }
                    VisibleRow::Loading {
                        depth, refreshing, ..
                    } => {
                        body = body.child(
                            div()
                                .pl(px(24.0 + depth as f32 * 16.0))
                                .py_1()
                                .text_xs()
                                .text_color(rgb(0x70685b))
                                .child(if refreshing {
                                    "Refreshing…"
                                } else {
                                    "Loading…"
                                }),
                        );
                    }
                    VisibleRow::Empty { depth, .. } => {
                        body = body.child(
                            div()
                                .pl(px(24.0 + depth as f32 * 16.0))
                                .py_1()
                                .text_xs()
                                .text_color(rgb(0x8b8275))
                                .child("No Markdown files or folders"),
                        );
                    }
                    VisibleRow::Error {
                        path,
                        depth,
                        message,
                    } => {
                        let retry_path = path.clone();
                        body = body.child(
                            div()
                                .h_flex()
                                .pl(px(24.0 + depth as f32 * 16.0))
                                .pr_2()
                                .gap_2()
                                .text_xs()
                                .text_color(rgb(0x9b392a))
                                .child(div().flex_1().min_w_0().truncate().child(message))
                                .child(
                                    Button::new(("tree-retry", row_index))
                                        .label("Retry")
                                        .small()
                                        .ghost()
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.load_tree_directory(retry_path.clone(), window, cx)
                                        })),
                                ),
                        );
                    }
                }
            }
        }

        div()
            .debug_selector(|| "file-tree-panel".into())
            .key_context(FILE_TREE_CONTEXT)
            .track_focus(&self.file_tree_focus)
            .on_action(cx.listener(|this, _: &FileTreeUp, _, cx| this.move_tree_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &FileTreeDown, _, cx| this.move_tree_selection(1, cx)))
            .on_action(cx.listener(|this, _: &FileTreeLeft, _, cx| this.tree_selection_left(cx)))
            .on_action(cx.listener(|this, _: &FileTreeRight, window, cx| {
                this.tree_selection_right(window, cx)
            }))
            .on_action(cx.listener(|this, _: &FileTreeOpen, window, cx| {
                this.open_selected_tree_entry(window, cx)
            }))
            .v_flex()
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .border_r_1()
            .border_color(rgb(0xd3c8b5))
            .bg(rgb(0xebe3d4))
            .child(header)
            .child(body)
    }

    fn outline_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let sections = markdown::sections(&self.document.content, &self.outline);
        let mut panel = div()
            .debug_selector(|| "outline-panel".into())
            .v_flex()
            .size_full()
            .min_w_0()
            .h_full()
            .flex_none()
            .overflow_y_scrollbar()
            .p_3()
            .gap_1()
            .border_l_1()
            .border_color(rgb(0xd3c8b5))
            .bg(rgb(0xebe3d4))
            .child(
                div()
                    .debug_selector(|| "outline-mode-toolbar".into())
                    .h_flex()
                    .gap_1()
                    .mb_2()
                    .pb_2()
                    .border_b_1()
                    .border_color(rgb(0xd3c8b5))
                    .child(
                        state_button(
                            Button::new("outline-jump-mode").label("Jump").small(),
                            self.outline_mode == OutlineMode::Jump,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.outline_mode = OutlineMode::Jump;
                            this.focused_section = None;
                            if this.selected_heading.is_some() {
                                this.outline_jump_request =
                                    this.outline_jump_request.wrapping_add(1);
                            }
                            cx.notify();
                        })),
                    )
                    .child(
                        state_button(
                            Button::new("outline-focus-mode").label("Focus").small(),
                            self.outline_mode == OutlineMode::Focus,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.outline_mode = OutlineMode::Focus;
                            this.focused_section = this.selected_heading.and_then(|heading| {
                                markdown::sections(&this.document.content, &this.outline)
                                    .iter()
                                    .position(|section| section.heading_index == Some(heading))
                            });
                            cx.notify();
                        })),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "outline-full-document".into())
                    .child(
                        state_button(
                            Button::new("show-full-document")
                                .label("Full document")
                                .small()
                                .w_full()
                                .justify_start(),
                            self.focused_section.is_none() && self.selected_heading.is_none(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.focused_section = None;
                            this.selected_heading = None;
                            this.view_mode = ViewMode::Preview;
                            cx.notify();
                        })),
                    ),
            );

        for (heading_index, heading) in self.outline.iter().enumerate() {
            let section_index = sections
                .iter()
                .position(|section| section.heading_index == Some(heading_index));
            let marker = if heading.level == 1 { "◆" } else { "•" };
            let label = format!("{marker} {}", heading.title);
            panel = panel.child(
                Button::new(("outline-heading", heading_index))
                    .label(label)
                    .small()
                    .ghost()
                    .w_full()
                    .justify_start()
                    .pl(px(outline_indent(heading.level)))
                    .pr_2()
                    .selected(self.selected_heading == Some(heading_index))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_heading = Some(heading_index);
                        match this.outline_mode {
                            OutlineMode::Jump => {
                                this.focused_section = None;
                                this.outline_jump_request =
                                    this.outline_jump_request.wrapping_add(1);
                            }
                            OutlineMode::Focus => this.focused_section = section_index,
                        }
                        this.view_mode = ViewMode::Preview;
                        cx.notify();
                    })),
            );
        }
        if self.outline.is_empty() {
            panel = panel.child(
                div()
                    .p_3()
                    .text_sm()
                    .text_color(rgb(0x70685b))
                    .child("This document has no headings"),
            );
        }
        panel
    }

    fn preview_source(&self) -> (SharedString, usize) {
        let Some(range) = self.focused_section.and_then(|section_index| {
            markdown::sections(&self.document.content, &self.outline)
                .get(section_index)
                .map(|section| section.range.clone())
        }) else {
            return (self.preview_markdown.clone(), 0);
        };
        let source_offset = range.start;
        (
            SharedString::from(self.document.content[range].to_owned()),
            source_offset,
        )
    }

    fn preview_panel(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let logical_width: f32 = window.viewport_size().width.into();
        let panel_fraction = if self.view_mode == ViewMode::Split {
            0.5
        } else {
            1.0
        };
        let viewport_width = (logical_width * panel_fraction * window.scale_factor())
            .ceil()
            .clamp(1.0, 1_280.0) as u32;
        self.image_root.set_viewport_width(viewport_width);
        let app = cx.entity().downgrade();
        let (preview_source, preview_source_offset) = self.preview_source();
        let mut preview = TextView::markdown("native-markdown-preview", preview_source, window, cx)
            .selectable(true)
            .scrollable(true);
        if self.outline_mode == OutlineMode::Jump {
            if let Some(heading_index) = self.selected_heading {
                preview = preview.scroll_to_heading_once(heading_index, self.outline_jump_request);
            }
        }
        let preview = preview.code_block_renderer(move |code_block, _, cx| {
            if !code_block
                .lang()
                .as_deref()
                .is_some_and(|language| language.trim().eq_ignore_ascii_case("mermaid"))
            {
                return None;
            }
            let source_range = code_block.source_range()?;
            let source_range = source_range.start + preview_source_offset
                ..source_range.end + preview_source_offset;
            let entity = app.upgrade()?;
            let app_state = entity.read(cx);
            let preview = app_state.mermaid.preview_for_block(
                source_range,
                viewport_width,
                app_state.zoom.percent(),
            )?;
            Some(mermaid_preview_element(preview, app.clone()))
        });

        div()
            .debug_selector(|| "preview-panel".into())
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(rgb(0xfaf7f0))
            .child(
                image_cache(self.image_cache.clone())
                    .size_full()
                    .child(div().size_full().px_5().child(preview)),
            )
    }

    fn show_mermaid_source(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(range) = self.mermaid.block_body_range(index) else {
            return;
        };
        let position = byte_offset_position(&self.document.content, range.start);
        self.view_mode = ViewMode::Split;
        self.focused_section = None;
        self.selected_heading = None;
        self.editor.update(cx, |editor, cx| {
            editor.set_cursor_position(position, window, cx)
        });
        self.set_notice(
            format!("Mermaid source at line {}", position.line + 1),
            false,
        );
        cx.notify();
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
        let content = if self.document.is_empty() {
            self.welcome(cx).into_any_element()
        } else {
            match self.view_mode {
                ViewMode::Preview => self.preview_panel(window, cx).into_any_element(),
                ViewMode::Source => self.editor_panel().into_any_element(),
                ViewMode::Split => div()
                    .h_flex()
                    .size_full()
                    .child(self.editor_panel())
                    .child(div().w(px(1.0)).h_full().bg(rgb(0xd3c8b5)))
                    .child(self.preview_panel(window, cx))
                    .into_any_element(),
            }
        };

        let width: f32 = window.viewport_size().width.into();
        let (regular_tree, regular_outline, overlay_tree, overlay_outline) =
            self.responsive_panel_visibility(width);
        let app = cx.entity().downgrade();
        let resizable = h_resizable("document-workspace-panels")
            .with_state(&self.workspace_resizable)
            .on_resize(move |state, _, cx| {
                let sizes = state.read(cx).sizes().clone();
                if sizes.len() != 3 {
                    return;
                }
                let file_tree_width: f32 = sizes[0].into();
                let outline_width: f32 = sizes[2].into();
                app.update(cx, |app, _| {
                    let mut settings = LayoutSettings {
                        file_tree_open: app.file_tree_open,
                        outline_open: app.outline_open,
                        file_tree_width: app.file_tree_width,
                        outline_width: app.outline_width,
                    };
                    settings.set_widths(file_tree_width, outline_width);
                    app.file_tree_width = settings.file_tree_width;
                    app.outline_width = settings.outline_width;
                    let _ = settings.save();
                })
                .ok();
            })
            .child(
                resizable_panel()
                    .visible(regular_tree)
                    .size(px(self.file_tree_width))
                    .size_range(px(120.0)..px(520.0))
                    .child(self.file_tree_panel(cx)),
            )
            .child(
                resizable_panel()
                    .size_range(px(420.0)..px(10_000.0))
                    .child(content),
            )
            .child(
                resizable_panel()
                    .visible(regular_outline)
                    .size(px(self.outline_width))
                    .size_range(px(120.0)..px(520.0))
                    .child(self.outline_panel(cx)),
            );

        div()
            .debug_selector(|| "document-workspace".into())
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(resizable)
            .when(overlay_tree, |view| {
                view.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(self.file_tree_width))
                        .shadow_lg()
                        .child(self.file_tree_panel(cx)),
                )
            })
            .when(overlay_outline, |view| {
                view.child(
                    div()
                        .absolute()
                        .right_0()
                        .top_0()
                        .bottom_0()
                        .w(px(self.outline_width))
                        .shadow_lg()
                        .child(self.outline_panel(cx)),
                )
            })
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
        let mermaid_status = match self.mermaid.error_count() {
            0 => String::new(),
            count => format!(
                " · {count} Mermaid error{}",
                if count == 1 { "" } else { "s" }
            ),
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
                "{} words · {} min read · {} · {}% · {} local images{image_status}{mermaid_status}",
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
            .on_action(cx.listener(|this, _: &ZoomIn, window, cx| this.zoom_in(window, cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, window, cx| this.zoom_out(window, cx)))
            .on_action(cx.listener(|this, _: &ResetZoom, window, cx| this.reset_zoom(window, cx)))
            .on_action(cx.listener(|this, _: &OpenTreeContext, window, cx| {
                if let Some(path) = this.tree_context_path.clone() {
                    this.request_document_action(DocumentAction::OpenTreePath(path), window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SetTreeRootContext, window, cx| {
                if let Some(path) = this.tree_context_path.clone() {
                    this.set_file_tree_root(Some(path), window, cx);
                }
            }))
            .on_drop(cx.listener(Self::handle_drop))
            .v_flex()
            .size_full()
            .overflow_hidden()
            .bg(rgb(0xf6f1e7))
            .text_color(rgb(0x292723))
            .child(self.toolbar(window, cx))
            .when(self.search_open, |view| view.child(self.search_bar(cx)))
            .child(self.workspace(window, cx))
            .child(self.status_bar(cx))
    }
}

fn mermaid_preview_element(
    preview: MermaidPreview,
    app: WeakEntity<NativeMarkdownApp>,
) -> AnyElement {
    let (image_uri, message, is_error) = match preview.status {
        MermaidPreviewStatus::Ready { image_uri } => (Some(image_uri), None, false),
        MermaidPreviewStatus::Loading { image_uri, message } => (image_uri, Some(message), false),
        MermaidPreviewStatus::Error { image_uri, message } => (image_uri, Some(message), true),
    };
    let mut element = div().v_flex().w_full().gap_2();
    if let Some(image_uri) = image_uri {
        element = element.child(
            img(SharedString::from(image_uri))
                .object_fit(ObjectFit::Contain)
                .max_w(relative(1.0)),
        );
    }
    if let Some(message) = message {
        let index = preview.index;
        let source_app = app.clone();
        element = element.child(
            div()
                .debug_selector(|| "mermaid-preview-message".into())
                .v_flex()
                .w_full()
                .gap_2()
                .p_3()
                .rounded_md()
                .bg(if is_error {
                    rgb(0xf7e5df)
                } else {
                    rgb(0xeee9df)
                })
                .text_color(if is_error {
                    rgb(0x9b392a)
                } else {
                    rgb(0x665b4d)
                })
                .child(div().whitespace_normal().child(message))
                .child(
                    Button::new(("mermaid-view-source", index))
                        .label("View source")
                        .small()
                        .on_click(move |_, window, cx| {
                            source_app
                                .update(cx, |app, cx| app.show_mermaid_source(index, window, cx))
                                .ok();
                        }),
                ),
        );
    }
    if preview.experimental {
        element = element.child(div().text_sm().text_color(rgb(0x70685b)).child(format!(
            "Experimental Mermaid {} rendering",
            preview.diagram_label
        )));
    }
    element.into_any_element()
}

fn byte_offset_position(source: &str, offset: usize) -> Position {
    let offset = offset.min(source.len());
    let before = &source[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = before[line_start..].chars().count() as u32;
    Position::new(line, character)
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

    #[gpui::test]
    fn tree_open_preserves_view_root_and_current_file_no_op(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.md");
        let nested = directory.path().join("nested");
        let second = nested.join("second.md");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(&first, "# First").unwrap();
        std::fs::write(&second, "# Second").unwrap();
        let image_root = DocumentImageRoot::default();

        let (app, cx) = cx.add_window_view(|window, cx| {
            NativeMarkdownApp::new(Some(first.clone()), image_root, window, cx)
        });
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.view_mode = ViewMode::Source;
                app.open_tree_path(second.clone(), window, cx);
                assert_eq!(app.document.path.as_deref(), Some(second.as_path()));
                assert_eq!(app.file_tree.root(), Some(directory.path()));
                assert_eq!(app.view_mode, ViewMode::Source);
            });
        });
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.document.content = "unsaved edit".to_owned();
                app.request_document_action(
                    DocumentAction::OpenTreePath(second.clone()),
                    window,
                    cx,
                );
            });
        });
        assert!(!cx.has_pending_prompt());
        app.read_with(cx, |app, _| {
            assert_eq!(app.document.content, "unsaved edit");
            assert_eq!(app.view_mode, ViewMode::Source);
        });
    }

    #[gpui::test]
    fn external_open_rehomes_tree_and_uses_preview(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.md");
        let other = directory.path().join("other");
        let second = other.join("second.md");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(&first, "# First").unwrap();
        std::fs::write(&second, "# Second").unwrap();
        let image_root = DocumentImageRoot::default();

        let (app, cx) = cx.add_window_view(|window, cx| {
            NativeMarkdownApp::new(Some(first), image_root, window, cx)
        });
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.view_mode = ViewMode::Split;
                app.open_path(second.clone(), window, cx);
                assert_eq!(app.document.path.as_deref(), Some(second.as_path()));
                assert_eq!(app.file_tree.root(), Some(other.as_path()));
                assert_eq!(app.view_mode, ViewMode::Preview);
            });
        });
    }

    #[gpui::test]
    fn sidebars_render_on_opposite_sides_and_hide_responsively(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("layout.md");
        std::fs::write(&path, "# Layout\n\n## Section").unwrap();
        let image_root = DocumentImageRoot::default();
        let (app, cx) = cx.add_window_view(|window, cx| {
            NativeMarkdownApp::new(Some(path), image_root, window, cx)
        });

        cx.simulate_resize(size(px(1180.0), px(780.0)));
        app.update(cx, |_, cx| cx.notify());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let tree = cx.debug_bounds("file-tree-panel").unwrap();
        let preview = cx.debug_bounds("preview-panel").unwrap();
        let outline = cx.debug_bounds("outline-panel").unwrap();
        let outline_toolbar = cx.debug_bounds("outline-mode-toolbar").unwrap();
        let full_document = cx.debug_bounds("outline-full-document").unwrap();
        assert!(tree.origin.x + tree.size.width <= preview.origin.x);
        assert!(outline.origin.x >= preview.origin.x + preview.size.width);
        let outline_control_gap =
            full_document.origin.y - (outline_toolbar.origin.y + outline_toolbar.size.height);
        assert!(
            outline_control_gap >= px(8.0),
            "outline mode controls need breathing room before the document tree: {outline_control_gap:?}"
        );

        app.read_with(cx, |app, _| {
            assert_eq!(
                app.responsive_panel_visibility(900.0),
                (true, false, false, false)
            );
            assert_eq!(
                app.responsive_panel_visibility(700.0),
                (false, false, false, false)
            );
        });
    }

    #[gpui::test]
    fn focused_section_renders_mermaid_without_rewriting_markdown(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("focused-mermaid.md");
        std::fs::write(
            &path,
            "# First\n\nText.\n\n# Diagram\n\n```mermaid\nflowchart LR\nA-->B\n```\n",
        )
        .unwrap();
        let image_root = DocumentImageRoot::default();
        let (app, cx) = cx.add_window_view(|window, cx| {
            let mut app = NativeMarkdownApp::new(Some(path), image_root, window, cx);
            app.focused_section = Some(1);
            app
        });

        app.update(cx, |_, cx| cx.notify());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        assert!(cx.debug_bounds("mermaid-preview-message").is_some());
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
