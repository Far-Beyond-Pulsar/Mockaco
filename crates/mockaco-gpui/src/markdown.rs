//! A small Markdown parser for language-server documentation.
//!
//! Hover and completion documentation (Rust docs in particular) uses a
//! predictable subset of Markdown: fenced code, headings, lists, quotes,
//! rules, and inline code / emphasis / links. The result is a flat list of
//! blocks so a virtualized list can render only the blocks in view.
//!
//! No UI types live here; the native view turns blocks into elements.

/// Inline style of a run of text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpanStyle {
    pub code: bool,
    pub bold: bool,
    pub italic: bool,
    /// Link target, when the run is part of `[text](target)`.
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: SpanStyle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Span>),
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    /// Fenced code. `language` is the first word of the info string, with any
    /// `,attr` suffix (rustdoc's ```` ```rust,ignore ````) removed.
    Code {
        language: String,
        text: String,
    },
    ListItem {
        depth: usize,
        marker: String,
        spans: Vec<Span>,
    },
    Quote(Vec<Span>),
    Rule,
}

impl Block {
    /// Plain text of the block, for sizing and tests.
    pub fn plain_text(&self) -> String {
        match self {
            Self::Paragraph(spans)
            | Self::Quote(spans)
            | Self::Heading { spans, .. }
            | Self::ListItem { spans, .. } => spans.iter().map(|span| span.text.as_str()).collect(),
            Self::Code { text, .. } => text.clone(),
            Self::Rule => String::new(),
        }
    }
}

/// Parses `source` into blocks. Never fails: unrecognised syntax is text.
pub fn parse(source: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    let mut lines = source.lines().peekable();

    fn flush(blocks: &mut Vec<Block>, paragraph: &mut Vec<&str>) {
        if paragraph.is_empty() {
            return;
        }
        let joined = paragraph.join(" ");
        blocks.push(Block::Paragraph(parse_inline(joined.trim())));
        paragraph.clear();
    }

    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        if let Some(fence) = fence_marker(trimmed) {
            flush(&mut blocks, &mut paragraph);
            let info = trimmed[fence.len()..].trim();
            let language = info
                .split(|c: char| c == ',' || c.is_whitespace())
                .next()
                .unwrap_or("")
                .to_owned();
            let mut text = String::new();
            for body in lines.by_ref() {
                let body_trimmed = body.trim_start();
                if body_trimmed.starts_with(fence)
                    && body_trimmed
                        .trim_matches(fence.chars().next().unwrap())
                        .is_empty()
                {
                    break;
                }
                // Remove the fence's own indentation, keep the rest.
                let stripped = body
                    .char_indices()
                    .take_while(|(index, c)| *index < indent && c.is_whitespace())
                    .count();
                text.push_str(body.get(stripped..).unwrap_or(body));
                text.push('\n');
            }
            if text.ends_with('\n') {
                text.pop();
            }
            blocks.push(Block::Code { language, text });
            continue;
        }

        if trimmed.is_empty() {
            flush(&mut blocks, &mut paragraph);
            continue;
        }

        if is_rule(trimmed) {
            flush(&mut blocks, &mut paragraph);
            blocks.push(Block::Rule);
            continue;
        }

        if let Some((level, rest)) = heading(trimmed) {
            flush(&mut blocks, &mut paragraph);
            blocks.push(Block::Heading {
                level,
                spans: parse_inline(rest.trim().trim_end_matches('#').trim_end()),
            });
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix('>') {
            flush(&mut blocks, &mut paragraph);
            blocks.push(Block::Quote(parse_inline(rest.trim())));
            continue;
        }

        if let Some((marker, rest)) = list_marker(trimmed) {
            flush(&mut blocks, &mut paragraph);
            blocks.push(Block::ListItem {
                depth: indent / 2,
                marker,
                spans: parse_inline(rest.trim()),
            });
            continue;
        }

        // A four-space indented line after a blank line is an indented code
        // block (rustdoc accepts these); inside a paragraph it is just text.
        if indent >= 4 && paragraph.is_empty() {
            let mut text = String::new();
            text.push_str(line.get(4..).unwrap_or(""));
            while let Some(next) = lines.peek() {
                if next.trim().is_empty() || next.len() - next.trim_start().len() >= 4 {
                    let body = lines.next().unwrap();
                    text.push('\n');
                    text.push_str(body.get(4..).unwrap_or(""));
                } else {
                    break;
                }
            }
            blocks.push(Block::Code {
                language: String::new(),
                text: text.trim_end_matches('\n').to_owned(),
            });
            continue;
        }

        paragraph.push(trimmed.trim_end());
    }
    flush(&mut blocks, &mut paragraph);
    blocks
}

