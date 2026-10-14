use crate::{
    Affinity, AppliedTransaction, Edit, EditorState, Grouping, Selection, SelectionSet,
    TransactionError,
};
use std::fmt;
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseSensitivity {
    Sensitive,
    Insensitive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub pattern: String,
    pub case_sensitivity: CaseSensitivity,
    /// Only matches with no word character (alphanumeric or `_`) right
    /// before or after them.
    pub whole_word: bool,
    /// `pattern` is a regular expression (the `regex` crate's syntax, `^`
    /// and `$` matching at line ends); a replacement can refer to its groups
    /// (`$1`, `${name}`). Empty matches are skipped.
    pub regex: bool,
}

impl SearchQuery {
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            case_sensitivity: CaseSensitivity::Sensitive,
            whole_word: false,
            regex: false,
        }
    }

    pub fn case_sensitivity(mut self, case_sensitivity: CaseSensitivity) -> Self {
        self.case_sensitivity = case_sensitivity;
        self
    }

    pub fn whole_word(mut self, whole_word: bool) -> Self {
        self.whole_word = whole_word;
        self
    }

    pub fn regex(mut self, regex: bool) -> Self {
        self.regex = regex;
        self
    }

    /// Why the query cannot match anything: its regular expression does not
    /// compile. `None` for a valid query.
    pub fn error(&self) -> Option<String> {
        if !self.regex || self.pattern.is_empty() {
            return None;
        }
        self.compile().err().map(|error| error.to_string())
    }

    /// The text that replaces `range`, a match of this query in `text`:
    /// `replacement` as given, or with a regular expression's group
    /// references (`$1`, `${name}`, `$$` for a dollar) expanded.
    pub fn expand_replacement(&self, text: &str, range: Range<usize>, replacement: &str) -> String {
        if !self.regex {
            return replacement.to_owned();
        }
        let Ok(regex) = self.compile() else {
            return replacement.to_owned();
        };
        match regex.captures_at(text, range.start) {
            Some(captures) if captures.get(0).map(|m| m.range()) == Some(range) => {
                let mut expanded = String::new();
                captures.expand(replacement, &mut expanded);
                expanded
            }
            _ => replacement.to_owned(),
        }
    }

    fn compile(&self) -> Result<regex::Regex, regex::Error> {
        let pattern = if self.whole_word {
            format!(r"\b(?:{})\b", self.pattern)
        } else {
            self.pattern.clone()
        };
        regex::RegexBuilder::new(&pattern)
            .case_insensitive(self.case_sensitivity == CaseSensitivity::Insensitive)
            .multi_line(true)
            .build()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchError {
    StaleDocumentVersion { expected: u64, actual: u64 },
    Transaction(TransactionError),
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleDocumentVersion { expected, actual } => {
                write!(
                    f,
                    "search expects document version {expected}, got {actual}"
                )
            }
            Self::Transaction(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for SearchError {}

impl From<TransactionError> for SearchError {
    fn from(error: TransactionError) -> Self {
        Self::Transaction(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceResult {
    pub applied: AppliedTransaction,
    pub replaced_ranges: Vec<Range<usize>>,
}

#[derive(Debug, Clone)]
pub struct SearchSession {
    query: SearchQuery,
    document_version: u64,
    matches: Vec<Range<usize>>,
    current: Option<usize>,
}

impl SearchSession {
    pub fn new(snapshot: &crate::DocumentSnapshot, query: SearchQuery) -> Self {
        let matches = find_matches(snapshot.text(), &query);
        Self {
            query,
            document_version: snapshot.version(),
            matches,
            current: None,
        }
    }

    pub fn query(&self) -> &SearchQuery {
        &self.query
    }

    pub fn document_version(&self) -> u64 {
        self.document_version
    }

    pub fn matches(&self) -> &[Range<usize>] {
        &self.matches
    }

    pub fn current_match_index(&self) -> Option<usize> {
        self.current
    }

    pub fn current_match(&self) -> Option<&Range<usize>> {
        self.current.and_then(|index| self.matches.get(index))
    }

    pub fn set_query(
        &mut self,
        snapshot: &crate::DocumentSnapshot,
        query: SearchQuery,
    ) -> Result<(), SearchError> {
        self.require_version(snapshot.version())?;
        self.query = query;
        self.matches = find_matches(snapshot.text(), &self.query);
        self.current = None;
        Ok(())
    }

    pub fn refresh(
        &mut self,
        snapshot: &crate::DocumentSnapshot,
        applied: &AppliedTransaction,
    ) -> Result<(), SearchError> {
        if applied.before_version != self.document_version {
            return Err(SearchError::StaleDocumentVersion {
                expected: self.document_version,
                actual: applied.before_version,
            });
        }
        if snapshot.version() != applied.after_version {
            return Err(SearchError::StaleDocumentVersion {
                expected: applied.after_version,
                actual: snapshot.version(),
            });
        }
        let previous = self.current_match().cloned();
        self.document_version = snapshot.version();
        self.matches = find_matches(snapshot.text(), &self.query);
        self.current = previous.and_then(|range| {
            let mapped_start = applied.change_map.map_offset(range.start, Affinity::After);
            self.matches
                .iter()
                .position(|candidate| candidate.start >= mapped_start)
        });
        Ok(())
    }

    pub fn next_match(&mut self) -> Option<&Range<usize>> {
        self.advance(1)
    }

    pub fn previous_match(&mut self) -> Option<&Range<usize>> {
        self.advance(-1)
    }

    pub fn replace_current(
        &mut self,
        editor: &mut EditorState,
        replacement: impl Into<String>,
    ) -> Result<Option<ReplaceResult>, SearchError> {
        self.refresh_if_needed(editor)?;
        let Some(index) = self
            .current
            .or_else(|| (!self.matches.is_empty()).then_some(0))
        else {
            return Ok(None);
        };
        self.current = Some(index);
        let range = self.matches[index].clone();
        let replacement = self.query.expand_replacement(
            &editor.snapshot().text().to_string(),
            range.clone(),
            &replacement.into(),
        );
        let transaction =
            crate::Transaction::from_edits([Edit::replace(range.clone(), replacement)]);
        let applied = editor.apply_with_result(&transaction, Grouping::Separate)?;
        let snapshot = editor.snapshot();
        let replacement_end = applied.change_map.map_offset(range.end, Affinity::After);
        editor.set_selections(crate::SelectionSet::caret(replacement_end));
        self.refresh(&snapshot, &applied)?;
        self.current = self
            .matches
            .iter()
            .position(|candidate| candidate.start == range.start);
        Ok(Some(ReplaceResult {
            applied,
            replaced_ranges: vec![range],
        }))
    }

    pub fn replace_all(
        &mut self,
        editor: &mut EditorState,
        replacement: impl Into<String>,
    ) -> Result<Option<ReplaceResult>, SearchError> {
        self.refresh_if_needed(editor)?;
        if self.matches.is_empty() {
            return Ok(None);
        }
        let replacement = replacement.into();
        let ranges = self.matches.clone();
        let text = editor.snapshot().text().to_string();
        let transaction = crate::Transaction::from_edits(ranges.iter().cloned().map(|range| {
            let expanded = self
                .query
                .expand_replacement(&text, range.clone(), &replacement);
            Edit::replace(range, expanded)
        }));
        let previous_selections = editor.selections().clone();
        let applied = editor.apply_with_result(&transaction, Grouping::Separate)?;
        editor.set_selections(previous_selections.map(&applied.change_map));
        let snapshot = editor.snapshot();
        self.refresh(&snapshot, &applied)?;
        self.current = None;
        Ok(Some(ReplaceResult {
            applied,
            replaced_ranges: ranges,
        }))
    }

    fn advance(&mut self, direction: isize) -> Option<&Range<usize>> {
        if self.matches.is_empty() {
            self.current = None;
            return None;
        }
        let length = self.matches.len() as isize;
        let current = self.current.map(|index| index as isize).unwrap_or_else(|| {
            if direction < 0 {
                0
            } else {
                -1
            }
        });
        self.current = Some((current + direction).rem_euclid(length) as usize);
        self.current_match()
    }

    fn refresh_if_needed(&mut self, editor: &EditorState) -> Result<(), SearchError> {
        if editor.snapshot().version() == self.document_version {
            return Ok(());
        }
        Err(SearchError::StaleDocumentVersion {
            expected: self.document_version,
            actual: editor.snapshot().version(),
        })
    }

    fn require_version(&self, actual: u64) -> Result<(), SearchError> {
        if actual == self.document_version {
            Ok(())
        } else {
            Err(SearchError::StaleDocumentVersion {
                expected: self.document_version,
                actual,
            })
        }
    }
}

/// A search is a scan of the whole document by nature, so it reads the text
/// as one string; it runs when the query or the document changes, never per
/// frame.
fn find_matches(text: &crate::Text, query: &SearchQuery) -> Vec<Range<usize>> {
    let text = &text.to_string();
    if query.pattern.is_empty() {
        return Vec::new();
    }
    if query.regex {
        let Ok(regex) = query.compile() else {
            return Vec::new();
        };
        return regex
            .find_iter(text)
            .filter(|found| !found.is_empty())
            .map(|found| found.range())
            .collect();
    }
    let mut matches = match query.case_sensitivity {
        CaseSensitivity::Sensitive => find_sensitive(text, &query.pattern),
        CaseSensitivity::Insensitive => find_insensitive(text, &query.pattern),
    };
    if query.whole_word {
        matches.retain(|range| is_whole_word(text, range));
    }
    matches
}

fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// Whether no word character touches `range` in `text`.
fn is_whole_word(text: &str, range: &Range<usize>) -> bool {
    !text[..range.start]
        .chars()
        .next_back()
        .is_some_and(is_word_character)
        && !text[range.end..]
            .chars()
            .next()
            .is_some_and(is_word_character)
}

/// Ctrl+D: with a caret as the primary selection, selects the word under
/// it; with a selection, adds the next occurrence of its text (exact case)
/// after the last selection, wrapping to the start, skipping text already
/// selected. `None` when there is nothing to select or add.
pub fn add_next_occurrence(text: &crate::Text, selections: &SelectionSet) -> Option<SelectionSet> {
    let primary = selections.primary()?;
    let source = text.to_string();
    if primary.is_caret() {
        let caret = primary.head.min(source.len());
        let start = source[..caret]
            .char_indices()
            .rev()
            .take_while(|(_, character)| is_word_character(*character))
            .last()
            .map_or(caret, |(offset, _)| offset);
        let end = source[caret..]
            .char_indices()
            .take_while(|(_, character)| is_word_character(*character))
            .last()
            .map_or(caret, |(offset, character)| {
                caret + offset + character.len_utf8()
            });
        if start == end {
            return None;
        }
        let mut next: Vec<Selection> = selections
            .selections()
            .iter()
            .copied()
            .filter(|selection| *selection != primary)
            .collect();
        next.push(Selection::range(start, end));
        return Some(SelectionSet::new(next));
    }
    let needle = &source[primary.ordered_range()];
    let taken = |range: &Range<usize>| {
        selections.selections().iter().any(|selection| {
            let selected = selection.ordered_range();
            selected.start < range.end && range.start < selected.end
        })
    };
    let after = primary.ordered_range().end;
    let found = find_sensitive(&source[after..], needle)
        .into_iter()
        .map(|range| range.start + after..range.end + after)
        .chain(find_sensitive(&source[..after], needle))
        .find(|range| !taken(range))?;
    let mut next = selections.selections().to_vec();
    next.push(Selection::range(found.start, found.end));
    Some(SelectionSet::new(next))
}

fn find_sensitive(text: &str, pattern: &str) -> Vec<Range<usize>> {
    let mut matches = Vec::new();
    let mut offset = 0;
    while let Some(relative) = text[offset..].find(pattern) {
        let start = offset + relative;
        let end = start + pattern.len();
        matches.push(start..end);
        offset = end;
    }
    matches
}

fn find_insensitive(text: &str, pattern: &str) -> Vec<Range<usize>> {
    let normalized = NormalizedText::new(text);
    let pattern = pattern.to_lowercase();
    let mut matches = Vec::new();
    let mut offset = 0;
    while let Some(relative) = normalized.text[offset..].find(&pattern) {
        let start = offset + relative;
        let end = start + pattern.len();
        if normalized.text.is_char_boundary(start) && normalized.text.is_char_boundary(end) {
            if let (Some(source_start), Some(source_end)) =
                (normalized.source_start(start), normalized.source_end(end))
            {
                matches.push(source_start..source_end);
            }
        }
        offset = end.max(start + 1);
    }
    matches
}

struct NormalizedText {
    text: String,
    spans: Vec<(Range<usize>, Range<usize>)>,
}

impl NormalizedText {
    fn new(source: &str) -> Self {
        let mut text = String::new();
        let mut spans = Vec::new();
        for (start, character) in source.char_indices() {
            let end = start + character.len_utf8();
            let normalized = character.to_lowercase().collect::<String>();
            let normalized_start = text.len();
            text.push_str(&normalized);
            let normalized_end = text.len();
            spans.push((normalized_start..normalized_end, start..end));
        }
        Self { text, spans }
    }

    fn source_start(&self, offset: usize) -> Option<usize> {
        self.spans
            .iter()
            .find(|(normalized, _)| normalized.start == offset)
            .map(|(_, source)| source.start)
    }

    fn source_end(&self, offset: usize) -> Option<usize> {
        self.spans
            .iter()
            .find(|(normalized, _)| normalized.end == offset)
            .map(|(_, source)| source.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Document, Text};

    fn matches(text: &str, query: SearchQuery) -> Vec<&str> {
        let document = Document::new(text);
        let session = SearchSession::new(&document.snapshot(), query);
        session
            .matches()
            .iter()
            .map(|range| &text[range.clone()])
            .collect()
    }

    #[test]
    fn whole_word_skips_matches_inside_words() {
        let text = "let foo = foobar + foo_x + (foo);";
        assert_eq!(matches(text, SearchQuery::new("foo")).len(), 4);
        let whole = SearchQuery::new("foo").whole_word(true);
        assert_eq!(matches(text, whole.clone()), ["foo", "foo"]);
        let insensitive = whole.case_sensitivity(CaseSensitivity::Insensitive);
        assert_eq!(matches("Foo FOOD foo", insensitive), ["Foo", "foo"]);
    }

    #[test]
    fn regex_matches_with_case_lines_and_whole_words() {
        let text = "fn alpha() {}\nfn Beta() {}\nlet fnord = 1;";
        let query = SearchQuery::new(r"fn \w+").regex(true);
        assert_eq!(matches(text, query.clone()), ["fn alpha", "fn Beta"]);
        assert_eq!(
            matches(text, SearchQuery::new(r"^fn").regex(true)).len(),
            2,
            "^ matches at each line start"
        );
        let insensitive = SearchQuery::new("beta")
            .regex(true)
            .case_sensitivity(CaseSensitivity::Insensitive);
        assert_eq!(matches(text, insensitive), ["Beta"]);
        let whole = SearchQuery::new("fn").regex(true).whole_word(true);
        assert_eq!(matches(text, whole).len(), 2, "not the fn in fnord");
        assert!(
            matches(text, SearchQuery::new("x*").regex(true)).is_empty(),
            "empty matches are skipped"
        );
    }

    #[test]
    fn an_invalid_regex_reports_why_and_matches_nothing() {
        let query = SearchQuery::new("(unclosed").regex(true);
        assert!(query.error().is_some());
        assert!(matches("(unclosed", query).is_empty());
        assert!(
            SearchQuery::new("(unclosed").error().is_none(),
            "plain text"
        );
    }

    #[test]
    fn regex_replacements_expand_groups() {
        let mut editor = EditorState::new("a=1, b=2");
        let query = SearchQuery::new(r"(\w)=(\d)").regex(true);
        let mut session = SearchSession::new(&editor.snapshot(), query);
        session.replace_all(&mut editor, "$2:$1").unwrap();
        assert_eq!(editor.snapshot().text().to_string(), "1:a, 2:b");

        let mut editor = EditorState::new("x=1 y=2");
        let query = SearchQuery::new(r"(?<name>\w)=\d").regex(true);
        let mut session = SearchSession::new(&editor.snapshot(), query);
        session.next_match();
        session.replace_current(&mut editor, "${name}$$").unwrap();
        assert_eq!(editor.snapshot().text().to_string(), "x$ y=2");
    }

    #[test]
    fn add_next_occurrence_selects_the_word_then_adds_the_next_ones() {
        let text = Text::new("foo bar foo\nfoo foobar");
        let caret = SelectionSet::caret(1);
        let word = add_next_occurrence(&text, &caret).unwrap();
        assert_eq!(word.selections(), [Selection::range(0, 3)]);

        let two = add_next_occurrence(&text, &word).unwrap();
        assert_eq!(two.selections()[1].ordered_range(), 8..11);
        let three = add_next_occurrence(&text, &two).unwrap();
        assert_eq!(three.selections()[2].ordered_range(), 12..15);
        let four = add_next_occurrence(&text, &three).unwrap();
        assert_eq!(
            four.selections()[3].ordered_range(),
            16..19,
            "the text, not the word: foobar contains foo"
        );
        assert!(
            add_next_occurrence(&text, &four).is_none(),
            "every occurrence is selected"
        );

        // Wraps to the start for occurrences before the last selection.
        let from_middle = SelectionSet::new([Selection::range(8, 11)]);
        let wrapped = add_next_occurrence(&text, &from_middle).unwrap();
        assert_eq!(wrapped.len(), 2);
        // Later occurrences first, then the one before it.
        let third = add_next_occurrence(&text, &wrapped).unwrap();
        let wrapped = add_next_occurrence(&text, &third).unwrap();
        let ranges: Vec<_> = wrapped
            .selections()
            .iter()
            .map(Selection::ordered_range)
            .collect();
        assert_eq!(ranges, [0..3, 8..11, 12..15, 16..19]);

        assert!(
            add_next_occurrence(&text, &SelectionSet::caret(3 + 1)).is_some(),
            "a caret at a word's start"
        );
        assert!(add_next_occurrence(&Text::new("a  b"), &SelectionSet::caret(2)).is_none());
    }
}
