//! Clipboard snapshot/restore, selection grabbing, screen snipping and image encoding.

use crate::logging::log_err;
use arboard::Clipboard;
use base64::Engine;
use image::{imageops::FilterType, DynamicImage, RgbaImage};
use std::os::windows::process::CommandExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const MAX_SIDE: u32 = 2000;
const JPEG_QUALITY: u8 = 88;

pub fn set_text(text: &str) {
    if let Ok(mut cb) = Clipboard::new() {
        let _ = cb.set_text(text.to_string());
    }
}

/// Copies the current selection of the foreground app via Ctrl+C and puts the clipboard back.
/// Waits up to `wait` for the app to copy; a code editor that copied its whole line because
/// nothing was selected counts as "nothing selected" (empty result).
/// The clipboard is left exactly as it was: when nothing was copied it is not touched at all,
/// otherwise every format (copied files, formatted text, pictures) is put back.
pub fn grab_selection(wait: Duration) -> String {
    use crate::native::{clipboard_restore, clipboard_save, clipboard_seq};
    let prev = clipboard_save();
    let seq = clipboard_seq();
    if !crate::native::copy_selection() {
        return String::new();
    }
    let end = Instant::now() + wait;
    let mut result = None;
    loop {
        if clipboard_seq() != seq {
            if let Ok(t) = Clipboard::new().and_then(|mut cb| cb.get_text()) {
                if !t.trim().is_empty() {
                    if !crate::native::clipboard_is_line_copy() {
                        result = Some(t.trim().to_string());
                    }
                    break;
                }
            }
        }
        if Instant::now() >= end {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if clipboard_seq() != seq {
        clipboard_restore(&prev);
    }
    result.unwrap_or_default()
}

/// Resizes to MAX_SIDE and encodes as base64 JPEG (full-size PNGs from a 4K screen are megabytes).
pub fn encode_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Option<String> {
    let img = DynamicImage::ImageRgba8(RgbaImage::from_raw(width, height, rgba)?);
    let side = width.max(height);
    let img = if side > MAX_SIDE {
        let k = MAX_SIDE as f64 / side as f64;
        img.resize_exact(((width as f64 * k).round() as u32).max(1), ((height as f64 * k).round() as u32).max(1), FilterType::Lanczos3)
    } else {
        img
    };
    let rgb = img.to_rgb8();
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, JPEG_QUALITY).encode_image(&rgb).ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(buf))
}

/// An image ready to attach: base64 JPEG for the model and a small RGBA thumbnail for the UI.
pub type Shot = (String, (u32, u32, Vec<u8>));

pub fn prepare_shot(w: u32, h: u32, rgba: Vec<u8>) -> Option<Shot> {
    let thumb = thumbnail(w, h, &rgba)?;
    Some((encode_rgba(w, h, rgba)?, thumb))
}

/// Returns the clipboard image as a base64 JPEG together with a small RGBA thumbnail.
pub fn clipboard_image() -> Option<Shot> {
    let img = Clipboard::new().ok()?.get_image().ok()?;
    prepare_shot(img.width as u32, img.height as u32, img.bytes.into_owned())
}

/// Cuts a part out of an RGBA image; `f` is (x, y, width, height) as fractions of the image.
pub fn crop_rgba(w: u32, h: u32, px: &[u8], f: [f32; 4]) -> (u32, u32, Vec<u8>) {
    let x0 = ((f[0].clamp(0.0, 1.0) * w as f32).round() as u32).min(w.saturating_sub(1));
    let y0 = ((f[1].clamp(0.0, 1.0) * h as f32).round() as u32).min(h.saturating_sub(1));
    let cw = ((f[2].clamp(0.0, 1.0) * w as f32).round() as u32).clamp(1, w - x0);
    let ch = ((f[3].clamp(0.0, 1.0) * h as f32).round() as u32).clamp(1, h - y0);
    let mut out = Vec::with_capacity((cw * ch * 4) as usize);
    for row in y0..y0 + ch {
        let start = ((row * w + x0) * 4) as usize;
        out.extend_from_slice(&px[start..start + (cw * 4) as usize]);
    }
    (cw, ch, out)
}

pub fn thumbnail(width: u32, height: u32, rgba: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let img = RgbaImage::from_raw(width, height, rgba.to_vec())?;
    let t = DynamicImage::ImageRgba8(img).thumbnail(160, 160).to_rgba8();
    Some((t.width(), t.height(), t.into_raw()))
}

/// Opens the Windows snipping overlay and waits (up to 60 s) for an image to land in the clipboard.
/// A newer call bumps `token`, which cancels this one.
pub fn snip(token: &AtomicU64, my: u64) -> Option<(u32, u32, Vec<u8>)> {
    let prev = crate::native::clipboard_save();
    if let Ok(mut cb) = Clipboard::new() {
        let _ = cb.clear();
    }
    if let Err(e) = std::process::Command::new("cmd").args(["/C", "start", "", "ms-screenclip:"]).creation_flags(0x0800_0000).spawn() {
        log_err(&format!("ms-screenclip: {e}"));
    }
    let end = Instant::now() + Duration::from_secs(60);
    while Instant::now() < end && token.load(Ordering::SeqCst) == my {
        std::thread::sleep(Duration::from_millis(60));
        if let Ok(img) = Clipboard::new().and_then(|mut cb| cb.get_image()) {
            let (w, h) = (img.width as u32, img.height as u32);
            if w > 0 && h > 0 {
                crate::native::clipboard_restore(&prev);
                return Some((w, h, img.bytes.into_owned()));
            }
        }
    }
    if token.load(Ordering::SeqCst) == my {
        crate::native::clipboard_restore(&prev);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_takes_the_selected_part() {
        // 4x2 image whose pixel value is its index
        let px: Vec<u8> = (0..8u8).flat_map(|i| [i, i, i, 255]).collect();
        let (w, h, out) = crop_rgba(4, 2, &px, [0.5, 0.5, 0.5, 0.5]);
        assert_eq!((w, h), (2, 1));
        assert_eq!(out, vec![6, 6, 6, 255, 7, 7, 7, 255]);
        // the whole image, and a selection hanging over the edge stays inside
        assert_eq!(crop_rgba(4, 2, &px, [0.0, 0.0, 1.0, 1.0]).2, px);
        assert_eq!(crop_rgba(4, 2, &px, [0.9, 0.9, 0.9, 0.9]).0, 1);
    }
}
