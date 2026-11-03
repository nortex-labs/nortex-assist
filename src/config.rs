//! Persistent settings (encrypted on disk) and validation of values coming from the UI.

use crate::crypto::{self, Store};
use crate::logging::log_err;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// "custom" is the user's own colour, kept in `accent_custom`.
pub const ACCENTS: [&str; 7] = ["green", "pink", "blue", "red", "orange", "violet", "custom"];
pub const DEFAULT_CUSTOM: (u8, u8, u8) = (0x4f, 0xd1, 0xc5);
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
pub const HOTKEY_NAMES: [&str; 4] = ["selection", "screenshot", "free", "retry"];

// `default` keeps configs saved before the retry hotkey existed readable.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Hotkeys {
    pub selection: String,
    pub screenshot: String,
    pub free: String,
    pub retry: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Hotkeys { selection: "Alt+Q".into(), screenshot: "Alt+W".into(), free: "Alt+E".into(), retry: "Alt+S".into() }
    }
}

impl Hotkeys {
    pub fn get(&self, i: usize) -> &str {
        match i {
            0 => &self.selection,
            1 => &self.screenshot,
            2 => &self.free,
            _ => &self.retry,
        }
    }
    pub fn set(&mut self, i: usize, v: String) {
        match i {
            0 => self.selection = v,
            1 => self.screenshot = v,
            2 => self.free = v,
            _ => self.retry = v,
        }
    }
}

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
    /// Short answer first, details behind a button; on unless this is `Some(false)`.
    pub two_level: Option<bool>,
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
    pub fn two_level_on(&self) -> bool {
        self.two_level != Some(false)
    }
    pub fn text_scale(&self) -> u32 {
        self.text_scale.filter(|s| TEXT_SCALES.contains(s)).unwrap_or(100)
    }
    pub fn dark(&self) -> bool {
        self.theme != "light"
    }
    pub fn accent(&self) -> &str {
        if ACCENTS.contains(&self.accent.as_str()) { &self.accent } else { "green" }
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

pub fn data_dir() -> PathBuf {
    // debug builds can use an isolated data folder for tests
    #[cfg(debug_assertions)]
    if let Ok(custom) = std::env::var("NORTEX_DATA_DIR") {
        let d = PathBuf::from(custom);
        let _ = std::fs::create_dir_all(&d);
        return d;
    }
    let d = dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("NortexAssist");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.bin")
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

/// Values the user can change in the settings panel.
pub struct SettingsInput {
    pub provider: String,
    pub gemini_key: String,
    pub groq_key: String,
    pub model: String,
    pub hotkeys: Hotkeys,
    pub history: bool,
    pub autostart: bool,
    pub accent: String,
    pub opacity: i32,
    pub prompt: String,
}

pub fn validate(s: &SettingsInput) -> Result<(), String> {
    if s.provider != "gemini" && s.provider != "groq" {
        return Err(crate::i18n::tr("Неизвестный провайдер модели"));
    }
    for (k, name) in [(&s.gemini_key, "Gemini"), (&s.groq_key, "Groq")] {
        let k = k.trim();
        if !k.is_empty() && (k.chars().count() < 10 || k.chars().count() > 500 || k.chars().any(char::is_whitespace)) {
            return Err(crate::i18n::trf("Ключ {name} выглядит неверно", &[("name", name)]));
        }
    }
    if s.provider == "gemini" && !s.model.is_empty() {
        let re = Regex::new(r"(?i)^gemini-[a-z0-9.\-]+$").unwrap();
        if !re.is_match(&s.model) {
            return Err(crate::i18n::tr("Недопустимое имя модели Gemini"));
        }
    }
    let accel = Regex::new(r"^(?:(?:Ctrl|Alt|Shift|Super)\+){0,4}[\w`\-=,./;'\[\]]{1,20}$").unwrap();
    let mut seen = Vec::new();
    for i in 0..HOTKEY_NAMES.len() {
        let a = s.hotkeys.get(i);
        if !accel.is_match(a) {
            return Err(crate::i18n::trf("Недопустимое сочетание клавиш: {a}", &[("a", &a.chars().take(30).collect::<String>())]));
        }
        if seen.contains(&a) {
            return Err(crate::i18n::tr("Два действия используют одинаковые клавиши"));
        }
        seen.push(a);
    }
    if !ACCENTS.contains(&s.accent.as_str()) {
        return Err(crate::i18n::tr("Неизвестный акцентный цвет"));
    }
    if !((MIN_OPACITY * 100.0) as i32..=100).contains(&s.opacity) {
        return Err(crate::i18n::trf("Прозрачность должна быть от {min} до 100%", &[("min", &((MIN_OPACITY * 100.0) as i32).to_string())]));
    }
    if s.prompt.chars().count() > MAX_PROMPT {
        return Err(crate::i18n::trf("Промпт слишком длинный (больше {max} символов)", &[("max", &MAX_PROMPT.to_string())]));
    }
    Ok(())
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
