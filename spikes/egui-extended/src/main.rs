use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use eframe::egui;
use egui_commonmark_extended::{CommonMarkCache, CommonMarkViewer};

mod fonts;
#[path = "../../shared/metrics.rs"]
mod metrics;
#[path = "../../shared/scroll_speed.rs"]
mod scroll_speed;
use metrics::FrameMetrics;

fn main() -> eframe::Result<()> {
    let process_started = Instant::now();
    let path = document_argument();
    let source: Arc<str> = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
        .into();
    let base_dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("PROTOTYPE — egui_commonmark_extended")
            .with_inner_size([900.0, 760.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Markdown renderer spike",
        options,
        Box::new(move |cc| {
            configure_renderer(&cc.egui_ctx);
            Ok(Box::new(SpikeApp::new(
                source,
                path,
                base_dir,
                process_started,
            )))
        }),
    )
}

fn configure_renderer(ctx: &egui::Context) {
    let wheel_points = scroll_speed::comparison_points_per_notch();
    ctx.options_mut(|options| {
        options.reduce_texture_memory = true;
        options.input_options.line_scroll_speed = wheel_points;
    });
    let font = fonts::install_cjk_fallback(ctx)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "unavailable".to_owned());
    println!(
        "MARKDOWN_SPIKE_CONFIG renderer=egui-extended cjk_font={font:?} wheel_lines={} wheel_points_per_notch={wheel_points:.1}",
        scroll_speed::system_wheel_lines(),
    );
}

fn document_argument() -> PathBuf {
    std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .unwrap_or_else(|| panic!("usage: markdown-spike-egui-extended <document.md>"))
}

struct SpikeApp {
    source: Arc<str>,
    path: PathBuf,
    base_dir: PathBuf,
    cache: CommonMarkCache,
    metrics: FrameMetrics,
    auto_scroll: bool,
    exit_after_seconds: Option<f32>,
    scroll_offset: f32,
    max_scroll_offset: f32,
    previous_frame: Instant,
    finished: bool,
}

impl SpikeApp {
    fn new(source: Arc<str>, path: PathBuf, base_dir: PathBuf, process_started: Instant) -> Self {
        Self {
            source,
            path,
            base_dir,
            cache: CommonMarkCache::default(),
            metrics: FrameMetrics::new(process_started),
            auto_scroll: env_flag("MARKDOWN_SPIKE_AUTOSCROLL"),
            exit_after_seconds: std::env::var("MARKDOWN_SPIKE_SECONDS")
                .ok()
                .and_then(|value| value.parse().ok()),
            scroll_offset: 0.0,
            max_scroll_offset: 0.0,
            previous_frame: Instant::now(),
            finished: false,
        }
    }
}

impl eframe::App for SpikeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.metrics.tick("egui-extended");

        let now = Instant::now();
        let delta = now.duration_since(self.previous_frame).as_secs_f32();
        self.previous_frame = now;
        if self.auto_scroll {
            self.scroll_offset += 720.0 * delta;
            if self.max_scroll_offset > 0.0 && self.scroll_offset >= self.max_scroll_offset {
                self.scroll_offset = 0.0;
            }
        }

        egui::TopBottomPanel::top("metrics").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong("PROTOTYPE");
                ui.label("egui_commonmark_extended 0.25");
                ui.separator();
                ui.label(format!(
                    "wheel {:.0} pt/notch",
                    scroll_speed::comparison_points_per_notch()
                ));
                ui.separator();
                ui.label(self.path.display().to_string());
                ui.separator();
                ui.monospace(self.metrics.summary());
            });
        });

        let html_base = self.base_dir.clone();
        let render_html = move |ui: &mut egui::Ui, html: &str| {
            render_html_block(ui, html, &html_base);
        };

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.set_max_width(900.0);
            let base_uri = format!("{}/", file_uri(&self.base_dir).trim_end_matches('/'));
            let mut viewer = CommonMarkViewer::new()
                .content_version(1)
                .max_image_width(Some(ui.available_width() as usize))
                .show_alt_text_on_hover(true)
                .default_implicit_uri_scheme(base_uri)
                .render_html_fn(Some(&render_html));
            if self.auto_scroll {
                viewer = viewer.pending_scroll_offset(Some(self.scroll_offset));
            }
            let output =
                viewer.show_scrollable("benchmark-document", ui, &mut self.cache, &self.source);
            self.max_scroll_offset = (output.content_size.y - output.inner_rect.height()).max(0.0);
        });

        if self.auto_scroll {
            ctx.request_repaint();
        } else if self.exit_after_seconds.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if !self.finished
            && self
                .exit_after_seconds
                .is_some_and(|seconds| self.metrics.elapsed().as_secs_f32() >= seconds)
        {
            self.finished = true;
            println!(
                "MARKDOWN_SPIKE_METRIC renderer=egui-extended event=finished {}",
                self.metrics.summary()
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn render_html_block(ui: &mut egui::Ui, html: &str, base_dir: &Path) {
    let Some(image_tag) = extract_tag(html, "img") else {
        ui.label(strip_tags(html));
        return;
    };
    let Some(src) = attribute(image_tag, "src") else {
        ui.colored_label(egui::Color32::LIGHT_RED, "HTML image has no src");
        return;
    };

    let image_path = base_dir.join(src.replace('/', std::path::MAIN_SEPARATOR_STR));
    let uri = file_uri(&image_path);
    let width = attribute(image_tag, "width")
        .and_then(parse_width)
        .map(|width| match width {
            ImageWidth::Percent(percent) => ui.available_width() * percent / 100.0,
            ImageWidth::Pixels(pixels) => pixels,
        })
        .unwrap_or_else(|| ui.available_width());

    let image = html_image(uri, width.min(ui.available_width()));
    if html.contains("text-align: center") || html.contains("<center") {
        ui.vertical_centered(|ui| {
            ui.add(image);
        });
    } else {
        ui.add(image);
    }
}

fn html_image(uri: String, width: f32) -> egui::Image<'static> {
    egui::Image::new(uri)
        .fit_to_original_size(1.0)
        .max_width(width)
        .maintain_aspect_ratio(true)
        .show_loading_spinner(true)
}

enum ImageWidth {
    Percent(f32),
    Pixels(f32),
}

fn parse_width(value: &str) -> Option<ImageWidth> {
    let value = value.trim();
    if let Some(percent) = value.strip_suffix('%') {
        return percent
            .trim()
            .parse::<f32>()
            .ok()
            .map(|value| ImageWidth::Percent(value.clamp(1.0, 100.0)));
    }
    value
        .strip_suffix("px")
        .unwrap_or(value)
        .trim()
        .parse::<f32>()
        .ok()
        .map(|value| ImageWidth::Pixels(value.clamp(1.0, 16_384.0)))
}

fn extract_tag<'a>(html: &'a str, name: &str) -> Option<&'a str> {
    let start = html.find(&format!("<{name}"))?;
    let tail = &html[start..];
    let end = tail.find('>')? + 1;
    Some(&tail[..end])
}

fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let bytes = tag.as_bytes();
    let mut cursor = 0;
    while let Some(relative) = tag[cursor..].find(name) {
        let start = cursor + relative;
        let before_ok = start == 0 || bytes[start - 1].is_ascii_whitespace();
        let after = start + name.len();
        if before_ok && tag[after..].trim_start().starts_with('=') {
            let equals = after + tag[after..].find('=')?;
            let value = tag[equals + 1..].trim_start();
            let quote = value.as_bytes().first().copied()?;
            if quote == b'\'' || quote == b'"' {
                let body = &value[1..];
                return body.find(quote as char).map(|end| &body[..end]);
            }
            let end = value
                .find(|character: char| character.is_whitespace() || character == '>')
                .unwrap_or(value.len());
            return Some(&value[..end]);
        }
        cursor = after;
    }
    None
}

fn strip_tags(html: &str) -> String {
    let mut output = String::new();
    let mut inside_tag = false;
    for character in html.chars() {
        match character {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if !inside_tag => output.push(character),
            _ => {}
        }
    }
    output.trim().to_owned()
}

fn file_uri(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    format!("file:///{path}")
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn configured_fonts_cover_chinese_in_body_and_code() {
        let ctx = egui::Context::default();
        configure_renderer(&ctx);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                let font = egui::FontId::new(16.0, family);
                ctx.fonts_mut(|fonts| {
                    assert!(fonts.has_glyphs(&font, "理解大语言模型"));
                });
            }
        });
    }

    #[test]
    fn egui_line_scroll_matches_gpui_list() {
        let ctx = egui::Context::default();
        configure_renderer(&ctx);
        let actual = ctx.options(|options| options.input_options.line_scroll_speed);
        assert_eq!(actual, scroll_speed::comparison_points_per_notch());
    }

    #[test]
    fn parses_sample_image_block() {
        let html = r#"<div style="text-align: center;">
<img src="../images/ch01.png" width="75%" />
</div>"#;
        let tag = extract_tag(html, "img").unwrap();
        assert_eq!(attribute(tag, "src"), Some("../images/ch01.png"));
        assert!(matches!(
            attribute(tag, "width").and_then(parse_width),
            Some(ImageWidth::Percent(75.0))
        ));
    }

    #[test]
    fn html_callback_receives_reading_width() {
        let ctx = egui::Context::default();
        let observed_width = Arc::new(AtomicU32::new(0.0_f32.to_bits()));
        let callback_width = observed_width.clone();
        let mut cache = CommonMarkCache::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 760.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let callback_width = callback_width.clone();
                let render_html = move |ui: &mut egui::Ui, _: &str| {
                    callback_width.store(ui.available_width().to_bits(), Ordering::Relaxed);
                };
                CommonMarkViewer::new()
                    .render_html_fn(Some(&render_html))
                    .show_scrollable(
                        "html-width-test",
                        ui,
                        &mut cache,
                        "<div><img src=\"image.png\" width=\"75%\"></div>",
                    );
            });
        });

        let observed_width = f32::from_bits(observed_width.load(Ordering::Relaxed));
        assert!(
            observed_width >= 500.0,
            "HTML callback width was only {} points",
            observed_width
        );
    }

    #[test]
    fn html_image_width_is_not_limited_by_remaining_vertical_space() {
        let image = html_image("file:///image.png".to_owned(), 600.0);
        let size = image.calc_size(egui::vec2(800.0, 2.0), Some(egui::vec2(1200.0, 800.0)));
        assert!(size.x >= 599.0, "image was unexpectedly shrunk to {size:?}");
    }
}
