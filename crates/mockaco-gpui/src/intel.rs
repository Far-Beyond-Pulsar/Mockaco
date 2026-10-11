//! Language-intelligence data and logic that does not depend on a UI toolkit
//! or on a particular language server.
//!
//! The editor view *asks* its host for hover, completion and definition
//! (via [`crate::native::EditorEvent`]) and the host answers with the types in
//! this module. The host owns the transport, so the same editor works with the
//! language server Pulsar already runs, a test double, or none at all.
//!
//! Everything here works in byte offsets of the document the request was made
//! against. Filtering and ranking are pure functions so they can be tested and
//! benchmarked without a window.

use crate::markdown::{self, Block};
use std::ops::Range;

/// A text edit in byte offsets of the document at request time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Range<usize>,
    pub new_text: String,
}

/// What a hover popup shows.
#[derive(Debug, Clone, PartialEq)]
pub struct HoverContent {
    pub blocks: Vec<Block>,
    /// Byte range the information is about, when the server reports one.
    pub range: Option<Range<usize>>,
}

impl HoverContent {
    pub fn from_markdown(markdown: &str, range: Option<Range<usize>>) -> Self {
        Self {
            blocks: markdown::parse(markdown),
            range,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
}

/// A resolved definition/reference target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// Path or URI as the host understands it.
    pub target: String,
    /// Zero-based line and UTF-16 column, as LSP reports them.
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    Text,
    Method,
    Function,
    Constructor,
    Field,
    Variable,
    Class,
    Interface,
    Module,
    Property,
    Unit,
    Value,
    Enum,
    Keyword,
    Snippet,
    Color,
    File,
    Reference,
    Folder,
    EnumMember,
    Constant,
    Struct,
    Event,
    Operator,
    TypeParameter,
}

impl CompletionKind {
    /// From the LSP `CompletionItemKind` number.
    pub fn from_lsp(kind: u32) -> Self {
        match kind {
            2 => Self::Method,
            3 => Self::Function,
            4 => Self::Constructor,
            5 => Self::Field,
            6 => Self::Variable,
            7 => Self::Class,
            8 => Self::Interface,
            9 => Self::Module,
            10 => Self::Property,
            11 => Self::Unit,
            12 => Self::Value,
            13 => Self::Enum,
            14 => Self::Keyword,
            15 => Self::Snippet,
            16 => Self::Color,
            17 => Self::File,
            18 => Self::Reference,
            19 => Self::Folder,
            20 => Self::EnumMember,
            21 => Self::Constant,
            22 => Self::Struct,
            23 => Self::Event,
            24 => Self::Operator,
            25 => Self::TypeParameter,
            _ => Self::Text,
        }
    }

    /// One-letter icon for the menu.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Method | Self::Function | Self::Constructor => "\u{0192}",
            Self::Field | Self::Property => "f",
            Self::Variable | Self::Value => "v",
            Self::Class | Self::Struct => "S",
            Self::Interface => "I",
            Self::Module | Self::Folder | Self::File => "M",
            Self::Enum | Self::EnumMember => "E",
            Self::Keyword => "k",
            Self::Snippet => "s",
            Self::Constant => "C",
            Self::TypeParameter => "T",
            Self::Operator => "o",
            Self::Unit | Self::Color | Self::Reference | Self::Event | Self::Text => "\u{00b7}",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompletionEntry {
    pub label: String,
    /// Dimmed text right after the label (rust-analyzer's signature hint).
    pub label_detail: Option<String>,
    pub detail: Option<String>,
    /// Markdown documentation, when the server sent it with the item.
    pub documentation: Option<String>,
    pub kind: CompletionKind,
    /// Text that replaces the typed prefix on accept.
    pub insert_text: String,
    /// `insert_text` is an LSP snippet (`$1`, `${2:name}`, `$0`).
    pub is_snippet: bool,
    pub filter_text: Option<String>,
    /// Ordering key supplied by the server; empty falls back to the label.
    pub sort_key: String,
    pub preselect: bool,
    pub deprecated: bool,
    /// Edits elsewhere in the document, such as an added `use`.
    pub additional_edits: Vec<TextEdit>,
    /// Opaque host handle (non-zero) used to resolve the item lazily: the
    /// view hands it back in resolve and commit events.
    pub host_data: u64,
}

impl CompletionEntry {
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        Self {
            insert_text: label.clone(),
            label,
            label_detail: None,
            detail: None,
            documentation: None,
            kind: CompletionKind::Text,
            is_snippet: false,
            filter_text: None,
            sort_key: String::new(),
            preselect: false,
            deprecated: false,
            additional_edits: Vec::new(),
            host_data: 0,
        }
    }

