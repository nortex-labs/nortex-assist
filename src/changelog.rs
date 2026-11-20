//! "What's new": the changes of every version in plain words, built into the program (no internet
//! needed), in Russian and English. The newest version comes first.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tag {
    New = 0,
    Better = 1,
    Fixed = 2,
}

pub struct Item {
    pub tag: Tag,
    pub ru: &'static str,
    pub en: &'static str,
}

pub struct Version {
    pub version: &'static str,
    /// (day, month)
    pub date: (u32, u32),
    pub items: &'static [Item],
}

const fn n(ru: &'static str, en: &'static str) -> Item {
    Item { tag: Tag::New, ru, en }
}
const fn b(ru: &'static str, en: &'static str) -> Item {
    Item { tag: Tag::Better, ru, en }
}
const fn f(ru: &'static str, en: &'static str) -> Item {
    Item { tag: Tag::Fixed, ru, en }
}

pub const VERSIONS: &[Version] = &[
    Version {
        version: "1.4.0",
        date: (20, 11),
        items: &[
            n("Одна клавиша на всё: Alt+W сам понимает, спросить про выделенный текст или сделать снимок экрана, и ответ приходит сразу.", "One key for everything: Alt+W figures out whether to ask about the selected text or take a screenshot, and the answer comes at once."),
            n("Голосовой ввод: зажми F8, скажи вопрос и отпусти.", "Voice input: hold F8, say your question and let go."),
            n("Кнопки под ответом: «Объясни», «Короче», «Переведи», «Другой вариант».", "Buttons under the answer: Explain, Shorter, Translate, Another answer."),
            n("Когда у Groq кончается лимит, на этот вопрос отвечает Gemini.", "When Groq runs out of its limit, Gemini answers that question."),
            b("Новый спокойный вид: компактное окно, вопрос и ответ как в мессенджере, простые настройки и «Расширенные».", "A new calm look: a compact window, question and answer like in a messenger, simple settings plus Advanced."),
            b("Тема и язык сами берутся из Windows.", "The theme and language follow Windows."),
            b("Ответ всегда приходит целиком: кнопки «Подробнее» больше нет, объяснение — по кнопке «Объясни».", "The answer always comes in full: the More button is gone, use Explain instead."),
            f("Окно иногда открывалось большим, размером для настроек.", "The window sometimes opened as large as the settings."),
            f("Подробный ответ на большое задание иногда не приходил: «Groq вернул пустой ответ».", "A detailed answer to a big task sometimes did not come: \"Groq returned an empty answer\"."),
            f("После Alt+Q пропадали скопированные файлы и оформление текста в буфере обмена.", "After Alt+Q, copied files and text formatting disappeared from the clipboard."),
        ],
    },
    Version {
        version: "1.3.3",
        date: (18, 11),
        items: &[
            b("Поле запроса переносит строки и растёт, пока вы печатаете. Enter отправляет, Shift+Enter — новая строка.", "The question field wraps and grows while you type. Enter sends, Shift+Enter starts a new line."),
            f("Длинный запрос и длинный ключ API уходили за край поля.", "A long question or API key ran past the edge of the field."),
            f("Длинный ответ иногда не листался до конца.", "A long answer sometimes could not be scrolled to the end."),
        ],
    },
    Version {
        version: "1.3.1",
        date: (3, 11),
        items: &[
            n("Аккуратные ответы: заголовки, списки, блоки кода и формулы.", "Tidy answers: headings, lists, code blocks and formulas."),
            n("Скрепка: к вопросу можно прикрепить текстовые файлы.", "Paperclip: attach text files to a question."),
            n("Английский язык интерфейса и ответов.", "English interface and answers."),
            n("Режим «Поверх окон» для игр, видео и лекций.", "The On top mode for games, videos and lectures."),
            b("Язык ответа по умолчанию — русский, если вопрос не на другом языке.", "Answers are in Russian by default unless the question is in another language."),
            f("Groq иногда отвечал ошибкой «Request too large».", "Groq sometimes replied with a Request too large error."),
        ],
    },
    Version {
        version: "1.3.0",
        date: (1, 11),
        items: &[
            n("Свой выбор области для скриншота: экран замирает, обведите нужное мышью.", "Own area picker for screenshots: the screen freezes, drag around what you need."),
            n("Свой акцентный цвет и настройка значка в трее.", "Your own accent colour and a tray icon setting."),
            b("В простое программа почти не занимает память.", "When idle the program uses almost no memory."),
            b("Плавные анимации окна, панелей и кнопок.", "Smooth animations of the window, panels and buttons."),
            f("Окно со скриншотом появлялось только после лишнего клика.", "The screenshot window appeared only after an extra click."),
        ],
    },
    Version {
        version: "1.2.0",
        date: (31, 10),
        items: &[
            n("Обновления приходят сами: полоса «Обновить» над ответом.", "Updates arrive by themselves: the Update strip above the answer."),
            n("Кнопка «Стоп», подсказки у кнопок, миниатюры скриншотов.", "A Stop button, tooltips, screenshot thumbnails."),
            n("Свой промпт и заготовки стиля ответов.", "Own prompt and answer style presets."),
        ],
    },
    Version {
        version: "1.1.0",
        date: (31, 10),
        items: &[n("Скриншот всего экрана без «Ножниц».", "A whole-screen screenshot without the Snipping Tool.")],
    },
    Version {
        version: "1.0.0",
        date: (31, 10),
        items: &[n("Первая версия: вопросы по выделенному тексту, скриншоту и в чате.", "The first version: questions about selected text, screenshots and in the chat.")],
    },
];

const MONTHS_RU: [&str; 12] = ["января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря"];
const MONTHS_EN: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// "20 ноября" / "November 20"
pub fn date_text(v: &Version) -> String {
    let (d, m) = v.date;
    let i = (m.clamp(1, 12) - 1) as usize;
    if crate::i18n::is_en() { format!("{} {d}", MONTHS_EN[i]) } else { format!("{d} {}", MONTHS_RU[i]) }
}

pub fn item_text(it: &Item) -> &'static str {
    if crate::i18n::is_en() { it.en } else { it.ru }
}

#[cfg(test)]
mod tests {
    #[test]
    fn current_version_is_described() {
        assert_eq!(super::VERSIONS[0].version, env!("CARGO_PKG_VERSION"), "add the new version to changelog.rs");
        for v in super::VERSIONS {
            assert!(!v.items.is_empty());
        }
    }
}
