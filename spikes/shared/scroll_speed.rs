pub const GPUI_LIST_POINTS_PER_LINE: f32 = 20.0;

pub fn comparison_points_per_notch() -> f32 {
    GPUI_LIST_POINTS_PER_LINE * system_wheel_lines() as f32
}

#[cfg(target_os = "windows")]
pub fn system_wheel_lines() -> u32 {
    use std::ffi::c_void;

    let mut lines = 3_u32;
    let success = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::SystemParametersInfoW(
            windows_sys::Win32::UI::WindowsAndMessaging::SPI_GETWHEELSCROLLLINES,
            0,
            (&mut lines as *mut u32).cast::<c_void>(),
            0,
        )
    };
    if success == 0 || lines == u32::MAX {
        3
    } else {
        lines
    }
}

#[cfg(not(target_os = "windows"))]
pub fn system_wheel_lines() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpui_three_line_wheel_notch_is_sixty_points() {
        assert_eq!(GPUI_LIST_POINTS_PER_LINE * 3.0, 60.0);
    }
}
