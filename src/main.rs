#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod attach;
mod changelog;
mod clip;
mod config;
mod crypto;
mod history;
mod i18n;
mod llm;
mod logging;
mod md;
mod native;
mod update;
mod voice;

#[cfg(debug_assertions)]
mod demo;

use config::{Config, Hotkeys, QuickAction, Rect, ACCENTS, HK_ASK, HK_CHAT, HK_NOTE, HK_RETRY, HK_SHOT, HK_SMART, HK_VOICE, SHOT_MODES};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use history::History;
use llm::{Request, Turn};
use logging::log_err;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows;
use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

slint::include_modules!();

const MAX_SHOTS: usize = 4;
/// Size of a window that has never been resized (logical px).
const NORMAL_SIZE: (f32, f32) = (380.0, 220.0);
/// The settings need room: a lower window grows to this height while they are open (and goes
/// back when they close; the bigger size is never remembered).
const SETTINGS_MIN: (f32, f32) = (380.0, 540.0);
const BLUR_GRACE: Duration = Duration::from_millis(400);
const OVERLAY_TIMEOUT: Duration = Duration::from_secs(20);
const TOAST_TIME: Duration = Duration::from_millis(4000);
/// Alt+E brings the last dialog back if it is younger than this; otherwise a fresh one starts.
const CHAT_KEEP: Duration = Duration::from_secs(15 * 60);
/// How long a smart Alt+Q waits for the app to copy the selection before taking a screenshot.
const SMART_COPY_WAIT: Duration = Duration::from_millis(450);
const CLASSIC_COPY_WAIT: Duration = Duration::from_millis(2000);
/// Hotkey ids that are not settings slots.
const SLOT_UNINSTALL: usize = 99;
const SLOT_ESCAPE: usize = 98;

// A saved model that is not in this list shows up as "own model" with its id filled in.
const GEMINI_IDS: [&str; 4] = ["gemini-3.5-flash-lite", "gemini-3.8-flash", "gemini-3.1-pro-preview", "gemini-2.5-flash"];
const CUSTOM_MODEL_INDEX: i32 = GEMINI_IDS.len() as i32;

/// One question and its answer in the dialog on screen.
#[derive(Default)]
struct Answer {
    text: String,
    /// what was sent (to ask again); None for answers opened from the history
    req: Option<Request>,
    is_error: bool,
    /// the question as remembered in the dialog for the model
    record_q: Option<String>,
    /// what was asked, shown in the chip above the answer
    question: String,
    shots: usize,
    /// small grey line under the answer ("Gemini answered ...")
    note: String,
    /// a button under an error: "gemini-key", "mic", "settings"
    action: String,
    open: bool,
    waiting: bool,
    /// names of the attached files, shown under the question
    files: String,
    /// asked with a button under the answer ("Объясни"...): the history names it after the dialog's question
    quick: bool,
    /// the history entry of this answer: "another answer" replaces it instead of adding a second one
    hist_id: Option<i64>,
}

/// Files of a request as one short line: "q1.txt, notes.md and 3 more".
fn files_line(files: &[llm::Attachment]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = files.iter().take(3).map(|f| f.name.rsplit(['\\', '/']).next().unwrap_or(&f.name)).collect();
    let list = names.join(", ");
    if files.len() > 3 {
        i18n::trf("{names} и ещё {n}", &[("names", &list), ("n", &(files.len() - 3).to_string())])
    } else {
        list
    }
}

/// A saved question as people read it: "[скриншот]" (the form kept in the history) becomes "Снимок экрана".
fn pretty_question(q: &str) -> String {
    let re = regex::Regex::new(r"^\[(?:скриншот|screenshot)(?: x(\d+))?\]").unwrap();
    match re.captures(q) {
        Some(c) => {
            let name = match c.get(1) {
                Some(n) => i18n::trf("Снимки экрана ({n})", &[("n", n.as_str())]),
                None => i18n::tr("Снимок экрана"),
            };
            format!("{name}{}", &q[c.get(0).unwrap().end()..])
        }
        None => q.to_string(),
    }
}

struct DragState {
    resize: bool,
    cursor: (i32, i32),
    rect: Rect,
}

#[derive(Clone)]
enum Composer {
    Free,
    Selection(String),
    /// screenshots live in `pending` like pasted images, so they get thumbnails
    Shot,
    FollowUp,
}

/// A voice recording in progress (or being turned into text).
struct VoiceRun {
    rec: Option<voice::Recorder>,
    started: Instant,
    /// true: the window was closed, the pill shows the recording and the question is sent at once
    popup: bool,
    /// started by holding the voice key (released = done); false: the microphone button
    by_key: bool,
    /// text selected in the other app when the key was pressed (the speech becomes a note to it)
    selection: Arc<Mutex<Option<String>>>,
    transcribing: bool,
    id: u64,
}

struct App {
    ui: slint::Weak<MainWindow>,
    cfg: Config,
    store: Arc<crypto::Store>,
    history: History,

    turns: Vec<Answer>,
    feed: Rc<VecModel<FeedItem>>,
    status: String,
    last_activity: Instant,
    req_id: u64,
    /// id of the request whose answer is still awaited (0: none); the worker thread stops when it changes
    live_req: Arc<AtomicU64>,
    busy: bool,
    in_flight: Option<(Request, Option<String>)>,
    update: Option<update::Release>,
    update_dismissed: bool,
    updating: bool,
    /// read by the background update thread
    auto_update: Arc<AtomicBool>,
    chat: Vec<Turn>,
    composer: Option<Composer>,
    pending: Vec<(String, Image)>,
    /// text files and folders attached with the paperclip: (chip label, is a folder, files)
    attached: Vec<(String, bool, Vec<llm::Attachment>)>,
    /// the file dialog is open: the window must not hide because it lost focus
    dialog_open: Arc<AtomicBool>,
    /// the newest answer is shown as plain selectable text instead of formatted blocks
    select_mode: bool,
    uninstall_left: u32,
    uninstall_timer: Timer,
    /// after the next screenshot the question is sent at once (smart Alt+Q)
    shot_auto: bool,

    visible: bool,
    overlay_view: bool,
    auto_hide: Option<Instant>,
    last_hide: Instant,
    /// housekeeping timer (blur-hide, auto-hide, geometry saving): runs only while the window is visible
    tick_timer: Timer,
    ticks: u32,
    placed: bool,
    pinned: bool,
    hist_open: bool,
    shown_at: Instant,
    watched: Option<(Rect, Instant)>,
    drag: Option<DragState>,
    snip_token: Arc<AtomicU64>,
    /// own area picker while it is open, with the full screenshot it shows and (token, was_visible)
    picker: Option<SnipWindow>,
    picker_src: Option<(u32, u32, Vec<u8>)>,
    picker_ctx: (u64, bool),
    toast_gen: u64,
    save_gen: u64,
    tray: Option<tray_icon::TrayIcon>,
    dark: bool,

    hot_mgr: GlobalHotKeyManager,
    registered: Vec<HotKey>,
    hot_ids: HashMap<u32, usize>,
    /// slots whose key could not be registered (another program has it)
    hot_failed: Vec<usize>,
    esc_hotkey: Option<HotKey>,

    // settings
    key_check_id: u64,
    key_edit_gen: u64,
    /// keys that passed the check this session (failures are not remembered: the network may be back)
    key_checks: HashMap<String, bool>,
    /// the settings wait for a new key combination: the hotkeys are switched off meanwhile
    capturing: bool,
    /// a problem with the combination just pressed, shown under that key's row
    key_note: Option<(usize, String)>,
    smart_explain: bool,
    /// the update strip shows "updated to X" with "what's new"
    news_pending: bool,
    /// the window size before the settings made it taller
    pre_settings: Option<Rect>,
    /// the window has had the keyboard focus since it was shown
    had_focus: bool,

    // voice
    voice: Option<VoiceRun>,
    voice_id: u64,
    voice_timer: Timer,
    pill: Option<VoicePill>,
}

thread_local! {
    /// tray menu entries, kept to retitle them when the language changes
    static TRAY_ITEMS: RefCell<Option<(MenuItem, MenuItem, MenuItem)>> = const { RefCell::new(None) };
    static APP: RefCell<Option<Rc<RefCell<App>>>> = const { RefCell::new(None) };
}

fn with_app(f: impl FnOnce(&mut App)) {
    let rc = APP.with(|a| a.borrow().clone());
    if let Some(rc) = rc {
        match rc.try_borrow_mut() {
            Ok(mut app) => f(&mut app),
            Err(_) => log_err("re-entrant access to app state ignored"),
        }
    }
}

fn record_turn(chat: &mut Vec<Turn>, q: &str, a: &str) {
    let n = chat.len();
    if n >= 2 && chat[n - 1].model && !chat[n - 2].model && chat[n - 2].text == q {
        chat[n - 1].text = a.to_string();
    } else {
        chat.push(Turn { model: false, text: q.to_string() });
        chat.push(Turn { model: true, text: a.to_string() });
    }
    if chat.len() > 20 {
        let extra = chat.len() - 20;
        chat.drain(..extra);
    }
}

/// "1 файл", "3 файла", "12 файлов"
fn files_word(n: usize) -> String {
    if i18n::is_en() {
        return format!("{n} {}", if n == 1 { "file" } else { "files" });
    }
    let word = match (n % 10, n % 100) {
        (_, 11..=14) => "файлов",
        (1, _) => "файл",
        (2..=4, _) => "файла",
        _ => "файлов",
    };
    format!("{n} {word}")
}

fn make_image(w: u32, h: u32, rgba: &[u8]) -> Image {
    Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(rgba, w, h))
}

/// One-line, length-limited form of a question for the chip above the answer.
fn question_line(q: &str) -> String {
    let flat = q.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 160 { flat.chars().take(160).collect::<String>() + "…" } else { flat }
}

fn hwnd_of(w: &slint::Window) -> Option<isize> {
    match w.window_handle().window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// The user's own accent colour, with dark or white ink depending on how light it is.
fn set_custom_theme(theme: &Theme, (r, g, b): (u8, u8, u8)) {
    theme.set_custom_accent(slint::Color::from_rgb_u8(r, g, b));
    let lum = 0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32;
    theme.set_custom_ink(if lum > 150.0 { slint::Color::from_rgb_u8(0x15, 0x15, 0x18) } else { slint::Color::from_rgb_u8(0xff, 0xff, 0xff) });
}

/// Hue in degrees, saturation and value 0..1 (the palette's coordinates).
fn rgb_to_hsv((r, g, b): (u8, u8, u8)) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, if max == 0.0 { 0.0 } else { d / max }, max)
}

/// "сегодня, 14:05", "вчера, 09:30", "28.10, 18:00" or "28.10.2025".
fn when_text(ms: i64) -> String {
    use chrono::Datelike;
    let Some(d) = chrono::DateTime::from_timestamp_millis(ms) else { return String::new() };
    let d = d.with_timezone(&chrono::Local);
    let today = chrono::Local::now().date_naive();
    let day = d.date_naive();
    let hm = d.format("%H:%M");
    if day == today {
        i18n::trf("сегодня, {hm}", &[("hm", &hm.to_string())])
    } else if Some(day) == today.pred_opt() {
        i18n::trf("вчера, {hm}", &[("hm", &hm.to_string())])
    } else if day.year() == today.year() {
        d.format("%d.%m, %H:%M").to_string()
    } else {
        d.format("%d.%m.%Y").to_string()
    }
}

fn first_line(s: &str) -> &str {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or("")
}

fn clamp_rect(r: Rect, wa: (i32, i32, i32, i32)) -> Rect {
    let w = r.w.min(wa.2.max(1) as u32).max(1);
    let h = r.h.min(wa.3.max(1) as u32).max(1);
    let x = r.x.min(wa.0 + wa.2 - w as i32).max(wa.0);
    let y = r.y.min(wa.1 + wa.3 - h as i32).max(wa.1);
    Rect { x, y, w, h }
}

/// Maps the text of a Slint key event to a global-hotkey key name.
fn key_name(text: &str) -> Option<String> {
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    // Russian layout letters map to the physical QWERTY keys.
    const RU: &str = "йцукенгшщзхъфывапролджэячсмитьбюё";
    const EN: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,.`";
    let lc = c.to_lowercase().next().unwrap_or(c);
    let c = RU.chars().position(|r| r == lc).and_then(|i| EN.chars().nth(i)).unwrap_or(c);
    let named = match c as u32 {
        0x7f => Some("Delete"),
        0x08 => Some("Backspace"),
        0x09 => Some("Tab"),
        0x0a | 0x0d => Some("Enter"),
        0x20 => Some("Space"),
        0xF700 => Some("Up"),
        0xF701 => Some("Down"),
        0xF702 => Some("Left"),
        0xF703 => Some("Right"),
        0xF727 => Some("Insert"),
        0xF729 => Some("Home"),
        0xF72B => Some("End"),
        0xF72C => Some("PageUp"),
        0xF72D => Some("PageDown"),
        _ => None,
    };
    if let Some(n) = named {
        return Some(n.to_string());
    }
    if (0xF704..=0xF71B).contains(&(c as u32)) {
        return Some(format!("F{}", c as u32 - 0xF703));
    }
    if c.is_ascii_alphanumeric() {
        return Some(c.to_ascii_uppercase().to_string());
    }
    if "`-=,./;'[]".contains(c) {
        return Some(c.to_string());
    }
    None
}

/// "Alt+Q" → ["Alt", "Q"]; keypad keys read "Num 5".
fn key_caps(accel: &str) -> ModelRc<SharedString> {
    let caps: Vec<SharedString> = accel
        .split('+')
        .filter(|p| !p.is_empty())
        .map(|p| {
            if let Some(d) = p.strip_prefix("Numpad") {
                format!("Num {d}").into()
            } else if p == "Super" {
                "Win".into()
            } else if p == "Space" && !i18n::is_en() {
                "Пробел".into()
            } else {
                p.into()
            }
        })
        .collect();
    ModelRc::new(VecModel::from(caps))
}

/// The buttons under the answer with their pictures: the built-in ones keep theirs (also after a
/// language switch), the user's own get a sparkle.
fn action_buttons(list: &[QuickAction]) -> Vec<ActionBtn> {
    let defaults = config::default_actions();
    let known = ["Объясни", "Короче", "Переведи", "Explain", "Shorter", "Translate"];
    list.iter()
        .map(|a| {
            let kind = defaults
                .iter()
                .position(|d| d.prompt == a.prompt)
                .or_else(|| known.iter().position(|k| *k == a.label).map(|i| i % 3))
                .map(|i| i as i32)
                .unwrap_or(3);
            ActionBtn { label: a.label.as_str().into(), kind }
        })
        .collect()
}

/// What a hotkey slot does, as the settings call it.
fn slot_label(slot: usize, smart: bool) -> String {
    i18n::tr(match slot {
        HK_SMART => "Выделенное или экран",
        HK_ASK => "Выделенный текст",
        HK_SHOT => "Скриншот",
        HK_CHAT if smart => "Открыть чат",
        HK_CHAT => "Чат",
        HK_VOICE => "Голос — зажми",
        HK_NOTE => "Спросить с пометкой",
        _ => "Повторить запрос",
    })
}

