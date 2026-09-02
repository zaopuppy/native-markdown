use std::{rc::Rc, time::Duration};

use gpui::{
    actions, canvas, div, fill, img, point, prelude::FluentBuilder as _, px, relative, rgba, size,
    Animation, AnimationExt as _, AnyElement, App, Bounds, ClickEvent, Context, Entity,
    FocusHandle, Focusable, ImgResourceLoader, InteractiveElement as _, IntoElement, KeyBinding,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, ParentElement as _,
    Render, Resource, ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Window,
};
use gpui_component::button::Button;
use gpui_component::input::Escape;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Disableable as _, Sizable as _, StyledExt as _};

use crate::image_cache::BudgetImageCache;

const CONTEXT: &str = "NativeMarkdownImageViewer";
const MIN_SCALE: f32 = 0.1;
const MAX_SCALE: f32 = 8.0;
const BUTTON_FACTOR: f32 = 1.2;
const KEYBOARD_PAN: f32 = 48.0;
const CANVAS_MARGIN: f32 = 24.0;
const CHROME_HEIGHT: f32 = 104.0;
const DRAG_THRESHOLD: f32 = 3.0;

actions!(
    image_viewer,
    [
        ImageZoomIn,
        ImageZoomOut,
        ImageReset,
        ImagePanUp,
        ImagePanDown,
        ImagePanLeft,
        ImagePanRight,
    ]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Escape, Some(CONTEXT)),
        KeyBinding::new("+", ImageZoomIn, Some(CONTEXT)),
        KeyBinding::new("=", ImageZoomIn, Some(CONTEXT)),
        KeyBinding::new("-", ImageZoomOut, Some(CONTEXT)),
        KeyBinding::new("0", ImageReset, Some(CONTEXT)),
        KeyBinding::new("up", ImagePanUp, Some(CONTEXT)),
        KeyBinding::new("down", ImagePanDown, Some(CONTEXT)),
        KeyBinding::new("left", ImagePanLeft, Some(CONTEXT)),
        KeyBinding::new("right", ImagePanRight, Some(CONTEXT)),
    ]);
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Vec2 {
    x: f32,
    y: f32,
}

impl Vec2 {
    fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ViewportModel {
    intrinsic: Vec2,
    viewport: Vec2,
    scale: f32,
    initial_scale: f32,
    offset: Vec2,
    manipulated: bool,
}

impl ViewportModel {
    fn new(intrinsic: Vec2, viewport: Vec2) -> Self {
        let initial_scale = fit_scale(intrinsic, viewport);
        Self {
            intrinsic,
            viewport,
            scale: initial_scale,
            initial_scale,
            offset: Vec2::default(),
            manipulated: false,
        }
    }

    fn effective_min(&self) -> f32 {
        MIN_SCALE.min(self.initial_scale)
    }

    fn percent(&self) -> u32 {
        (self.scale * 100.0).round() as u32
    }

    fn rendered_size(&self) -> Vec2 {
        Vec2::new(self.intrinsic.x * self.scale, self.intrinsic.y * self.scale)
    }

    fn update_viewport(&mut self, viewport: Vec2) {
        if viewport.x <= 0.0 || viewport.y <= 0.0 || self.viewport == viewport {
            return;
        }
        self.viewport = viewport;
        self.initial_scale = fit_scale(self.intrinsic, viewport);
        if !self.manipulated {
            self.scale = self.initial_scale;
            self.offset = Vec2::default();
        }
        self.clamp_offset();
    }

    fn replace_intrinsic(&mut self, intrinsic: Vec2) {
        if intrinsic.x <= 0.0 || intrinsic.y <= 0.0 || self.intrinsic == intrinsic {
            return;
        }
        let rendered = self.rendered_size();
        self.intrinsic = intrinsic;
        self.initial_scale = fit_scale(intrinsic, self.viewport);
        if self.manipulated {
            self.scale = (rendered.x / intrinsic.x).clamp(self.effective_min(), MAX_SCALE);
        } else {
            self.scale = self.initial_scale;
            self.offset = Vec2::default();
        }
        self.clamp_offset();
    }

