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

/// Wait until the hotkey modifiers are released, then ask the foreground window to copy its selection.
/// Terminals (and editors with a built-in terminal) get Ctrl+Insert: there Ctrl+C without a
/// selection stops the running command. Everything else gets the usual Ctrl+C.
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
    let fg = foreground();
    let (class, exe) = (window_class(fg), process_name(fg));
    if is_terminal(&class, &exe) {
        key(VK_CONTROL, false);
        key_ext(VK_INSERT, false);
        key_ext(VK_INSERT, true);
        key(VK_CONTROL, true);
    } else {
        key(VK_CONTROL, false);
        key(VK_C, false);
        key(VK_C, true);
        key(VK_CONTROL, true);
    }
    true
}

const VK_INSERT: u8 = 0x2D;

/// Insert is an "extended" key: without the flag some programs read it as keypad 0.
fn key_ext(vk: u8, up: bool) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, KEYEVENTF_EXTENDEDKEY, MAPVK_VK_TO_VSC};
    unsafe {
        let scan = MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) as u8;
        let flags = if up { KEYEVENTF_KEYUP | KEYEVENTF_EXTENDEDKEY } else { KEYEVENTF_EXTENDEDKEY };
        keybd_event(vk, scan, flags, 0)
    }
}

/// A window where Ctrl+C means "interrupt": consoles, terminal apps, code editors with a terminal panel.
pub fn is_terminal(class: &str, exe: &str) -> bool {
    const CLASSES: [&str; 6] = ["ConsoleWindowClass", "CASCADIA_HOSTING_WINDOW_CLASS", "mintty", "PuTTY", "VirtualConsoleClass", "org.wezfurlong.wezterm"];
    const EXES: [&str; 22] = [
        "windowsterminal.exe", "wt.exe", "conhost.exe", "openconsole.exe", "cmd.exe", "powershell.exe", "pwsh.exe", "mintty.exe",
        "putty.exe", "kitty.exe", "conemu64.exe", "conemu.exe", "alacritty.exe", "wezterm-gui.exe", "tabby.exe", "hyper.exe",
        "warp.exe", "code.exe", "code - insiders.exe", "cursor.exe", "windsurf.exe", "vscodium.exe",
    ];
    CLASSES.iter().any(|c| class.eq_ignore_ascii_case(c)) || EXES.iter().any(|e| exe.eq_ignore_ascii_case(e))
}

fn window_class(h: isize) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd(h), &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// File name of the program that owns the window, e.g. "code.exe".
fn process_name(h: isize) -> String {
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd(h), Some(&mut pid));
        if pid == 0 {
            return String::new();
        }
        let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(p);
        if !ok {
            return String::new();
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        full.rsplit('\\').next().unwrap_or("").to_lowercase()
    }
}

/// Everything in the clipboard, format by format, so that it can be put back exactly
/// (copied files, formatted text, pictures — not only plain text).
pub struct ClipboardCopy(Vec<(u32, Vec<u8>)>);

