const MIN_FACTOR: f32 = 0.5;
const MAX_FACTOR: f32 = 2.5;
const KEYBOARD_STEP: f32 = 0.1;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoomLevel(f32);

impl Default for ZoomLevel {
    fn default() -> Self {
        Self::from_factor(1.0)
    }
}

impl ZoomLevel {
    pub fn from_factor(factor: f32) -> Self {
        Self(clamp_factor(factor))
    }

    pub fn factor(self) -> f32 {
        self.0
    }

    pub fn percent(self) -> u32 {
        (self.0 * 100.0).round() as u32
    }

    pub fn zoom_in(&mut self) -> bool {
        self.set_rounded(self.0 + KEYBOARD_STEP)
    }

    pub fn zoom_out(&mut self) -> bool {
        self.set_rounded(self.0 - KEYBOARD_STEP)
    }

    pub fn apply_gesture(&mut self, delta: f32) -> bool {
        if !delta.is_finite() || delta <= 0.0 || (delta - 1.0).abs() <= f32::EPSILON {
            return false;
        }
        self.set(self.0 * delta)
    }

    pub fn reset(&mut self) -> bool {
        self.set(1.0)
    }

    fn set_rounded(&mut self, factor: f32) -> bool {
        self.set((factor * 10.0).round() / 10.0)
    }

    fn set(&mut self, factor: f32) -> bool {
        let factor = clamp_factor(factor);
        if (self.0 - factor).abs() <= f32::EPSILON {
            return false;
        }
        self.0 = factor;
        true
    }
}

fn clamp_factor(factor: f32) -> f32 {
    if factor.is_finite() {
        factor.clamp(MIN_FACTOR, MAX_FACTOR)
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_zoom_moves_in_ten_percent_steps() {
        let mut zoom = ZoomLevel::default();
        assert!(zoom.zoom_in());
        assert_eq!(zoom.percent(), 110);
        assert!(zoom.zoom_out());
        assert_eq!(zoom.percent(), 100);
    }

    #[test]
    fn gesture_zoom_is_smooth_and_clamped() {
        let mut zoom = ZoomLevel::default();
        assert!(zoom.apply_gesture(1.035));
        assert_eq!(zoom.percent(), 104);
        assert_eq!(ZoomLevel::from_factor(99.0).percent(), 250);
        assert_eq!(ZoomLevel::from_factor(0.01).percent(), 50);
    }

    #[test]
    fn reset_returns_to_one_hundred_percent() {
        let mut zoom = ZoomLevel::from_factor(1.7);
        assert!(zoom.reset());
        assert_eq!(zoom.percent(), 100);
    }
}
