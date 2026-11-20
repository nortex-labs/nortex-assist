//! Voice input: recording from the default microphone with Windows' waveIn (the sound stays in
//! memory, nothing is written to disk) and turning it into text with Whisper on Groq or with Gemini.

use crate::config::Config;
use base64::Engine;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows::Win32::Media::Audio::{
    waveInAddBuffer, waveInClose, waveInOpen, waveInPrepareHeader, waveInReset, waveInStart, waveInUnprepareHeader, CALLBACK_NULL, HWAVEIN, WAVEFORMATEX,
    WAVEHDR, WAVE_FORMAT_PCM, WAVE_MAPPER, WHDR_DONE,
};

pub const RATE: u32 = 16_000;
/// Longest recording; longer ones stop by themselves.
pub const MAX_SECONDS: u64 = 60;
/// Shorter key presses are taken for accidental ones and ignored.
pub const MIN_MILLIS: u64 = 300;
/// How many level bars the window shows.
pub const BARS: usize = 16;

const BUF_SAMPLES: usize = (RATE / 20) as usize; // 50 ms
const BUFFERS: usize = 8;

/// A running recording. `stop` returns the sound; dropping it throws the sound away.
pub struct Recorder {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<Vec<i16>>>,
    /// recent loudness 0..1, newest last
    pub levels: Arc<Mutex<Vec<f32>>>,
}

struct Handle(HWAVEIN);
unsafe impl Send for Handle {}

