//! Interface language of the Rust side (the .slint texts are translated with bundled .po files).
//! Russian is the source language: `tr` returns the text as it is, or its English version.

use std::sync::atomic::{AtomicBool, Ordering};

static EN: AtomicBool = AtomicBool::new(false);

pub fn is_en() -> bool {
    EN.load(Ordering::Relaxed)
}

pub fn set_en(on: bool) {
    EN.store(on, Ordering::Relaxed);
}

/// The text in the current language. Unknown texts stay as they are.
pub fn tr(s: &str) -> String {
    if is_en() {
        if let Some((_, en)) = TABLE.iter().find(|(ru, _)| *ru == s) {
            return (*en).to_string();
        }
    }
    s.to_string()
}

/// Like `tr`, then replaces every `{name}` with its value.
pub fn trf(s: &str, args: &[(&str, &str)]) -> String {
    let mut out = tr(s);
    for (k, v) in args {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

const TABLE: &[(&str, &str)] = &[
    // window and tray
    ("Открыть", "Open"),
    ("Настройки", "Settings"),
    ("Выход", "Quit"),
    ("Вопрос по выделенному тексту", "Question about the selected text"),
    ("Скриншот", "Screenshot"),
    ("Свободный вопрос", "Free question"),
    ("Повторить запрос", "Repeat the request"),
    // dates
    ("сегодня, {hm}", "today, {hm}"),
    ("вчера, {hm}", "yesterday, {hm}"),
    // files
    ("файл", "file"),
    ("Пропущено: {list}", "Skipped: {list}"),
    ("картинка (вставьте её через Ctrl+V)", "a picture (paste it with Ctrl+V)"),
    ("не текстовый файл", "not a text file"),
    ("не открылся", "could not be opened"),
    ("слишком большой", "too large"),
    ("пустой", "empty"),
    ("\n… (файл обрезан)", "\n… (file truncated)"),
    ("ещё {n} не добавлено (лимит или не текст)", "{n} more not added (limit reached or not text)"),
    ("{name}: {why}", "{name}: {why}"),
    ("[скриншот]", "[screenshot]"),
    ("[скриншот x{n}]", "[screenshot x{n}]"),
    ("[файлы]", "[files]"),
    (" [файлы: {names}]", " [files: {names}]"),
    (" [файлы: {names} и ещё {n}]", " [files: {names} and {n} more]"),
    // composer and statuses
    ("Уточни ответ или прикрепи файл", "Follow up or attach a file"),
    ("Думаю…", "Thinking…"),
    ("Запрос остановлен.", "Request stopped."),
    ("\n\n[остановлено]", "\n\n[stopped]"),
    ("Ошибка: {e}", "Error: {e}"),
    ("\n\n{key} — повторить запрос", "\n\n{key} — repeat the request"),
    ("Нет запроса для повтора. Сначала задай вопрос или сделай снимок.", "Nothing to repeat. Ask a question or take a screenshot first."),
    ("История очищена", "History cleared"),
    ("Добавь ключ Groq API в настройках", "Add a Groq API key in the settings"),
    ("Добавь ключ Gemini API в настройках", "Add a Gemini API key in the settings"),
    ("Задай любой вопрос", "Ask anything"),
    ("Задай вопрос", "Ask a question"),
    (
        "{sel} — вопрос по выделенному тексту\n{shot} — скриншот\n{retry} — повторить последний запрос",
        "{sel} — a question about the selected text\n{shot} — screenshot\n{retry} — repeat the last request",
    ),
    ("Шестерёнка вверху справа открывает настройки.", "The gear at the top right opens the settings."),
    ("Не удалось скопировать выделение. Проверь, что приложение разрешает Ctrl+C.", "Could not copy the selection. Check that the app allows Ctrl+C."),
    ("Не удалось скопировать текст. Проверь выделение и разрешение Ctrl+C.", "Could not copy the text. Check the selection and that Ctrl+C is allowed."),
    ("Выделено: {preview}", "Selected: {preview}"),
    ("Напиши запрос", "Write your request"),
    ("Снимок не удался.", "The screenshot failed."),
    ("Снимок экрана готов", "Screenshot ready"),
    ("Снимков экрана: {n}", "Screenshots: {n}"),
    ("Что сделать со снимком?", "What should be done with the screenshot?"),
    ("\n{key} — добавить ещё снимок (до {max}).", "\n{key} — add another screenshot (up to {max})."),
    (
        "Enter с пустым полем — модель решит задание со снимка.{more}",
        "Enter with an empty field — the model will solve the task from the screenshot.{more}",
    ),
    ("Снимок экрана не удался.", "The screenshot failed."),
    // settings
    ("Ключ API Groq", "Groq API key"),
    ("Ключ API Gemini", "Gemini API key"),
    ("Ключ сохранён (введи новый, чтобы заменить)", "Key saved (enter a new one to replace it)"),
    ("Вставь ключ Groq API", "Paste a Groq API key"),
    ("Вставь ключ Google AI Studio", "Paste a Google AI Studio key"),
    ("Открыть Groq Console", "Open Groq Console"),
    ("Открыть Google AI Studio", "Open Google AI Studio"),
    ("Версия {v}", "Version {v}"),
    ("Введи ID своей модели Gemini", "Enter the ID of your Gemini model"),
    ("Не удалось сохранить настройки. Проверь доступ к папке данных.", "Could not save the settings. Check access to the data folder."),
    ("Клавиши заняты другой программой: {keys}. Поменяй их в настройках.", "These keys are used by another program: {keys}. Change them in the settings."),
    ("Сохранено", "Saved"),
    ("Проверяю…", "Checking…"),
    ("Доступна новая версия {v}", "A new version {v} is available"),
    ("Обновления пока не настроены.", "Updates are not set up yet."),
    ("Есть новая версия {v}: нажми «Обновить» в полосе над ответом.", "A new version {v} is available: press «Update» in the strip above the answer."),
    ("У тебя последняя версия.", "You have the latest version."),
    ("Не удалось проверить: {e}.", "Could not check: {e}."),
    ("Скачиваю версию {v}…", "Downloading version {v}…"),
    ("Обновление не удалось: {e}", "The update failed: {e}"),
    // validation
    ("Неизвестный провайдер модели", "Unknown model provider"),
    ("Ключ {name} выглядит неверно", "The {name} key looks wrong"),
    ("Недопустимое имя модели Gemini", "Invalid Gemini model name"),
    ("Недопустимое сочетание клавиш: {a}", "Invalid key combination: {a}"),
    ("Два действия используют одинаковые клавиши", "Two actions use the same keys"),
    ("Неизвестный акцентный цвет", "Unknown accent colour"),
    ("Прозрачность должна быть от {min} до 100%", "Opacity must be from {min} to 100%"),
    ("Промпт слишком длинный (больше {max} символов)", "The prompt is too long (over {max} characters)"),
    // file dialog
    ("Выберите папку с файлами", "Choose a folder with files"),
    ("Выберите файлы", "Choose files"),
    // updates
    ("ответ GitHub не прочитан: {e}", "could not read the GitHub reply: {e}"),
    ("GitHub ответил кодом {code}", "GitHub replied with code {code}"),
    ("нет связи с GitHub ({e})", "no connection to GitHub ({e})"),
    ("скачивание не удалось ({e})", "the download failed ({e})"),
    ("скачивание прервалось ({e})", "the download was interrupted ({e})"),
    ("скачанный файл повреждён (не совпала контрольная сумма)", "the downloaded file is damaged (checksum mismatch)"),
    ("не удалось сохранить файл ({e})", "could not save the file ({e})"),
    ("в выпуске нет установщика", "the release has no installer"),
    ("установщик не запустился ({e})", "the installer did not start ({e})"),
    ("в выпуске нет портативной версии", "the release has no portable version"),
    ("нет доступа к папке программы ({e})", "no access to the program folder ({e})"),
    ("не удалось заменить файл ({e})", "could not replace the file ({e})"),
    ("новая версия не запустилась ({e})", "the new version did not start ({e})"),
    // model errors
    ("Пустой запрос", "Empty request"),
    ("Неверный или заблокированный API-ключ Groq.", "Invalid or blocked Groq API key."),
    ("Лимит запросов Groq. Подожди немного и повтори.", "Groq request limit reached. Wait a little and try again."),
    ("Модель Qwen 3.8 27B сейчас недоступна в Groq.", "The Qwen 3.8 27B model is currently unavailable on Groq."),
    ("Groq временно перегружен. Повтори позже.", "Groq is temporarily overloaded. Try again later."),
    ("Groq не ответил вовремя.", "Groq did not answer in time."),
    ("Нет связи с Groq.", "No connection to Groq."),
    ("Неверный или заблокированный API-ключ.", "Invalid or blocked API key."),
    ("Лимит Gemini. Подожди минуту.", "Gemini limit reached. Wait a minute."),
    ("Gemini перегружен. Повтори позже.", "Gemini is overloaded. Try again later."),
    ("Gemini не ответил вовремя.", "Gemini did not answer in time."),
    ("Нет связи с Gemini.", "No connection to Gemini."),
    ("Ключ не введён.", "No key entered."),
    ("Непонятный ответ Groq: {e}", "Unclear reply from Groq: {e}"),
    ("Ключ работает", "The key works"),
    ("Ключ работает, но модель {m} сейчас недоступна", "The key works, but the model {m} is unavailable right now"),
    ("Недопустимое имя модели", "Invalid model name"),
    ("Ключ работает, модель {m} доступна", "The key works, the model {m} is available"),
    ("Ключ работает, но модель {m} не найдена", "The key works, but the model {m} was not found"),
    ("API-ключ не задан. Открой настройки.", "No API key set. Open the settings."),
    ("нет вариантов", "no candidates"),
    ("Пустой ответ ({why})", "Empty answer ({why})"),
    ("API-ключ Groq не задан. Открой настройки.", "No Groq API key set. Open the settings."),
    ("Groq принимает не более трёх изображений за запрос.", "Groq accepts at most three images per request."),
    ("Ошибка потока Groq", "Groq stream error"),
    ("Groq вернул пустой ответ.", "Groq returned an empty answer."),
];
