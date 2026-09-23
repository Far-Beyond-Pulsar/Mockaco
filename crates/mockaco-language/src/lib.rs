//! Replaceable language-service contracts for Mockaco.
//!
//! Providers operate on `mockaco_core::DocumentSnapshot` and versioned byte
//! ranges. Parser implementations, including a future Tree-sitter adapter,
//! can live outside this crate without leaking parser types into the editor.

use mockaco_core::{AppliedTransaction, ChangeMap, DocumentSnapshot};
use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;
use std::sync::{Arc, Mutex};
use streaming_iterator::StreamingIterator;
use tree_sitter::{InputEdit, Language, Parser, Point, Query, QueryCursor};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LanguageId(String);

impl LanguageId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into().to_ascii_lowercase())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LanguageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BracketPair {
    pub open: char,
    pub close: char,
}

impl BracketPair {
    pub fn new(open: char, close: char) -> Self {
        Self { open, close }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageConfig {
    pub id: LanguageId,
    pub extensions: Vec<String>,
    pub line_comment: Option<String>,
    pub block_comment: Option<(String, String)>,
    pub brackets: Vec<BracketPair>,
    pub indent_unit: String,
    pub tab_width: usize,
}

impl LanguageConfig {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: LanguageId::new(id),
            extensions: Vec::new(),
            line_comment: None,
            block_comment: None,
            brackets: vec![
                BracketPair::new('(', ')'),
                BracketPair::new('[', ']'),
                BracketPair::new('{', '}'),
            ],
            indent_unit: "    ".to_owned(),
            tab_width: 4,
        }
    }

    pub fn extension(mut self, extension: impl Into<String>) -> Self {
        let extension = extension
            .into()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        if !extension.is_empty() && !self.extensions.contains(&extension) {
            self.extensions.push(extension);
        }
        self
    }

    pub fn line_comment(mut self, comment: impl Into<String>) -> Self {
        self.line_comment = Some(comment.into());
        self
    }

    pub fn block_comment(mut self, open: impl Into<String>, close: impl Into<String>) -> Self {
        self.block_comment = Some((open.into(), close.into()));
        self
    }

    pub fn indent_unit(mut self, unit: impl Into<String>) -> Self {
        self.indent_unit = unit.into();
        self
    }

    pub fn tab_width(mut self, width: usize) -> Self {
        self.tab_width = width.max(1);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    DuplicateLanguage(LanguageId),
    DuplicateExtension {
        extension: String,
        existing: LanguageId,
    },
    UnknownLanguage(LanguageId),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateLanguage(id) => write!(f, "language {id} is already registered"),
            Self::DuplicateExtension {
                extension,
                existing,
            } => {
                write!(
                    f,
                    "extension .{extension} is already registered for {existing}"
                )
            }
            Self::UnknownLanguage(id) => write!(f, "language {id} is not registered"),
        }
    }
}

impl std::error::Error for RegistryError {}

#[derive(Debug, Clone, Default)]
pub struct LanguageRegistry {
    languages: BTreeMap<LanguageId, LanguageConfig>,
    extensions: BTreeMap<String, LanguageId>,
}

