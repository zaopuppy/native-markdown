use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, FontFamily, FontId, Key, KeyboardShortcut, Modifiers, RichText,
    Sense, TextFormat, Vec2,
};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

use crate::document::Document;
use crate::markdown::{self, Heading, SearchHit};
use crate::scroll::{ScrollMetrics, ScrollPane, ScrollSync};
use crate::theme;
use crate::zoom::ZoomLevel;

const LAST_PATH_KEY: &str = "native-markdown.last-path";
const ZOOM_KEY: &str = "native-markdown.zoom-factor";
const READING_WIDTH: f32 = 760.0;
const MIN_SPLIT_PANE_WIDTH: f32 = 320.0;

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

#[derive(Clone, Debug)]
enum PendingAction {
    New,
    OpenDialog,
    OpenPath(PathBuf),
    Close,
}

#[derive(Clone, Debug)]
enum Command {
    Action(PendingAction),
    Save,
    SaveAs,
}

struct Notice {
    text: String,
    is_error: bool,
    created_at: Instant,
}

pub struct NativeMarkdownApp {
    document: Document,
    view_mode: ViewMode,
    outline_open: bool,
    search_open: bool,
    search_query: String,
    last_search_query: String,
    active_hit: usize,
    search_hits: Vec<SearchHit>,
    outline: Vec<Heading>,
    analyzed_source: String,
    preview_target: Option<usize>,
    markdown_cache: CommonMarkCache,
    pending_action: Option<PendingAction>,
    queued_command: Option<Command>,
    last_path: Option<PathBuf>,
    recovery_available: bool,
    notice: Option<Notice>,
    last_title: String,
    focus_search: bool,
    synchronize_scroll: bool,
    scroll_sync: ScrollSync,
    source_scroll: ScrollMetrics,
    preview_scroll: ScrollMetrics,
    zoom: ZoomLevel,
}

