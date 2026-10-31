use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

const LOG_MAX: u64 = 512 * 1024;

pub fn log_err(msg: &str) {
    let dir = crate::config::data_dir();
    let file = dir.join("log.txt");
    if std::fs::metadata(&file).map(|m| m.len() > LOG_MAX).unwrap_or(false) {
        let _ = std::fs::rename(&file, dir.join("log.old.txt"));
    }
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(file) {
        let _ = writeln!(f, "[{secs}] {msg}");
    }
}
