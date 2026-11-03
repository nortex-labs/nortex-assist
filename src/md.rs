//! Turns the model's light Markdown into display blocks (headings, lists, code, formulas) and
//! splits an answer into its short and detailed halves.

/// Written by the model between the short answer and the details (see `llm::TWO_LEVEL_NOTE`).
pub const MORE_MARK: &str = "<<<MORE>>>";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Para = 0,
    Heading = 1,
    Bullet = 2,
    Numbered = 3,
    Quote = 4,
    Code = 5,
    Formula = 6,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub kind: Kind,
    /// the text to show; for `Para`/`Bullet`/`Numbered`/`Quote`/`Heading` it still holds inline Markdown
    pub text: String,
    /// list number ("1.") or code language
    pub label: String,
}

/// Short and detailed parts of an answer. `detail` is `None` when the model gave only one level.
pub fn split_levels(text: &str) -> (String, Option<String>) {
    match text.find(MORE_MARK) {
        Some(i) => {
            let short = text[..i].trim().to_string();
            let detail = text[i + MORE_MARK.len()..].trim().to_string();
            (short, if detail.is_empty() { None } else { Some(detail) })
        }
        None => {
            // while streaming the marker may be half written: do not show "<<<MO" to the user
            let t = text.trim_end();
            let mut cut = t.len();
            for n in (1..MORE_MARK.len()).rev() {
                if t.is_char_boundary(t.len().saturating_sub(n)) && t.len() >= n && MORE_MARK.starts_with(&t[t.len() - n..]) {
                    cut = t.len() - n;
                    break;
                }
            }
            (t[..cut].trim().to_string(), None)
        }
    }
}

/// The answer without the marker, for copying and for the chat history sent back to the model.
pub fn without_mark(text: &str) -> String {
    match text.find(MORE_MARK) {
        Some(_) => {
            let (s, d) = split_levels(text);
            match d {
                Some(d) => format!("{s}\n\n{d}"),
                None => s,
            }
        }
        None => text.to_string(),
    }
}

/// Plain text of parsed blocks: no Markdown marks, formulas as text, code as it is.
fn plain_blocks(blocks: Vec<Block>) -> Vec<String> {
    blocks
        .into_iter()
        .map(|b| match b.kind {
            Kind::Code | Kind::Formula => b.text,
            Kind::Bullet => format!("- {}", plain_inline(&b.text)),
            Kind::Numbered => format!("{} {}", b.label, plain_inline(&b.text)),
            _ => plain_inline(&b.text),
        })
        .collect()
}

/// What the copy button puts on the clipboard: the short answer only, without any formatting marks.
pub fn short_plain(text: &str) -> String {
    let (short, _) = split_levels(text);
    plain_blocks(parse(&short)).join("\n")
}

/// The answer as plain selectable text: the short part, and the details when they are open.
pub fn plain_visible(text: &str, with_detail: bool) -> String {
    let (short, detail) = split_levels(text);
    let mut lines = plain_blocks(parse(&short));
    if let (true, Some(d)) = (with_detail, detail) {
        lines.push(String::new());
        lines.extend(plain_blocks(parse(&d)));
    }
    lines.join("\n")
}

