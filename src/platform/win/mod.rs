//! Windows implementation of the platform boundary.
//!
//! A `WS_EX_LAYERED` popup whose pixels are a top-down 32bpp DIB presented with
//! `UpdateLayeredWindow`. This is the only module that talks to Win32, which is
//! what lets every other module be tested headlessly (spec: architecture).

use crate::config::{MonitorSelector, WindowConfig};
use crate::geometry::Rect;
use std::cell::Cell;
use std::fmt;
use std::mem::size_of;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EndPaint,
    EnumDisplayMonitors, GetDC, GetMonitorInfoW, MonitorFromWindow, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
    HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, PAINTSTRUCT, AC_SRC_ALPHA, AC_SRC_OVER,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, KillTimer, LoadCursorW, LoadIconW, PostMessageW, PostQuitMessage,
    RegisterClassW, SetCursor, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    SystemParametersInfoW, TranslateMessage, UpdateLayeredWindow, CS_HREDRAW, CS_VREDRAW,
    GWL_EXSTYLE, GWLP_USERDATA, HWND_TOPMOST, IDC_ARROW, IDC_SIZEALL, MONITORINFOF_PRIMARY, MSG,
    SPI_GETWORKAREA, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SW_SHOWNOACTIVATE, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, ULW_ALPHA, WM_APP, WM_CLOSE,
    WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ERASEBKGND, WM_PAINT, WM_SETCURSOR, WM_TIMER,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    WS_POPUP,
};

const CLASS_NAME: PCWSTR = w!("GlassineWindow");
const WINDOW_TITLE: PCWSTR = w!("glassine");

/// Window style mask, fixed by Global Constraints (exstyle `0x0808_00A8`).
const WINDOW_EX_STYLE_MASK: u32 = WS_EX_LAYERED.0
    | WS_EX_TOOLWINDOW.0
    | WS_EX_TRANSPARENT.0
    | WS_EX_NOACTIVATE.0
    | WS_EX_TOPMOST.0;

const TIMER_TICK: usize = 1;
const TIMER_REASSERT: usize = 2;
const TIMER_CLOSE_AFTER: usize = 3;

/// The icon resource id `winresource` assigns to `assets/glassine.ico`.
const ICON_RESOURCE_ID: u16 = 1;

/// Why a window operation failed.
#[derive(Debug)]
pub struct PlatformError {
    message: String,
}

impl PlatformError {
    fn new(message: impl Into<String>) -> PlatformError {
        PlatformError { message: message.into() }
    }
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PlatformError {}

impl From<windows::core::Error> for PlatformError {
    fn from(error: windows::core::Error) -> Self {
        PlatformError::new(error.to_string())
    }
}

/// One attached display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorInfo {
    pub device_name: String,
    pub work_area: Rect,
    pub primary: bool,
}

/// Everything the window reports to its owner.
///
/// Every callback receives the window, because the loop owns it while it runs:
/// the handler cannot reach a window it does not own, and the alternative — a
/// self-referential pointer from the handler back to the loop's window — would
/// put two mutable paths to the same window in play.
///
/// There is deliberately no `on_command`: the window has no channel to drain,
/// and the queue belongs to the application (spec: thread model). A wake-up is
/// all this layer can honestly deliver.
pub trait WindowEvents {
    /// A scheduled redraw is due.
    fn on_tick(&mut self, window: &mut LayeredWindow);
    /// The HTTP thread has queued work; drain it.
    fn on_wake(&mut self, window: &mut LayeredWindow);
    fn on_dpi_changed(&mut self, window: &mut LayeredWindow, dpi: u32);
    fn on_display_change(&mut self, window: &mut LayeredWindow);
    /// The window is being destroyed; release anything tied to its lifetime,
    /// such as the tray icon.
    fn on_quit_requested(&mut self, window: &mut LayeredWindow);
}