    fn filter_key(&self) -> &str {
        self.filter_text.as_deref().unwrap_or(&self.label)
    }

    fn order_key(&self) -> &str {
        if self.sort_key.is_empty() {
            &self.label
        } else {
            &self.sort_key
        }
    }
}

/// Most entries a session ranks and shows. The menu is virtualized, but
/// ranking and keyboard navigation stay bounded all the same.
pub const MAX_VISIBLE_ENTRIES: usize = 1000;

/// An open completion menu: the server's items, the user's typed prefix, and
/// the ranked subset that currently matches.
#[derive(Debug, Clone)]
pub struct CompletionSession {
    /// Request that produced the items.
    pub request: u64,
    /// Byte offset where the typed prefix starts.
    pub anchor: usize,
    items: Vec<CompletionEntry>,
    ranked: Vec<usize>,
    selected: usize,
    prefix: String,
}

impl CompletionSession {
    pub fn new(request: u64, anchor: usize, mut items: Vec<CompletionEntry>, prefix: &str) -> Self {
        // Stable: equal keys keep the server's order.
        items.sort_by(|a, b| a.order_key().cmp(b.order_key()));
        let mut session = Self {
            request,
            anchor,
            items,
            ranked: Vec::new(),
            selected: 0,
            prefix: String::new(),
        };
        session.refilter(prefix);
        session
    }

    /// Re-ranks for a new typed prefix and selects the best match.
    pub fn refilter(&mut self, prefix: &str) {
        self.prefix.clear();
        self.prefix.push_str(prefix);
        let mut scored: Vec<(i32, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                fuzzy_score(prefix, item.filter_key()).map(|score| (score, index))
            })
            .collect();
        // Best score first; the original (server) order breaks ties.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.truncate(MAX_VISIBLE_ENTRIES);
        self.ranked = scored.into_iter().map(|(_, index)| index).collect();
        self.selected = if prefix.is_empty() {
            self.ranked
                .iter()
                .position(|index| self.items[*index].preselect)
                .unwrap_or(0)
        } else {
            0
        };
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    pub fn len(&self) -> usize {
        self.ranked.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ranked.is_empty()
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The entry at menu position `rank`.
    pub fn entry(&self, rank: usize) -> Option<&CompletionEntry> {
        self.ranked.get(rank).map(|index| &self.items[*index])
    }

    pub fn selected_entry(&self) -> Option<&CompletionEntry> {
        self.entry(self.selected)
    }

    /// Moves the selection by `delta`, wrapping at both ends.
    pub fn move_selection(&mut self, delta: isize) {
        let len = self.ranked.len() as isize;
        if len == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
    }

    /// Moves by `delta` but stops at the ends (page up/down).
    pub fn move_selection_clamped(&mut self, delta: isize) {
        let len = self.ranked.len() as isize;
        if len == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).clamp(0, len - 1) as usize;
    }

    pub fn select(&mut self, rank: usize) {
        if rank < self.ranked.len() {
            self.selected = rank;
        }
    }

