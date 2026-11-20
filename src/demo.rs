//! Debug builds only: scenes for screenshots of the window (NORTEX_DEMO_OUT=<dir>).
//! Run with NORTEX_DATA_DIR (own data folder) and NORTEX_NO_HOTKEYS: nothing of the user's is used.

use super::{with_app, Answer, Composer, APP};
use slint::{ComponentHandle, Timer};
use std::path::{Path, PathBuf};
use std::time::Duration;

const ANSWER: &str = r#"**Ответ: x = 5 и x = −1**

$$D = b^2 - 4ac = 36$$

1. Найдём дискриминант: $D = 36$.
2. Вычислим корни: $x = \frac{4 \pm 6}{2}$.

```python
D = 36
x1 = 5
x2 = -1
```"#;

fn answer(q: &str, text: &str, open: bool) -> Answer {
    Answer {
        text: text.into(),
        req: None,
        is_error: false,
        record_q: Some(q.into()),
        question: q.into(),
        shots: 0,
        note: String::new(),
        action: String::new(),
        open,
        waiting: false,
        ..Default::default()
    }
}

fn snap(out: &Path, name: &str) {
    let rc = APP.with(|a| a.borrow().clone());
    let Some(rc) = rc else { return };
    let Ok(app) = rc.try_borrow() else { return };
    let ui = app.ui();
    match ui.window().take_snapshot() {
        Ok(buf) => {
            let img = image::RgbaImage::from_raw(buf.width(), buf.height(), buf.as_bytes().to_vec());
            if let Some(img) = img {
                let _ = img.save(out.join(name));
            }
        }
        Err(e) => super::log_err(&format!("snapshot: {e}")),
    }
    if let Some(p) = &app.pill {
        if name.starts_with("voice-pill") {
            if let Ok(buf) = p.window().take_snapshot() {
                if let Some(img) = image::RgbaImage::from_raw(buf.width(), buf.height(), buf.as_bytes().to_vec()) {
                    let _ = img.save(out.join(name));
                }
            }
        }
    }
}

/// Sets the window to a logical size (and keeps it pinned, so it does not hide without focus).
fn size(w: f32, h: f32) {
    with_app(|a| {
        let scale = a.ui().window().scale_factor();
        let mut r = a.window_rect();
        r.w = (w * scale) as u32;
        r.h = (h * scale) as u32;
        a.apply_rect(r);
    });
}

