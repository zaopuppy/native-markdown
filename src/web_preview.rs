//! Windows-only child WebView. The native app owns files, editing, dialogs and commands.
use crate::web_document::{self, Resources, ORIGIN};
use futures::channel::mpsc::UnboundedSender;
use gpui::{canvas, prelude::*, Bounds, Pixels, SharedString, Window};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, RwLock},
    time::Instant,
};
use wry::raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, WindowHandle};
use wry::{WebViewBuilder, WebViewBuilderExtWindows};

/// A native parent handle detached from GPUI's AppCell/Window borrows.
/// Only used synchronously on the UI thread while the application owns the parent window.
pub struct PreviewParent(RawWindowHandle);

impl PreviewParent {
    pub fn capture(window: &Window) -> anyhow::Result<Self> {
        Ok(Self(HasWindowHandle::window_handle(window)?.as_raw()))
    }
}

impl HasWindowHandle for PreviewParent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // SAFETY: capture is called for the live GPUI parent. Creation runs immediately on the
        // same UI thread, without an await, and GPUI retains ownership of that native window.
        Ok(unsafe { WindowHandle::borrow_raw(self.0) })
    }
}

pub enum PreviewEvent {
    Message(String),
    DroppedFile(PathBuf),
}

pub struct WebPreview {
    view: wry::WebView,
    _context: wry::WebContext,
    resources: Arc<RwLock<Resources>>,
    source: SharedString,
    document: Option<PathBuf>,
    visible: bool,
    allowed: bool,
    bounds: Option<Bounds<Pixels>>,
    scope: usize,
    configuration: String,
    ready: bool,
    pending_reload: Option<bool>,
}

impl WebPreview {
    pub fn new(
        parent: &PreviewParent,
        source: SharedString,
        document: Option<&Path>,
        sender: UnboundedSender<PreviewEvent>,
    ) -> anyhow::Result<Self> {
        let resources = Arc::new(RwLock::new(Resources::default()));
        resources.write().unwrap().render(&source, document);
        let protocol = resources.clone();
        let profile = std::env::var_os("NATIVE_MARKDOWN_WEB_PROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(std::env::temp_dir)
                    .join("NativeMarkdown/WebView2")
            });
        let mut context = wry::WebContext::new(Some(profile));
        let began = Instant::now();
        let drop_sender = sender.clone();
        let mut builder = WebViewBuilder::new_with_web_context(&mut context)
            .with_bounds(wry::Rect::default())
            .with_visible(false)
            .with_browser_accelerator_keys(false)
            .with_custom_protocol("reader".into(), move |_, request| {
                web_document::respond(&protocol, request.uri().path())
            })
            .with_navigation_handler(|url| {
                url == format!("{ORIGIN}/") || url.starts_with(&format!("{ORIGIN}/#"))
            })
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_permission_handler(|_| wry::PermissionResponse::Deny)
            .with_download_started_handler(|_, _| false)
            .with_drag_drop_handler(move |event| {
                if let wry::DragDropEvent::Drop { paths, .. } = event {
                    if let Some(path) = paths.into_iter().find(|path| path.is_file()) {
                        let _ = drop_sender.unbounded_send(PreviewEvent::DroppedFile(path));
                    }
                }
                true
            })
            .with_ipc_handler(move |request| {
                if request.uri().scheme_str() != Some("http")
                    || request.uri().host() != Some("reader.localhost")
                    || request.body().len() > 4096
                {
                    return;
                }
                if std::env::var_os("NATIVE_MARKDOWN_BENCHMARK").is_some()
                    || std::env::var_os("NATIVE_MARKDOWN_WEB_TEST_ARGS").is_some()
                {
                    println!(
                        "READER_EVENT elapsed_ms={} process_ms={} body={}",
                        began.elapsed().as_millis(),
                        crate::process_elapsed_ms(),
                        request.body()
                    );
                }
                let _ = sender.unbounded_send(PreviewEvent::Message(request.into_body()));
            })
            .with_url("reader://localhost/");
        // Explicit test opt-in only; no remote debugging listener in normal application launches.
        if let Ok(args) = std::env::var("NATIVE_MARKDOWN_WEB_TEST_ARGS") {
            builder = builder.with_additional_browser_args(args);
        }
        let view = builder.build_as_child(parent)?;
        Ok(Self {
            view,
            _context: context,
            resources,
            source,
            document: document.map(Path::to_path_buf),
            visible: false,
            allowed: true,
            bounds: None,
            scope: 0,
            configuration: String::new(),
            ready: false,
            pending_reload: None,
        })
    }

    pub fn update(
        &mut self,
        source: SharedString,
        document: Option<&Path>,
        scope: usize,
        settings: serde_json::Value,
    ) {
        let changed_document = self.document.as_deref() != document;
        let changed_scope = self.scope != scope;
        if self.source != source || changed_document || changed_scope {
            self.resources.write().unwrap().render(&source, document);
            self.source = source;
            self.document = document.map(Path::to_path_buf);
            self.scope = scope;
            self.pending_reload = Some(changed_document || changed_scope);
        }
        let configuration = settings.to_string();
        if self.ready && (configuration != self.configuration || self.pending_reload.is_some()) {
            if let Some(reset) = self.pending_reload.take() {
                self.eval(&format!("window.readerReload?.({reset})"));
            }
            self.eval(&format!("window.readerConfigure?.({configuration})"));
        }
        self.configuration = configuration;
    }

    pub fn on_ready(&mut self) {
        self.ready = true;
        if let Some(reset) = self.pending_reload.take() {
            self.eval(&format!("window.readerReload?.({reset})"));
        }
        if !self.configuration.is_empty() {
            self.eval(&format!("window.readerConfigure?.({})", self.configuration));
        }
    }

    pub fn image_count(&self) -> usize {
        self.resources.read().unwrap().image_count()
    }

    pub fn link_target(&self, id: &str) -> Option<PathBuf> {
        self.resources.read().unwrap().link_target(id)
    }

    pub fn eval(&self, script: &str) {
        let _ = self.view.evaluate_script(script);
    }

    pub fn focus_parent(&self) {
        let _ = self.view.focus_parent();
    }

    pub fn set_allowed(&mut self, allowed: bool) {
        self.allowed = allowed;
        if !allowed {
            self.set_visible(false);
        }
    }

    pub fn set_visible(&mut self, visible: bool) {
        if visible != self.visible {
            if !visible {
                let _ = self.view.focus_parent();
            }
            let _ = self.view.set_visible(visible);
            self.visible = visible;
        }
    }

    pub fn surface(view: Rc<RefCell<Self>>) -> impl IntoElement {
        canvas(
            move |bounds, _, _| {
                let mut preview = view.borrow_mut();
                if preview.bounds != Some(bounds) {
                    let _ = preview.view.set_bounds(wry::Rect {
                        position: wry::dpi::LogicalPosition::new(
                            f64::from(bounds.origin.x),
                            f64::from(bounds.origin.y),
                        )
                        .into(),
                        size: wry::dpi::LogicalSize::new(
                            f64::from(bounds.size.width),
                            f64::from(bounds.size.height),
                        )
                        .into(),
                    });
                    preview.bounds = Some(bounds);
                }
                let allowed = preview.allowed;
                preview.set_visible(allowed);
            },
            |_, _, _, _| {},
        )
        .size_full()
    }
}
