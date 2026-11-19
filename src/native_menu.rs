//! Native popup menus remain above child HWNDs without hiding the reading surface.
use gpui::{Pixels, Point, Window};
use windows_sys::Win32::{
    Foundation::{HWND, POINT},
    Graphics::Gdi::ClientToScreen,
    UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, IsWindow, TrackPopupMenuEx, MF_CHECKED,
        MF_SEPARATOR, MF_STRING, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTALIGN, TPM_RIGHTBUTTON,
    },
};
use wry::raw_window_handle::{HasWindowHandle, RawWindowHandle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Command {
    New = 1,
    Open,
    Save,
    SaveAs,
    Preview,
    Split,
    Source,
    ZoomIn,
    ZoomOut,
    ResetZoom,
}

pub struct Item {
    label: &'static str,
    command: Option<Command>,
    checked: bool,
}

impl Item {
    pub fn command(label: &'static str, command: Command) -> Self {
        Self {
            label,
            command: Some(command),
            checked: false,
        }
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub fn separator() -> Self {
        Self {
            label: "",
            command: None,
            checked: false,
        }
    }
}

pub struct Popup {
    owner: usize,
    position: POINT,
    right_aligned: bool,
    items: Vec<Item>,
}

impl Popup {
    pub fn capture(
        window: &Window,
        position: Point<Pixels>,
        right_aligned: bool,
        items: Vec<Item>,
    ) -> anyhow::Result<Self> {
        let RawWindowHandle::Win32(handle) = HasWindowHandle::window_handle(window)?.as_raw()
        else {
            anyhow::bail!("popup menu requires a Windows window");
        };
        let owner = handle.hwnd.get() as usize;
        let scale = window.scale_factor();
        let mut position = POINT {
            x: (f32::from(position.x) * scale).round() as i32,
            y: (f32::from(position.y) * scale).round() as i32,
        };
        // SAFETY: the HWND belongs to this live GPUI window and POINT is initialized.
        if unsafe { ClientToScreen(owner as HWND, &mut position) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self {
            owner,
            position,
            right_aligned,
            items,
        })
    }

    /// Run on the UI thread, outside GPUI entity/window borrows: TrackPopupMenuEx
    /// runs a native message loop that can reenter GPUI while the menu is open.
    pub fn show(self) -> anyhow::Result<Option<Command>> {
        // SAFETY: all handles and UTF-16 buffers are valid for the synchronous calls.
        // The menu is destroyed on every exit path, after the tracking loop returns.
        unsafe {
            if IsWindow(self.owner as HWND) == 0 {
                return Ok(None);
            }
            let menu = CreatePopupMenu();
            anyhow::ensure!(!menu.is_null(), "could not create popup menu");
            struct OwnedMenu(windows_sys::Win32::UI::WindowsAndMessaging::HMENU);
            impl Drop for OwnedMenu {
                fn drop(&mut self) {
                    unsafe {
                        DestroyMenu(self.0);
                    }
                }
            }
            let menu = OwnedMenu(menu);
            for item in &self.items {
                let label: Vec<u16> = item.label.encode_utf16().chain(Some(0)).collect();
                let flags = if item.command.is_none() {
                    MF_SEPARATOR
                } else {
                    MF_STRING | if item.checked { MF_CHECKED } else { 0 }
                };
                anyhow::ensure!(
                    AppendMenuW(
                        menu.0,
                        flags,
                        item.command.map_or(0, |command| command as usize),
                        label.as_ptr()
                    ) != 0,
                    "could not add popup menu item"
                );
            }
            let flags = TPM_RETURNCMD
                | TPM_NONOTIFY
                | TPM_RIGHTBUTTON
                | if self.right_aligned {
                    TPM_RIGHTALIGN
                } else {
                    0
                };
            let selected = TrackPopupMenuEx(
                menu.0,
                flags,
                self.position.x,
                self.position.y,
                self.owner as HWND,
                std::ptr::null(),
            );
            Ok(self
                .items
                .iter()
                .filter_map(|item| item.command)
                .find(|command| *command as u32 == selected as u32))
        }
    }
}
