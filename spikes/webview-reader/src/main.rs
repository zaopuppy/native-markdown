#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[path = "../../../src/web_document.rs"]
mod web_document;

use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Instant,
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};
use wry::{WebViewBuilder, WebViewBuilderExtWindows};

#[derive(Default)]
struct Poc {
    window: Option<Window>,
    webview: Option<wry::WebView>,
}

impl ApplicationHandler for Poc {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let started = Instant::now();
        let window = event_loop
            .create_window(
                Window::default_attributes()
                    .with_title("Markdown WebView PoC")
                    .with_inner_size(winit::dpi::LogicalSize::new(1180, 780)),
            )
            .unwrap();
        let path = std::env::args_os().nth(1).map(PathBuf::from);
        let source = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        let resources = Arc::new(RwLock::new(web_document::Resources::default()));
        resources.write().unwrap().render(&source, path.as_deref());
        let protocol_resources = resources.clone();
        let mut context = wry::WebContext::new(Some(
            std::env::var_os("NATIVE_MARKDOWN_WEB_PROFILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("native-markdown-poc")),
        ));
        let webview = WebViewBuilder::new_with_web_context(&mut context)
            .with_additional_browser_args(
                &std::env::var("NATIVE_MARKDOWN_WEB_TEST_ARGS").unwrap_or_default(),
            )
            .with_custom_protocol("reader".into(), move |_, request| {
                web_document::respond(&protocol_resources, request.uri().path())
            })
            .with_navigation_handler(|url| {
                url == format!("{}/", web_document::ORIGIN)
                    || url.starts_with(&format!("{}/#", web_document::ORIGIN))
            })
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_ipc_handler(move |request| {
                println!(
                    "READER_EVENT elapsed_ms={} body={}",
                    started.elapsed().as_millis(),
                    request.body()
                )
            })
            .with_url("reader://localhost/")
            .build(&window)
            .unwrap();
        self.window = Some(window);
        self.webview = Some(webview);
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(view) = &self.webview {
                    let _ = view.set_bounds(wry::Rect {
                        position: wry::dpi::PhysicalPosition::new(0, 0).into(),
                        size: wry::dpi::PhysicalSize::new(size.width, size.height).into(),
                    });
                }
            }
            _ => {}
        }
    }
}

fn main() {
    EventLoop::new()
        .unwrap()
        .run_app(&mut Poc::default())
        .unwrap();
}
