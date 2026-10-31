#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clip;
mod config;
mod crypto;
mod history;
mod llm;
mod logging;
mod native;

use config::{Config, Hotkeys, Rect, SettingsInput, ACCENTS, GROQ_MODEL, HOTKEY_NAMES};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use history::History;
use llm::{Request, Turn};
use logging::log_err;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows;
use slint::winit_030::WinitWindowAccessor;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

slint::include_modules!();

const MAX_ANSWERS: usize = 5;
const MAX_SHOTS: usize = 4;
const NORMAL_SIZE: (f32, f32) = (540.0, 680.0);
const SETTINGS_SIZE: (f32, f32) = (720.0, 600.0);
const BLUR_GRACE: Duration = Duration::from_millis(400);
const OVERLAY_SIZE: (f32, f32) = (460.0, 150.0);
const OVERLAY_TIMEOUT: Duration = Duration::from_secs(20);

const GEMINI_IDS: [&str; 10] = [
    "gemini-3.5-flash-lite",
    "gemini-3.8-flash",
    "gemini-3.7-flash",
    "gemini-3.6-flash",
    "gemini-3.5-flash",
    "gemini-3.1-flash-lite",
    "gemini-3.1-pro-preview",
    "gemini-3-flash-preview",
    "gemini-2.5-pro",
    "gemini-2.5-flash",
];
const CUSTOM_MODEL_INDEX: i32 = 10;
const HOTKEY_LABELS: [&str; 4] = ["Выделенный текст", "Скриншот", "Свободный вопрос / настройки", "Повторить запрос"];

struct Answer {
    text: String,
    req: Option<Request>,
    chat: Option<u64>,
    is_error: bool,
    record_q: Option<String>,
    /// what was asked, shown above the answer
    question: String,
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
    Shot(Vec<String>),
    FollowUp,
}

struct App {
    ui: slint::Weak<MainWindow>,
    cfg: Config,
    store: Arc<crypto::Store>,
    history: History,

    answers: Vec<Answer>,
    idx: usize,
    req_id: u64,
    /// id of the request whose answer is still awaited (0: none); the worker thread stops when it changes
    live_req: Arc<AtomicU64>,
    busy: bool,
    streaming: String,
    in_flight: Option<(Request, Option<String>, Option<u64>)>,
    key_check_id: u64,
    chat_id: u64,
    chat: Vec<Turn>,
    composer: Option<Composer>,
    pending: Vec<(String, Image)>,
    pending_shots: Vec<String>,

    visible: bool,
    overlay_view: bool,
    auto_hide: Option<Instant>,
    last_hide: Instant,
    /// housekeeping timer (blur-hide, auto-hide, geometry saving): runs only while the window is visible
    tick_timer: Timer,
    placed: bool,
    pinned: bool,
    hist_open: bool,
    shown_at: Instant,
    prev_bounds: Option<Rect>,
    watched: Option<(Rect, Instant)>,
    drag: Option<DragState>,
    snip_token: Arc<AtomicU64>,

    hot_mgr: GlobalHotKeyManager,
    registered: Vec<HotKey>,
    hot_ids: HashMap<u32, usize>,

    draft_hot: Hotkeys,
    key_drafts: [String; 2],
    active_provider: usize,
}

thread_local! {
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

fn make_image(w: u32, h: u32, rgba: &[u8]) -> Image {
    Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(rgba, w, h))
}