    fn zoom_at(&mut self, factor: f32, anchor: Vec2) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let old_scale = self.scale;
        let new_scale = (old_scale * factor).clamp(self.effective_min(), MAX_SCALE);
        if (new_scale - old_scale).abs() <= f32::EPSILON {
            return;
        }
        let image_point = Vec2::new(
            (anchor.x - self.viewport.x * 0.5 - self.offset.x) / old_scale,
            (anchor.y - self.viewport.y * 0.5 - self.offset.y) / old_scale,
        );
        self.scale = new_scale;
        self.offset = Vec2::new(
            anchor.x - self.viewport.x * 0.5 - image_point.x * new_scale,
            anchor.y - self.viewport.y * 0.5 - image_point.y * new_scale,
        );
        self.manipulated = true;
        self.clamp_offset();
    }

    fn pan_by(&mut self, delta: Vec2) {
        self.offset.x += delta.x;
        self.offset.y += delta.y;
        self.manipulated = true;
        self.clamp_offset();
    }

    fn reset(&mut self) {
        self.scale = self.initial_scale;
        self.offset = Vec2::default();
        self.manipulated = false;
    }

    fn toggle_actual_size(&mut self) {
        if (self.scale - self.initial_scale).abs() <= 0.01 {
            let center = Vec2::new(self.viewport.x * 0.5, self.viewport.y * 0.5);
            self.zoom_at(1.0 / self.scale, center);
        } else {
            self.reset();
        }
    }

    fn clamp_offset(&mut self) {
        let rendered = self.rendered_size();
        self.offset.x = clamp_axis(self.offset.x, rendered.x, self.viewport.x);
        self.offset.y = clamp_axis(self.offset.y, rendered.y, self.viewport.y);
    }
}

fn fit_scale(intrinsic: Vec2, viewport: Vec2) -> f32 {
    if intrinsic.x <= 0.0 || intrinsic.y <= 0.0 || viewport.x <= 0.0 || viewport.y <= 0.0 {
        return 1.0;
    }
    (viewport.x / intrinsic.x)
        .min(viewport.y / intrinsic.y)
        .clamp(f32::MIN_POSITIVE, 1.0)
}

