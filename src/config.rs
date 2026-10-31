//! Persistent settings (encrypted on disk) and validation of values coming from the UI.

use crate::crypto::{self, Store};
use crate::logging::log_err;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const ACCENTS: [&str; 6] = ["green", "pink", "blue", "red", "orange", "violet"];
pub const GROQ_MODEL: &str = "qwen/qwen3.8-27b";
/// Lowest window opacity (10 %): below this the window is too easy to lose.
pub const MIN_OPACITY: f32 = 0.10;
pub const MAX_PROMPT: usize = 8000;
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
    /// Screenshot hotkey grabs the whole monitor at once instead of opening the Windows snipping tool.
    pub full_screenshot: bool,
    /// Automatic update checks are on unless this is set.
    pub no_update_check: bool,
    pub theme: String,
    pub accent: String,
    pub opacity: Option<f32>,
    pub bounds: Option<Rect>,
    /// Own system prompt; empty means the built-in one.
    pub prompt: String,
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
    pub fn dark(&self) -> bool {
        self.theme != "light"
    }
    pub fn accent(&self) -> &str {
        if ACCENTS.contains(&self.accent.as_str()) { &self.accent } else { "green" }
    }
    pub fn has_key(&self) -> bool {
        if self.is_groq() { !self.groq_key.is_empty() } else { !self.gemini_key.is_empty() }
    }
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
        return Err("Неизвестный провайдер модели".into());
    }
    for (k, name) in [(&s.gemini_key, "Gemini"), (&s.groq_key, "Groq")] {
        let k = k.trim();
        if !k.is_empty() && (k.chars().count() < 10 || k.chars().count() > 500 || k.chars().any(char::is_whitespace)) {
            return Err(format!("Ключ {name} выглядит неверно"));
        }
    }
    if s.provider == "gemini" && !s.model.is_empty() {
        let re = Regex::new(r"(?i)^gemini-[a-z0-9.\-]+$").unwrap();
        if !re.is_match(&s.model) {
            return Err("Недопустимое имя модели Gemini".into());
        }
    }
    let accel = Regex::new(r"^(?:(?:Ctrl|Alt|Shift|Super)\+){0,4}[\w`\-=,./;'\[\]]{1,20}$").unwrap();
    let mut seen = Vec::new();
    for i in 0..HOTKEY_NAMES.len() {
        let a = s.hotkeys.get(i);
        if !accel.is_match(a) {
            return Err(format!("Недопустимое сочетание клавиш: {}", a.chars().take(30).collect::<String>()));
        }
        if seen.contains(&a) {
            return Err("Два действия используют одинаковые клавиши".into());
        }
        seen.push(a);
    }
    if !ACCENTS.contains(&s.accent.as_str()) {
        return Err("Неизвестный акцентный цвет".into());
    }
    if !((MIN_OPACITY * 100.0) as i32..=100).contains(&s.opacity) {
        return Err(format!("Прозрачность должна быть от {} до 100%", (MIN_OPACITY * 100.0) as i32));
    }
    if s.prompt.chars().count() > MAX_PROMPT {
        return Err(format!("Промпт слишком длинный (больше {MAX_PROMPT} символов)"));
    }
    Ok(())
}
