//! Persistent settings (encrypted on disk) and validation of values coming from the UI.

use crate::crypto::{self, Store};
use crate::logging::log_err;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// "custom" is the user's own colour, kept in `accent_custom`.
pub const ACCENTS: [&str; 8] = ["violet", "blue", "green", "pink", "orange", "red", "graphite", "custom"];
pub const DEFAULT_CUSTOM: (u8, u8, u8) = (0x8f, 0x80, 0xf7);
pub const GROQ_MODEL: &str = "qwen/qwen3.8-27b";
/// Lowest window opacity (10 %): below this the window is too easy to lose.
pub const MIN_OPACITY: f32 = 0.10;
/// Text sizes (px) offered for the game-overlay window; the window itself fits the text.
pub const OVERLAY_FONTS: [u32; 8] = [12, 14, 16, 18, 20, 24, 28, 32];
pub const DEFAULT_OVERLAY_FONT: u32 = 16;
/// Answer text sizes (percent) the A− / A+ buttons step through.
pub const TEXT_SCALES: [u32; 6] = [85, 100, 115, 130, 150, 175];
pub const MAX_PROMPT: usize = 8000;
pub const SHOT_MODES: [&str; 3] = ["select", "full", "snip"];
/// Hotkey slots (indices into `Hotkeys`).
pub const HK_ASK: usize = 0;
pub const HK_SHOT: usize = 1;
pub const HK_CHAT: usize = 2;
pub const HK_RETRY: usize = 3;
pub const HK_VOICE: usize = 4;
pub const HK_NOTE: usize = 5;
/// "Одна клавиша на всё": the selection or the screen, asked at once.
pub const HK_SMART: usize = 6;
pub const DEFAULT_VOICE_KEY: &str = "F8";

// `default` keeps configs saved before the newer hotkeys existed readable.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Hotkeys {
    /// "Спросить": the selection (smart mode: or a screenshot when nothing is selected)
    pub selection: String,
    pub screenshot: String,
    /// free question / the chat window
    pub free: String,
    pub retry: String,
    /// hold to talk; one key without modifiers by default
    pub voice: String,
    /// smart mode: like "ask", but the window waits for a note before sending
    pub note: String,
    /// smart mode: the one key (selection or screen, asked at once)
    pub ask: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Hotkeys {
            selection: "Alt+Q".into(),
            screenshot: "Alt+W".into(),
            free: "Alt+E".into(),
            retry: "Alt+S".into(),
            voice: DEFAULT_VOICE_KEY.into(),
            note: "Alt+Shift+W".into(),
            ask: "Alt+W".into(),
        }
    }
}

impl Hotkeys {
    pub fn get(&self, i: usize) -> &str {
        match i {
            0 => &self.selection,
            1 => &self.screenshot,
            2 => &self.free,
            3 => &self.retry,
            4 => &self.voice,
            5 => &self.note,
            _ => &self.ask,
        }
    }
    pub fn set(&mut self, i: usize, v: String) {
        match i {
            0 => self.selection = v,
            1 => self.screenshot = v,
            2 => self.free = v,
            3 => self.retry = v,
            4 => self.voice = v,
            5 => self.note = v,
            _ => self.ask = v,
        }
    }
    /// The slots that are registered in a key mode.
    pub fn active(smart: bool) -> Vec<usize> {
        if smart {
            vec![HK_SMART, HK_CHAT, HK_NOTE, HK_RETRY, HK_VOICE]
        } else {
            vec![HK_ASK, HK_SHOT, HK_CHAT, HK_RETRY, HK_VOICE]
        }
    }
}

/// A ready-made follow-up under the answer ("Объясни", "Короче"...): the button text and what is sent.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct QuickAction {
    pub label: String,
    pub prompt: String,
}

/// The buttons under an answer when the user has not changed them. The last one ("Другой
/// вариант") is special: it asks the same question again and marks the answer as not good enough.
pub fn default_actions() -> Vec<QuickAction> {
    let t = crate::i18n::tr;
    vec![
        QuickAction { label: t("Объясни"), prompt: t("Объясни этот ответ: почему так и как к нему прийти. Коротко, по шагам.") },
        QuickAction { label: t("Короче"), prompt: t("Сократи ответ до самого главного.") },
        QuickAction { label: t("Переведи"), prompt: t("Переведи ответ на английский язык (если он уже на английском — на русский).") },
    ]
}

