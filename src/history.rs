use crate::config::data_dir;
use crate::crypto::{self, Store};
use crate::llm::Request;
use crate::logging::log_err;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX: usize = 300;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Entry {
    pub id: i64,
    pub t: i64,
    pub q: String,
    pub a: String,
}

pub struct History {
    store: Arc<Store>,
    items: Option<Vec<Entry>>,
}

fn path() -> PathBuf {
    data_dir().join("history.bin")
}

impl History {
    pub fn new(store: Arc<Store>) -> Self {
        History { store, items: None }
    }

    fn save(&self) {
        let Some(items) = &self.items else { return };
        let res = serde_json::to_vec(items)
            .map_err(|e| e.to_string())
            .and_then(|j| self.store.encrypt(&j))
            .and_then(|b| std::fs::write(path(), b).map_err(|e| e.to_string()));
        if let Err(e) = res {
            log_err(&format!("history save: {e}"));
        }
    }

    fn load(&mut self) -> &mut Vec<Entry> {
        if self.items.is_none() {
            let p = path();
            let items = match std::fs::read(&p) {
                Err(_) => Vec::new(),
                Ok(raw) => {
                    let parsed = if crypto::is_encrypted(&raw) {
                        self.store.decrypt(&raw).and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
                    } else {
                        Err("history: unknown format".to_string())
                    };
                    parsed.unwrap_or_else(|e| {
                        log_err(&e);
                        crypto::backup(&p);
                        Vec::new()
                    })
                }
            };
            self.items = Some(items);
        }
        self.items.as_mut().unwrap()
    }

    pub fn list(&mut self) -> Vec<Entry> {
        self.load().clone()
    }

    pub fn add(&mut self, enabled: bool, req: &Request, answer: &str) {
        if !enabled {
            return;
        }
        let n = req.images.len();
        let mut q = if n > 0 {
            format!("[скриншот{}]", if n > 1 { format!(" x{n}") } else { String::new() })
        } else {
            req.text.clone()
        };
        if !req.extra.is_empty() {
            q.push_str(" - ");
            q.push_str(&req.extra);
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
        let q: String = q.chars().take(300).collect();
        let list = self.load();
        list.insert(0, Entry { id: now, t: now, q, a: answer.to_string() });
        list.truncate(MAX);
        self.save();
    }

    pub fn clear(&mut self) {
        self.items = Some(Vec::new());
        self.save();
    }
}