fn is_f_key(k: &str) -> bool {
    k.len() > 1 && k.starts_with('F') && k[1..].chars().all(|c| c.is_ascii_digit())
}

/// A single key without Ctrl/Alt/Shift/Win that also types something (a letter, digit, space...).
fn types_text(accel: &str) -> bool {
    !accel.contains('+') && !is_f_key(accel) && !accel.starts_with("Numpad")
}

impl App {
    fn ui(&self) -> MainWindow {
        self.ui.upgrade().expect("main window is alive")
    }

    fn hwnd(&self) -> Option<isize> {
        let ui = self.ui.upgrade()?;
        hwnd_of(ui.window())
    }

    fn window_rect(&self) -> Rect {
        let (x, y, w, h) = self.hwnd().and_then(native::window_rect).unwrap_or((0, 0, 0, 0));
        Rect { x, y, w, h }
    }

    fn apply_rect(&self, r: Rect) {
        if let Some(h) = self.hwnd() {
            native::set_window_rect(h, r.x, r.y, r.w, r.h);
        }
    }

    // ---------- window ----------

    /// Shows the regular, focus-taking window.
    fn show_window(&mut self) {
        self.present(false);
    }

    /// Shows the compact "on top" window: it never takes focus and lets clicks through.
    fn show_overlay(&mut self) {
        self.present(true);
    }

    fn present(&mut self, overlay: bool) {
        if overlay {
            // the compact window shows the answer only: no settings or history behind it
            if self.ui().get_settings_open() {
                self.close_settings();
            }
            self.hide_hist();
        }
        let ui = self.ui();
        self.shown_at = Instant::now();
        self.had_focus = false;
        self.follow_windows_theme();
        let mode_changed = self.overlay_view != overlay;
        self.overlay_view = overlay;
        ui.set_overlay_view(overlay);
        if !self.visible {
            // fade in: start from transparent, switch on a moment later so the change animates
            ui.set_appear_on(false);
            let weak = self.ui.clone();
            Timer::single_shot(Duration::from_millis(20), move || {
                if let Some(ui) = weak.upgrade() {
                    ui.set_appear_on(true);
                }
            });
        }
        let _ = ui.show();
        if !self.tick_timer.running() {
            self.tick_timer.start(TimerMode::Repeated, Duration::from_millis(120), || with_app(|a| a.tick()));
        }
        self.apply_window_mode(overlay);
        self.apply_alpha();
        if !self.visible {
            self.visible = true;
            if self.placed {
                self.place_window();
            } else {
                // Right after the first show() Slint still assumes a scale factor of 1.0 and only
                // learns the real one once the event loop has run, so physical sizes set now would
                // be rescaled. Keep the window invisible and place it a moment later.
                self.placed = true;
                ui.set_win_opacity(0.0);
                Timer::single_shot(Duration::from_millis(80), || with_app(|a| a.place_window()));
            }
        } else if mode_changed || overlay {
            self.place_window();
        }
        if !overlay && self.news_pending && self.cfg.seen_version != update::VERSION {
            self.cfg.seen_version = update::VERSION.into();
            self.save_now();
        }
        if overlay {
            self.auto_hide = Some(Instant::now() + OVERLAY_TIMEOUT);
            ui.set_ov_left(1.0);
        } else {
            self.auto_hide = None;
            self.force_focus();
            for ms in [40u64, 250, 600] {
                Timer::single_shot(Duration::from_millis(ms), || with_app(|a| a.force_focus_if_lost()));
            }
        }
    }

    /// Overlay: no activation, mouse passes through. Regular: normal interactive window.
    fn apply_window_mode(&self, overlay: bool) {
        let ui = self.ui();
        if let Some(h) = self.hwnd() {
            native::set_no_activate(h, overlay);
            native::block_menu_mode(h);
        }
        ui.window().with_winit_window(|w| {
            let _ = w.set_cursor_hittest(!overlay);
        });
    }

    fn place_window(&mut self) {
        let ui = self.ui();
        if self.overlay_view {
            ui.set_ov_font(self.cfg.overlay_font() as f32);
            self.fit_overlay();
            ui.set_win_opacity(1.0);
            return;
        }
        let wa = native::work_area(self.hwnd());
        let r = clamp_rect(self.cfg.bounds.unwrap_or_else(|| self.default_rect()), wa);
        self.apply_rect(r);
        ui.set_win_opacity(1.0);
    }

    /// Where the regular window opens when it has never been moved: bottom right corner.
    fn default_rect(&self) -> Rect {
        let scale = self.ui().window().scale_factor();
        let wa = native::work_area(self.hwnd());
        let (lw, lh) = normal_size();
        let (w, h) = ((lw * scale) as u32, (lh * scale) as u32);
        let m = (16.0 * scale) as i32;
        Rect { x: wa.0 + wa.2 - w as i32 - m, y: wa.1 + wa.3 - h as i32 - m, w, h }
    }

    /// The overlay opens at the top left corner of the regular chat window (wherever the user
    /// left it) and is exactly as big as its text: it grows right and down while the answer streams in.
    fn fit_overlay(&self) {
        if !self.overlay_view || !self.visible {
            return;
        }
        let ui = self.ui();
        let scale = ui.window().scale_factor();
        let anchor = self.cfg.bounds.unwrap_or_else(|| self.default_rect());
        let wa = native::work_area_at(anchor.x, anchor.y);
        let (w, h) = ((ui.get_ov_want_w() * scale).ceil() as u32, (ui.get_ov_want_h() * scale).ceil() as u32);
        let r = clamp_rect(Rect { x: anchor.x, y: anchor.y, w, h }, wa);
        if self.window_rect() != r {
            self.apply_rect(r);
        }
    }

    fn apply_tray(&self) {
        if let Some(t) = &self.tray {
            if let Err(e) = t.set_visible(!self.cfg.hide_tray) {
                log_err(&format!("tray: {e}"));
            }
        }
    }

    /// The opacity setting is applied to the window as a whole (see native::set_window_alpha).
    fn apply_alpha(&self) {
        if let Some(h) = self.hwnd() {
            native::set_window_alpha(h, self.cfg.opacity());
        }
    }

    /// Light or dark from the setting, or from Windows ("Как в Windows"); applied to every window.
    fn apply_theme(&mut self) {
        let dark = self.cfg.dark();
        self.dark = dark;
        let ui = self.ui();
        ui.global::<Theme>().set_dark(dark);
        ui.invoke_sync_palette();
        if let Some(p) = &self.pill {
            p.global::<Theme>().set_dark(dark);
        }
        if let Some(w) = &self.picker {
            w.global::<Theme>().set_dark(dark);
        }
    }

    /// "Как в Windows": picks up a change of the Windows colour mode.
    fn follow_windows_theme(&mut self) {
        if self.cfg.theme_mode() == "system" && native::windows_dark() != self.dark {
            self.apply_theme();
        }
    }

    fn set_opacity(&mut self, pct: f32) {
        let v = (pct.round() / 100.0).clamp(config::MIN_OPACITY, 1.0);
        if self.cfg.opacity == Some(v) {
            return;
        }
        self.cfg.opacity = Some(v);
        self.apply_alpha();
        self.save_soon();
    }

    /// Saves the settings once the user stops dragging a slider or typing, not on every step.
    fn save_soon(&mut self) {
        self.save_gen += 1;
        let gen = self.save_gen;
        Timer::single_shot(Duration::from_millis(400), move || {
            with_app(|a| {
                if a.save_gen == gen {
                    a.save_now();
                }
            })
        });
    }

    fn save_now(&mut self) {
        if !config::save(&self.store, &self.cfg) {
            self.ui().set_settings_note(i18n::tr("Не удалось сохранить настройки. Проверь доступ к папке данных.").into());
        }
    }

    /// A colour from the palette: becomes the accent, and the HEX field shows it.
    fn custom_color_picked(&mut self, c: slint::Color) {
        let rgb = (c.red(), c.green(), c.blue());
        self.ui().set_custom_text(config::hex_color(rgb).into());
        self.use_custom(rgb);
    }

    /// Typed into the HEX/RGB field: applied as soon as it reads as a colour.
    fn custom_text_edited(&mut self, text: &str) {
        let Some(rgb) = config::parse_color(text) else { return };
        let ui = self.ui();
        let (h, s, v) = rgb_to_hsv(rgb);
        if s > 0.0 && v > 0.0 {
            ui.set_custom_h(h); // grey has no hue: the strip keeps its place
        }
        if v > 0.0 {
            ui.set_custom_s(s);
        }
        ui.set_custom_v(v);
        self.use_custom(rgb);
    }

    fn use_custom(&mut self, rgb: (u8, u8, u8)) {
        self.cfg.accent_custom = config::hex_color(rgb);
        self.cfg.accent = "custom".into();
        let ui = self.ui();
        set_custom_theme(&ui.global::<Theme>(), rgb);
        ui.global::<Theme>().set_accent_name("custom".into());
        self.save_soon();
    }

    fn force_focus(&self) {
        #[cfg(debug_assertions)]
        if std::env::var("NORTEX_DEMO_OUT").is_ok() {
            return; // screenshot runs must not take the keyboard from the user
        }
        if let Some(h) = self.hwnd() {
            native::focus_window(h);
        }
    }

    fn force_focus_if_lost(&self) {
        if self.visible && !self.overlay_view && self.hwnd().is_some_and(|h| native::foreground() != h) {
            self.force_focus();
        }
    }

    fn is_foreground(&self) -> bool {
        self.visible && !self.overlay_view && self.hwnd().is_some_and(|h| native::foreground() == h)
    }

    fn hide_window(&mut self) {
        if self.ui().get_settings_open() {
            self.close_settings();
        }
        if self.voice.as_ref().is_some_and(|v| !v.popup) {
            self.voice_cancel();
        }
        self.hide_hist();
        self.ui().global::<Tip>().set_text("".into());
        let _ = self.ui().hide();
        self.visible = false;
        self.overlay_view = false;
        self.auto_hide = None;
        self.watched = None;
        self.last_hide = Instant::now();
        self.tick_timer.stop();
        Self::trim_memory_soon();
    }

    /// While the app only waits for a hotkey, hand its memory back to Windows.
    fn trim_memory_soon() {
        Timer::single_shot(Duration::from_secs(3), || {
            with_app(|a| {
                if !a.visible && a.voice.is_none() {
                    native::trim_memory();
                }
            })
        });
    }

    fn drag_start(&mut self, resize: bool) {
        self.drag = Some(DragState { resize, cursor: native::cursor_pos(), rect: self.window_rect() });
    }

    fn drag_move(&mut self) {
        let Some(d) = &self.drag else { return };
        let (cx, cy) = native::cursor_pos();
        let (dx, dy) = (cx - d.cursor.0, cy - d.cursor.1);
        let r = d.rect;
        let next = if d.resize {
            let scale = self.ui().window().scale_factor();
            let (min_w, min_h) = ((self.ui().get_min_w() * scale) as i64, (self.ui().get_min_h() * scale) as i64);
            Rect { w: (r.w as i64 + dx as i64).max(min_w) as u32, h: (r.h as i64 + dy as i64).max(min_h) as u32, ..r }
        } else {
            Rect { x: r.x + dx, y: r.y + dy, ..r }
        };
        self.apply_rect(next);
    }

    /// Called from a 120 ms timer: hides on focus loss and persists the window geometry.
    fn tick(&mut self) {
        if !self.visible {
            return;
        }
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks % 16 == 0 {
            self.follow_windows_theme();
        }
        if self.overlay_view {
            self.fit_overlay();
            // the overlay never has focus, so it leaves on a timer (or when the hotkey is pressed again)
            if let Some(t) = self.auto_hide {
                let left = t.saturating_duration_since(Instant::now()).as_secs_f32() / OVERLAY_TIMEOUT.as_secs_f32();
                self.ui().set_ov_left(left);
                if Instant::now() >= t {
                    self.hide_window();
                }
            }
            return;
        }
        let Some(h) = self.hwnd() else { return };
        if native::foreground() == h {
            self.had_focus = true;
        }
        // hides when the user goes to another window; if Windows never gave the window the focus
        // (it may refuse), it stays instead of vanishing right after it appeared
        if !self.pinned
            && self.had_focus
            && !self.dialog_open.load(Ordering::SeqCst)
            && !self.ui().get_uninstall_open()
            && self.voice.is_none()
            && self.shown_at.elapsed() > BLUR_GRACE
            && native::foreground() != h
        {
            self.hide_window();
            return;
        }
        if self.pre_settings.is_some() {
            return; // the taller settings size is not remembered
        }
        let now = self.window_rect();
        if now.w == 0 || now.h == 0 {
            return;
        }
        match &self.watched {
            Some((r, since)) if *r == now => {
                if since.elapsed() > Duration::from_millis(400) && self.cfg.bounds != Some(now) {
                    self.cfg.bounds = Some(now);
                    self.save_now();
                }
            }
            _ => self.watched = Some((now, Instant::now())),
        }
    }

    // ---------- the dialog ----------

    /// Short notice at the bottom of the window that goes away by itself.
    fn toast(&mut self, text: &str) {
        self.toast_gen += 1;
        let gen = self.toast_gen;
        self.ui().set_toast_text(text.into());
        Timer::single_shot(TOAST_TIME, move || {
            with_app(|a| {
                if a.toast_gen == gen {
                    a.ui().set_toast_text("".into());
                }
            })
        });
    }

    fn feed_item(&self, t: &Answer, accent: &str) -> FeedItem {
        let text = md::without_mark(&t.text);
        let blocks: Vec<BlockItem> = if t.is_error {
            vec![BlockItem { kind: 0, text: text.as_str().into(), rich: slint::StyledText::from_plain_text(&text), label: "".into() }]
        } else {
            md::parse(&text)
                .into_iter()
                .map(|b| {
                    let plain = !matches!(b.kind, md::Kind::Code | md::Kind::Formula);
                    let rich = if plain {
                        slint::StyledText::from_markdown(&md::inline_markdown(&b.text, accent)).unwrap_or_else(|_| slint::StyledText::from_plain_text(&md::plain_inline(&b.text)))
                    } else {
                        slint::StyledText::default()
                    };
                    let txt = if b.kind == md::Kind::Heading { md::plain_inline(&b.text) } else { b.text };
                    BlockItem { kind: b.kind as i32, text: txt.into(), rich, label: b.label.into() }
                })
                .collect()
        };
        let action_text = match t.action.as_str() {
            "gemini-key" => i18n::tr("Добавить ключ Gemini"),
            "mic" => i18n::tr("Открыть настройки микрофона"),
            "settings" => i18n::tr("Открыть настройки"),
            _ => String::new(),
        };
        FeedItem {
            q: question_line(&t.question).into(),
            shots: t.shots as i32,
            files: t.files.as_str().into(),
            blocks: ModelRc::new(VecModel::from(blocks)),
            note: t.note.as_str().into(),
            error: t.is_error,
            action: t.action.as_str().into(),
            action_text: action_text.into(),
            open: t.open,
            preview: first_line(&md::plain(&text)).into(),
            waiting: t.waiting,
        }
    }

