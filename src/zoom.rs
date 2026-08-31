const MIN_FACTOR: f32 = 0.5;
const MAX_FACTOR: f32 = 2.5;
const KEYBOARD_STEP: f32 = 0.1;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoomLevel {
    applied_factor: f32,
    gesture_factor: f32,
}

impl Default for ZoomLevel {
    fn default() -> Self {
        Self::from_factor(1.0)
    }
}

impl ZoomLevel {
    pub fn from_factor(factor: f32) -> Self {
        let factor = clamp_factor(factor);
        Self {
            applied_factor: factor,
            gesture_factor: factor,
        }
    }

    pub fn factor(self) -> f32 {
        self.applied_factor
    }

    pub fn percent(self) -> u32 {
        (self.applied_factor * 100.0).round() as u32
    }

    pub fn zoom_in(&mut self) -> bool {
        self.set_rounded(self.applied_factor + KEYBOARD_STEP)
    }

    pub fn zoom_out(&mut self) -> bool {
        self.set_rounded(self.applied_factor - KEYBOARD_STEP)
    }

    pub fn apply_gesture(&mut self, delta: f32) -> bool {
        if !delta.is_finite() || delta <= 0.0 || (delta - 1.0).abs() <= f32::EPSILON {
            return false;
        }
        self.gesture_factor = clamp_factor(self.gesture_factor * delta);
        let factor = round_to_keyboard_step(self.gesture_factor);
        if (self.applied_factor - factor).abs() <= f32::EPSILON {
            return false;
        }
        self.applied_factor = factor;
        true
    }

    pub fn reset(&mut self) -> bool {
        self.set(1.0)
    }

    fn set_rounded(&mut self, factor: f32) -> bool {
        self.set(round_to_keyboard_step(factor))
    }

    fn set(&mut self, factor: f32) -> bool {
        let factor = clamp_factor(factor);
        self.gesture_factor = factor;
        if (self.applied_factor - factor).abs() <= f32::EPSILON {
            return false;
        }
        self.applied_factor = factor;
        true
    }
}

fn round_to_keyboard_step(factor: f32) -> f32 {
    (factor / KEYBOARD_STEP).round() * KEYBOARD_STEP
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
    fn gesture_zoom_accumulates_but_only_applies_ten_percent_steps() {
        let mut zoom = ZoomLevel::default();
        assert!(!zoom.apply_gesture(1.035));
        assert!(zoom.apply_gesture(1.035));
        assert_eq!(zoom.percent(), 110);
        assert_eq!(zoom.factor(), 1.1);
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
