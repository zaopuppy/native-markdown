mod app;
mod document;
mod image_cache;
mod image_loader;
mod markdown;
mod zoom;

use std::path::PathBuf;
use std::sync::Arc;

use app::NativeMarkdownApp;
use gpui::{px, size, App, AppContext, Application, Bounds, WindowBounds, WindowOptions};
use gpui_component::{Root, Theme};
use image_loader::{DocumentImageClient, DocumentImageRoot};

fn main() {
    let initial_path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .filter(|path| path.is_file());
    let image_root = DocumentImageRoot::default();
    let app = Application::new();

    app.run(move |cx| {
        gpui_component::init(cx);
        configure_theme(cx);
        app::bind_keys(cx);

        let fallback_http_client = cx.http_client();
        cx.set_http_client(Arc::new(DocumentImageClient::new(
            image_root.clone(),
            env_flag("NATIVE_MARKDOWN_REMOTE_IMAGES"),
            fallback_http_client,
        )));

        let bounds = Bounds::centered(None, size(px(1180.0), px(780.0)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                {
                    let image_root = image_root.clone();
                    move |window, cx| {
                        let app = cx.new(|cx| {
                            NativeMarkdownApp::new(initial_path.clone(), image_root, window, cx)
                        });
                        let weak_app = app.downgrade();
                        window.on_window_should_close(cx, move |_, cx| {
                            weak_app
                                .update(cx, |app, cx| app.should_close(cx))
                                .unwrap_or(true)
                        });
                        cx.new(|cx| Root::new(app, window, cx))
                    }
                },
            )
            .expect("failed to open Native Markdown window");

        let _ = window.update(cx, |_, window, _| window.activate_window());
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
    });
}

fn configure_theme(cx: &mut App) {
    let theme = Theme::global_mut(cx);
    theme.font_family = ".SystemUIFont".into();
    theme.font_size = px(16.0);
    theme.mono_font_family = "Consolas".into();
    theme.mono_font_size = px(13.0);
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "yes"))
}