/// Window state the `wndproc` owns and mutates.
///
/// Kept separate from [`LayeredWindow`] so that the `wndproc` never has to
/// reach through the `&mut LayeredWindow` held by `run_message_loop`.
struct MessageContext {
    /// The window the loop owns while it runs, so callbacks can be handed it.
    window: Cell<*mut LayeredWindow>,
    handler: Cell<Option<*mut (dyn WindowEvents + 'static)>>,
    /// The origin `present` draws at. Also the authority `set_position`
    /// updates, so the two cannot drift apart.
    origin: Cell<(i32, i32)>,
    dpi: Cell<u32>,
    destroyed: Cell<bool>,
    move_mode: Cell<bool>,
    click_through: Cell<bool>,
}

/// The layered popup and its DIB surface.
pub struct LayeredWindow {
    hwnd: HWND,
    screen_dc: HDC,
    mem_dc: HDC,
    bitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    /// Start of the DIB's pixels; valid until `destroy` deletes the bitmap.
    bits: *mut u8,
    rect: Rect,
    /// Owns the message state; never read directly, only through `context`.
    _context: Box<MessageContext>,
    /// The single path every access to the message state takes — this type's and
    /// the `wndproc`'s alike. A second path would be a second borrow of the same
    /// state, which is what this arrangement exists to avoid.
    context: *mut MessageContext,
}

impl LayeredWindow {
    pub fn create(config: &WindowConfig, rect: Rect) -> Result<LayeredWindow, PlatformError> {
        unsafe {
            let instance = HINSTANCE(windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?.0);

            let class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                hIcon: LoadIconW(Some(instance), PCWSTR(ICON_RESOURCE_ID as *const u16))
                    .unwrap_or_default(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: CLASS_NAME,
                ..Default::default()
            };
            // A second window in this process re-registers the same class; the
            // call then fails harmlessly and creation below still succeeds.
            RegisterClassW(&class);

            let hwnd = CreateWindowExW(
                windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(WINDOW_EX_STYLE_MASK),
                CLASS_NAME,
                WINDOW_TITLE,
                WS_POPUP,
                rect.x,
                rect.y,
                rect.width as i32,
                rect.height as i32,
                None,
                None,
                Some(instance),
                None,
            )?;

            let mut context = Box::new(MessageContext {
                window: Cell::new(std::ptr::null_mut()),
                handler: Cell::new(None),
                origin: Cell::new((rect.x, rect.y)),
                dpi: Cell::new(GetDpiForWindow(hwnd)),
                destroyed: Cell::new(false),
                move_mode: Cell::new(false),
                click_through: Cell::new(true),
            });
            // Derived from a mutable borrow, so later writes through it — the
            // `wndproc`'s included — are legitimate.
            let context_pointer: *mut MessageContext = &mut *context;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, context_pointer as isize);

            let screen_dc = GetDC(None);
            let mem_dc = CreateCompatibleDC(Some(screen_dc));

            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: rect.width as i32,
                    // Negative height selects a top-down DIB, so row 0 is the top
                    // row and stride is exactly width * 4 with nothing to pad.
                    biHeight: -(rect.height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(mem_dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let old_bitmap = SelectObject(mem_dc, bitmap.into());

            // Shown without activation: a sticky note must never take focus.
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            if config.reassert_topmost_ms > 0 {
                SetTimer(Some(hwnd), TIMER_REASSERT, config.reassert_topmost_ms, None);
            }

            Ok(LayeredWindow {
                hwnd,
                screen_dc,
                mem_dc,
                bitmap,
                old_bitmap,
                bits: bits as *mut u8,
                rect,
                _context: context,
                context: context_pointer,
            })
        }
    }

    /// The message state, reached through the one pointer that owns it.
    ///
    /// Shared, not mutable: every mutable field is a `Cell`, which is what makes
    /// interior mutation legitimate here. Returning `&mut` from `&self` would be
    /// unsound, and it would also claim exclusive access this type does not have
    /// — the `wndproc` mutates the same cells at the same time.
    fn state(&self) -> &MessageContext {
        // SAFETY: `context` points into the boxed state this window owns, which
        // lives until `destroy` consumes the window. Every access — here and in
        // the `wndproc` — uses that same pointer, so no second borrow of that
        // state can exist. The window's own fields live in a different
        // allocation, so borrowing them does not disturb this one.
        unsafe { &*self.context }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn origin(&self) -> (i32, i32) {
        self.state().origin.get()
    }

    pub fn dpi(&self) -> u32 {
        self.state().dpi.get()
    }

    /// Asks the window to close, exactly as closing it by hand would.
    pub fn close(&self) {
        // SAFETY: `hwnd` is alive, and posting a message is safe from any thread.
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }

    pub fn monitor_name(&self) -> String {
        unsafe {
            let monitor = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
            monitor_info(monitor).map(|info| info.device_name).unwrap_or_else(|_| "primary".to_string())
        }
    }

    /// The DIB's pixels, premultiplied BGRA once `render::rgba_to_bgra_in_place`
    /// has run.
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        let length = self.rect.width as usize * self.rect.height as usize * 4;
        // SAFETY: `bits` is the DIB's pixel base, the buffer is exactly
        // `rect.width * rect.height * 4` bytes, and the bitmap stays alive until
        // `destroy` — which consumes `self`, so no slice can outlive it.
        unsafe { std::slice::from_raw_parts_mut(self.bits, length) }
    }

    pub fn present(&mut self) -> Result<(), PlatformError> {
        let (x, y) = self.state().origin.get();
        let size = SIZE { cx: self.rect.width as i32, cy: self.rect.height as i32 };
        let source = POINT { x: 0, y: 0 };
        let destination = POINT { x, y };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };

        // SAFETY: `screen_dc` is the screen DC `UpdateLayeredWindow` requires,
        // `mem_dc` holds a DIB sized `rect`, and both outlive the call.
        unsafe {
            UpdateLayeredWindow(
                self.hwnd,
                Some(self.screen_dc),
                Some(&destination as *const POINT),
                Some(&size as *const SIZE),
                Some(self.mem_dc),
                Some(&source as *const POINT),
                COLORREF(0),
                Some(&blend as *const BLENDFUNCTION),
                ULW_ALPHA,
            )?;
        }
        Ok(())
    }