/// Changes every time anything is put into the clipboard.
pub fn clipboard_seq() -> u32 {
    unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

fn open_clipboard() -> bool {
    use windows::Win32::System::DataExchange::OpenClipboard;
    for _ in 0..25 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// Formats that are not plain memory blocks (GDI handles) or that point into another program's
/// objects: Windows rebuilds the useful ones (a bitmap from the DIB) by itself.
fn skip_format(f: u32, name: &str) -> bool {
    matches!(f, 2 | 3 | 9 | 14 | 0x80 | 0x82 | 0x83 | 0x8E) || (0x200..=0x3FF).contains(&f) || name == "DataObject" || name == "Ole Private Data"
}

fn format_name(f: u32) -> String {
    use windows::Win32::System::DataExchange::GetClipboardFormatNameW;
    let mut buf = [0u16; 128];
    let n = unsafe { GetClipboardFormatNameW(f, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn clipboard_save() -> ClipboardCopy {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{CloseClipboard, EnumClipboardFormats, GetClipboardData};
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    let mut out = Vec::new();
    if !open_clipboard() {
        return ClipboardCopy(out);
    }
    unsafe {
        let mut total = 0usize;
        let mut f = EnumClipboardFormats(0);
        while f != 0 {
            if !skip_format(f, &format_name(f)) {
                if let Ok(h) = GetClipboardData(f) {
                    let g = HGLOBAL(h.0);
                    let p = GlobalLock(g) as *const u8;
                    if !p.is_null() {
                        let n = GlobalSize(g);
                        if n > 0 && total + n <= 300 << 20 {
                            out.push((f, std::slice::from_raw_parts(p, n).to_vec()));
                            total += n;
                        }
                        let _ = GlobalUnlock(g);
                    }
                }
            }
            f = EnumClipboardFormats(f);
        }
        let _ = CloseClipboard();
    }
    ClipboardCopy(out)
}

pub fn clipboard_restore(c: &ClipboardCopy) {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, SetClipboardData};
    use windows::Win32::Foundation::GlobalFree;
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    if !open_clipboard() {
        return;
    }
    unsafe {
        let _ = EmptyClipboard();
        for (f, bytes) in &c.0 {
            let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, bytes.len()) else { continue };
            let p = GlobalLock(g) as *mut u8;
            if p.is_null() {
                let _ = GlobalFree(Some(g));
                continue;
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
            let _ = GlobalUnlock(g);
            // on success the clipboard owns the memory
            if SetClipboardData(*f, Some(HANDLE(g.0))).is_err() {
                let _ = GlobalFree(Some(g));
            }
        }
        let _ = CloseClipboard();
    }
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

/// Work area (x, y, w, h) of the monitor that contains the point (or the nearest one).
pub fn work_area_at(x: i32, y: i32) -> (i32, i32, i32, i32) {
    unsafe {
        let mon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
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
    // test copies with their own data folder run next to the user's copy
    #[cfg(debug_assertions)]
    if std::env::var("NORTEX_DATA_DIR").is_ok() {
        return true;
    }
    unsafe {
        #[cfg(feature = "test-copy")]
        let name = w!("Local\\NortexAssistTestCopy");
        #[cfg(not(feature = "test-copy"))]
        let name = w!("Local\\NortexAssistSingleton");
        match CreateMutexW(None, true, name) {
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
    // the test copy must not touch the autostart entry of the installed program
    if cfg!(feature = "test-copy") {
        let _ = enabled;
        return Ok(());
    }
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

/// The system "open" dialog for files (several can be picked) or for a folder. Blocks until it is
/// closed, so call it from a helper thread. `owner` keeps the dialog on top of our window.
pub fn pick_paths(folder: bool, owner: isize) -> Vec<std::path::PathBuf> {
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, FOS_ALLOWMULTISELECT, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH};
    let mut out = Vec::new();
    unsafe {
        let inited = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let mut run = || -> windows::core::Result<()> {
            let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
            let mut opts = dlg.GetOptions()? | FOS_ALLOWMULTISELECT | FOS_FORCEFILESYSTEM;
            if folder {
                opts |= FOS_PICKFOLDERS;
            }
            dlg.SetOptions(opts)?;
            let title = if folder { crate::i18n::tr("Выберите папку с файлами") } else { crate::i18n::tr("Выберите файлы") };
            let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
            dlg.SetTitle(windows::core::PCWSTR(wide.as_ptr()))?;
            dlg.Show(Some(hwnd(owner)))?; // Err when the user cancels
            let items = dlg.GetResults()?;
            for i in 0..items.GetCount()? {
                let name = items.GetItemAt(i)?.GetDisplayName(SIGDN_FILESYSPATH)?;
                out.push(std::path::PathBuf::from(name.to_string()?));
                CoTaskMemFree(Some(name.0 as *const c_void));
            }
            Ok(())
        };
        let _ = run();
        if inited {
            CoUninitialize();
        }
    }
    out
}

/// Runs a cmd.exe script that outlives this process (hidden window); used to finish uninstalling.
pub fn run_detached(script: &str) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    let _ = std::process::Command::new("cmd")
        .raw_arg(format!("/d /s /c \"{script}\""))
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn();
}

/// Windows' own "app mode" colour (Settings → Personalisation → Colours): true for dark.
pub fn windows_dark() -> bool {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_value::<u32, _>("AppsUseLightTheme"))
        .map(|light| light == 0)
        .unwrap_or(true)
}

/// True when the Windows display language is English (any country).
pub fn windows_english() -> bool {
    let id = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
    id & 0x3ff == 0x09
}

/// Code editors copy the whole current line on Ctrl+C when nothing is selected. They mark such a
/// copy in the clipboard: VS Code (and other Electron editors) with `"isFromEmptySelection":true`
/// in its editor data, Visual Studio with an extra format. Call right after the copy.
pub fn clipboard_is_line_copy() -> bool {
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW};
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows::Win32::Foundation::HGLOBAL;
    unsafe {
        let fmt = |name: &str| {
            let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            RegisterClipboardFormatW(windows::core::PCWSTR(wide.as_ptr()))
        };
        for vs in ["MSDEVLineSelect", "VisualStudioEditorOperationsLineCutCopyClipboardTag"] {
            if IsClipboardFormatAvailable(fmt(vs)).is_ok() {
                return true;
            }
        }
        let editor_formats = [fmt("vscode-editor-data"), fmt("Chromium Web Custom MIME Data Format")];
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        if !opened {
            return false;
        }
        let mut found = false;
        for f in editor_formats {
            if f == 0 || IsClipboardFormatAvailable(f).is_err() {
                continue;
            }
            let Ok(h) = GetClipboardData(f) else { continue };
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u8;
            if p.is_null() {
                continue;
            }
            let n = GlobalSize(g).min(1 << 20);
            let bytes = std::slice::from_raw_parts(p, n);
            found = has_empty_selection_flag(bytes);
            let _ = GlobalUnlock(g);
            if found {
                break;
            }
        }
        let _ = CloseClipboard();
        found
    }
}

/// Looks for `"isFromEmptySelection":true` written as UTF-8 or UTF-16 (Chromium keeps custom
/// clipboard types as UTF-16).
fn has_empty_selection_flag(bytes: &[u8]) -> bool {
    let utf8: String = String::from_utf8_lossy(bytes).chars().filter(|c| !c.is_whitespace() && *c != '\0').collect();
    if utf8.contains("\"isFromEmptySelection\":true") {
        return true;
    }
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let utf16: String = String::from_utf16_lossy(&units).chars().filter(|c| !c.is_whitespace()).collect();
    utf16.contains("\"isFromEmptySelection\":true")
}

#[cfg(test)]
mod tests {
    #[test]
    fn line_copy_flag() {
        let json = r#"{"version":1,"isFromEmptySelection": true,"mode":"rust"}"#;
        assert!(super::has_empty_selection_flag(json.as_bytes()));
        let wide: Vec<u8> = json.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert!(super::has_empty_selection_flag(&wide));
        assert!(!super::has_empty_selection_flag(br#"{"isFromEmptySelection":false}"#));
    }
}

/// Is the numeric-keypad digit `d` held down right now? (The toolkit reports keypad digits like
/// the ones above the letters, so the hotkey field asks Windows.)
pub fn numpad_down(d: u32) -> bool {
    d <= 9 && unsafe { GetAsyncKeyState(0x60 + d as i32) } < 0
}