    /// The accent as "#rrggbb" for the inline Markdown of the answers (links, code).
    fn accent_hex(&self) -> String {
        let c = self.ui().global::<Theme>().get_accent_text();
        format!("#{:02x}{:02x}{:02x}", c.red(), c.green(), c.blue())
    }

    /// Rebuilds the whole dialog on screen.
    fn render(&mut self) {
        let accent = self.accent_hex();
        let items: Vec<FeedItem> = self.turns.iter().map(|t| self.feed_item(t, &accent)).collect();
        self.feed.set_vec(items);
        self.after_feed_change();
    }

    /// Redraws only the newest answer (while it streams in).
    fn render_last(&mut self) {
        let Some(t) = self.turns.last() else { return self.render() };
        let accent = self.accent_hex();
        let item = self.feed_item(t, &accent);
        let n = self.feed.row_count();
        if n == self.turns.len() && n > 0 {
            self.feed.set_row_data(n - 1, item);
        } else {
            return self.render();
        }
        self.after_feed_change();
    }

    fn after_feed_change(&mut self) {
        let ui = self.ui();
        ui.set_status_text(self.status.as_str().into());
        ui.set_text_scale(self.cfg.text_scale() as f32 / 100.0);
        ui.set_select_mode(self.select_mode);
        let last = self.turns.last();
        if self.select_mode {
            ui.set_select_text(last.map(|t| md::plain(&t.text)).unwrap_or_default().into());
        }
        // the "on top" window shows the newest answer as plain text
        let body = match last {
            Some(t) if !t.waiting => if t.is_error { t.text.clone() } else { md::plain(&t.text) },
            _ => self.status.clone(),
        };
        ui.set_body_text(body.into());
        ui.set_thinking(last.is_some_and(|t| t.waiting));
        self.fit_overlay();
    }

    fn set_status(&mut self, text: &str) {
        self.status = text.to_string();
        self.ui().set_status_text(text.into());
        self.after_feed_change();
    }

    fn show_status(&mut self, text: &str) {
        self.hide_hist();
        self.set_status(text);
    }

    fn toggle_turn(&mut self, i: usize) {
        if let Some(t) = self.turns.get_mut(i) {
            t.open = !t.open;
            self.render();
        }
    }

    fn toggle_language(&mut self) {
        let en = !self.cfg.is_en();
        self.cfg.lang = if en { "en".into() } else { "ru".into() };
        self.apply_language();
        self.save_now();
    }

    fn apply_language(&mut self) {
        let en = self.cfg.is_en();
        i18n::set_en(en);
        let _ = slint::select_bundled_translation(if en { "en-US" } else { "" });
        let ui = self.ui();
        ui.set_lang_en(en);
        TRAY_ITEMS.with(|t| {
            if let Some((open, settings, quit)) = t.borrow().as_ref() {
                open.set_text(i18n::tr("Открыть"));
                settings.set_text(i18n::tr("Настройки"));
                quit.set_text(i18n::tr("Выход"));
            }
        });
        self.refresh_keys_ui();
        self.refresh_actions();
        self.fill_news();
        if ui.get_settings_open() {
            self.fill_settings();
        }
        if self.hist_open {
            self.refresh_history();
        }
        self.update_placeholder();
    }

    fn toggle_select(&mut self) {
        self.select_mode = !self.select_mode;
        self.after_feed_change();
    }

