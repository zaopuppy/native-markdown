use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use egui::{
    Color32, FontData, FontDefinitions, FontFamily, FontId, Rounding, Stroke, Style, TextStyle,
    Visuals,
};

pub const PAPER: Color32 = Color32::from_rgb(246, 241, 231);
pub const PAPER_DEEP: Color32 = Color32::from_rgb(235, 227, 212);
pub const INK: Color32 = Color32::from_rgb(41, 39, 35);
pub const MUTED: Color32 = Color32::from_rgb(112, 104, 91);
pub const BRASS: Color32 = Color32::from_rgb(150, 105, 43);
pub const BRASS_SOFT: Color32 = Color32::from_rgb(226, 207, 169);
pub const RULE: Color32 = Color32::from_rgb(211, 200, 181);
pub const EDITOR_BG: Color32 = Color32::from_rgb(238, 233, 223);
const CJK_FONT_NAME: &str = "Native Markdown CJK";

pub fn apply(ctx: &egui::Context) {
    install_cjk_fallback(ctx);

    let mut visuals = Visuals::light();
    visuals.override_text_color = Some(INK);
    visuals.panel_fill = PAPER;
    visuals.window_fill = PAPER;
    visuals.extreme_bg_color = Color32::from_rgb(250, 247, 240);
    visuals.faint_bg_color = PAPER_DEEP;
    visuals.code_bg_color = Color32::from_rgb(231, 224, 211);
    visuals.hyperlink_color = BRASS;
    visuals.selection.bg_fill = BRASS_SOFT;
    visuals.selection.stroke = Stroke::new(1.0, BRASS);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, RULE);
    visuals.widgets.inactive.bg_fill = Color32::TRANSPARENT;
    visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, RULE);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, MUTED);
    visuals.widgets.hovered.bg_fill = PAPER_DEEP;
    visuals.widgets.hovered.weak_bg_fill = PAPER_DEEP;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, BRASS_SOFT);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.2, INK);
    visuals.widgets.active.bg_fill = BRASS_SOFT;
    visuals.widgets.active.weak_bg_fill = BRASS_SOFT;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, BRASS);
    visuals.widgets.active.fg_stroke = Stroke::new(1.2, INK);
    visuals.window_rounding = Rounding::same(10.0);
    visuals.menu_rounding = Rounding::same(8.0);

    let mut text_styles = BTreeMap::new();
    text_styles.insert(
        TextStyle::Small,
        FontId::new(11.0, FontFamily::Proportional),
    );
    text_styles.insert(TextStyle::Body, FontId::new(16.0, FontFamily::Proportional));
    text_styles.insert(
        TextStyle::Button,
        FontId::new(14.0, FontFamily::Proportional),
    );
    text_styles.insert(
        TextStyle::Heading,
        FontId::new(28.0, FontFamily::Proportional),
    );
    text_styles.insert(
        TextStyle::Monospace,
        FontId::new(14.0, FontFamily::Monospace),
    );

    let mut style = Style {
        visuals,
        text_styles,
        ..Default::default()
    };
    style.spacing.item_spacing = egui::vec2(9.0, 7.0);
    style.spacing.button_padding = egui::vec2(11.0, 6.0);
    style.spacing.menu_margin = egui::Margin::same(8.0);
    style.spacing.window_margin = egui::Margin::same(14.0);
    ctx.set_style(style);
}

fn install_cjk_fallback(ctx: &egui::Context) {
    let Some(font_bytes) = cjk_font_candidates()
        .into_iter()
        .find_map(|path| fs::read(path).ok())
    else {
        return;
    };

    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert(CJK_FONT_NAME.to_owned(), FontData::from_owned(font_bytes));
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push(CJK_FONT_NAME.to_owned());
    }
    ctx.set_fonts(fonts);
}

#[cfg(target_os = "windows")]
fn cjk_font_candidates() -> Vec<PathBuf> {
    let windows = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let fonts = windows.join("Fonts");
    ["msyh.ttc", "Deng.ttf", "simhei.ttf", "simsun.ttc"]
        .into_iter()
        .map(|name| fonts.join(name))
        .collect()
}

#[cfg(target_os = "macos")]
fn cjk_font_candidates() -> Vec<PathBuf> {
    [
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Medium.ttc",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn cjk_font_candidates() -> Vec<PathBuf> {
    [
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
        "/usr/share/fonts/opentype/ipafont-gothic/ipag.ttf",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_fonts_cover_chinese_document_text() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let body = FontId::new(16.0, FontFamily::Proportional);
            let source = FontId::new(14.0, FontFamily::Monospace);
            ctx.fonts(|fonts| {
                assert!(fonts.has_glyphs(&body, "理解大语言模型"));
                assert!(fonts.has_glyphs(&source, "理解大语言模型"));
            });
        });
    }
}