impl LanguageRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, config: LanguageConfig) -> Result<(), RegistryError> {
        if self.languages.contains_key(&config.id) {
            return Err(RegistryError::DuplicateLanguage(config.id));
        }
        for extension in &config.extensions {
            if let Some(existing) = self.extensions.get(extension) {
                return Err(RegistryError::DuplicateExtension {
                    extension: extension.clone(),
                    existing: existing.clone(),
                });
            }
        }
        let id = config.id.clone();
        for extension in &config.extensions {
            self.extensions.insert(extension.clone(), id.clone());
        }
        self.languages.insert(id, config);
        Ok(())
    }

    pub fn get(&self, id: &LanguageId) -> Option<&LanguageConfig> {
        self.languages.get(id)
    }

    pub fn language_for_path(&self, path: &str) -> Option<&LanguageConfig> {
        let file_name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let extension = file_name.rsplit_once('.')?.1.to_ascii_lowercase();
        let id = self.extensions.get(&extension)?;
        self.languages.get(id)
    }

    pub fn language_for_extension(&self, extension: &str) -> Option<&LanguageConfig> {
        let extension = extension.trim_start_matches('.').to_ascii_lowercase();
        let id = self.extensions.get(&extension)?;
        self.languages.get(id)
    }

    pub fn languages(&self) -> impl Iterator<Item = &LanguageConfig> {
        self.languages.values()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TokenKind(String);

impl TokenKind {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxToken {
    pub range: Range<usize>,
    pub kind: TokenKind,
}

impl SyntaxToken {
    pub fn new(range: Range<usize>, kind: impl Into<String>) -> Self {
        Self {
            range,
            kind: TokenKind::new(kind),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightResult {
    pub version: u64,
    pub range: Range<usize>,
    pub tokens: Vec<SyntaxToken>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LanguageError {
    InvalidRange(Range<usize>),
    StaleVersion { expected: u64, actual: u64 },
    Provider(String),
}

impl fmt::Display for LanguageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRange(range) => write!(f, "invalid language range {range:?}"),
            Self::StaleVersion { expected, actual } => {
                write!(
                    f,
                    "stale language result: expected {expected}, got {actual}"
                )
            }
            Self::Provider(error) => f.write_str(error),
        }
    }
}

impl std::error::Error for LanguageError {}

pub trait SyntaxHighlightProvider: Send + Sync {
    fn highlight(
        &self,
        snapshot: &DocumentSnapshot,
        range: Range<usize>,
    ) -> Result<HighlightResult, LanguageError>;
}

/// Runtime provider seam for adding languages without coupling the editor to
/// a particular parser implementation. Hosts can register providers as they
/// load language support and query them by the normalized [`LanguageId`].
#[derive(Default)]
pub struct LanguageProviderRegistry {
    highlighters: BTreeMap<LanguageId, Arc<dyn SyntaxHighlightProvider>>,
    folders: BTreeMap<LanguageId, Arc<dyn FoldingProvider>>,
}

impl fmt::Debug for LanguageProviderRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LanguageProviderRegistry")
            .field(
                "highlighters",
                &self.highlighters.keys().collect::<Vec<_>>(),
            )
            .field("folders", &self.folders.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl LanguageProviderRegistry {
    pub fn register_highlighter(
        &mut self,
        language: impl Into<LanguageId>,
        provider: Arc<dyn SyntaxHighlightProvider>,
    ) {
        self.highlighters.insert(language.into(), provider);
    }

    pub fn register_folder(
        &mut self,
        language: impl Into<LanguageId>,
        provider: Arc<dyn FoldingProvider>,
    ) {
        self.folders.insert(language.into(), provider);
    }

    pub fn highlighter(&self, language: &LanguageId) -> Option<Arc<dyn SyntaxHighlightProvider>> {
        self.highlighters.get(language).cloned()
    }

    pub fn folder(&self, language: &LanguageId) -> Option<Arc<dyn FoldingProvider>> {
        self.folders.get(language).cloned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldRange {
    pub start_line: usize,
    pub end_line: usize,
    pub placeholder: String,
}

impl FoldRange {
    pub fn new(start_line: usize, end_line: usize) -> Self {
        Self {
            start_line,
            end_line,
            placeholder: "…".to_owned(),
        }
    }
}

pub trait FoldingProvider: Send + Sync {
    fn fold_ranges(
        &self,
        snapshot: &DocumentSnapshot,
        range: Range<usize>,
    ) -> Result<Vec<FoldRange>, LanguageError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BracketMatch {
    pub open: usize,
    pub close: usize,
}

pub trait BracketProvider: Send + Sync {
    fn matching_bracket(
        &self,
        snapshot: &DocumentSnapshot,
        byte_offset: usize,
    ) -> Result<Option<BracketMatch>, LanguageError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Indentation {
    pub columns: usize,
    pub unit: String,
}

pub trait IndentationProvider: Send + Sync {
    fn indentation(
        &self,
        snapshot: &DocumentSnapshot,
        line: usize,
    ) -> Result<Indentation, LanguageError>;
}

/// The supported concrete parser provider for Rust source.
///
/// The parser and syntax tree are kept behind a mutex so callers can run the
/// provider from a background worker while the editor continues painting the
/// last accepted result. The public contracts intentionally expose only byte
/// ranges and semantic token names.
pub struct RustTreeSitterProvider {
    highlights: Query,
    state: Mutex<RustParserState>,
}

impl fmt::Debug for RustTreeSitterProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RustTreeSitterProvider")
            .field("capture_count", &self.highlights.capture_names().len())
            .finish_non_exhaustive()
    }
}

struct RustParserState {
    parser: Parser,
    tree: Option<tree_sitter::Tree>,
    text: String,
    version: Option<u64>,
}

impl RustTreeSitterProvider {
    pub fn new() -> Result<Self, LanguageError> {
        let language: Language = tree_sitter_rust::LANGUAGE.into();
        let mut parser = Parser::new();
        parser
            .set_language(&language)
            .map_err(|error| LanguageError::Provider(error.to_string()))?;
        let highlights = Query::new(&language, tree_sitter_rust::HIGHLIGHTS_QUERY)
            .map_err(|error| LanguageError::Provider(error.to_string()))?;
        Ok(Self {
            highlights,
            state: Mutex::new(RustParserState {
                parser,
                tree: None,
                text: String::new(),
                version: None,
            }),
        })
    }

    pub fn language_config() -> LanguageConfig {
        LanguageConfig::new("rust")
            .extension("rs")
            .line_comment("//")
            .block_comment("/*", "*/")
    }

    /// Incrementally reparses after a core transaction when the previous
    /// parser state is the transaction's before-version. A full parse is used
    /// as a safe fallback when a worker receives a snapshot out of order.
    pub fn apply_transaction(
        &self,
        before: &DocumentSnapshot,
        after: &DocumentSnapshot,
        transaction: &AppliedTransaction,
        range: Range<usize>,
    ) -> Result<HighlightResult, LanguageError> {
        if before.version() != transaction.before_version
            || after.version() != transaction.after_version
        {
            return Err(LanguageError::StaleVersion {
                expected: transaction.after_version,
                actual: after.version(),
            });
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| LanguageError::Provider("parser state lock poisoned".into()))?;
        if let Some(version) = state.version {
            if version != before.version() || state.text != before.text() {
                return Err(LanguageError::StaleVersion {
                    expected: version,
                    actual: before.version(),
                });
            }
        }
        if state.version == Some(before.version()) && state.text == before.text() {
            for edit in transaction.change_map.edits().iter().rev() {
                let new_range = transaction.change_map.map_range(edit.range.clone());
                let old_start = point_at(before.text(), edit.range.start);
                let old_end = point_at(before.text(), edit.range.end);
                let new_end = point_at(after.text(), new_range.end);
                if let Some(tree) = state.tree.as_mut() {
                    tree.edit(&InputEdit {
                        start_byte: edit.range.start,
                        old_end_byte: edit.range.end,
                        new_end_byte: new_range.end,
                        start_position: old_start,
                        old_end_position: old_end,
                        new_end_position: new_end,
                    });
                }
            }
        } else {
            state.tree = None;
        }
        let old_tree = state.tree.take();
        state.tree = state.parser.parse(after.text(), old_tree.as_ref());
        state.text = after.text().to_owned();
        state.version = Some(after.version());
        let tree = state
            .tree
            .as_ref()
            .ok_or_else(|| LanguageError::Provider("Tree-sitter returned no tree".into()))?;
        Ok(self.capture_highlights(tree, after, range))
    }

    pub fn fold_ranges_versioned(
        &self,
        snapshot: &DocumentSnapshot,
        range: Range<usize>,
    ) -> Result<VersionedFoldResult, LanguageError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| LanguageError::Provider("parser state lock poisoned".into()))?;
        if state.version != Some(snapshot.version()) || state.text != snapshot.text() {
            state.tree = None;
            state.tree = state.parser.parse(snapshot.text(), None);
            state.text = snapshot.text().to_owned();
            state.version = Some(snapshot.version());
        }
        let tree = state
            .tree
            .as_ref()
            .ok_or_else(|| LanguageError::Provider("Tree-sitter returned no tree".into()))?;
        let mut folds = Vec::new();
        collect_folds(tree.root_node(), &range, &mut folds);
        Ok(VersionedFoldResult {
            version: snapshot.version(),
            ranges: folds,
        })
    }

    fn capture_highlights(
        &self,
        tree: &tree_sitter::Tree,
        snapshot: &DocumentSnapshot,
        range: Range<usize>,
    ) -> HighlightResult {
        let range = clamp_range(snapshot.text(), range);
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range.clone());
        let mut tokens = Vec::new();
        let mut captures = cursor.captures(
            &self.highlights,
            tree.root_node(),
            snapshot.text().as_bytes(),
        );
        while let Some(capture) = captures.next() {
            let query_capture = capture.0.captures[capture.1];
            let node = query_capture.node;
            let start = node.start_byte().max(range.start);
            let end = node.end_byte().min(range.end);
            if start < end {
                let name = self.highlights.capture_names()[query_capture.index as usize];
                tokens.push(SyntaxToken::new(start..end, name));
            }
        }
        tokens.sort_by_key(|token| (token.range.start, token.range.end));
        HighlightResult {
            version: snapshot.version(),
            range,
            tokens,
        }
    }
}

impl SyntaxHighlightProvider for RustTreeSitterProvider {
    fn highlight(
        &self,
        snapshot: &DocumentSnapshot,
        range: Range<usize>,
    ) -> Result<HighlightResult, LanguageError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| LanguageError::Provider("parser state lock poisoned".into()))?;
        if state.version != Some(snapshot.version()) || state.text != snapshot.text() {
            state.tree = None;
            state.tree = state.parser.parse(snapshot.text(), None);
            state.text = snapshot.text().to_owned();
            state.version = Some(snapshot.version());
        }
        let tree = state
            .tree
            .as_ref()
            .ok_or_else(|| LanguageError::Provider("Tree-sitter returned no tree".into()))?;
        Ok(self.capture_highlights(tree, snapshot, range))
    }
}

impl FoldingProvider for RustTreeSitterProvider {
    fn fold_ranges(
        &self,
        snapshot: &DocumentSnapshot,
        range: Range<usize>,
    ) -> Result<Vec<FoldRange>, LanguageError> {
        Ok(self.fold_ranges_versioned(snapshot, range)?.ranges)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionedFoldResult {
    pub version: u64,
    pub ranges: Vec<FoldRange>,
}

fn clamp_range(text: &str, range: Range<usize>) -> Range<usize> {
    let start = range.start.min(text.len());
    let end = range.end.min(text.len()).max(start);
    let start = (0..=start)
        .rev()
        .find(|offset| text.is_char_boundary(*offset))
        .unwrap_or(0);
    let end = (end..=text.len())
        .find(|offset| text.is_char_boundary(*offset))
        .unwrap_or(text.len());
    start..end
}

fn point_at(text: &str, byte: usize) -> Point {
    let byte = byte.min(text.len());
    let prefix = &text[..byte.min(text.len())];
    let row = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let column = prefix.rsplit('\n').next().map_or(0, str::len);
    Point { row, column }
}

fn collect_folds(node: tree_sitter::Node<'_>, range: &Range<usize>, folds: &mut Vec<FoldRange>) {
    let start = node.start_byte();
    let end = node.end_byte();
    if end <= range.start || start >= range.end {
        return;
    }
    let kind = node.kind();
    let foldable = kind.ends_with("_block")
        || matches!(
            kind,
            "block" | "impl_item" | "function_item" | "struct_item" | "enum_item" | "trait_item"
        );
    if foldable && start < range.end && end > range.start {
        let start_line = node.start_position().row;
        // Display folding ranges use an exclusive end line so the closing
        // brace remains inside the collapsed placeholder row.
        let end_line = node.end_position().row.saturating_add(1);
        if start_line < end_line {
            folds.push(FoldRange::new(start_line, end_line));
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_folds(child, range, folds);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl Rgba {
    pub const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha: 255,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FontStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenStyle {
    pub foreground: Rgba,
    pub background: Option<Rgba>,
    pub font: FontStyle,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Theme {
    tokens: BTreeMap<TokenKind, TokenStyle>,
    pub editor_background: Option<Rgba>,
    pub editor_foreground: Option<Rgba>,
}

impl Theme {
    pub fn set_token_style(&mut self, kind: impl Into<String>, style: TokenStyle) {
        self.tokens.insert(TokenKind::new(kind), style);
    }

    pub fn style_for(&self, kind: &TokenKind) -> Option<TokenStyle> {
        self.tokens.get(kind).copied()
    }

    pub fn map_tokens(&self, tokens: &[SyntaxToken]) -> Vec<StyledToken> {
        tokens
            .iter()
            .map(|token| StyledToken {
                range: token.range.clone(),
                kind: token.kind.clone(),
                style: self.style_for(&token.kind),
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledToken {
    pub range: Range<usize>,
    pub kind: TokenKind,
    pub style: Option<TokenStyle>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightInvalidation {
    pub old_range: Range<usize>,
    pub new_range: Range<usize>,
    pub preserved_tokens: usize,
    pub dropped_tokens: usize,
    pub version: u64,
}

#[derive(Debug, Clone, Default)]
pub struct IncrementalHighlights {
    version: Option<u64>,
    tokens: Vec<SyntaxToken>,
}

impl IncrementalHighlights {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_result(result: HighlightResult) -> Self {
        Self {
            version: Some(result.version),
            tokens: result.tokens,
        }
    }

    pub fn version(&self) -> Option<u64> {
        self.version
    }

    pub fn tokens(&self) -> &[SyntaxToken] {
        &self.tokens
    }

    /// Returns the sorted token slice intersecting a byte range without
    /// scanning tokens outside that range. Highlight tokens are kept ordered
    /// by start byte when results are accepted.
    pub fn tokens_in_range(&self, range: Range<usize>) -> &[SyntaxToken] {
        let start = self
            .tokens
            .partition_point(|token| token.range.end <= range.start);
        let end = self
            .tokens
            .partition_point(|token| token.range.start < range.end);
        &self.tokens[start.min(end)..end]
    }

    pub fn accept(&mut self, result: HighlightResult) -> Result<(), LanguageError> {
        if let Some(version) = self.version {
            if result.version < version {
                return Err(LanguageError::StaleVersion {
                    expected: version,
                    actual: result.version,
                });
            }
        }
        self.version = Some(result.version);
        self.tokens = result.tokens;
        Ok(())
    }

    /// Accepts a range result while retaining already-highlighted ranges
    /// outside it. The editor maps those retained tokens through the core
    /// transaction before asking the parser for the changed/visible window.
    pub fn accept_range(&mut self, result: HighlightResult) -> Result<(), LanguageError> {
        if let Some(version) = self.version {
            if result.version < version {
                return Err(LanguageError::StaleVersion {
                    expected: version,
                    actual: result.version,
                });
            }
        }
        self.tokens.retain(|token| {
            token.range.end <= result.range.start || token.range.start >= result.range.end
        });
        self.tokens.extend(result.tokens);
        self.tokens
            .sort_by_key(|token| (token.range.start, token.range.end));
        self.version = Some(result.version);
        Ok(())
    }

    pub fn apply_transaction(
        &mut self,
        snapshot: &DocumentSnapshot,
        transaction: &AppliedTransaction,
    ) -> Result<HighlightInvalidation, LanguageError> {
        let expected = self.version.unwrap_or(transaction.before_version);
        if expected != transaction.before_version {
            return Err(LanguageError::StaleVersion {
                expected,
                actual: transaction.before_version,
            });
        }
        if snapshot.version() != transaction.after_version {
            return Err(LanguageError::StaleVersion {
                expected: transaction.after_version,
                actual: snapshot.version(),
            });
        }
        let old_start = transaction
            .change_map
            .edits()
            .iter()
            .map(|edit| edit.range.start)
            .min()
            .unwrap_or(0);
        let old_end = transaction
            .change_map
            .edits()
            .iter()
            .map(|edit| edit.range.end)
            .max()
            .unwrap_or(old_start);
        let old_range = old_start..old_end;
        let new_range = transaction.change_map.map_range(old_range.clone());
        let mut preserved = Vec::new();
        let mut dropped_tokens = 0;
        for token in self.tokens.drain(..) {
            if token.range.end <= old_range.start {
                preserved.push(token);
            } else if token.range.start >= old_range.end {
                preserved.push(SyntaxToken {
                    range: transaction.change_map.map_range(token.range.clone()),
                    kind: token.kind,
                });
            } else {
                dropped_tokens += 1;
            }
        }
        preserved.sort_by_key(|token| token.range.start);
        let preserved_tokens = preserved.len();
        self.tokens = preserved;
        self.version = Some(snapshot.version());
        Ok(HighlightInvalidation {
            old_range,
            new_range,
            preserved_tokens,
            dropped_tokens,
            version: snapshot.version(),
        })
    }
}

/// A small helper for providers that need to reject results after edits.
pub fn version_matches(snapshot: &DocumentSnapshot, result_version: u64) -> bool {
    snapshot.version() == result_version
}

/// Maps a source range through a transaction while preserving its boundary
/// affinity. This is public so parser adapters can share the same policy.
pub fn map_range(changes: &ChangeMap, range: Range<usize>) -> Range<usize> {
    changes.map_range(range)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockaco_core::{Document, Transaction};
    use std::sync::Arc;

    #[test]
    fn registry_detects_extensions_case_insensitively() {
        let mut registry = LanguageRegistry::new();
        registry
            .register(
                LanguageConfig::new("Rust")
                    .extension("rs")
                    .extension("RS")
                    .line_comment("//"),
            )
            .unwrap();
        assert_eq!(
            registry.language_for_path("src/main.RS").unwrap().id,
            LanguageId::new("rust")
        );
        assert!(matches!(
            registry.register(LanguageConfig::new("other").extension("rs")),
            Err(RegistryError::DuplicateExtension { .. })
        ));
    }

    #[test]
    fn provider_registry_keeps_parser_types_behind_language_ids() {
        let provider = Arc::new(RustTreeSitterProvider::new().unwrap());
        let mut registry = LanguageProviderRegistry::default();
        registry.register_highlighter(LanguageId::new("rust"), provider.clone());
        registry.register_folder(LanguageId::new("rust"), provider);
        assert!(registry.highlighter(&LanguageId::new("RUST")).is_some());
        assert!(registry.folder(&LanguageId::new("rust")).is_some());
    }

    #[test]
    fn theme_maps_known_and_unknown_token_kinds() {
        let mut theme = Theme::default();
        let style = TokenStyle {
            foreground: Rgba::rgb(1, 2, 3),
            background: None,
            font: FontStyle {
                bold: true,
                ..FontStyle::default()
            },
        };
        theme.set_token_style("keyword", style);
        let tokens = vec![
            SyntaxToken::new(0..2, "keyword"),
            SyntaxToken::new(3..5, "unknown"),
        ];
        let mapped = theme.map_tokens(&tokens);
        assert_eq!(mapped[0].style, Some(style));
        assert_eq!(mapped[1].style, None);
    }

    #[test]
    fn incremental_highlights_preserve_unaffected_tokens_and_reject_stale_versions() {
        let mut document = Document::new("let x = 1;\nlet y = 2;");
        let initial = HighlightResult {
            version: 0,
            range: 0..document.len_bytes(),
            tokens: vec![
                SyntaxToken::new(0..3, "keyword"),
                SyntaxToken::new(11..14, "keyword"),
            ],
        };
        let mut cache = IncrementalHighlights::from_result(initial);
        let applied = document
            .apply(&Transaction::new().insert(4, "long "))
            .unwrap();
        let invalidation = cache
            .apply_transaction(&document.snapshot(), &applied)
            .unwrap();
        assert_eq!(invalidation.preserved_tokens, 2);
        assert_eq!(invalidation.dropped_tokens, 0);
        assert_eq!(cache.tokens()[0].range, 0..3);
        assert_eq!(cache.tokens()[1].range, 16..19);
        assert!(matches!(
            cache.apply_transaction(&document.snapshot(), &applied),
            Err(LanguageError::StaleVersion { .. })
        ));
    }

    #[test]
    fn versioned_helpers_are_exact() {
        let document = Document::new("abc");
        assert!(version_matches(&document.snapshot(), 0));
        assert!(!version_matches(&document.snapshot(), 1));
        assert_eq!(
            map_range(
                &Transaction::new()
                    .insert(1, "x")
                    .apply("abc", 0)
                    .unwrap()
                    .change_map,
                1..3
            ),
            1..4
        );
    }

    #[test]
    fn rust_tree_sitter_highlights_nested_unicode_constructs() {
        let provider = RustTreeSitterProvider::new().unwrap();
        let document = Document::new(
            "// привет\nfn main() { let π = 42; println!(\"значение: {π}\"); if π > 0 { return; } }\n",
        );
        let result = provider
            .highlight(&document.snapshot(), 0..document.len_bytes())
            .unwrap();
        let kinds: Vec<_> = result
            .tokens
            .iter()
            .map(|token| token.kind.as_str())
            .collect();
        assert!(kinds.contains(&"comment"));
        assert!(kinds.contains(&"keyword"));
        assert!(kinds.contains(&"string"));
        assert!(kinds.contains(&"constant.builtin"));
        assert!(result
            .tokens
            .iter()
            .all(|token| document.text().is_char_boundary(token.range.start)
                && document.text().is_char_boundary(token.range.end)));
    }

    #[test]
    fn rust_tree_sitter_incremental_changes_and_stale_versions_are_guarded() {
        let provider = RustTreeSitterProvider::new().unwrap();
        let mut document = Document::new("fn main() {\n    let value = 1;\n}\n");
        let before = document.snapshot();
        provider.highlight(&before, 0..before.len_bytes()).unwrap();
        let applied = document
            .apply(&Transaction::new().replace(25..26, "2"))
            .unwrap();
        let after = document.snapshot();
        let result = provider
            .apply_transaction(&before, &after, &applied, 0..after.len_bytes())
            .unwrap();
        assert_eq!(result.version, after.version());
        assert!(result
            .tokens
            .iter()
            .any(|token| token.kind.as_str() == "constant.builtin"));
        let stale = Document::new("stale").snapshot();
        assert!(matches!(
            provider.apply_transaction(&stale, &after, &applied, 0..after.len_bytes()),
            Err(LanguageError::StaleVersion { .. })
        ));
    }

    #[test]
    fn rust_tree_sitter_derives_nested_fold_ranges_with_versions() {
        let provider = RustTreeSitterProvider::new().unwrap();
        let document =
            Document::new("fn main() {\n    if true {\n        println!(\"x\");\n    }\n}\n");
        let result = provider
            .fold_ranges_versioned(&document.snapshot(), 0..document.len_bytes())
            .unwrap();
        assert_eq!(result.version, document.version());
        assert!(result.ranges.iter().any(|fold| fold.start_line == 0));
        assert!(result.ranges.iter().any(|fold| fold.start_line == 1));
    }
}