    /// Fills in lazily resolved documentation/detail for the entry the host
    /// tagged with `host_data`. Returns whether such an entry exists.
    pub fn resolve(
        &mut self,
        host_data: u64,
        documentation: Option<String>,
        detail: Option<String>,
    ) -> bool {
        if host_data == 0 {
            return false;
        }
        match self
            .items
            .iter_mut()
            .find(|item| item.host_data == host_data)
        {
            Some(item) => {
                if documentation.is_some() {
                    item.documentation = documentation;
                }
                if detail.is_some() {
                    item.detail = detail;
                }
                true
            }
            None => false,
        }
    }
}

/// Case-insensitive subsequence match of `query` in `candidate`. `None` when
/// `query` is not a subsequence; otherwise higher is better. Prefix matches,
/// matches at word starts (`snake_case`, `camelCase`) and runs of consecutive
/// characters score higher.
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let cand: Vec<char> = candidate.chars().collect();
    let lower: Vec<char> = cand.iter().map(|c| c.to_ascii_lowercase()).collect();
    let mut score = 0i32;
    let mut position = 0usize;
    let mut previous: Option<usize> = None;
    for q in query.chars() {
        let q_lower = q.to_ascii_lowercase();
        let found = (position..lower.len()).find(|index| lower[*index] == q_lower)?;
        score += 1;
        if cand[found] == q {
            score += 1;
        }
        if previous.map_or(false, |p| p + 1 == found) {
            score += 8;
        }
        let boundary = found == 0
            || !cand[found - 1].is_alphanumeric()
            || (cand[found - 1].is_lowercase() && cand[found].is_uppercase());
        if boundary {
            score += 10;
        }
        if found == 0 {
            score += 15;
        }
        // Gaps cost a little, so tighter matches win.
        if let Some(p) = previous {
            score -= (found - p - 1).min(6) as i32;
        } else {
            score -= found.min(6) as i32;
        }
        previous = Some(found);
        position = found + 1;
    }
    let query_lower: String = query.chars().map(|c| c.to_ascii_lowercase()).collect();
    let candidate_lower: String = lower.iter().collect();
    if candidate_lower.starts_with(&query_lower) {
        score += 100;
        if candidate_lower.len() == query_lower.len() {
            score += 50;
        }
    }
    Some(score)
}

/// Char indices of `candidate` that [`fuzzy_score`] matched for `query`, for
/// highlighting in the menu. Empty when `query` does not match.
pub fn fuzzy_positions(query: &str, candidate: &str) -> Vec<usize> {
    let lower: Vec<char> = candidate.chars().map(|c| c.to_ascii_lowercase()).collect();
    let mut position = 0usize;
    let mut found = Vec::new();
    for q in query.chars() {
        let q_lower = q.to_ascii_lowercase();
        match (position..lower.len()).find(|index| lower[*index] == q_lower) {
            Some(index) => {
                found.push(index);
                position = index + 1;
            }
            None => return Vec::new(),
        }
    }
    found
}

/// A diagnostic as the host knows it: LSP positions (zero-based line,
/// UTF-16 column). The view converts to byte ranges against its own text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticInput {
    pub start: mockaco_core::TextPosition,
    pub end: mockaco_core::TextPosition,
    pub severity: mockaco_core::DiagnosticSeverity,
    pub message: String,
    /// Producer and code, e.g. `rustc(E0308)` or `clippy(needless_return)`.
    pub source: Option<String>,
}

/// A text edit in LSP coordinates, applied against the document as it is when
/// the edit arrives (used for late-resolved completion edits such as imports).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionEdit {
    pub start: mockaco_core::TextPosition,
    pub end: mockaco_core::TextPosition,
    pub new_text: String,
}

/// The result of expanding an LSP snippet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandedSnippet {
    pub text: String,
    /// Where the caret/selection goes, relative to the start of `text`: the
    /// first numbered tab stop's placeholder, else `$0`, else the end.
    pub selection: Range<usize>,
}

/// Expands the common subset of LSP snippet syntax: tab stops (`$1`, `${1}`),
/// placeholders (`${1:name}`, nested), choices (`${1|a,b|}`, first wins),
/// variables (dropped) and `\` escapes.
pub fn expand_snippet(source: &str) -> ExpandedSnippet {
    let chars: Vec<char> = source.chars().collect();
    let mut position = 0;
    let mut text = String::new();
    let mut stops: Vec<(u32, usize, usize)> = Vec::new();
    parse_snippet(&chars, &mut position, &mut text, &mut stops, false);

    let first = stops
        .iter()
        .filter(|(index, _, _)| *index > 0)
        .min_by_key(|(index, start, _)| (*index, *start));
    let selection = match first {
        Some((_, start, end)) => *start..*end,
        None => match stops.iter().find(|(index, _, _)| *index == 0) {
            Some((_, start, end)) => *start..*end,
            None => text.len()..text.len(),
        },
    };
    ExpandedSnippet { text, selection }
}