impl NativeMarkdownApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        cc.egui_ctx
            .options_mut(|options| options.zoom_with_keyboard = false);

        let last_path = cc
            .storage
            .and_then(|storage| storage.get_string(LAST_PATH_KEY))
            .map(PathBuf::from)
            .filter(|path| path.is_file());

        let cli_path = std::env::args_os()
            .nth(1)
            .map(PathBuf::from)
            .filter(|path| path.is_file());

        let (document, notice) = if let Some(path) = cli_path {
            match Document::open(path) {
                Ok(document) => (document, None),
                Err(error) => (
                    Document::default(),
                    Some(Notice {
                        text: format!("Could not open document: {error}"),
                        is_error: true,
                        created_at: Instant::now(),
                    }),
                ),
            }
        } else {
            (Document::default(), None)
        };

        let analyzed_source = document.content.clone();
        let outline = markdown::headings(&analyzed_source);
        let recovery_available = Document::recovery_exists();
        let zoom = cc
            .storage
            .and_then(|storage| storage.get_string(ZOOM_KEY))
            .and_then(|factor| factor.parse::<f32>().ok())
            .map(ZoomLevel::from_factor)
            .unwrap_or_default();
        cc.egui_ctx.set_zoom_factor(zoom.factor());

        Self {
            document,
            view_mode: ViewMode::Preview,
            outline_open: false,
            search_open: false,
            search_query: String::new(),
            last_search_query: String::new(),
            active_hit: 0,
            search_hits: Vec::new(),
            outline,
            analyzed_source,
            preview_target: None,
            markdown_cache: CommonMarkCache::default(),
            pending_action: None,
            queued_command: None,
            last_path,
            recovery_available,
            notice,
            last_title: String::new(),
            focus_search: false,
            synchronize_scroll: true,
            scroll_sync: ScrollSync::default(),
            source_scroll: ScrollMetrics::default(),
            preview_scroll: ScrollMetrics::default(),
            zoom,
        }
    }

    fn queue(&mut self, command: Command) {
        self.queued_command = Some(command);
    }

    fn open_dialog(&mut self) {
        let dialog = rfd::FileDialog::new()
            .add_filter("Markdown", &["md", "markdown", "mdown", "mkd"])
            .add_filter("Text", &["txt"]);
        if let Some(path) = dialog.pick_file() {
            self.open_path(path);
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        match Document::open(path.clone()) {
            Ok(document) => {
                self.document = document;
                self.last_path = Some(path);
                self.view_mode = ViewMode::Preview;
                self.search_open = false;
                self.outline_open = false;
                self.preview_target = None;
                self.scroll_sync.reset();
                self.recovery_available = false;
                Document::clear_recovery();
                self.set_notice("Document opened", false);
                self.refresh_analysis();
            }
            Err(error) => self.set_notice(format!("Could not open document: {error}"), true),
        }
    }

    fn save_current(&mut self) -> bool {
        if self.document.path.is_none() {
            return self.save_as();
        }

        match self.document.save() {
            Ok(()) => {
                self.last_path.clone_from(&self.document.path);
                self.set_notice("Saved", false);
                true
            }
            Err(error) => {
                self.set_notice(format!("Could not save document: {error}"), true);
                false
            }
        }
    }

    fn save_as(&mut self) -> bool {
        let mut dialog = rfd::FileDialog::new()
            .add_filter("Markdown", &["md", "markdown"])
            .set_file_name(self.document.display_name());
        if let Some(parent) = self.document.path.as_deref().and_then(Path::parent) {
            dialog = dialog.set_directory(parent);
        }

        let Some(mut path) = dialog.save_file() else {
            return false;
        };
        if path.extension().is_none() {
            path.set_extension("md");
        }

        match self.document.save_as(path.clone()) {
            Ok(()) => {
                self.last_path = Some(path);
                self.set_notice("Saved", false);
                true
            }
            Err(error) => {
                self.set_notice(format!("Could not save document: {error}"), true);
                false
            }
        }
    }

    fn request_action(&mut self, action: PendingAction) {
        if self.document.is_dirty() {
            self.pending_action = Some(action);
        } else {
            self.execute_action(action);
        }
    }

    fn execute_action(&mut self, action: PendingAction) {
        match action {
            PendingAction::New => {
                Document::clear_recovery();
                self.document = Document::new_document();
                self.view_mode = ViewMode::Split;
                self.outline_open = false;
                self.search_open = false;
                self.scroll_sync.reset();
                self.refresh_analysis();
            }
            PendingAction::OpenDialog => self.open_dialog(),
            PendingAction::OpenPath(path) => self.open_path(path),
            PendingAction::Close => {
                Document::clear_recovery();
                self.document = Document::default();
            }
        }
    }

    fn process_command(&mut self, command: Command, ctx: &egui::Context) {
        match command {
            Command::Action(PendingAction::Close) if !self.document.is_dirty() => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Command::Action(action) => self.request_action(action),
            Command::Save => {
                self.save_current();
            }
            Command::SaveAs => {
                self.save_as();
            }
        }
    }

    fn set_notice(&mut self, text: impl Into<String>, is_error: bool) {
        self.notice = Some(Notice {
            text: text.into(),
            is_error,
            created_at: Instant::now(),
        });
    }

    fn refresh_analysis(&mut self) {
        self.analyzed_source.clone_from(&self.document.content);
        self.outline = markdown::headings(&self.document.content);
        self.search_hits =
            markdown::search(&self.document.content, &self.search_query, &self.outline);
        self.active_hit = self
            .active_hit
            .min(self.search_hits.len().saturating_sub(1));
        self.markdown_cache = CommonMarkCache::default();
    }

    fn sync_analysis(&mut self) {
        if self.document.content != self.analyzed_source {
            self.refresh_analysis();
        } else if self.search_query != self.last_search_query {
            self.search_hits =
                markdown::search(&self.document.content, &self.search_query, &self.outline);
            self.active_hit = 0;
            self.last_search_query.clone_from(&self.search_query);
        }
    }

    fn apply_zoom(&self, ctx: &egui::Context) {
        ctx.set_zoom_factor(self.zoom.factor());
    }

    fn zoom_in(&mut self, ctx: &egui::Context) {
        if self.zoom.zoom_in() {
            self.apply_zoom(ctx);
        }
    }

    fn zoom_out(&mut self, ctx: &egui::Context) {
        if self.zoom.zoom_out() {
            self.apply_zoom(ctx);
        }
    }

    fn reset_zoom(&mut self, ctx: &egui::Context) {
        if self.zoom.reset() {
            self.apply_zoom(ctx);
        }
    }

    fn handle_zoom_gesture(&mut self, ctx: &egui::Context) {
        let delta = ctx.input(|input| input.zoom_delta());
        if self.zoom.apply_gesture(delta) {
            self.apply_zoom(ctx);
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let command = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let command_shift = |key| {
            KeyboardShortcut::new(
                Modifiers {
                    command: true,
                    shift: true,
                    ..Modifiers::NONE
                },
                key,
            )
        };
        let mut zoom_step = 0_i8;
        let mut reset_zoom = false;

        ctx.input_mut(|input| {
            if input.consume_shortcut(&command(Key::Plus))
                || input.consume_shortcut(&command(Key::Equals))
            {
                zoom_step = 1;
            } else if input.consume_shortcut(&command(Key::Minus)) {
                zoom_step = -1;
            } else if input.consume_shortcut(&command(Key::Num0)) {
                reset_zoom = true;
            } else if input.consume_shortcut(&command(Key::O)) {
                self.queue(Command::Action(PendingAction::OpenDialog));
            } else if input.consume_shortcut(&command(Key::N)) {
                self.queue(Command::Action(PendingAction::New));
            } else if input.consume_shortcut(&command_shift(Key::S)) {
                self.queue(Command::SaveAs);
            } else if input.consume_shortcut(&command(Key::S)) {
                self.queue(Command::Save);
            } else if input.consume_shortcut(&command(Key::F)) {
                self.search_open = true;
                self.focus_search = true;
            } else if input.consume_shortcut(&command(Key::E)) {
                self.view_mode = if self.view_mode == ViewMode::Preview {
                    ViewMode::Split
                } else {
                    ViewMode::Preview
                };
            } else if input.consume_shortcut(&command(Key::Num1)) {
                self.view_mode = ViewMode::Preview;
            } else if input.consume_shortcut(&command(Key::Num2)) {
                self.view_mode = ViewMode::Split;
            } else if input.consume_shortcut(&command(Key::Num3)) {
                self.view_mode = ViewMode::Source;
            } else if input.key_pressed(Key::Escape) && self.search_open {
                self.search_open = false;
            }
        });

        if reset_zoom {
            self.reset_zoom(ctx);
        } else if zoom_step > 0 {
            self.zoom_in(ctx);
        } else if zoom_step < 0 {
            self.zoom_out(ctx);
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|input| input.raw.dropped_files.clone());
        if let Some(path) = dropped.into_iter().find_map(|file| file.path) {
            self.queue(Command::Action(PendingAction::OpenPath(path)));
        }
    }

    fn header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("app_header")
            .exact_height(78.0)
            .frame(
                egui::Frame::none()
                    .fill(theme::PAPER)
                    .inner_margin(egui::Margin::symmetric(16.0, 7.0))
                    .stroke(egui::Stroke::new(1.0, theme::RULE)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("NATIVE / MARKDOWN")
                            .size(11.0)
                            .strong()
                            .color(theme::BRASS),
                    );
                    ui.add_space(10.0);
                    egui::menu::bar(ui, |ui| {
                        ui.menu_button("File", |ui| {
                            if menu_item(ui, "New document", "Ctrl+N") {
                                self.queue(Command::Action(PendingAction::New));
                                ui.close_menu();
                            }
                            if menu_item(ui, "Open…", "Ctrl+O") {
                                self.queue(Command::Action(PendingAction::OpenDialog));
                                ui.close_menu();
                            }
                            ui.separator();
                            if menu_item(ui, "Save", "Ctrl+S") {
                                self.queue(Command::Save);
                                ui.close_menu();
                            }
                            if menu_item(ui, "Save as…", "Ctrl+Shift+S") {
                                self.queue(Command::SaveAs);
                                ui.close_menu();
                            }
                        });
                        ui.menu_button("View", |ui| {
                            for mode in [ViewMode::Preview, ViewMode::Split, ViewMode::Source] {
                                if ui
                                    .selectable_label(self.view_mode == mode, mode.label())
                                    .clicked()
                                {
                                    self.view_mode = mode;
                                    ui.close_menu();
                                }
                            }
                            ui.separator();
                            ui.checkbox(&mut self.outline_open, "Document outline");
                            if ui
                                .checkbox(&mut self.synchronize_scroll, "Synchronize scrolling")
                                .changed()
                            {
                                self.scroll_sync.reset();
                            }
                            if ui.button("Find in document").clicked() {
                                self.search_open = true;
                                self.focus_search = true;
                                ui.close_menu();
                            }
                            ui.separator();
                            ui.label(
                                RichText::new(format!("Zoom: {}%", self.zoom.percent()))
                                    .size(12.0)
                                    .color(theme::MUTED),
                            );
                            if menu_item(ui, "Zoom in", "Ctrl++") {
                                self.zoom_in(ctx);
                                ui.close_menu();
                            }
                            if menu_item(ui, "Zoom out", "Ctrl+-") {
                                self.zoom_out(ctx);
                                ui.close_menu();
                            }
                            if menu_item(ui, "Reset zoom", "Ctrl+0") {
                                self.reset_zoom(ctx);
                                ui.close_menu();
                            }
                        });
                    });
                });

                ui.add_space(3.0);
                ui.horizontal(|ui| {
                    let dirty = if self.document.is_dirty() {
                        "  •"
                    } else {
                        ""
                    };
                    let path_hint = self
                        .document
                        .path
                        .as_deref()
                        .map(|path| path.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "No file path".to_owned());
                    ui.label(
                        RichText::new(format!("{}{}", self.document.display_name(), dirty))
                            .size(17.0)
                            .strong(),
                    )
                    .on_hover_text(path_hint);

                    ui.separator();
                    if ui
                        .button("Open")
                        .on_hover_text("Open a Markdown document  Ctrl+O")
                        .clicked()
                    {
                        self.queue(Command::Action(PendingAction::OpenDialog));
                    }
                    if (!self.document.is_empty())
                        && (self.document.is_dirty() || self.view_mode != ViewMode::Preview)
                        && ui
                            .button("Save")
                            .on_hover_text("Save changes  Ctrl+S")
                            .clicked()
                    {
                        self.queue(Command::Save);
                    }

                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .button("Find")
                            .on_hover_text("Find in document  Ctrl+F")
                            .clicked()
                        {
                            self.search_open = true;
                            self.focus_search = true;
                        }
                        if self.view_mode != ViewMode::Source
                            && ui.selectable_label(self.outline_open, "Outline").clicked()
                        {
                            self.outline_open = !self.outline_open;
                        }
                        if self.view_mode == ViewMode::Split
                            && ui
                                .selectable_label(self.synchronize_scroll, "Sync scroll")
                                .on_hover_text(
                                    "Keep source and preview at the same reading position",
                                )
                                .clicked()
                        {
                            self.synchronize_scroll = !self.synchronize_scroll;
                            self.scroll_sync.reset();
                        }
                        ui.separator();
                        for mode in [ViewMode::Source, ViewMode::Split, ViewMode::Preview] {
                            if ui
                                .selectable_label(self.view_mode == mode, mode.label())
                                .clicked()
                            {
                                self.view_mode = mode;
                            }
                        }
                    });
                });
            });
    }

    fn search_bar(&mut self, ctx: &egui::Context) {
        if !self.search_open {
            return;
        }

        egui::TopBottomPanel::top("search_bar")
            .exact_height(66.0)
            .frame(
                egui::Frame::none()
                    .fill(theme::PAPER_DEEP)
                    .inner_margin(egui::Margin::symmetric(18.0, 8.0))
                    .stroke(egui::Stroke::new(1.0, theme::RULE)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("FIND")
                            .size(11.0)
                            .strong()
                            .color(theme::MUTED),
                    );
                    let response = ui.add_sized(
                        [300.0, 30.0],
                        egui::TextEdit::singleline(&mut self.search_query)
                            .hint_text("Search this document")
                            .margin(Vec2::new(8.0, 6.0)),
                    );
                    if self.focus_search {
                        response.request_focus();
                        self.focus_search = false;
                    }

                    let count = self.result_count();
                    ui.label(
                        RichText::new(if count == 0 {
                            "No matches".to_owned()
                        } else {
                            format!("{} of {}", self.active_hit + 1, count)
                        })
                        .size(12.0)
                        .color(theme::MUTED),
                    );
                    if ui
                        .add_enabled(count > 0, egui::Button::new("Previous"))
                        .clicked()
                    {
                        self.previous_hit();
                    }
                    if ui
                        .add_enabled(count > 0, egui::Button::new("Next"))
                        .clicked()
                    {
                        self.next_hit();
                    }
                    if ui.button("Close").clicked() {
                        self.search_open = false;
                    }
                });

                if self.view_mode != ViewMode::Source {
                    if let Some(hit) = self.search_hits.get(self.active_hit) {
                        ui.label(
                            RichText::new(format!("…{}…", hit.snippet))
                                .size(11.0)
                                .color(theme::MUTED),
                        );
                    }
                }
            });
    }

    fn result_count(&self) -> usize {
        if self.search_query.trim().is_empty() {
            return 0;
        }
        if self.view_mode == ViewMode::Source {
            let haystack = self.document.content.to_lowercase();
            let needle = self.search_query.to_lowercase();
            haystack.match_indices(&needle).count()
        } else {
            self.search_hits.len()
        }
    }

    fn next_hit(&mut self) {
        let count = self.result_count();
        if count == 0 {
            return;
        }
        self.active_hit = (self.active_hit + 1) % count;
        if self.view_mode != ViewMode::Source {
            self.preview_target = self
                .search_hits
                .get(self.active_hit)
                .map(|hit| hit.section_index);
            self.scroll_sync.reset();
        }
    }

    fn previous_hit(&mut self) {
        let count = self.result_count();
        if count == 0 {
            return;
        }
        self.active_hit = if self.active_hit == 0 {
            count - 1
        } else {
            self.active_hit - 1
        };
        if self.view_mode != ViewMode::Source {
            self.preview_target = self
                .search_hits
                .get(self.active_hit)
                .map(|hit| hit.section_index);
            self.scroll_sync.reset();
        }
    }

    fn outline_panel(&mut self, ctx: &egui::Context) {
        if !self.outline_open || self.view_mode == ViewMode::Source || self.document.is_empty() {
            return;
        }

        egui::SidePanel::left("document_outline")
            .default_width(230.0)
            .width_range(180.0..=320.0)
            .resizable(true)
            .frame(
                egui::Frame::none()
                    .fill(theme::PAPER_DEEP)
                    .inner_margin(egui::Margin::symmetric(13.0, 16.0))
                    .stroke(egui::Stroke::new(1.0, theme::RULE)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("CONTENTS")
                            .size(11.0)
                            .strong()
                            .color(theme::BRASS),
                    );
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Hide").clicked() {
                            self.outline_open = false;
                        }
                    });
                });
                ui.add_space(10.0);

                if self.outline.is_empty() {
                    ui.label(
                        RichText::new("This document has no headings.")
                            .size(13.0)
                            .color(theme::MUTED),
                    );
                    return;
                }

                let sections = markdown::sections(&self.document.content, &self.outline);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (heading_index, heading) in self.outline.iter().enumerate() {
                        let indent = (heading.level.saturating_sub(1) as f32) * 12.0;
                        ui.horizontal(|ui| {
                            ui.add_space(indent);
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(&heading.title)
                                            .size(if heading.level == 1 { 14.0 } else { 12.5 })
                                            .strong_if(heading.level <= 2),
                                    )
                                    .frame(false),
                                )
                                .clicked()
                            {
                                self.preview_target = sections.iter().position(|section| {
                                    section.heading_index == Some(heading_index)
                                });
                                self.scroll_sync.reset();
                            }
                        });
                    }
                });
            });
    }

    fn workspace(&mut self, ctx: &egui::Context) {
        if self.document.is_empty() {
            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(theme::PAPER))
                .show(ctx, |ui| self.welcome(ui));
            return;
        }

        let compact = ctx.available_rect().width() < 820.0;
        let effective_mode = if compact && self.view_mode == ViewMode::Split {
            ViewMode::Source
        } else {
            self.view_mode
        };

        if effective_mode == ViewMode::Split {
            let available_width = ctx.available_rect().width();
            egui::SidePanel::left("source_panel")
                .default_width(available_width * 0.5)
                .width_range(split_source_width_range(available_width))
                .resizable(true)
                .frame(
                    egui::Frame::none()
                        .fill(theme::EDITOR_BG)
                        .inner_margin(egui::Margin::symmetric(14.0, 14.0))
                        .stroke(egui::Stroke::new(1.0, theme::RULE)),
                )
                .show(ctx, |ui| self.editor(ui, true));
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(if effective_mode == ViewMode::Source {
                        theme::EDITOR_BG
                    } else {
                        theme::PAPER
                    })
                    .inner_margin(egui::Margin::same(0.0)),
            )
            .show(ctx, |ui| match effective_mode {
                ViewMode::Source => {
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        ui.vertical(|ui| self.editor(ui, false));
                    });
                }
                ViewMode::Preview | ViewMode::Split => {
                    self.preview(ui, effective_mode == ViewMode::Split)
                }
            });
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        let height = ui.available_height();
        ui.add_space((height * 0.18).min(130.0));
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new("N / M")
                    .size(13.0)
                    .strong()
                    .color(theme::BRASS),
            );
            ui.add_space(18.0);
            ui.label(
                RichText::new("Read Markdown without the machinery.")
                    .size(30.0)
                    .strong(),
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    "A quiet native reader. Editing stays out of the way until you need it.",
                )
                .size(15.0)
                .color(theme::MUTED),
            );
            ui.add_space(28.0);
            if ui
                .add_sized(
                    [190.0, 38.0],
                    egui::Button::new(RichText::new("Open document").strong())
                        .fill(theme::BRASS_SOFT)
                        .stroke(egui::Stroke::new(1.0, theme::BRASS)),
                )
                .clicked()
            {
                self.queue(Command::Action(PendingAction::OpenDialog));
            }
            ui.add_space(6.0);
            if let Some(path) = self.last_path.clone().filter(|path| path.is_file()) {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("last document");
                if ui.button(format!("Reopen {name}")).clicked() {
                    self.queue(Command::Action(PendingAction::OpenPath(path)));
                }
            }
            if self.recovery_available && ui.button("Recover unsaved draft").clicked() {
                match Document::recover() {
                    Ok(document) => {
                        self.document = document;
                        self.view_mode = ViewMode::Split;
                        self.recovery_available = false;
                        self.set_notice("Recovered unsaved draft", false);
                        self.refresh_analysis();
                    }
                    Err(error) => {
                        self.set_notice(format!("Could not recover draft: {error}"), true)
                    }
                }
            }
            ui.add_space(22.0);
            ui.label(
                RichText::new("Drop a .md file anywhere  ·  Ctrl+O to open")
                    .size(12.0)
                    .color(theme::MUTED),
            );
        });
    }

    fn editor(&mut self, ui: &mut egui::Ui, in_split_view: bool) {
        let query = if self.search_open {
            self.search_query.clone()
        } else {
            String::new()
        };
        let viewport_height = ui.available_height();
        let editor_height = source_editor_height(&self.document.content, viewport_height);
        let mut layouter = move |ui: &egui::Ui, text: &str, wrap_width: f32| {
            let job = editor_layout(text, &query, wrap_width);
            ui.fonts(|fonts| fonts.layout_job(job))
        };

        let sync_from_preview = in_split_view
            && self.synchronize_scroll
            && self.scroll_sync.driver() == Some(ScrollPane::Preview);
        let mut scroll_area = egui::ScrollArea::vertical()
            .id_salt("source_scroll")
            .auto_shrink([false, false]);
        if sync_from_preview {
            scroll_area = scroll_area.vertical_scroll_offset(self.scroll_sync.target_offset(
                self.source_scroll.content_height,
                self.source_scroll.viewport_height,
            ));
        }

        let output = scroll_area.show(ui, |ui| {
            ui.add_sized(
                [ui.available_width(), editor_height],
                egui::TextEdit::multiline(&mut self.document.content)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .margin(Vec2::new(12.0, 12.0))
                    .frame(false)
                    .layouter(&mut layouter),
            )
            .changed()
        });

        let previous_offset = self.source_scroll.offset;
        self.source_scroll = ScrollMetrics::new(
            output.state.offset.y,
            output.content_size.y,
            output.inner_rect.height(),
        );
        if in_split_view
            && self.synchronize_scroll
            && pane_was_scrolled(
                ui.ctx(),
                output.inner_rect,
                previous_offset,
                self.source_scroll.offset,
            )
        {
            self.scroll_sync
                .update_from(ScrollPane::Source, self.source_scroll);
            ui.ctx().request_repaint();
        }
        if output.inner {
            self.markdown_cache = CommonMarkCache::default();
        }
    }

    fn preview(&mut self, ui: &mut egui::Ui, in_split_view: bool) {
        let sections = markdown::sections(&self.document.content, &self.outline);
        let target = self.preview_target.take();
        if target.is_some() {
            self.scroll_sync.reset();
        }
        let base_uri = self
            .document
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(file_uri_base);

        let sync_from_source = in_split_view
            && self.synchronize_scroll
            && target.is_none()
            && self.scroll_sync.driver() == Some(ScrollPane::Source);
        let mut scroll_area = egui::ScrollArea::vertical()
            .id_salt("preview_scroll")
            .auto_shrink([false, false]);
        if sync_from_source {
            scroll_area = scroll_area.vertical_scroll_offset(self.scroll_sync.target_offset(
                self.preview_scroll.content_height,
                self.preview_scroll.viewport_height,
            ));
        }

        let output = scroll_area.show(ui, |ui| {
            let available = ui.available_width();
            let content_width = available.min(READING_WIDTH);
            let gutter = ((available - content_width) / 2.0).max(18.0);
            ui.horizontal(|ui| {
                ui.add_space(gutter);
                ui.vertical(|ui| {
                    ui.set_width(content_width - gutter.min(18.0));
                    ui.add_space(34.0);
                    for (section_index, section) in sections.iter().enumerate() {
                        let anchor = ui.allocate_response(
                            Vec2::new(ui.available_width(), 1.0),
                            Sense::hover(),
                        );
                        if target == Some(section_index) {
                            anchor.scroll_to_me(Some(Align::Min));
                        }
                        let source = markdown::safe_preview_source(
                            &self.document.content[section.range.clone()],
                        );
                        let mut viewer = CommonMarkViewer::new()
                            .max_image_width(Some(ui.available_width() as usize))
                            .show_alt_text_on_hover(true);
                        if let Some(base_uri) = &base_uri {
                            viewer = viewer.default_implicit_uri_scheme(base_uri.clone());
                        }
                        viewer.show(ui, &mut self.markdown_cache, &source);
                        ui.add_space(9.0);
                    }
                    ui.add_space(70.0);
                });
            });
        });

        let previous_offset = self.preview_scroll.offset;
        self.preview_scroll = ScrollMetrics::new(
            output.state.offset.y,
            output.content_size.y,
            output.inner_rect.height(),
        );
        if in_split_view
            && self.synchronize_scroll
            && pane_was_scrolled(
                ui.ctx(),
                output.inner_rect,
                previous_offset,
                self.preview_scroll.offset,
            )
        {
            self.scroll_sync
                .update_from(ScrollPane::Preview, self.preview_scroll);
            ui.ctx().request_repaint();
        }
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        if self
            .notice
            .as_ref()
            .is_some_and(|notice| notice.created_at.elapsed() > Duration::from_secs(5))
        {
            self.notice = None;
        }
        let words = markdown::word_count(&self.document.content);
        let minutes = markdown::reading_minutes(words);

        egui::TopBottomPanel::bottom("status_bar")
            .exact_height(30.0)
            .frame(
                egui::Frame::none()
                    .fill(theme::PAPER_DEEP)
                    .inner_margin(egui::Margin::symmetric(14.0, 5.0))
                    .stroke(egui::Stroke::new(1.0, theme::RULE)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(notice) = &self.notice {
                        let color = if notice.is_error {
                            Color32::from_rgb(155, 57, 42)
                        } else {
                            theme::MUTED
                        };
                        ui.label(RichText::new(&notice.text).size(11.5).color(color));
                    } else {
                        let state = if self.document.is_dirty() {
                            "Unsaved changes · recovery on"
                        } else {
                            "Saved"
                        };
                        ui.label(RichText::new(state).size(11.5).color(theme::MUTED));
                    }
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!(
                                "{} words  ·  {} min read  ·  {}  ·  {}%",
                                words,
                                minutes,
                                self.view_mode.label(),
                                self.zoom.percent()
                            ))
                            .size(11.5)
                            .color(theme::MUTED),
                        );
                    });
                });
            });
    }

    fn unsaved_dialog(&mut self, ctx: &egui::Context) {
        let Some(action) = self.pending_action.clone() else {
            return;
        };

        enum Decision {
            Save,
            Discard,
            Cancel,
        }
        let mut decision = None;
        egui::Window::new("Unsaved changes")
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .fixed_size([410.0, 150.0])
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("Keep the changes to this document?")
                        .size(17.0)
                        .strong(),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "A recovery copy exists, but your file has not been overwritten.",
                    )
                    .size(13.0)
                    .color(theme::MUTED),
                );
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    if ui.button("Save and continue").clicked() {
                        decision = Some(Decision::Save);
                    }
                    if ui.button("Discard changes").clicked() {
                        decision = Some(Decision::Discard);
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(Decision::Cancel);
                    }
                });
            });

        match decision {
            Some(Decision::Save) if self.save_current() => {
                self.pending_action = None;
                match action {
                    PendingAction::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                    _ => self.execute_action(action),
                }
            }
            Some(Decision::Save) => {}
            Some(Decision::Discard) => {
                self.pending_action = None;
                Document::clear_recovery();
                match action {
                    PendingAction::Close => {
                        self.document = Document::default();
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    _ => self.execute_action(action),
                }
            }
            Some(Decision::Cancel) => self.pending_action = None,
            None => {}
        }
    }

    fn update_window_title(&mut self, ctx: &egui::Context) {
        let marker = if self.document.is_dirty() { " •" } else { "" };
        let title = format!(
            "{}{} — Native Markdown",
            self.document.display_name(),
            marker
        );
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }
}