/// Live run against the real services with the keys of the test copy: every step waits until the
/// answer is in, writes it to live.txt and takes a picture.
fn run_live(out: PathBuf) {
    use std::cell::RefCell;
    use std::rc::Rc;
    type Step = Box<dyn Fn()>;
    let log = {
        let o = out.clone();
        move |s: &str| {
            let _ = std::fs::OpenOptions::new().create(true).append(true).open(o.join("live.txt")).and_then(|mut f| std::io::Write::write_all(&mut f, format!("{s}\n").as_bytes()));
        }
    };
    let last = || {
        APP.with(|a| {
            a.borrow().as_ref().map(|r| {
                let r = r.borrow();
                let t = r.turns.last();
                (r.busy || r.voice.is_some(), t.map(|t| format!("Q: {}\nA: {}\nnote: {} | error: {} | action: {}", t.question, t.text, t.note, t.is_error, t.action)).unwrap_or_default())
            })
        })
        .unwrap_or((true, String::new()))
    };
    let steps: Vec<(&'static str, Step)> = vec![
        ("L1-text", Box::new(|| with_app(|a| {
            a.new_chat();
            a.send(super::Request { text: "Столица Австралии? A) Сидней B) Канберра C) Мельбурн".into(), ..Default::default() }, Some("Столица Австралии?".into()), "Столица Австралии? A) Сидней B) Канберра C) Мельбурн".into(), false);
        }))),
        ("L2-explain", Box::new(|| with_app(|a| a.quick_action(0)))),
        ("L3-another", Box::new(|| with_app(|a| a.retry()))),
        ("L4-math", Box::new(|| with_app(|a| {
            a.new_chat();
            a.send(super::Request { text: "Реши уравнение x^2 - 4x - 5 = 0".into(), ..Default::default() }, Some("Реши уравнение x^2 - 4x - 5 = 0".into()), "Реши уравнение x² − 4x − 5 = 0".into(), false);
        }))),
        ("L5-voice", Box::new(|| with_app(|a| {
            // a recorded phrase instead of the microphone: Whisper on Groq, then the question
            let Ok(path) = std::env::var("NORTEX_VOICE_FILE") else { return };
            let Ok(bytes) = std::fs::read(path) else { return };
            let samples: Vec<i16> = bytes.get(44..).unwrap_or(&[]).chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
            let cfg = a.cfg.clone();
            a.voice_id += 1;
            let id = a.voice_id;
            a.voice = Some(super::VoiceRun { rec: None, started: std::time::Instant::now(), popup: true, by_key: false, selection: std::sync::Arc::new(std::sync::Mutex::new(None)), transcribing: true, id });
            std::thread::spawn(move || {
                let res = crate::voice::transcribe(&cfg, &samples);
                let _ = slint::invoke_from_event_loop(move || with_app(|a| a.voice_done(id, res)));
            });
        }))),
    ];
    let only = std::env::var("NORTEX_DEMO_SCENES").unwrap_or_default();
    let steps: Vec<(&'static str, Step)> = steps.into_iter().filter(|(n, _)| only.is_empty() || only.split(',').any(|x| n.starts_with(x.trim()))).collect();
    let steps = Rc::new(steps);
    let state = Rc::new(RefCell::new((0usize, false, std::time::Instant::now())));
    let timer = Rc::new(Timer::default());
    let t2 = timer.clone();
    with_app(|a| {
        a.pinned = true;
        a.open_chat();
    });
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(400), move || {
        let mut st = state.borrow_mut();
        let (i, started, at) = *st;
        if i >= steps.len() {
            t2.stop();
            let _ = slint::quit_event_loop();
            return;
        }
        if !started {
            log(&format!("===== {}", steps[i].0));
            (steps[i].1)();
            *st = (i, true, std::time::Instant::now());
            return;
        }
        let (busy, text) = last();
        if (busy && at.elapsed() < Duration::from_secs(90)) || at.elapsed() < Duration::from_millis(1500) {
            return;
        }
        log(&format!("{text}\n(time {:.1}s)", at.elapsed().as_secs_f32()));
        snap(&out, &format!("{}.png", steps[i].0));
        *st = (i + 1, false, std::time::Instant::now());
    });
    std::mem::forget(timer);
}