fn parse_snippet(
    chars: &[char],
    position: &mut usize,
    out: &mut String,
    stops: &mut Vec<(u32, usize, usize)>,
    nested: bool,
) {
    while *position < chars.len() {
        let c = chars[*position];
        match c {
            '}' if nested => return,
            '\\' => {
                if let Some(next) = chars.get(*position + 1) {
                    if matches!(next, '$' | '}' | '\\') {
                        out.push(*next);
                        *position += 2;
                        continue;
                    }
                }
                out.push('\\');
                *position += 1;
            }
            '$' => {
                *position += 1;
                match chars.get(*position) {
                    Some(d) if d.is_ascii_digit() => {
                        let index = read_number(chars, position);
                        stops.push((index, out.len(), out.len()));
                    }
                    Some('{') => {
                        *position += 1;
                        parse_braced(chars, position, out, stops);
                    }
                    Some(l) if l.is_alphabetic() || *l == '_' => {
                        // `$NAME` variable: unsupported, expands to nothing.
                        while chars
                            .get(*position)
                            .map_or(false, |c| c.is_alphanumeric() || *c == '_')
                        {
                            *position += 1;
                        }
                    }
                    _ => out.push('$'),
                }
            }
            _ => {
                out.push(c);
                *position += 1;
            }
        }
    }
}

fn read_number(chars: &[char], position: &mut usize) -> u32 {
    let mut value = 0u32;
    while let Some(d) = chars.get(*position).and_then(|c| c.to_digit(10)) {
        value = value.saturating_mul(10).saturating_add(d);
        *position += 1;
    }
    value
}

fn parse_braced(
    chars: &[char],
    position: &mut usize,
    out: &mut String,
    stops: &mut Vec<(u32, usize, usize)>,
) {
    if chars.get(*position).map_or(false, |c| c.is_ascii_digit()) {
        let index = read_number(chars, position);
        let start = out.len();
        match chars.get(*position) {
            Some(':') => {
                *position += 1;
                parse_snippet(chars, position, out, stops, true);
            }
            Some('|') => {
                // Choice list: take the first option.
                *position += 1;
                while *position < chars.len() && !matches!(chars[*position], ',' | '|') {
                    out.push(chars[*position]);
                    *position += 1;
                }
                while *position < chars.len() && chars[*position] != '|' {
                    *position += 1;
                }
                *position += 1;
            }
            _ => {}
        }
        // Skip to the closing brace.
        while *position < chars.len() && chars[*position] != '}' {
            *position += 1;
        }
        *position += 1;
        stops.push((index, start, out.len()));
    } else {
        // `${VAR}` / `${VAR:default}`: use the default text, if any.
        while chars
            .get(*position)
            .map_or(false, |c| c.is_alphanumeric() || *c == '_')
        {
            *position += 1;
        }
        if chars.get(*position) == Some(&':') {
            *position += 1;
            parse_snippet(chars, position, out, stops, true);
        }
        while *position < chars.len() && chars[*position] != '}' {
            *position += 1;
        }
        *position += 1;
    }
}

/// Whether `c` continues an identifier (word characters for completion).
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Longest run of word characters around `byte`, or `None` when the
/// character at `byte` is not one. Reads only the word itself, however long
/// the line is.
pub fn word_range(text: &mockaco_core::Text, byte: usize) -> Option<Range<usize>> {
    if byte >= text.len() || !text.char_at(byte).is_some_and(is_word_char) {
        return None;
    }
    let mut start = byte;
    for (offset, c) in text.chars_before(byte) {
        if !is_word_char(c) {
            break;
        }
        start = offset;
    }
    let mut end = byte;
    for (offset, c) in text.chars_from(byte) {
        if !is_word_char(c) {
            break;
        }
        end = offset + c.len_utf8();
    }
    Some(start..end)
}

