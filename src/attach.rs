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

/// What one picked file or folder became: a chip in the composer.
pub struct Group {
    pub label: String,
    pub folder: bool,
    pub files: Vec<Attachment>,
}

#[derive(Default)]
pub struct Loaded {
    pub groups: Vec<Group>,
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
            crate::i18n::tr("картинка (вставьте её через Ctrl+V)")
        } else {
            crate::i18n::tr("не текстовый файл")
        });
    }
    let meta = std::fs::metadata(path).map_err(|_| crate::i18n::tr("не открылся"))?;
    if meta.len() > MAX_BYTES_PER_FILE {
        return Err(crate::i18n::tr("слишком большой"));
    }
    let bytes = std::fs::read(path).map_err(|_| crate::i18n::tr("не открылся"))?;
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return Err(crate::i18n::tr("не текстовый файл"));
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
/// Names already attached are not added twice.
pub fn load(paths: &[PathBuf], already: &[Attachment]) -> Loaded {
    let mut res = Loaded::default();
    let mut total: usize = already.iter().map(|a| a.text.chars().count()).sum();
    let mut count = already.len();
    let mut taken: Vec<String> = already.iter().map(|a| a.name.clone()).collect();
    let mut hidden = 0usize;

    for p in paths {
        let own = p.file_name().and_then(|n| n.to_str()).map(str::to_string).unwrap_or_else(|| crate::i18n::tr("файл"));
        let folder = p.is_dir();
        let mut jobs: Vec<(PathBuf, String)> = Vec::new();
        if folder {
            let mut found = Vec::new();
            walk(p, p, 0, &mut found);
            for f in found {
                let rel = f.strip_prefix(p).unwrap_or(&f).to_string_lossy().replace("\\", "/");
                jobs.push((f, format!("{own}/{rel}")));
            }
        } else {
            // two different files with one name (from different folders) keep their folder in the name
            let mut name = own.clone();
            if taken.contains(&name) {
                if let Some(parent) = p.parent().and_then(|d| d.file_name()).and_then(|n| n.to_str()) {
                    name = format!("{parent}/{own}");
                }
            }
            jobs.push((p.clone(), name));
        }

        let mut group = Group { label: own, folder, files: Vec::new() };
        for (path, name) in jobs {
            if taken.contains(&name) {
                continue;
            }
            if count >= MAX_FILES || total >= MAX_TOTAL_CHARS {
                hidden += 1;
                continue;
            }
            match read_text(&path) {
                Ok(text) => {
                    let mut text: String = text;
                    if text.chars().count() > MAX_FILE_CHARS {
                        text = text.chars().take(MAX_FILE_CHARS).collect::<String>() + &crate::i18n::tr("
… (файл обрезан)");
                    }
                    if text.trim().is_empty() {
                        res.skipped.push(crate::i18n::trf("{name}: {why}", &[("name", &name), ("why", &crate::i18n::tr("пустой"))]));
                        continue;
                    }
                    total += text.chars().count();
                    count += 1;
                    taken.push(name.clone());
                    group.files.push(Attachment { name, text });
                }
                Err(why) => {
                    // a folder full of pictures and archives should not flood the notice
                    if res.skipped.len() < 3 {
                        res.skipped.push(crate::i18n::trf("{name}: {why}", &[("name", &name), ("why", &why)]));
                    } else {
                        hidden += 1;
                    }
                }
            }
        }
        if !group.files.is_empty() {
            res.groups.push(group);
        }
    }
    if hidden > 0 {
        res.skipped.push(crate::i18n::trf("ещё {n} не добавлено (лимит или не текст)", &[("n", &hidden.to_string())]));
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nortex-attach-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn same_name_from_two_folders_keeps_both() {
        let (a, b) = (temp("a"), temp("b"));
        std::fs::write(a.join("notes.txt"), "first").unwrap();
        std::fs::write(b.join("notes.txt"), "second").unwrap();
        let loaded = load(&[a.join("notes.txt"), b.join("notes.txt")], &[]);
        let names: Vec<&str> = loaded.groups.iter().flat_map(|g| g.files.iter().map(|f| f.name.as_str())).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert_ne!(names[0], names[1]);
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn folder_skips_binary_and_build_dirs() {
        let d = temp("folder");
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::create_dir_all(d.join("node_modules")).unwrap();
        std::fs::write(d.join("src").join("a.rs"), "fn main() {}").unwrap();
        std::fs::write(d.join("node_modules").join("x.js"), "junk").unwrap();
        std::fs::write(d.join("pic.png"), [0u8, 1, 2]).unwrap();
        // old Russian files in Windows-1251
        std::fs::write(d.join("old.txt"), [0xcf, 0xf0, 0xe8, 0xe2, 0xe5, 0xf2]).unwrap();
        let loaded = load(&[d.clone()], &[]);
        assert_eq!(loaded.groups.len(), 1);
        let g = &loaded.groups[0];
        assert!(g.folder);
        let mut names: Vec<&str> = g.files.iter().map(|f| f.name.rsplit('/').next().unwrap()).collect();
        names.sort();
        assert_eq!(names, vec!["a.rs", "old.txt"]);
        assert_eq!(g.files.iter().find(|f| f.name.ends_with("old.txt")).unwrap().text, "Привет");
        let _ = std::fs::remove_dir_all(&d);
    }
}