impl eframe::App for NativeMarkdownApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_zoom_gesture(ctx);
        self.handle_shortcuts(ctx);
        self.handle_dropped_files(ctx);

        if ctx.input(|input| input.viewport().close_requested()) && self.document.is_dirty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.pending_action.is_none() {
                self.pending_action = Some(PendingAction::Close);
            }
        }

        self.sync_analysis();
        self.header(ctx);
        self.search_bar(ctx);
        self.status_bar(ctx);
        self.outline_panel(ctx);
        self.workspace(ctx);
        self.unsaved_dialog(ctx);

        self.sync_analysis();
        if let Err(error) = self.document.maybe_write_recovery() {
            if self.notice.as_ref().is_none_or(|notice| !notice.is_error) {
                self.set_notice(format!("Recovery copy failed: {error}"), true);
            }
        }
        self.recovery_available = Document::recovery_exists();

        if let Some(command) = self.queued_command.take() {
            self.process_command(command, ctx);
        }

        let modifiers = ctx.input(|input| input.modifiers);
        if ctx.output(|output| output.open_url.is_some()) && !(modifiers.ctrl || modifiers.command)
        {
            ctx.output_mut(|output| output.open_url = None);
            self.set_notice("Hold Ctrl while clicking to open links", false);
        }

        self.update_window_title(ctx);
        if self.document.is_dirty() {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Some(path) = self.document.path.as_deref().or(self.last_path.as_deref()) {
            storage.set_string(LAST_PATH_KEY, path.to_string_lossy().into_owned());
        }
        storage.set_string(ZOOM_KEY, self.zoom.factor().to_string());
    }
}