    fn copy_answer(&mut self) {
        let Some(t) = self.turns.last() else { return };
        clip::set_text(&md::plain(&t.text));
        let ui = self.ui();
        ui.set_copy_done(true);
        let weak = self.ui.clone();
        Timer::single_shot(Duration::from_millis(1200), move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_copy_done(false);
            }
        });
    }

    // ---------- uninstall ----------

    /// Opens the confirmation: the "Удалить" button becomes available after five seconds.
    fn uninstall_ask(&mut self) {
        if !self.visible || self.overlay_view {
            self.show_window();
        }
        self.uninstall_left = 5;
        let ui = self.ui();
        ui.set_uninstall_left(5);
        ui.set_uninstall_open(true);
        self.uninstall_timer.start(TimerMode::Repeated, Duration::from_secs(1), || {
            with_app(|a| {
                a.uninstall_left = a.uninstall_left.saturating_sub(1);
                a.ui().set_uninstall_left(a.uninstall_left as i32);
                if a.uninstall_left == 0 {
                    a.uninstall_timer.stop();
                }
            })
        });
    }

    fn uninstall_cancel(&mut self) {
        self.uninstall_timer.stop();
        self.uninstall_left = 0;
        let ui = self.ui();
        ui.set_uninstall_open(false);
        ui.set_uninstall_left(0);
    }

    fn uninstall_confirmed(&mut self) {
        if self.uninstall_left != 0 || !self.ui().get_uninstall_open() {
            return;
        }
        self.uninstall_now();
    }

    /// Removes the autostart entry and, after this process has quit, the program and its data.
    /// Alt+Shift+Delete calls it directly, without asking (kept on purpose).
    fn uninstall_now(&mut self) {
        let _ = native::set_autostart(false);
        let exe = std::env::current_exe().unwrap_or_default();
        let data = config::data_dir();
        let wait = "ping -n 3 127.0.0.1 >nul";
        let script = if update::is_installed() {
            let unins = exe.parent().map(|d| d.join("unins000.exe")).unwrap_or_default();
            format!("{wait} & \"{}\" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART & rmdir /s /q \"{}\"", unins.display(), data.display())
        } else {
            format!("{wait} & del /f /q \"{}\" & rmdir /s /q \"{}\"", exe.display(), data.display())
        };
        native::run_detached(&script);
        let _ = slint::quit_event_loop();
    }

    fn set_text_scale_index(&mut self, i: usize) {
        let scales = config::TEXT_SCALES;
        let v = scales[i.min(scales.len() - 1)];
        if self.cfg.text_scale == Some(v) {
            return;
        }
        self.cfg.text_scale = Some(v);
        let ui = self.ui();
        ui.set_text_scale(v as f32 / 100.0);
        ui.set_text_scale_index(i.min(scales.len() - 1) as i32);
        self.save_soon();
    }

    // ---------- paperclip ----------

    fn refresh_files(&self) {
        let items: Vec<FileItem> = self
            .attached
            .iter()
            .map(|(label, folder, files)| FileItem {
                name: label.as_str().into(),
                info: if *folder { format!("· {}", files_word(files.len())) } else { String::new() }.into(),
                folder: *folder,
            })
            .collect();
        self.ui().set_files(ModelRc::new(VecModel::from(items)));
    }

    fn all_files(&self) -> Vec<llm::Attachment> {
        self.attached.iter().flat_map(|(_, _, f)| f.iter().cloned()).collect()
    }

    fn take_files(&mut self) -> Vec<llm::Attachment> {
        let f = self.all_files();
        self.attached.clear();
        self.refresh_files();
        f
    }

    fn remove_file(&mut self, i: usize) {
        if i < self.attached.len() {
            self.attached.remove(i);
            self.refresh_files();
        }
    }

    /// Opens the system dialog on a helper thread (it blocks) and attaches what was picked.
    fn attach_pick(&mut self, folder: bool) {
        if self.dialog_open.swap(true, Ordering::SeqCst) {
            return;
        }
        let owner = self.hwnd().unwrap_or(0);
        let flag = self.dialog_open.clone();
        std::thread::spawn(move || {
            let picked = native::pick_paths(folder, owner);
            let _ = slint::invoke_from_event_loop(move || {
                flag.store(false, Ordering::SeqCst);
                with_app(|a| a.files_picked(picked));
            });
        });
    }

    fn files_dropped(&mut self, path: std::path::PathBuf) {
        if self.composer.is_none() || self.ui().get_settings_open() {
            return;
        }
        self.files_picked(vec![path]);
    }

    fn files_picked(&mut self, paths: Vec<std::path::PathBuf>) {
        if let Some(h) = self.hwnd() {
            native::focus_window(h);
        }
        self.ui().invoke_focus_input();
        if paths.is_empty() {
            return;
        }
        let already = self.all_files();
        let loaded = attach::load(&paths, &already);
        for g in loaded.groups {
            self.attached.push((g.label, g.folder, g.files));
        }
        self.refresh_files();
        if !loaded.skipped.is_empty() {
            let note = i18n::trf("Пропущено: {list}", &[("list", &loaded.skipped.join("; "))]);
            self.toast(&note);
        }
    }

    fn set_busy(&mut self, on: bool) {
        self.busy = on;
        let ui = self.ui();
        ui.set_busy(on);
        // the send / stop button changes under the pointer: its old tooltip must not stay
        ui.global::<Tip>().set_text("".into());
    }

    fn focus_input_soon(&self) {
        for ms in [0u64, 60, 300] {
            let weak = self.ui.clone();
            Timer::single_shot(Duration::from_millis(ms), move || {
                if let Some(ui) = weak.upgrade() {
                    ui.invoke_focus_input();
                }
            });
        }
    }

    fn update_placeholder(&self) {
        let ui = self.ui();
        let p = match &self.composer {
            Some(Composer::Selection(_)) => i18n::tr("Что сделать? Enter — спросить"),
            Some(Composer::Shot) => i18n::tr("Что сделать? Enter — решить"),
            _ if self.turns.is_empty() => i18n::tr("Задай вопрос"),
            _ => i18n::tr("Уточни ответ"),
        };
        ui.set_composer_placeholder(p.into());
    }

    /// Opens the input field in a mode; `keep` leaves pictures and files that are already attached.
    fn show_composer(&mut self, status: &str, comp: Composer, keep: bool) {
        if self.ui().get_settings_open() {
            self.close_settings();
        }
        self.hide_hist();
        let ui = self.ui();
        if !keep {
            ui.set_composer_text("".into());
            self.pending.clear();
            self.refresh_thumbs();
            self.attached.clear();
            self.refresh_files();
        }
        self.composer = Some(comp);
        ui.set_composer_on(true);
        self.update_placeholder();
        self.set_status(status);
        self.focus_input_soon();
    }

    fn refresh_thumbs(&self) {
        let items: Vec<ThumbItem> = self.pending.iter().map(|(_, img)| ThumbItem { img: img.clone() }).collect();
        self.ui().set_thumbs(ModelRc::new(VecModel::from(items)));
    }

    /// How many pictures one question may carry: Groq takes three, Gemini four.
    fn max_shots(&self) -> usize {
        if self.cfg.is_groq() { 3 } else { MAX_SHOTS }
    }

    fn take_pending(&mut self) -> Vec<String> {
        let imgs = self.pending.drain(..).map(|(b, _)| b).collect();
        self.refresh_thumbs();
        imgs
    }

    /// A fresh dialog: the old one stays in the history.
    fn new_chat(&mut self) {
        self.chat.clear();
        self.turns.clear();
        self.select_mode = false;
        self.status.clear();
        self.render();
    }

    // ---------- asking ----------

    fn submit(&mut self, extra: String) {
        if self.busy || self.voice.is_some() {
            return;
        }
        let extra = extra.trim().to_string();
        let Some(comp) = self.composer.clone() else { return };
        match comp {
            Composer::Free | Composer::FollowUp => {
                if extra.is_empty() && self.pending.is_empty() && self.attached.is_empty() {
                    return;
                }
                self.ask_in_chat(extra, None, false);
            }
            Composer::Selection(text) => {
                let imgs = self.take_pending();
                let files = self.take_files();
                self.new_chat();
                if imgs.is_empty() {
                    let q = if extra.is_empty() { text.clone() } else { format!("{text} — {extra}") };
                    self.send(Request { text: text.clone(), extra, files, ..Default::default() }, Some(text), q, false);
                } else {
                    let combined = if extra.is_empty() { text.clone() } else { format!("{text}\n{extra}") };
                    self.send(Request { extra: combined, images: imgs, files, ..Default::default() }, Some(text.clone()), text, false);
                }
            }
            Composer::Shot => {
                if self.pending.is_empty() {
                    // every snapshot was removed: what is left is an ordinary question
                    if !extra.is_empty() || !self.attached.is_empty() {
                        self.new_chat();
                        self.ask_in_chat(extra, None, false);
                    }
                    return;
                }
                let all = self.take_pending();
                let files = self.take_files();
                self.new_chat();
                let q = if extra.is_empty() { i18n::tr("[скриншот]") } else { extra.clone() };
                let shown = if extra.is_empty() { i18n::tr("Снимок экрана") } else { extra.clone() };
                self.send(Request { extra, images: all, files, ..Default::default() }, Some(q), shown, false);
            }
        }
        self.ui().set_composer_text("".into());
    }

    /// A question in the current dialog (typed, or a button under the answer). `shown` replaces
    /// the question in the chip (the button's caption instead of its long request).
    fn ask_in_chat(&mut self, q: String, shown: Option<String>, voice: bool) {
        let imgs = self.take_pending();
        let files = self.take_files();
        let history = self.chat.clone();
        if imgs.is_empty() {
            let rq = if q.is_empty() { i18n::tr("[файлы]") } else { q.clone() };
            let show = shown.unwrap_or_else(|| rq.clone());
            self.send(Request { text: q, history, files, voice, ..Default::default() }, Some(rq), show, false);
        } else {
            let rq = if q.is_empty() { i18n::tr("[скриншот]") } else { q.clone() };
            let show = shown.unwrap_or_else(|| if q.is_empty() { i18n::tr("Снимок экрана") } else { q.clone() });
            self.send(Request { extra: q, images: imgs, history, files, voice, ..Default::default() }, Some(rq), show, false);
        }
    }

    fn quick_action(&mut self, i: usize) {
        if self.busy {
            return;
        }
        let Some(a) = self.cfg.actions().get(i).cloned() else { return };
        if a.prompt.trim().is_empty() {
            return;
        }
        self.ask_in_chat(a.prompt.trim().to_string(), Some(a.label.clone()), false);
        if let Some(t) = self.turns.last_mut() {
            t.quick = true;
        }
    }

    /// Sends a request; the answer is added to the dialog (or replaces the newest one: "another answer").
    fn send(&mut self, req: Request, record_q: Option<String>, question: String, replace_last: bool) {
        self.req_id += 1;
        let id = self.req_id;
        self.status.clear();
        self.select_mode = false;
        self.last_activity = Instant::now();
        // older answers fold to one line, the new one is open
        for t in self.turns.iter_mut() {
            t.open = false;
        }
        let turn = Answer {
            text: String::new(),
            req: None,
            is_error: false,
            record_q: record_q.clone(),
            question,
            shots: req.images.len(),
            note: String::new(),
            action: String::new(),
            open: true,
            waiting: true,
            ..Default::default()
        };
        let mut turn = turn;
        turn.files = files_line(&req.files);
        if replace_last && !self.turns.is_empty() {
            let old = self.turns.last().unwrap();
            turn.quick = old.quick;
            turn.hist_id = old.hist_id;
            *self.turns.last_mut().unwrap() = turn;
        } else {
            self.turns.push(turn);
        }
        if self.turns.len() > 30 {
            self.turns.remove(0);
        }
        // the field stays: the next message is a follow-up to this answer
        if !self.overlay_view {
            self.composer = Some(Composer::FollowUp);
            self.ui().set_composer_on(true);
            self.update_placeholder();
        }
        self.render();
        self.live_req.store(id, Ordering::SeqCst);
        self.in_flight = Some((req.clone(), record_q.clone()));
        self.set_busy(true);
        let cfg = self.cfg.clone();
        let live = self.live_req.clone();
        std::thread::spawn(move || {
            let on_chunk = |t: &str| {
                let t = t.to_string();
                let _ = slint::invoke_from_event_loop(move || with_app(|a| a.on_chunk(id, &t)));
            };
            let result = llm::ask(&cfg, &req, &on_chunk, &|| live.load(Ordering::SeqCst) != id);
            let _ = slint::invoke_from_event_loop(move || with_app(|a| a.on_done(id, req, result, record_q)));
        });
    }

    fn on_chunk(&mut self, id: u64, text: &str) {
        if id != self.req_id {
            return;
        }
        if let Some(t) = self.turns.last_mut() {
            t.text = text.to_string();
            t.waiting = false;
        }
        self.render_last();
    }

    /// Stop: drop the running request, keep whatever part of the answer already arrived.
    fn stop(&mut self) {
        if !self.busy {
            return;
        }
        self.req_id += 1; // late chunks and the final result of the old request are ignored
        self.live_req.store(0, Ordering::SeqCst);
        self.set_busy(false);
        let Some((req, record_q)) = self.in_flight.take() else { return self.render() };
        if let Some(t) = self.turns.last_mut() {
            let partial = t.text.trim().to_string();
            t.text = if partial.is_empty() { i18n::tr("Запрос остановлен.") } else { format!("{partial}{}", i18n::tr("\n\n[остановлено]")) };
            // marked as an error so that "again" simply resends it instead of treating it as a rejected answer
            t.is_error = true;
            t.waiting = false;
            t.req = Some(req);
            t.record_q = record_q;
        }
        self.render();
    }

    fn on_done(&mut self, id: u64, req: Request, result: Result<llm::Reply, String>, record_q: Option<String>) {
        if id != self.req_id {
            return;
        }
        self.in_flight = None;
        self.live_req.store(0, Ordering::SeqCst);
        self.set_busy(false);
        self.last_activity = Instant::now();
        let overlay = self.overlay_view;
        let retry_key = self.cfg.hotkeys().retry;
        let Some(t) = self.turns.last_mut() else { return };
        t.waiting = false;
        t.req = Some(req.clone());
        t.record_q = record_q.clone();
        match result {
            Ok(reply) => {
                // a button under the answer is saved as "Короче · <the dialog's question>"
                let title = if t.quick {
                    let first = self.chat.first().map(|c| c.text.clone()).unwrap_or_default();
                    format!("{} · {}", t.question, pretty_question(first_line(&first)))
                } else {
                    history::describe(&req)
                };
                t.hist_id = match t.hist_id {
                    Some(id) if self.history.replace(id, &title, &reply.text) => Some(id),
                    _ => self.history.add(self.cfg.history_on(), &title, &reply.text),
                };
                if let Some(q) = &record_q {
                    record_turn(&mut self.chat, q, &reply.text);
                }
                t.text = reply.text;
                t.is_error = false;
                t.note = match reply.fallback {
                    Some(llm::Fallback::Limit) => i18n::tr("Ответила Gemini — у Groq кончился лимит"),
                    Some(llm::Fallback::Busy) => i18n::tr("Ответила Gemini — Groq сейчас перегружен"),
                    Some(llm::Fallback::Images) => i18n::tr("Ответила Gemini — Groq берёт не больше трёх снимков"),
                    None => String::new(),
                };
                t.action.clear();
            }
            Err(e) => {
                let (e, action) = match e.strip_suffix(llm::NEED_GEMINI_KEY) {
                    Some(rest) => (rest.to_string(), "gemini-key"),
                    None if e.contains("API-ключ") || e.contains("API key") => (e.clone(), "settings"),
                    None => (e.clone(), ""),
                };
                let mut text = i18n::trf("Ошибка: {e}", &[("e", &e)]);
                if overlay {
                    // the overlay cannot be clicked, so point at the hotkey that resends the request
                    text.push_str(&i18n::trf("\n\n{key} — повторить запрос", &[("key", &retry_key)]));
                }
                t.text = text;
                t.is_error = true;
                t.note.clear();
                t.action = action.to_string();
            }
        }
        self.render();
    }

    /// "Другой вариант" / "Повторить": the newest question again; a real answer counts as rejected.
    fn retry(&mut self) {
        if self.busy {
            return;
        }
        let Some(t) = self.turns.last() else { return };
        let Some(mut req) = t.req.clone() else {
            // an answer from the history: ask its question again in this dialog
            let q = t.question.clone();
            let mut history = self.chat.clone();
            history.truncate(history.len().saturating_sub(2));
            let req = Request { text: q.clone(), history, rejected: vec![md::without_mark(&t.text)], ..Default::default() };
            return self.send(req, Some(q.clone()), q, true);
        };
        if !t.is_error {
            req.rejected.push(md::without_mark(&t.text));
        }
        let keep = req.rejected.len().saturating_sub(3);
        req.rejected.drain(..keep);
        let (record_q, question) = (t.record_q.clone(), t.question.clone());
        self.send(req, record_q, question, true);
    }

    /// Hotkey: send the last request again (after a limit error, or when the answer was wrong).
    fn retry_last(&mut self) {
        let has_request = self.turns.last().is_some_and(|a| a.req.is_some() || !a.text.is_empty());
        let overlay = self.cfg.overlay_mode && !(self.visible && !self.overlay_view);
        if !has_request {
            self.show_status(&i18n::tr("Нет запроса для повтора. Сначала задай вопрос или сделай снимок."));
        } else {
            self.retry();
        }
        if overlay {
            self.show_overlay();
        } else if !self.visible {
            self.show_window();
        }
    }

    /// Esc or the close button: stop a running request or recording, otherwise hide.
    fn close_pressed(&mut self) {
        if self.ui().get_uninstall_open() {
            self.uninstall_cancel();
            return;
        }
        if self.voice.is_some() {
            self.voice_cancel();
            return;
        }
        let was_busy = self.busy;
        self.stop();
        if was_busy && self.pinned {
            return;
        }
        if self.ui().get_settings_open() && self.pinned {
            self.close_settings();
            return;
        }
        self.hide_window();
    }

    fn tray_open(&mut self) {
        if self.visible && !self.overlay_view {
            self.hide_window();
        } else if !self.visible && self.last_hide.elapsed() < Duration::from_millis(600) {
            // the click that took focus from the window already hid it: treat this click as "close"
        } else {
            self.open_chat();
        }
    }

    fn tray_settings(&mut self) {
        self.open_chat();
        if !self.ui().get_settings_open() {
            self.open_settings();
        }
    }

    // ---------- history ----------

    fn hide_hist(&mut self) {
        self.hist_open = false;
        self.ui().set_history_open(false);
    }

    fn refresh_history(&mut self) {
        let items: Vec<HistItem> = self
            .history
            .list()
            .iter()
            .map(|e| HistItem { title: pretty_question(first_line(&e.q)).into(), sub: format!("{} · {}", when_text(e.t), first_line(&md::plain(&e.a))).into() })
            .collect();
        self.ui().set_history_items(ModelRc::new(VecModel::from(items)));
    }

    fn toggle_history(&mut self) {
        if self.hist_open {
            return self.hide_hist();
        }
        if self.ui().get_settings_open() {
            self.close_settings();
        }
        self.hist_open = true;
        self.refresh_history();
        self.ui().set_history_open(true);
    }

    /// Opens a saved answer together with its question as a new dialog, so it can be clarified.
    fn history_selected(&mut self, i: usize) {
        let Some(e) = self.history.list().get(i).cloned() else { return };
        self.stop();
        self.new_chat();
        let answer = md::without_mark(&e.a);
        self.chat = vec![Turn { model: false, text: e.q.clone() }, Turn { model: true, text: answer.clone() }];
        self.turns.push(Answer {
            text: answer,
            req: None,
            is_error: false,
            record_q: Some(e.q.clone()),
            question: pretty_question(&e.q),
            hist_id: Some(e.id),
            shots: 0,
            note: String::new(),
            action: String::new(),
            open: true,
            waiting: false,
            ..Default::default()
        });
        self.hide_hist();
        self.composer = Some(Composer::FollowUp);
        self.ui().set_composer_on(true);
        self.update_placeholder();
        self.render();
        self.focus_input_soon();
    }

    fn clear_history(&mut self) {
        self.history.clear();
        if self.hist_open {
            self.refresh_history();
        }
        self.toast(&i18n::tr("История очищена"));
    }

    // ---------- hotkeys ----------

    fn register_hotkeys(&mut self) {
        #[cfg(debug_assertions)]
        if std::env::var("NORTEX_NO_HOTKEYS").is_ok() {
            return;
        }
        for hk in self.registered.drain(..) {
            let _ = self.hot_mgr.unregister(hk);
        }
        self.hot_ids.clear();
        if self.capturing {
            return; // the settings are recording a new combination; registered again when that ends
        }
        self.hot_failed.clear();
        let keys = self.cfg.hotkeys();
        for i in Hotkeys::active(self.cfg.smart_keys()) {
            let accel = keys.get(i);
            match HotKey::from_str(accel).map_err(|e| e.to_string()).and_then(|hk| self.hot_mgr.register(hk).map(|_| hk).map_err(|e| e.to_string())) {
                Ok(hk) => {
                    self.hot_ids.insert(hk.id(), i);
                    self.registered.push(hk);
                }
                Err(e) => {
                    log_err(&format!("hotkey {accel}: {e}"));
                    self.hot_failed.push(i);
                }
            }
        }
        // removes the program at once, without asking (kept on purpose, see the release notes)
        if let Ok(hk) = HotKey::from_str("Alt+Shift+Delete") {
            if self.hot_mgr.register(hk).is_ok() {
                self.hot_ids.insert(hk.id(), SLOT_UNINSTALL);
                self.registered.push(hk);
            }
        }
        if self.visible || self.ui().get_settings_open() {
            self.refresh_keys_ui();
        }
    }

    fn on_hotkey(&mut self, slot: usize, pressed: bool) {
        if slot == HK_VOICE {
            return if pressed { self.voice_press() } else { self.voice_release() };
        }
        if !pressed {
            return;
        }
        if slot == SLOT_ESCAPE {
            return self.voice_cancel();
        }
        native::suppress_alt_menu();
        let smart = self.cfg.smart_keys();
        match slot {
            HK_SMART => self.smart_ask(),
            HK_ASK => self.grab_then(false, CLASSIC_COPY_WAIT),
            HK_NOTE => self.grab_then(true, SMART_COPY_WAIT),
            HK_SHOT => {
                self.shot_auto = false;
                self.start_screenshot();
            }
            HK_RETRY => self.retry_last(),
            SLOT_UNINSTALL => self.uninstall_now(),
            HK_CHAT if self.cfg.overlay_mode && self.visible && self.overlay_view => self.hide_window(),
            HK_CHAT if smart && self.visible && !self.overlay_view => self.hide_window(),
            HK_CHAT => self.open_chat(),
            _ => {}
        }
    }

    /// Copies the selection on a helper thread, then goes on with it.
    /// `note`: the smart mode's "ask with a note" (nothing selected → a screenshot with a note).
    fn grab_then(&mut self, note: bool, wait: Duration) {
        std::thread::spawn(move || {
            let text = clip::grab_selection(wait);
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|a| {
                    if note && text.is_empty() {
                        a.shot_auto = false;
                        a.start_screenshot();
                    } else {
                        a.mode_selection(text);
                    }
                })
            });
        });
    }

    /// "Одна клавиша на всё": selected text → asked at once; nothing selected → a screenshot,
    /// asked at once. Our own window in front: it is the screen that is meant.
    fn smart_ask(&mut self) {
        if self.is_foreground() {
            self.shot_auto = true;
            return self.start_screenshot();
        }
        std::thread::spawn(|| {
            let text = clip::grab_selection(SMART_COPY_WAIT);
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|a| {
                    if text.is_empty() {
                        a.shot_auto = true;
                        a.start_screenshot();
                    } else {
                        a.ask_selection_now(text);
                    }
                })
            });
        });
    }

    fn ask_selection_now(&mut self, text: String) {
        self.new_chat();
        self.send(Request { text: text.clone(), ..Default::default() }, Some(text.clone()), text, false);
        if self.cfg.overlay_mode {
            self.show_overlay();
        } else {
            self.show_window();
        }
    }

    /// Alt+E, the tray: the chat window. The last dialog comes back if it is recent.
    fn open_chat(&mut self) {
        let keep = self.cfg.smart_keys() && !self.turns.is_empty() && self.last_activity.elapsed() < CHAT_KEEP && !self.busy;
        if keep {
            self.status.clear();
            self.composer = Some(Composer::FollowUp);
            self.ui().set_composer_on(true);
            self.update_placeholder();
            self.render();
            self.focus_input_soon();
        } else if !self.busy {
            self.new_chat();
            self.show_composer("", Composer::Free, false);
        }
        self.show_window();
    }

    /// Selected text with the field for a note (the separate-keys Alt+Q, the smart Alt+Shift+Q).
    fn mode_selection(&mut self, text: String) {
        if self.cfg.overlay_mode {
            if text.is_empty() {
                self.new_chat();
                self.show_status(&i18n::tr("Не удалось скопировать выделение. Проверь, что приложение разрешает Ctrl+C."));
            } else {
                return self.ask_selection_now(text);
            }
            self.show_overlay();
            return;
        }
        if text.is_empty() {
            self.new_chat();
            self.show_composer(&i18n::tr("Не удалось скопировать текст. Проверь выделение и разрешение Ctrl+C."), Composer::Free, false);
        } else {
            self.new_chat();
            let preview: String = if text.chars().count() > 300 { text.chars().take(300).collect::<String>() + "…" } else { text.clone() };
            self.show_composer(&i18n::trf("Выделено: «{preview}»", &[("preview", &preview)]), Composer::Selection(text), false);
        }
        self.show_window();
    }

    /// "On top" screenshot: grab the whole monitor under the cursor without any UI of ours,
    /// ask the model right away and show the answer in the overlay.
    fn start_overlay_screenshot(&mut self) {
        let my = self.snip_token.fetch_add(1, Ordering::SeqCst) + 1;
        if self.visible {
            self.hide_window(); // keep our own window out of the picture
        }
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(140));
            let image = native::capture_cursor_monitor().and_then(|(w, h, px)| clip::encode_rgba(w, h, px));
            let _ = slint::invoke_from_event_loop(move || with_app(|a| a.finish_overlay_screenshot(image, my)));
        });
    }

    fn finish_overlay_screenshot(&mut self, image: Option<String>, my: u64) {
        if self.snip_token.load(Ordering::SeqCst) != my {
            return;
        }
        self.new_chat();
        match image {
            Some(b64) => self.send(Request { images: vec![b64], ..Default::default() }, Some(i18n::tr("[скриншот]")), i18n::tr("Снимок экрана"), false),
            None => self.show_status(&i18n::tr("Снимок не удался.")),
        }
        self.show_overlay();
    }

    fn start_screenshot(&mut self) {
        if self.cfg.overlay_mode {
            return self.start_overlay_screenshot();
        }
        if self.picker.is_some() {
            return; // the area picker is already on screen
        }
        let my = self.snip_token.fetch_add(1, Ordering::SeqCst) + 1;
        let was_visible = self.visible;
        if was_visible {
            self.hide_window();
        }
        let token = self.snip_token.clone();
        let mode = self.cfg.shot_mode_index();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            if mode == 0 {
                // freeze the screen now; the picker shows this picture while the user selects
                let cap = native::capture_cursor_monitor_rect();
                let _ = slint::invoke_from_event_loop(move || with_app(|a| a.open_picker(cap, my, was_visible)));
                return;
            }
            let raw = if mode == 1 { native::capture_cursor_monitor() } else { clip::snip(&token, my) };
            let image = raw.and_then(|(w, h, px)| clip::prepare_shot(w, h, px));
            let _ = slint::invoke_from_event_loop(move || with_app(|a| a.finish_screenshot(image, my, was_visible)));
        });
    }

    fn finish_screenshot(&mut self, image: Option<clip::Shot>, my: u64, was_visible: bool) {
        if self.snip_token.load(Ordering::SeqCst) != my {
            return;
        }
        match image {
            Some((b64, (w, h, rgba))) => {
                if self.shot_auto {
                    // smart Alt+Q: the picture is the question
                    self.shot_auto = false;
                    self.new_chat();
                    self.send(Request { images: vec![b64], ..Default::default() }, Some(i18n::tr("[скриншот]")), i18n::tr("Снимок экрана"), false);
                    self.show_window();
                    return;
                }
                if !matches!(self.composer, Some(Composer::Shot)) {
                    self.pending.clear();
                }
                while self.pending.len() >= self.max_shots() {
                    self.pending.remove(0); // the oldest snapshot makes room
                }
                self.pending.push((b64, make_image(w, h, &rgba)));
                self.refresh_thumbs();
                let n = self.pending.len();
                let label = if n == 1 { i18n::tr("Снимок экрана готов.") } else { i18n::trf("Снимков экрана: {n}.", &[("n", &n.to_string())]) };
                let more = if n < self.max_shots() && !self.cfg.smart_keys() {
                    i18n::trf(" {key} — добавить ещё снимок (до {max}).", &[("key", &self.cfg.hotkeys().screenshot), ("max", &self.max_shots().to_string())])
                } else {
                    String::new()
                };
                if !matches!(self.composer, Some(Composer::Shot)) {
                    self.new_chat();
                }
                self.show_composer(&format!("{label}{more}"), Composer::Shot, true);
                self.show_window();
            }
            None if self.cfg.shot_mode_index() != 2 => {
                self.shot_auto = false;
                self.show_status(&i18n::tr("Снимок экрана не удался."));
                self.show_window();
            }
            None if was_visible => self.show_window(),
            None => self.shot_auto = false,
        }
    }

    /// Own area picker: a full-screen window with the frozen screenshot; the user drags a rectangle.
    fn open_picker(&mut self, cap: Option<(i32, i32, u32, u32, Vec<u8>)>, my: u64, was_visible: bool) {
        if self.snip_token.load(Ordering::SeqCst) != my || self.picker.is_some() {
            return;
        }
        let Some((x, y, w, h, px)) = cap else {
            self.show_status(&i18n::tr("Снимок экрана не удался."));
            self.show_window();
            return;
        };
        let win = match SnipWindow::new() {
            Ok(win) => win,
            Err(e) => {
                log_err(&format!("area picker: {e}"));
                return;
            }
        };
        set_custom_theme(&win.global::<Theme>(), self.cfg.custom_rgb());
        win.global::<Theme>().set_accent_name(self.cfg.accent().into());
        win.global::<Theme>().set_dark(self.dark);
        win.set_shot(make_image(w, h, &px));
        win.set_img_w(w as f32);
        win.set_img_h(h as f32);
        win.window().set_position(slint::PhysicalPosition::new(x, y));
        win.window().set_size(slint::PhysicalSize::new(w, h));
        win.on_done(|fx, fy, fw, fh| with_app(|a| a.picker_closed(Some([fx, fy, fw, fh]))));
        win.on_cancelled(|| with_app(|a| a.picker_closed(None)));
        if let Err(e) = win.show() {
            log_err(&format!("area picker show: {e}"));
            return;
        }
        let place = move |win: &SnipWindow| {
            // exact physical placement over the monitor (the toolkit may rescale its own sizes)
            if let Some(hw) = hwnd_of(win.window()) {
                native::block_menu_mode(hw);
                native::set_window_rect(hw, x, y, w, h);
                native::focus_window(hw);
            }
            win.invoke_grab_focus();
        };
        place(&win);
        let weak = win.as_weak();
        Timer::single_shot(Duration::from_millis(60), move || {
            if let Some(win) = weak.upgrade() {
                place(&win);
            }
        });
        self.picker = Some(win);
        self.picker_src = Some((w, h, px));
        self.picker_ctx = (my, was_visible);
    }

    fn picker_closed(&mut self, sel: Option<[f32; 4]>) {
        let Some(win) = self.picker.take() else { return };
        let _ = win.hide();
        // dropped a moment later: this runs inside the window's own callback
        Timer::single_shot(Duration::from_millis(50), move || drop(win));
        let src = self.picker_src.take();
        let (my, was_visible) = self.picker_ctx;
        match (sel, src) {
            (Some(f), Some((w, h, px))) => {
                std::thread::spawn(move || {
                    let (cw, ch, cpx) = clip::crop_rgba(w, h, &px, f);
                    let image = clip::prepare_shot(cw, ch, cpx);
                    let _ = slint::invoke_from_event_loop(move || with_app(|a| a.finish_screenshot(image, my, was_visible)));
                });
            }
            _ => {
                self.shot_auto = false;
                if was_visible {
                    self.show_window();
                }
                Self::trim_memory_soon();
            }
        }
    }

    // ---------- pasted images ----------

    fn paste_image(&mut self) -> bool {
        let Some((b64, (w, h, rgba))) = clip::clipboard_image() else { return false };
        if self.pending.len() < self.max_shots() {
            self.pending.push((b64, make_image(w, h, &rgba)));
            self.refresh_thumbs();
        }
        true
    }

    fn remove_image(&mut self, i: usize) {
        if i < self.pending.len() {
            self.pending.remove(i);
            self.refresh_thumbs();
        }
    }

    // ---------- voice ----------

    /// The voice key went down (or the microphone button was pressed).
    fn voice_press(&mut self) {
        self.voice_begin(true);
    }

    fn voice_release(&mut self) {
        let Some(v) = &self.voice else { return };
        if !v.by_key || v.transcribing {
            return;
        }
        if v.started.elapsed() < Duration::from_millis(voice::MIN_MILLIS) {
            // an accidental tap
            return self.voice_cancel();
        }
        self.voice_finish();
    }

    fn mic_clicked(&mut self) {
        match &self.voice {
            Some(v) if !v.transcribing => self.voice_finish(),
            Some(_) => {}
            None => self.voice_begin(false),
        }
    }

    fn voice_begin(&mut self, by_key: bool) {
        if self.voice.is_some() {
            return;
        }
        // into the field when the chat window is open, otherwise a pill at the bottom of the screen
        let popup = !(self.visible && !self.overlay_view && self.composer.is_some() && !self.ui().get_settings_open());
        if self.cfg.groq_key.trim().is_empty() && self.cfg.gemini_key.trim().is_empty() {
            return self.voice_problem(&i18n::tr("Для голоса нужен ключ Groq или Gemini. Добавь его в настройках."), "settings", popup);
        }
        if voice::mic_blocked() {
            return self.voice_problem(&i18n::tr("Windows не даёт программам слушать микрофон."), "mic", popup);
        }
        let rec = match voice::Recorder::start() {
            Ok(r) => r,
            Err(e) => return self.voice_problem(&e, "", popup),
        };
        self.voice_id += 1;
        let selection = Arc::new(Mutex::new(None));
        if popup && by_key {
            // something selected in the other app: the speech becomes a note to it
            let sel = selection.clone();
            std::thread::spawn(move || {
                let text = clip::grab_selection(SMART_COPY_WAIT);
                if let Ok(mut s) = sel.lock() {
                    *s = Some(text);
                }
            });
        }
        self.voice = Some(VoiceRun { rec: Some(rec), started: Instant::now(), popup, by_key, selection, transcribing: false, id: self.voice_id });
        let ui = self.ui();
        if popup {
            self.show_pill(0, "");
            // Esc anywhere cancels while the pill is up
            if let Ok(hk) = HotKey::from_str("Escape") {
                if self.hot_mgr.register(hk).is_ok() {
                    self.hot_ids.insert(hk.id(), SLOT_ESCAPE);
                    self.esc_hotkey = Some(hk);
                }
            }
        } else {
            ui.set_recording(true);
            ui.set_transcribing(false);
        }
        self.voice_timer.start(TimerMode::Repeated, Duration::from_millis(80), || with_app(|a| a.voice_tick()));
        self.voice_tick();
    }

    fn voice_tick(&mut self) {
        let Some(v) = &self.voice else { return self.voice_timer.stop() };
        if v.transcribing {
            return;
        }
        let secs = v.started.elapsed().as_secs();
        let time = format!("{}:{:02}", secs / 60, secs % 60);
        let levels: Vec<f32> = v.rec.as_ref().and_then(|r| r.levels.lock().ok().map(|l| l.clone())).unwrap_or_default();
        let model = ModelRc::new(VecModel::from(levels));
        if v.popup {
            if let Some(p) = &self.pill {
                p.set_levels(model);
                p.set_time_text(time.into());
            }
        } else {
            let ui = self.ui();
            ui.set_rec_levels(model);
            ui.set_rec_time(time.into());
        }
        if secs >= voice::MAX_SECONDS {
            self.voice_finish();
        }
    }

    /// Recording done: turn it into text on a helper thread.
    fn voice_finish(&mut self) {
        let Some(v) = self.voice.as_mut() else { return };
        if v.transcribing {
            return;
        }
        v.transcribing = true;
        let rec = v.rec.take();
        let (id, popup) = (v.id, v.popup);
        self.drop_escape_key();
        if popup {
            self.show_pill(1, "");
        } else {
            let ui = self.ui();
            ui.set_recording(false);
            ui.set_transcribing(true);
        }
        let cfg = self.cfg.clone();
        std::thread::spawn(move || {
            let samples = rec.map(|r| r.stop()).unwrap_or_default();
            let (frames, speech, noise, threshold) = voice::speech_stats(&samples);
            log_err(&format!("voice: {frames} frames, {speech} with speech, noise {noise:.0}, threshold {threshold:.0}"));
            let res = if voice::is_silent(&samples) { Ok(String::new()) } else { voice::transcribe(&cfg, &samples) };
            let _ = slint::invoke_from_event_loop(move || with_app(|a| a.voice_done(id, res)));
        });
    }

    fn voice_done(&mut self, id: u64, res: Result<String, String>) {
        let Some(v) = self.voice.take() else { return };
        if v.id != id {
            self.voice = Some(v);
            return;
        }
        self.voice_timer.stop();
        let ui = self.ui();
        ui.set_recording(false);
        ui.set_transcribing(false);
        let text = match res {
            Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
            Ok(_) => return self.voice_problem(&i18n::tr("Ничего не расслышал. Говори ближе к микрофону."), "", v.popup),
            Err(e) => return self.voice_problem(&i18n::trf("Голос не распознан: {e}", &[("e", &e)]), "", v.popup),
        };
        if !v.popup {
            // into the field: can be corrected before sending
            let cur = ui.get_composer_text().to_string();
            let joined = if cur.trim().is_empty() { text } else { format!("{} {text}", cur.trim_end()) };
            ui.set_composer_text(joined.into());
            self.focus_input_soon();
            return;
        }
        self.hide_pill();
        let selection = v.selection.lock().ok().and_then(|s| s.clone()).unwrap_or_default();
        self.new_chat();
        if selection.trim().is_empty() {
            self.send(Request { text: text.clone(), voice: true, ..Default::default() }, Some(text.clone()), text, false);
        } else {
            let q = format!("{} — {text}", selection.trim());
            self.send(Request { text: selection.clone(), extra: text, voice: true, ..Default::default() }, Some(selection), q, false);
        }
        if self.cfg.overlay_mode {
            self.show_overlay();
        } else {
            self.show_window();
        }
    }

    /// Esc, a too short press: the recording is thrown away.
    fn voice_cancel(&mut self) {
        let Some(mut v) = self.voice.take() else { return };
        drop(v.rec.take());
        self.voice_timer.stop();
        self.drop_escape_key();
        self.hide_pill();
        let ui = self.ui();
        ui.set_recording(false);
        ui.set_transcribing(false);
    }

    fn drop_escape_key(&mut self) {
        if let Some(hk) = self.esc_hotkey.take() {
            let _ = self.hot_mgr.unregister(hk);
            self.hot_ids.remove(&hk.id());
        }
    }

    /// A problem with the voice: in the pill for a few seconds, or as a line in the open window.
    fn voice_problem(&mut self, text: &str, action: &str, popup: bool) {
        if let Some(mut v) = self.voice.take() {
            drop(v.rec.take());
        }
        self.voice_timer.stop();
        self.drop_escape_key();
        let ui = self.ui();
        ui.set_recording(false);
        ui.set_transcribing(false);
        if popup && action.is_empty() {
            self.show_pill(2, text);
            let id = self.voice_id;
            Timer::single_shot(Duration::from_millis(3500), move || {
                with_app(|a| {
                    if a.voice.is_none() && a.voice_id == id {
                        a.hide_pill();
                    }
                })
            });
            return;
        }
        self.hide_pill();
        if popup {
            // a button is needed: show it in the window
            self.new_chat();
            self.turns.push(Answer {
                text: text.to_string(),
                req: None,
                is_error: true,
                record_q: None,
                question: i18n::tr("Голосовой ввод"),
                shots: 0,
                note: String::new(),
                action: action.to_string(),
                open: true,
                waiting: false,
                ..Default::default()
            });
            self.render();
            self.show_window();
        } else {
            self.toast(text);
            if action == "mic" {
                native::open_url("ms-settings:privacy-microphone");
            } else if action == "settings" {
                self.open_settings();
            }
        }
    }

    /// The pill at the bottom centre of the screen: 0 recording, 1 recognising, 2 a message.
    fn show_pill(&mut self, phase: i32, message: &str) {
        if self.pill.is_none() {
            match VoicePill::new() {
                Ok(p) => {
                    let t = p.global::<Theme>();
                    t.set_dark(self.dark);
                    set_custom_theme(&t, self.cfg.custom_rgb());
                    t.set_accent_name(self.cfg.accent().into());
                    self.pill = Some(p);
                }
                Err(e) => return log_err(&format!("voice pill: {e}")),
            }
        }
        let Some(p) = &self.pill else { return };
        p.set_phase(phase);
        p.set_message(message.into());
        p.set_hint(i18n::tr("Отпусти — отправлю").into());
        let _ = p.show();
        if let Some(h) = hwnd_of(p.window()) {
            native::set_no_activate(h, true);
            let (cx, cy) = native::cursor_pos();
            let wa = native::work_area_at(cx, cy);
            let scale = p.window().scale_factor();
            let (w, ht) = ((300.0 * scale) as u32, (52.0 * scale) as u32);
            let x = wa.0 + (wa.2 - w as i32) / 2;
            let y = wa.1 + wa.3 - ht as i32 - (28.0 * scale) as i32;
            native::set_window_rect(h, x, y, w, ht);
        }
        p.window().with_winit_window(|w| {
            let _ = w.set_cursor_hittest(false);
        });
    }

    fn hide_pill(&mut self) {
        if let Some(p) = &self.pill {
            let _ = p.hide();
        }
    }

    fn turn_action(&mut self, action: &str) {
        match action {
            "gemini-key" => self.open_settings(),
            "mic" => native::open_url("ms-settings:privacy-microphone"),
            _ => self.open_settings(),
        }
    }

    // ---------- settings ----------

    fn toggle_settings(&mut self) {
        if self.ui().get_settings_open() {
            self.close_settings();
        } else {
            self.open_settings();
        }
    }

    /// Settings open inside the same window, at the same size.
    fn open_settings(&mut self) {
        if !self.visible || self.overlay_view {
            self.show_window();
        }
        self.hide_hist();
        self.fill_settings();
        self.grow_for_settings();
        let ui = self.ui();
        ui.set_settings_open(true);
        // the offer of the new key mode is shown once: opening the settings counts as seeing it
        if self.cfg.smart_offer == "pending" {
            ui.set_settings_dot(false);
        }
        self.check_saved_key();
    }

    fn close_settings(&mut self) {
        let ui = self.ui();
        if !ui.get_settings_open() {
            return;
        }
        self.key_note = None;
        self.hotkey_capture(false);
        self.key_done();
        ui.set_settings_open(false);
        ui.set_settings_note("".into());
        if let Some(r) = self.pre_settings.take() {
            self.apply_rect(r);
            self.watched = None;
        }
        ui.set_api_key("".into());
        if self.cfg.smart_offer == "pending" {
            // seen and ignored: never offered again
            self.cfg.smart_offer = "done".into();
            ui.set_smart_offer(false);
            self.save_now();
        }
        self.smart_explain = false;
        ui.set_smart_explain(false);
        ui.set_has_key(self.cfg.has_key());
        self.refresh_hints();
        if self.composer.is_some() {
            self.focus_input_soon();
        }
    }

    /// A low window gets taller for the settings: upwards when it sits in the lower half of the
    /// screen, so its bottom edge stays where it was.
    fn grow_for_settings(&mut self) {
        if self.pre_settings.is_some() || self.overlay_view {
            return;
        }
        let cur = self.window_rect();
        if cur.w == 0 || cur.h == 0 {
            return;
        }
        let scale = self.ui().window().scale_factor();
        let wa = native::work_area(self.hwnd());
        let w = cur.w.max((SETTINGS_MIN.0 * scale) as u32);
        let h = cur.h.max((SETTINGS_MIN.1 * scale) as u32);
        if (w, h) == (cur.w, cur.h) {
            return;
        }
        let x = if cur.x + cur.w as i32 / 2 > wa.0 + wa.2 / 2 { cur.x + cur.w as i32 - w as i32 } else { cur.x };
        let y = if cur.y + cur.h as i32 / 2 > wa.1 + wa.3 / 2 { cur.y + cur.h as i32 - h as i32 } else { cur.y };
        self.pre_settings = Some(cur);
        self.apply_rect(clamp_rect(Rect { x, y, w, h }, wa));
    }

    fn fill_settings(&mut self) {
        let ui = self.ui();
        ui.set_advanced(self.cfg.advanced);
        ui.set_provider_index(if self.cfg.is_groq() { 1 } else { 0 });
        let model = if self.cfg.model.trim().is_empty() { GEMINI_IDS[0] } else { self.cfg.model.trim() };
        let models: Vec<String> = ["Gemini 3.5 Flash-Lite · быстрая", "Gemini 3.8 Flash · умнее", "Gemini 3.1 Pro · самая умная", "Gemini 2.5 Flash · запасная", "Своя модель…"]
            .iter()
            .map(|s| i18n::tr(s))
            .collect();
        // the closed field shows just the name, the open list says what each one is for
        let short: Vec<SharedString> = models.iter().map(|m| m.split(" · ").next().unwrap_or(m).into()).collect();
        ui.set_gemini_models(ModelRc::new(VecModel::from(models.iter().map(SharedString::from).collect::<Vec<_>>())));
        ui.set_gemini_models_short(ModelRc::new(VecModel::from(short)));
        match GEMINI_IDS.iter().position(|m| *m == model) {
            Some(i) => {
                ui.set_model_index(i as i32);
                ui.set_model_custom("".into());
            }
            None => {
                ui.set_model_index(CUSTOM_MODEL_INDEX);
                ui.set_model_custom(model.into());
            }
        }
        ui.set_api_key("".into());
        self.update_key_ui();
        ui.set_smart_offer(self.cfg.smart_offer == "pending");
        ui.set_smart_explain(self.smart_explain);
        ui.set_smart_keys(self.cfg.smart_keys());
        ui.set_key_mode_index(if self.cfg.smart_keys() { 0 } else { 1 });
        self.refresh_keys_ui();
        ui.set_style_index(self.style_index());
        let prompt = if self.cfg.prompt.trim().is_empty() { llm::DEFAULT_PROMPT } else { self.cfg.prompt.as_str() };
        ui.set_prompt_text(prompt.into());
        ui.set_theme_index(match self.cfg.theme_mode() {
            "dark" => 1,
            "light" => 2,
            _ => 0,
        });
        ui.set_opacity_pct((self.cfg.opacity() * 100.0).round());
        ui.set_text_scale_index(config::TEXT_SCALES.iter().position(|&x| x == self.cfg.text_scale()).unwrap_or(1) as i32);
        ui.set_overlay_mode(self.cfg.overlay_mode);
        ui.set_overlay_fonts(ModelRc::new(VecModel::from(config::OVERLAY_FONTS.iter().map(|&f| f as i32).collect::<Vec<_>>())));
        ui.set_overlay_font_index(self.cfg.overlay_font_index() as i32);
        ui.set_shot_mode(self.cfg.shot_mode_index() as i32);
        ui.set_history_enabled(self.cfg.history_on());
        ui.set_autostart(self.cfg.autostart);
        ui.set_show_tray(!self.cfg.hide_tray);
        ui.set_auto_update(!self.cfg.no_update_check);
        ui.set_update_status("".into());
        self.refresh_actions();
        let rgb = self.cfg.custom_rgb();
        let (h, s, v) = rgb_to_hsv(rgb);
        ui.set_custom_h(h);
        ui.set_custom_s(s);
        ui.set_custom_v(v);
        ui.set_custom_text(config::hex_color(rgb).into());
        ui.set_custom_open(false);
        ui.set_version_text(i18n::trf("Версия {v}", &[("v", update::VERSION)]).into());
        ui.set_settings_note("".into());
    }

    /// 0 short, 1 with a reason, 2 detailed, -1 the user's own prompt.
    fn style_index(&self) -> i32 {
        let p = self.cfg.prompt.trim();
        if p.is_empty() {
            return 0;
        }
        llm::PRESETS.iter().position(|x| x.trim() == p).map(|i| i as i32).unwrap_or(-1)
    }

    fn update_key_ui(&mut self) {
        let ui = self.ui();
        let groq = self.cfg.is_groq();
        let saved = if groq { &self.cfg.groq_key } else { &self.cfg.gemini_key };
        let placeholder = if !saved.is_empty() {
            i18n::tr("•••••••• ключ сохранён")
        } else if groq {
            i18n::tr("Вставь ключ Groq")
        } else {
            i18n::tr("Вставь ключ из Google AI Studio")
        };
        ui.set_api_key_placeholder(placeholder.into());
        match self.key_checks.get(saved.as_str()) {
            Some(true) if !saved.is_empty() => {
                ui.set_key_state(2);
                ui.set_key_state_text(i18n::tr("Ключ работает").into());
            }
            _ => {
                ui.set_key_state(0);
                ui.set_key_state_text("".into());
            }
        }
    }

    fn provider_picked(&mut self, i: usize) {
        let groq = i == 1;
        if self.cfg.is_groq() == groq {
            return;
        }
        self.key_done();
        self.cfg.provider = if groq { "groq" } else { "gemini" }.into();
        self.save_now();
        let ui = self.ui();
        ui.set_provider_index(i as i32);
        ui.set_api_key("".into());
        self.key_check_id += 1;
        self.update_key_ui();
        self.check_saved_key();
        ui.set_has_key(self.cfg.has_key());
    }

    /// Typed or pasted into the key field: saved and checked once the typing stops.
    fn key_edited(&mut self) {
        self.key_edit_gen += 1;
        let gen = self.key_edit_gen;
        Timer::single_shot(Duration::from_millis(700), move || {
            with_app(|a| {
                if a.key_edit_gen == gen {
                    a.key_done();
                }
            })
        });
    }

    fn key_done(&mut self) {
        let ui = self.ui();
        let typed = ui.get_api_key().trim().to_string();
        if typed.is_empty() {
            return;
        }
        let groq = self.cfg.is_groq();
        let saved = if groq { &self.cfg.groq_key } else { &self.cfg.gemini_key };
        if *saved == typed {
            return;
        }
        if !config::key_looks_valid(&typed) {
            ui.set_key_state(3);
            ui.set_key_state_text(i18n::tr("Это не похоже на ключ: он одним словом, без пробелов.").into());
            return;
        }
        if groq {
            self.cfg.groq_key = typed.clone();
        } else {
            self.cfg.gemini_key = typed.clone();
        }
        self.save_now();
        ui.set_has_key(self.cfg.has_key());
        self.run_key_check(groq, typed);
    }

    /// The saved key of the chosen service is checked once per session when the settings open.
    fn check_saved_key(&mut self) {
        let groq = self.cfg.is_groq();
        let key = if groq { self.cfg.groq_key.clone() } else { self.cfg.gemini_key.clone() };
        if key.is_empty() || self.key_checks.contains_key(&key) {
            return;
        }
        self.run_key_check(groq, key);
    }

    fn run_key_check(&mut self, groq: bool, key: String) {
        let ui = self.ui();
        self.key_check_id += 1;
        let id = self.key_check_id;
        ui.set_key_state(1);
        ui.set_key_state_text(i18n::tr("Проверяю ключ…").into());
        let model = if groq { String::new() } else { self.cfg.model.clone() };
        std::thread::spawn(move || {
            let res = llm::check_key(groq, &key, &model);
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|a| {
                    let (ok, text) = match res {
                        Ok(_) => (true, i18n::tr("Ключ работает")),
                        Err(e) => (false, e),
                    };
                    if ok {
                        a.key_checks.insert(key.clone(), true);
                    }
                    if a.key_check_id != id {
                        return;
                    }
                    let ui = a.ui();
                    ui.set_key_state(if ok { 2 } else { 3 });
                    ui.set_key_state_text(text.into());
                })
            });
        });
    }

    fn model_picked(&mut self) {
        let ui = self.ui();
        let model = match ui.get_model_index() {
            CUSTOM_MODEL_INDEX => ui.get_model_custom().trim().to_string(),
            i => GEMINI_IDS.get(i as usize).copied().unwrap_or(GEMINI_IDS[0]).to_string(),
        };
        if model.is_empty() {
            return;
        }
        if !config::valid_gemini_model(&model) {
            ui.set_settings_note(i18n::tr("Недопустимое имя модели Gemini").into());
            return;
        }
        ui.set_settings_note("".into());
        let stored = if model == GEMINI_IDS[0] { String::new() } else { model };
        if self.cfg.model != stored {
            self.cfg.model = stored;
            self.save_now();
            self.key_checks.clear();
            self.check_saved_key();
        }
    }

    fn set_key_mode(&mut self, smart: bool) {
        self.cfg.key_mode = if smart { "smart" } else { "classic" }.into();
        self.save_now();
        self.register_hotkeys();
        let ui = self.ui();
        ui.set_smart_keys(smart);
        ui.set_key_mode_index(if smart { 0 } else { 1 });
        self.refresh_keys_ui();
    }

    fn offer_try(&mut self) {
        self.cfg.smart_offer = "done".into();
        self.set_key_mode(true);
        self.smart_explain = true;
        let ui = self.ui();
        ui.set_smart_offer(false);
        ui.set_smart_explain(true);
        ui.set_settings_dot(false);
    }

    fn offer_later(&mut self) {
        self.cfg.smart_offer = "done".into();
        self.save_now();
        let ui = self.ui();
        ui.set_smart_offer(false);
        ui.set_settings_dot(false);
    }

    /// Hints of the empty window and the "how the keys work now" card.
    fn refresh_hints(&self) {
        let k = self.cfg.hotkeys();
        let row = |accel: &str, text: &str| HintRow { keys: key_caps(accel), text: i18n::tr(text).into() };
        let rows = if self.cfg.smart_keys() {
            vec![
                row(&k.ask, "спросить про выделенное или экран"),
                row(&k.free, "открыть или спрятать чат"),
                row(&k.voice, "зажми и говори"),
            ]
        } else {
            vec![
                row(&k.selection, "вопрос по выделенному тексту"),
                row(&k.screenshot, "скриншот"),
                row(&k.retry, "повторить последний запрос"),
                row(&k.voice, "зажми и говори"),
            ]
        };
        let ui = self.ui();
        ui.set_hints(ModelRc::new(VecModel::from(rows)));
        ui.set_has_key(self.cfg.has_key());
        ui.set_voice_key(k.voice.as_str().into());
    }

    /// The hotkey rows of the settings, with remarks.
    fn refresh_keys_ui(&mut self) {
        self.refresh_hints();
        let k = self.cfg.hotkeys();
        let smart = self.cfg.smart_keys();
        let failed = self.hot_failed.clone();
        let note = self.key_note.clone();
        let make = |slot: usize, label: &str| {
            let accel = k.get(slot).to_string();
            if let Some((_, text)) = note.as_ref().filter(|(s, _)| *s == slot) {
                return KeyRow { slot: slot as i32, label: i18n::tr(label).into(), keys: key_caps(&accel), warn: text.as_str().into(), bad: true };
            }
            let bad = failed.contains(&slot);
            let warn = if bad {
                i18n::tr("Занято другой программой — нажми и выбери другое")
            } else if slot == HK_VOICE && types_text(&accel) {
                i18n::tr("Перестанет печататься в других программах")
            } else {
                String::new()
            };
            KeyRow { slot: slot as i32, label: i18n::tr(label).into(), keys: key_caps(&accel), warn: warn.into(), bad }
        };
        let main = if smart {
            vec![make(HK_SMART, "Выделенное или экран"), make(HK_CHAT, "Открыть чат"), make(HK_VOICE, "Голос — зажми")]
        } else {
            vec![make(HK_ASK, "Выделенный текст"), make(HK_SHOT, "Скриншот"), make(HK_CHAT, "Чат"), make(HK_VOICE, "Голос — зажми")]
        };
        let extra = if smart {
            vec![make(HK_NOTE, "Спросить с пометкой"), make(HK_RETRY, "Повторить запрос")]
        } else {
            vec![make(HK_RETRY, "Повторить запрос")]
        };
        let ui = self.ui();
        ui.set_main_keys(ModelRc::new(VecModel::from(main)));
        ui.set_extra_keys(ModelRc::new(VecModel::from(extra)));
        let hint = if is_f_key(&k.voice) { i18n::tr("На ноутбуке F-клавиши иногда нажимаются с Fn") } else { String::new() };
        ui.set_keys_hint(hint.into());
    }

    /// The settings start or stop waiting for a new key combination. Meanwhile the program's own
    /// hotkeys are off: pressed, they are recorded instead of opening the chat or taking a snapshot.
    fn hotkey_capture(&mut self, on: bool) {
        if self.capturing == on {
            return;
        }
        self.capturing = on;
        if on {
            for hk in self.registered.drain(..) {
                let _ = self.hot_mgr.unregister(hk);
            }
            self.hot_ids.clear();
        } else {
            self.register_hotkeys();
        }
    }

    /// A problem with the combination just pressed: under that key's row, not at the bottom of the settings.
    fn key_problem(&mut self, slot: usize, text: String) {
        self.key_note = Some((slot, text));
        // the rows are drawn anew, so the field stops waiting: the hotkeys work again
        self.capturing = false;
        self.register_hotkeys();
        self.refresh_keys_ui();
    }

    fn hotkey_key(&mut self, slot: usize, text: &str, ctrl: bool, alt: bool, shift: bool, meta: bool) {
        let Some(mut key) = key_name(text) else { return };
        // keypad digits come in like the ones above the letters
        if key.len() == 1 {
            if let Some(d) = key.chars().next().and_then(|c| c.to_digit(10)) {
                if native::numpad_down(d) {
                    key = format!("Numpad{d}");
                }
            }
        }
        let plain = !(ctrl || alt || shift || meta);
        // the voice key may be any single key; the others need a modifier (or an F key)
        if plain && slot != HK_VOICE && !is_f_key(&key) {
            return self.key_problem(slot, i18n::tr("Добавь Alt, Ctrl или Shift (или выбери F1–F12)"));
        }
        let mut parts = Vec::new();
        if ctrl {
            parts.push("Ctrl".to_string());
        }
        if alt {
            parts.push("Alt".to_string());
        }
        if shift {
            parts.push("Shift".to_string());
        }
        if meta {
            parts.push("Super".to_string());
        }
        parts.push(key);
        let accel = parts.join("+");
        if !config::valid_accel(&accel) {
            return self.key_problem(slot, i18n::trf("Так нельзя: {a}", &[("a", &accel)]));
        }
        let mut keys = self.cfg.hotkeys();
        if accel == "Alt+Shift+Delete" {
            return self.key_problem(slot, i18n::tr("Это сочетание занято: удаление программы"));
        }
        if let Some(other) = Hotkeys::active(self.cfg.smart_keys()).into_iter().find(|&i| i != slot && keys.get(i) == accel) {
            return self.key_problem(slot, i18n::trf("Уже занято: {what}", &[("what", &slot_label(other, self.cfg.smart_keys()))]));
        }
        keys.set(slot, accel);
        self.cfg.hotkeys = Some(keys);
        self.save_now();
        self.key_note = None;
        let ui = self.ui();
        ui.invoke_focus_root();
        self.capturing = false;
        self.register_hotkeys();
    }

    fn reset_hotkeys(&mut self) {
        self.key_note = None;
        self.cfg.hotkeys = Some(Hotkeys::default());
        self.save_now();
        self.register_hotkeys();
    }

    fn style_picked(&mut self, i: usize) {
        let Some(p) = llm::PRESETS.get(i) else { return };
        // the built-in prompt is stored as "default", so later versions of it still apply
        self.cfg.prompt = if i == 0 { String::new() } else { p.to_string() };
        self.save_now();
        let ui = self.ui();
        ui.set_prompt_text((*p).into());
        ui.set_style_index(i as i32);
    }

    fn prompt_saved(&mut self) {
        let ui = self.ui();
        let prompt = ui.get_prompt_text().trim().to_string();
        if prompt.chars().count() > config::MAX_PROMPT {
            ui.set_settings_note(i18n::trf("Промпт слишком длинный (больше {max} символов)", &[("max", &config::MAX_PROMPT.to_string())]).into());
            return;
        }
        self.cfg.prompt = if prompt == llm::DEFAULT_PROMPT.trim() { String::new() } else { prompt };
        self.save_now();
        ui.set_style_index(self.style_index());
        self.toast(&i18n::tr("Промпт сохранён"));
    }

    fn theme_picked(&mut self, i: usize) {
        self.cfg.theme = match i {
            1 => "dark",
            2 => "light",
            _ => "system",
        }
        .into();
        self.save_now();
        self.apply_theme();
    }

    fn pick_accent(&mut self, name: &str) {
        if ACCENTS.contains(&name) {
            self.cfg.accent = name.to_string();
            self.ui().global::<Theme>().set_accent_name(name.into());
            self.save_now();
            self.render();
        }
    }

    fn overlay_changed(&mut self) {
        self.cfg.overlay_mode = self.ui().get_overlay_mode();
        self.save_now();
    }

    /// Switches and lists of the settings: everything is read and saved at once.
    fn setting_changed(&mut self) {
        let ui = self.ui();
        self.cfg.advanced = ui.get_advanced();
        self.cfg.history = Some(ui.get_history_enabled());
        let autostart = ui.get_autostart();
        if autostart != self.cfg.autostart {
            if let Err(e) = native::set_autostart(autostart) {
                log_err(&format!("autostart: {e}"));
            }
        }
        self.cfg.autostart = autostart;
        self.cfg.hide_tray = !ui.get_show_tray();
        self.cfg.no_update_check = !ui.get_auto_update();
        self.auto_update.store(!self.cfg.no_update_check, Ordering::SeqCst);
        let shot = (ui.get_shot_mode().max(0) as usize).min(SHOT_MODES.len() - 1);
        self.cfg.shot_mode = SHOT_MODES[shot].to_string();
        self.cfg.full_screenshot = shot == 1;
        let fi = (ui.get_overlay_font_index().max(0) as usize).min(config::OVERLAY_FONTS.len() - 1);
        self.cfg.overlay_font = Some(config::OVERLAY_FONTS[fi]);
        self.apply_tray();
        self.save_now();
    }

    /// The buttons under the answer: their captions, and the editable list in the settings.
    fn refresh_actions(&self) {
        let ui = self.ui();
        let list = self.cfg.actions();
        ui.set_actions(ModelRc::new(VecModel::from(action_buttons(&list))));
        ui.set_action_items(ModelRc::new(VecModel::from(
            list.iter().map(|a| ActionItem { label: a.label.as_str().into(), prompt: a.prompt.as_str().into() }).collect::<Vec<_>>(),
        )));
        ui.set_actions_custom(self.cfg.actions.is_some());
    }

    fn action_edited(&mut self, i: usize, label: &str, prompt: &str) {
        let mut list = self.cfg.actions();
        let Some(a) = list.get_mut(i) else { return };
        a.label = label.chars().take(24).collect();
        a.prompt = prompt.chars().take(1000).collect();
        self.cfg.actions = Some(list);
        // only the captions under the answer: the fields being typed in stay as they are
        let ui = self.ui();
        ui.set_actions(ModelRc::new(VecModel::from(action_buttons(&self.cfg.actions()))));
        ui.set_actions_custom(true);
        self.save_soon();
    }

    fn action_add(&mut self) {
        let mut list = self.cfg.actions();
        if list.len() >= config::MAX_ACTIONS {
            return;
        }
        list.push(QuickAction { label: i18n::tr("Пример"), prompt: i18n::tr("Приведи пример.") });
        self.cfg.actions = Some(list);
        self.save_now();
        self.refresh_actions();
    }

    fn action_remove(&mut self, i: usize) {
        let mut list = self.cfg.actions();
        if i < list.len() {
            list.remove(i);
        }
        // an empty list would bring the default buttons back: keep it empty on purpose
        self.cfg.actions = Some(if list.is_empty() { vec![] } else { list });
        self.save_now();
        self.refresh_actions();
    }

    fn actions_reset(&mut self) {
        self.cfg.actions = None;
        self.save_now();
        self.refresh_actions();
    }

    /// "Что нового": every version, newest first.
    fn fill_news(&self) {
        let ui = self.ui();
        let versions: Vec<NewsVersion> = changelog::VERSIONS
            .iter()
            .map(|v| NewsVersion {
                version: v.version.into(),
                date: changelog::date_text(v).into(),
                items: ModelRc::new(VecModel::from(
                    v.items.iter().map(|it| NewsItem { tag: it.tag as i32, text: changelog::item_text(it).into() }).collect::<Vec<_>>(),
                )),
            })
            .collect();
        ui.set_news(ModelRc::new(VecModel::from(versions)));
        ui.set_news_title(i18n::trf("Что нового в {v}", &[("v", update::VERSION)]).into());
    }

    fn show_news(&mut self) {
        self.news_pending = false;
        let ui = self.ui();
        ui.set_news_strip(false);
        ui.set_update_shown(self.update.is_some() && !self.update_dismissed);
        self.open_settings();
        ui.set_news_open(true);
    }

    // ---------- updates ----------

    fn update_found(&mut self, rel: update::Release) {
        let ui = self.ui();
        ui.set_news_strip(false);
        ui.set_update_text(i18n::trf("Доступна новая версия {v}", &[("v", &rel.version)]).into());
        ui.set_update_shown(!self.update_dismissed);
        self.update = Some(rel);
    }

    fn check_updates(&mut self) {
        let ui = self.ui();
        ui.set_update_status(i18n::tr("Проверяю…").into());
        std::thread::spawn(|| {
            let res = update::check();
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|a| {
                    let text = match res {
                        Ok(Some(rel)) => {
                            let t = i18n::trf("Есть версия {v}: нажми «Обновить» над ответом.", &[("v", &rel.version)]);
                            a.update_dismissed = false;
                            a.update_found(rel);
                            t
                        }
                        Ok(None) => i18n::tr("У тебя последняя версия."),
                        Err(e) => i18n::trf("Не удалось проверить: {e}.", &[("e", &e.to_string())]),
                    };
                    a.ui().set_update_status(text.into());
                })
            });
        });
    }

    fn update_now(&mut self) {
        let Some(rel) = self.update.clone() else { return };
        if self.updating {
            return;
        }
        let ready = if update::is_installed() { rel.installer.is_some() } else { rel.portable.is_some() };
        if !ready {
            native::open_url(&rel.page); // nothing to install automatically: show the release page
            return;
        }
        self.updating = true;
        let ui = self.ui();
        ui.set_update_busy(true);
        ui.set_update_text(i18n::trf("Скачиваю версию {v}…", &[("v", &rel.version)]).into());
        std::thread::spawn(move || {
            let res = update::apply(&rel);
            let _ = slint::invoke_from_event_loop(move || match res {
                Ok(()) => {
                    let _ = slint::quit_event_loop();
                }
                Err(e) => with_app(|a| {
                    log_err(&format!("update: {e}"));
                    a.updating = false;
                    let ui = a.ui();
                    ui.set_update_busy(false);
                    ui.set_update_text(i18n::trf("Обновление не удалось: {e}", &[("e", &e.to_string())]).into());
                }),
            });
        });
    }

    fn update_dismiss(&mut self) {
        let ui = self.ui();
        if self.news_pending {
            self.news_pending = false;
            ui.set_news_strip(false);
            ui.set_update_shown(self.update.is_some() && !self.update_dismissed);
            if self.update.is_some() && !self.update_dismissed {
                if let Some(rel) = self.update.clone() {
                    self.update_found(rel);
                }
            }
            return;
        }
        self.update_dismissed = true;
        ui.set_update_shown(false);
    }
}