    /// Moves the window. Both the cached origin and the window itself must move:
    /// `present` draws at the cached origin, while hit-testing and
    /// `WindowFromPoint` read the window's own position. Updating only one of
    /// them makes the next redraw yank the window back.
    pub fn set_position(&mut self, x: i32, y: i32) {
        self.state().origin.set((x, y));
        // SAFETY: `hwnd` is alive, and the flags keep size, z-order and focus
        // untouched.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    pub fn set_click_through(&mut self, enabled: bool) {
        let style = unsafe { GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) };
        let transparent = WS_EX_TRANSPARENT.0 as isize;
        let updated = if enabled { style | transparent } else { style & !transparent };

        // SAFETY: `hwnd` is alive; `SWP_FRAMECHANGED` is what makes the hit
        // test notice the new style.
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, updated);
            let _ = SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
        self.state().click_through.set(enabled);
    }

    /// Re-asserts `WS_EX_TOPMOST` if something took it away.
    ///
    /// Deliberately does not touch the surface or trigger a redraw: this runs
    /// every few seconds for the life of the process, and a redraw here would
    /// break the idle-CPU ceiling (spec §14).
    pub fn reassert_topmost(&mut self) {
        reassert_topmost_if_lost(self.hwnd);
    }

    /// Arms the periodic redraw.
    pub fn set_tick_interval(&mut self, milliseconds: u32) {
        // SAFETY: `hwnd` is alive and the timer id is ours.
        unsafe {
            if milliseconds == 0 {
                let _ = KillTimer(Some(self.hwnd), TIMER_TICK);
            } else {
                SetTimer(Some(self.hwnd), TIMER_TICK, milliseconds, None);
            }
        }
    }

    /// Arms a one-shot timer that closes the window, used by `--smoke-ms` so a
    /// test can drive a real window without a user.
    pub fn close_after(&mut self, milliseconds: u32) {
        // SAFETY: `hwnd` is alive and the timer id is ours.
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_CLOSE_AFTER, milliseconds.max(1), None);
        }
    }

    /// `'static` because the pointers are parked in the window's context, which
    /// outlives any borrow: the handler is an owned `App`, not a borrow of one.
    pub fn run_message_loop(&mut self, handler: &mut (dyn WindowEvents + 'static)) {
        // Both pointers are published from here and `self` is not touched once
        // dispatching starts, so nothing the `wndproc` sees outlives this scope.
        let window: *mut LayeredWindow = self;
        let handler_pointer: *mut (dyn WindowEvents + 'static) = handler;
        let context = self.context;
        // SAFETY: `context` is this window's live message state, and `window`
        // points at the window the loop is running for.
        unsafe {
            (*context).window.set(window);
            (*context).handler.set(Some(handler_pointer));
        }

        let mut message = MSG::default();
        loop {
            // SAFETY: `message` is a valid out-parameter; a null filter means
            // "every message for this thread".
            let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
            if result.0 <= 0 {
                // 0 is WM_QUIT, -1 is an error. Both end the loop.
                break;
            }
            // SAFETY: `message` was filled by `GetMessageW`.
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }

        // SAFETY: as above. Clearing makes the loop's scope the callback's
        // outermost extent, so a callback can never be invoked after it returns.
        unsafe {
            (*context).window.set(std::ptr::null_mut());
            (*context).handler.set(None);
        }
    }

    pub fn destroy(self) {
        // SAFETY: every handle here was created in `create` and is released
        // exactly once, because `destroy` consumes `self`.
        unsafe {
            if !self.state().destroyed.get() {
                let _ = DestroyWindow(self.hwnd);
            }
            // Deselecting before deleting: a bitmap selected into a DC cannot
            // be deleted.
            let _ = SelectObject(self.mem_dc, self.old_bitmap);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.mem_dc);
            let _ = ReleaseDC(None, self.screen_dc);
        }
    }
}

