//! Reads the files and folders picked with the paperclip into text the model can see.

use crate::llm::{Attachment, MAX_FILES, MAX_FILE_CHARS};
use std::path::{Path, PathBuf};

/// Hard limits so one click cannot send a whole disk.
const MAX_BYTES_PER_FILE: u64 = 2_000_000;
const MAX_TOTAL_CHARS: usize = 250_000;
const MAX_DEPTH: usize = 6;
const SKIP_DIRS: [&str; 12] = [
    ".git", "node_modules", "target", "dist", "build", "__pycache__", ".venv", "venv", ".idea", ".vscode", "obj", "bin",
];
const BINARY_EXT: [&str; 36] = [
    "exe", "dll", "so", "o", "obj", "bin", "zip", "rar", "7z", "gz", "tar", "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "png",
    "jpg", "jpeg", "gif", "webp", "bmp", "ico", "mp3", "mp4", "avi", "mkv", "wav", "ttf", "otf", "woff", "woff2", "iso", "lock",
];

#[derive(Default)]
pub struct Loaded {
    pub files: Vec<Attachment>,
    /// what was left out and why, one short line each
    pub skipped: Vec<String>,
}

/// Windows-1251 text (old Russian files) when it is not valid UTF-8.
fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.trim_start_matches('\u{feff}').to_string(),
        Err(_) => bytes
            .iter()
            .map(|&b| match b {
                0..=0x7f => b as char,
                0xa8 => 'Ё',
                0xb8 => 'ё',
                0xc0..=0xff => char::from_u32(0x0410 + (b as u32 - 0xc0)).unwrap_or('?'),
                _ => '?',
            })
            .collect(),
    }
}

fn read_text(path: &Path) -> Result<String, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if BINARY_EXT.contains(&ext.as_str()) {
        return Err(if ["png", "jpg", "jpeg", "gif", "webp", "bmp"].contains(&ext.as_str()) {
            "картинка (вставьте её через Ctrl+V)".into()
        } else {
            "не текстовый файл".into()
        });
    }
    let meta = std::fs::metadata(path).map_err(|_| "не открылся".to_string())?;
    if meta.len() > MAX_BYTES_PER_FILE {
        return Err("слишком большой".into());
    }
    let bytes = std::fs::read(path).map_err(|_| "не открылся".to_string())?;
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return Err("не текстовый файл".into());
    }
    Ok(decode(&bytes))
}

fn walk(dir: &Path, root: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        if out.len() >= MAX_FILES * 4 {
            return;
        }
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if p.is_dir() {
            if depth < MAX_DEPTH && !name.starts_with('.') && !SKIP_DIRS.contains(&name.to_lowercase().as_str()) {
                walk(&p, root, depth + 1, out);
            }
        } else if !name.starts_with('.') {
            out.push(p);
        }
    }
}

/// Files are read as they are; a folder is walked (without hidden and build folders).
pub fn load(paths: &[PathBuf], already: &[Attachment]) -> Loaded {
    let mut res = Loaded::default();
    let mut total: usize = already.iter().map(|a| a.text.chars().count()).sum();
    let mut jobs: Vec<(PathBuf, String)> = Vec::new();
    for p in paths {
        if p.is_dir() {
            let base = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            let mut found = Vec::new();
            walk(p, p, 0, &mut found);
            for f in found {
                let rel = f.strip_prefix(p).unwrap_or(&f).to_string_lossy().replace("\\", "/");
                jobs.push((f, format!("{base}/{rel}")));
            }
        } else {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("файл").to_string();
            jobs.push((p.clone(), name));
        }
    }
    let mut hidden = 0usize;
    for (path, name) in jobs {
        if already.iter().chain(res.files.iter()).any(|a| a.name == name) {
            continue;
        }
        if already.len() + res.files.len() >= MAX_FILES || total >= MAX_TOTAL_CHARS {
            hidden += 1;
            continue;
        }
        match read_text(&path) {
            Ok(text) => {
                let mut text: String = text;
                if text.chars().count() > MAX_FILE_CHARS {
                    text = text.chars().take(MAX_FILE_CHARS).collect::<String>() + "\n… (файл обрезан)";
                }
                if text.trim().is_empty() {
                    res.skipped.push(format!("{name}: пустой"));
                    continue;
                }
                total += text.chars().count();
                res.files.push(Attachment { name, text });
            }
            Err(why) => {
                // a folder full of pictures and archives should not flood the notice
                if res.skipped.len() < 3 {
                    res.skipped.push(format!("{name}: {why}"));
                } else {
                    hidden += 1;
                }
            }
        }
    }
    if hidden > 0 {
        res.skipped.push(format!("ещё {hidden} не добавлено (лимит или не текст)"));
    }
    res
}