fn pane_was_scrolled(
    ctx: &egui::Context,
    pane_rect: egui::Rect,
    previous_offset: f32,
    current_offset: f32,
) -> bool {
    if (current_offset - previous_offset).abs() <= 0.5 {
        return false;
    }

    ctx.input(|input| {
        let pointer_is_over = input
            .pointer
            .hover_pos()
            .is_some_and(|position| pane_rect.expand(16.0).contains(position));
        pointer_is_over
            && (input.raw_scroll_delta.y.abs() > f32::EPSILON || input.pointer.primary_down())
    })
}

fn source_editor_height(source: &str, viewport_height: f32) -> f32 {
    ((source.lines().count() + 2) as f32 * 21.0 + 24.0).max(viewport_height)
}

fn split_source_width_range(available_width: f32) -> std::ops::RangeInclusive<f32> {
    MIN_SPLIT_PANE_WIDTH..=(available_width - MIN_SPLIT_PANE_WIDTH).max(MIN_SPLIT_PANE_WIDTH)
}

fn menu_item(ui: &mut egui::Ui, label: &str, shortcut: &str) -> bool {
    ui.horizontal(|ui| {
        let clicked = ui.button(label).clicked();
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(shortcut).size(11.0).color(theme::MUTED));
        });
        clicked
    })
    .inner
}

