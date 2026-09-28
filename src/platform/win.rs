//! Windows implementation of the platform boundary.

use crate::geometry::Rect;
use windows::Win32::Foundation::RECT;
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

/// The primary monitor's work area, i.e. its bounds minus the taskbar.
///
/// `SPI_GETWORKAREA` reports the primary display only. Task 8 replaces this with
/// per-monitor enumeration so that `window.monitor` can select a display; until
/// then every selector resolves here, and `--check-config` says so.
pub fn primary_work_area() -> Rect {
    let mut rect = RECT::default();
    // SAFETY: `rect` is a valid, properly aligned out-parameter that outlives
    // the call, and `SPI_GETWORKAREA` ignores `uiparam` and the update flags.
    let result = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut rect as *mut RECT as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    if result.is_err() {
        // A 1920x1080 fallback keeps the diagnostic useful on a machine where
        // the query fails, rather than aborting `--check-config`.
        return Rect { x: 0, y: 0, width: 1920, height: 1080 };
    }
    Rect {
        x: rect.left,
        y: rect.top,
        width: (rect.right - rect.left).max(0) as u32,
        height: (rect.bottom - rect.top).max(0) as u32,
    }
}