impl Recorder {
    pub fn start() -> Result<Recorder, String> {
        let fmt = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 1,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * 2,
            nBlockAlign: 2,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let mut h = HWAVEIN::default();
        let r = unsafe { waveInOpen(Some(&mut h), WAVE_MAPPER, &fmt, None, None, CALLBACK_NULL) };
        if r != 0 {
            return Err(open_error(r));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let levels = Arc::new(Mutex::new(vec![0.0; BARS]));
        let (s2, l2, handle) = (stop.clone(), levels.clone(), Handle(h));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let thread = std::thread::spawn(move || record_loop(handle, s2, l2, ready_tx));
        match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(Recorder { stop, thread: Some(thread), levels }),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                Err(crate::i18n::tr("Микрофон не отвечает."))
            }
        }
    }

    /// Ends the recording and returns the sound as 16-bit mono samples.
    pub fn stop(mut self) -> Vec<i16> {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().and_then(|t| t.join().ok()).unwrap_or_default()
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn open_error(code: u32) -> String {
    // MMSYSERR_BADDEVICEID (2), MMSYSERR_NODRIVER (6), WAVERR_BADFORMAT (32)
    match code {
        2 | 6 => crate::i18n::tr("Микрофон не найден. Подключи его и попробуй ещё раз."),
        4 => crate::i18n::tr("Микрофон занят другой программой."),
        _ => crate::i18n::trf("Микрофон не открылся (код {c}).", &[("c", &code.to_string())]),
    }
}

fn record_loop(h: Handle, stop: Arc<AtomicBool>, levels: Arc<Mutex<Vec<f32>>>, ready: std::sync::mpsc::Sender<Result<(), String>>) -> Vec<i16> {
    let h = h.0;
    let hdr_size = std::mem::size_of::<WAVEHDR>() as u32;
    let mut bufs: Vec<Vec<i16>> = (0..BUFFERS).map(|_| vec![0i16; BUF_SAMPLES]).collect();
    let mut hdrs: Vec<Box<WAVEHDR>> = Vec::new();
    for b in bufs.iter_mut() {
        let mut hdr = Box::new(WAVEHDR {
            lpData: windows::core::PSTR(b.as_mut_ptr() as *mut u8),
            dwBufferLength: (BUF_SAMPLES * 2) as u32,
            ..Default::default()
        });
        unsafe {
            waveInPrepareHeader(h, &mut *hdr, hdr_size);
            waveInAddBuffer(h, &mut *hdr, hdr_size);
        }
        hdrs.push(hdr);
    }
    let started = unsafe { waveInStart(h) };
    let _ = ready.send(if started == 0 { Ok(()) } else { Err(open_error(started)) });
    let mut out: Vec<i16> = Vec::with_capacity((RATE as usize) * 10);
    let max = (RATE as u64 * MAX_SECONDS) as usize;
    let mut next = 0usize;
    if started == 0 {
        while !stop.load(Ordering::SeqCst) && out.len() < max {
            let p: *mut WAVEHDR = &mut *hdrs[next];
            let cur = unsafe { std::ptr::read_volatile(p) };
            if cur.dwFlags & WHDR_DONE == 0 {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            let n = (cur.dwBytesRecorded as usize / 2).min(BUF_SAMPLES);
            let chunk = &bufs[next][..n];
            out.extend_from_slice(chunk);
            push_level(&levels, chunk);
            unsafe {
                let mut again = cur;
                again.dwFlags &= !WHDR_DONE;
                again.dwBytesRecorded = 0;
                std::ptr::write_volatile(p, again);
                waveInAddBuffer(h, p, hdr_size);
            }
            next = (next + 1) % BUFFERS;
        }
    }
    unsafe {
        waveInReset(h);
        // what was recorded in the buffers that were still filling
        for i in 0..BUFFERS {
            let k = (next + i) % BUFFERS;
            let p: *mut WAVEHDR = &mut *hdrs[k];
            let cur = std::ptr::read_volatile(p);
            if cur.dwFlags & WHDR_DONE != 0 && cur.dwBytesRecorded > 0 && out.len() < max {
                let n = (cur.dwBytesRecorded as usize / 2).min(BUF_SAMPLES);
                out.extend_from_slice(&bufs[k][..n]);
            }
            waveInUnprepareHeader(h, p, hdr_size);
        }
        waveInClose(h);
    }
    out
}

fn push_level(levels: &Mutex<Vec<f32>>, chunk: &[i16]) {
    if chunk.is_empty() {
        return;
    }
    let rms = (chunk.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / chunk.len() as f64).sqrt();
    // speech sits around 500..8000; a gentle curve so quiet speech still moves the bars
    let v = ((rms / 6000.0).sqrt() as f32).clamp(0.0, 1.0);
    if let Ok(mut l) = levels.lock() {
        l.push(v);
        let extra = l.len().saturating_sub(BARS);
        l.drain(..extra);
    }
}

/// Microphone access switched off in Windows (Settings → Privacy → Microphone)?
pub fn mic_blocked() -> bool {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let base = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";
    let deny = |path: &str| {
        RegKey::predef(HKEY_CURRENT_USER).open_subkey(path).and_then(|k| k.get_value::<String, _>("Value")).map(|v| v.eq_ignore_ascii_case("Deny")).unwrap_or(false)
    };
    deny(base) || deny(&format!(r"{base}\NonPackaged"))
}

/// The recording as a WAV file in memory.
pub fn wav(samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&1u16.to_le_bytes()); // mono
    v.extend_from_slice(&RATE.to_le_bytes());
    v.extend_from_slice(&(RATE * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

/// Loudness of the recording in 20 ms frames: (frames, frames that sound like speech, noise floor, threshold).
pub fn speech_stats(samples: &[i16]) -> (usize, usize, f64, f64) {
    const FRAME: usize = (RATE / 50) as usize;
    let mut rms: Vec<f64> = samples
        .chunks(FRAME)
        .filter(|c| c.len() == FRAME)
        .map(|c| (c.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / FRAME as f64).sqrt())
        .collect();
    if rms.is_empty() {
        return (0, 0, 0.0, 0.0);
    }
    let frames = rms.len();
    let loud: Vec<f64> = rms.clone();
    rms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // the quietest fifth is the room; speech must stand out from it (and be loud enough anyway)
    let noise = rms[frames / 5];
    let threshold = (noise * 2.5).clamp(250.0, 600.0);
    // voice comes in syllables of 100 ms and more; a key click or a tap on the desk is one or two
    // frames, so only loud stretches of at least 80 ms count
    let (mut speech, mut run) = (0usize, 0usize);
    for &r in loud.iter().chain(std::iter::once(&0.0)) {
        if r > threshold {
            run += 1;
        } else {
            if run >= 4 {
                speech += run;
            }
            run = 0;
        }
    }
    (frames, speech, noise, threshold)
}

/// True when there is no speech worth sending: silence, room noise, a click or a cough.
/// Whisper turns such recordings into made-up phrases, so they never reach it.
pub fn is_silent(samples: &[i16]) -> bool {
    let (_, speech, _, _) = speech_stats(samples);
    speech < 12 // less than about a quarter of a second of voice
}

/// Phrases Whisper is known to invent from silence or noise (subtitle credits, "thanks for watching").
pub fn is_hallucination(text: &str) -> bool {
    let t: String = text.to_lowercase().chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect();
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() {
        return true;
    }
    const PARTS: [&str; 8] = ["субтитр", "subtitles by", "amara org", "dimatorzok", "редактор субтитров", "корректор", "подписывайтесь на канал", "ставьте лайк"];
    const WHOLE: [&str; 14] = [
        "продолжение следует", "спасибо за просмотр", "спасибо за внимание", "благодарю за внимание", "до новых встреч",
        "thank you for watching", "thanks for watching", "thank you", "you", "bye", "music", "музыка", "мы вернемся", "до свидания",
    ];
    PARTS.iter().any(|p| t.contains(p)) || WHOLE.contains(&t.as_str())
}

fn origin(env_name: &str, default: &str) -> String {
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var(env_name) {
        return v;
    }
    let _ = env_name;
    default.to_string()
}

/// Speech to text: Whisper on Groq when there is a Groq key (fast and free), otherwise Gemini.
pub fn transcribe(cfg: &Config, samples: &[i16]) -> Result<String, String> {
    let audio = wav(samples);
    let groq_key = cfg.groq_key.trim();
    let gemini_key = cfg.gemini_key.trim();
    let mut last_err = String::new();
    if !groq_key.is_empty() {
        match whisper_groq(groq_key, &audio) {
            Ok(t) => return Ok(t),
            Err(e) => last_err = e,
        }
    }
    if !gemini_key.is_empty() {
        return gemini_transcribe(cfg, gemini_key, &audio);
    }
    if last_err.is_empty() {
        return Err(crate::i18n::tr("Для голоса нужен ключ Groq или Gemini. Добавь его в настройках."));
    }
    Err(last_err)
}

fn whisper_groq(key: &str, audio: &[u8]) -> Result<String, String> {
    let boundary = format!("----nortex{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    let mut body: Vec<u8> = Vec::with_capacity(audio.len() + 600);
    let field = |body: &mut Vec<u8>, name: &str, value: &str| {
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
    };
    field(&mut body, "model", "whisper-large-v3-turbo");
    // verbose_json: every piece comes with how sure Whisper is that it heard speech at all
    field(&mut body, "response_format", "verbose_json");
    field(&mut body, "temperature", "0");
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"voice.wav\"\r\nContent-Type: audio/wav\r\n\r\n").as_bytes());
    body.extend_from_slice(audio);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let url = format!("{}/audio/transcriptions", origin("NORTEX_GROQ_ORIGIN", "https://api.groq.com/openai/v1"));
    let resp = ureq::post(&url)
        .set("authorization", &format!("Bearer {key}"))
        .set("content-type", &format!("multipart/form-data; boundary={boundary}"))
        .timeout(Duration::from_secs(40))
        .send_bytes(&body)
        .map_err(|e| crate::llm::explain_error(true, &transport_message(e)))?;
    let j: Value = resp.into_json().map_err(|e| e.to_string())?;
    Ok(clean_transcript(&j))
}

/// The text without the pieces Whisper itself marks as "probably no speech" (that is where it
/// makes things up), and without its well-known invented phrases.
fn clean_transcript(j: &Value) -> String {
    let text = match j["segments"].as_array() {
        Some(segs) => segs
            .iter()
            .inspect(|s| {
                // numbers only (no words): to tune the filter on real recordings
                crate::logging::log_err(&format!(
                    "whisper segment: no_speech {:.2}, logprob {:.2}, compression {:.2}, language {}",
                    s["no_speech_prob"].as_f64().unwrap_or(-1.0),
                    s["avg_logprob"].as_f64().unwrap_or(0.0),
                    s["compression_ratio"].as_f64().unwrap_or(0.0),
                    j["language"].as_str().unwrap_or("?")
                ))
            })
            .filter(|s| {
                let nsp = s["no_speech_prob"].as_f64().unwrap_or(0.0);
                let logp = s["avg_logprob"].as_f64().unwrap_or(0.0);
                !(nsp > 0.9 || (nsp > 0.6 && logp < -0.8))
            })
            .filter_map(|s| s["text"].as_str())
            .collect::<Vec<_>>()
            .join(" "),
        None => j["text"].as_str().unwrap_or("").to_string(),
    };
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if is_hallucination(&text) { String::new() } else { text }
}

fn gemini_transcribe(cfg: &Config, key: &str, audio: &[u8]) -> Result<String, String> {
    let model = if cfg.model.trim().to_lowercase().starts_with("gemini-") { cfg.model.trim().to_string() } else { "gemini-3.5-flash-lite".to_string() };
    let body = json!({
        "contents": [{ "role": "user", "parts": [
            { "text": "Transcribe this voice recording word for word in the language it is spoken in. Output only the transcription, nothing else. If there is no speech, output nothing." },
            { "inlineData": { "mimeType": "audio/wav", "data": base64::engine::general_purpose::STANDARD.encode(audio) } }
        ]}],
        "generationConfig": { "maxOutputTokens": 1024, "thinkingConfig": { "thinkingLevel": "low" } },
    });
    let url = format!("{}/v1beta/models/{model}:generateContent", origin("NORTEX_GEMINI_ORIGIN", "https://generativelanguage.googleapis.com"));
    let resp = ureq::post(&url)
        .set("content-type", "application/json")
        .set("x-goog-api-key", key)
        .timeout(Duration::from_secs(40))
        .send_string(&body.to_string())
        .map_err(|e| crate::llm::explain_error(false, &transport_message(e)))?;
    let j: Value = resp.into_json().map_err(|e| e.to_string())?;
    let text: String = j["candidates"][0]["content"]["parts"]
        .as_array()
        .map(|p| p.iter().filter(|x| !x["thought"].as_bool().unwrap_or(false)).filter_map(|x| x["text"].as_str()).collect())
        .unwrap_or_default();
    let text = text.trim().to_string();
    Ok(if is_hallucination(&text) { String::new() } else { text })
}

fn transport_message(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body: Value = resp.into_json().unwrap_or(Value::Null);
            body["error"]["message"].as_str().map(str::to_string).unwrap_or_else(|| code.to_string())
        }
        other => {
            let s = other.to_string();
            if s.to_lowercase().contains("timed out") { "timeout".to_string() } else { format!("fetch failed: {s}") }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn wav_header() {
        let w = super::wav(&[0, 1, -1]);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(w.len(), 44 + 6);
        assert_eq!(u32::from_le_bytes([w[40], w[41], w[42], w[43]]), 6);
        assert!(super::is_silent(&[0, 10, -20]));
    }

    /// 16 kHz test sounds: room hiss, a single click, half a second of "voice" in the middle.
    fn sound(seconds: f32, f: impl Fn(usize) -> f64) -> Vec<i16> {
        (0..(16_000.0 * seconds) as usize).map(|i| f(i).clamp(-32000.0, 32000.0) as i16).collect()
    }
    fn hiss(i: usize, level: f64) -> f64 {
        // cheap pseudo-random noise
        let x = (i as u64).wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407) >> 33;
        ((x % 2000) as f64 / 1000.0 - 1.0) * level
    }

    #[test]
    fn speech_is_told_from_silence() {
        // quiet room and a louder fan: nothing to send
        assert!(super::is_silent(&sound(2.0, |i| hiss(i, 120.0))));
        assert!(super::is_silent(&sound(2.0, |i| hiss(i, 700.0))));
        // a key click in silence
        assert!(super::is_silent(&sound(2.0, |i| if (16000..16200).contains(&i) { 9000.0 } else { hiss(i, 100.0) })));
        // half a second of voice in a quiet room
        let voice = |i: usize| if (8000..16000).contains(&i) { (i as f64 * 0.12).sin() * 3000.0 } else { hiss(i, 100.0) };
        assert!(!super::is_silent(&sound(1.5, voice)));
        // talking the whole time, the key held just as long
        assert!(!super::is_silent(&sound(1.0, |i| (i as f64 * 0.1).sin() * 2500.0 + hiss(i, 300.0))));
        // quiet voice with a quiet microphone
        assert!(!super::is_silent(&sound(1.5, |i| if (4000..14000).contains(&i) { (i as f64 * 0.1).sin() * 700.0 } else { hiss(i, 60.0) })));
    }

    #[test]
    fn made_up_phrases_are_dropped() {
        assert!(super::is_hallucination("Продолжение следует..."));
        assert!(super::is_hallucination("Субтитры сделал DimaTorzok"));
        assert!(super::is_hallucination(" Thank you. "));
        assert!(!super::is_hallucination("Сколько будет два плюс два?"));
        let j = serde_json::json!({"text": "x", "segments": [
            {"text": " Das ist der Röbe.", "no_speech_prob": 0.95, "avg_logprob": -0.4},
            {"text": " Привет", "no_speech_prob": 0.05, "avg_logprob": -0.2}]});
        assert_eq!(super::clean_transcript(&j), "Привет");
    }
}
