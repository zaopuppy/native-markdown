//! File-tree hints stay in the GPUI sidebar instead of overlapping the WebView child HWND.
use std::{cell::Cell, rc::Rc};

use gpui::{
    anchored, div, point, prelude::*, px, AnyView, App, AppContext, Bounds, Context, IntoElement,
    Pixels, Render, SharedString, Window,
};
use gpui_component::ActiveTheme;

pub type HintBounds = Rc<Cell<Bounds<Pixels>>>;

pub fn trigger(
    content: impl IntoElement,
    text: &'static str,
    bounds: HintBounds,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(text))
        .child(content)
        .tooltip(move |window, cx| build(text, bounds.clone(), window, cx))
}

pub fn build(
    text: impl Into<SharedString>,
    bounds: HintBounds,
    _: &mut Window,
    cx: &mut App,
) -> AnyView {
    cx.new(|_| SidebarHint {
        text: text.into(),
        bounds,
    })
    .into()
}

struct SidebarHint {
    text: SharedString,
    bounds: HintBounds,
}

impl Render for SidebarHint {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bounds = self.bounds.get();
        // Use measured bounds, including after a resize; GPUI's normal tooltip placement
        // only avoids the outer window edges and knows nothing about the child HWND.
        anchored()
            .position(point(
                bounds.left() + px(8.),
                window.mouse_position().y + px(16.),
            ))
            .snap_to_window()
            .child(
                div()
                    .debug_selector(|| "file-tree-hint-popup".into())
                    .w((bounds.size.width - px(16.)).max(px(1.)))
                    .p_2()
                    .rounded_md()
                    .bg(cx.theme().popover)
                    .text_color(cx.theme().popover_foreground)
                    .border_1()
                    .border_color(cx.theme().border)
                    .text_sm()
                    .whitespace_normal()
                    .child(self.text.clone()),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{size, TestAppContext};

    struct Probe(HintBounds);

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(self.0.get().size.width)
                .h(px(100.))
                .flex()
                .flex_row()
                .justify_end()
                .child(trigger(
                    gpui_component::button::Button::new("edge-button").label("Hint"),
                    "打开当前文件夹并定位当前文件（文件资源管理器）",
                    self.0.clone(),
                ))
        }
    }

    #[gpui::test]
    fn hovering_right_edge_button_keeps_tooltip_out_of_webview(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let bounds = HintBounds::default();
        bounds.set(Bounds::new(point(px(0.), px(0.)), size(px(180.), px(700.))));
        let (_, cx) = cx.add_window_view(|_, _| Probe(bounds));
        cx.simulate_resize(size(px(1180.), px(780.)));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_mouse_move(point(px(160.), px(15.)), None, Default::default());
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let popup = cx.debug_bounds("file-tree-hint-popup").unwrap();
        assert!(popup.left() >= px(8.));
        assert!(popup.right() <= px(172.));
        assert!(popup.size.height > px(45.));
    }

    #[gpui::test]
    fn long_path_hint_wraps_inside_sidebar_after_resize(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let bounds = HintBounds::default();
        let (hint, cx) = cx.add_window_view(|_, _| SidebarHint {
            text: format!(
                "E:\\workspace\\{}\\说明.md",
                "long_directory_name".repeat(8)
            )
            .into(),
            bounds: bounds.clone(),
        });
        cx.simulate_resize(size(px(1180.), px(780.)));
        for width in [280., 180., 360.] {
            bounds.set(Bounds::new(
                point(px(0.), px(70.)),
                size(px(width), px(650.)),
            ));
            hint.update(cx, |_, cx| cx.notify());
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            let popup = cx.debug_bounds("file-tree-hint-popup").unwrap();
            assert!(popup.left() >= px(8.));
            assert!(popup.right() <= px(width - 8.));
            assert!(popup.size.height > px(45.), "long unbroken paths must wrap");
        }
    }
}