fn clamp_axis(offset: f32, rendered: f32, viewport: f32) -> f32 {
    if rendered <= viewport {
        0.0
    } else {
        offset.clamp(-(rendered - viewport) * 0.5, (rendered - viewport) * 0.5)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DragState {
    origin: Vec2,
    last: Vec2,
    active: bool,
}

impl DragState {
    fn new(position: Vec2) -> Self {
        Self {
            origin: position,
            last: position,
            active: false,
        }
    }

    fn update(&mut self, position: Vec2) -> Option<Vec2> {
        if !self.active {
            let distance_squared =
                (position.x - self.origin.x).powi(2) + (position.y - self.origin.y).powi(2);
            if distance_squared <= DRAG_THRESHOLD.powi(2) {
                return None;
            }
            self.active = true;
        }
        let delta = Vec2::new(position.x - self.last.x, position.y - self.last.y);
        self.last = position;
        Some(delta)
    }
}

type DismissHandler = Rc<dyn Fn(&mut Window, &mut App)>;

pub struct ImageViewer {
    preview_uri: SharedString,
    high_resolution_uri: SharedString,
    image_cache: Entity<BudgetImageCache>,
    title: SharedString,
    focus_handle: FocusHandle,
    dismiss: DismissHandler,
    model: Option<ViewportModel>,
    drag: Option<DragState>,
    suppress_click: bool,
    high_resolution_failed: bool,
}

impl ImageViewer {
    pub fn new(
        preview_uri: SharedString,
        high_resolution_uri: SharedString,
        title: SharedString,
        image_cache: Entity<BudgetImageCache>,
        dismiss: DismissHandler,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            preview_uri,
            high_resolution_uri,
            image_cache,
            title,
            focus_handle: cx.focus_handle().tab_stop(true),
            dismiss,
            model: None,
            drag: None,
            suppress_click: false,
            high_resolution_failed: false,
        }
    }

    pub fn focus(&self, window: &mut Window) {
        self.focus_handle.focus(window);
    }

    fn available_viewport(window: &Window) -> Vec2 {
        let size = window.viewport_size();
        Vec2::new(
            (f32::from(size.width) - CANVAS_MARGIN * 2.0).max(1.0),
            (f32::from(size.height) - CHROME_HEIGHT).max(1.0),
        )
    }

    fn resource_state(
        uri: &SharedString,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Vec2, ()>> {
        let resource = Resource::Uri(uri.clone().into());
        window
            .get_asset::<ImgResourceLoader>(&resource, cx)
            .map(|result| {
                result
                    .map(|image| {
                        let size = image.size(0);
                        Vec2::new(i32::from(size.width) as f32, i32::from(size.height) as f32)
                    })
                    .map_err(|_| ())
            })
    }

    fn refresh_model(&mut self, window: &mut Window, cx: &mut App) {
        let viewport = Self::available_viewport(window);
        let high_resolution = Self::resource_state(&self.high_resolution_uri, window, cx);
        self.high_resolution_failed = matches!(high_resolution, Some(Err(())));
        let intrinsic = high_resolution
            .and_then(Result::ok)
            .or_else(|| Self::resource_state(&self.preview_uri, window, cx).and_then(Result::ok));
        if let Some(intrinsic) = intrinsic {
            if let Some(model) = &mut self.model {
                model.update_viewport(viewport);
                model.replace_intrinsic(intrinsic);
            } else {
                self.model = Some(ViewportModel::new(intrinsic, viewport));
            }
        } else if let Some(model) = &mut self.model {
            model.update_viewport(viewport);
        }
    }

    fn zoom_by(&mut self, factor: f32, anchor: Option<Vec2>, cx: &mut Context<Self>) {
        if let Some(model) = &mut self.model {
            let anchor =
                anchor.unwrap_or_else(|| Vec2::new(model.viewport.x * 0.5, model.viewport.y * 0.5));
            model.zoom_at(factor, anchor);
            cx.notify();
        }
    }

    fn pan_by(&mut self, delta: Vec2, cx: &mut Context<Self>) {
        if let Some(model) = &mut self.model {
            model.pan_by(delta);
            cx.notify();
        }
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        if let Some(model) = &mut self.model {
            model.reset();
            cx.notify();
        }
    }

    fn dismiss(&self, window: &mut Window, cx: &mut App) {
        (self.dismiss)(window, cx);
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta: f32 = event.delta.pixel_delta(px(20.0)).y.into();
        let factor = (delta * 0.0025).clamp(-0.35, 0.35).exp();
        let anchor = Vec2::new(
            f32::from(event.position.x) - CANVAS_MARGIN,
            f32::from(event.position.y) - CANVAS_MARGIN,
        );
        self.zoom_by(factor, Some(anchor), cx);
        cx.stop_propagation();
    }

    fn begin_drag(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.drag = Some(DragState::new(Vec2::new(
            f32::from(event.position.x),
            f32::from(event.position.y),
        )));
        self.suppress_click = false;
        cx.stop_propagation();
    }

    fn continue_drag(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = &mut self.drag else {
            return;
        };
        if !event.dragging() {
            self.drag = None;
            return;
        }
        let current = Vec2::new(f32::from(event.position.x), f32::from(event.position.y));
        if let Some(delta) = drag.update(current) {
            self.suppress_click = true;
            self.pan_by(delta, cx);
        }
        cx.stop_propagation();
    }

    fn end_drag(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            cx.stop_propagation();
        }
    }

    fn image_clicked(&mut self, event: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.suppress_click) {
            cx.stop_propagation();
            return;
        }
        if let ClickEvent::Mouse(event) = event {
            if event.up.click_count >= 2 {
                if let Some(model) = &mut self.model {
                    model.toggle_actual_size();
                    cx.notify();
                }
            }
        }
        cx.stop_propagation();
    }

    fn checkerboard() -> impl IntoElement {
        canvas(
            |_, _, _| {},
            |bounds, _, window, _| {
                let cell = 14.0;
                let left = f32::from(bounds.origin.x).max(0.0);
                let top = f32::from(bounds.origin.y).max(0.0);
                let viewport = window.viewport_size();
                let right = f32::from(bounds.right()).min(f32::from(viewport.width));
                let bottom = f32::from(bounds.bottom()).min(f32::from(viewport.height));
                let first_column = ((left - f32::from(bounds.origin.x)) / cell).floor() as i32;
                let last_column = ((right - f32::from(bounds.origin.x)) / cell).ceil() as i32;
                let first_row = ((top - f32::from(bounds.origin.y)) / cell).floor() as i32;
                let last_row = ((bottom - f32::from(bounds.origin.y)) / cell).ceil() as i32;
                for row in first_row..last_row {
                    for column in first_column..last_column {
                        let color = if (row + column) % 2 == 0 {
                            rgba(0xb8b8b8ff)
                        } else {
                            rgba(0xd2d2d2ff)
                        };
                        window.paint_quad(fill(
                            Bounds {
                                origin: point(
                                    bounds.origin.x + px(column as f32 * cell),
                                    bounds.origin.y + px(row as f32 * cell),
                                ),
                                size: size(px(cell), px(cell)),
                            },
                            color,
                        ));
                    }
                }
            },
        )
        .size_full()
    }

    fn image_surface(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(model) = self.model else {
            return div()
                .id("image-viewer-surface")
                .absolute()
                .left(px(CANVAS_MARGIN))
                .right(px(CANVAS_MARGIN))
                .top(px(CANVAS_MARGIN))
                .bottom(px(CHROME_HEIGHT - CANVAS_MARGIN))
                .overflow_hidden()
                .child(Self::checkerboard())
                .child(
                    img(self.preview_uri.clone())
                        .image_cache(&self.image_cache)
                        .id("image-viewer-preview-image")
                        .debug_selector(|| "image-viewer-preview-image".into())
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                )
                .child(
                    img(self.high_resolution_uri.clone())
                        .image_cache(&self.image_cache)
                        .id("image-viewer-high-resolution-image")
                        .debug_selector(|| "image-viewer-high-resolution-image".into())
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                )
                .on_mouse_down(MouseButton::Left, cx.listener(Self::begin_drag))
                .on_click(cx.listener(Self::image_clicked))
                .into_any_element();
        };
        let rendered = model.rendered_size();
        let left = CANVAS_MARGIN + (model.viewport.x - rendered.x) * 0.5 + model.offset.x;
        let top = CANVAS_MARGIN + (model.viewport.y - rendered.y) * 0.5 + model.offset.y;
        div()
            .id("image-viewer-surface")
            .debug_selector(|| "image-viewer-surface".into())
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(rendered.x))
            .h(px(rendered.y))
            .overflow_hidden()
            .border_1()
            .border_color(rgba(0xffffff22))
            .cursor_grab()
            .when(self.drag.is_some_and(|drag| drag.active), |surface| {
                surface.cursor_grabbing()
            })
            .child(Self::checkerboard())
            .child(
                img(self.preview_uri.clone())
                    .image_cache(&self.image_cache)
                    .id("image-viewer-preview-image")
                    .debug_selector(|| "image-viewer-preview-image".into())
                    .absolute()
                    .inset_0()
                    .size_full()
                    .object_fit(ObjectFit::Contain),
            )
            .child(
                img(self.high_resolution_uri.clone())
                    .image_cache(&self.image_cache)
                    .id("image-viewer-high-resolution-image")
                    .debug_selector(|| "image-viewer-high-resolution-image".into())
                    .absolute()
                    .inset_0()
                    .size_full()
                    .object_fit(ObjectFit::Contain),
            )
            .on_mouse_down(MouseButton::Left, cx.listener(Self::begin_drag))
            .on_click(cx.listener(Self::image_clicked))
            .into_any_element()
    }
}