/// Start of the identifier that ends at `caret` (the caret itself when the
/// character before it is not a word character).
pub fn word_start_before(text: &mockaco_core::Text, caret: usize) -> usize {
    let mut start = caret.min(text.len());
    for (offset, c) in text.chars_before(start) {
        if !is_word_char(c) {
            break;
        }
        start = offset;
    }
    start
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_rejects_non_subsequences() {
        assert_eq!(fuzzy_score("xyz", "hello"), None);
        assert_eq!(fuzzy_score("ol", "lo"), None);
    }

    #[test]
    fn prefix_beats_scattered_match() {
        let prefix = fuzzy_score("vec", "vec_push").unwrap();
        let scattered = fuzzy_score("vec", "very_exact_cut").unwrap();
        assert!(prefix > scattered);
    }

    #[test]
    fn word_boundaries_win() {
        let camel = fuzzy_score("hm", "HashMap").unwrap();
        let buried = fuzzy_score("hm", "ahimsa").unwrap();
        assert!(camel > buried);
    }

    #[test]
    fn exact_match_ranks_first() {
        let mut session = CompletionSession::new(
            1,
            0,
            vec![
                CompletionEntry::new("pushed"),
                CompletionEntry::new("push"),
                CompletionEntry::new("push_str"),
            ],
            "push",
        );
        assert_eq!(session.selected_entry().unwrap().label, "push");
        session.refilter("push_");
        assert_eq!(session.len(), 1);
        assert_eq!(session.selected_entry().unwrap().label, "push_str");
    }

    #[test]
    fn empty_prefix_keeps_server_order_and_preselect() {
        let mut preferred = CompletionEntry::new("b");
        preferred.preselect = true;
        preferred.sort_key = "2".into();
        let mut a = CompletionEntry::new("a");
        a.sort_key = "1".into();
        let mut c = CompletionEntry::new("c");
        c.sort_key = "3".into();
        let session = CompletionSession::new(1, 0, vec![c, preferred, a], "");
        assert_eq!(session.entry(0).unwrap().label, "a");
        assert_eq!(session.selected_entry().unwrap().label, "b");
    }

    #[test]
    fn selection_wraps_and_clamps() {
        let mut session = CompletionSession::new(
            1,
            0,
            vec![CompletionEntry::new("a"), CompletionEntry::new("b")],
            "",
        );
        session.move_selection(-1);
        assert_eq!(session.selected(), 1);
        session.move_selection(1);
        assert_eq!(session.selected(), 0);
        session.move_selection_clamped(10);
        assert_eq!(session.selected(), 1);
    }

    #[test]
    fn ranking_is_bounded() {
        let items = (0..5000)
            .map(|i| CompletionEntry::new(format!("item_{i}")))
            .collect();
        let session = CompletionSession::new(1, 0, items, "item");
        assert_eq!(session.len(), MAX_VISIBLE_ENTRIES);
    }

    #[test]
    fn snippet_tab_stops_and_placeholders() {
        let expanded = expand_snippet("foo(${1:arg}, ${2:other})$0");
        assert_eq!(expanded.text, "foo(arg, other)");
        assert_eq!(expanded.selection, 4..7);
    }

    #[test]
    fn snippet_final_stop_places_caret() {
        let expanded = expand_snippet("println!($0)");
        assert_eq!(expanded.text, "println!()");
        assert_eq!(expanded.selection, 9..9);
    }

    #[test]
    fn snippet_without_stops_ends_at_the_end() {
        let expanded = expand_snippet("plain");
        assert_eq!(expanded.selection, 5..5);
    }

    #[test]
    fn snippet_escapes_choices_and_nesting() {
        assert_eq!(expand_snippet(r"cost: \$5").text, "cost: $5");
        assert_eq!(expand_snippet("${1|alpha,beta|}").text, "alpha");
        let nested = expand_snippet("${1:a${2:b}c}");
        assert_eq!(nested.text, "abc");
        assert_eq!(nested.selection, 0..3);
        assert_eq!(expand_snippet("$TM_FILENAME x").text, " x");
    }

    #[test]
    fn word_range_finds_the_identifier_around_a_byte() {
        let text = mockaco_core::Text::new("let foo_bar = x + é2;");
        assert_eq!(word_range(&text, 6), Some(4..11));
        assert_eq!(word_range(&text, 4), Some(4..11));
        assert_eq!(word_range(&text, 10), Some(4..11));
        assert_eq!(word_range(&text, 3), None);
        assert_eq!(word_range(&text, 11), None);
        assert_eq!(word_range(&text, 999), None);
        let accented = text.len() - 4;
        assert_eq!(word_range(&text, accented), Some(accented..text.len() - 1));
        assert_eq!(word_start_before(&text, 11), 4);
        assert_eq!(word_start_before(&text, 12), 12);
    }

    #[test]
    fn fuzzy_positions_match_the_scoring_walk() {
        assert_eq!(fuzzy_positions("hm", "HashMap"), vec![0, 4]);
        assert!(fuzzy_positions("zz", "HashMap").is_empty());
    }

    #[test]
    fn snippet_never_panics() {
        for source in [
            "", "$", "${", "${1", "${1:", "\\", "${1|a", "$$", "}", "${a:b",
        ] {
            let _ = expand_snippet(source);
        }
    }
}
