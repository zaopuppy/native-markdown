use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{Application, Context, SharedString, Window, WindowOptions, div, prelude::*};
use gpui_component::{Root, StyledExt, text::TextView};

mod local_images;
#[path = "../../shared/metrics.rs"]
mod metrics;
#[path = "../../shared/scroll_speed.rs"]
mod scroll_speed;
use local_images::{LocalImageHttpClient, local_image_load_count};
use metrics::FrameMetrics;

fn main() {
    let process_started = Instant::now();
    let path = document_argument();
    let markdown: Arc<str> = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
        .into();
    let base_dir = path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();
    let app = Application::new();
    let exit_after_seconds = std::env::var("MARKDOWN_SPIKE_SECONDS")
        .ok()
        .and_then(|value| value.parse::<f32>().ok());
    println!(
        "MARKDOWN_SPIKE_CONFIG renderer=gpui-text-view wheel_lines={} wheel_points_per_notch={:.1}",
        scroll_speed::system_wheel_lines(),
        scroll_speed::comparison_points_per_notch(),
    );

    app.run(move |cx| {
        let fallback_http_client = cx.http_client();
        cx.set_http_client(Arc::new(LocalImageHttpClient::new(
            base_dir,
            env_flag("MARKDOWN_SPIKE_REMOTE_IMAGES"),
            fallback_http_client,
        )));
        gpui_component::init(cx);
        let path = path.clone();
        let markdown = markdown.clone();
        cx.spawn(async move |cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                let view = cx.new(|_| SpikeView {
                    markdown: SharedString::from(markdown.to_string()),
                    path,
                    metrics: FrameMetrics::new(process_started),
                    first_render: true,
                });
                if exit_after_seconds.is_some() {
                    let window_handle = window.window_handle();
                    cx.spawn(async move |cx| {
                        let mut peak_rss_mib = 0.0_f64;
                        loop {
                            smol::Timer::after(Duration::from_millis(16)).await;
                            let elapsed = process_started.elapsed().as_secs_f32();
                            let rss_mib = metrics::current_rss_mib();
                            peak_rss_mib = peak_rss_mib.max(rss_mib);
                            if exit_after_seconds.is_some_and(|seconds| elapsed >= seconds) {
                                println!(
                                    "MARKDOWN_SPIKE_METRIC renderer=gpui-text-view event=finished elapsed_ms={:.2} rss_mib={rss_mib:.2} peak_rss_mib={peak_rss_mib:.2} local_images_loaded={}",
                                    elapsed * 1_000.0,
                                    local_image_load_count(),
                                );
                                let _ = window_handle.update(cx, |_, _, cx| cx.quit());
                                break;
                            }
                        }
                    })
                    .detach();
                }
                cx.new(|cx| Root::new(view, window, cx))
            })?;
            Ok::<_, anyhow::Error>(())
        })
        .detach();
    });
}

fn document_argument() -> PathBuf {
    std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .unwrap_or_else(|| panic!("usage: markdown-spike-gpui-text-view <document.md>"))
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "yes"))
}

struct SpikeView {
    markdown: SharedString,
    path: PathBuf,
    metrics: FrameMetrics,
    first_render: bool,
}

impl Render for SpikeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.metrics.tick("gpui-text-view");
        if self.first_render {
            self.first_render = false;
            println!(
                "MARKDOWN_SPIKE_METRIC renderer=gpui-text-view event=document_loaded bytes={}",
                self.markdown.len()
            );
        }

        div()
            .v_flex()
            .size_full()
            .child(div().px_3().py_2().border_b_1().child(format!(
                "PROTOTYPE | gpui-component 0.5.1 | wheel {:.0} pt/notch | {} | local images {} | {}",
                scroll_speed::comparison_points_per_notch(),
                self.path.display(),
                local_image_load_count(),
                self.metrics.summary()
            )))
            .child(
                div().flex_1().min_h_0().px_4().child(
                    TextView::markdown("benchmark-document", self.markdown.clone(), window, cx)
                        .selectable(true)
                        .scrollable(true),
                ),
            )
    }
}
