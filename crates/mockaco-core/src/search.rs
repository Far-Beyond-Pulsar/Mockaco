use crate::{Affinity, AppliedTransaction, Edit, EditorState, Grouping, TransactionError};
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
}

impl SearchQuery {
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            case_sensitivity: CaseSensitivity::Sensitive,
        }
    }

    pub fn case_sensitivity(mut self, case_sensitivity: CaseSensitivity) -> Self {
        self.case_sensitivity = case_sensitivity;
        self
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
        let replacement = replacement.into();
        let transaction =
            crate::Transaction::from_edits([Edit::replace(range.clone(), replacement.clone())]);
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
        let transaction = crate::Transaction::from_edits(
            ranges
                .iter()
                .cloned()
                .map(|range| Edit::replace(range, replacement.clone())),
        );
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

fn find_matches(text: &str, query: &SearchQuery) -> Vec<Range<usize>> {
    if query.pattern.is_empty() {
        return Vec::new();
    }
    match query.case_sensitivity {
        CaseSensitivity::Sensitive => find_sensitive(text, &query.pattern),
        CaseSensitivity::Insensitive => find_insensitive(text, &query.pattern),
    }
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