/// The primary monitor's work area, i.e. its bounds minus the taskbar.
///
/// `SPI_GETWORKAREA` reports the primary display only; `enumerate_monitors` is
/// what honours `window.monitor`.
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
        // Keeps the diagnostic useful on a machine where the query fails,
        // rather than aborting `--check-config`.
        return Rect { x: 0, y: 0, width: 1920, height: 1080 };
    }
    Rect {
        x: rect.left,
        y: rect.top,
        width: (rect.right - rect.left).max(0) as u32,
        height: (rect.bottom - rect.top).max(0) as u32,
    }
}

/// Every attached display.
pub fn enumerate_monitors() -> Vec<MonitorInfo> {
    let mut found: Vec<MonitorInfo> = Vec::new();
    // SAFETY: `found` outlives the call and the callback writes only into it.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut found as *mut Vec<MonitorInfo> as isize),
        );
    }
    found
}

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _dc: HDC,
    _clip: *mut RECT,
    data: LPARAM,
) -> windows::core::BOOL {
    // SAFETY: `data` is the `&mut Vec<MonitorInfo>` handed over by
    // `enumerate_monitors`, which is alive for the whole enumeration.
    let found = unsafe { &mut *(data.0 as *mut Vec<MonitorInfo>) };
    if let Ok(info) = monitor_info(monitor) {
        found.push(info);
    }
    windows::core::BOOL(1)
}