fn fence_marker(trimmed: &str) -> Option<&'static str> {
    if trimmed.starts_with("```") {
        Some("```")
    } else if trimmed.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

fn is_rule(trimmed: &str) -> bool {
    let stripped: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    stripped.len() >= 3
        && (stripped.chars().all(|c| c == '-')
            || stripped.chars().all(|c| c == '*')
            || stripped.chars().all(|c| c == '_'))
}

fn heading(trimmed: &str) -> Option<(u8, &str)> {
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) {
        let rest = &trimmed[hashes..];
        if rest.is_empty() || rest.starts_with(' ') {
            return Some((hashes as u8, rest));
        }
    }
    None
}

fn list_marker(trimmed: &str) -> Option<(String, &str)> {
    let mut chars = trimmed.chars();
    let first = chars.next()?;
    if matches!(first, '-' | '*' | '+') && trimmed[1..].starts_with(' ') {
        return Some(("\u{2022}".to_owned(), &trimmed[2..]));
    }
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if (1..=9).contains(&digits) {
        let rest = &trimmed[digits..];
        if (rest.starts_with(". ") || rest.starts_with(") ")) && rest.len() > 2 {
            return Some((format!("{}.", &trimmed[..digits]), &rest[2..]));
        }
    }
    None
}

/// Parses inline Markdown into styled runs. Adjacent runs with the same style
/// are merged.
pub fn parse_inline(source: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    inline(source, &SpanStyle::default(), &mut spans);
    let mut merged: Vec<Span> = Vec::with_capacity(spans.len());
    for span in spans {
        if span.text.is_empty() {
            continue;
        }
        match merged.last_mut() {
            Some(last) if last.style == span.style => last.text.push_str(&span.text),
            _ => merged.push(span),
        }
    }
    merged
}

fn push(out: &mut Vec<Span>, text: &str, style: &SpanStyle) {
    if !text.is_empty() {
        out.push(Span {
            text: text.to_owned(),
            style: style.clone(),
        });
    }
}

