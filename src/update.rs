//! Updates from GitHub Releases. The installed copy downloads the new installer and runs it silently
//! (it closes this app and starts the new one); the portable copy swaps its own exe and restarts.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

/// "owner/repo" on GitHub whose releases carry the new versions.
pub const REPO: &str = "nortex-labs/nortex-assist";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Command-line flag of a freshly swapped portable exe: wait for the old process to exit first.
pub const AFTER_UPDATE_ARG: &str = "--after-update";

const INSTALLER_PREFIX: &str = "NortexAssist-Setup-";
const PORTABLE_NAME: &str = "nortex-assist.exe";
const MAX_DOWNLOAD: u64 = 200 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Asset {
    pub url: String,
    /// lowercase hex SHA-256 published by GitHub, when present
    pub sha256: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub page: String,
    pub installer: Option<Asset>,
    pub portable: Option<Asset>,
}

fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let mut it = s.trim().trim_start_matches(['v', 'V']).split('.').map(|p| p.parse::<u32>().ok());
    Some((it.next()??, it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0)))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    matches!((parse_version(candidate), parse_version(current)), (Some(a), Some(b)) if a > b)
}

/// True when the exe was put in place by the installer (its uninstaller sits next to it).
pub fn is_installed() -> bool {
    std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("unins000.exe").exists())).unwrap_or(false)
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).timeout_read(Duration::from_secs(60)).build()
}

fn asset(list: &[Value], pick: impl Fn(&str) -> bool) -> Option<Asset> {
    list.iter().find(|a| a["name"].as_str().is_some_and(&pick)).and_then(|a| {
        Some(Asset {
            url: a["browser_download_url"].as_str()?.to_string(),
            sha256: a["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(str::to_lowercase),
        })
    })
}

/// Latest release, if it is newer than this build. Ok(None): up to date (or no releases yet).
pub fn check() -> Result<Option<Release>, String> {
    check_newer_than(VERSION)
}

fn check_newer_than(current: &str) -> Result<Option<Release>, String> {
    if REPO.is_empty() {
        return Ok(None);
    }
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = agent()
        .get(&url)
        .set("user-agent", &format!("NortexAssist/{VERSION}"))
        .set("accept", "application/vnd.github+json")
        .call();
    let body: Value = match resp {
        Ok(r) => r.into_json().map_err(|e| crate::i18n::trf("ответ GitHub не прочитан: {e}", &[("e", &e.to_string())]))?,
        Err(ureq::Error::Status(404, _)) => return Ok(None),
        Err(ureq::Error::Status(code, _)) => return Err(crate::i18n::trf("GitHub ответил кодом {code}", &[("code", &code.to_string())])),
        Err(e) => return Err(crate::i18n::trf("нет связи с GitHub ({e})", &[("e", &e.to_string())])),
    };
    let tag = body["tag_name"].as_str().unwrap_or_default();
    if !is_newer(tag, current) {
        return Ok(None);
    }
    let assets = body["assets"].as_array().cloned().unwrap_or_default();
    Ok(Some(Release {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        page: body["html_url"].as_str().unwrap_or_default().to_string(),
        installer: asset(&assets, |n| n.starts_with(INSTALLER_PREFIX) && n.ends_with(".exe")),
        portable: asset(&assets, |n| n == PORTABLE_NAME),
    }))
}

fn download(a: &Asset, to: &PathBuf) -> Result<(), String> {
    let resp = agent()
        .get(&a.url)
        .set("user-agent", &format!("NortexAssist/{VERSION}"))
        .call()
        .map_err(|e| crate::i18n::trf("скачивание не удалось ({e})", &[("e", &e.to_string())]))?;
    let mut data = Vec::new();
    resp.into_reader().take(MAX_DOWNLOAD).read_to_end(&mut data).map_err(|e| crate::i18n::trf("скачивание прервалось ({e})", &[("e", &e.to_string())]))?;
    if let Some(expected) = &a.sha256 {
        let got: String = Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect();
        if &got != expected {
            return Err(crate::i18n::tr("скачанный файл повреждён (не совпала контрольная сумма)"));
        }
    }
    std::fs::write(to, &data).map_err(|e| crate::i18n::trf("не удалось сохранить файл ({e})", &[("e", &e.to_string())]))
}

/// Downloads the new version and starts it. On Ok the caller quits right away.
pub fn apply(rel: &Release) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    if is_installed() {
        let a = rel.installer.as_ref().ok_or_else(|| crate::i18n::tr("в выпуске нет установщика"))?;
        let setup = std::env::temp_dir().join(format!("{INSTALLER_PREFIX}{}.exe", rel.version));
        download(a, &setup)?;
        // the installer closes this app itself and starts the new version when it is done
        std::process::Command::new(&setup)
            .args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
            .spawn()
            .map_err(|e| crate::i18n::trf("установщик не запустился ({e})", &[("e", &e.to_string())]))?;
        return Ok(());
    }
    let a = rel.portable.as_ref().ok_or_else(|| crate::i18n::tr("в выпуске нет портативной версии"))?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let fresh = exe.with_extension("new");
    download(a, &fresh)?;
    // a running exe cannot be overwritten but can be renamed; the old copy is removed on the next start
    let old = exe.with_extension("old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| crate::i18n::trf("нет доступа к папке программы ({e})", &[("e", &e.to_string())]))?;
    if let Err(e) = std::fs::rename(&fresh, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(crate::i18n::trf("не удалось заменить файл ({e})", &[("e", &e.to_string())]));
    }
    std::process::Command::new(&exe)
        .arg(AFTER_UPDATE_ARG)
        .creation_flags(0x0000_0008) // DETACHED_PROCESS
        .spawn()
        .map_err(|e| crate::i18n::trf("новая версия не запустилась ({e})", &[("e", &e.to_string())]))?;
    Ok(())
}

/// Leftover of a portable self-update.
pub fn cleanup() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_extension("old"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_newer("v1.2.0", "1.1.0"));
        assert!(is_newer("1.10.0", "1.9.9"));
        assert!(is_newer("v2", "1.9.9"));
        assert!(!is_newer("v1.1.0", "1.1.0"));
        assert!(!is_newer("v1.0.9", "1.1.0"));
        assert!(!is_newer("latest", "1.1.0"));
    }

    /// Talks to the real GitHub: run with `cargo test -- --ignored` after publishing a release.
    #[test]
    #[ignore]
    fn live_release() {
        assert!(check().unwrap().is_none(), "this build should be the latest");
        let rel = check_newer_than("0.0.1").unwrap().expect("a published release");
        for a in [rel.installer.as_ref().expect("installer asset"), rel.portable.as_ref().expect("portable asset")] {
            let to = std::env::temp_dir().join("nortex-update-test.bin");
            download(a, &to).unwrap();
            assert!(a.sha256.is_some(), "GitHub should publish a digest");
            let _ = std::fs::remove_file(to);
        }
        println!("release {} ok", rel.version);
    }
}