fn monitor_info(monitor: HMONITOR) -> Result<MonitorInfo, PlatformError> {
    // `MONITORINFO` as projected has no `szDevice`, only the EX form does, and
    // the two differ solely by the trailing name array. `cbSize` must describe
    // the EX struct so the call fills the name in.
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `monitorInfo` is the leading field of `info`, and `cbSize` was set
    // to the full EX size.
    if !unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo as *mut _) }.as_bool() {
        return Err(PlatformError::new("GetMonitorInfoW failed"));
    }
    let work = info.monitorInfo.rcWork;
    Ok(MonitorInfo {
        device_name: wide_to_string(&info.szDevice),
        work_area: Rect {
            x: work.left,
            y: work.top,
            width: (work.right - work.left).max(0) as u32,
            height: (work.bottom - work.top).max(0) as u32,
        },
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
    })
}

/// Chooses the work area to anchor against and the device name to report.
///
/// A selector that matches nothing — an undocked laptop, a renamed display —
/// falls back to the primary monitor rather than failing the start (Review
/// Focus). An empty list still yields a usable rectangle.
pub fn resolve_work_area(selector: &MonitorSelector, monitors: &[MonitorInfo]) -> (Rect, String) {
    let selected = match selector {
        MonitorSelector::Primary => monitors.iter().find(|monitor| monitor.primary),
        MonitorSelector::Index(index) => monitors.get(*index),
        MonitorSelector::DeviceName(name) => monitors
            .iter()
            .find(|monitor| monitor.device_name.eq_ignore_ascii_case(name)),
    };
    if let Some(monitor) = selected {
        return (monitor.work_area, monitor.device_name.clone());
    }

    if let Some(primary) = monitors.iter().find(|monitor| monitor.primary) {
        log::warn!(
            "window: monitor {selector:?} is not attached; anchoring to {}",
            primary.device_name
        );
        return (primary.work_area, primary.device_name.clone());
    }

    log::warn!("window: no monitor reported itself; assuming 1920x1080 at the origin");
    (Rect { x: 0, y: 0, width: 1920, height: 1080 }, "primary".to_string())
}

/// Whether the topmost bit has gone missing from an extended style.
pub fn needs_topmost_reassert(exstyle: u32) -> bool {
    exstyle & WS_EX_TOPMOST.0 == 0
}

fn reassert_topmost_if_lost(hwnd: HWND) {
    // SAFETY: `hwnd` is alive for the duration of the call.
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    if !needs_topmost_reassert(style) {
        return;
    }
    // SAFETY: `SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE` leaves the geometry and
    // focus untouched; only the z-order changes.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
    log::info!("window: topmost was lost (exstyle {style:#010x}); re-asserted");
}