fn inline(source: &str, style: &SpanStyle, out: &mut Vec<Span>) {
    let bytes = source.as_bytes();
    let mut plain = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &source[i..];
        let c = rest.chars().next().unwrap();
        match c {
            '\\' => {
                // Backslash escapes an ASCII punctuation character.
                if let Some(next) = rest[1..].chars().next() {
                    if next.is_ascii_punctuation() {
                        plain.push(next);
                        i += 1 + next.len_utf8();
                        continue;
                    }
                }
                plain.push('\\');
                i += 1;
            }
            '`' => {
                let ticks = rest.chars().take_while(|c| *c == '`').count();
                let fence = "`".repeat(ticks);
                if let Some(end) = rest[ticks..].find(&fence) {
                    push(out, &std::mem::take(&mut plain), style);
                    let code = rest[ticks..ticks + end].trim_matches(' ');
                    let mut code_style = style.clone();
                    code_style.code = true;
                    push(out, code, &code_style);
                    i += ticks + end + ticks;
                } else {
                    plain.push_str(&fence);
                    i += ticks;
                }
            }
            '*' | '_' => {
                let marks = rest.chars().take_while(|m| *m == c).count().min(2);
                let delimiter = c.to_string().repeat(marks);
                // Underscores only emphasise at word boundaries
                // (snake_case identifiers stay intact).
                let boundary_ok = c == '*'
                    || source[..i]
                        .chars()
                        .next_back()
                        .map_or(true, |p| !p.is_alphanumeric());
                if boundary_ok {
                    if let Some(end) = find_closing(&rest[marks..], &delimiter, c == '_') {
                        let inner = &rest[marks..marks + end];
                        if !inner.is_empty() && !inner.starts_with(' ') {
                            push(out, &std::mem::take(&mut plain), style);
                            let mut inner_style = style.clone();
                            if marks == 2 {
                                inner_style.bold = true;
                            } else {
                                inner_style.italic = true;
                            }
                            inline(inner, &inner_style, out);
                            i += marks + end + marks;
                            continue;
                        }
                    }
                }
                plain.push_str(&delimiter);
                i += marks;
            }
            '[' => {
                if let Some((text, target, consumed)) = link(rest) {
                    push(out, &std::mem::take(&mut plain), style);
                    let mut link_style = style.clone();
                    link_style.link = Some(target.to_owned());
                    inline(text, &link_style, out);
                    i += consumed;
                } else {
                    plain.push('[');
                    i += 1;
                }
            }
            '<' => {
                // Autolink: <https://example.com>
                if let Some(end) = rest.find('>') {
                    let target = &rest[1..end];
                    if (target.starts_with("http://") || target.starts_with("https://"))
                        && !target.contains(' ')
                    {
                        push(out, &std::mem::take(&mut plain), style);
                        let mut link_style = style.clone();
                        link_style.link = Some(target.to_owned());
                        push(out, target, &link_style);
                        i += end + 1;
                        continue;
                    }
                }
                plain.push('<');
                i += 1;
            }
            _ => {
                plain.push(c);
                i += c.len_utf8();
            }
        }
    }
    push(out, &plain, style);
}

