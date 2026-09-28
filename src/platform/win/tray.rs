//! The tray icon, its menu, and the events the two produce.
//!
//! The icon lives on a hidden message-only window rather than on the note's own
//! window, because dismissing a popup menu needs `SetForegroundWindow` and
//! `WS_EX_NOACTIVATE` forbids activating the note (spec §11).

use super::{PlatformError, ICON_RESOURCE_ID};
use std::mem::size_of;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GetLastError, HINSTANCE, HWND, LPARAM, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, HICON, LoadIconW, PostMessageW,
    SetForegroundWindow, TrackPopupMenu, MF_STRING, SW_SHOWNORMAL, TPM_NONOTIFY, TPM_RETURNCMD,
    WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP,
};

/// The message the shell posts to the hidden window for icon events.
pub const TRAY_CALLBACK_MESSAGE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;

/// The icon's id within the hidden window.
const TRAY_ICON_ID: u32 = 1;

/// One entry of the tray menu.
pub struct MenuItem {
    pub id: u32,
    pub label: &'static str,
}

pub const MENU_ID_MOVE: u32 = 1;
pub const MENU_ID_OPEN_CONFIG: u32 = 2;
pub const MENU_ID_QUIT: u32 = 3;

/// The menu, in the order the spec fixes.
pub const MENU_ITEMS: [MenuItem; 3] = [
    MenuItem { id: MENU_ID_MOVE, label: "移动窗口" },
    MenuItem { id: MENU_ID_OPEN_CONFIG, label: "打开配置文件" },
    MenuItem { id: MENU_ID_QUIT, label: "退出" },
];

/// What the user asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    MoveWindow,
    OpenConfig,
    Quit,
}

/// The one thing a click on the icon does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    ShowMenu,
}

pub fn event_for(command_id: u32) -> Option<TrayEvent> {
    match command_id {
        MENU_ID_MOVE => Some(TrayEvent::MoveWindow),
        MENU_ID_OPEN_CONFIG => Some(TrayEvent::OpenConfig),
        MENU_ID_QUIT => Some(TrayEvent::Quit),
        _ => None,
    }
}

/// Whether a mouse message on the icon should open the menu.
///
/// Both buttons open the same menu: a single click never performs an action, so
/// nothing can happen by misclick.
pub fn tray_message_action(message: u32) -> Option<TrayAction> {
    match message {
        WM_LBUTTONUP | WM_RBUTTONUP => Some(TrayAction::ShowMenu),
        _ => None,
    }
}

/// The installed icon.
pub struct Tray {
    hidden: HWND,
    icon: HICON,
    added: bool,
}

