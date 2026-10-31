//! Thin Win32 helpers: synthesised Ctrl+C, window focus, native drag/resize, monitor work area,
//! single-instance mutex and autostart registry entry.

use std::ffi::c_void;
use std::time::Duration;
use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, GetLastError, COLORREF, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, GetMonitorInfoW, MonitorFromPoint,
    MonitorFromWindow, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, SRCCOPY,
};
use windows::Win32::System::Threading::{AttachThreadInput, CreateMutexW, GetCurrentProcess, GetCurrentThreadId, SetProcessWorkingSetSize};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, keybd_event, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP};
use windows::Win32::Foundation::RECT;
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, SetForegroundWindow,
    SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, LWA_ALPHA, SC_KEYMENU, SWP_NOACTIVATE, SWP_NOZORDER, WM_SYSCOMMAND, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

const VK_CONTROL: u8 = 0x11;
const VK_MENU: u8 = 0x12;
const VK_C: u8 = 0x43;
const VK_UNASSIGNED: u8 = 0x87;

fn hwnd(h: isize) -> HWND {
    HWND(h as *mut c_void)
}

fn key(vk: u8, up: bool) {
    unsafe { keybd_event(vk, 0, if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, 0) }
}

/// Wait until the hotkey modifiers are released, then send Ctrl+C to the foreground window.
pub fn copy_selection() -> bool {
    let held = || [0x10, 0x11, 0x12, 0x5B, 0x5C].iter().any(|&v| unsafe { GetAsyncKeyState(v) } < 0);
    let mut waited = 0;
    while waited < 4000 && held() {
        std::thread::sleep(Duration::from_millis(10));
        waited += 10;
    }
    if held() {
        return false;
    }
    key(VK_CONTROL, false);
    key(VK_C, false);
    key(VK_C, true);
    key(VK_CONTROL, true);
    true
}

/// A tap on an unassigned key stops the foreground app from opening its menu bar when Alt is released.
pub fn suppress_alt_menu() {
    key(VK_UNASSIGNED, false);
    key(VK_UNASSIGNED, true);
}

pub fn foreground() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
}

pub fn focus_window(h: isize) {
    unsafe {
        let foreground_thread = GetWindowThreadProcessId(GetForegroundWindow(), None);
        let me = GetCurrentThreadId();
        let _ = AttachThreadInput(me, foreground_thread, true);
        // Any key input lifts Windows' foreground lock. A bare Alt tap would also put the window into
        // menu mode, which freezes its drawing until the next click, so a dummy key goes in between.
        key(VK_MENU, false);
        key(VK_UNASSIGNED, false);
        key(VK_UNASSIGNED, true);
        key(VK_MENU, true);
        let _ = BringWindowToTop(hwnd(h));
        let _ = SetForegroundWindow(hwnd(h));
        let _ = AttachThreadInput(me, foreground_thread, false);
    }
}

/// Cursor position in physical screen pixels.
pub fn cursor_pos() -> (i32, i32) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

/// Window rectangle (x, y, w, h) in physical pixels.
pub fn window_rect(h: isize) -> Option<(i32, i32, u32, u32)> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd(h), &mut r).ok()? };
    Some((r.left, r.top, (r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32))
}

/// Moves and resizes the window in physical pixels, bypassing toolkit scale-factor conversions.
pub fn set_window_rect(h: isize, x: i32, y: i32, w: u32, ht: u32) {
    unsafe {
        let _ = SetWindowPos(hwnd(h), None, x, y, w as i32, ht as i32, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// Work area (x, y, w, h) of the monitor under the window, or the primary monitor.
pub fn work_area(h: Option<isize>) -> (i32, i32, i32, i32) {
    unsafe {
        let mon = match h {
            Some(h) => MonitorFromWindow(hwnd(h), MONITOR_DEFAULTTONEAREST),
            None => MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY),
        };
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let r = mi.rcWork;
            (r.left, r.top, r.right - r.left, r.bottom - r.top)
        } else {
            (0, 0, 1280, 720)
        }
    }
}

/// Work area (x, y, w, h) of the monitor the cursor is on.
pub fn work_area_cursor() -> (i32, i32, i32, i32) {
    unsafe {
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let mon = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let r = mi.rcWork;
            (r.left, r.top, r.right - r.left, r.bottom - r.top)
        } else {
            work_area(None)
        }
    }
}

