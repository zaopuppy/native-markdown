#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollPane {
    Source,
    Preview,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ScrollMetrics {
    pub offset: f32,
    pub content_height: f32,
    pub viewport_height: f32,
}

impl ScrollMetrics {
    pub fn new(offset: f32, content_height: f32, viewport_height: f32) -> Self {
        Self {
            offset,
            content_height,
            viewport_height,
        }
    }

    fn max_offset(self) -> f32 {
        (self.content_height - self.viewport_height).max(0.0)
    }

    fn progress(self) -> f32 {
        let max_offset = self.max_offset();
        if max_offset <= f32::EPSILON {
            0.0
        } else {
            (self.offset / max_offset).clamp(0.0, 1.0)
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ScrollSync {
    progress: f32,
    driver: Option<ScrollPane>,
}

impl ScrollSync {
    pub fn update_from(&mut self, pane: ScrollPane, metrics: ScrollMetrics) {
        self.progress = metrics.progress();
        self.driver = Some(pane);
    }

    pub fn driver(self) -> Option<ScrollPane> {
        self.driver
    }

    pub fn target_offset(self, content_height: f32, viewport_height: f32) -> f32 {
        self.progress * (content_height - viewport_height).max(0.0)
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_scroll_progress_maps_to_preview() {
        let mut sync = ScrollSync::default();
        sync.update_from(
            ScrollPane::Source,
            ScrollMetrics::new(600.0, 1_600.0, 400.0),
        );

        assert_eq!(sync.driver(), Some(ScrollPane::Source));
        assert!((sync.target_offset(2_800.0, 700.0) - 1_050.0).abs() < 0.01);
    }

    #[test]
    fn preview_scroll_progress_maps_back_to_source() {
        let mut sync = ScrollSync::default();
        sync.update_from(
            ScrollPane::Preview,
            ScrollMetrics::new(1_575.0, 2_800.0, 700.0),
        );

        assert_eq!(sync.driver(), Some(ScrollPane::Preview));
        assert!((sync.target_offset(1_600.0, 400.0) - 900.0).abs() < 0.01);
    }

    #[test]
    fn scroll_offsets_are_clamped_to_each_pane() {
        let mut sync = ScrollSync::default();
        sync.update_from(
            ScrollPane::Source,
            ScrollMetrics::new(9_999.0, 1_600.0, 400.0),
        );

        assert!((sync.target_offset(2_800.0, 700.0) - 2_100.0).abs() < 0.01);
    }
}