impl Focusable for ImageViewer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ImageViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh_model(window, cx);
        let percent = self.model.map_or(100, |model| model.percent());
        let can_zoom_out = self
            .model
            .is_some_and(|model| model.scale > model.effective_min() + f32::EPSILON);
        let can_zoom_in = self
            .model
            .is_some_and(|model| model.scale < MAX_SCALE - f32::EPSILON);
        let weak = cx.entity().downgrade();
        let dismiss = self.dismiss.clone();
        let dismiss_from_backdrop = dismiss.clone();
        let image_surface = self.image_surface(cx);

        div()
            .id("image-viewer")
            .debug_selector(|| "image-viewer".into())
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .tab_group()
            .occlude()
            .absolute()
            .inset_0()
            .overflow_hidden()
            .bg(rgba(0x111111e8))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_mouse_move(cx.listener(Self::continue_drag))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::end_drag))
            .on_click(move |_, window, cx| dismiss_from_backdrop(window, cx))
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                this.dismiss(window, cx);
                cx.stop_propagation();
            }))
            .on_action(
                cx.listener(|this, _: &ImageZoomIn, _, cx| this.zoom_by(BUTTON_FACTOR, None, cx)),
            )
            .on_action(cx.listener(|this, _: &ImageZoomOut, _, cx| {
                this.zoom_by(1.0 / BUTTON_FACTOR, None, cx)
            }))
            .on_action(cx.listener(|this, _: &ImageReset, _, cx| this.reset(cx)))
            .on_action(cx.listener(|this, _: &ImagePanUp, _, cx| {
                this.pan_by(Vec2::new(0.0, KEYBOARD_PAN), cx)
            }))
            .on_action(cx.listener(|this, _: &ImagePanDown, _, cx| {
                this.pan_by(Vec2::new(0.0, -KEYBOARD_PAN), cx)
            }))
            .on_action(cx.listener(|this, _: &ImagePanLeft, _, cx| {
                this.pan_by(Vec2::new(KEYBOARD_PAN, 0.0), cx)
            }))
            .on_action(cx.listener(|this, _: &ImagePanRight, _, cx| {
                this.pan_by(Vec2::new(-KEYBOARD_PAN, 0.0), cx)
            }))
            .child(image_surface)
            .when(!self.title.is_empty(), |overlay| {
                overlay.child(
                    div()
                        .absolute()
                        .bottom(px(68.0))
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .id("image-viewer-title")
                                .max_w(relative(0.75))
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_sm()
                                .text_color(rgba(0xffffffff))
                                .tooltip({
                                    let title = self.title.clone();
                                    move |window, cx| Tooltip::new(title.clone()).build(window, cx)
                                })
                                .child(self.title.clone()),
                        ),
                )
            })
            .when(self.high_resolution_failed, |overlay| {
                overlay.child(
                    div()
                        .absolute()
                        .top_4()
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .text_sm()
                        .text_color(rgba(0xffd5c7ff))
                        .child("High-resolution image could not be loaded"),
                )
            })
            .child(
                div()
                    .id("image-viewer-toolbar")
                    .absolute()
                    .bottom_4()
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded_lg()
                            .bg(rgba(0x242424f2))
                            .border_1()
                            .border_color(rgba(0xffffff26))
                            .child(
                                Button::new("image-viewer-zoom-out")
                                    .label("−")
                                    .small()
                                    .disabled(!can_zoom_out)
                                    .on_click({
                                        let weak = weak.clone();
                                        move |_, _, cx| {
                                            weak.update(cx, |viewer, cx| {
                                                viewer.zoom_by(1.0 / BUTTON_FACTOR, None, cx)
                                            })
                                            .ok();
                                        }
                                    }),
                            )
                            .child(
                                div()
                                    .w_16()
                                    .text_center()
                                    .text_sm()
                                    .text_color(rgba(0xffffffff))
                                    .child(format!("{percent}%")),
                            )
                            .child(
                                Button::new("image-viewer-zoom-in")
                                    .label("+")
                                    .small()
                                    .disabled(!can_zoom_in)
                                    .on_click({
                                        let weak = weak.clone();
                                        move |_, _, cx| {
                                            weak.update(cx, |viewer, cx| {
                                                viewer.zoom_by(BUTTON_FACTOR, None, cx)
                                            })
                                            .ok();
                                        }
                                    }),
                            )
                            .child(
                                Button::new("image-viewer-reset")
                                    .label("Reset")
                                    .small()
                                    .on_click({
                                        let weak = weak.clone();
                                        move |_, _, cx| {
                                            weak.update(cx, |viewer, cx| viewer.reset(cx)).ok();
                                        }
                                    }),
                            )
                            .child(
                                Button::new("image-viewer-close")
                                    .label("Close")
                                    .small()
                                    .on_click(move |_, window, cx| dismiss(window, cx)),
                            ),
                    ),
            )
            .with_animation(
                "image-viewer-fade-in",
                Animation::new(Duration::from_millis(150)),
                |overlay, progress| overlay.opacity(progress),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_never_upscales_small_images() {
        assert_eq!(
            fit_scale(Vec2::new(400.0, 300.0), Vec2::new(1_000.0, 800.0)),
            1.0
        );
    }

    #[test]
    fn drag_waits_for_three_pixel_threshold() {
        let mut drag = DragState::new(Vec2::new(10.0, 10.0));
        assert_eq!(drag.update(Vec2::new(12.0, 12.0)), None);
        assert_eq!(
            drag.update(Vec2::new(14.0, 10.0)),
            Some(Vec2::new(4.0, 0.0))
        );
        assert!(drag.active);
        assert_eq!(
            drag.update(Vec2::new(16.0, 11.0)),
            Some(Vec2::new(2.0, 1.0))
        );
    }

    #[test]
    fn huge_images_can_fit_below_ten_percent() {
        let model = ViewportModel::new(Vec2::new(20_000.0, 10_000.0), Vec2::new(1_000.0, 800.0));
        assert_eq!(model.scale, 0.05);
        assert_eq!(model.effective_min(), 0.05);
    }

    #[test]
    fn zoom_keeps_anchor_over_same_image_point() {
        let mut model = ViewportModel::new(Vec2::new(2_000.0, 2_000.0), Vec2::new(1_000.0, 800.0));
        let anchor = Vec2::new(700.0, 400.0);
        let before = Vec2::new(
            (anchor.x - model.viewport.x * 0.5 - model.offset.x) / model.scale,
            (anchor.y - model.viewport.y * 0.5 - model.offset.y) / model.scale,
        );
        model.zoom_at(2.0, anchor);
        let after = Vec2::new(
            (anchor.x - model.viewport.x * 0.5 - model.offset.x) / model.scale,
            (anchor.y - model.viewport.y * 0.5 - model.offset.y) / model.scale,
        );
        assert!((before.x - after.x).abs() < 0.01);
        assert!((before.y - after.y).abs() < 0.01);
    }

    #[test]
    fn panning_is_centered_when_image_is_smaller_than_viewport() {
        let mut model = ViewportModel::new(Vec2::new(400.0, 300.0), Vec2::new(1_000.0, 800.0));
        model.pan_by(Vec2::new(200.0, 200.0));
        assert_eq!(model.offset, Vec2::default());
    }

    #[test]
    fn high_resolution_replacement_preserves_manipulated_rendered_size() {
        let mut model = ViewportModel::new(Vec2::new(1_000.0, 500.0), Vec2::new(800.0, 600.0));
        model.zoom_at(2.0, Vec2::new(400.0, 300.0));
        let before = model.rendered_size();
        model.replace_intrinsic(Vec2::new(2_000.0, 1_000.0));
        assert_eq!(model.rendered_size(), before);
    }
}