impl Tray {
    /// Adds the icon to the notification area of `hidden_hwnd`.
    pub fn install(hidden_hwnd: HWND, tooltip: &str) -> Result<Tray, PlatformError> {
        let instance = HINSTANCE(unsafe { GetModuleHandleW(None)? }.0);
        // SAFETY: the module handle is this process's, and the icon resource id
        // is the one `build.rs` embedded.
        let icon = unsafe { LoadIconW(Some(instance), PCWSTR(ICON_RESOURCE_ID as *const u16)) }
            .map_err(|error| PlatformError::new(format!("cannot load the tray icon: {error}")))?;

        let mut tray = Tray { hidden: hidden_hwnd, icon, added: false };
        let data = tray.icon_data(NIF_ICON | NIF_MESSAGE | NIF_TIP, tooltip);

        // SAFETY: `data` is a fully initialised NOTIFYICONDATAW whose `cbSize`
        // describes it, and the window outlives the icon.
        if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
            return Err(PlatformError::new(format!(
                "Shell_NotifyIconW(NIM_ADD) failed (error {})",
                unsafe { GetLastError() }.0
            )));
        }
        tray.added = true;
        log::info!("tray: icon installed on hwnd 0x{:x}", hidden_hwnd.0 as isize);
        Ok(tray)
    }

    pub fn hidden_hwnd(&self) -> HWND {
        self.hidden
    }

    /// Shows the menu and reports what the user chose, if anything.
    pub fn handle_message(&mut self, action: TrayAction) -> Option<TrayEvent> {
        match action {
            TrayAction::ShowMenu => self.show_menu(),
        }
    }

    /// Removes the icon.
    ///
    /// Every exit path calls this; an icon left behind lingers as a ghost until
    /// the mouse happens to pass over it (spec §11).
    pub fn remove(&mut self) {
        if !self.added {
            return;
        }
        self.added = false;

        let data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hidden,
            uID: TRAY_ICON_ID,
            ..Default::default()
        };
        // SAFETY: `data` names the icon this handle installed.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
        }
        log::info!("tray: icon removed");
    }

    /// Shows a balloon. Used to report a failure the window cannot.
    pub fn notify(&mut self, title: &str, text: &str) {
        if !self.added {
            return;
        }
        let mut data = self.icon_data(NIF_INFO, "");
        write_wide(&mut data.szInfo, text);
        write_wide(&mut data.szInfoTitle, title);
        // SAFETY: as in `install`; NIM_MODIFY only touches the fields named by
        // `uFlags`.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    fn icon_data(
        &self,
        flags: windows::Win32::UI::Shell::NOTIFY_ICON_DATA_FLAGS,
        tooltip: &str,
    ) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hidden,
            uID: TRAY_ICON_ID,
            uFlags: flags,
            uCallbackMessage: TRAY_CALLBACK_MESSAGE,
            hIcon: self.icon,
            ..Default::default()
        };
        write_wide(&mut data.szTip, tooltip);
        data
    }

    fn show_menu(&mut self) -> Option<TrayEvent> {
        // SAFETY: every handle is created and destroyed within this function, and
        // the labels outlive the calls that read them.
        unsafe {
            let menu = CreatePopupMenu().ok()?;
            for item in MENU_ITEMS {
                let label = wide(item.label);
                let _ = AppendMenuW(menu, MF_STRING, item.id as usize, PCWSTR(label.as_ptr()));
            }

            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);

            // Without this the menu does not dismiss when the user clicks
            // somewhere else — the reason the icon needs its own window.
            let _ = SetForegroundWindow(self.hidden);

            // The return value is the chosen command id because of
            // TPM_RETURNCMD; 0 means the menu was dismissed without a choice, and
            // no item has that id.
            let chosen = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY,
                cursor.x,
                cursor.y,
                None,
                self.hidden,
                None,
            );

            // Documented companion to SetForegroundWindow: without it the next
            // context menu can be swallowed.
            let _ = PostMessageW(Some(self.hidden), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);

            event_for(chosen.0 as u32)
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
    }
}

/// Opens the configuration with whatever the user has associated with `.toml`.
pub fn open_config(path: &std::path::Path) -> Result<(), PlatformError> {
    let operation = wide("open");
    let file = wide(&path.to_string_lossy());

    // SAFETY: both strings are NUL-terminated and outlive the call.
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(operation.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };

    // ShellExecuteW reports success as anything greater than 32.
    let code = result.0 as isize;
    if code <= 32 {
        return Err(PlatformError::new(format!("ShellExecuteW returned {code}")));
    }
    Ok(())
}

/// A NUL-terminated UTF-16 copy of `text`.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Copies `text` into a fixed-size wide buffer, truncating rather than
/// overflowing. The terminator is included when it fits, and the buffer is
/// zeroed beforehand so a truncated string still ends.
fn write_wide(buffer: &mut [u16], text: &str) {
    buffer.fill(0);
    for (slot, unit) in buffer.iter_mut().zip(text.encode_utf16()) {
        *slot = unit;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::WM_MOUSEMOVE;

    #[test]
    fn exactly_three_menu_items_are_declared_in_a_fixed_order() {
        assert_eq!(MENU_ITEMS.len(), 3);
        assert_eq!(MENU_ITEMS[0].label, "移动窗口");
        assert_eq!(MENU_ITEMS[1].label, "打开配置文件");
        assert_eq!(MENU_ITEMS[2].label, "退出");
    }

    #[test]
    fn menu_ids_map_to_events() {
        assert_eq!(event_for(MENU_ID_MOVE), Some(TrayEvent::MoveWindow));
        assert_eq!(event_for(MENU_ID_OPEN_CONFIG), Some(TrayEvent::OpenConfig));
        assert_eq!(event_for(MENU_ID_QUIT), Some(TrayEvent::Quit));
        assert_eq!(event_for(0), None);
        assert_eq!(event_for(MENU_ID_QUIT + 1), None);
    }

    #[test]
    fn click_on_the_icon_opens_the_menu_rather_than_acting_directly() {
        // Both WM_LBUTTONUP and WM_RBUTTONUP are routed to the same popup, so the
        // icon never performs an action on a single click.
        assert_eq!(tray_message_action(WM_LBUTTONUP), Some(TrayAction::ShowMenu));
        assert_eq!(tray_message_action(WM_RBUTTONUP), Some(TrayAction::ShowMenu));
        assert_eq!(tray_message_action(WM_MOUSEMOVE), None);
    }
}
