use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use crate::sidebar_hint::{self, HintBounds};
use crate::source_search::SourceSearch;
use gpui::{
    actions, div, prelude::*, px, rgb, AnyElement, App, AppContext, Context, Entity, ExternalPaths,
    FocusHandle, Focusable, IntoElement, KeyBinding, PromptButton, PromptLevel, Render,
    ScrollEvent, ScrollHandle, SharedString, Subscription, Window,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Editor, EditorState, Escape, Input, InputEvent, InputState};
#[cfg(not(windows))]
use gpui_component::menu::ContextMenuExt as _;
#[cfg(not(windows))]
use gpui_component::menu::DropdownMenu as _;
use gpui_component::resizable::{h_resizable, resizable_panel, ResizableState};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::tooltip::Tooltip;
#[cfg(test)]
use gpui_component::Theme;
use gpui_component::{
    Disableable as _, ElementExt as _, Icon, Selectable as _, Sizable as _, StyledExt as _,
};
use rfd::AsyncFileDialog;

use crate::appearance::{Appearance, Palette};
use crate::benchmark::BenchmarkScenario;
use crate::document::{Document, SaveOutcome};
use crate::file_tree::{self, EntryKind, FileTree, VisibleRow};
use crate::layout_settings::LayoutSettings;
use crate::markdown::{self, Heading, SearchHit};
use crate::zoom::ZoomLevel;

const APP_CONTEXT: &str = "NativeMarkdown";
const FILE_TREE_CONTEXT: &str = "NativeMarkdownFileTree";
const BASE_MONO_FONT_SIZE: f32 = 13.0;
const FILE_TREE_HIDE_BELOW: f32 = 760.0;
const OUTLINE_HIDE_BELOW: f32 = 1000.0;

// Inline vectors keep toolbar icons crisp at every DPI without an external asset loader.
fn tool_icon(paths: &str) -> Icon {
    Icon::default().data(format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">{paths}</svg>"#
    ).as_bytes()).size_4()
}

fn tool_button(id: &'static str, label: &'static str, paths: &str) -> Button {
    Button::new(id)
        .icon(tool_icon(paths))
        .accessibility_label(label)
        .tooltip(label)
        .small()
        .ghost()
        .border_0()
        .rounded_md()
        .w(px(28.0))
        .h(px(28.0))
}