pub fn parse(text: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut para: Vec<String> = Vec::new();
    let mut code: Option<(String, Vec<String>)> = None;
    let mut formula: Option<Vec<String>> = None;

    fn flush(out: &mut Vec<Block>, para: &mut Vec<String>) {
        if !para.is_empty() {
            out.push(Block { kind: Kind::Para, text: para.join("\n"), label: String::new() });
            para.clear();
        }
    }

    for raw in text.replace("\r\n", "\n").lines() {
        let line = raw.trim_end();
        let t = line.trim_start();

        if let Some((lang, lines)) = code.as_mut() {
            if t.starts_with("```") {
                let (lang, lines) = (lang.clone(), lines.clone());
                code = None;
                out.push(Block { kind: Kind::Code, text: lines.join("\n"), label: lang });
            } else {
                lines.push(line.to_string());
            }
            continue;
        }
        if let Some(lines) = formula.as_mut() {
            if t.contains("$$") || t.contains("\\]") {
                let end = t.split("$$").next().unwrap_or("").trim_end_matches("\\]").to_string();
                if !end.trim().is_empty() {
                    lines.push(end);
                }
                let body = lines.join(" ");
                formula = None;
                out.push(Block { kind: Kind::Formula, text: latex(&body), label: String::new() });
            } else {
                lines.push(t.to_string());
            }
            continue;
        }

        if let Some(rest) = t.strip_prefix("```") {
            flush(&mut out, &mut para);
            code = Some((rest.trim().to_string(), Vec::new()));
            continue;
        }
        // display formula: $$ ... $$ on one line or spread over several
        if t.starts_with("$$") || t.starts_with("\\[") {
            flush(&mut out, &mut para);
            let inner = t.trim_start_matches("$$").trim_start_matches("\\[");
            if let Some(end) = inner.find("$$").or_else(|| inner.find("\\]")) {
                out.push(Block { kind: Kind::Formula, text: latex(inner[..end].trim()), label: String::new() });
            } else {
                formula = Some(if inner.trim().is_empty() { Vec::new() } else { vec![inner.trim().to_string()] });
            }
            continue;
        }
        if t.is_empty() {
            flush(&mut out, &mut para);
            continue;
        }
        if t.chars().all(|c| c == '-' || c == '*' || c == '_') && t.len() >= 3 {
            flush(&mut out, &mut para);
            continue; // horizontal rule: the gap between blocks is enough
        }
        if t.starts_with('#') {
            let hashes = t.chars().take_while(|&c| c == '#').count();
            if hashes <= 6 && t[hashes..].starts_with(' ') {
                flush(&mut out, &mut para);
                out.push(Block { kind: Kind::Heading, text: t[hashes..].trim().to_string(), label: hashes.to_string() });
                continue;
            }
        }
        if let Some(rest) = t.strip_prefix("> ").or_else(|| t.strip_prefix('>')) {
            flush(&mut out, &mut para);
            out.push(Block { kind: Kind::Quote, text: rest.trim().to_string(), label: String::new() });
            continue;
        }
        if let Some(rest) = ["- ", "* ", "• ", "+ "].iter().find_map(|p| t.strip_prefix(p)) {
            flush(&mut out, &mut para);
            out.push(Block { kind: Kind::Bullet, text: rest.trim().to_string(), label: String::new() });
            continue;
        }
        let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && digits <= 3 {
            let rest = &t[digits..];
            if let Some(body) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
                flush(&mut out, &mut para);
                out.push(Block { kind: Kind::Numbered, text: body.trim().to_string(), label: format!("{}.", &t[..digits]) });
                continue;
            }
        }
        para.push(t.to_string());
    }
    // an unfinished fence (the answer is still streaming) is shown as code so far
    if let Some((lang, lines)) = code {
        out.push(Block { kind: Kind::Code, text: lines.join("\n"), label: lang });
    }
    if let Some(lines) = formula {
        out.push(Block { kind: Kind::Formula, text: latex(&lines.join(" ")), label: String::new() });
    }
    flush(&mut out, &mut para);
    out
}