/// Releasing Alt on one of our windows would put it into Windows' "menu mode", in which it stops
/// redrawing until the next click (the app looks frozen). Our windows have no menu, so it is switched off.
pub fn block_menu_mode(h: isize) {
    unsafe extern "system" fn proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> LRESULT {
        if msg == WM_SYSCOMMAND && (wp.0 & 0xFFF0) == SC_KEYMENU as usize {
            return LRESULT(0);
        }
        unsafe { DefSubclassProc(h, msg, wp, lp) }
    }
    unsafe {
        let _ = SetWindowSubclass(hwnd(h), Some(proc), 0x4E58, 0);
    }
}

/// Opacity of the whole window as one picture (0..1), applied by Windows. Unlike per-element
/// opacity, darker parts such as the input field do not end up less transparent than the rest.
/// The toolkit resets the window's extended style when it is shown, so this is applied after each show.
pub fn set_window_alpha(h: isize, alpha: f32) {
    unsafe {
        let cur = GetWindowLongPtrW(hwnd(h), GWL_EXSTYLE) as u32;
        if cur & WS_EX_LAYERED.0 == 0 {
            SetWindowLongPtrW(hwnd(h), GWL_EXSTYLE, (cur | WS_EX_LAYERED.0) as isize);
        }
        let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        let _ = SetLayeredWindowAttributes(hwnd(h), COLORREF(0), a, LWA_ALPHA);
    }
}

/// Makes the window show up without becoming the foreground window (so a game keeps its focus).
pub fn set_no_activate(h: isize, enabled: bool) {
    unsafe {
        let cur = GetWindowLongPtrW(hwnd(h), GWL_EXSTYLE) as u32;
        let next = if enabled { (cur | WS_EX_NOACTIVATE.0) & !WS_EX_TOOLWINDOW.0 } else { cur & !WS_EX_NOACTIVATE.0 & !WS_EX_TOOLWINDOW.0 };
        SetWindowLongPtrW(hwnd(h), GWL_EXSTYLE, next as isize);
    }
}

/// Captures the whole monitor under the cursor (physical pixels) as RGBA. No window is shown for it,
/// so a full-screen game is not minimised. Exclusive-fullscreen games may come back black.
pub fn capture_cursor_monitor() -> Option<(u32, u32, Vec<u8>)> {
    capture_cursor_monitor_rect().map(|(_, _, w, h, px)| (w, h, px))
}

/// Same as `capture_cursor_monitor`, plus where that monitor is: (x, y, w, h, RGBA).
pub fn capture_cursor_monitor_rect() -> Option<(i32, i32, u32, u32, Vec<u8>)> {
    unsafe {
        let mut p = POINT::default();
        GetCursorPos(&mut p).ok()?;
        let mon = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            return None;
        }
        let r = mi.rcMonitor;
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        if w <= 0 || h <= 0 {
            return None;
        }
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp.into());
        let copied = BitBlt(mem, 0, 0, w, h, Some(screen), r.left, r.top, SRCCOPY | CAPTUREBLT).is_ok();
        let mut pixels = vec![0u8; (w as usize) * (h as usize) * 4];
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let lines = if copied { GetDIBits(mem, bmp, 0, h as u32, Some(pixels.as_mut_ptr() as *mut c_void), &mut info, DIB_RGB_COLORS) } else { 0 };
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        if lines == 0 {
            return None;
        }
        for px in pixels.chunks_exact_mut(4) {
            px.swap(0, 2); // BGRA -> RGBA
            px[3] = 255;
        }
        Some((r.left, r.top, w as u32, h as u32, pixels))
    }
}

/// Moves the process's pages out of its working set, so an idle app holds almost no RAM.
pub fn trim_memory() {
    unsafe {
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

/// Returns false if another instance already holds the mutex.
pub fn acquire_single_instance() -> bool {
    unsafe {
        match CreateMutexW(None, true, w!("Local\\NortexAssistSingleton")) {
            Ok(handle) => {
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    // let go of it, so the name disappears once the other instance exits (see the update restart)
                    let _ = CloseHandle(handle);
                    false
                } else {
                    true // the handle is intentionally kept for the process lifetime
                }
            }
            Err(_) => true,
        }
    }
}

pub fn set_autostart(enabled: bool) -> Result<(), String> {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const NAME: &str = "NortexAssist";
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN).map_err(|e| e.to_string())?;
    if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        key.set_value(NAME, &format!("\"{}\"", exe.display())).map_err(|e| e.to_string())
    } else {
        let _ = key.delete_value(NAME);
        Ok(())
    }
}

pub fn open_url(url: &str) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).creation_flags(0x0800_0000).spawn();
}