/// Skia draws on the CPU too (no graphics driver memory), with smoother shapes and text.
fn renderer_name() -> String {
    #[cfg(debug_assertions)]
    if let Ok(r) = std::env::var("NORTEX_RENDERER") {
        return r;
    }
    "skia-software".into()
}

/// The regular window size; debug builds can try others (NORTEX_SIZE=420x560) for screenshots.
fn normal_size() -> (f32, f32) {
    #[cfg(debug_assertions)]
    if let Ok(s) = std::env::var("NORTEX_SIZE") {
        if let Some((w, h)) = s.split_once('x') {
            if let (Ok(w), Ok(h)) = (w.trim().parse::<f32>(), h.trim().parse::<f32>()) {
                return (w, h);
            }
        }
    }
    NORMAL_SIZE
}

fn main() {
    std::panic::set_hook(Box::new(|info| log_err(&format!("panic: {info}"))));
    if std::env::args().any(|a| a == update::AFTER_UPDATE_ARG) {
        // started by a portable self-update: the old copy is still quitting
        let deadline = Instant::now() + Duration::from_secs(15);
        while !native::acquire_single_instance() {
            if Instant::now() > deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    } else if !native::acquire_single_instance() {
        return;
    }
    update::cleanup();
    let store = match crypto::Store::open(&config::data_dir()) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            log_err(&format!("storage: {e}"));
            return;
        }
    };
    let fresh = !config::exists();
    let mut cfg = config::load(&store);
    let mut news_pending = false;
    #[cfg(feature = "test-copy")]
    {
        if fresh {
            config::import_real_keys(&mut cfg);
        }
        cfg.no_update_check = true; // the test copy never updates itself
    }
    if fresh {
        // the very first start: language from Windows, the new key mode, nothing to tell about
        cfg.lang = if native::windows_english() { "en".into() } else { "ru".into() };
        cfg.key_mode = "smart".into();
        cfg.seen_version = update::VERSION.into();
    } else {
        if cfg.key_mode.is_empty() {
            // updated from 1.3: the keys keep working as before, the new mode is offered once
            cfg.key_mode = "classic".into();
            cfg.smart_offer = "pending".into();
        }
        if cfg.seen_version != update::VERSION {
            // marked as seen once the window shows the strip (see present): a restart before
            // the first opening must not lose it
            news_pending = true;
        }
    }
    let _ = config::save(&store, &cfg);
    i18n::set_en(cfg.is_en());
    #[cfg(debug_assertions)]
    {
        if let Ok(k) = std::env::var("NORTEX_GEMINI_KEY") {
            cfg.gemini_key = k;
        }
        if let Ok(k) = std::env::var("NORTEX_GROQ_KEY") {
            cfg.groq_key = k;
            cfg.provider = "groq".into();
        }
        if std::env::var("NORTEX_OVERLAY").is_ok() {
            cfg.overlay_mode = true;
        }
        if let Ok(p) = std::env::var("NORTEX_PROVIDER") {
            cfg.provider = p; // test runs: pick the provider without touching the saved settings
        }
    }

    // Windows are created "inactive": showing the overlay must not steal focus from a game.
    // The regular window takes focus explicitly (see force_focus).
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(renderer_name())
        // no taskbar button: the app lives in the tray
        .with_winit_window_attributes_hook(|attrs| attrs.with_active(false).with_skip_taskbar(true))
        .select()
        .expect("select winit backend");

    let ui = MainWindow::new().expect("create window");
    // English texts come from the bundled .po file; "" selects the Russian source texts
    let _ = slint::select_bundled_translation(if cfg.is_en() { "en-US" } else { "" });
    ui.set_lang_en(cfg.is_en());
    if cfg!(feature = "test-copy") {
        ui.set_app_title("Nortex Assist · тест".into());
    }
    // files and folders dragged from Explorer into the window are attached like the paperclip ones
    ui.window().on_winit_window_event(|_, event| {
        if let slint::winit_030::winit::event::WindowEvent::DroppedFile(path) = event {
            let path = path.clone();
            let _ = slint::invoke_from_event_loop(move || with_app(|a| a.files_dropped(path)));
        }
        slint::winit_030::EventResult::Propagate
    });
    let dark = cfg.dark();
    {
        let theme = ui.global::<Theme>();
        theme.set_dark(dark);
        set_custom_theme(&theme, cfg.custom_rgb());
        theme.set_accent_name(cfg.accent().into());
    }
    ui.invoke_sync_palette();

    let hot_mgr = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => {
            log_err(&format!("hotkey manager: {e}"));
            return;
        }
    };
    #[cfg(debug_assertions)]
    let open_settings = std::env::var("NORTEX_OPEN_SETTINGS").is_ok();
    #[cfg(not(debug_assertions))]
    let open_settings = false;
    let first_run = !cfg.has_key() || open_settings;
    let cfg_auto_off = cfg.no_update_check;
    let feed = Rc::new(VecModel::<FeedItem>::default());
    ui.set_feed(ModelRc::from(feed.clone()));
    let smart_offer = cfg.smart_offer == "pending";
    let app = Rc::new(RefCell::new(App {
        ui: ui.as_weak(),
        history: History::new(store.clone()),
        store,
        cfg,
        turns: Vec::new(),
        feed,
        status: String::new(),
        last_activity: Instant::now(),
        req_id: 0,
        live_req: Arc::new(AtomicU64::new(0)),
        busy: false,
        in_flight: None,
        update: None,
        update_dismissed: false,
        updating: false,
        auto_update: Arc::new(AtomicBool::new(!cfg_auto_off)),
        chat: Vec::new(),
        composer: None,
        pending: Vec::new(),
        attached: Vec::new(),
        dialog_open: Arc::new(AtomicBool::new(false)),
        select_mode: false,
        uninstall_left: 0,
        uninstall_timer: Timer::default(),
        shot_auto: false,
        visible: false,
        overlay_view: false,
        auto_hide: None,
        last_hide: Instant::now(),
        tick_timer: Timer::default(),
        ticks: 0,
        placed: false,
        pinned: false,
        hist_open: false,
        shown_at: Instant::now(),
        watched: None,
        drag: None,
        snip_token: Arc::new(AtomicU64::new(0)),
        picker: None,
        picker_src: None,
        picker_ctx: (0, false),
        toast_gen: 0,
        save_gen: 0,
        tray: None,
        dark,
        hot_mgr,
        registered: Vec::new(),
        hot_ids: HashMap::new(),
        hot_failed: Vec::new(),
        esc_hotkey: None,
        key_check_id: 0,
        key_edit_gen: 0,
        key_checks: HashMap::new(),
        capturing: false,
        key_note: None,
        smart_explain: false,
        news_pending,
        pre_settings: None,
        had_focus: false,
        voice: None,
        voice_id: 0,
        voice_timer: Timer::default(),
        pill: None,
    }));
    APP.with(|a| *a.borrow_mut() = Some(app.clone()));

    // ---- UI callbacks ----
    ui.on_submit(|text| with_app(|a| a.submit(text.to_string())));
    ui.on_stop(|| with_app(|a| a.stop()));
    ui.on_quick_action(|i| with_app(|a| a.quick_action(i.max(0) as usize)));
    ui.on_retry(|| with_app(|a| a.retry()));
    ui.on_copy_answer(|| with_app(|a| a.copy_answer()));
    ui.on_toggle_select(|| with_app(|a| a.toggle_select()));
    ui.on_copy_code(|t| clip::set_text(&t));
    ui.on_toggle_turn(|i| with_app(|a| a.toggle_turn(i.max(0) as usize)));
    ui.on_turn_action(|s| with_app(|a| a.turn_action(&s)));
    ui.on_mic_clicked(|| with_app(|a| a.mic_clicked()));
    ui.on_update_now(|| with_app(|a| a.update_now()));
    ui.on_update_dismiss(|| with_app(|a| a.update_dismiss()));
    ui.on_show_news(|| with_app(|a| a.show_news()));
    ui.on_attach_pick(|folder| with_app(|a| a.attach_pick(folder)));
    ui.on_remove_file(|i| with_app(|a| a.remove_file(i.max(0) as usize)));
    ui.on_paste_image(|| {
        let mut handled = false;
        with_app(|a| handled = a.paste_image());
        handled
    });
    ui.on_remove_image(|i| with_app(|a| a.remove_image(i.max(0) as usize)));
    ui.on_toggle_history(|| with_app(|a| a.toggle_history()));
    ui.on_history_selected(|i| with_app(|a| a.history_selected(i.max(0) as usize)));
    ui.on_clear_history(|| with_app(|a| a.clear_history()));
    ui.on_toggle_settings(|| with_app(|a| a.toggle_settings()));
    ui.on_toggle_pin(|| {
        with_app(|a| {
            a.pinned = !a.pinned;
            a.ui().set_pinned(a.pinned);
        })
    });
    ui.on_hide_window(|| with_app(|a| a.close_pressed()));
    ui.on_escape_pressed(|| with_app(|a| a.close_pressed()));
    // Moving/resizing is done by following the cursor ourselves rather than via the native
    // modal loop, which swallows the mouse-up and leaves the toolkit's buttons unresponsive.
    ui.on_drag_start(|| with_app(|a| a.drag_start(false)));
    ui.on_drag_move(|| with_app(|a| a.drag_move()));
    ui.on_resize_start(|| with_app(|a| a.drag_start(true)));
    ui.on_resize_move(|| with_app(|a| a.drag_move()));
    // settings
    ui.on_provider_picked(|i| with_app(|a| a.provider_picked(i.max(0) as usize)));
    ui.on_key_edited(|_| with_app(|a| a.key_edited()));
    ui.on_key_done(|| with_app(|a| a.key_done()));
    ui.on_open_key_link(|| {
        with_app(|a| native::open_url(if a.cfg.is_groq() { "https://console.groq.com/keys" } else { "https://aistudio.google.com/app/apikey" }))
    });
    ui.on_model_picked(|| with_app(|a| a.model_picked()));
    ui.on_key_mode_picked(|smart| with_app(|a| a.set_key_mode(smart)));
    ui.on_offer_try(|| with_app(|a| a.offer_try()));
    ui.on_offer_later(|| with_app(|a| a.offer_later()));
    ui.on_explain_done(|| {
        with_app(|a| {
            a.smart_explain = false;
            a.ui().set_smart_explain(false);
        })
    });
    ui.on_hotkey_key(|i, t, c, al, s, m| with_app(|a| a.hotkey_key(i.max(0) as usize, &t, c, al, s, m)));
    ui.on_hotkey_cancelled({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.invoke_focus_root();
            }
        }
    });
    ui.on_reset_hotkeys(|| with_app(|a| a.reset_hotkeys()));
    // deferred: the key field also loses its focus while the app itself redraws the rows
    ui.on_hotkey_capture(|on| {
        let _ = slint::invoke_from_event_loop(move || with_app(|a| a.hotkey_capture(on)));
    });
    ui.on_style_picked(|i| with_app(|a| a.style_picked(i.max(0) as usize)));
    ui.on_prompt_saved(|| with_app(|a| a.prompt_saved()));
    ui.on_theme_picked(|i| with_app(|a| a.theme_picked(i.max(0) as usize)));
    ui.on_accent_picked(|name| with_app(|a| a.pick_accent(&name)));
    ui.on_custom_color_picked(|c| with_app(|a| a.custom_color_picked(c)));
    ui.on_custom_text_edited(|t| with_app(|a| a.custom_text_edited(&t)));
    ui.on_opacity_changed(|v| with_app(|a| a.set_opacity(v)));
    ui.on_text_scale_changed(|i| with_app(|a| a.set_text_scale_index(i.max(0) as usize)));
    ui.on_overlay_changed(|| with_app(|a| a.overlay_changed()));
    ui.on_setting_changed(|| with_app(|a| a.setting_changed()));
    ui.on_action_edited(|i, l, p| with_app(|a| a.action_edited(i.max(0) as usize, &l, &p)));
    ui.on_action_add(|| with_app(|a| a.action_add()));
    ui.on_action_remove(|i| with_app(|a| a.action_remove(i.max(0) as usize)));
    ui.on_actions_reset(|| with_app(|a| a.actions_reset()));
    ui.on_check_updates(|| with_app(|a| a.check_updates()));
    ui.on_toggle_lang(|| with_app(|a| a.toggle_language()));
    ui.on_exit_app(|| {
        with_app(|a| a.close_settings());
        let _ = slint::quit_event_loop();
    });
    ui.on_uninstall_ask(|| with_app(|a| a.uninstall_ask()));
    ui.on_uninstall_cancel(|| with_app(|a| a.uninstall_cancel()));
    ui.on_uninstall_confirm(|| with_app(|a| a.uninstall_confirmed()));
    ui.window().on_close_requested(|| {
        with_app(|a| {
            if a.ui().get_settings_open() {
                a.close_settings();
            }
            a.visible = false;
            a.watched = None;
            a.tick_timer.stop();
        });
        slint::CloseRequestResponse::HideWindow
    });

    // ---- hotkeys, texts that depend on the settings ----
    {
        let mut a = app.borrow_mut();
        a.register_hotkeys();
        if !a.hot_failed.is_empty() {
            log_err(&format!("hotkeys not registered: {:?}", a.hot_failed));
        }
        a.refresh_keys_ui();
        a.refresh_actions();
        a.fill_news();
        a.update_placeholder();
        let ui = a.ui();
        ui.set_settings_dot(smart_offer);
        ui.set_version_text(i18n::trf("Версия {v}", &[("v", update::VERSION)]).into());
        if a.news_pending {
            ui.set_news_strip(true);
            ui.set_update_text(i18n::trf("Обновлено до {v}", &[("v", update::VERSION)]).into());
            ui.set_update_shown(true);
        }
    }
    // ---- tray icon (the app has no taskbar button) ----
    let open_item = MenuItem::new(i18n::tr("Открыть"), true, None);
    let settings_item = MenuItem::new(i18n::tr("Настройки"), true, None);
    let quit_item = MenuItem::new(i18n::tr("Выход"), true, None);
    TRAY_ITEMS.with(|t| *t.borrow_mut() = Some((open_item.clone(), settings_item.clone(), quit_item.clone())));
    let menu = Menu::new();
    let _ = menu.append_items(&[&open_item, &settings_item, &PredefinedMenuItem::separator(), &quit_item]);
    let tray_image = image::load_from_memory(include_bytes!("../ui/icon.png"))
        .map(|i| image::imageops::resize(&i.to_rgba8(), 32, 32, image::imageops::FilterType::Lanczos3))
        .expect("embedded icon decodes");
    let tray = tray_icon::Icon::from_rgba(tray_image.into_raw(), 32, 32)
        .map_err(|e| e.to_string())
        .and_then(|icon| {
            TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_tooltip(if cfg!(feature = "test-copy") { "Nortex Assist 1.4.0 — тестовая копия" } else { "Nortex Assist" })
                .with_icon(icon)
                .build()
                .map_err(|e| e.to_string())
        })
        .map_err(|e| log_err(&format!("tray icon: {e}")))
        .ok();
    {
        let mut a = app.borrow_mut();
        a.tray = tray;
        a.apply_tray();
    }
    let (open_id, settings_id, quit_id) = (open_item.id().clone(), settings_item.id().clone(), quit_item.id().clone());

    // Hotkey and tray events arrive on the UI thread through the Win32 message loop; forward each
    // one to the Slint event loop instead of polling, so the idle app does not wake up at all.
    GlobalHotKeyEvent::set_event_handler(Some(|ev: GlobalHotKeyEvent| {
        let id = ev.id;
        let pressed = ev.state == HotKeyState::Pressed;
        let _ = slint::invoke_from_event_loop(move || {
            with_app(|a| {
                if let Some(&slot) = a.hot_ids.get(&id) {
                    a.on_hotkey(slot, pressed);
                }
            })
        });
    }));
    TrayIconEvent::set_event_handler(Some(|ev: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
            let _ = slint::invoke_from_event_loop(|| with_app(|a| a.tray_open()));
        }
    }));
    MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
        if ev.id == open_id {
            let _ = slint::invoke_from_event_loop(|| with_app(|a| a.tray_open()));
        } else if ev.id == settings_id {
            let _ = slint::invoke_from_event_loop(|| with_app(|a| a.tray_settings()));
        } else if ev.id == quit_id {
            let _ = slint::quit_event_loop();
        }
    }));

    if !update::REPO.is_empty() && !cfg!(feature = "test-copy") {
        let enabled = app.borrow().auto_update.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(10));
            if enabled.load(Ordering::SeqCst) {
                match update::check() {
                    Ok(Some(rel)) => {
                        let _ = slint::invoke_from_event_loop(move || {
                            with_app(|a| {
                                if a.news_pending {
                                    a.update = Some(rel); // shown after the "what's new" strip
                                } else {
                                    a.update_found(rel);
                                }
                            })
                        });
                    }
                    Ok(None) => {}
                    Err(e) => log_err(&format!("update check: {e}")),
                }
            }
            std::thread::sleep(Duration::from_secs(12 * 3600));
        });
    }

    App::trim_memory_soon();

    #[cfg(debug_assertions)]
    let demo = std::env::var("NORTEX_DEMO_OUT").ok();
    #[cfg(not(debug_assertions))]
    let demo: Option<String> = None;

    if first_run && demo.is_none() {
        // on first launch show the window so the user can enter a key
        let mut a = app.borrow_mut();
        a.open_chat();
        a.open_settings();
    }

    #[cfg(debug_assertions)]
    if let Some(dir) = demo {
        demo::run(std::path::PathBuf::from(dir));
    }

    slint::run_event_loop_until_quit().expect("event loop");
}