/// One-line, length-limited form of a question for the line above the answer.
fn question_line(q: &str) -> String {
    let flat = q.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 200 { flat.chars().take(200).collect::<String>() + "…" } else { flat }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
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
    if (0xF704..=0xF70F).contains(&(c as u32)) {
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

impl App {
    fn ui(&self) -> MainWindow {
        self.ui.upgrade().expect("main window is alive")
    }

    fn hwnd(&self) -> Option<isize> {
        let ui = self.ui.upgrade()?;
        let handle = ui.window().window_handle();
        match handle.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
            _ => None,
        }
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

    /// Shows the compact game-overlay window: it never takes focus and lets clicks through.
    fn show_overlay(&mut self) {
        self.present(true);
    }

    fn present(&mut self, overlay: bool) {
        let ui = self.ui();
        self.shown_at = Instant::now();
        let mode_changed = self.overlay_view != overlay;
        self.overlay_view = overlay;
        ui.set_overlay_view(overlay);
        let _ = ui.show();
        if !self.tick_timer.running() {
            self.tick_timer.start(TimerMode::Repeated, Duration::from_millis(120), || with_app(|a| a.tick()));
        }
        self.apply_window_mode(overlay);
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
        if overlay {
            self.auto_hide = Some(Instant::now() + OVERLAY_TIMEOUT);
        } else {
            self.auto_hide = None;
            self.force_focus();
            for ms in [40u64, 250] {
                Timer::single_shot(Duration::from_millis(ms), || with_app(|a| a.force_focus_if_lost()));
            }
        }
    }

    /// Overlay: no activation, mouse passes through. Regular: normal interactive window.
    fn apply_window_mode(&self, overlay: bool) {
        let ui = self.ui();
        if let Some(h) = self.hwnd() {
            native::set_no_activate(h, overlay);
        }
        ui.window().with_winit_window(|w| {
            let _ = w.set_cursor_hittest(!overlay);
        });
    }

    fn place_window(&mut self) {
        let ui = self.ui();
        let scale = ui.window().scale_factor();
        let r = if self.overlay_view {
            let wa = native::work_area_cursor();
            let (w, h) = ((OVERLAY_SIZE.0 * scale) as u32, (OVERLAY_SIZE.1 * scale) as u32);
            let m = (16.0 * scale) as i32;
            self.apply_rect(clamp_rect(Rect { x: wa.0 + wa.2 - w as i32 - m, y: wa.1 + wa.3 - h as i32 - m, w, h }, wa));
            ui.set_win_opacity(self.cfg.opacity());
            return;
        } else {
            let wa = native::work_area(self.hwnd());
            let r = match self.prev_bounds {
                Some(_) => self.window_rect(), // settings are open: they already sized the window
                None => self.cfg.bounds.unwrap_or_else(|| {
                    let (w, h) = ((NORMAL_SIZE.0 * scale) as u32, (NORMAL_SIZE.1 * scale) as u32);
                    let m = (16.0 * scale) as i32;
                    Rect { x: wa.0 + wa.2 - w as i32 - m, y: wa.1 + wa.3 - h as i32 - m, w, h }
                }),
            };
            clamp_rect(r, wa)
        };
        self.apply_rect(r);
        ui.set_win_opacity(self.cfg.opacity());
    }

    fn force_focus(&self) {
        if let Some(h) = self.hwnd() {
            native::focus_window(h);
        }
    }

    fn force_focus_if_lost(&self) {
        if self.visible && self.hwnd().is_some_and(|h| native::foreground() != h) {
            self.force_focus();
        }
    }

    fn hide_window(&mut self) {
        if self.prev_bounds.is_some() {
            let _ = self.save_settings(); // leaving with settings open keeps what was typed, like closing them
            self.finish_settings_close();
        }
        self.hide_hist();
        let _ = self.ui().hide();
        self.visible = false;
        self.overlay_view = false;
        self.auto_hide = None;
        self.watched = None;
        self.last_hide = Instant::now();
        self.tick_timer.stop();
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
        if self.overlay_view {
            // the overlay never has focus, so it leaves on a timer (or when the hotkey is pressed again)
            if self.auto_hide.is_some_and(|t| Instant::now() >= t) {
                self.hide_window();
            }
            return;
        }
        let Some(h) = self.hwnd() else { return };
        if !self.pinned && self.shown_at.elapsed() > BLUR_GRACE && native::foreground() != h {
            self.hide_window();
            return;
        }
        if self.prev_bounds.is_some() {
            return; // the temporary settings size is not remembered
        }
        let now = self.window_rect();
        match &self.watched {
            Some((r, since)) if *r == now => {
                if since.elapsed() > Duration::from_millis(400) && self.cfg.bounds != Some(now) {
                    self.cfg.bounds = Some(now);
                    config::save(&self.store, &self.cfg);
                }
            }
            _ => self.watched = Some((now, Instant::now())),
        }
    }

    // ---------- answers ----------

    fn set_nav(&self, on: bool) {
        let ui = self.ui();
        let n = self.answers.len();
        ui.set_pos_text(if n > 0 { format!("{} / {}", self.idx + 1, n) } else { String::new() }.into());
        ui.set_prev_enabled(on && self.idx > 0);
        ui.set_next_enabled(on && self.idx + 1 < n);
        ui.set_retry_enabled(on && self.answers.get(self.idx).is_some_and(|a| a.req.is_some()));
    }

    fn show_status(&mut self, text: &str) {
        self.hide_hist();
        self.composer = None;
        let ui = self.ui();
        ui.set_composer_on(false);
        ui.set_question_text("".into());
        ui.set_body_text(text.into());
        self.set_nav(false);
    }

    fn set_busy(&mut self, on: bool) {
        self.busy = on;
        self.ui().set_busy(on);
    }

    fn render(&mut self) {
        self.hide_hist();
        self.composer = None;
        let ui = self.ui();
        ui.set_composer_on(false);
        let Some(e) = self.answers.get(self.idx) else {
            ui.set_question_text("".into());
            ui.set_body_text("".into());
            self.set_nav(true);
            return;
        };
        ui.set_question_text(question_line(&e.question).into());
        ui.set_body_text(e.text.as_str().into());
        let follow_up = !self.overlay_view && e.chat == Some(self.chat_id) && self.idx + 1 == self.answers.len();
        self.set_nav(true);
        if follow_up {
            ui.set_composer_placeholder("Уточни ответ".into());
            ui.set_composer_text("".into());
            self.composer = Some(Composer::FollowUp);
            ui.set_composer_on(true);
            self.focus_input_soon();
        }
    }

    fn push_answer(&mut self, a: Answer) {
        if let Some(last) = self.answers.last_mut() {
            last.req = None;
        }
        self.answers.push(a);
        if self.answers.len() > MAX_ANSWERS {
            self.answers.remove(0);
        }
        self.idx = self.answers.len() - 1;
        self.render();
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

    fn show_composer(&mut self, label: &str, comp: Composer, placeholder: &str, keep: bool) -> bool {
        if !self.close_settings() {
            return false;
        }
        self.hide_hist();
        let ui = self.ui();
        ui.set_question_text("".into());
        ui.set_body_text(label.into());
        self.set_nav(false);
        ui.set_composer_placeholder(placeholder.into());
        if !keep {
            ui.set_composer_text("".into());
            self.pending.clear();
            self.refresh_thumbs();
        }
        self.composer = Some(comp);
        ui.set_composer_on(true);
        self.focus_input_soon();
        true
    }

    fn refresh_thumbs(&self) {
        let items: Vec<ThumbItem> = self.pending.iter().map(|(_, img)| ThumbItem { img: img.clone() }).collect();
        self.ui().set_thumbs(ModelRc::new(VecModel::from(items)));
    }

    fn take_pending(&mut self) -> Vec<String> {
        let imgs = self.pending.drain(..).map(|(b, _)| b).collect();
        self.refresh_thumbs();
        imgs
    }

    fn new_chat(&mut self) {
        self.chat_id += 1;
        self.chat.clear();
    }

    // ---------- asking ----------

    fn submit(&mut self, extra: String) {
        let extra = extra.trim().to_string();
        let Some(comp) = self.composer.clone() else { return };
        match comp {
            Composer::Free => {
                if extra.is_empty() && self.pending.is_empty() {
                    self.hide_window();
                    return;
                }
                self.ask_in_chat(extra);
            }
            Composer::FollowUp => {
                if extra.is_empty() && self.pending.is_empty() {
                    return;
                }
                self.ask_in_chat(extra);
            }
            Composer::Selection(text) => {
                let imgs = self.take_pending();
                self.new_chat();
                let cid = self.chat_id;
                if imgs.is_empty() {
                    self.send(Request { text: text.clone(), extra, ..Default::default() }, Some(text), Some(cid));
                } else {
                    let combined = if extra.is_empty() { text.clone() } else { format!("{text}\n{extra}") };
                    self.send(Request { extra: combined, images: imgs, ..Default::default() }, Some(text), Some(cid));
                }
            }
            Composer::Shot(shots) => {
                let mut all = shots;
                all.extend(self.take_pending());
                self.new_chat();
                let cid = self.chat_id;
                let q = if extra.is_empty() { "[скриншот]".to_string() } else { extra.clone() };
                self.send(Request { extra, images: all, ..Default::default() }, Some(q), Some(cid));
            }
        }
    }

    fn ask_in_chat(&mut self, q: String) {
        let imgs = self.take_pending();
        let history = self.chat.clone();
        let cid = self.chat_id;
        if imgs.is_empty() {
            self.send(Request { text: q.clone(), history, ..Default::default() }, Some(q), Some(cid));
        } else {
            let rq = if q.is_empty() { "[скриншот]".to_string() } else { q.clone() };
            self.send(Request { extra: q, images: imgs, history, ..Default::default() }, Some(rq), Some(cid));
        }
    }

    fn send(&mut self, req: Request, record_q: Option<String>, chat: Option<u64>) {
        self.req_id += 1;
        let id = self.req_id;
        if !req.images.is_empty() {
            self.pending_shots.clear();
        }
        self.show_status("Думаю…");
        self.ui().set_question_text(question_line(&history::describe(&req)).into());
        self.live_req.store(id, Ordering::SeqCst);
        self.streaming.clear();
        self.in_flight = Some((req.clone(), record_q.clone(), chat));
        self.set_busy(true);
        let cfg = self.cfg.clone();
        let live = self.live_req.clone();
        std::thread::spawn(move || {
            let on_chunk = |t: &str| {
                let t = t.to_string();
                let _ = slint::invoke_from_event_loop(move || with_app(|a| a.on_chunk(id, &t)));
            };
            let result = llm::ask(&cfg, &req, &on_chunk, &|| live.load(Ordering::SeqCst) != id);
            let _ = slint::invoke_from_event_loop(move || with_app(|a| a.on_done(id, req, result, record_q, chat)));
        });
    }

    fn on_chunk(&mut self, id: u64, text: &str) {
        if id == self.req_id {
            self.streaming = text.to_string();
            self.ui().set_body_text(text.into());
        }
    }

    /// Stop button: drop the running request, keep whatever part of the answer already arrived.
    fn stop(&mut self) {
        if !self.busy {
            return;
        }
        self.req_id += 1; // late chunks and the final result of the old request are ignored
        self.live_req.store(0, Ordering::SeqCst);
        self.set_busy(false);
        let partial = std::mem::take(&mut self.streaming);
        let Some((req, record_q, chat)) = self.in_flight.take() else { return self.render() };
        let text = if partial.trim().is_empty() { "Запрос остановлен.".to_string() } else { format!("{}\n\n[остановлено]", partial.trim()) };
        let question = history::describe(&req);
        // marked as an error so that "retry" simply resends it instead of treating it as a rejected answer
        self.push_answer(Answer { text, req: Some(req), chat, is_error: true, record_q, question });
    }

    fn on_done(&mut self, id: u64, req: Request, result: Result<String, String>, record_q: Option<String>, chat: Option<u64>) {
        if id != self.req_id {
            return;
        }
        self.in_flight = None;
        self.streaming.clear();
        self.live_req.store(0, Ordering::SeqCst);
        self.set_busy(false);
        let question = history::describe(&req);
        let (text, is_error) = match result {
            Ok(answer) => {
                self.history.add(self.cfg.history_on(), &req, &answer);
                if let (Some(q), Some(cid)) = (&record_q, chat) {
                    if cid == self.chat_id {
                        record_turn(&mut self.chat, q, &answer);
                    }
                }
                (answer, false)
            }
            Err(e) => {
                let mut text = format!("Ошибка: {e}");
                if self.overlay_view {
                    // the overlay cannot be clicked, so point at the hotkey that resends the request
                    text.push_str(&format!("\n\n{} — повторить запрос", self.cfg.hotkeys().retry));
                }
                (text, true)
            }
        };
        self.push_answer(Answer { text, req: Some(req), chat, is_error, record_q, question });
    }

    fn retry(&mut self) {
        let Some(e) = self.answers.get(self.idx) else { return };
        let Some(mut req) = e.req.clone() else { return };
        if !e.is_error {
            req.rejected.push(e.text.clone());
        }
        let keep = req.rejected.len().saturating_sub(3);
        req.rejected.drain(..keep);
        let (record_q, chat) = (e.record_q.clone(), e.chat);
        self.send(req, record_q, chat);
    }

    /// Hotkey: send the last request again (after a rate-limit error, or when the answer was wrong).
    fn retry_last(&mut self) {
        let has_request = self.answers.last().is_some_and(|a| a.req.is_some());
        let overlay = self.cfg.overlay_mode && !(self.visible && !self.overlay_view);
        if !has_request {
            self.show_status("Нет запроса для повтора. Сначала задай вопрос или сделай снимок.");
        } else {
            self.idx = self.answers.len() - 1;
            self.retry();
        }
        if overlay {
            self.show_overlay();
        } else if !self.visible {
            self.show_window();
        }
    }

    fn tray_open(&mut self) {
        if self.visible && !self.overlay_view {
            self.hide_window();
        } else if !self.visible && self.last_hide.elapsed() < Duration::from_millis(600) {
            // the click that took focus from the window already hid it: treat this click as "close"
        } else {
            self.mode_free();
        }
    }

    fn tray_settings(&mut self) {
        self.mode_free();
        Timer::single_shot(Duration::from_millis(250), || {
            with_app(|a| {
                if a.visible && !a.ui().get_settings_open() {
                    a.open_settings();
                }
            })
        });
    }

    fn step(&mut self, delta: isize) {
        let next = self.idx as isize + delta;
        if next >= 0 && (next as usize) < self.answers.len() {
            self.idx = next as usize;
            self.render();
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
            .map(|e| {
                let when = chrono::DateTime::from_timestamp_millis(e.t)
                    .map(|d| d.with_timezone(&chrono::Local).format("%d.%m.%Y, %H:%M:%S").to_string())
                    .unwrap_or_default();
                HistItem { title: first_line(&e.q).into(), sub: format!("{when} - {}", first_line(&e.a)).into() }
            })
            .collect();
        self.ui().set_history_items(ModelRc::new(VecModel::from(items)));
    }

    fn toggle_history(&mut self) {
        if self.hist_open {
            return self.hide_hist();
        }
        if !self.close_settings() {
            return;
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
        self.chat = vec![Turn { model: false, text: e.q.clone() }, Turn { model: true, text: e.a.clone() }];
        let cid = self.chat_id;
        self.push_answer(Answer { text: e.a, req: None, chat: Some(cid), is_error: false, record_q: None, question: e.q });
    }

    fn clear_history(&mut self) {
        self.history.clear();
        if self.hist_open {
            self.refresh_history();
        }
        if self.ui().get_settings_open() {
            self.ui().set_saved_text("История очищена".into());
        }
    }

    // ---------- hotkeys ----------

    fn register_hotkeys(&mut self) -> Vec<String> {
        for hk in self.registered.drain(..) {
            let _ = self.hot_mgr.unregister(hk);
        }
        self.hot_ids.clear();
        let keys = self.cfg.hotkeys();
        let mut failed = Vec::new();
        for i in 0..HOTKEY_NAMES.len() {
            let accel = keys.get(i);
            match HotKey::from_str(accel).map_err(|e| e.to_string()).and_then(|hk| self.hot_mgr.register(hk).map(|_| hk).map_err(|e| e.to_string())) {
                Ok(hk) => {
                    self.hot_ids.insert(hk.id(), i);
                    self.registered.push(hk);
                }
                Err(e) => {
                    log_err(&format!("hotkey {accel}: {e}"));
                    failed.push(accel.to_string());
                }
            }
        }
        failed
    }

    fn on_hotkey(&mut self, i: usize) {
        native::suppress_alt_menu();
        match i {
            0 => {
                std::thread::spawn(|| {
                    let text = clip::grab_selection();
                    let _ = slint::invoke_from_event_loop(move || with_app(|a| a.mode_selection(text)));
                });
            }
            1 => self.start_screenshot(),
            3 => self.retry_last(),
            _ if self.cfg.overlay_mode && self.visible && self.overlay_view => self.hide_window(),
            _ => self.mode_free(),
        }
    }

    fn no_key_hint(&self) -> String {
        if self.cfg.is_groq() { "Добавь ключ Groq API в настройках".into() } else { "Добавь ключ Gemini API в настройках".into() }
    }

    fn mode_free(&mut self) {
        self.new_chat();
        let label = if self.cfg.has_key() { "Задай любой вопрос".to_string() } else { self.no_key_hint() };
        if self.show_composer(&label, Composer::Free, "Задай вопрос", false) {
            self.show_window();
        }
    }

    fn mode_selection(&mut self, text: String) {
        if self.cfg.overlay_mode {
            if text.is_empty() {
                self.show_status("Не удалось скопировать выделение. Проверь, что приложение разрешает Ctrl+C.");
            } else {
                self.new_chat();
                let cid = self.chat_id;
                self.send(Request { text: text.clone(), ..Default::default() }, Some(text), Some(cid));
            }
            self.show_overlay();
            return;
        }
        if text.is_empty() {
            self.show_status("Не удалось скопировать текст. Проверь выделение и разрешение Ctrl+C.");
        } else {
            let preview: String = if text.chars().count() > 150 { text.chars().take(150).collect::<String>() + "…" } else { text.clone() };
            if !self.show_composer(&format!("Выделено: {preview}"), Composer::Selection(text), "Напиши запрос", false) {
                return;
            }
        }
        self.show_window();
    }

    /// Game-overlay screenshot: grab the whole monitor under the cursor without any UI of ours,
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
        match image {
            Some(b64) => {
                self.new_chat();
                let cid = self.chat_id;
                self.send(Request { images: vec![b64], ..Default::default() }, Some("[скриншот]".into()), Some(cid));
            }
            None => self.show_status("Снимок не удался."),
        }
        self.show_overlay();
    }

    fn start_screenshot(&mut self) {
        if self.cfg.overlay_mode {
            return self.start_overlay_screenshot();
        }
        let my = self.snip_token.fetch_add(1, Ordering::SeqCst) + 1;
        let was_visible = self.visible;
        if was_visible {
            self.hide_window();
        }
        let token = self.snip_token.clone();
        let full = self.cfg.full_screenshot;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            let image = if full {
                // our window is hidden by now, so it is not in the picture
                native::capture_cursor_monitor().and_then(|(w, h, px)| clip::encode_rgba(w, h, px))
            } else {
                clip::snip(&token, my)
            };
            let _ = slint::invoke_from_event_loop(move || with_app(|a| a.finish_screenshot(image, my, was_visible)));
        });
    }

    fn finish_screenshot(&mut self, image: Option<String>, my: u64, was_visible: bool) {
        if self.snip_token.load(Ordering::SeqCst) != my {
            return;
        }
        match image {
            Some(img) => {
                self.pending_shots.push(img);
                let extra = self.pending_shots.len().saturating_sub(MAX_SHOTS);
                self.pending_shots.drain(..extra);
                let shots = self.pending_shots.clone();
                let label = if shots.len() == 1 { "Снимок экрана готов".to_string() } else { format!("Снимков экрана: {}", shots.len()) };
                if self.show_composer(&label, Composer::Shot(shots), "Задай вопрос", true) {
                    self.show_window();
                }
            }
            None if self.cfg.full_screenshot => {
                self.show_status("Снимок экрана не удался.");
                self.show_window();
            }
            None if was_visible => self.show_window(),
            None => {}
        }
    }

    // ---------- pasted images ----------

    fn paste_image(&mut self) -> bool {
        let Some((b64, (w, h, rgba))) = clip::clipboard_image() else { return false };
        if self.pending.len() < MAX_SHOTS {
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

    // ---------- settings ----------

    fn update_provider_ui(&self) {
        let ui = self.ui();
        let groq = self.active_provider == 1;
        let saved = if groq { !self.cfg.groq_key.is_empty() } else { !self.cfg.gemini_key.is_empty() };
        ui.set_api_key_label(if groq { "Ключ API Groq" } else { "Ключ API Gemini" }.into());
        ui.set_api_key_placeholder(
            if saved { "Ключ сохранён (введи новый, чтобы заменить)" } else if groq { "Вставь ключ Groq API" } else { "Вставь ключ Google AI Studio" }.into(),
        );
        ui.set_api_key_link_text(if groq { "Открыть Groq Console" } else { "Открыть Google AI Studio" }.into());
    }

    fn set_hotkey_models(&self) {
        let ui = self.ui();
        ui.set_hotkey_labels(ModelRc::new(VecModel::from(HOTKEY_LABELS.iter().map(|s| (*s).into()).collect::<Vec<slint::SharedString>>())));
        let values: Vec<slint::SharedString> = (0..HOTKEY_LABELS.len()).map(|i| self.draft_hot.get(i).into()).collect();
        ui.set_hotkey_values(ModelRc::new(VecModel::from(values)));
    }

    fn fill_settings(&mut self) {
        let ui = self.ui();
        self.active_provider = if self.cfg.is_groq() { 1 } else { 0 };
        ui.set_provider_index(self.active_provider as i32);
        let model = if self.cfg.model.trim().is_empty() { GEMINI_IDS[0] } else { self.cfg.model.trim() };
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
        self.key_drafts = [String::new(), String::new()];
        ui.set_api_key("".into());
        self.update_provider_ui();
        ui.set_opacity_pct((self.cfg.opacity() * 100.0).round());
        ui.set_autostart(self.cfg.autostart);
        ui.set_history_enabled(self.cfg.history_on());
        ui.set_overlay_mode(self.cfg.overlay_mode);
        ui.set_full_screenshot(self.cfg.full_screenshot);
        let prompt = if self.cfg.prompt.trim().is_empty() { llm::DEFAULT_PROMPT } else { self.cfg.prompt.as_str() };
        ui.set_prompt_text(prompt.into());
        self.clear_key_check();
        self.draft_hot = self.cfg.hotkeys();
        self.set_hotkey_models();
        ui.set_saved_text("".into());
    }

    fn toggle_settings(&mut self) {
        if self.ui().get_settings_open() {
            self.close_settings();
        } else {
            self.open_settings();
        }
    }

    fn open_settings(&mut self) {
        if self.prev_bounds.is_some() {
            return;
        }
        self.hide_hist();
        let ui = self.ui();
        let cur = self.window_rect();
        self.prev_bounds = Some(cur);
        let scale = ui.window().scale_factor();
        let wa = native::work_area(self.hwnd());
        let w = ((SETTINGS_SIZE.0 * scale) as i32).min(wa.2);
        let h = ((SETTINGS_SIZE.1 * scale) as i32).min(wa.3);
        ui.set_min_w((620.0f32).min(wa.2 as f32 / scale));
        ui.set_min_h((500.0f32).min(wa.3 as f32 / scale));
        let (cw, ch) = (cur.w as i32, cur.h as i32);
        let mut x = if cur.x + cw / 2 > wa.0 + wa.2 / 2 { cur.x + cw - w } else { cur.x };
        let mut y = if cur.y + ch / 2 > wa.1 + wa.3 / 2 { cur.y + ch - h } else { cur.y };
        x = x.min(wa.0 + wa.2 - w).max(wa.0);
        y = y.min(wa.1 + wa.3 - h).max(wa.1);
        self.apply_rect(Rect { x, y, w: w as u32, h: h as u32 });
        self.fill_settings();
        ui.set_settings_open(true);
    }

    fn finish_settings_close(&mut self) {
        let ui = self.ui();
        ui.set_settings_open(false);
        ui.set_min_w(280.0);
        ui.set_min_h(120.0);
        if let Some(b) = self.prev_bounds.take() {
            self.apply_rect(b);
        }
        if self.composer.is_some() {
            self.focus_input_soon();
        }
    }

    /// Saves and closes the settings panel; returns false (panel stays open) if saving failed.
    fn close_settings(&mut self) -> bool {
        if !self.ui().get_settings_open() {
            return true;
        }
        if !self.save_settings() {
            return false;
        }
        self.finish_settings_close();
        true
    }

    fn save_settings(&mut self) -> bool {
        let ui = self.ui();
        let fail = |msg: &str| {
            ui.set_saved_text(msg.into());
            false
        };
        self.key_drafts[self.active_provider] = ui.get_api_key().to_string();
        let groq = ui.get_provider_index() == 1;
        let model = if groq { GROQ_MODEL.to_string() } else { self.selected_gemini_model() };
        // the built-in prompt is stored as "default", so later versions of it still apply
        let prompt = ui.get_prompt_text().trim().to_string();
        let prompt = if prompt == llm::DEFAULT_PROMPT.trim() { String::new() } else { prompt };
        if !groq && model.is_empty() {
            return fail("Введи ID своей модели Gemini");
        }
        let input = SettingsInput {
            provider: if groq { "groq" } else { "gemini" }.into(),
            gemini_key: self.key_drafts[0].trim().to_string(),
            groq_key: self.key_drafts[1].trim().to_string(),
            model,
            hotkeys: self.draft_hot.clone(),
            history: ui.get_history_enabled(),
            autostart: ui.get_autostart(),
            accent: self.cfg.accent().to_string(),
            opacity: ui.get_opacity_pct().round() as i32,
            prompt,
        };
        if let Err(e) = config::validate(&input) {
            return fail(&e);
        }

        let previous = self.cfg.clone();
        if !input.gemini_key.is_empty() {
            self.cfg.gemini_key = input.gemini_key.clone();
        }
        if !input.groq_key.is_empty() {
            self.cfg.groq_key = input.groq_key.clone();
        }
        self.cfg.provider = input.provider.clone();
        if !groq {
            self.cfg.model = input.model.clone();
        }
        self.cfg.hotkeys = Some(input.hotkeys.clone());
        self.cfg.history = Some(input.history);
        self.cfg.autostart = input.autostart;
        self.cfg.overlay_mode = ui.get_overlay_mode();
        self.cfg.full_screenshot = ui.get_full_screenshot();
        self.cfg.opacity = Some(input.opacity as f32 / 100.0);
        self.cfg.prompt = input.prompt.clone();
        if !config::save(&self.store, &self.cfg) {
            self.cfg = previous;
            return fail("Не удалось сохранить настройки. Проверь доступ к папке данных.");
        }
        if let Err(e) = native::set_autostart(input.autostart) {
            log_err(&format!("autostart: {e}"));
        }
        ui.set_win_opacity(self.cfg.opacity());
        self.key_drafts = [String::new(), String::new()];
        ui.set_api_key("".into());
        self.active_provider = ui.get_provider_index() as usize;
        self.update_provider_ui();

        let failed = self.register_hotkeys();
        if !failed.is_empty() {
            return fail(&format!("Клавиши заняты другой программой или неверны: {}", failed.join(", ")));
        }
        ui.set_saved_text("Сохранено".into());
        true
    }

    fn selected_gemini_model(&self) -> String {
        let ui = self.ui();
        match ui.get_model_index() {
            CUSTOM_MODEL_INDEX => ui.get_model_custom().trim().to_string(),
            i => GEMINI_IDS.get(i as usize).copied().unwrap_or(GEMINI_IDS[0]).to_string(),
        }
    }

    fn clear_key_check(&mut self) {
        self.key_check_id += 1; // a check still running for the old provider must not report back
        self.ui().set_key_check_text("".into());
    }

    /// Checks the key typed in the field (or the saved one) against the selected provider.
    fn check_key(&mut self) {
        let ui = self.ui();
        let groq = self.active_provider == 1;
        let typed = ui.get_api_key().trim().to_string();
        let key = if !typed.is_empty() {
            typed
        } else if groq {
            self.cfg.groq_key.clone()
        } else {
            self.cfg.gemini_key.clone()
        };
        let model = if groq { String::new() } else { self.selected_gemini_model() };
        self.key_check_id += 1;
        let id = self.key_check_id;
        ui.set_key_check_ok(true);
        ui.set_key_check_text("Проверяю…".into());
        std::thread::spawn(move || {
            let res = llm::check_key(groq, &key, &model);
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|a| {
                    if a.key_check_id != id {
                        return;
                    }
                    let ui = a.ui();
                    ui.set_key_check_ok(res.is_ok());
                    let text = match res {
                        Ok(t) => format!("✓ {t}"),
                        Err(e) => format!("✗ {e}"),
                    };
                    ui.set_key_check_text(text.into());
                })
            });
        });
    }

    fn prompt_preset(&self, i: usize) {
        if let Some(p) = llm::PRESETS.get(i) {
            self.ui().set_prompt_text((*p).into());
        }
    }

    fn provider_changed(&mut self, idx: usize) {
        self.clear_key_check();
        let ui = self.ui();
        self.key_drafts[self.active_provider] = ui.get_api_key().to_string();
        self.active_provider = idx.min(1);
        ui.set_api_key(self.key_drafts[self.active_provider].as_str().into());
        self.update_provider_ui();
    }

    fn hotkey_key(&mut self, i: usize, text: &str, ctrl: bool, alt: bool, shift: bool, meta: bool) {
        let Some(key) = key_name(text) else { return };
        let is_f = key.starts_with('F') && key[1..].chars().all(|c| c.is_ascii_digit()) && key.len() > 1;
        if !(ctrl || alt || shift || meta) && !is_f {
            return;
        }
        let mut parts = Vec::new();
        if ctrl { parts.push("Ctrl".to_string()); }
        if alt { parts.push("Alt".to_string()); }
        if shift { parts.push("Shift".to_string()); }
        if meta { parts.push("Super".to_string()); }
        parts.push(key);
        self.draft_hot.set(i, parts.join("+"));
        self.set_hotkey_models();
    }

    fn set_theme(&mut self, dark: bool) {
        self.cfg.theme = if dark { "dark" } else { "light" }.into();
        self.ui().global::<Theme>().set_dark(dark);
        self.ui().invoke_sync_palette();
        config::save(&self.store, &self.cfg);
    }

    fn pick_accent(&mut self, name: &str) {
        if ACCENTS.contains(&name) {
            self.cfg.accent = name.to_string();
            self.ui().global::<Theme>().set_accent_name(name.into());
            config::save(&self.store, &self.cfg);
        }
    }
}

fn main() {
    std::panic::set_hook(Box::new(|info| log_err(&format!("panic: {info}"))));
    if !native::acquire_single_instance() {
        return;
    }
    let store = match crypto::Store::open(&config::data_dir()) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            log_err(&format!("storage: {e}"));
            return;
        }
    };
    #[allow(unused_mut)]
    let mut cfg = config::load(&store);
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
    }

    // Windows are created "inactive": showing the overlay must not steal focus from a game.
    // The regular window takes focus explicitly (see force_focus).
    slint::BackendSelector::new()
        .backend_name("winit".into())
        // no taskbar button: the app lives in the tray
        .with_winit_window_attributes_hook(|attrs| attrs.with_active(false).with_skip_taskbar(true))
        .select()
        .expect("select winit backend");

    let ui = MainWindow::new().expect("create window");
    {
        let theme = ui.global::<Theme>();
        theme.set_dark(cfg.dark());
        theme.set_accent_name(cfg.accent().into());
    }
    ui.invoke_sync_palette();
    ui.set_win_opacity(cfg.opacity());

    let hot_mgr = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => {
            log_err(&format!("hotkey manager: {e}"));
            return;
        }
    };
    let first_run = !cfg.has_key();
    let app = Rc::new(RefCell::new(App {
        ui: ui.as_weak(),
        history: History::new(store.clone()),
        store,
        draft_hot: cfg.hotkeys(),
        cfg,
        answers: Vec::new(),
        idx: 0,
        req_id: 0,
        live_req: Arc::new(AtomicU64::new(0)),
        busy: false,
        streaming: String::new(),
        in_flight: None,
        key_check_id: 0,
        chat_id: 0,
        chat: Vec::new(),
        composer: None,
        pending: Vec::new(),
        pending_shots: Vec::new(),
        visible: false,
        overlay_view: false,
        auto_hide: None,
        last_hide: Instant::now(),
        tick_timer: Timer::default(),
        placed: false,
        pinned: false,
        hist_open: false,
        shown_at: Instant::now(),
        prev_bounds: None,
        watched: None,
        drag: None,
        snip_token: Arc::new(AtomicU64::new(0)),
        hot_mgr,
        registered: Vec::new(),
        hot_ids: HashMap::new(),
        key_drafts: [String::new(), String::new()],
        active_provider: 0,
    }));
    APP.with(|a| *a.borrow_mut() = Some(app.clone()));

    // ---- UI callbacks ----
    ui.on_submit(|text| with_app(|a| a.submit(text.to_string())));
    ui.on_prev(|| with_app(|a| a.step(-1)));
    ui.on_next(|| with_app(|a| a.step(1)));
    ui.on_retry(|| with_app(|a| a.retry()));
    ui.on_stop(|| with_app(|a| a.stop()));
    ui.on_check_key(|| with_app(|a| a.check_key()));
    ui.on_prompt_preset(|i| with_app(|a| a.prompt_preset(i.max(0) as usize)));
    ui.on_copy({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            clip::set_text(&ui.get_body_text());
            ui.set_copy_done(true);
            let w2 = weak.clone();
            Timer::single_shot(Duration::from_millis(1200), move || {
                if let Some(ui) = w2.upgrade() {
                    ui.set_copy_done(false);
                }
            });
        }
    });
    ui.on_toggle_history(|| with_app(|a| a.toggle_history()));
    ui.on_clear_history(|| with_app(|a| a.clear_history()));
    ui.on_toggle_settings(|| with_app(|a| a.toggle_settings()));
    ui.on_toggle_pin({
        let weak = ui.as_weak();
        move || {
            with_app(|a| a.pinned = !a.pinned);
            let pinned = APP.with(|a| a.borrow().as_ref().map(|r| r.borrow().pinned).unwrap_or(false));
            if let Some(ui) = weak.upgrade() {
                ui.set_pinned(pinned);
            }
        }
    });
    ui.on_hide_window(|| with_app(|a| if a.close_settings() { a.hide_window() }));
    ui.on_escape_pressed(|| with_app(|a| if a.close_settings() { a.hide_window() }));
    // Moving/resizing is done by following the cursor ourselves rather than via the native
    // modal loop, which swallows the mouse-up and leaves the toolkit's buttons unresponsive.
    ui.on_drag_start(|| with_app(|a| a.drag_start(false)));
    ui.on_drag_move(|| with_app(|a| a.drag_move()));
    ui.on_resize_start(|| with_app(|a| a.drag_start(true)));
    ui.on_resize_move(|| with_app(|a| a.drag_move()));
    ui.on_paste_image(|| {
        let mut handled = false;
        with_app(|a| handled = a.paste_image());
        handled
    });
    ui.on_remove_image(|i| with_app(|a| a.remove_image(i.max(0) as usize)));
    ui.on_history_selected(|i| with_app(|a| a.history_selected(i.max(0) as usize)));
    ui.on_provider_changed(|i| with_app(|a| a.provider_changed(i.max(0) as usize)));
    ui.on_opacity_changed({
        let weak = ui.as_weak();
        move |v| {
            if let Some(ui) = weak.upgrade() {
                ui.set_win_opacity((v / 100.0).clamp(config::MIN_OPACITY, 1.0));
            }
        }
    });
    ui.on_toggle_theme(|| with_app(|a| {
        let dark = a.cfg.dark();
        a.set_theme(!dark);
    }));
    ui.on_accent_picked(|name| with_app(|a| a.pick_accent(&name)));
    ui.on_hotkey_key(|i, t, c, al, s, m| with_app(|a| a.hotkey_key(i.clamp(0, 3) as usize, &t, c, al, s, m)));
    ui.on_hotkey_cancelled({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.invoke_focus_root();
            }
        }
    });
    ui.on_reset_hotkeys(|| with_app(|a| {
        a.draft_hot = Hotkeys::default();
        a.set_hotkey_models();
    }));
    ui.on_open_key_link(|| with_app(|a| {
        native::open_url(if a.active_provider == 1 { "https://console.groq.com/keys" } else { "https://aistudio.google.com/app/apikey" });
    }));
    ui.on_save_settings(|| with_app(|a| {
        a.close_settings();
    }));
    ui.on_exit_app(|| {
        with_app(|a| {
            if a.close_settings() {
                let _ = slint::quit_event_loop();
            }
        });
    });
    ui.window().on_close_requested(|| {
        with_app(|a| {
            a.visible = false;
            a.watched = None;
            a.tick_timer.stop();
        });
        slint::CloseRequestResponse::HideWindow
    });

    // ---- hotkeys + periodic work ----
    let failed = app.borrow_mut().register_hotkeys();
    if !failed.is_empty() {
        log_err(&format!("hotkeys not registered: {}", failed.join(", ")));
    }
    // ---- tray icon (the app has no taskbar button) ----
    let open_item = MenuItem::new("Открыть", true, None);
    let settings_item = MenuItem::new("Настройки", true, None);
    let quit_item = MenuItem::new("Выход", true, None);
    let menu = Menu::new();
    let _ = menu.append_items(&[&open_item, &settings_item, &PredefinedMenuItem::separator(), &quit_item]);
    let tray_image = image::load_from_memory(include_bytes!("../ui/icon.png"))
        .map(|i| image::imageops::resize(&i.to_rgba8(), 32, 32, image::imageops::FilterType::Lanczos3))
        .expect("embedded icon decodes");
    let _tray = tray_icon::Icon::from_rgba(tray_image.into_raw(), 32, 32)
        .map_err(|e| e.to_string())
        .and_then(|icon| {
            TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_tooltip("Nortex Assist")
                .with_icon(icon)
                .build()
                .map_err(|e| e.to_string())
        })
        .map_err(|e| log_err(&format!("tray icon: {e}")))
        .ok();
    let (open_id, settings_id, quit_id) = (open_item.id().clone(), settings_item.id().clone(), quit_item.id().clone());

    // Hotkey and tray events arrive on the UI thread through the Win32 message loop; forward each
    // one to the Slint event loop instead of polling, so the idle app does not wake up at all.
    GlobalHotKeyEvent::set_event_handler(Some(|ev: GlobalHotKeyEvent| {
        if ev.state == HotKeyState::Pressed {
            let id = ev.id;
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|a| {
                    if let Some(&i) = a.hot_ids.get(&id) {
                        a.on_hotkey(i);
                    }
                })
            });
        }
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

    if first_run {
        // on first launch show the window so the user can enter a key
        let mut a = app.borrow_mut();
        let hint = a.no_key_hint();
        a.show_status(&hint);
        a.show_window();
        Timer::single_shot(Duration::from_millis(200), || with_app(|a| a.toggle_settings()));
    }

    slint::run_event_loop_until_quit().expect("event loop");
}