fn editor_layout(text: &str, query: &str, wrap_width: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = wrap_width;
    let base = TextFormat {
        font_id: FontId::new(14.0, FontFamily::Monospace),
        color: theme::INK,
        line_height: Some(21.0),
        ..Default::default()
    };

    let query = query.trim();
    if query.is_empty() {
        job.append(text, 0.0, base);
        return job;
    }

    let lowercase = text.to_lowercase();
    let needle = query.to_lowercase();
    if lowercase.len() != text.len() || needle.len() != query.len() {
        job.append(text, 0.0, base);
        return job;
    }

    let mut cursor = 0;
    for (start, _) in lowercase.match_indices(&needle) {
        job.append(&text[cursor..start], 0.0, base.clone());
        let end = start + needle.len();
        let mut matched = base.clone();
        matched.background = theme::BRASS_SOFT;
        matched.color = theme::INK;
        job.append(&text[start..end], 0.0, matched);
        cursor = end;
    }
    job.append(&text[cursor..], 0.0, base);
    job
}

fn file_uri_base(path: &Path) -> String {
    let mut path = path.to_string_lossy().replace('\\', "/");
    if !path.ends_with('/') {
        path.push('/');
    }
    format!("file:///{path}")
}

trait RichTextStrength {
    fn strong_if(self, condition: bool) -> Self;
}

impl RichTextStrength for RichText {
    fn strong_if(self, condition: bool) -> Self {
        if condition {
            self.strong()
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_source_content_exceeds_the_editor_viewport() {
        let source = (0..200)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(source_editor_height(&source, 500.0) > 4_000.0);
    }

    #[test]
    fn short_source_content_still_fills_the_editor_viewport() {
        assert_eq!(source_editor_height("# Short", 500.0), 500.0);
    }

    #[test]
    fn split_divider_can_reach_the_center_of_a_wide_window() {
        let available_width = 1_800.0;
        let limits = split_source_width_range(available_width);

        assert!(*limits.end() >= available_width / 2.0);
    }

    #[test]
    fn split_divider_always_leaves_room_for_the_preview() {
        let available_width = 900.0;
        let limits = split_source_width_range(available_width);

        assert!(*limits.end() <= available_width - MIN_SPLIT_PANE_WIDTH);
    }
}