pub fn run(out: PathBuf) {
    if std::env::var("NORTEX_DEMO_LIVE").is_ok() {
        return run_live(out);
    }
    let only = std::env::var("NORTEX_DEMO_SCENES").unwrap_or_default();
    let mut t = 600u64;
    let mut at = |ms: u64, f: Box<dyn FnOnce()>| {
        t += ms;
        Timer::single_shot(Duration::from_millis(t), f);
    };
    let want = move |s: &str| only.is_empty() || only.split(',').any(|x| s.starts_with(x.trim()));
    let o = out.clone();
    let shot = move |name: &'static str| -> Box<dyn FnOnce()> {
        let o = o.clone();
        Box::new(move || snap(&o, name))
    };

    at(0, Box::new(|| {
        with_app(|a| {
            a.pinned = true;
            a.ui().set_pinned(false);
            a.open_chat();
        })
    }));
    if want("1") {
        // the empty window, three sizes and the old one for comparison
        for (name, w, h) in [("1-empty-380x220.png", 380.0, 220.0), ("1-empty-420x300.png", 420.0, 300.0)] {
            at(300, Box::new(move || size(w, h)));
            at(500, shot(name));
        }
        at(100, Box::new(|| size(380.0, 220.0)));
    }
    if want("2") {
        at(300, Box::new(|| {
            with_app(|a| {
                a.new_chat();
                a.turns.push(answer("Реши x² − 4x − 5 = 0", ANSWER, true));
                a.composer = Some(Composer::FollowUp);
                a.update_placeholder();
                a.render();
            })
        }));
        for (name, w, h) in [("2-answer-380x220.png", 380.0, 220.0), ("2-answer-300x420.png", 300.0, 420.0), ("2-answer-380x420.png", 380.0, 420.0)] {
            at(300, Box::new(move || size(w, h)));
            at(500, shot(name));
        }
        at(100, Box::new(|| with_app(|a| a.ui().set_copy_done(true))));
        at(400, shot("2-answer-copied.png"));
        at(100, Box::new(|| { with_app(|a| a.ui().set_copy_done(false)); size(380.0, 220.0) }));
    }
    if want("3") {
        // a dialog: the older turn folded, Gemini answered instead of Groq
        at(300, Box::new(|| {
            with_app(|a| {
                a.new_chat();
                a.turns.push(answer("Столица Австралии? A) Сидней B) Канберра C) Мельбурн", "**B) Канберра**", false));
                let mut t = answer("Объясни", "Канберру построили как компромисс между Сиднеем и Мельбурном: оба города хотели быть столицей, поэтому в 1908 году выбрали место между ними.\n\n- Сидней — самый большой город\n- Мельбурн был временной столицей до 1927 года", true);
                t.note = crate::i18n::tr("Ответила Gemini — у Groq кончился лимит");
                a.turns.push(t);
                a.render();
            })
        }));
        at(600, shot("3-dialog.png"));
        at(100, Box::new(|| {
            with_app(|a| {
                a.new_chat();
                let mut t = answer("Переведи на английский: доброе утро", "Ошибка: Лимит запросов Groq. Подожди немного и повтори.\nДобавь ключ Gemini — тогда в такие моменты будет отвечать Gemini.", true);
                t.is_error = true;
                t.action = "gemini-key".into();
                a.turns.push(t);
                a.render();
            })
        }));
        at(600, shot("3-error-gemini-key.png"));
    }
    if want("4") {
        // voice inside the window and the pill
        at(300, Box::new(|| {
            with_app(|a| {
                a.new_chat();
                a.composer = Some(Composer::Free);
                let ui = a.ui();
                ui.set_composer_on(true);
                ui.set_recording(true);
                ui.set_rec_time("0:04".into());
                let lv: Vec<f32> = (0..16).map(|i| (i as f32 * 0.9).sin().abs() * 0.8 + 0.1).collect();
                ui.set_rec_levels(slint::ModelRc::new(slint::VecModel::from(lv.clone())));
                a.show_pill(0, "");
                if let Some(p) = &a.pill {
                    p.set_levels(slint::ModelRc::new(slint::VecModel::from(lv)));
                    p.set_time_text("0:04".into());
                }
            })
        }));
        at(600, shot("4-voice-window.png"));
        at(100, shot("voice-pill.png"));
        at(100, Box::new(|| {
            with_app(|a| {
                a.ui().set_recording(false);
                a.hide_pill();
            })
        }));
    }
    if want("5") {
        // the settings: a tall window shows them whole
        at(300, Box::new(|| {
            with_app(|a| {
                a.open_settings();
                a.cfg.smart_offer = "pending".into();
                let ui = a.ui();
                ui.set_smart_offer(true);
                ui.set_key_state(2);
                ui.set_key_state_text(crate::i18n::tr("Ключ работает").into());
            })
        }));
        at(200, Box::new(|| size(420.0, 1250.0)));
        at(700, shot("5-settings-simple.png"));
        at(100, Box::new(|| {
            with_app(|a| {
                let ui = a.ui();
                ui.set_smart_offer(false);
                a.cfg.smart_offer.clear();
                ui.set_advanced(true);
                ui.set_news_open(true);
            })
        }));
        at(200, Box::new(|| size(420.0, 2300.0)));
        at(900, shot("5-settings-advanced.png"));
        at(100, Box::new(|| {
            with_app(|a| {
                let ui = a.ui();
                ui.set_advanced(false);
                ui.set_news_open(false);
                a.close_settings();
            })
        }));
        at(200, Box::new(|| size(380.0, 220.0)));
    }
    if want("6") {
        // light theme
        at(300, Box::new(|| {
            with_app(|a| {
                a.cfg.theme = "light".into();
                a.apply_theme();
                a.new_chat();
                a.turns.push(answer("Реши x² − 4x − 5 = 0", ANSWER, true));
                a.composer = Some(Composer::FollowUp);
                a.update_placeholder();
                a.render();
            })
        }));
        at(600, shot("6-light-answer.png"));
        at(100, Box::new(|| {
            with_app(|a| {
                a.new_chat();
                a.update_placeholder();
            })
        }));
        at(500, shot("6-light-empty.png"));
        at(100, Box::new(|| {
            with_app(|a| {
                a.cfg.theme = "dark".into();
                a.apply_theme();
            })
        }));
    }
    if want("7") {
        // "on top"
        at(300, Box::new(|| {
            with_app(|a| {
                a.new_chat();
                a.turns.push(answer("[скриншот]", "B) Канберра", true));
                a.render();
                a.show_overlay();
                a.ui().set_ov_left(0.6);
            })
        }));
        at(700, shot("7-overlay.png"));
        at(100, Box::new(|| with_app(|a| a.show_window())));
    }
    at(500, Box::new(|| {
        let _ = slint::quit_event_loop();
    }));
}