/// Index of the closing `delimiter` in `text`, skipping code spans. For
/// underscores the closer must also end a word.
fn find_closing(text: &str, delimiter: &str, word_boundary: bool) -> Option<usize> {
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        if rest.starts_with('`') {
            let ticks = rest.chars().take_while(|c| *c == '`').count();
            match rest[ticks..].find(&"`".repeat(ticks)) {
                Some(end) => {
                    i += ticks + end + ticks;
                    continue;
                }
                None => {
                    i += ticks;
                    continue;
                }
            }
        }
        if rest.starts_with('\\') && rest.len() > 1 {
            i += 1 + rest[1..].chars().next().map_or(0, char::len_utf8);
            continue;
        }
        if rest.starts_with(delimiter) && i > 0 {
            let after = rest[delimiter.len()..].chars().next();
            if !word_boundary || after.map_or(true, |a| !a.is_alphanumeric()) {
                return Some(i);
            }
        }
        i += rest.chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// `[text](target)` at the start of `rest`: `(text, target, bytes consumed)`.
fn link(rest: &str) -> Option<(&str, &str, usize)> {
    // Find the matching ']' honouring nesting and code spans.
    let mut depth = 0usize;
    let mut i = 0;
    let mut close = None;
    while i < rest.len() {
        let tail = &rest[i..];
        let c = tail.chars().next()?;
        match c {
            '`' => {
                let ticks = tail.chars().take_while(|c| *c == '`').count();
                match tail[ticks..].find(&"`".repeat(ticks)) {
                    Some(end) => i += ticks + end + ticks,
                    None => i += ticks,
                }
                continue;
            }
            '\\' => {
                i += 1 + tail[1..].chars().next().map_or(0, char::len_utf8);
                continue;
            }
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
        i += c.len_utf8();
    }
    let close = close?;
    let after = &rest[close + 1..];
    let inner = after.strip_prefix('(')?;
    let end = inner.find(')')?;
    let target = inner[..end].split_whitespace().next().unwrap_or("");
    Some((&rest[1..close], target, close + 1 + 1 + end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn fenced_code_keeps_language_and_indentation() {
        let blocks = parse("```rust\nfn main() {\n    run();\n}\n```\n\nDocs here.");
        assert_eq!(
            blocks[0],
            Block::Code {
                language: "rust".into(),
                text: "fn main() {\n    run();\n}".into()
            }
        );
        assert!(matches!(blocks[1], Block::Paragraph(_)));
    }

    #[test]
    fn rustdoc_fence_attributes_are_dropped() {
        let blocks = parse("```rust,ignore\nlet x = 1;\n```");
        assert!(matches!(&blocks[0], Block::Code { language, .. } if language == "rust"));
    }

    #[test]
    fn rust_analyzer_hover_shape() {
        let source = "```rust\nstd::vec\n```\n\n```rust\npub struct Vec<T>\n```\n\n---\n\nA contiguous growable array type, written as `Vec<T>`.\n\n# Examples\n\n```\nlet v = vec![1];\n```";
        let blocks = parse(source);
        assert!(matches!(blocks[0], Block::Code { .. }));
        assert!(matches!(blocks[1], Block::Code { .. }));
        assert_eq!(blocks[2], Block::Rule);
        let Block::Paragraph(spans) = &blocks[3] else {
            panic!("paragraph expected")
        };
        assert!(spans.iter().any(|s| s.style.code && s.text == "Vec<T>"));
        assert!(matches!(&blocks[4], Block::Heading { level: 1, .. }));
        assert!(matches!(&blocks[5], Block::Code { language, .. } if language.is_empty()));
    }

    #[test]
    fn paragraph_lines_join_with_spaces() {
        let blocks = parse("first line\nsecond line");
        assert_eq!(blocks[0].plain_text(), "first line second line");
    }

    #[test]
    fn lists_and_depth() {
        let blocks = parse("- one\n  - nested\n1. first");
        assert!(
            matches!(&blocks[0], Block::ListItem { depth: 0, marker, .. } if marker == "\u{2022}")
        );
        assert!(matches!(&blocks[1], Block::ListItem { depth: 1, .. }));
        assert!(matches!(&blocks[2], Block::ListItem { marker, .. } if marker == "1."));
    }

    #[test]
    fn inline_styles() {
        let spans = parse_inline("a **bold** and *it* and `code`");
        assert!(spans.iter().any(|s| s.style.bold && s.text == "bold"));
        assert!(spans.iter().any(|s| s.style.italic && s.text == "it"));
        assert!(spans.iter().any(|s| s.style.code && s.text == "code"));
        assert_eq!(text(&spans), "a bold and it and code");
    }

    #[test]
    fn snake_case_is_not_emphasis() {
        let spans = parse_inline("use my_var_name here");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "use my_var_name here");
    }

    #[test]
    fn links_with_code_text() {
        let spans =
            parse_inline("see [`Vec`](https://doc.rust-lang.org/std/vec/struct.Vec.html) now");
        let link = spans
            .iter()
            .find(|s| s.style.link.is_some())
            .expect("link span");
        assert_eq!(link.text, "Vec");
        assert!(link.style.code);
        assert_eq!(
            link.style.link.as_deref(),
            Some("https://doc.rust-lang.org/std/vec/struct.Vec.html")
        );
    }

    #[test]
    fn unmatched_markers_stay_literal() {
        assert_eq!(text(&parse_inline("a * b [c and `d")), "a * b [c and `d");
    }

    #[test]
    fn escapes() {
        assert_eq!(text(&parse_inline(r"\*not bold\*")), "*not bold*");
    }

    #[test]
    fn indented_code_block() {
        let blocks = parse("text\n\n    let x = 1;\n    let y = 2;\n\nafter");
        assert!(matches!(&blocks[1], Block::Code { text, .. } if text == "let x = 1;\nlet y = 2;"));
        assert!(matches!(blocks[2], Block::Paragraph(_)));
    }

    #[test]
    fn never_panics_on_odd_input() {
        for source in [
            "",
            "```",
            "[",
            "[]()",
            "**",
            "_",
            "`",
            "> ",
            "#",
            "1.",
            "é**é**é",
            "[é](é)",
        ] {
            let _ = parse(source);
        }
    }
}
