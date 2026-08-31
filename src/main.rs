mod app;
mod benchmark;
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
    let benchmark_config = match benchmark::BenchmarkConfig::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("NATIVE_MARKDOWN_BENCHMARK event=error message={error:?}");
            std::process::exit(2);
        }
    };
    let benchmark_enabled = benchmark_config.is_some();
    let benchmark_outcome = benchmark::new_outcome();
    let outcome_for_app = benchmark_outcome.clone();
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
                        if let Some(config) = benchmark_config.clone() {
                            benchmark::start(
                                config,
                                app.downgrade(),
                                outcome_for_app.clone(),
                                window,
                                cx,
                            );
                        }
                        let weak_app = app.downgrade();
                        window.on_window_should_close(cx, move |window, cx| {
                            weak_app
                                .update(cx, |app, cx| app.should_close(window, cx))
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

    if benchmark_enabled {
        let outcome = benchmark_outcome
            .lock()
            .expect("benchmark outcome lock poisoned");
        let exit_code = match outcome.as_ref() {
            Some(Ok(report)) if report.passed => 0,
            Some(Ok(_)) => 1,
            Some(Err(_)) | None => 2,
        };
        std::process::exit(exit_code);
    }
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
