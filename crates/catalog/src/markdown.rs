//! A small, safe Markdown subset for FAQ answers (F4e): paragraphs, `**bold**`, `*italic*`,
//! `[text](https://…)` links and `-` or `*` bullet lists. Everything else is plain text.
//!
//! The result is structure, not HTML: templates render it, so askama escapes every piece
//! of text and no `|safe` is needed.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Span>),
    List(Vec<Vec<Span>>),
}

/// A run of text with one formatting. `link` is always a plain `https://` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub link: Option<String>,
}

enum Open {
    Paragraph(Vec<String>),
    List(Vec<String>),
}

#[must_use]
pub fn parse(source: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut open: Option<Open> = None;
    for line in source.lines() {
        let line = line.trim();
        if line.is_empty() {
            close(&mut blocks, open.take());
            continue;
        }
        if let Some(item) = list_item(line) {
            match &mut open {
                Some(Open::List(items)) => items.push(item.to_owned()),
                _ => {
                    close(&mut blocks, open.take());
                    open = Some(Open::List(vec![item.to_owned()]));
                }
            }
            continue;
        }
        match &mut open {
            Some(Open::Paragraph(lines)) => lines.push(line.to_owned()),
            Some(Open::List(items)) => {
                if let Some(last) = items.last_mut() {
                    last.push(' ');
                    last.push_str(line);
                }
            }
            None => open = Some(Open::Paragraph(vec![line.to_owned()])),
        }
    }
    close(&mut blocks, open);
    blocks
}

fn list_item(line: &str) -> Option<&str> {
    line.strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .map(str::trim)
        .filter(|item| !item.is_empty())
}

fn close(blocks: &mut Vec<Block>, open: Option<Open>) {
    match open {
        Some(Open::Paragraph(lines)) => blocks.push(Block::Paragraph(inline(&lines.join(" ")))),
        Some(Open::List(items)) => {
            blocks.push(Block::List(items.iter().map(|item| inline(item)).collect()));
        }
        None => {}
    }
}

struct Inline {
    spans: Vec<Span>,
    text: String,
    bold: bool,
    italic: bool,
}

impl Inline {
    fn flush(&mut self) {
        if self.text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.text);
        self.push(Span {
            text,
            bold: self.bold,
            italic: self.italic,
            link: None,
        });
    }

    fn push(&mut self, span: Span) {
        if let Some(last) = self.spans.last_mut()
            && last.link.is_none()
            && span.link.is_none()
            && last.bold == span.bold
            && last.italic == span.italic
        {
            last.text.push_str(&span.text);
            return;
        }
        self.spans.push(span);
    }
}

fn inline(source: &str) -> Vec<Span> {
    let chars: Vec<char> = source.chars().collect();
    let mut out = Inline {
        spans: Vec::new(),
        text: String::new(),
        bold: false,
        italic: false,
    };
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        match c {
            '\\' if chars.get(i + 1).is_some_and(char::is_ascii_punctuation) => {
                out.text.push(chars[i + 1]);
                i += 2;
            }
            '*' => i += stars(&chars, i, &mut out),
            '[' => match link(&chars, i) {
                Some((label, url, end)) => {
                    out.flush();
                    match url {
                        Some(url) => out.push(Span {
                            text: label,
                            bold: out.bold,
                            italic: out.italic,
                            link: Some(url),
                        }),
                        None => out.text.push_str(&label),
                    }
                    i = end;
                }
                None => {
                    out.text.push('[');
                    i += 1;
                }
            },
            _ => {
                out.text.push(c);
                i += 1;
            }
        }
    }
    out.flush();
    out.spans
}

/// Handles the run of `*` at `start` and returns how many characters it used. A marker
/// only opens when a matching marker follows and the next character is not a space, so
/// "2 * 3" and an unclosed `**` stay as text.
fn stars(chars: &[char], start: usize, out: &mut Inline) -> usize {
    let run = chars[start..].iter().take_while(|&&c| c == '*').count();
    let mut used = 0;
    if out.italic && (run == 1 || run >= 3) {
        out.flush();
        out.italic = false;
        used += 1;
    }
    if out.bold && run - used >= 2 {
        out.flush();
        out.bold = false;
        used += 2;
    }
    if used > 0 {
        return used;
    }
    let opens = chars
        .get(start + run)
        .is_some_and(|next| !next.is_whitespace());
    if opens && run >= 2 && !out.bold && closes(chars, start + 2, 2) {
        out.flush();
        out.bold = true;
        return 2;
    }
    if opens && run == 1 && !out.italic && closes(chars, start + 1, 1) {
        out.flush();
        out.italic = true;
        return 1;
    }
    out.text.extend(std::iter::repeat_n('*', run));
    run
}

/// Whether a run of at least `len` stars, right after a non-space, follows `from`.
fn closes(chars: &[char], from: usize, len: usize) -> bool {
    (from..chars.len()).any(|i| {
        chars[i] == '*'
            && chars[i - 1] != '\\'
            && !chars[i - 1].is_whitespace()
            && chars[i..].iter().take_while(|&&c| c == '*').count() >= len
    })
}

/// `[label](url)` at `start`: the label, the URL when it is safe, and the index after `)`.
fn link(chars: &[char], start: usize) -> Option<(String, Option<String>, usize)> {
    let close = start + 1 + chars[start + 1..].iter().position(|&c| c == ']')?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = close + 2 + chars[close + 2..].iter().position(|&c| c == ')')?;
    let label: String = chars[start + 1..close].iter().collect();
    if label.contains('[') {
        return None;
    }
    let url: String = chars[close + 2..end].iter().collect();
    Some((label, safe_url(&url), end + 1))
}

/// Only plain `https://` URLs without spaces, quotes or angle brackets.
fn safe_url(url: &str) -> Option<String> {
    let ok = url
        .strip_prefix("https://")
        .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
        && url
            .chars()
            .all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\'' | '<' | '>' | '\\' | '`'));
    ok.then(|| url.to_owned())
}
