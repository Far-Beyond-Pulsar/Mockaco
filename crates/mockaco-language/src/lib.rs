//! Replaceable language-service contracts for Mockaco.
//!
//! Providers operate on `mockaco_core::DocumentSnapshot` and versioned byte
//! ranges. Parser implementations, including a future Tree-sitter adapter,
//! can live outside this crate without leaking parser types into the editor.

use mockaco_core::{AppliedTransaction, ChangeMap, DocumentSnapshot};
use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;

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
}