pub const MAX_ACTIONS: usize = 3;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Config {
    pub gemini_key: String,
    pub groq_key: String,
    pub provider: String,
    pub model: String,
    pub hotkeys: Option<Hotkeys>,
    pub history: Option<bool>,
    pub autostart: bool,
    /// Game-overlay mode: answers appear in a small click-through window that never takes focus.
    pub overlay_mode: bool,
    /// Text size of the game-overlay window in px (one of OVERLAY_FONTS); None = 16.
    pub overlay_font: Option<u32>,
    /// Screenshot hotkey grabs the whole monitor at once instead of opening the Windows snipping tool.
    pub full_screenshot: bool,
    /// Screenshot hotkey: "select" (own area picker), "full" (whole monitor) or "snip" (Windows snipping tool).
    pub shot_mode: String,
    /// Automatic update checks are on unless this is set.
    pub no_update_check: bool,
    /// The tray icon is shown unless this is set.
    pub hide_tray: bool,
    pub theme: String,
    pub accent: String,
    /// Own accent colour as "#RRGGBB" (used when `accent` is "custom").
    pub accent_custom: String,
    pub opacity: Option<f32>,
    pub bounds: Option<Rect>,
    /// Own system prompt; empty means the built-in one.
    pub prompt: String,
    /// "smart" (one key does it all, the default for new installs) or "classic" (separate keys,
    /// as in 1.3); empty in configs from before 1.4.
    pub key_mode: String,
    /// The offer of the smart key mode to users who updated from 1.3: "" none, "pending", "done".
    pub smart_offer: String,
    /// The version whose "what's new" the user has seen (shown once after an update).
    pub seen_version: String,
    /// Own set of buttons under the answer; None = the default ones.
    pub actions: Option<Vec<QuickAction>>,
    /// "Расширенные" is switched on in the settings.
    pub advanced: bool,
    /// Size of the answer text in percent (one of TEXT_SCALES); None = 100.
    pub text_scale: Option<u32>,
    /// Interface language: "en" for English, anything else is Russian.
    pub lang: String,
}

impl Config {
    pub fn is_groq(&self) -> bool {
        self.provider == "groq"
    }
    pub fn hotkeys(&self) -> Hotkeys {
        self.hotkeys.clone().unwrap_or_default()
    }
    pub fn history_on(&self) -> bool {
        self.history != Some(false)
    }
    pub fn opacity(&self) -> f32 {
        self.opacity.filter(|o| o.is_finite()).map(|o| o.clamp(MIN_OPACITY, 1.0)).unwrap_or(1.0)
    }
    pub fn overlay_font(&self) -> u32 {
        self.overlay_font.filter(|f| OVERLAY_FONTS.contains(f)).unwrap_or(DEFAULT_OVERLAY_FONT)
    }
    pub fn overlay_font_index(&self) -> usize {
        OVERLAY_FONTS.iter().position(|&f| f == self.overlay_font()).unwrap_or(2)
    }
    pub fn is_en(&self) -> bool {
        self.lang == "en"
    }
    pub fn smart_keys(&self) -> bool {
        self.key_mode != "classic"
    }
    pub fn actions(&self) -> Vec<QuickAction> {
        match &self.actions {
            Some(a) if !a.is_empty() => a.iter().take(MAX_ACTIONS).cloned().collect(),
            _ => default_actions(),
        }
    }
    /// "system" (follow Windows, the default), "dark" or "light".
    pub fn theme_mode(&self) -> &str {
        match self.theme.as_str() {
            "dark" => "dark",
            "light" => "light",
            _ => "system",
        }
    }
    pub fn text_scale(&self) -> u32 {
        self.text_scale.filter(|s| TEXT_SCALES.contains(s)).unwrap_or(100)
    }
    pub fn dark(&self) -> bool {
        match self.theme_mode() {
            "dark" => true,
            "light" => false,
            _ => crate::native::windows_dark(),
        }
    }
    pub fn accent(&self) -> &str {
        if ACCENTS.contains(&self.accent.as_str()) { &self.accent } else { "violet" }
    }
    pub fn custom_rgb(&self) -> (u8, u8, u8) {
        parse_color(&self.accent_custom).unwrap_or(DEFAULT_CUSTOM)
    }
    /// 0 = own area picker, 1 = whole monitor, 2 = Windows snipping tool.
    pub fn shot_mode_index(&self) -> usize {
        match self.shot_mode.as_str() {
            "select" => 0,
            "full" => 1,
            "snip" => 2,
            // configs from before the choice existed
            _ => if self.full_screenshot { 1 } else { 0 },
        }
    }

    pub fn has_key(&self) -> bool {
        if self.is_groq() { !self.groq_key.is_empty() } else { !self.gemini_key.is_empty() }
    }
}

/// Reads a colour typed by the user: "#4FD1C5", "4fd1c5", "#4FD", "79, 209, 197", "79 209 197" or "rgb(79,209,197)".
pub fn parse_color(text: &str) -> Option<(u8, u8, u8)> {
    let t = text.trim().to_lowercase();
    let t = t.strip_prefix("rgb").map(|r| r.trim().trim_start_matches('(').trim_end_matches(')').to_string()).unwrap_or(t);
    let parts: Vec<&str> = t.split(|c: char| c == ',' || c == ';' || c.is_whitespace()).filter(|p| !p.is_empty()).collect();
    if parts.len() == 3 {
        let mut v = [0u8; 3];
        for (i, p) in parts.iter().enumerate() {
            v[i] = p.parse::<u8>().ok()?;
        }
        return Some((v[0], v[1], v[2]));
    }
    let hex = t.trim_start_matches('#');
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |s: &str| u8::from_str_radix(s, 16).ok();
    match hex.len() {
        6 => Some((byte(&hex[0..2])?, byte(&hex[2..4])?, byte(&hex[4..6])?)),
        3 => {
            let d = |i: usize| byte(&hex[i..i + 1]).map(|x| x * 17);
            Some((d(0)?, d(1)?, d(2)?))
        }
        _ => None,
    }
}