/// Calls `event` with the owner's handler and the window, if a loop is running.
///
/// # Safety
///
/// `host` and `window` must both be the pointers `run_message_loop` published
/// for this thread; that publication is the only way either is non-null, and the
/// loop clears them before its scope ends.
unsafe fn dispatch(
    host: Option<*mut (dyn WindowEvents + 'static)>,
    window: *mut LayeredWindow,
    event: impl FnOnce(&mut dyn WindowEvents, &mut LayeredWindow),
) {
    if let Some(host) = host {
        if !window.is_null() {
            event(&mut *host, &mut *window);
        }
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // Before `create` stores the context — and for any window of this class we
    // did not create — there is nothing to dispatch to.
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut MessageContext;
    if pointer.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    // SAFETY: published in `create` from a mutable borrow, and cleared only when
    // the window itself is gone. Shared, because every field it exposes is a
    // `Cell` and mutation goes through those.
    let context = unsafe { &*pointer };
    let host = context.handler.get();
    let window = context.window.get();

    match message {
        WM_PAINT => {
            // Mandatory pairing: skipping it leaves WM_PAINT unacknowledged, so
            // it is re-posted forever and burns a core for nothing.
            let mut paint = PAINTSTRUCT::default();
            // SAFETY: `paint` is a valid out-parameter.
            unsafe {
                let _ = BeginPaint(hwnd, &mut paint);
                let _ = EndPaint(hwnd, &paint);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_TIMER => {
            match wparam.0 {
                TIMER_TICK => unsafe {
                    dispatch(host, window, |handler, window| handler.on_tick(window))
                },
                TIMER_REASSERT => reassert_topmost_if_lost(hwnd),
                TIMER_CLOSE_AFTER => {
                    // SAFETY: one-shot; the timer is ours and `hwnd` is alive.
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_CLOSE_AFTER);
                        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        // `WM_APP + 1` is the wake-up the HTTP thread posts (spec §12).
        m if m == WM_APP + 1 => {
            unsafe { dispatch(host, window, |handler, window| handler.on_wake(window)) };
            LRESULT(0)
        }
        WM_DPICHANGED => {
            // High word is the Y DPI; PerMonitorV2 keeps both equal.
            let dpi = (wparam.0 & 0xFFFF) as u32;
            context.dpi.set(dpi);
            unsafe {
                dispatch(host, window, |handler, window| {
                    handler.on_dpi_changed(window, dpi)
                })
            };
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            unsafe {
                dispatch(host, window, |handler, window| {
                    handler.on_display_change(window)
                })
            };
            LRESULT(0)
        }
        WM_SETCURSOR => {
            if context.move_mode.get() {
                // SAFETY: the cursor resource is owned by the system.
                unsafe {
                    SetCursor(Some(
                        LoadCursorW(None, IDC_SIZEALL).unwrap_or_default(),
                    ));
                }
                LRESULT(1)
            } else {
                unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
            }
        }
        WM_CLOSE => {
            // SAFETY: `hwnd` is alive; `WM_DESTROY` follows and posts the quit.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            context.destroyed.set(true);
            unsafe {
                dispatch(host, window, |handler, window| {
                    handler.on_quit_requested(window)
                })
            };
            // SAFETY: ends the message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn wide_to_string(wide: &[u16]) -> String {
    let length = wide.iter().position(|&unit| unit == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..length])
}

#[cfg(test)]
mod monitor_tests {
    use super::*;

    fn monitors() -> Vec<MonitorInfo> {
        vec![
            MonitorInfo {
                device_name: "\\\\.\\DISPLAY1".into(),
                work_area: Rect { x: 0, y: 0, width: 1920, height: 1040 },
                primary: true,
            },
            MonitorInfo {
                device_name: "\\\\.\\DISPLAY2".into(),
                work_area: Rect { x: 1920, y: 0, width: 1280, height: 984 },
                primary: false,
            },
        ]
    }

    #[test]
    fn primary_index_and_device_name_select_the_expected_work_area() {
        let m = monitors();
        assert_eq!(resolve_work_area(&MonitorSelector::Primary, &m).1, "\\\\.\\DISPLAY1");
        assert_eq!(resolve_work_area(&MonitorSelector::Index(1), &m).0.x, 1920);
        assert_eq!(
            resolve_work_area(&MonitorSelector::DeviceName("\\\\.\\DISPLAY2".into()), &m).0.x,
            1920
        );
    }

    #[test]
    fn a_monitor_that_is_not_attached_falls_back_to_primary_without_failing() {
        // Review Focus: an undocked laptop must not fail to start.
        let m = monitors();
        assert_eq!(resolve_work_area(&MonitorSelector::Index(99), &m).1, "\\\\.\\DISPLAY1");
        assert_eq!(
            resolve_work_area(&MonitorSelector::DeviceName("\\\\.\\DISPLAY9".into()), &m).1,
            "\\\\.\\DISPLAY1"
        );
        // An empty list must still produce a usable rectangle.
        let (area, name) = resolve_work_area(&MonitorSelector::Index(0), &[]);
        assert_eq!((area.width, area.height), (1920, 1080));
        assert!(!name.is_empty());
    }

    #[test]
    fn topmost_is_only_reasserted_when_the_style_was_lost() {
        assert!(!needs_topmost_reassert(0x0808_00A8));
        assert!(needs_topmost_reassert(0x0808_00A8 & !0x0000_0008));
    }
}