/// Removes inline Markdown marks and turns `$...$` formulas into readable text.
pub fn plain_inline(s: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '*' | '_' if i + 1 < chars.len() && chars[i + 1] == c => i += 2,
            '*' => i += 1,
            '`' => i += 1,
            '$' => {
                // inline formula up to the next unescaped $ on the same line
                if let Some(end) = chars[i + 1..].iter().position(|&x| x == '$') {
                    let body: String = chars[i + 1..i + 1 + end].iter().collect();
                    if !body.trim().is_empty() && !body.starts_with(' ') {
                        out.push_str(&latex(&body));
                        i += end + 2;
                        continue;
                    }
                }
                out.push(c);
                i += 1;
            }
            '[' => {
                // [text](url) keeps only the text
                let rest: String = chars[i..].iter().collect();
                if let Some(close) = rest.find("](") {
                    if let Some(paren) = rest[close..].find(')') {
                        out.push_str(&rest[1..close]);
                        i += rest[..close + paren].chars().count() + 1;
                        continue;
                    }
                }
                out.push(c);
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Backslash-escapes what the Markdown reader would take for formatting.
fn escape_md(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if matches!(c, '*' | '_' | '`' | '\\' | '<' | '>' | '[' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Markdown for the `StyledText` element: bold terms get the accent colour, `$...$` becomes text.
pub fn inline_markdown(s: &str, accent: &str) -> String {
    // formulas first, so their symbols are not read as Markdown
    let mut text = String::new();
    let mut rest = s;
    while let Some(a) = rest.find('$') {
        let after = &rest[a + 1..];
        match after.find('$') {
            Some(b) if !after[..b].trim().is_empty() && !after.starts_with(' ') => {
                text.push_str(&rest[..a]);
                text.push_str(&format!("*{}*", escape_md(&latex(&after[..b]))));
                rest = &after[b + 1..];
            }
            _ => {
                text.push_str(&rest[..a + 1]);
                rest = after;
            }
        }
    }
    text.push_str(rest);
    // **term** -> bold accent-coloured term
    let mut out = String::new();
    let mut parts = text.split("**");
    if let Some(first) = parts.next() {
        out.push_str(first);
    }
    let mut open = true;
    for p in parts {
        if open && !p.is_empty() && !p.contains('\n') {
            out.push_str(&format!("<font color=\"{accent}\">**{p}**</font>"));
        } else {
            out.push_str(p);
        }
        open = !open;
    }
    out
}

const SYMBOLS: &[(&str, &str)] = &[
    ("\\leftrightarrow", "↔"), ("\\Rightarrow", "⇒"), ("\\rightarrow", "→"), ("\\Leftarrow", "⇐"), ("\\leftarrow", "←"),
    ("\\Leftrightarrow", "⇔"), ("\\implies", "⇒"), ("\\iff", "⇔"), ("\\longrightarrow", "⟶"), ("\\to", "→"),
    ("\\approx", "≈"), ("\\neq", "≠"), ("\\ne", "≠"), ("\\leq", "≤"), ("\\le", "≤"), ("\\geq", "≥"), ("\\ge", "≥"),
    ("\\equiv", "≡"), ("\\sim", "∼"), ("\\propto", "∝"), ("\\pm", "±"), ("\\mp", "∓"), ("\\times", "×"), ("\\cdot", "·"),
    ("\\div", "÷"), ("\\infty", "∞"), ("\\partial", "∂"), ("\\nabla", "∇"), ("\\sum", "∑"), ("\\prod", "∏"),
    ("\\int", "∫"), ("\\oint", "∮"), ("\\in", "∈"), ("\\notin", "∉"), ("\\subseteq", "⊆"), ("\\subset", "⊂"),
    ("\\supseteq", "⊇"), ("\\supset", "⊃"), ("\\cup", "∪"), ("\\cap", "∩"), ("\\emptyset", "∅"), ("\\varnothing", "∅"),
    ("\\forall", "∀"), ("\\exists", "∃"), ("\\neg", "¬"), ("\\land", "∧"), ("\\lor", "∨"), ("\\angle", "∠"),
    ("\\degree", "°"), ("\\circ", "°"), ("\\ldots", "…"), ("\\dots", "…"), ("\\cdots", "⋯"), ("\\hbar", "ħ"),
    ("\\alpha", "α"), ("\\beta", "β"), ("\\gamma", "γ"), ("\\delta", "δ"), ("\\epsilon", "ε"), ("\\varepsilon", "ε"),
    ("\\zeta", "ζ"), ("\\eta", "η"), ("\\theta", "θ"), ("\\vartheta", "ϑ"), ("\\iota", "ι"), ("\\kappa", "κ"),
    ("\\lambda", "λ"), ("\\mu", "μ"), ("\\nu", "ν"), ("\\xi", "ξ"), ("\\pi", "π"), ("\\rho", "ρ"), ("\\sigma", "σ"),
    ("\\tau", "τ"), ("\\upsilon", "υ"), ("\\varphi", "φ"), ("\\phi", "φ"), ("\\chi", "χ"), ("\\psi", "ψ"), ("\\omega", "ω"),
    ("\\Gamma", "Γ"), ("\\Delta", "Δ"), ("\\Theta", "Θ"), ("\\Lambda", "Λ"), ("\\Xi", "Ξ"), ("\\Pi", "Π"),
    ("\\Sigma", "Σ"), ("\\Phi", "Φ"), ("\\Psi", "Ψ"), ("\\Omega", "Ω"),
    ("\\lim", "lim"), ("\\log", "log"), ("\\ln", "ln"), ("\\sin", "sin"), ("\\cos", "cos"), ("\\tan", "tan"),
    ("\\tg", "tg"), ("\\ctg", "ctg"), ("\\exp", "exp"), ("\\min", "min"), ("\\max", "max"),
    ("\\quad", "  "), ("\\qquad", "    "), ("\\,", " "), ("\\;", " "), ("\\:", " "), ("\\!", ""), ("\\ ", " "),
    ("\\left", ""), ("\\right", ""), ("\\displaystyle", ""), ("\\big", ""), ("\\Big", ""),
    ("\\{", "{"), ("\\}", "}"), ("\\%", "%"), ("\\&", "&"), ("\\#", "#"), ("\\_", "_"),
];

fn sup_char(c: char) -> Option<char> {
    Some(match c {
        '0' => '⁰', '1' => '¹', '2' => '²', '3' => '³', '4' => '⁴', '5' => '⁵', '6' => '⁶', '7' => '⁷', '8' => '⁸', '9' => '⁹',
        '+' => '⁺', '-' | '−' => '⁻', '=' => '⁼', '(' => '⁽', ')' => '⁾', 'n' => 'ⁿ', 'i' => 'ⁱ', 'x' => 'ˣ', 'y' => 'ʸ',
        'a' => 'ᵃ', 'b' => 'ᵇ', 'c' => 'ᶜ', 'd' => 'ᵈ', 'e' => 'ᵉ', 'k' => 'ᵏ', 'm' => 'ᵐ', 't' => 'ᵗ',
        _ => return None,
    })
}

fn sub_char(c: char) -> Option<char> {
    Some(match c {
        '0' => '₀', '1' => '₁', '2' => '₂', '3' => '₃', '4' => '₄', '5' => '₅', '6' => '₆', '7' => '₇', '8' => '₈', '9' => '₉',
        '+' => '₊', '-' | '−' => '₋', '=' => '₌', '(' => '₍', ')' => '₎', 'a' => 'ₐ', 'e' => 'ₑ', 'i' => 'ᵢ', 'j' => 'ⱼ',
        'k' => 'ₖ', 'l' => 'ₗ', 'm' => 'ₘ', 'n' => 'ₙ', 'o' => 'ₒ', 'p' => 'ₚ', 'r' => 'ᵣ', 's' => 'ₛ', 't' => 'ₜ', 'x' => 'ₓ',
        _ => return None,
    })
}

/// Reads one `{...}` group (or one character) starting at `i`; returns the content and the next index.
fn group(chars: &[char], i: usize) -> (String, usize) {
    if i >= chars.len() {
        return (String::new(), i);
    }
    if chars[i] == '{' {
        let mut depth = 0;
        for (j, &c) in chars.iter().enumerate().skip(i) {
            if c == '{' {
                depth += 1;
            } else if c == '}' {
                depth -= 1;
                if depth == 0 {
                    return (chars[i + 1..j].iter().collect(), j + 1);
                }
            }
        }
        return (chars[i + 1..].iter().collect(), chars.len());
    }
    if chars[i] == '\\' {
        let mut j = i + 1;
        while j < chars.len() && chars[j].is_ascii_alphabetic() {
            j += 1;
        }
        return (chars[i..j.max(i + 2).min(chars.len())].iter().collect(), j.max(i + 2).min(chars.len()));
    }
    (chars[i].to_string(), i + 1)
}

fn script(body: &str, up: bool) -> String {
    let conv = |c: char| if up { sup_char(c) } else { sub_char(c) };
    // a comma has no small form: x_{1,2} reads x₁,₂
    if body.chars().all(|c| conv(c).is_some() || c == ',') && body.chars().any(|c| c != ',') {
        body.chars().filter_map(|c| if c == ',' { Some(',') } else { conv(c) }).collect()
    } else if body.chars().count() == 1 {
        format!("{}{}", if up { "^" } else { "_" }, body)
    } else {
        format!("{}({})", if up { "^" } else { "_" }, body)
    }
}

/// A readable one-line version of a LaTeX formula (fractions, roots, powers, Greek letters, operators).
pub fn latex(src: &str) -> String {
    let src = src.trim();
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            let rest: String = chars[i..].iter().collect();
            if rest.starts_with("\\frac") || rest.starts_with("\\dfrac") || rest.starts_with("\\tfrac") {
                let skip = if rest.starts_with("\\frac") { 5 } else { 6 };
                let (a, j) = group(&chars, i + skip);
                let (b, k) = group(&chars, j);
                let (a, b) = (latex(&a), latex(&b));
                let wrap = |s: &str| if s.chars().count() > 1 && !s.chars().all(|c| c.is_alphanumeric() || c == '.') { format!("({s})") } else { s.to_string() };
                out.push_str(&format!("{}⁄{}", wrap(&a), wrap(&b)).replace('⁄', "/"));
                i = k;
                continue;
            }
            if rest.starts_with("\\sqrt") {
                let mut j = i + 5;
                let mut index = String::new();
                if chars.get(j) == Some(&'[') {
                    if let Some(e) = chars[j..].iter().position(|&x| x == ']') {
                        index = chars[j + 1..j + e].iter().collect();
                        j += e + 1;
                    }
                }
                let (a, k) = group(&chars, j);
                let a = latex(&a);
                let root = match index.as_str() {
                    "" | "2" => "√",
                    "3" => "∛",
                    "4" => "∜",
                    _ => "√",
                };
                if a.chars().count() > 1 && !a.chars().all(|c| c.is_alphanumeric() || c == '.') {
                    out.push_str(&format!("{root}({a})"));
                } else {
                    out.push_str(&format!("{root}{a}"));
                }
                i = k;
                continue;
            }
            if rest.starts_with("\\text") || rest.starts_with("\\mathrm") || rest.starts_with("\\mathbf") || rest.starts_with("\\mathit") || rest.starts_with("\\operatorname") || rest.starts_with("\\mbox") {
                let name_len = rest.chars().take_while(|&x| x == '\\' || x.is_ascii_alphabetic()).count();
                let (a, k) = group(&chars, i + name_len);
                out.push_str(&latex(&a));
                i = k;
                continue;
            }
            if rest.starts_with("\\vec") || rest.starts_with("\\overrightarrow") {
                let name_len = rest.chars().take_while(|&x| x == '\\' || x.is_ascii_alphabetic()).count();
                let (a, k) = group(&chars, i + name_len);
                out.push_str(&latex(&a));
                out.push('\u{20D7}');
                i = k;
                continue;
            }
            if rest.starts_with("\\bar") || rest.starts_with("\\overline") {
                let name_len = rest.chars().take_while(|&x| x == '\\' || x.is_ascii_alphabetic()).count();
                let (a, k) = group(&chars, i + name_len);
                out.push_str(&latex(&a));
                out.push('\u{0304}');
                i = k;
                continue;
            }
            if rest.starts_with("\\hat") {
                let (a, k) = group(&chars, i + 4);
                out.push_str(&latex(&a));
                out.push('\u{0302}');
                i = k;
                continue;
            }
            if rest.starts_with("\\\\") {
                out.push_str("; ");
                i += 2;
                continue;
            }
            // longest known command that is not just the beginning of a longer word
            let mut best: Option<(&str, &str)> = None;
            for &(k, v) in SYMBOLS {
                if rest.starts_with(k) && best.is_none_or(|(bk, _)| k.len() > bk.len()) {
                    let tail = rest[k.len()..].chars().next();
                    let letters = k.chars().last().is_some_and(|l| l.is_ascii_alphabetic());
                    if !(letters && tail.is_some_and(|t| t.is_ascii_alphabetic())) {
                        best = Some((k, v));
                    }
                }
            }
            if let Some((k, v)) = best {
                out.push_str(v);
                i += k.chars().count();
                continue;
            }
            // unknown command: keep its name without the slash
            let name: String = chars[i + 1..].iter().take_while(|x| x.is_ascii_alphabetic()).collect();
            out.push_str(&name);
            i += 1 + name.chars().count();
            continue;
        }
        if c == '^' || c == '_' {
            let (body, k) = group(&chars, i + 1);
            out.push_str(&script(&latex(&body), c == '^'));
            i = k;
            continue;
        }
        match c {
            '{' | '}' => {}
            '~' => out.push(' '),
            '-' if chars.get(i + 1).is_some_and(|n| n.is_ascii_digit() || n.is_alphabetic()) && i > 0 && chars[i - 1] != ' ' && chars[i - 1] != '(' && chars[i - 1] != '=' => out.push('−'),
            _ => out.push(c),
        }
        i += 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ").replace(" - ", " − ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_two_levels() {
        let (s, d) = split_levels("Ответ: 5\n<<<MORE>>>\nПотому что 2+3.");
        assert_eq!(s, "Ответ: 5");
        assert_eq!(d.as_deref(), Some("Потому что 2+3."));
        let (s, d) = split_levels("Ответ: 5\n<<<MO");
        assert_eq!(s, "Ответ: 5");
        assert!(d.is_none());
        assert_eq!(without_mark("A\n<<<MORE>>>\nB"), "A\n\nB");
    }

    #[test]
    fn parses_blocks() {
        let b = parse("## Итог\nТекст **важно**\n\n- раз\n- два\n1. шаг\n```rust\nfn main() {}\n```\n$$x^2 + 1$$");
        let kinds: Vec<Kind> = b.iter().map(|x| x.kind).collect();
        assert_eq!(kinds, vec![Kind::Heading, Kind::Para, Kind::Bullet, Kind::Bullet, Kind::Numbered, Kind::Code, Kind::Formula]);
        assert_eq!(b[5].label, "rust");
        assert_eq!(b[5].text, "fn main() {}");
        assert_eq!(b[6].text, "x²+ 1".replace("²+", "² +"));
    }

    #[test]
    fn latex_reads_well() {
        assert_eq!(latex("\\frac{a+b}{2}"), "(a+b)/2");
        assert_eq!(latex("x^2 + y_1"), "x² + y₁");
        assert_eq!(latex("x_{1,2}"), "x₁,₂");
        assert_eq!(latex("\\sqrt{x^2+1}"), "√(x²+1)");
        assert_eq!(latex("\\alpha \\cdot \\pi \\le 3"), "α · π ≤ 3");
        assert_eq!(latex("e^{i\\pi} + 1 = 0"), "eⁱπ + 1 = 0".replace("ⁱπ", "^(iπ)"));
        assert_eq!(plain_inline("a **b** `c` $x^2$"), "a b c x²");
    }
}