pub fn hex_color((r, g, b): (u8, u8, u8)) -> String {
    format!("#{r:02X}{g:02X}{b:02X}")
}

/// The real data folder of the installed program.
pub fn real_data_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("NortexAssist")
}

/// Test copy: the API keys (and the chosen service and model) from the user's real settings,
/// read without changing them.
#[cfg(feature = "test-copy")]
pub fn import_real_keys(cfg: &mut Config) {
    let dir = real_data_dir();
    let Some(store) = crate::crypto::Store::open_read_only(&dir) else { return };
    let Ok(raw) = std::fs::read(dir.join("config.bin")) else { return };
    let plain = if crypto::is_encrypted(&raw) { store.decrypt(&raw).ok() } else { Some(raw) };
    let Some(real) = plain.and_then(|p| serde_json::from_slice::<Config>(&p).ok()) else { return };
    cfg.gemini_key = real.gemini_key;
    cfg.groq_key = real.groq_key;
    cfg.provider = real.provider;
    cfg.model = real.model;
}

pub fn data_dir() -> PathBuf {
    // the test copy keeps its own settings and history next to its exe
    #[cfg(feature = "test-copy")]
    {
        let d = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join("test-data"))).unwrap_or_else(|| PathBuf::from("test-data"));
        let _ = std::fs::create_dir_all(&d);
        return d;
    }
    // debug builds can use an isolated data folder for tests
    #[cfg(debug_assertions)]
    if let Ok(custom) = std::env::var("NORTEX_DATA_DIR") {
        let d = PathBuf::from(custom);
        let _ = std::fs::create_dir_all(&d);
        return d;
    }
    #[allow(unreachable_code)]
    {
        let d = real_data_dir();
        let _ = std::fs::create_dir_all(&d);
        d
    }
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.bin")
}

/// False before the very first start (no settings saved yet).
pub fn exists() -> bool {
    config_path().exists()
}

pub fn load(store: &Store) -> Config {
    let path = config_path();
    let raw = match std::fs::read(&path) {
        Ok(r) => r,
        Err(_) => return Config::default(),
    };
    let parsed = if crypto::is_encrypted(&raw) {
        store.decrypt(&raw).and_then(|p| serde_json::from_slice::<Config>(&p).map_err(|e| e.to_string()))
    } else {
        serde_json::from_slice::<Config>(&raw).map_err(|e| e.to_string())
    };
    match parsed {
        Ok(c) => c,
        Err(e) => {
            log_err(&format!("config load: {e}"));
            crypto::backup(&path); // do not silently overwrite what could not be read
            Config::default()
        }
    }
}

pub fn save(store: &Store, cfg: &Config) -> bool {
    let write = || -> Result<(), String> {
        let json = serde_json::to_vec(cfg).map_err(|e| e.to_string())?;
        let blob = store.encrypt(&json)?;
        let path = config_path();
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, blob).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    };
    match write() {
        Ok(()) => true,
        Err(e) => {
            log_err(&format!("config save: {e}"));
            false
        }
    }
}

/// An API key as pasted: a single word of reasonable length.
pub fn key_looks_valid(k: &str) -> bool {
    let k = k.trim();
    (10..=500).contains(&k.chars().count()) && !k.chars().any(char::is_whitespace)
}

pub fn valid_gemini_model(m: &str) -> bool {
    Regex::new(r"(?i)^gemini-[a-z0-9.\-]+$").unwrap().is_match(m.trim())
}

/// "Alt+Q", "Ctrl+Shift+F5", "F8", "Numpad5"...
pub fn valid_accel(a: &str) -> bool {
    Regex::new(r"^(?:(?:Ctrl|Alt|Shift|Super)\+){0,4}[\w`\-=,./;'\[\]]{1,20}$").unwrap().is_match(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(parse_color("#4FD1C5"), Some((0x4f, 0xd1, 0xc5)));
        assert_eq!(parse_color(" 4fd1c5 "), Some((0x4f, 0xd1, 0xc5)));
        assert_eq!(parse_color("#fff"), Some((255, 255, 255)));
        assert_eq!(parse_color("79, 209, 197"), Some((79, 209, 197)));
        assert_eq!(parse_color("rgb(79 209 197)"), Some((79, 209, 197)));
        assert_eq!(parse_color("300, 0, 0"), None);
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("зелёный"), None);
        assert_eq!(hex_color((79, 209, 197)), "#4FD1C5");
    }
}