// Native caption hit testing retains Windows snapping, system actions and the
// existing on_window_should_close guard. Inline SVGs need no icon asset loader.
fn caption_control(
    id: &'static str,
    label: &'static str,
    paths: &str,
    area: gpui::WindowControlArea,
    palette: Palette,
) -> impl IntoElement {
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(label)
        .window_control_area(area)
        .h_full()
        .w(px(38.0))
        .flex_none()
        .h_flex()
        .items_center()
        .justify_center()
        .text_color(rgb(palette.text))
        .hover(move |style| {
            style
                .bg(rgb(if id == "window-close" {
                    0xc42b1c
                } else {
                    palette.hover
                }))
                .text_color(rgb(if id == "window-close" {
                    0xffffff
                } else {
                    palette.text
                }))
        })
        .child(tool_icon(paths))
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
        PastePlainText,
        FindDocument,
        NextSearchHit,
        PreviousSearchHit,
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
        KeyBinding::new("enter", NextSearchHit, Some("NativeMarkdownSearch")),
        KeyBinding::new(
            "shift-enter",
            PreviousSearchHit,
            Some("NativeMarkdownSearch"),
        ),
        KeyBinding::new("ctrl-n", NewDocument, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-o", OpenDocument, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-s", SaveDocument, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-shift-s", SaveDocumentAs, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-shift-v", PastePlainText, Some("NativeMarkdownSource")),
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

struct PendingPaste {
    id: u64,
    range: std::ops::Range<usize>,
    cancelled: Arc<AtomicBool>,
    prepared: Option<crate::paste::Import>,
}
impl Drop for PendingPaste {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
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
    #[cfg(windows)]
    web_preview: Option<Rc<std::cell::RefCell<crate::web_preview::WebPreview>>>,
    #[cfg(windows)]
    web_error: Option<String>,
    #[cfg(windows)]
    web_starting: bool,
    appearance: Appearance,
    markdown_dialect: markdown::Dialect,
    preserve_line_breaks: bool,
    reading_width: u16,
    reading_width_follows_zoom: bool,
    settings_open: bool,
    settings_section: usize,
    download_timeouts: crate::paste::DownloadTimeouts,
    document: Document,
    recent: crate::recent::Recent,
    recent_folder_pending: Option<PathBuf>,
    editor: Entity<EditorState>,
    search_input: Entity<InputState>,
    outline: Vec<Heading>,
    word_count: usize,
    reading_minutes: usize,
    search_hits: Vec<SearchHit>,
    search_query: String,
    active_hit: usize,
    active_search_offset: Option<usize>,
    search_request: u64,
    source_search: SourceSearch,
    scroll_sync: crate::scroll_sync::ScrollSync,
    sync_scroll_enabled: bool,
    app_focus: FocusHandle,
    view_mode: ViewMode,
    file_tree: FileTree,
    file_tree_focus: FocusHandle,
    tree_context_path: Option<PathBuf>,
    file_tree_open: bool,
    outline_open: bool,
    file_tree_width: f32,
    file_tree_hint_bounds: HintBounds,
    outline_width: f32,
    file_tree_narrow_reveal: bool,
    outline_narrow_reveal: bool,
    workspace_resizable: Entity<ResizableState>,
    workspace_panel_visibility: Option<(bool, bool)>,
    search_open: bool,
    preview_markdown: SharedString,
    pending_analysis: Option<gpui::Task<()>>,
    preview_refresh_delay_ms: u64,
    focused_section: Option<usize>,
    selected_heading: Option<usize>,
    // None means no observation yet; Some(None) is the introduction before any heading.
    reading_heading: Option<Option<usize>>,
    outline_scroll: ScrollHandle,
    outline_revision: u64,
    outline_mode: OutlineMode,
    outline_jump_request: u64,
    zoom: ZoomLevel,
    last_path: Option<PathBuf>,
    recovery_available: bool,
    notice: Option<Notice>,
    dialog_in_flight: bool,
    pending_paste: Option<PendingPaste>,
    paste_request: u64,
    _subscriptions: Vec<Subscription>,
}

impl NativeMarkdownApp {
    fn state_button(&self, button: Button, selected: bool) -> Button {
        button
            .ghost()
            .selected(selected)
            .border_0()
            .rounded_md()
            .when(selected, |button| {
                button
                    .bg(rgb(self.palette().selection))
                    .text_color(rgb(self.palette().accent))
            })
            .when(!selected, |button| {
                button.text_color(rgb(self.palette().muted))
            })
    }

    fn palette(&self) -> Palette {
        self.appearance.palette()
    }

    fn set_appearance(&mut self, appearance: Appearance, cx: &mut Context<Self>) {
        self.appearance = appearance;
        appearance.apply(cx);
        self.persist_layout();
        cx.notify();
    }

    fn preview_refresh_settings(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette();
        div()
            .v_flex()
            .gap_3()
            .mt_3()
            .child(div().font_semibold().child("Preview refresh · 预览刷新"))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(p.muted))
                    .child("停止输入后等待多久更新预览和目录。"),
            )
            .child(
                div().h_flex().gap_2().flex_wrap().children(
                    [
                        (500u64, "0.5 秒"),
                        (1000, "1 秒"),
                        (2000, "2 秒"),
                        (3000, "3 秒"),
                        (5000, "5 秒"),
                    ]
                    .into_iter()
                    .map(|(delay_ms, label)| {
                        Button::new(("preview-refresh-delay", delay_ms as usize))
                            .label(label)
                            .small()
                            .selected(self.preview_refresh_delay_ms == delay_ms)
                            .debug_selector(move || format!("preview-refresh-delay-{delay_ms}"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_preview_refresh_delay(delay_ms, cx);
                            }))
                    }),
                ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(p.muted))
                    .child("默认 1 秒。当前仅本次运行生效。"),
            )
    }

    fn set_markdown_settings(
        &mut self,
        dialect: markdown::Dialect,
        preserve_line_breaks: bool,
        cx: &mut Context<Self>,
    ) {
        if self.markdown_dialect == dialect && self.preserve_line_breaks == preserve_line_breaks {
            return;
        }
        self.markdown_dialect = dialect;
        self.preserve_line_breaks = preserve_line_breaks;
        self.refresh_analysis();
        cx.notify();
    }

    fn markdown_settings(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div().v_flex().gap_3().mt_3()
            .child(div().font_semibold().child("Markdown · 方言与换行"))
            .child(div().h_flex().gap_2().children([
                (markdown::Dialect::GitHub, "GitHub (GFM)"),
                (markdown::Dialect::CommonMark, "CommonMark"),
            ].into_iter().map(|(dialect, label)| {
                Button::new(("markdown-dialect", dialect as usize)).label(label).small()
                    .selected(self.markdown_dialect == dialect)
                    .debug_selector(move || format!("markdown-dialect-{}", dialect.id()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_markdown_settings(dialect, this.preserve_line_breaks, cx);
                    }))
            })))
            .child(Button::new("markdown-preserve-line-breaks").label("保留普通换行")
                .selected(self.preserve_line_breaks)
                .debug_selector(|| "markdown-preserve-line-breaks".into())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.set_markdown_settings(this.markdown_dialect, !this.preserve_line_breaks, cx);
                })))
            .child(div().text_xs().text_color(rgb(self.palette().muted))
                .child("默认 GFM；普通回车按段落合并。开启保留换行后，普通回车也会显示为换行。"))
            .child(div().text_xs().text_color(rgb(self.palette().muted))
                .child("GFM 提供表格、任务列表、删除线和脚注；两种模式均保留公式与 Mermaid。仅本次运行生效。"))
    }

    fn download_settings(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let options = self.download_timeouts;
        div().v_flex().gap_4()
            .child(div().font_semibold().child("图片下载超时"))
            .child(div().text_sm().text_color(rgb(self.palette().muted))
                .child("后台下载有明确期限；失败处保留原因和可用的图片来源链接。"))
            .children([
                (0usize, "连接超时", "建立网络连接的等待上限。", options.connect_secs, vec![3, 5, 10, 15, 30]),
                (1, "单张图片超时", "包含连接和响应体下载的总时间。", options.image_secs, vec![5, 10, 20, 30, 60]),
                (2, "整次粘贴超时", "所有网络图片共享的期限，包含排队时间。", options.batch_secs, vec![10, 20, 30, 60, 120]),
            ].into_iter().map(|(kind, title, hint, current, values)| {
                div().v_flex().gap_2()
                    .child(div().font_semibold().child(title))
                    .child(div().text_sm().text_color(rgb(self.palette().muted)).child(hint))
                    .child(div().h_flex().gap_2().flex_wrap().children(values.into_iter().map(|seconds| {
                        Button::new(("download-timeout", kind * 1000 + seconds as usize))
                            .label(format!("{seconds} 秒")).small().selected(current == seconds)
                            .debug_selector(move || format!("download-timeout-{kind}-{seconds}"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                match kind {
                                    0 => this.download_timeouts.connect_secs = seconds,
                                    1 => this.download_timeouts.image_secs = seconds,
                                    _ => this.download_timeouts.batch_secs = seconds,
                                }
                                cx.notify();
                            }))
                    })))
            }))
            .child(div().text_xs().text_color(rgb(self.palette().muted))
                .child("默认：连接 5 秒 / 单张 10 秒 / 整次 20 秒。实际等待取各级剩余期限中较短者；不自动重试。"))
            .child(div().text_xs().text_color(rgb(self.palette().muted))
                .child("仅本次运行生效，用于之后开始的图片导入；正在下载的任务保持原期限。"))
            .child(Button::new("reset-download-timeouts").label("恢复默认超时").small()
                .debug_selector(|| "reset-download-timeouts".into())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.download_timeouts = crate::paste::DownloadTimeouts::default();
                    cx.notify();
                })))
    }

    fn settings_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette();
        let content = match self.settings_section {
            1 => div()
                .v_flex()
                .gap_4()
                .child(self.markdown_settings(cx))
                .child(self.preview_refresh_settings(cx)),
            2 => div().v_flex().gap_4().child(self.download_settings(cx)),
            _ => div()
                .v_flex()
                .gap_4()
                .child(
                    div()
                        .v_flex()
                        .gap_1()
                        .child(div().font_semibold().child("Appearance · 外观"))
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(p.muted))
                                .child("Choose a theme for the app, editor and reading area."),
                        ),
                )
                .children(Appearance::ALL.into_iter().map(|appearance| {
                    let colors = appearance.palette();
                    let selected = appearance == self.appearance;
                    Button::new(appearance.id())
                        .accessibility_label(appearance.label())
                        .w_full()
                        .h(px(52.0))
                        .ghost()
                        .border_1()
                        .border_color(rgb(if selected { p.accent } else { p.border }))
                        .bg(rgb(if selected { p.selection } else { p.surface }))
                        .child(
                            div()
                                .h_flex()
                                .w_full()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .w(px(30.0))
                                        .h(px(30.0))
                                        .rounded_md()
                                        .border_1()
                                        .border_color(rgb(colors.border))
                                        .bg(rgb(colors.canvas))
                                        .text_color(rgb(colors.text))
                                        .child("Aa"),
                                )
                                .child(div().flex_1().child(appearance.label()))
                                .child(if selected { "✓" } else { "" }),
                        )
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.set_appearance(appearance, cx)),
                        )
                }))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(p.muted))
                        .child("Applied immediately · Saved for next time"),
                )
                .child(
                    div()
                        .v_flex()
                        .gap_3()
                        .mt_3()
                        .child(div().font_semibold().child("Reading width · 正文宽度"))
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(p.muted))
                                .child("100% 缩放时的宽度上限；始终适应阅读区。"),
                        )
                        .child(
                            div().h_flex().gap_2().flex_wrap().children(
                                [
                                    (720u16, "紧凑 · 720"),
                                    (980, "标准 · 980"),
                                    (1280, "宽 · 1280"),
                                    (0, "铺满"),
                                ]
                                .into_iter()
                                .map(|(width, label)| {
                                    Button::new(("reading-width", width as usize))
                                        .label(label)
                                        .small()
                                        .selected(self.reading_width == width)
                                        .debug_selector(move || format!("reading-width-{width}"))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.reading_width = width;
                                            cx.notify();
                                        }))
                                }),
                            ),
                        )
                        .child(
                            Button::new("reading-width-follow-zoom")
                                .label(if self.reading_width_follows_zoom {
                                    "✓ 宽度随放大比例扩展"
                                } else {
                                    "宽度随放大比例扩展"
                                })
                                .accessibility_label("宽度随放大比例扩展")
                                .selected(self.reading_width_follows_zoom)
                                .disabled(self.reading_width == 0)
                                .debug_selector(|| "reading-width-follow-zoom".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.reading_width_follows_zoom =
                                        !this.reading_width_follows_zoom;
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(p.muted))
                                .child("放大时增加宽度，最多铺满阅读区。当前仅本次运行生效。"),
                        ),
                ),
        };
        div()
            .id("settings-panel")
            .v_flex()
            .flex_1()
            .min_h_0()
            .p_6()
            .bg(rgb(p.canvas))
            .child(
                div()
                    .v_flex()
                    .w_full()
                    .max_w(px(640.0))
                    .mx_auto()
                    .flex_1()
                    .min_h_0()
                    .gap_4()
                    .child(
                        div()
                            .h_flex()
                            .flex_shrink_0()
                            .items_center()
                            .justify_between()
                            .child(div().text_xl().font_semibold().child("Settings · 设置"))
                            .child(
                                Button::new("close-settings")
                                    .label("Done")
                                    .small()
                                    .debug_selector(|| "close-settings".into())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings_open = false;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div().h_flex().flex_shrink_0().gap_2().children(
                            [
                                (0usize, "外观与阅读"),
                                (1, "Markdown 与预览"),
                                (2, "粘贴与下载"),
                            ]
                            .into_iter()
                            .map(|(section, label)| {
                                Button::new(("settings-section", section))
                                    .label(label)
                                    .small()
                                    .selected(self.settings_section == section)
                                    .debug_selector(move || format!("settings-section-{section}"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.settings_section = section;
                                        cx.notify();
                                    }))
                            }),
                        ),
                    )
                    .child(
                        div()
                            .id(("settings-content", self.settings_section))
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .child(content),
                    ),
            )
    }

    #[cfg(windows)]
    fn web_preview_allowed(&self, width: f32) -> bool {
        let (_, _, overlay_tree, overlay_outline) = self.responsive_panel_visibility(width);
        // Native dialogs have their own top-level window. Their in-flight flag guards
        // operations, but must not blank the reading surface behind them.
        self.view_mode != ViewMode::Source
            && !overlay_tree
            && !overlay_outline
            && !self.settings_open
    }
    #[cfg(windows)]
    fn sync_web_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if cfg!(test) || self.web_error.is_some() {
            return;
        }
        if self.document.is_empty() {
            // Dropping the last controller releases this application's browser process group.
            self.web_preview.take();
            return;
        }
        let (_, regular_outline, _, _) =
            self.responsive_panel_visibility(window.viewport_size().width.into());
        let visible = self.web_preview_allowed(window.viewport_size().width.into());
        let (source, offset) = self.preview_source();
        let sync_enabled = visible
            && self.view_mode == ViewMode::Split
            && self.sync_scroll_enabled
            && self.pending_analysis.is_none();
        self.scroll_sync.configure_with_dialect(
            sync_enabled,
            self.preview_markdown.clone(),
            offset..offset + source.len(),
            self.markdown_dialect,
        );
        if self.web_preview.is_none() && !self.web_starting && visible {
            use futures::StreamExt as _;
            self.web_starting = true;
            let (sender, mut receiver) = futures::channel::mpsc::unbounded();
            let app = cx.entity().downgrade();
            let initial_source = source.clone();
            let initial_path = self.document.path.clone();
            let initial_appearance = self.appearance;
            window.spawn(cx,async move |cx| {
                // WebView2 construction pumps native messages. Both the entity borrow AND the
                // outer GPUI AppCell borrow must be released before entering its message loop.
                cx.background_executor().timer(Duration::from_millis(1)).await;
                let Ok(parent)=cx.update(|window,_|crate::web_preview::PreviewParent::capture(window)) else {return;};
                // Deliberately outside cx.update: timers and cursor tasks may now borrow GPUI.
                let result=parent.and_then(|parent|crate::web_preview::WebPreview::new(&parent,initial_source,initial_path.as_deref(),initial_appearance,sender));
                let installed=cx.update(|_window,cx|app.update(cx,|app,cx| {
                    app.web_starting=false;
                    if app.document.is_empty() {return false;}
                    match result {
                        Ok(view)=>app.web_preview=Some(Rc::new(std::cell::RefCell::new(view))),
                        Err(error)=>{
                            app.web_error=Some(error.to_string());
                            app.set_notice(format!("WebView2 preview unavailable: {error}. Source editing and saving remain available."),true);
                        }
                    }
                    cx.notify();
                    app.web_preview.is_some()
                })).ok().and_then(Result::ok).unwrap_or(false);
                if !installed {return;}
                while let Some(message)=receiver.next().await {
                    if cx.update(|window,cx|app.update(cx,|app,cx|match message {
                        crate::web_preview::PreviewEvent::Message(message)=>app.web_message(&message,window,cx),
                        crate::web_preview::PreviewEvent::DroppedFile(path)=>app.request_document_action(DocumentAction::OpenPath(path),window,cx),
                    })).is_err() {break;}
                }
            }).detach();
        }
        if let Some(view) = &self.web_preview {
            let mut view = view.borrow_mut();
            view.set_allowed(visible);
            let heading = (self.outline_mode == OutlineMode::Jump)
                .then_some(self.selected_heading)
                .flatten();
            let preceding = self
                .search_hits
                .iter()
                .take_while(|hit| hit.source_offset.is_some_and(|position| position < offset))
                .count();
            let source_end = offset + source.len();
            view.update(source, self.document.path.as_deref(), offset, serde_json::json!({
                "markdownDialect":self.markdown_dialect.id(), "preserveLineBreaks":self.preserve_line_breaks,
                "zoom":self.zoom.factor(), "heading":heading, "navigation":self.outline_jump_request,
                "query":if self.search_open { self.search_query.as_str() } else { "" }, "occurrence":self.active_hit.saturating_sub(preceding), "searchRequest":self.search_request,
                "theme":self.appearance.reader_settings(),
                "readingWidth":self.reading_width,"widthFollowsZoom":self.reading_width_follows_zoom,
                "outline":{"enabled":visible && regular_outline && !self.outline.is_empty(),
                    "revision":self.outline_revision,"base":offset,"end":source_end},
                "sync":{"enabled":sync_enabled,"revision":self.scroll_sync.revision(),"base":offset},
                "searchOpen":self.search_open,
                "nativeSearchFocus":self.search_open && self.search_input.read(cx).focus_handle(cx).is_focused(window),
            }));
        }
    }

    fn active_outline_heading(&self) -> Option<usize> {
        if self.view_mode == ViewMode::Source {
            self.selected_heading
        } else {
            self.reading_heading.unwrap_or(self.selected_heading)
        }
    }

    #[cfg(windows)]
    fn apply_outline_position(
        &mut self,
        heading: Option<usize>,
        base: usize,
        end: usize,
        revision: u64,
    ) -> bool {
        if revision != self.outline_revision {
            return false;
        }
        let range = self
            .focused_section
            .and_then(|section| {
                markdown::focus_range(&self.preview_markdown, &self.outline, section)
            })
            .unwrap_or(0..self.preview_markdown.len());
        if base != range.start || end != range.end {
            return false;
        }
        let heading = match heading {
            Some(local_index) => {
                let first = self
                    .outline
                    .partition_point(|heading| heading.offset < base);
                let Some(index) = first.checked_add(local_index).filter(|index| {
                    self.outline
                        .get(*index)
                        .is_some_and(|heading| heading.offset < end)
                }) else {
                    return false;
                };
                Some(index)
            }
            None => None,
        };
        if self.reading_heading == Some(heading) {
            return false;
        }
        self.reading_heading = Some(heading);
        // The toolbar and Full document row precede the heading rows. The default
        // strategy moves only enough to reveal an offscreen row, without taking focus.
        self.outline_scroll
            .scroll_to_item(heading.map_or(1, |index| index + 2));
        true
    }

    #[cfg(windows)]
    fn web_message(&mut self, message: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(message) else {
            return;
        };
        match value["kind"].as_str() {
            Some("outline-position")
                if self.view_mode != ViewMode::Source
                    && self
                        .responsive_panel_visibility(window.viewport_size().width.into())
                        .1
                    && !self.settings_open
                    && !self.dialog_in_flight =>
            {
                let parse_offset = |name| {
                    value[name]
                        .as_u64()
                        .and_then(|value| usize::try_from(value).ok())
                };
                let heading = if value["heading"].is_null() {
                    Some(None)
                } else {
                    parse_offset("heading").map(Some)
                };
                if let (Some(heading), Some(base), Some(end), Some(revision)) = (
                    heading,
                    parse_offset("base"),
                    parse_offset("end"),
                    value["revision"].as_u64(),
                ) {
                    if self.apply_outline_position(heading, base, end, revision) {
                        cx.notify();
                    }
                }
            }
            Some("scroll-position")
                if self.view_mode == ViewMode::Split
                    && self.sync_scroll_enabled
                    && self.pending_analysis.is_none()
                    && !self.settings_open =>
            {
                if let (Some(start), Some(fraction), Some(edge), Some(revision)) = (
                    value["start"]
                        .as_u64()
                        .and_then(|value| usize::try_from(value).ok()),
                    value["fraction"].as_f64(),
                    value["edge"]
                        .as_i64()
                        .and_then(|value| i8::try_from(value).ok()),
                    value["revision"].as_u64(),
                ) {
                    if self.scroll_sync.receive(
                        crate::scroll_sync::Position {
                            start,
                            fraction: fraction as f32,
                            edge,
                        },
                        revision,
                    ) {
                        cx.notify();
                    }
                }
            }
            Some("document-readable") => {
                self.scroll_sync.realign();
                cx.notify();
            }

            Some("zoom-wheel") => {
                if let Some(delta) = value["delta"].as_f64().filter(|delta| delta.is_finite()) {
                    if self
                        .zoom
                        .apply_gesture((-delta.clamp(-1000., 1000.) as f32 * 0.0025).exp())
                    {
                        self.apply_zoom(window, cx);
                    }
                }
            }
            Some("document-link") => {
                let path = value["id"]
                    .as_str()
                    .and_then(|id| self.web_preview.as_ref()?.borrow().link_target(id));
                if let Some(path) = path {
                    self.request_document_action(DocumentAction::OpenPath(path), window, cx);
                }
            }
            Some("shortcut") => {
                let shift = value["shift"].as_bool().unwrap_or(false);
                if matches!(
                    value["key"].as_str(),
                    Some("o" | "n" | "f" | "e" | "1" | "2" | "3")
                ) || (shift && value["key"] == "s")
                {
                    if let Some(view) = &self.web_preview {
                        view.borrow().focus_parent();
                    }
                }
                match value["key"].as_str() {
                    Some("o") => {
                        self.request_document_action(DocumentAction::OpenDialog, window, cx)
                    }
                    Some("n") => self.request_document_action(DocumentAction::New, window, cx),
                    Some("s") if shift => self.save_as(None, window, cx),
                    Some("s") => self.save_current(window, cx),
                    Some("f") => self.show_search(window, cx),
                    Some("e") => self.on_toggle_preview(&TogglePreview, window, cx),
                    Some("1") => self.set_view_mode(ViewMode::Preview, cx),
                    Some("2") => self.set_view_mode(ViewMode::Split, cx),
                    Some("3") => self.set_view_mode(ViewMode::Source, cx),
                    Some("+" | "=") => self.zoom_in(window, cx),
                    Some("-") => self.zoom_out(window, cx),
                    Some("0") => self.reset_zoom(window, cx),
                    _ => {}
                }
            }
            Some("link") => {
                if let Some(url) = value["url"].as_str().filter(|url| {
                    url.starts_with("https://")
                        || url.starts_with("http://")
                        || url.starts_with("mailto:")
                }) {
                    cx.open_url(url);
                }
            }
            Some("ready") => {
                if let Some(view) = &self.web_preview {
                    view.borrow_mut().on_ready();
                    self.scroll_sync.realign();
                }
                // Wry's synchronous initialization pumps native messages. Retry any initial tree
                // scan whose completion arrived while GPUI was constructing the child HWND.
                self.refresh_file_tree(window, cx);
            }
            Some("search-applied")
                if self.search_open
                    && value["request"].as_u64() == Some(self.search_request)
                    && self.active_search_offset.is_some() =>
            {
                // The WebView selection may already have displaced logical as well as HWND
                // focus. The page only sends this acknowledgement for native-owned searches.
                self.focus_native_surface();
                self.search_input
                    .update(cx, |input, cx| input.focus(window, cx));
            }
            Some("search-step") if self.search_open => {
                if value["backward"].as_bool().unwrap_or(false) {
                    self.previous_hit(window, cx);
                } else {
                    self.next_hit(window, cx);
                }
            }
            Some("escape") if self.search_open => {
                if let Some(view) = &self.web_preview {
                    view.borrow().focus_parent();
                }
                self.close_search(window, cx);
            }
            _ => {} // No arbitrary invoke, path, script, file read or write command exists.
        }
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
            BenchmarkScenario::SwitchIdle => {
                let started = Instant::now();
                if step < 5 {
                    if let Some(path) = secondary_document {
                        let path = if step.is_multiple_of(2) {
                            std::env::args_os()
                                .nth(1)
                                .map(PathBuf::from)
                                .unwrap_or_else(|| path.to_path_buf())
                        } else {
                            path.to_path_buf()
                        };
                        self.open_path(path, window, cx);
                    }
                } else if step == 5 {
                    self.replace_document(Document::default(), window, cx);
                } else {
                    return;
                }
                println!(
                    "READER_SWITCH step={step} handler_ms={}",
                    started.elapsed().as_millis()
                );
            }
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
                if let Some(path) = secondary_document {
                    self.open_path(path.to_path_buf(), window, cx);
                    println!("NATIVE_MARKDOWN_BENCHMARK event=document_switched");
                }
            }
            BenchmarkScenario::ImageRelease => {}
        }
    }

    pub fn new(initial_path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (document, mut notice) = match initial_path.as_ref() {
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

        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("markdown")
                .line_number(true)
                .default_value(document.content.clone())
        });
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in document"));
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
        let subscriptions = vec![
            cx.subscribe_in(
                &editor,
                window,
                |this: &mut Self, editor, event: &InputEvent, _window, cx| {
                    if matches!(event, InputEvent::Change) {
                        let content = editor.read(cx).value().to_string();
                        // set_value during document replacement may emit a deferred Change.
                        if this.document.content == content {
                            return;
                        }
                        if let Some(pending) = &mut this.pending_paste {
                            if !crate::paste::rebase_selection(
                                &mut pending.range,
                                &this.document.content,
                                &content,
                            ) {
                                this.cancel_paste();
                                this.set_notice(
                                    "Paste cancelled: its insertion point was edited",
                                    false,
                                );
                            }
                        }
                        this.document.content = content;
                        this.source_search.clear();

                        this.schedule_analysis(cx);

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
                |this: &mut Self, input, event: &InputEvent, _window, cx| match event {
                    InputEvent::Change => {
                        this.search_query = input.read(cx).value().to_string();
                        this.flush_pending_analysis();
                        this.active_hit = 0;
                        this.active_search_offset = None;

                        this.source_search.clear();
                        this.refresh_search();
                        this.search_request = this.search_request.wrapping_add(1);
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => {}
                    InputEvent::Focus | InputEvent::Blur => {}
                },
            ),
            cx.observe_window_activation(window, |this: &mut Self, window, cx| {
                if window.is_window_active() {
                    this.refresh_file_tree(window, cx);
                }
            }),
        ];

        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
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

        let outline =
            markdown::headings_with_dialect(&document.content, markdown::Dialect::default());
        let word_count =
            markdown::word_count_with_dialect(&document.content, markdown::Dialect::default());
        let reading_minutes = markdown::reading_minutes(word_count);
        let last_path = document.path.clone().or(initial_path);
        let preview_markdown = SharedString::from(document.content.clone());

        #[cfg(not(test))]
        let mut recent = crate::recent::Recent::load();
        #[cfg(test)]
        let mut recent = crate::recent::Recent::default();
        if let Some(path) = document.path.clone() {
            if let Err(error) = recent.file(path) {
                notice = Some(Notice {
                    text: format!("Could not save recent history: {error}"),
                    is_error: true,
                    created_at: Instant::now(),
                });
            }
        }
        let mut app = Self {
            #[cfg(windows)]
            web_preview: None,
            #[cfg(windows)]
            web_error: None,
            #[cfg(windows)]
            web_starting: false,
            appearance: layout.appearance,
            reading_width: 980,
            reading_width_follows_zoom: true,
            settings_open: false,
            settings_section: 0,
            download_timeouts: crate::paste::DownloadTimeouts::default(),
            document,
            recent,
            recent_folder_pending: None,
            editor,
            search_input,
            outline,
            word_count,
            reading_minutes,
            search_hits: Vec::new(),
            search_query: String::new(),
            active_hit: 0,
            active_search_offset: None,
            search_request: 0,
            source_search: SourceSearch::default(),
            scroll_sync: crate::scroll_sync::ScrollSync::default(),
            sync_scroll_enabled: true,
            app_focus: cx.focus_handle(),
            view_mode: ViewMode::Preview,
            file_tree: FileTree::new(tree_root),
            file_tree_focus,
            tree_context_path: None,
            file_tree_open: layout.file_tree_open,
            outline_open: layout.outline_open,
            file_tree_width: layout.file_tree_width,
            file_tree_hint_bounds: HintBounds::default(),
            outline_width: layout.outline_width,
            file_tree_narrow_reveal: false,
            outline_narrow_reveal: false,
            workspace_resizable,
            workspace_panel_visibility: None,
            search_open: false,
            preview_markdown,
            pending_analysis: None,
            preview_refresh_delay_ms: 1000,
            markdown_dialect: markdown::Dialect::default(),
            preserve_line_breaks: false,
            focused_section: None,
            selected_heading: None,
            reading_heading: None,
            outline_scroll: ScrollHandle::new(),
            outline_revision: 0,
            outline_mode: OutlineMode::Jump,
            outline_jump_request: 0,
            zoom: ZoomLevel::default(),
            last_path,
            recovery_available: Document::recovery_exists(),
            notice,
            dialog_in_flight: false,
            pending_paste: None,
            paste_request: 0,
            _subscriptions: subscriptions,
        };
        if let Some(root) = app.file_tree.root().map(Path::to_path_buf) {
            app.load_tree_directory(root, window, cx);
        }

        app
    }

    pub fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.cancel_paste();
        if self.document.is_dirty() || self.dialog_in_flight {
            self.request_document_action(DocumentAction::CloseWindow, window, cx);
            false
        } else {
            Document::clear_recovery();
            true
        }
    }

    fn set_preview_refresh_delay(&mut self, delay_ms: u64, cx: &mut Context<Self>) {
        if self.preview_refresh_delay_ms == delay_ms {
            return;
        }
        self.preview_refresh_delay_ms = delay_ms;
        if self.pending_analysis.is_some() {
            self.schedule_analysis(cx);
        }
        cx.notify();
    }

    fn schedule_analysis(&mut self, cx: &mut Context<Self>) {
        // Dropping the previous task cancels its timer, so only the last edit publishes.
        self.pending_analysis = None;
        self.active_search_offset = None;
        self.active_hit = 0;
        if self.document.content.as_str() == self.preview_markdown.as_ref() {
            return;
        }
        let delay = Duration::from_millis(self.preview_refresh_delay_ms);
        self.pending_analysis = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                this.refresh_analysis();
                cx.notify();
            });
        }));
    }

    fn flush_pending_analysis(&mut self) {
        if self.pending_analysis.is_some() {
            self.refresh_analysis();
        }
    }

    fn refresh_analysis(&mut self) {
        self.pending_analysis = None;
        self.reading_heading = None;
        self.outline_revision = self.outline_revision.wrapping_add(1);
        self.active_search_offset = None;
        self.active_hit = 0;

        self.source_search.clear();
        self.preview_markdown = SharedString::from(self.document.content.clone());
        self.outline =
            markdown::headings_with_dialect(&self.document.content, self.markdown_dialect);
        self.word_count =
            markdown::word_count_with_dialect(&self.document.content, self.markdown_dialect);
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
            appearance: self.appearance,
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
        self.recent_folder_pending = root.clone();
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
        // Following a document's directory is implicit navigation, not another folder open.
        self.recent_folder_pending = None;
        self.file_tree.set_root(root);
        if let Some(root) = self.file_tree.root().map(Path::to_path_buf) {
            self.load_tree_directory(root, window, cx);
        }
        cx.notify();
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
        let scan = cx
            .background_executor()
            .spawn(async move { file_tree::scan_directory(&scan_path, show_hidden) });
        let app = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let result = scan.await;
                cx.update(|_, cx| {
                    app.update(cx, |app, cx| {
                        let succeeded = result.is_ok();
                        if app.file_tree.finish_load(&path, request, result) {
                            // A refresh may supersede the initial scan; consume the intent only
                            // when the current request completes, never on a stale scan result.
                            if app.recent_folder_pending.as_ref() == Some(&path) {
                                app.recent_folder_pending = None;
                                if succeeded {
                                    if let Err(error) = app.recent.folder(path.clone()) {
                                        app.set_notice(
                                            format!("Could not save recent history: {error}"),
                                            true,
                                        );
                                    }
                                }
                            }
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

        if self.pending_paste.is_none()
            && self
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
        self.search_hits = markdown::search_with_dialect(
            &self.document.content,
            &self.search_query,
            &self.outline,
            self.markdown_dialect,
        );
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
        self.cancel_paste();
        self.document = document;
        self.last_path.clone_from(&self.document.path);

        self.focused_section = None;
        self.selected_heading = None;
        self.search_open = false;
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
                self.remember_current_file();
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
                self.remember_current_file();
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
        self.save_existing(cx);
        cx.notify();
    }

    fn save_existing(&mut self, cx: &mut Context<Self>) -> bool {
        match self.document.save() {
            Ok(SaveOutcome::Written) => {
                self.last_path.clone_from(&self.document.path);

                self.show_saved_notice(cx);
                true
            }
            Ok(SaveOutcome::Unchanged) => true,
            Err(error) => {
                self.set_notice(format!("Could not save document: {error}"), true);
                false
            }
        }
    }

    fn save_before(&mut self, action: DocumentAction, window: &mut Window, cx: &mut Context<Self>) {
        if self.document.path.is_some() {
            if self.save_existing(cx) {
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
                            if app.cancel_paste() {
                                app.set_notice("Paste cancelled", false);
                            }
                            cx.notify();
                            return;
                        };
                        if path.extension().is_none() {
                            path.set_extension("md");
                        }
                        match app.document.save_as(path.clone()) {
                            Ok(()) => {
                                app.last_path = Some(path);

                                let should_reset_tree = app.file_tree.root().is_none_or(|root| {
                                    app.document
                                        .path
                                        .as_deref()
                                        .is_none_or(|document| !document.starts_with(root))
                                });
                                if should_reset_tree {
                                    app.reset_tree_to_document(window, cx);
                                }
                                app.show_saved_notice(cx);
                                app.remember_current_file();
                                app.resume_paste(window, cx);
                                if let Some(action) = continuation {
                                    app.perform_document_action(action, window, cx);
                                } else {
                                    cx.notify();
                                }
                            }
                            Err(error) => {
                                app.cancel_paste();
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

    fn remember_current_file(&mut self) {
        if let Some(path) = self.document.path.clone() {
            if let Err(error) = self.recent.file(path) {
                self.set_notice(format!("Could not save recent history: {error}"), true);
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

    fn show_saved_notice(&mut self, cx: &mut Context<Self>) {
        self.set_notice("Saved", false);
        let created_at = self.notice.as_ref().unwrap().created_at;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let _ = this.update(cx, |this, cx| {
                // A later save or error owns its own lifetime.
                if this
                    .notice
                    .as_ref()
                    .is_some_and(|notice| notice.created_at == created_at)
                {
                    this.notice = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn show_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_pending_analysis();
        self.focus_native_surface();
        self.search_open = true;
        self.search_input.update(cx, |input, cx| {
            input.focus(window, cx);
        });
        self.search_request = self.search_request.wrapping_add(1);
        if !self.search_query.is_empty() {
            self.activate_search_hit(window, cx);
        }
        cx.notify();
    }

    fn focus_native_surface(&self) {
        // GPUI's logical FocusHandle does not transfer Win32 focus from a child HWND.
        #[cfg(windows)]
        if let Some(preview) = &self.web_preview {
            preview.borrow().focus_parent();
        }
    }

    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = false;
        self.active_search_offset = None;
        self.source_search.dismiss(cx);
        self.search_request = self.search_request.wrapping_add(1);
        self.app_focus.focus(window, cx);
        cx.notify();
    }

    fn next_hit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_pending_analysis();
        if !self.search_hits.is_empty() {
            if self.active_search_offset.is_some() {
                self.active_hit = (self.active_hit + 1) % self.search_hits.len();
            }
            self.activate_search_hit(window, cx);
        }
    }

    fn previous_hit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_pending_analysis();
        if !self.search_hits.is_empty() {
            self.active_hit = if self.active_hit == 0 {
                self.search_hits.len() - 1
            } else {
                self.active_hit - 1
            };
            self.activate_search_hit(window, cx);
        }
    }

    fn activate_search_hit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(hit) = self.search_hits.get(self.active_hit) else {
            return;
        };
        self.active_search_offset = hit.source_offset;
        self.search_request = self.search_request.wrapping_add(1);

        self.source_search.clear();
        let range = hit.source_offset.map(|start| start..hit.source_end);
        self.source_search.set_match(range, self.search_request);
        if self.view_mode != ViewMode::Source {
            self.focused_section = None;
            self.selected_heading = markdown::sections(&self.document.content, &self.outline)
                .get(hit.section_index)
                .and_then(|section| section.heading_index);
        }
        cx.notify();
    }

    fn apply_zoom(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
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
        event: &ScrollEvent,
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
        if self.active_search_offset.is_some() {
            self.search_request = self.search_request.wrapping_add(1);

            self.source_search.clear();
            let range = self
                .search_hits
                .get(self.active_hit)
                .and_then(|hit| hit.source_offset.map(|start| start..hit.source_end));
            self.source_search.set_match(range, self.search_request);
        }
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
        self.cancel_paste();
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

    #[cfg(windows)]
    fn open_tree_context_menu(
        &mut self,
        path: PathBuf,
        is_directory: bool,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::native_menu::{Command, Item, Popup};
        if cfg!(test) {
            return; // Headless tests exercise the right-click path without a native modal loop.
        }
        let item = if is_directory {
            Item::command("Set as root", Command::OpenFolder)
        } else {
            Item::command("Open", Command::Open)
        };
        let popup = match Popup::capture(window, position, false, vec![item]) {
            Ok(popup) => popup,
            Err(error) => {
                self.set_notice(format!("Could not open menu: {error}"), true);
                cx.notify();
                return;
            }
        };
        let app = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                // TrackPopupMenuEx can reenter GPUI: release entity/window borrows first.
                cx.background_executor()
                    .timer(Duration::from_millis(1))
                    .await;
                let result = popup.show();
                let _ = cx.update(|window, cx| {
                    let _ = app.update(cx, |this, cx| match result {
                        Ok(Some(Command::OpenFolder)) if is_directory => {
                            this.set_file_tree_root(Some(path), window, cx)
                        }
                        Ok(Some(Command::Open)) if !is_directory => this.request_document_action(
                            DocumentAction::OpenTreePath(path),
                            window,
                            cx,
                        ),
                        Err(error) => {
                            this.set_notice(format!("Could not open menu: {error}"), true);
                            cx.notify();
                        }
                        _ => {}
                    });
                });
            })
            .detach();
    }

    #[cfg(windows)]
    fn open_toolbar_menu(&mut self, file_menu: bool, window: &mut Window, cx: &mut Context<Self>) {
        use crate::native_menu::{Command, Item, Popup};
        if cfg!(test) {
            return; // Native tracking needs an interactive desktop; exercised by the UI check.
        }
        // Native tracking pumps messages; keep command indices tied to this menu's snapshot.
        let recent_files = self.recent.files.clone();
        let recent_folders = self.recent.folders.clone();
        let items = if file_menu {
            vec![
                Item::command("New document\tCtrl+N", Command::New),
                Item::command("Open…\tCtrl+O", Command::Open),
                Item::command("Open folder…", Command::OpenFolder),
                Item::submenu(
                    "Recent files",
                    self.recent
                        .files
                        .iter()
                        .enumerate()
                        .map(|(index, path)| {
                            Item::command(
                                path.to_string_lossy()
                                    .replace('&', "&&")
                                    .replace(['\t', '\n', '\r'], " "),
                                Command::RecentFile(index),
                            )
                        })
                        .collect(),
                ),
                Item::submenu(
                    "Recent folders",
                    self.recent
                        .folders
                        .iter()
                        .enumerate()
                        .map(|(index, path)| {
                            Item::command(
                                path.to_string_lossy()
                                    .replace('&', "&&")
                                    .replace(['\t', '\n', '\r'], " "),
                                Command::RecentFolder(index),
                            )
                        })
                        .collect(),
                ),
                Item::separator(),
                Item::command("Save\tCtrl+S", Command::Save),
                Item::command("Save as…\tCtrl+Shift+S", Command::SaveAs),
            ]
        } else {
            vec![
                Item::command("Preview\tCtrl+1", Command::Preview)
                    .checked(self.view_mode == ViewMode::Preview),
                Item::command("Split\tCtrl+2", Command::Split)
                    .checked(self.view_mode == ViewMode::Split),
                Item::command("Source\tCtrl+3", Command::Source)
                    .checked(self.view_mode == ViewMode::Source),
                Item::separator(),
                Item::command("Zoom in\tCtrl++", Command::ZoomIn),
                Item::command("Zoom out\tCtrl+-", Command::ZoomOut),
                Item::command("Actual size\tCtrl+0", Command::ResetZoom),
            ]
        };
        let position = gpui::point(
            if file_menu {
                px(12.0)
            } else {
                window.viewport_size().width - px(12.0)
            },
            px(42.0),
        );
        let popup = match Popup::capture(window, position, !file_menu, items) {
            Ok(popup) => popup,
            Err(error) => {
                self.set_notice(format!("Could not open menu: {error}"), true);
                cx.notify();
                return;
            }
        };
        let app = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                // Release both GPUI borrows before entering the modal native message loop.
                cx.background_executor()
                    .timer(Duration::from_millis(1))
                    .await;
                let result = popup.show();
                cx.update(|window, cx| {
                    app.update(cx, |this, cx| match result {
                        Ok(Some(Command::New)) => {
                            this.request_document_action(DocumentAction::New, window, cx)
                        }
                        Ok(Some(Command::Open)) => {
                            this.request_document_action(DocumentAction::OpenDialog, window, cx)
                        }
                        Ok(Some(Command::OpenFolder)) => this.choose_tree_root(window, cx),
                        Ok(Some(Command::RecentFile(index))) => {
                            if let Some(path) = recent_files.get(index).cloned() {
                                if this.document.path.as_ref() != Some(&path) {
                                    this.request_document_action(
                                        DocumentAction::OpenPath(path),
                                        window,
                                        cx,
                                    );
                                }
                            }
                        }
                        Ok(Some(Command::RecentFolder(index))) => {
                            if let Some(path) = recent_folders.get(index).cloned() {
                                if this.file_tree.root() != Some(path.as_path()) {
                                    this.set_file_tree_root(Some(path), window, cx);
                                }
                            }
                        }
                        Ok(Some(Command::Save)) => this.save_current(window, cx),
                        Ok(Some(Command::SaveAs)) => this.save_as(None, window, cx),
                        Ok(Some(Command::Preview)) => this.set_view_mode(ViewMode::Preview, cx),
                        Ok(Some(Command::Split)) => this.set_view_mode(ViewMode::Split, cx),
                        Ok(Some(Command::Source)) => this.set_view_mode(ViewMode::Source, cx),
                        Ok(Some(Command::ZoomIn)) => this.zoom_in(window, cx),
                        Ok(Some(Command::ZoomOut)) => this.zoom_out(window, cx),
                        Ok(Some(Command::ResetZoom)) => this.reset_zoom(window, cx),
                        Ok(None) => {}
                        Err(error) => {
                            this.set_notice(format!("Could not open menu: {error}"), true);
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .ok();
            })
            .detach();
    }

    fn toolbar(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width: f32 = window.viewport_size().width.into();
        let (regular_tree, regular_outline, overlay_tree, overlay_outline) =
            self.responsive_panel_visibility(width);
        #[cfg(not(windows))]
        let focus = self.app_focus.clone();
        #[cfg(not(windows))]
        let file_menu = Button::new("file-menu")
            .label("File")
            .icon(tool_icon(r#"<path d="m9 10 3 3 3-3"/>"#))
            .small()
            .ghost()
            .border_0()
            .accessibility_label("File menu")
            .dropdown_menu(move |menu, _, _| {
                menu.action_context(focus.clone())
                    .min_w(px(220.0))
                    .menu("New document", Box::new(NewDocument))
                    .menu("Open…", Box::new(OpenDocument))
                    .separator()
                    .menu("Save", Box::new(SaveDocument))
                    .menu("Save as…", Box::new(SaveDocumentAs))
            });
        #[cfg(not(windows))]
        let focus = self.app_focus.clone();
        #[cfg(not(windows))]
        let more_menu = tool_button("view-menu", "More options",
                r#"<circle cx="5" cy="12" r="1"/><circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/>"#)
            .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
                menu.action_context(focus.clone())
                    .min_w(px(220.0))
                    .label("Reading mode")
                    .menu("Preview", Box::new(ShowPreview))
                    .menu("Split", Box::new(ShowSplit))
                    .menu("Source", Box::new(ShowSource))
                    .separator()
                    .label("Document zoom")
                    .menu("Zoom in", Box::new(ZoomIn))
                    .menu("Zoom out", Box::new(ZoomOut))
                    .menu("Actual size", Box::new(ResetZoom))
            });
        #[cfg(windows)]
        let file_menu = Button::new("file-menu")
            .label("File")
            .icon(tool_icon(r#"<path d="m9 10 3 3 3-3"/>"#))
            .small()
            .ghost()
            .border_0()
            .accessibility_label("File menu")
            .on_click(cx.listener(|this, _, window, cx| this.open_toolbar_menu(true, window, cx)));
        #[cfg(windows)]
        let more_menu = tool_button("view-menu", "More options",
            r#"<circle cx="5" cy="12" r="1"/><circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/>"#)
            .on_click(cx.listener(|this, _, window, cx| this.open_toolbar_menu(false, window, cx)));
        let modes = div().h_flex().gap(px(2.0)).p(px(2.0)).rounded_md()
            .bg(rgb(self.palette().panel))
            .child(self.state_button(
                tool_button("preview-mode", "Preview",
                    r#"<path d="M2 4h7a3 3 0 0 1 3 3v14a4 4 0 0 0-4-3H2zM22 4h-7a3 3 0 0 0-3 3v14a4 4 0 0 1 4-3h6z"/>"#)
                    .when(width >= 760.0, |b| b.label("Preview").w_auto().px_2()),
                self.view_mode == ViewMode::Preview)
                .on_click(cx.listener(|this, _, _, cx| this.set_view_mode(ViewMode::Preview, cx))))
            .child(self.state_button(
                tool_button("split-mode", "Split",
                    r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M12 4v16"/>"#)
                    .when(width >= 760.0, |b| b.label("Split").w_auto().px_2()),
                self.view_mode == ViewMode::Split)
                .on_click(cx.listener(|this, _, _, cx| this.set_view_mode(ViewMode::Split, cx))))
            .child(self.state_button(
                tool_button("source-mode", "Source",
                    r#"<path d="m8 7-5 5 5 5m8-10 5 5-5 5m-3-13-2 20"/>"#)
                    .when(width >= 760.0, |b| b.label("Source").w_auto().px_2()),
                self.view_mode == ViewMode::Source)
                .on_click(cx.listener(|this, _, _, cx| this.set_view_mode(ViewMode::Source, cx))));
        div()
            .debug_selector(|| "application-toolbar".into())
            .h_flex()
            .w_full()
            .min_w_0()
            .h(px(42.0))
            .flex_none()
            .items_center()
            .gap_1()
            .pl_3()
            .border_b_1()
            .border_color(rgb(self.palette().border))
            .bg(rgb(self.palette().surface))
            .child(file_menu)
            .child(
                self.state_button(
                    tool_button(
                        "toggle-file-tree",
                        "Files",
                        r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16"/>"#,
                    ),
                    regular_tree || overlay_tree,
                )
                .on_click(cx.listener(|this, _, window, cx| this.toggle_file_tree(window, cx))),
            )
            .child(
                div()
                    .id("window-drag-region")
                    .h_full()
                    .h_flex()
                    .items_center()
                    .when(cfg!(windows), |view| view.window_control_area(gpui::WindowControlArea::Drag))
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .text_sm()
                    .truncate()
                    .text_color(rgb(self.palette().muted))
                    .child(format!(
                        "{}{}",
                        self.document.display_name(),
                        if self.document.is_dirty() { " •" } else { "" }
                    )),
            )
            .child(modes)
            .when(cfg!(windows) && self.view_mode == ViewMode::Split, |view| view.child(
                self.state_button(tool_button("sync-scroll", "Sync scrolling", r#"<path d="M4 7h14l-3-3m5 13H6l3 3M18 7l-3 3M6 17l3-3"/>"#), self.sync_scroll_enabled)
                    .debug_selector(|| "sync-scroll-toggle".into())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sync_scroll_enabled = !this.sync_scroll_enabled;
                        cx.notify();
                    }))))
            .child(div().w(px(1.0)).h(px(16.0)).mx_2().bg(rgb(self.palette().border)))
            .child(
                tool_button(
                    "find-document",
                    "Find",
                    r#"<circle cx="10.5" cy="10.5" r="6.5"/><path d="m16 16 5 5"/>"#,
                )
                .on_click(cx.listener(|this, _, window, cx| this.show_search(window, cx))),
            )
            .child(
                self.state_button(
                    tool_button(
                        "toggle-outline",
                        "Outline",
                        r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M15 4v16"/>"#,
                    ),
                    regular_outline || overlay_outline,
                )
                .on_click(cx.listener(|this, _, window, cx| this.toggle_outline(window, cx))),
            )
            .child(tool_button("settings", "Settings", r#"<circle cx="12" cy="12" r="3"/><path d="m9 3 1-2h4l1 2 3 2 2 .3 2 3-1 2v3l1 2-2 3-2 .3-3 2-1 2h-4l-1-2-3-2-2-.3-2-3 1-2v-3l-1-2 2-3L6 5Z"/>"#)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.settings_open = !this.settings_open;
                    this.focus_native_surface();
                    this.app_focus.focus(window, cx);
                    cx.notify();
                })))
            .child(more_menu)
            .when(cfg!(windows), |view| view
                .child(div().w(px(8.0)))
                .child(caption_control("window-minimize", "Minimize", r#"<path d="M6 12h12"/>"#, gpui::WindowControlArea::Min, self.palette()))
                .child(caption_control("window-maximize", if window.is_maximized() { "Restore" } else { "Maximize" },
                    if window.is_maximized() { r#"<path d="M8 8V5h11v11h-3"/><rect x="5" y="8" width="11" height="11"/>"# } else { r#"<rect x="6" y="6" width="12" height="12"/>"# }, gpui::WindowControlArea::Max, self.palette()))
                .child(caption_control("window-close", "Close", r#"<path d="m7 7 10 10M7 17 17 7"/>"#, gpui::WindowControlArea::Close, self.palette())))
    }

    fn search_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.search_hits.len();
        let current = if count == 0 { 0 } else { self.active_hit + 1 };
        div()
            .debug_selector(|| "search-bar".into())
            .h_flex()
            .h(px(42.0))
            .w_full()
            .flex_none()
            .px_3()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(rgb(self.palette().border))
            .bg(rgb(self.palette().surface))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .max_w(px(480.0))
                    .child(Input::new(&self.search_input).small()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(self.palette().muted))
                    .child(if count == 0 {
                        "No matches".to_owned()
                    } else {
                        format!("{current} of {count}")
                    }),
            )
            .child(
                tool_button(
                    "previous-hit",
                    "Previous match (Shift+Enter)",
                    r#"<path d="m6 15 6-6 6 6"/>"#,
                )
                .disabled(count == 0)
                .on_click(cx.listener(|this, _, window, cx| this.previous_hit(window, cx))),
            )
            .child(
                tool_button(
                    "next-hit",
                    "Next match (Enter)",
                    r#"<path d="m6 9 6 6 6-6"/>"#,
                )
                .disabled(count == 0)
                .on_click(cx.listener(|this, _, window, cx| this.next_hit(window, cx))),
            )
            .child(
                tool_button(
                    "close-search",
                    "Close search (Escape)",
                    r#"<path d="m6 6 12 12M6 18 18 6"/>"#,
                )
                .on_click(cx.listener(|this, _, window, cx| this.close_search(window, cx))),
            )
    }

    fn file_tree_header(&mut self, hint_bounds: HintBounds, cx: &mut Context<Self>) -> AnyElement {
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
        let root_hint_bounds = hint_bounds.clone();
        let can_go_up = root.as_deref().and_then(Path::parent).is_some();
        let has_document_path = self.document.path.is_some();
        let active_not_shown = self.document.path.as_deref().is_some_and(|active| {
            !file_tree::is_markdown_path(active)
                || root.as_deref().is_none_or(|root| !active.starts_with(root))
        });
        let header = div()
            .v_flex()
            .flex_none()
            .gap_1()
            .p_2()
            .text_sm()
            .border_b_1()
            .border_color(rgb(self.palette().border))
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
                                sidebar_hint::build(root_tooltip.clone(), root_hint_bounds.clone(), window, cx)
                            }),
                    )
                    .child(
                        Button::new("tree-open-in-explorer")
                            .icon(tool_icon(r#"<path d="M3 7V5a2 2 0 0 1 2-2h5l2 3h7a2 2 0 0 1 2 2v3M3 7v12a2 2 0 0 0 2 2h8m3-6h6v6m0-6-8 8"/>"#))
                            .accessibility_label("打开当前文件夹")
                            .small()
                            .ghost()
                            .disabled(root.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(root) = this.file_tree.root() {
                                    if let Some(path) = this.document.path.as_deref()
                                        .filter(|path| path.starts_with(root))
                                    {
                                        cx.reveal_path(path);
                                    } else {
                                        cx.open_with_system(root);
                                    }
                                }
                            }))
                            .map(|button| sidebar_hint::trigger(button, "打开当前文件夹并定位当前文件（文件资源管理器）", hint_bounds.clone())),
                    )
                    .child(
                        Button::new("close-file-tree")
                            .icon(tool_icon(r#"<path d="m6 6 12 12M6 18 18 6"/>"#))
                            .accessibility_label("Close File Tree")
                            .small()
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.file_tree_open = false;
                                this.file_tree_narrow_reveal = false;
                                this.persist_layout();
                                cx.notify();
                            }))
                            .map(|button| sidebar_hint::trigger(button, "Close file tree", hint_bounds.clone())),
                    ),
            )
            .child(
                div()
                    .h_flex()
                    .gap_1()
                    .child(
                        Button::new("tree-up")
                            .icon(tool_icon(r#"<path d="M12 20V4m-7 7 7-7 7 7"/>"#))
                            .accessibility_label("Tree Up")
                            .small()
                            .ghost()
                            .disabled(!can_go_up)
                            .on_click(cx.listener(|this, _, window, cx| this.tree_up(window, cx)))
                            .map(|button| sidebar_hint::trigger(button, "Use the parent folder as root", hint_bounds.clone())),
                    )
                    .child(
                        Button::new("tree-document-root")
                            .icon(tool_icon(r#"<path d="m3 10 9-7 9 7v10H3zM9 20v-7h6v7"/>"#))
                            .accessibility_label("Tree Document Root")
                            .small()
                            .ghost()
                            .disabled(!has_document_path)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reset_tree_to_document(window, cx)
                            }))
                            .map(|button| sidebar_hint::trigger(button, "Use the current document folder as root", hint_bounds.clone())),
                    )
                    .child(
                        Button::new("tree-refresh")
                            .icon(tool_icon(r#"<path d="M20 7v5h-5M4 17v-5h5"/><path d="M6 7a7 7 0 0 1 12-1l2 6M4 12l2 6a7 7 0 0 0 12-1"/>"#))
                            .accessibility_label("Tree Refresh")
                            .small()
                            .ghost()
                            .disabled(root.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.refresh_file_tree(window, cx)
                            })),
                    )
                    .child(
                        Button::new("tree-choose-root")
                            .icon(tool_icon(r#"<path d="M3 7h7l2-3h9v16H3z"/>"#))
                            .accessibility_label("Tree Choose Root")
                            .small()
                            .ghost()
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.choose_tree_root(window, cx)
                                }),
                            )
                            .map(|button| sidebar_hint::trigger(button, "Choose a folder as root", hint_bounds.clone())),
                    )
                    .child(
                        self.state_button(
                            Button::new("tree-show-hidden").label("Hidden").small(),
                            self.file_tree.show_hidden(),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            let show = !this.file_tree.show_hidden();
                            let paths = this.file_tree.set_show_hidden(show);
                            for path in paths {
                                this.load_tree_directory(path, window, cx);
                            }
                            cx.notify();
                        }))
                        .map(|button| sidebar_hint::trigger(button, "Show hidden, system, and heavy folders", hint_bounds.clone())),
                    ),
            )
            .when(active_not_shown, |view| {
                view.child(
                    div()
                        .text_xs()
                        .text_color(rgb(self.palette().accent))
                        .child("Current file is not shown in this tree"),
                )
            });

        header.into_any_element()
    }

    fn file_tree_body(
        &mut self,
        root_id: u64,
        hint_bounds: HintBounds,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let has_root = self.file_tree.root().is_some();
        let mut body = div()
            .id(("file-tree-scroll", root_id))
            .v_flex()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .py_2();

        if !has_root {
            body = body.child(
                div()
                    .v_flex()
                    .gap_2()
                    .p_4()
                    .text_sm()
                    .text_color(rgb(self.palette().muted))
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
                        let (is_directory, traversable) = match entry.kind {
                            EntryKind::Directory { traversable } => (true, traversable),
                            EntryKind::Markdown => (false, false),
                        };
                        let entry_icon = if is_directory {
                            if expanded {
                                tool_icon(r#"<path d="M3 18V5a2 2 0 0 1 2-2h5l2 3h7a2 2 0 0 1 2 2v2"/><path d="M3 10h19l-3 10H5z" fill="currentColor" fill-opacity=".24"/>"#)
                            } else {
                                tool_icon(r#"<path d="M3 7V5a2 2 0 0 1 2-2h5l2 3h7a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" fill="currentColor" fill-opacity=".24"/><path d="M3 8h18"/>"#)
                            }.text_color(rgb(self.palette().warning))
                        } else {
                            tool_icon(r#"<path d="M14 2H5a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2V9z" fill="currentColor" fill-opacity=".10"/><path d="M14 2v7h7M6 17v-6l3 4 3-4v6m5-6v6m-2-2 2 2 2-2"/>"#)
                                .text_color(rgb(self.palette().link))
                        }.size_5();
                        let click_path = path.clone();
                        let path_tooltip = path.display().to_string();
                        let path_hint_bounds = hint_bounds.clone();
                        let focus = self.file_tree_focus.clone();
                        let row_element = div()
                            .id(("file-tree-row", row_index))
                            .debug_selector(move || format!("file-tree-row-{row_index}"))
                            .h_flex()
                            .h(px(30.0))
                            .items_center()
                            .flex_none()
                            .min_w_0()
                            .pl(px(8.0 + depth as f32 * 16.0))
                            .pr_2()
                            .gap(px(6.0))
                            .text_sm()
                            .cursor_pointer()
                            .when(is_active, |view| {
                                view.bg(rgb(self.palette().selection)).font_semibold()
                            })
                            .when(is_cursor && !is_active, |view| {
                                view.bg(rgb(self.palette().hover))
                            })
                            .hover(|view| view.bg(rgb(self.palette().hover)))
                            .child(
                                div()
                                    .w(px(10.0))
                                    .flex_none()
                                    .text_color(rgb(self.palette().muted))
                                    .when(is_directory, |view| {
                                        view.child(
                                            tool_icon(if !traversable {
                                                r#"<path d="M6 18 18 6M6 6h12v12"/>"#
                                            } else if expanded {
                                                r#"<path d="m6 9 6 6 6-6"/>"#
                                            } else {
                                                r#"<path d="m9 6 6 6-6 6"/>"#
                                            })
                                            .size(px(10.0)),
                                        )
                                    }),
                            )
                            .child(div().size(px(20.0)).flex_none().child(entry_icon))
                            .child(div().min_w_0().truncate().child(entry.name.clone()))
                            .tooltip(move |window, cx| {
                                sidebar_hint::build(
                                    path_tooltip.clone(),
                                    path_hint_bounds.clone(),
                                    window,
                                    cx,
                                )
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                focus.focus(window, cx);
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

                        #[cfg(windows)]
                        {
                            body = body.child(row_element.on_mouse_down(
                                gpui::MouseButton::Right,
                                cx.listener(
                                    move |this, event: &gpui::MouseDownEvent, window, cx| {
                                        cx.stop_propagation();
                                        this.open_tree_context_menu(
                                            path.clone(),
                                            is_directory,
                                            event.position,
                                            window,
                                            cx,
                                        );
                                    },
                                ),
                            ));
                        }
                        #[cfg(not(windows))]
                        {
                            body = if is_directory {
                                let menu_path = path.clone();
                                let app = cx.entity().downgrade();
                                body.child(row_element.context_menu(move |menu, _, cx| {
                                    app.update(cx, |app, cx| {
                                        app.tree_context_path = Some(menu_path.clone());
                                    })
                                    .ok();
                                    menu.menu("Set as root", Box::new(SetTreeRootContext))
                                }))
                            } else {
                                let menu_path = path.clone();
                                let app = cx.entity().downgrade();
                                body.child(row_element.context_menu(move |menu, _, cx| {
                                    app.update(cx, |app, cx| {
                                        app.tree_context_path = Some(menu_path.clone());
                                    })
                                    .ok();
                                    menu.menu("Open", Box::new(OpenTreeContext))
                                }))
                            };
                        }
                    }
                    VisibleRow::Loading {
                        depth, refreshing, ..
                    } => {
                        body = body.child(
                            div()
                                .pl(px(24.0 + depth as f32 * 16.0))
                                .py_1()
                                .text_xs()
                                .text_color(rgb(self.palette().muted))
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
                                .text_color(rgb(self.palette().muted))
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
                                .text_color(rgb(self.palette().danger))
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

        body.into_any_element()
    }

    // Keep independent builders in separate stack frames: unoptimized GPUI chains
    // otherwise accumulate nearly a megabyte of temporaries in this function.
    fn file_tree_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        use std::hash::{Hash as _, Hasher as _};
        let mut root_hasher = std::collections::hash_map::DefaultHasher::new();
        self.file_tree.root().hash(&mut root_hasher);
        let root_id = root_hasher.finish();
        let hint_bounds = self.file_tree_hint_bounds.clone();
        let header = self.file_tree_header(hint_bounds.clone(), cx);
        let body = self.file_tree_body(root_id, hint_bounds.clone(), cx);
        div()
            .debug_selector(|| "file-tree-panel".into())
            .on_prepaint(move |bounds, _, _| hint_bounds.set(bounds))
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
            .border_color(rgb(self.palette().border))
            .bg(rgb(self.palette().surface))
            .child(header)
            .child(body)
    }

    fn outline_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let active_heading = self.active_outline_heading();
        let sections = markdown::sections(&self.preview_markdown, &self.outline);
        let mut panel = div()
            .debug_selector(|| "outline-panel".into())
            .v_flex()
            .size_full()
            .min_w_0()
            .h_full()
            .flex_none()
            .id("outline-scroll")
            .relative()
            .overflow_y_scroll()
            .track_scroll(&self.outline_scroll)
            .p_2()
            .text_sm()
            .gap_1()
            .border_l_1()
            .border_color(rgb(self.palette().border))
            .bg(rgb(self.palette().surface))
            .child(
                div()
                    .debug_selector(|| "outline-mode-toolbar".into())
                    .h_flex()
                    .gap_1()
                    .mb_2()
                    .pb_2()
                    .border_b_1()
                    .border_color(rgb(self.palette().border))
                    .child(
                        self.state_button(
                            Button::new("outline-jump-mode")
                                .label("Jump")
                                .small()
                                .debug_selector(|| "outline-jump-button".into()),
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
                        self.state_button(
                            Button::new("outline-focus-mode")
                                .label("Focus")
                                .small()
                                .debug_selector(|| "outline-focus-button".into()),
                            self.outline_mode == OutlineMode::Focus,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.outline_mode = OutlineMode::Focus;
                            this.focused_section = this.selected_heading.and_then(|heading| {
                                markdown::sections(&this.preview_markdown, &this.outline)
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
                        self.state_button(
                            Button::new("show-full-document")
                                .accessibility_label("Full document")
                                .child(
                                    div().h_flex().w_full().child(
                                        div()
                                            .debug_selector(|| "outline-full-document-label".into())
                                            .child("Full document"),
                                    ),
                                )
                                .small()
                                .w_full()
                                .justify_start(),
                            self.focused_section.is_none() && active_heading.is_none(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.focused_section = None;
                            this.selected_heading = None;
                            cx.notify();
                        })),
                    ),
            );

        for (heading_index, heading) in self.outline.iter().enumerate() {
            let section_index = sections
                .iter()
                .position(|section| section.heading_index == Some(heading_index));
            let label = heading.title.clone();
            panel = panel.child(
                Button::new(("outline-heading", heading_index))
                    .accessibility_label(label.clone())
                    .child(
                        div().h_flex().w_full().min_w_0().child(
                            div()
                                .debug_selector(move || {
                                    format!("outline-heading-label-{heading_index}")
                                })
                                .min_w_0()
                                .text_ellipsis()
                                .child(label),
                        ),
                    )
                    .small()
                    .ghost()
                    .w_full()
                    .justify_start()
                    .pl(px(outline_indent(heading.level)))
                    .pr_2()
                    .selected(active_heading == Some(heading_index))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_heading = Some(heading_index);
                        this.reading_heading = None;
                        match this.outline_mode {
                            OutlineMode::Jump => {
                                this.focused_section = None;
                                this.outline_jump_request =
                                    this.outline_jump_request.wrapping_add(1);
                            }
                            OutlineMode::Focus => this.focused_section = section_index,
                        }
                        cx.notify();
                    })),
            );
        }
        if self.outline.is_empty() {
            panel = panel.child(
                div()
                    .p_3()
                    .text_sm()
                    .text_color(rgb(self.palette().muted))
                    .child("This document has no headings"),
            );
        }
        panel.vertical_scrollbar(&self.outline_scroll)
    }

    fn preview_source(&self) -> (SharedString, usize) {
        let Some(range) = self.focused_section.and_then(|section_index| {
            markdown::focus_range(&self.preview_markdown, &self.outline, section_index)
        }) else {
            return (self.preview_markdown.clone(), 0);
        };
        let source_offset = range.start;
        (
            SharedString::from(self.preview_markdown[range].to_owned()),
            source_offset,
        )
    }

    fn preview_panel(&self) -> AnyElement {
        let panel = div()
            .debug_selector(|| "preview-panel".into())
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden();
        #[cfg(windows)]
        {
            if let Some(view) = &self.web_preview {
                return panel
                    .child(crate::web_preview::WebPreview::surface(view.clone()))
                    .into_any_element();
            }
            if let Some(error) = &self.web_error {
                return panel.p_5().child(format!("WebView2 preview unavailable: {error}. Install Microsoft Edge WebView2 Runtime and restart. You can still edit and save the source.")).into_any_element();
            }
        }
        let message = if cfg!(windows) {
            "Loading preview…"
        } else {
            "Web preview is currently supported on Windows. Source editing remains available."
        };
        panel.p_5().child(message).into_any_element()
    }

    fn cancel_paste(&mut self) -> bool {
        self.pending_paste.take().is_some()
    }

    fn begin_paste(
        &mut self,
        snapshot: crate::paste::Snapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dialog_in_flight {
            return;
        }
        self.cancel_paste();
        // Flush a deferred Change before taking the source selection as an anchor.
        let current = self.editor.read(cx).value().to_string();
        if self.document.content != current {
            self.document.content = current;
            self.source_search.clear();
            self.schedule_analysis(cx);
        }
        self.paste_request = self.paste_request.wrapping_add(1);
        let id = self.paste_request;
        self.pending_paste = Some(PendingPaste {
            id,
            range: self.editor.read(cx).selected_range(),
            cancelled: Arc::new(AtomicBool::new(false)),
            prepared: None,
        });
        self.set_notice("Preparing paste… Esc to cancel", false);
        cx.notify();
        let task = cx
            .background_executor()
            .spawn(async move { crate::paste::Import::prepare(snapshot) });
        let app = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let import = task.await;
                cx.update(|window, cx| {
                    app.update(cx, |app, cx| {
                        if app.pending_paste.as_ref().is_none_or(|p| p.id != id) {
                            return;
                        }
                        if !import.has_images() {
                            app.complete_paste(
                                id,
                                crate::paste::Finished {
                                    markdown: import.markdown,
                                    failed: 0,
                                },
                                window,
                                cx,
                            );
                        } else {
                            app.pending_paste.as_mut().unwrap().prepared = Some(import);
                            if app.document.path.is_none() {
                                app.set_notice(
                                    "Save the Markdown document to choose its image folder",
                                    false,
                                );
                                app.save_as(None, window, cx);
                            } else {
                                app.resume_paste(window, cx);
                            }
                        }
                    })
                    .ok();
                })
                .ok();
            })
            .detach();
    }

    fn resume_paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.document.path.clone() else {
            return;
        };
        let Some(pending) = self.pending_paste.as_mut() else {
            return;
        };
        let Some(import) = pending.prepared.take() else {
            return;
        };
        let id = pending.id;
        let cancelled = pending.cancelled.clone();
        let timeouts = self.download_timeouts;
        self.set_notice(
            format!(
                "Importing images… downloads limited to {}s. Esc to cancel",
                timeouts.batch_secs
            ),
            false,
        );
        cx.notify();
        let task = cx
            .background_executor()
            .spawn(async move { import.finish(&path, cancelled, timeouts) });
        let app = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let result = task.await;
                cx.update(|window, cx| {
                    app.update(cx, |app, cx| app.complete_paste(id, result, window, cx))
                        .ok();
                })
                .ok();
            })
            .detach();
    }

    fn complete_paste(
        &mut self,
        id: u64,
        result: crate::paste::Finished,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_paste.as_ref().is_none_or(|p| p.id != id) {
            return;
        }
        let pending = self.pending_paste.take().unwrap();
        // An edit event can still be deferred when a background result arrives.
        let current = self.editor.read(cx).value().to_string();
        let mut range = pending.range.clone();
        if current != self.document.content
            && !crate::paste::rebase_selection(&mut range, &self.document.content, &current)
        {
            self.set_notice("Paste cancelled: its insertion point was edited", false);
            cx.notify();
            return;
        }
        let mut selection = self.editor.read(cx).selected_range();
        let restore_selection = selection != range;
        self.editor.update(cx, |editor, cx| {
            editor.set_selected_range(range, cx);
            editor.replace(result.markdown, window, cx);
            if restore_selection
                && crate::paste::rebase_selection(&mut selection, &current, &editor.value())
            {
                editor.set_selected_range(selection, cx);
            }
        });
        self.set_notice(
            if result.failed == 0 {
                "Pasted Markdown".into()
            } else {
                format!(
                    "Pasted Markdown; {} image(s) failed — source links kept where available",
                    result.failed
                )
            },
            result.failed != 0,
        );
        cx.notify();
    }

    fn editor_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let zoom = cx.listener(Self::handle_scroll_wheel);
        div()
            .debug_selector(|| "editor-panel".into())
            .key_context("NativeMarkdownSource")
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Paste, window, cx| {
                    let clipboard = cx.read_from_clipboard();
                    let snapshot = crate::paste::Snapshot::read(clipboard.as_ref());
                    if snapshot.is_rich() {
                        this.begin_paste(snapshot, window, cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .capture_action(cx.listener(|this, _: &gpui_component::input::Undo, _, cx| {
                if this.cancel_paste() {
                    this.set_notice("Paste cancelled", false);
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                if this.cancel_paste() {
                    this.set_notice("Paste cancelled", false);
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &PastePlainText, window, cx| {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    this.editor
                        .update(cx, |editor, cx| editor.replace(text, window, cx));
                }
            }))
            .relative()
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(rgb(self.palette().surface))
            .child(
                Editor::new(&self.editor)
                    .size_full()
                    .text_size(px(BASE_MONO_FONT_SIZE * self.zoom.factor()))
                    .appearance(false),
            )
            .child(self.source_search.observer(self.editor.clone()))
            // The editor consumes wheel events before they bubble to the app root.
            // Capture only Ctrl+wheel; leave normal scrolling to the editor.
            .child(
                gpui::canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
                            if phase.capture()
                                && bounds.contains(&event.position)
                                && event.modifiers.control
                            {
                                zoom(event, window, cx);
                            }
                        });
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .map(|panel| {
                #[cfg(windows)]
                if let Some(preview) = self
                    .web_preview
                    .clone()
                    .filter(|_| self.view_mode == ViewMode::Split && self.sync_scroll_enabled)
                {
                    return panel.child(
                        self.scroll_sync.observer(
                            self.editor.clone(),
                            move |position, revision| {
                                preview.borrow().scroll_to(position, revision)
                            },
                        ),
                    );
                }
                panel
            })
    }

    fn welcome(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .bg(rgb(self.palette().canvas))
            .child(
                div()
                    .text_3xl()
                    .font_semibold()
                    .child("Read Markdown without the machinery."),
            )
            .child(
                div()
                    .text_color(rgb(self.palette().muted))
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
                    .text_color(rgb(self.palette().muted))
                    .child("Drop a .md file anywhere · Ctrl+O to open"),
            )
    }

    fn workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let content = if self.document.is_empty() {
            self.welcome(cx).into_any_element()
        } else {
            match self.view_mode {
                ViewMode::Preview => self.preview_panel().into_any_element(),
                ViewMode::Source => self.editor_panel(cx).into_any_element(),
                ViewMode::Split => div()
                    .h_flex()
                    .size_full()
                    .child(self.editor_panel(cx))
                    .child(div().w(px(1.0)).h_full().bg(rgb(self.palette().border)))
                    .child(self.preview_panel())
                    .into_any_element(),
            }
        };

        let width: f32 = window.viewport_size().width.into();
        let (regular_tree, regular_outline, overlay_tree, overlay_outline) =
            self.responsive_panel_visibility(width);
        let visibility = (regular_tree, regular_outline);
        if self.workspace_panel_visibility != Some(visibility) {
            // ResizableState tracks sizes by index. Hidden children retain their old
            // geometry, so rebuild the state when the visible panel set changes.
            self.workspace_resizable = cx.new(|_| ResizableState::default());
            self.workspace_panel_visibility = Some(visibility);
        }
        let mut panels = Vec::new();
        if regular_tree {
            panels.push(
                resizable_panel()
                    .size(px(self.file_tree_width))
                    .size_range(px(120.0)..px(520.0))
                    .child(self.file_tree_panel(cx)),
            );
        }
        panels.push(
            resizable_panel()
                .size_range(px(420.0)..px(10_000.0))
                .child(content),
        );
        if regular_outline {
            panels.push(
                resizable_panel()
                    .size(px(self.outline_width))
                    .size_range(px(120.0)..px(520.0))
                    .child(self.outline_panel(cx)),
            );
        }
        let panel_count = panels.len();
        let app = cx.entity().downgrade();
        let resizable = h_resizable("document-workspace-panels")
            .with_state(&self.workspace_resizable)
            .on_resize(move |state, _, cx| {
                let sizes = state.read(cx).sizes().clone();
                if sizes.len() != panel_count {
                    return;
                }
                app.update(cx, |app, _| {
                    let file_tree_width = if regular_tree {
                        sizes[0].into()
                    } else {
                        app.file_tree_width
                    };
                    let outline_width = if regular_outline {
                        sizes[panel_count - 1].into()
                    } else {
                        app.outline_width
                    };
                    let mut settings = LayoutSettings {
                        appearance: app.appearance,
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
            .children(panels);

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

    fn status_bar(&self) -> impl IntoElement {
        #[cfg(windows)]
        let local_images = self
            .web_preview
            .as_ref()
            .map_or(0, |view| view.borrow().image_count());
        #[cfg(not(windows))]
        let local_images = 0;
        let details = format!(
            "{} · {}% · {} local images",
            self.view_mode.label(),
            self.zoom.percent(),
            local_images
        );
        let left = self.notice.as_ref().map_or_else(
            || {
                if self.document.is_dirty() {
                    "Unsaved changes · recovery on".to_owned()
                } else {
                    String::new()
                }
            },
            |notice| notice.text.clone(),
        );
        let left_color = if self.notice.as_ref().is_some_and(|notice| notice.is_error) {
            rgb(self.palette().danger)
        } else {
            rgb(self.palette().muted)
        };
        div()
            .h_flex()
            .h(px(24.0))
            .flex_none()
            .items_center()
            .gap_3()
            .px_3()
            .border_t_1()
            .border_color(rgb(self.palette().border))
            .bg(rgb(self.palette().surface))
            .text_xs()
            .text_color(rgb(self.palette().muted))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(left_color)
                    .child(left),
            )
            .child(format!(
                "{} words · {} min read",
                self.word_count, self.reading_minutes
            ))
            .child(
                div()
                    .id("document-status")
                    .child(format!("{}%", self.zoom.percent()))
                    .tooltip(move |window, cx| Tooltip::new(details.clone()).build(window, cx)),
            )
    }

    fn handle_drop(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = paths.paths().iter().find(|path| path.is_file()).cloned() {
            self.request_document_action(DocumentAction::OpenPath(path), window, cx);
        }
    }
}

impl Render for NativeMarkdownApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(windows)]
        self.sync_web_preview(window, cx);
        let marker = if self.document.is_dirty() { " •" } else { "" };
        window.set_window_title(&format!(
            "{}{marker} — Native Markdown",
            self.document.display_name()
        ));

        // Removing the editor leaves its FocusHandle alive but outside the dispatch tree.
        // GPUI still reports Some(focus), so later app shortcuts have no dispatch path.
        if window.focused(cx).is_none()
            || (self.view_mode == ViewMode::Preview
                && self
                    .editor
                    .read(cx)
                    .focus_handle(cx)
                    .contains_focused(window, cx))
        {
            self.app_focus.focus(window, cx);
        }

        div()
            .track_focus(&self.app_focus)
            .capture_any_mouse_down(cx.listener(|this, _, _, _| this.focus_native_surface()))
            .capture_action(cx.listener(|this, _: &NextSearchHit, window, cx| {
                if this.search_open {
                    this.next_hit(window, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &PreviousSearchHit, window, cx| {
                if this.search_open {
                    this.previous_hit(window, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(
                |this, action: &gpui_component::input::Enter, window, cx| {
                    if this.search_open {
                        if action.shift {
                            this.previous_hit(window, cx);
                        } else {
                            this.next_hit(window, cx);
                        }
                        cx.stop_propagation();
                    }
                },
            ))
            .capture_action(cx.listener(|this, _: &FileTreeOpen, window, cx| {
                if this.search_open {
                    this.next_hit(window, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Search, window, cx| {
                    this.show_search(window, cx);
                    cx.stop_propagation();
                }),
            )
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if this.dialog_in_flight {
                    return;
                }
                let key = &event.keystroke;
                if key.modifiers.control && !key.modifiers.alt && key.key == "f" {
                    this.show_search(window, cx);
                    window.prevent_default();
                    cx.stop_propagation();
                } else if this.search_open
                    && key.key == "enter"
                    && !key.modifiers.control
                    && !key.modifiers.alt
                    && !key.modifiers.platform
                {
                    if key.modifiers.shift {
                        this.previous_hit(window, cx);
                    } else {
                        this.next_hit(window, cx);
                    }
                    window.prevent_default();
                    cx.stop_propagation();
                } else if this.settings_open && key.key == "escape" {
                    this.settings_open = false;
                    cx.notify();
                    window.prevent_default();
                    cx.stop_propagation();
                } else if this.search_open && key.key == "escape" {
                    this.close_search(window, cx);
                    this.app_focus.focus(window, cx);
                    window.prevent_default();
                    cx.stop_propagation();
                }
            }))
            .key_context(if self.search_open {
                "NativeMarkdown NativeMarkdownSearch"
            } else {
                APP_CONTEXT
            })
            .on_scroll(cx.listener(Self::handle_scroll_wheel))
            .on_action(cx.listener(Self::on_new))
            .on_action(cx.listener(Self::on_open))
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_save_as))
            .on_action(cx.listener(Self::on_find))
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                if this.cancel_paste() {
                    this.set_notice("Paste cancelled", false);
                    cx.notify();
                } else if this.search_open {
                    this.close_search(window, cx);
                }
            }))
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
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(rgb(self.palette().surface))
            .text_color(rgb(self.palette().text))
            .child(self.toolbar(window, cx))
            .when(self.search_open, |view| view.child(self.search_bar(cx)))
            .child(if self.settings_open {
                self.settings_panel(cx).into_any_element()
            } else {
                self.workspace(window, cx).into_any_element()
            })
            .child(self.status_bar())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size, Focusable, Modifiers, TestAppContext};

    fn draw_app(cx: &mut gpui::VisualTestContext) {
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    #[gpui::test]
    fn settings_categories_and_download_controls_preserve_layout(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let mut app = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NativeMarkdownApp::new(None, window, cx));
            app = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.simulate_resize(size(px(900.0), px(780.0)));
        app.update(cx, |app, cx| {
            app.settings_open = true;
            cx.notify();
        });
        draw_app(cx);
        let done = cx.debug_bounds("close-settings").unwrap();
        let nav = cx.debug_bounds("settings-section-2").unwrap();
        for (selector, expected) in [
            (
                "settings-section-2",
                crate::paste::DownloadTimeouts::default(),
            ),
            (
                "download-timeout-0-15",
                crate::paste::DownloadTimeouts {
                    connect_secs: 15,
                    image_secs: 10,
                    batch_secs: 20,
                },
            ),
            (
                "download-timeout-1-30",
                crate::paste::DownloadTimeouts {
                    connect_secs: 15,
                    image_secs: 30,
                    batch_secs: 20,
                },
            ),
            (
                "download-timeout-2-60",
                crate::paste::DownloadTimeouts {
                    connect_secs: 15,
                    image_secs: 30,
                    batch_secs: 60,
                },
            ),
            (
                "settings-section-1",
                crate::paste::DownloadTimeouts {
                    connect_secs: 15,
                    image_secs: 30,
                    batch_secs: 60,
                },
            ),
            (
                "settings-section-0",
                crate::paste::DownloadTimeouts {
                    connect_secs: 15,
                    image_secs: 30,
                    batch_secs: 60,
                },
            ),
            (
                "settings-section-2",
                crate::paste::DownloadTimeouts {
                    connect_secs: 15,
                    image_secs: 30,
                    batch_secs: 60,
                },
            ),
            (
                "reset-download-timeouts",
                crate::paste::DownloadTimeouts::default(),
            ),
        ] {
            let position = cx.debug_bounds(selector).unwrap().center();
            cx.simulate_mouse_down(position, gpui::MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_up(position, gpui::MouseButton::Left, Modifiers::default());
            draw_app(cx);
            assert_eq!(cx.debug_bounds("close-settings").unwrap(), done);
            assert_eq!(cx.debug_bounds("settings-section-2").unwrap(), nav);
            app.read_with(cx, |app, _| {
                assert_eq!(app.download_timeouts, expected);
                assert!(app.document.content.is_empty());
                assert!(!app.document.is_dirty());
            });
        }
    }

    #[gpui::test]
    fn markdown_controls_refresh_analysis_without_editing_source(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let directory = tempfile::tempdir_in("tmp").unwrap();
        let path = directory.path().join("dialect.md");
        let source = "# ~~Title~~\n\n1. aaa\n  -- bbb";
        std::fs::write(&path, source).unwrap();
        let mut app = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NativeMarkdownApp::new(Some(path), window, cx));
            app = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.simulate_resize(size(px(1180.0), px(1400.0)));
        app.update(cx, |app, cx| {
            assert_eq!(app.markdown_dialect, markdown::Dialect::GitHub);
            assert!(!app.preserve_line_breaks);
            assert_eq!(app.outline[0].title, "Title");
            app.settings_open = true;
            app.settings_section = 1;
            cx.notify();
        });
        for selector in [
            "markdown-dialect-commonmark",
            "markdown-preserve-line-breaks",
            "markdown-dialect-github",
        ] {
            draw_app(cx);
            let position = cx.debug_bounds(selector).unwrap().center();
            cx.simulate_mouse_down(position, gpui::MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_up(position, gpui::MouseButton::Left, Modifiers::default());
            draw_app(cx);
            app.read_with(cx, |app, _| {
                assert_eq!(app.document.content, source);
                assert!(!app.document.is_dirty());
                assert_eq!(app.preview_markdown.as_ref(), source);
                assert!(app.pending_analysis.is_none());
                if selector == "markdown-dialect-commonmark" {
                    assert_eq!(app.markdown_dialect, markdown::Dialect::CommonMark);
                    assert_eq!(app.outline[0].title, "~~Title~~");
                } else if selector == "markdown-preserve-line-breaks" {
                    assert!(app.preserve_line_breaks);
                } else {
                    assert_eq!(app.markdown_dialect, markdown::Dialect::GitHub);
                    assert_eq!(app.outline[0].title, "Title");
                    assert!(app.preserve_line_breaks);
                }
            });
        }
    }

    #[gpui::test]
    fn preview_refresh_delay_control_restarts_pending_refresh(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("refresh-setting.md");
        std::fs::write(&path, "# Before").unwrap();
        let mut app = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NativeMarkdownApp::new(Some(path), window, cx));
            app = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.simulate_resize(size(px(1180.0), px(780.0)));
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                assert_eq!(app.preview_refresh_delay_ms, 1000);
                app.set_view_mode(ViewMode::Split, cx);
                app.editor.update(cx, |editor, cx| {
                    editor.set_selected_range(0..8, cx);
                    editor.focus(window, cx);
                });
            })
        });
        draw_app(cx);
        cx.simulate_input("# After");
        draw_app(cx);
        app.update(cx, |app, cx| {
            app.settings_open = true;
            app.settings_section = 1;
            cx.notify();
        });
        draw_app(cx);
        for (delay, selector) in [
            (3000, "preview-refresh-delay-3000"),
            (500, "preview-refresh-delay-500"),
        ] {
            let position = cx.debug_bounds(selector).unwrap().center();
            cx.simulate_mouse_down(position, gpui::MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_up(position, gpui::MouseButton::Left, Modifiers::default());
            draw_app(cx);
            app.read_with(cx, |app, _| assert_eq!(app.preview_refresh_delay_ms, delay));
            cx.executor()
                .advance_clock(Duration::from_millis(if delay == 3000 {
                    1000
                } else {
                    499
                }));
            cx.run_until_parked();
            app.read_with(cx, |app, _| {
                assert_eq!(app.preview_markdown.as_ref(), "# Before")
            });
        }
        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();
        app.read_with(cx, |app, _| {
            assert_eq!(app.preview_markdown.as_ref(), "# After")
        });
        cx.executor().advance_clock(Duration::from_secs(3));
        cx.run_until_parked();
        app.read_with(cx, |app, _| assert_eq!(app.preview_refresh_delay_ms, 500));
    }

    #[gpui::test]
    fn editor_refresh_waits_one_second_after_the_last_change(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(bind_keys);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("debounce.md");
        let original = "# Parent\n\n## Child\n\nOriginal text.";
        std::fs::write(&path, original).unwrap();
        let (app, cx) = cx.add_window_view(|window, cx| {
            let mut app = NativeMarkdownApp::new(Some(path.clone()), window, cx);
            app.view_mode = ViewMode::Split;
            app.focused_section = Some(1);
            app
        });
        cx.simulate_resize(size(px(1180.0), px(780.0)));
        cx.update(|window, cx| {
            app.read(cx).editor.clone().update(cx, |editor, cx| {
                editor.set_selected_range(0..original.len(), cx);
                editor.focus(window, cx);
            })
        });
        draw_app(cx);
        let focused_preview = app.read_with(cx, |app, _| app.preview_source());
        let revision = app.read_with(cx, |app, _| app.outline_revision);
        cx.simulate_input("# 新 🦀");
        draw_app(cx);
        app.read_with(cx, |app, _| {
            assert_eq!(app.document.content, "# 新 🦀");
            assert!(app.document.is_dirty());
            assert_eq!(
                app.preview_markdown.as_ref(),
                original,
                "typing must not refresh preview"
            );
            assert_eq!(
                app.preview_source(),
                focused_preview,
                "Focus must use the same snapshot"
            );
        });
        cx.executor().advance_clock(Duration::from_millis(900));
        cx.run_until_parked();
        cx.simulate_input(" final");
        draw_app(cx);
        cx.executor().advance_clock(Duration::from_millis(999));
        cx.run_until_parked();
        app.update(cx, |app, cx| {
            assert_eq!(app.preview_markdown.as_ref(), original);
            assert_eq!(app.outline_revision, revision);
            assert!(
                app.save_existing(cx),
                "save must use the latest editor text before refresh"
            );
        });
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# 新 🦀 final");
        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();
        app.read_with(cx, |app, _| {
            assert_eq!(app.preview_markdown.as_ref(), "# 新 🦀 final");
            assert_eq!(app.outline[0].title, "新 🦀 final");
            assert_eq!(app.outline_revision, revision + 1);
        });
        cx.executor().advance_clock(Duration::from_secs(4));
        cx.run_until_parked();
        app.read_with(cx, |app, _| assert_eq!(app.outline_revision, revision + 1));
        // Reverting back to the published snapshot cancels the pending refresh entirely.
        cx.simulate_input("!");
        draw_app(cx);
        cx.simulate_keystrokes("backspace");
        draw_app(cx);
        cx.executor().advance_clock(Duration::from_secs(3));
        cx.run_until_parked();
        app.read_with(cx, |app, _| {
            assert_eq!(app.document.content, "# 新 🦀 final");
            assert_eq!(app.outline_revision, revision + 1);
        });
        // A new document is immediate and must cancel any old document's delayed work.
        cx.simulate_input(" pending");
        draw_app(cx);
        let replacement = directory.path().join("replacement.md");
        std::fs::write(&replacement, "# Replacement").unwrap();
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.replace_document(Document::open(replacement).unwrap(), window, cx);
            })
        });
        draw_app(cx);
        let revision = app.read_with(cx, |app, _| {
            assert_eq!(app.preview_markdown.as_ref(), "# Replacement");
            app.outline_revision
        });
        cx.executor().advance_clock(Duration::from_secs(4));
        cx.run_until_parked();
        app.read_with(cx, |app, _| {
            assert_eq!(app.preview_markdown.as_ref(), "# Replacement");
            assert_eq!(app.outline_revision, revision);
        });
    }

    #[gpui::test]
    fn mode_shortcuts_survive_removing_the_focused_editor(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(bind_keys);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mode-shortcuts.md");
        std::fs::write(&path, "# Mode shortcuts\n\nEditable text.").unwrap();
        let mut app = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NativeMarkdownApp::new(Some(path), window, cx));
            app = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.simulate_resize(size(px(1180.0), px(780.0)));
        draw_app(cx);
        for focus_editor in [true, false] {
            for entry in ["ctrl-2", "ctrl-3"] {
                for preview in ["ctrl-1", "ctrl-e"] {
                    for (exit, expected) in [
                        ("ctrl-2", ViewMode::Split),
                        ("ctrl-3", ViewMode::Source),
                        ("ctrl-e", ViewMode::Split),
                    ] {
                        cx.simulate_keystrokes(entry);
                        draw_app(cx);
                        cx.update(|window, cx| {
                            let app = app.read(cx);
                            if focus_editor {
                                app.editor.clone().update(cx, |editor, cx| {
                                    editor.set_selected_range(2..6, cx);
                                    editor.focus(window, cx);
                                });
                            } else {
                                app.file_tree_focus.clone().focus(window, cx);
                            }
                        });
                        draw_app(cx);
                        cx.simulate_keystrokes(preview);
                        draw_app(cx);
                        app.read_with(cx, |app, _| assert_eq!(app.view_mode, ViewMode::Preview));
                        cx.simulate_keystrokes(exit);
                        draw_app(cx);
                        app.read_with(cx, |app, cx| {
                            assert_eq!(
                                app.view_mode, expected,
                                "{entry} / {preview} / {exit}, editor focus: {focus_editor}"
                            );
                            assert_eq!(app.document.content, "# Mode shortcuts\n\nEditable text.");
                            if focus_editor {
                                assert_eq!(app.editor.read(cx).selected_range(), 2..6);
                            }
                        });
                    }
                }
            }
        }
    }

    #[gpui::test]
    #[cfg(windows)]
    fn outline_follow_reveals_only_offscreen_headings(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (app, cx) = cx.add_window_view(|window, cx| NativeMarkdownApp::new(None, window, cx));
        cx.simulate_resize(size(px(1180.0), px(780.0)));
        app.update(cx, |app, cx| {
            app.document.content = (0..90)
                .map(|index| format!("## Heading {index}\n\nBody.\n\n"))
                .collect();
            app.refresh_analysis();
            cx.notify();
        });
        draw_app(cx);
        let assert_visible = |app: &NativeMarkdownApp, heading: usize| {
            let viewport = app.outline_scroll.bounds();
            let row = app.outline_scroll.bounds_for_item(heading + 2).unwrap();
            let offset = app.outline_scroll.offset().y;
            assert!(
                row.top() + offset >= viewport.top(),
                "heading above viewport"
            );
            assert!(
                row.bottom() + offset <= viewport.bottom(),
                "heading below viewport"
            );
        };
        let follow = |heading, cx: &mut gpui::VisualTestContext| {
            app.update(cx, |app, cx| {
                let navigation = app.outline_jump_request;
                assert!(app.apply_outline_position(
                    Some(heading),
                    0,
                    app.document.content.len(),
                    app.outline_revision
                ));
                assert_eq!(app.outline_jump_request, navigation);
                cx.notify();
            });
            draw_app(cx);
        };
        follow(79, cx);
        app.read_with(cx, |app, _| assert_visible(app, 79));
        follow(2, cx);
        app.read_with(cx, |app, _| assert_visible(app, 2));
        let offset = app.read_with(cx, |app, _| app.outline_scroll.offset());
        follow(3, cx);
        app.read_with(cx, |app, _| {
            assert_visible(app, 3);
            assert_eq!(
                app.outline_scroll.offset(),
                offset,
                "visible rows must not shift the sidebar"
            );
        });
        // Browsing the outline manually must not be undone by duplicate reports.
        app.update(cx, |app, cx| {
            app.outline_scroll.set_offset(point(px(0.0), px(-400.0)));
            assert!(!app.apply_outline_position(
                Some(3),
                0,
                app.document.content.len(),
                app.outline_revision
            ));
            cx.notify();
        });
        draw_app(cx);
        app.read_with(cx, |app, _| {
            assert_eq!(app.outline_scroll.offset().y, px(-400.0))
        });
    }

    #[gpui::test]
    #[cfg(windows)]
    fn scroll_outline_highlight_never_changes_navigation_or_focus(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (app, cx) = cx.add_window_view(|window, cx| NativeMarkdownApp::new(None, window, cx));
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.document.content =
                    "Intro\n\n# Parent\nText\n\n## Child\nChild text\n\n# Last\nEnd".into();
                app.refresh_analysis();
                let revision = app.outline_revision;
                let end = app.document.content.len();
                let navigation = app.outline_jump_request;
                let selection = app.editor.read(cx).selected_range();
                assert!(app.apply_outline_position(Some(1), 0, end, revision));
                assert_eq!(app.active_outline_heading(), Some(1));
                assert_eq!(
                    app.selected_heading, None,
                    "scroll must not change the clicked heading"
                );
                assert_eq!(app.outline_jump_request, navigation);
                assert_eq!(app.focused_section, None);
                assert_eq!(app.editor.read(cx).selected_range(), selection);
                assert!(
                    !app.apply_outline_position(Some(1), 0, end, revision),
                    "same chapter must not request another redraw"
                );
                assert!(!app.apply_outline_position(Some(99), 0, end, revision));
                assert!(!app.apply_outline_position(Some(0), 0, end, revision.wrapping_sub(1)));
                assert!(app.apply_outline_position(None, 0, end, revision));
                assert_eq!(app.active_outline_heading(), None);

                // A focused subtree reports local indexes; old/full-document messages are stale.
                app.outline_mode = OutlineMode::Focus;
                app.focused_section = Some(1); // Parent follows the introduction section.
                let (scope, base) = app.preview_source();
                assert!(base > 0);
                assert!(!app.apply_outline_position(Some(0), 0, end, revision));
                assert!(app.apply_outline_position(Some(1), base, base + scope.len(), revision));
                assert_eq!(app.active_outline_heading(), Some(1));
                assert_eq!(app.focused_section, Some(1));
                assert_eq!(app.outline_jump_request, navigation);
                assert_eq!(app.editor.read(cx).selected_range(), selection);
                app.refresh_analysis();
                assert_eq!(app.reading_heading, None);
                assert!(!app.apply_outline_position(Some(0), base, base + scope.len(), revision));
                let _ = window;
            })
        });
    }

    #[gpui::test]
    #[cfg(windows)]
    fn tree_context_menu_keeps_document_visible(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("context.md");
        std::fs::write(&path, "# Still visible").unwrap();
        let mut app = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NativeMarkdownApp::new(Some(path.clone()), window, cx));
            app = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.simulate_resize(size(px(1180.0), px(780.0)));
        app.update(cx, |app, cx| {
            let request = app.file_tree.begin_load(directory.path());
            assert!(app.file_tree.finish_load(
                directory.path(),
                request,
                file_tree::scan_directory(directory.path(), false)
            ));
            cx.notify();
        });
        draw_app(cx);
        let row = cx.debug_bounds("file-tree-row-0").unwrap().center();
        cx.simulate_mouse_down(row, gpui::MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(row, gpui::MouseButton::Right, Modifiers::default());
        draw_app(cx);
        app.read_with(cx, |app, _| {
            assert!(
                app.web_preview_allowed(1180.0),
                "tree context menu must not suspend the document WebView"
            );
            assert_eq!(app.document.content, "# Still visible");
        });
    }

    #[gpui::test]
    #[cfg(windows)]
    fn file_menu_keeps_document_visible(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let mut app = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NativeMarkdownApp::new(None, window, cx));
            app = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.simulate_resize(size(px(1180.0), px(780.0)));
        draw_app(cx);
        cx.simulate_mouse_down(
            point(px(40.0), px(21.0)),
            gpui::MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            point(px(40.0), px(21.0)),
            gpui::MouseButton::Left,
            Modifiers::default(),
        );
        draw_app(cx);
        app.read_with(cx, |app, _| {
            assert!(
                app.web_preview_allowed(1180.0),
                "opening File must not suspend the document WebView"
            );
        });
    }

    #[gpui::test]
    fn outline_divider_tracks_mouse_after_navigation_toggle(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let mut app = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NativeMarkdownApp::new(None, window, cx));
            app = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.simulate_resize(size(px(1180.0), px(780.0)));
        cx.update(|_, cx| {
            app.update(cx, |app, cx| {
                app.document.content = "# Heading\n\nPreview content.".into();
                app.refresh_analysis();
                app.file_tree_open = false;
                app.outline_open = true;
                app.file_tree_width = 280.0;
                app.outline_width = 270.0;
                cx.notify();
            })
        });
        draw_app(cx);
        for open in [true, false] {
            cx.update(|_, cx| {
                app.update(cx, |app, cx| {
                    app.file_tree_open = open;
                    cx.notify();
                })
            });
            draw_app(cx);
        }
        let outline = cx.debug_bounds("outline-panel").unwrap();
        // Current GPUI debug bounds include padding and the border: the handle is at this edge.
        let start = point(outline.left(), outline.center().y);
        cx.simulate_mouse_down(start, gpui::MouseButton::Left, Modifiers::default());
        for (step, delta) in [-10.0, -30.0, -50.0, 20.0].into_iter().enumerate() {
            let target = point(start.x + px(delta), start.y);
            cx.simulate_mouse_move(target, gpui::MouseButton::Left, Modifiers::default());
            draw_app(cx);
            // The first move starts the drag; subsequent moves resize the panels.
            if step > 0 {
                let actual = cx.debug_bounds("outline-panel").unwrap().left();
                let expected = outline.left() + px(delta);
                assert!(
                    (actual - expected).abs() <= px(2.0),
                    "divider must follow mouse: expected={expected:?}, actual={actual:?}"
                );
            }
        }
    }

    #[gpui::test]
    fn search_shortcuts_work_from_sidebar_and_cycle_in_both_directions(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(bind_keys);
        let mut app = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NativeMarkdownApp::new(None, window, cx));
            app = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.document.content =
                    "# Search\n\nFirst Needle.\n\nSecond Needle.\n\nThird Needle.".into();
                app.refresh_analysis();
                app.file_tree_focus.focus(window, cx);
                app.show_search(window, cx);
                app.search_input
                    .update(cx, |input, cx| input.replace_all("needle", window, cx));
            })
        });
        cx.simulate_resize(size(px(1180.0), px(780.0)));
        draw_app(cx);
        cx.simulate_keystrokes("enter");
        app.read_with(cx, |app, _| {
            assert_eq!(app.active_hit, 0, "first Enter must activate first result")
        });
        cx.simulate_keystrokes("shift-enter");
        app.read_with(cx, |app, _| {
            assert_eq!(app.active_hit, 2, "Shift+Enter must wrap to last result")
        });
        cx.update(|window, cx| app.read(cx).file_tree_focus.clone().focus(window, cx));
        draw_app(cx);
        cx.simulate_keystrokes("enter");
        app.read_with(cx, |app, _| {
            assert_eq!(
                app.active_hit, 0,
                "search navigation must work from sidebar"
            )
        });
        cx.simulate_keystrokes("shift-enter");
        app.read_with(cx, |app, _| assert_eq!(app.active_hit, 2));
        cx.simulate_keystrokes("escape");
        cx.update(|window, cx| app.read(cx).file_tree_focus.clone().focus(window, cx));
        draw_app(cx);
        cx.simulate_keystrokes("ctrl-f");
        app.read_with(cx, |app, _| {
            assert!(app.search_open, "Ctrl+F must work from the sidebar")
        });
    }

    #[gpui::test]
    fn search_bar_uses_one_row_without_result_candidates(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let mut app = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NativeMarkdownApp::new(None, window, cx));
            app = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.document.content = (0..5)
                    .map(|index| format!("A long result {index} contains Needle and enough surrounding text to preview.\n\n"))
                    .collect();
                app.refresh_analysis();
                app.show_search(window, cx);
                app.search_input
                    .update(cx, |input, cx| input.replace_all("needle", window, cx));
            })
        });
        cx.simulate_resize(size(px(1180.0), px(780.0)));
        draw_app(cx);

        let bar = cx.debug_bounds("search-bar").unwrap();
        assert!(
            cx.debug_bounds("search-result-0").is_none(),
            "search must not show candidate rows"
        );
        assert!(
            bar.size.height <= px(44.0),
            "search must stay in one compact row: {bar:?}"
        );
    }

    #[gpui::test]
    fn search_keeps_focus_and_centers_source_after_switching_modes(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(bind_keys);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source-search.md");
        let source = format!(
            "# Source\n\n{}Target Needle on this line.\n\n{}",
            "Filler line.\n\n".repeat(80),
            "Trailing line.\n\n".repeat(80)
        );
        std::fs::write(&path, &source).unwrap();
        let mut app = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NativeMarkdownApp::new(Some(path), window, cx));
            app = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.update(|_, cx| {
            app.update(cx, |app, cx| {
                app.editor
                    .update(cx, |editor, cx| editor.set_selected_range(0..8, cx));
            });
        });
        cx.simulate_resize(size(px(1180.0), px(780.0)));
        draw_app(cx);
        cx.simulate_keystrokes("ctrl-f");
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.search_input
                    .update(cx, |input, cx| input.replace_all("needle", window, cx))
            })
        });
        draw_app(cx);
        cx.simulate_keystrokes("enter");
        draw_app(cx);
        app.update(cx, |app, cx| app.set_view_mode(ViewMode::Split, cx));
        for _ in 0..4 {
            draw_app(cx);
        }
        app.read_with(cx, |app, cx| {
            assert_eq!(
                app.editor.read(cx).selected_range(),
                0..8,
                "search must preserve the editing selection"
            );
            let geometry = app.source_search.geometry();
            assert!(
                geometry.painted,
                "Source marker must be visible: {geometry:?}"
            );
            assert_eq!(geometry.text.as_ref(), "Needle");
            assert!(
                (geometry.bounds.unwrap().center().y - geometry.viewport.unwrap().center().y).abs()
                    < px(4.),
                "Source target must be centered: {geometry:?}"
            );
            assert_eq!(app.document.content, source);
        });
        cx.update(|window, cx| {
            assert!(
                app.read(cx)
                    .search_input
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window),
                "activating Source must not steal search input focus"
            );
        });
        cx.simulate_keystrokes("escape");
        app.read_with(cx, |app, cx| {
            assert!(
                app.source_search.highlight_ranges(cx).is_empty(),
                "closing search must clear decorations before the editor paints"
            );
        });
        draw_app(cx);
        app.read_with(cx, |app, cx| {
            assert!(
                app.source_search.highlight_ranges(cx).is_empty(),
                "closing search must remove editor decorations"
            );
            assert!(
                !app.source_search.geometry().painted,
                "closing search must remove the source marker"
            );
        });
        cx.update(|window, cx| {
            app.read(cx)
                .editor
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx)
        });
        draw_app(cx);
        cx.simulate_keystrokes("ctrl-f");
        app.read_with(cx, |app, _| {
            assert!(
                app.search_open,
                "editor's internal find must not swallow app Ctrl+F"
            )
        });
    }

    #[gpui::test]
    fn escape_closes_search_and_preserves_the_query(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);

        let mut app = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NativeMarkdownApp::new(None, window, cx));
            app = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let app = app.unwrap();

        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.search_input.update(cx, |input, cx| {
                    input.replace_all("needle", window, cx);
                });
                app.show_search(window, cx);
            });
            let _ = window.draw(cx);
        });

        cx.simulate_keystrokes("escape");

        app.read_with(cx, |app, cx| {
            assert!(!app.search_open);
            assert_eq!(app.search_query, "needle");
            assert_eq!(app.search_input.read(cx).value().as_ref(), "needle");
        });
    }

    #[gpui::test]
    fn unsaved_document_action_does_not_block_gpui_tasks(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);

        let (app, cx) = cx.add_window_view(|window, cx| NativeMarkdownApp::new(None, window, cx));

        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.document.content = "changed while editing".to_owned();
                app.view_mode = ViewMode::Split;
                app.request_document_action(DocumentAction::OpenDialog, window, cx);
            });
        });

        assert!(cx.has_pending_prompt());
        #[cfg(windows)]
        app.read_with(cx, |app, _| {
            assert!(app.dialog_in_flight);
            assert!(
                app.web_preview_allowed(1180.0),
                "a native modal dialog must not hide the reading surface"
            );
        });
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

        let (app, cx) = cx.add_window_view(|window, cx| NativeMarkdownApp::new(None, window, cx));

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

        let (app, cx) = cx.add_window_view(|window, cx| {
            let mut app = NativeMarkdownApp::new(Some(path), window, cx);
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

        let (app, cx) = cx
            .add_window_view(|window, cx| NativeMarkdownApp::new(Some(first.clone()), window, cx));
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
    fn recent_persistence_avoids_implicit_folder_refresh_and_duplicate_writes(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let directory = tempfile::tempdir().unwrap();
        let document = directory.path().join("document.md");
        let storage = directory.path().join("config/recent.json");
        std::fs::write(&document, "# Document").unwrap();
        let (app, cx) = cx.add_window_view(|window, cx| NativeMarkdownApp::new(None, window, cx));
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.recent = crate::recent::Recent::load_from(storage.clone());
                app.open_path(document.clone(), window, cx);
            });
        });
        assert!(storage.is_file(), "new file must be persisted before exit");
        std::fs::write(&storage, b"sentinel").unwrap();
        cx.run_until_parked();
        assert_eq!(
            std::fs::read(&storage).unwrap(),
            b"sentinel",
            "implicit folder scan must not write again"
        );
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.set_file_tree_root(Some(directory.path().to_path_buf()), window, cx);
                app.refresh_file_tree(window, cx); // Supersede the initial scan.
            });
        });
        cx.run_until_parked();
        app.read_with(cx, |app, _| {
            assert_eq!(app.recent.folders, vec![directory.path().to_path_buf()]);
        });
        assert_eq!(
            crate::recent::Recent::load_from(storage.clone()).folders,
            vec![directory.path().to_path_buf()]
        );
        std::fs::write(&storage, b"sentinel").unwrap();
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.open_path(document.clone(), window, cx);
                app.set_file_tree_root(Some(directory.path().to_path_buf()), window, cx);
                app.refresh_file_tree(window, cx);
            });
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(3));
        cx.run_until_parked();
        assert_eq!(
            std::fs::read(&storage).unwrap(),
            b"sentinel",
            "reopens and background maintenance must not persist"
        );
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.set_file_tree_root(Some(directory.path().join("missing")), window, cx);
            });
        });
        cx.run_until_parked();
        assert_eq!(
            std::fs::read(&storage).unwrap(),
            b"sentinel",
            "failed folder opens must not persist"
        );
    }

    #[gpui::test]
    fn recent_files_track_successful_opens_without_recording_failures(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.md");
        let second = directory.path().join("second.md");
        std::fs::write(&first, "# First").unwrap();
        std::fs::write(&second, "# Second").unwrap();
        let (app, cx) = cx
            .add_window_view(|window, cx| NativeMarkdownApp::new(Some(first.clone()), window, cx));
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                assert_eq!(app.recent.files, vec![first.clone()]);
                app.open_tree_path(second.clone(), window, cx);
                assert_eq!(app.recent.files, vec![second.clone(), first.clone()]);
                app.request_document_action(DocumentAction::OpenPath(first.clone()), window, cx);
                assert_eq!(app.document.path.as_ref(), Some(&first));
                assert_eq!(app.recent.files, vec![first.clone(), second.clone()]);
                app.open_path(directory.path().join("missing.md"), window, cx);
                assert_eq!(app.document.path.as_ref(), Some(&first));
                assert_eq!(app.recent.files, vec![first.clone(), second.clone()]);
            });
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

        let (app, cx) =
            cx.add_window_view(|window, cx| NativeMarkdownApp::new(Some(first), window, cx));
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
    fn reading_width_controls_preserve_document_and_mode(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let mut app = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NativeMarkdownApp::new(None, window, cx));
            app = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.simulate_resize(size(px(1180.0), px(980.0)));
        app.update(cx, |app, cx| {
            app.settings_open = true;
            app.view_mode = ViewMode::Split;
            cx.notify();
        });
        for (selector, width, follow) in [
            ("reading-width-720", 720, true),
            ("reading-width-follow-zoom", 720, false),
            ("reading-width-1280", 1280, false),
            ("reading-width-0", 0, false),
            ("reading-width-follow-zoom", 0, false), // Disabled while using full width.
            ("reading-width-980", 980, false),
        ] {
            draw_app(cx);
            let position = cx.debug_bounds(selector).unwrap().center();
            cx.simulate_mouse_down(position, gpui::MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_up(position, gpui::MouseButton::Left, Modifiers::default());
            draw_app(cx);
            app.read_with(cx, |app, _| {
                assert_eq!(app.reading_width, width, "{selector}");
                assert_eq!(app.reading_width_follows_zoom, follow, "{selector}");
                assert_eq!(app.view_mode, ViewMode::Split);
                assert!(app.document.content.is_empty());
                assert!(!app.document.is_dirty());
            });
        }
    }

    #[gpui::test]
    fn outline_navigation_preserves_preview_split_and_source_modes(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("outline-modes.md");
        let source = "# First\n\nFirst body.\n\n## Child\n\nChild body.\n\n# Last\n\nLast body.";
        std::fs::write(&path, source).unwrap();
        let mut app = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NativeMarkdownApp::new(Some(path), window, cx));
            app = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.simulate_resize(size(px(1180.0), px(780.0)));
        for mode in [ViewMode::Split, ViewMode::Source, ViewMode::Preview] {
            app.update(cx, |app, cx| app.set_view_mode(mode, cx));
            for (selector, heading, focused) in [
                ("outline-jump-button", None, None),
                ("outline-heading-label-0", Some(0), None),
                ("outline-focus-button", Some(0), Some(0)),
                ("outline-heading-label-1", Some(1), Some(1)),
                ("outline-full-document-label", None, None),
            ] {
                draw_app(cx);
                let position = cx.debug_bounds(selector).unwrap().center();
                cx.simulate_mouse_down(position, gpui::MouseButton::Left, Modifiers::default());
                cx.simulate_mouse_up(position, gpui::MouseButton::Left, Modifiers::default());
                draw_app(cx);
                app.read_with(cx, |app, _| {
                    assert_eq!(
                        app.view_mode, mode,
                        "{selector} must preserve the current view"
                    );
                    assert_eq!(app.selected_heading, heading);
                    assert_eq!(app.focused_section, focused);
                    assert_eq!(app.document.content, source);
                    assert_eq!(
                        app.preview_source().0.as_ref(),
                        match focused {
                            Some(0) => "# First\n\nFirst body.\n\n## Child\n\nChild body.\n\n",
                            Some(1) => "## Child\n\nChild body.\n\n",
                            _ => source,
                        }
                    );
                });
            }
        }
    }

    #[gpui::test]
    fn sidebars_render_on_opposite_sides_and_hide_responsively(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("layout.md");
        std::fs::write(&path, "# Layout\n\n## Section").unwrap();

        let (app, cx) =
            cx.add_window_view(|window, cx| NativeMarkdownApp::new(Some(path), window, cx));

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
        let full_label = cx.debug_bounds("outline-full-document-label").unwrap();
        let heading_label = cx.debug_bounds("outline-heading-label-0").unwrap();
        let nested_label = cx.debug_bounds("outline-heading-label-1").unwrap();
        assert!(full_label.left() < outline.left() + px(40.0));
        assert!(heading_label.left() < outline.left() + px(40.0));
        assert!(
            (nested_label.left() - heading_label.left() - px(14.0)).abs() < px(1.0),
            "outline indentation should depend on heading level, not label width"
        );
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
    fn focused_section_preview_includes_descendant_sections(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("focused-subtree.md");
        std::fs::write(
            &path,
            "# Parent\nparent\n## Child\nchild\n### Grandchild\ngrandchild\n# Next\nnext",
        )
        .unwrap();

        let (app, cx) = cx.add_window_view(|window, cx| {
            let mut app = NativeMarkdownApp::new(Some(path), window, cx);
            app.focused_section = Some(0);
            app
        });

        app.read_with(cx, |app, _| {
            let (source, offset) = app.preview_source();
            assert_eq!(offset, 0);
            assert_eq!(
                source.to_string(),
                "# Parent\nparent\n## Child\nchild\n### Grandchild\ngrandchild\n"
            );
        });
    }

    #[test]
    fn view_mode_labels_are_stable() {
        assert_eq!(ViewMode::Preview.label(), "Preview");
        assert_eq!(ViewMode::Split.label(), "Split");
        assert_eq!(ViewMode::Source.label(), "Source");
    }

    #[gpui::test]
    fn saved_notice_expires_without_clearing_a_later_error(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("saved.md");
        std::fs::write(&path, "# Original").unwrap();
        let (app, cx) =
            cx.add_window_view(|window, cx| NativeMarkdownApp::new(Some(path.clone()), window, cx));
        app.update(cx, |app, cx| {
            assert!(app.notice.is_none(), "opening must not announce a save");
            app.document.content = "# Changed".into();
            assert!(app.save_existing(cx));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "# Changed");
            assert_eq!(app.notice.as_ref().unwrap().text, "Saved");
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(900));
        cx.run_until_parked();
        app.read_with(cx, |app, _| assert!(app.notice.is_some()));
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        app.read_with(cx, |app, _| assert!(app.notice.is_none()));

        app.update(cx, |app, cx| {
            assert!(app.save_existing(cx));
            assert!(
                app.notice.is_none(),
                "unchanged save must not announce a write"
            );
            app.document.content = "# Changed again".into();
            assert!(app.save_existing(cx));
            app.set_notice("Could not save document", true);
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(1_100));
        cx.run_until_parked();
        app.read_with(cx, |app, _| {
            let notice = app
                .notice
                .as_ref()
                .expect("save timer must preserve the error");
            assert!(notice.is_error);
            assert_eq!(notice.text, "Could not save document");
        });
    }

    #[gpui::test]
    fn background_maintenance_only_redraws_for_visible_changes(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);

        let (app, cx) = cx.add_window_view(|window, cx| NativeMarkdownApp::new(None, window, cx));

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

        let (app, cx) = cx.add_window_view(|window, cx| NativeMarkdownApp::new(None, window, cx));

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

    #[gpui::test]
    fn document_zoom_does_not_change_the_application_theme(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);

        let (app, cx) = cx.add_window_view(|window, cx| NativeMarkdownApp::new(None, window, cx));

        cx.update(|window, cx| {
            let initial_font_size = Theme::global(cx).font_size;
            let initial_mono_font_size = Theme::global(cx).mono_font_size;

            app.update(cx, |app, cx| app.zoom_in(window, cx));

            assert_eq!(Theme::global(cx).font_size, initial_font_size);
            assert_eq!(Theme::global(cx).mono_font_size, initial_mono_font_size);
        });
    }

    #[gpui::test]
    #[cfg(windows)]
    fn web_search_ack_restores_only_the_current_open_search(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (app, cx) = cx.add_window_view(|window, cx| NativeMarkdownApp::new(None, window, cx));
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.document.content = "# Search\n\nNeedle".into();
                app.search_query = "needle".into();
                app.refresh_analysis();
                app.show_search(window, cx);
                app.next_hit(window, cx);
                let request = app.search_request;
                app.app_focus.focus(window, cx);
                app.web_message(
                    &serde_json::json!({"kind":"search-applied","request":request.wrapping_sub(1)})
                        .to_string(),
                    window,
                    cx,
                );
                assert!(app.app_focus.is_focused(window));
                app.web_message(
                    &serde_json::json!({"kind":"search-applied","request":request}).to_string(),
                    window,
                    cx,
                );
                assert!(app
                    .search_input
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window));
                app.close_search(window, cx);
                app.web_message(
                    &serde_json::json!({"kind":"search-applied","request":request}).to_string(),
                    window,
                    cx,
                );
                assert!(app.app_focus.is_focused(window));
            });
        });
    }

    #[gpui::test]
    fn bitmap_paste_is_atomic_and_plain_paste_keeps_text(cx: &mut TestAppContext) {
        use base64::Engine as _;
        cx.update(gpui_component::init);
        cx.update(bind_keys);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("paste.md");
        std::fs::write(&path, "before ").unwrap();
        let (app, cx) = cx.add_window_view(|window, cx| {
            let mut app = NativeMarkdownApp::new(Some(path.clone()), window, cx);
            app.view_mode = ViewMode::Split;
            app
        });
        cx.simulate_resize(size(px(1180.0), px(780.0)));
        let bytes=base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==").unwrap();
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.editor.update(cx, |editor, cx| {
                    editor.set_selected_range(7..7, cx);
                    editor.focus(window, cx);
                })
            });
            cx.write_to_clipboard(gpui::ClipboardItem::new_image(&gpui::Image::from_bytes(
                gpui::ImageFormat::Png,
                bytes,
            )));
        });
        draw_app(cx);
        cx.simulate_keystrokes("ctrl-v");
        cx.run_until_parked();
        draw_app(cx);
        let pasted = app.read_with(cx, |app, _| {
            assert!(app.pending_paste.is_none());
            assert!(app.document.content.contains("paste.assets/nm-"));
            app.document.content.clone()
        });
        assert_eq!(
            std::fs::read_dir(directory.path().join("paste.assets"))
                .unwrap()
                .count(),
            1
        );
        cx.simulate_keystrokes("ctrl-z");
        draw_app(cx);
        app.read_with(cx, |app, _| assert_eq!(app.document.content, "before "));
        cx.simulate_keystrokes("ctrl-y");
        draw_app(cx);
        app.read_with(cx, |app, _| assert_eq!(app.document.content, pasted));
        cx.update(|_, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string("<b>literal</b>".into()))
        });
        cx.simulate_keystrokes("ctrl-shift-v");
        draw_app(cx);
        app.read_with(cx, |app, _| {
            assert!(app.document.content.contains("<b>literal</b>"))
        });
        cx.simulate_keystrokes("ctrl-s");
        draw_app(cx);
        assert!(Document::open(path)
            .unwrap()
            .content
            .contains("paste.assets/nm-"));
    }

    #[gpui::test]
    fn rich_paste_replaces_selection_in_one_undo(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(bind_keys);
        let (app, cx) = cx.add_window_view(|window, cx| {
            let mut app = NativeMarkdownApp::new(None, window, cx);
            app.view_mode = ViewMode::Source;
            app
        });
        cx.simulate_resize(size(px(1180.0), px(780.0)));
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.editor.update(cx, |editor, cx| {
                    editor.set_value("前🦀 replace 尾", window, cx);
                    editor.set_selected_range(8..15, cx);
                    editor.focus(window, cx);
                });
                app.begin_paste(
                    crate::paste::Snapshot {
                        html: Some("<b>粘贴</b><a href='https://example.com'>链接</a>".into()),
                        ..Default::default()
                    },
                    window,
                    cx,
                );
            })
        });
        cx.run_until_parked();
        draw_app(cx);
        let pasted = app.read_with(cx, |app, _| {
            assert!(app.pending_paste.is_none());
            assert!(app.document.content.contains("**粘贴**"));
            assert!(app.document.content.starts_with("前🦀 "));
            app.document.content.clone()
        });
        cx.simulate_keystrokes("ctrl-z");
        draw_app(cx);
        app.read_with(cx, |app, _| {
            assert_eq!(app.document.content, "前🦀 replace 尾")
        });
        cx.simulate_keystrokes("ctrl-y");
        draw_app(cx);
        app.read_with(cx, |app, _| assert_eq!(app.document.content, pasted));
    }

    #[gpui::test]
    fn pending_paste_rebases_edits_and_rejects_cancelled_results(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(bind_keys);
        let (app, cx) = cx.add_window_view(|window, cx| {
            let mut app = NativeMarkdownApp::new(None, window, cx);
            app.view_mode = ViewMode::Source;
            app
        });
        cx.simulate_resize(size(px(1180.0), px(780.0)));
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.document.content = "abc".into();
                app.editor
                    .update(cx, |e, cx| e.set_value("abc", window, cx));
            })
        });
        draw_app(cx);
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.pending_paste = Some(PendingPaste {
                    id: 1,
                    range: 3..3,
                    cancelled: Arc::new(AtomicBool::new(false)),
                    prepared: None,
                });
                app.editor.update(cx, |editor, cx| {
                    editor.set_selected_range(0..0, cx);
                    editor.replace("🦀", window, cx);
                    editor.focus(window, cx);
                });
            })
        });
        draw_app(cx);
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.complete_paste(
                    1,
                    crate::paste::Finished {
                        markdown: "PASTE".into(),
                        failed: 0,
                    },
                    window,
                    cx,
                )
            })
        });
        draw_app(cx);
        app.read_with(cx, |app, cx| {
            assert_eq!(app.document.content, "🦀abcPASTE");
            assert_eq!(app.editor.read(cx).selected_range(), 4..4);
        });
        cx.update(|_, cx| {
            app.update(cx, |app, _| {
                app.pending_paste = Some(PendingPaste {
                    id: 2,
                    range: 4..4,
                    cancelled: Arc::new(AtomicBool::new(false)),
                    prepared: None,
                })
            })
        });
        cx.simulate_keystrokes("escape");
        draw_app(cx);
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.complete_paste(
                    2,
                    crate::paste::Finished {
                        markdown: "LATE".into(),
                        failed: 0,
                    },
                    window,
                    cx,
                );
                assert!(!app.editor.read(cx).value().contains("LATE"));
                app.pending_paste = Some(PendingPaste {
                    id: 3,
                    range: 4..4,
                    cancelled: Arc::new(AtomicBool::new(false)),
                    prepared: None,
                });
                app.replace_document(Document::new_document(), window, cx);
                app.complete_paste(
                    3,
                    crate::paste::Finished {
                        markdown: "WRONG".into(),
                        failed: 0,
                    },
                    window,
                    cx,
                );
            })
        });
        draw_app(cx);
        app.read_with(cx, |app, _| {
            assert_eq!(app.document.content, "# Untitled\n\n")
        });
    }

    #[gpui::test]
    fn upstream_editor_updates_document_undoes_and_saves_exact_text(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(bind_keys);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("edit.md");
        let original = "# 编辑\r\n\r\nOriginal 中文.\r\n";
        std::fs::write(&path, original).unwrap();
        let (app, cx) = cx.add_window_view(|window, cx| {
            let mut app = NativeMarkdownApp::new(Some(path.clone()), window, cx);
            app.view_mode = ViewMode::Source;
            app
        });
        cx.simulate_resize(size(px(1180.0), px(780.0)));
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.editor.update(cx, |editor, cx| {
                    editor.set_selected_range(original.len()..original.len(), cx);
                    editor.focus(window, cx);
                });
            });
        });
        draw_app(cx);
        cx.simulate_input("追加内容 🦀");
        draw_app(cx);
        let expected = format!("{original}追加内容 🦀");
        app.read_with(cx, |app, _| {
            assert_eq!(app.document.content, expected);
            assert!(app.document.is_dirty());
        });
        cx.simulate_keystrokes("ctrl-z");
        draw_app(cx);
        app.read_with(cx, |app, _| assert_eq!(app.document.content, original));
        cx.simulate_keystrokes("ctrl-y");
        draw_app(cx);
        app.read_with(cx, |app, _| assert_eq!(app.document.content, expected));
        cx.simulate_keystrokes("ctrl-s");
        draw_app(cx);
        assert_eq!(std::fs::read(&path).unwrap(), expected.as_bytes());
        app.read_with(cx, |app, _| assert!(!app.document.is_dirty()));
    }

    #[gpui::test]
    fn ctrl_wheel_over_editor_zooms_without_scrolling_in_source_and_split(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        std::fs::create_dir_all("tmp").unwrap();
        let directory = tempfile::tempdir_in("tmp").unwrap();
        let path = directory.path().join("editor-wheel.md");
        let source = (0..200).map(|i| format!("Line {i}\n")).collect::<String>();
        std::fs::write(&path, &source).unwrap();
        let (app, cx) =
            cx.add_window_view(|window, cx| NativeMarkdownApp::new(Some(path), window, cx));
        cx.simulate_resize(size(px(1180.), px(780.)));
        for mode in [ViewMode::Source, ViewMode::Split] {
            cx.update(|window, cx| {
                app.update(cx, |app, cx| {
                    app.set_view_mode(mode, cx);
                    app.zoom = ZoomLevel::default();
                    app.editor.update(cx, |editor, cx| {
                        editor.set_selected_range(0..4, cx);
                        editor.focus(window, cx);
                    });
                })
            });
            draw_app(cx);
            let toolbar = cx.debug_bounds("application-toolbar").unwrap();
            let (position, offset) = app.read_with(cx, |app, cx| {
                let editor = app.editor.read(cx);
                (editor.input_bounds().center(), editor.scroll_offset())
            });
            cx.simulate_event(gpui::ScrollEvent {
                position,
                delta: gpui::ScrollDelta::Pixels(point(px(0.), px(40.))),
                modifiers: Modifiers {
                    control: true,
                    ..Default::default()
                },
                touch_phase: gpui::TouchPhase::Moved,
                source: gpui::ScrollSource::Pointer,
            });
            draw_app(cx);
            app.read_with(cx, |app, cx| {
                assert_eq!(
                    app.zoom.percent(),
                    110,
                    "Ctrl+wheel must reach document zoom in {mode:?}"
                );
                let editor = app.editor.read(cx);
                assert_eq!(editor.scroll_offset(), offset, "zoom must not also scroll");
                assert_eq!(editor.selected_range(), 0..4);
                assert_eq!(editor.value().as_ref(), source);
            });
            assert_eq!(cx.debug_bounds("application-toolbar").unwrap(), toolbar);
            cx.simulate_event(gpui::ScrollEvent {
                position,
                delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-60.))),
                modifiers: Modifiers::default(),
                touch_phase: gpui::TouchPhase::Moved,
                source: gpui::ScrollSource::Pointer,
            });
            draw_app(cx);
            app.read_with(cx, |app, cx| {
                assert_eq!(app.zoom.percent(), 110);
                assert_ne!(
                    app.editor.read(cx).scroll_offset(),
                    offset,
                    "ordinary wheel still scrolls"
                );
            });
        }
    }

    #[gpui::test]
    fn document_zoom_keeps_toolbar_at_fixed_height(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);

        let (app, cx) = cx.add_window_view(|window, cx| NativeMarkdownApp::new(None, window, cx));
        cx.simulate_resize(size(px(1180.0), px(780.0)));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let toolbar_before = cx.debug_bounds("application-toolbar").unwrap();

        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.zoom = ZoomLevel::from_factor(2.5);
                app.apply_zoom(window, cx);
            });
            let _ = window.draw(cx);
        });

        assert_eq!(
            cx.debug_bounds("application-toolbar").unwrap().size.height,
            toolbar_before.size.height
        );
    }
}
