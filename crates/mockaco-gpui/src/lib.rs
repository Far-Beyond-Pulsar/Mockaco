//! Presentation and input adapter for Mockaco.
//!
//! The default API contains no WGPUI types. It produces render data and routes
//! input through `mockaco-core` and `mockaco-renderer`; the optional
//! `native-wgpui` module is the only place that imports the real WGPUI crate.

use mockaco_core::{
    Affinity, DiagnosticSet, DocumentSnapshot, Edit, EditorState, Grouping, SearchSession,
    Selection, SelectionSet, Transaction, TransactionError,
};
use mockaco_diff::{
    DiffEditor, DiffError, DiffPosition, DiffResult, DiffRowKind, DiffScrollMode, DiffScrollState,
    DiffSide,
};
use mockaco_language::{
    FoldRange, FoldingProvider, FontStyle, GenericSyntaxProvider, HighlightResult,
    IncrementalHighlights, LanguageError, LanguageSyntax, Rgba, RustTreeSitterProvider,
    SyntaxHighlightProvider, Theme, TokenKind, TokenStyle,
};
use mockaco_renderer::{
    diagnostic_decorations, search_decorations, Decoration, DisplayMap, DisplayMapError,
    DisplayPoint, DisplayViewport, GutterLayout, ProjectedDecoration,
};
use std::fmt;
use std::ops::Range;

pub mod chrome;
#[cfg(feature = "native-wgpui")]
pub mod native;

/// Language the surface highlights and folds. Rust uses the bundled
/// Tree-sitter grammar; every other known language uses the lexical provider.
#[derive(Debug, Clone, Copy)]
pub enum Language {
    Plain,
    Rust,
    Syntax(&'static LanguageSyntax),
}

impl Language {
    pub fn id(self) -> &'static str {
        match self {
            Self::Plain => "text",
            Self::Rust => "rust",
            Self::Syntax(syntax) => syntax.id,
        }
    }

    pub fn from_extension(extension: &str) -> Self {
        let extension = extension.trim_start_matches('.');
        if extension.eq_ignore_ascii_case("rs") {
            Self::Rust
        } else {
            mockaco_language::syntax_for_extension(extension).map_or(Self::Plain, Self::Syntax)
        }
    }

    pub fn from_path(path: impl AsRef<std::path::Path>) -> Self {
        path.as_ref()
            .extension()
            .and_then(|extension| extension.to_str())
            .map_or(Self::Plain, Self::from_extension)
    }

    /// Line-comment prefix used by minimap tone detection and comment commands.
    pub fn line_comment(self) -> Option<&'static str> {
        match self {
            Self::Plain => None,
            Self::Rust => Some("//"),
            Self::Syntax(syntax) => syntax.line_comments.first().copied(),
        }
    }
}

impl PartialEq for Language {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl Eq for Language {}

#[derive(Debug)]
enum Highlighter {
    Plain,
    Rust(RustTreeSitterProvider),
    Generic(GenericSyntaxProvider),
}

impl Highlighter {
    fn new(language: Language) -> Self {
        match language {
            Language::Plain => Self::Plain,
            Language::Rust => {
                Self::Rust(RustTreeSitterProvider::new().expect("Rust Tree-sitter provider loads"))
            }
            Language::Syntax(syntax) => Self::Generic(GenericSyntaxProvider::new(syntax)),
        }
    }

    fn highlight(
        &self,
        snapshot: &DocumentSnapshot,
        range: Range<usize>,
    ) -> Result<HighlightResult, LanguageError> {
        match self {
            Self::Plain => Err(LanguageError::Provider("plain text".into())),
            Self::Rust(provider) => provider.highlight(snapshot, range),
            Self::Generic(provider) => provider.highlight(snapshot, range),
        }
    }

    fn fold_ranges(
        &self,
        snapshot: &DocumentSnapshot,
        range: Range<usize>,
    ) -> Result<Vec<FoldRange>, LanguageError> {
        match self {
            Self::Plain => Ok(Vec::new()),
            Self::Rust(provider) => provider.fold_ranges(snapshot, range),
            Self::Generic(provider) => provider.fold_ranges(snapshot, range),
        }
    }
}

/// Minimum digits reserved for line numbers.
pub const GUTTER_MIN_DIGITS: usize = 3;
/// Width of the diagnostic/search marker column at the gutter's left edge.
pub const GUTTER_MARKER_WIDTH: f32 = 14.0;
/// Gap between the line numbers and the fold chevron column.
pub const GUTTER_NUMBER_PADDING: f32 = 6.0;
/// Width of the fold chevron column at the gutter's right edge.
pub const GUTTER_FOLD_WIDTH: f32 = 18.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceGeometry {
    pub width: f32,
    pub height: f32,
    pub line_height: f32,
    pub character_width: f32,
    pub gutter_character_width: f32,
}

/// Horizontal breathing room between the gutter and the first code column in
/// the native presentation. Mouse hit testing uses the same constant as paint
/// so a click lands on the glyph under the pointer.
pub const CODE_PADDING: f32 = 12.0;

/// Extra columns of horizontal scroll range beyond the longest line, so the
/// caret at a line's end is never flush against the viewport edge.
const HORIZONTAL_SCROLL_SLACK: usize = 6;

impl Default for SurfaceGeometry {
    fn default() -> Self {
        Self {
            width: 800.0,
            height: 600.0,
            line_height: 20.0,
            character_width: 8.0,
            gutter_character_width: 8.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScrollState {
    pub top_row: usize,
    pub horizontal_columns: usize,
    pub viewport_rows: usize,
    pub viewport_columns: usize,
    pub content_rows: usize,
    pub content_columns: usize,
}

impl ScrollState {
    pub fn set_viewport(&mut self, rows: usize, columns: usize) {
        self.viewport_rows = rows;
        self.viewport_columns = columns;
        self.clamp();
    }

    pub fn set_content(&mut self, rows: usize, columns: usize) {
        self.set_content_with_bottom_padding(rows, columns, 0);
    }

    /// Sets the scrollable content size while reserving presentation-only
    /// rows after the real content. The padding participates in scroll bounds
    /// but is never a document/display row.
    pub fn set_content_with_bottom_padding(
        &mut self,
        rows: usize,
        columns: usize,
        bottom_padding_rows: usize,
    ) {
        self.content_rows = rows.saturating_add(bottom_padding_rows);
        self.content_columns = columns;
        self.clamp();
    }

    pub fn scroll_by(&mut self, vertical: isize, horizontal: isize) -> bool {
        let before = *self;
        self.top_row = offset(self.top_row, vertical);
        self.horizontal_columns = offset(self.horizontal_columns, horizontal);
        self.clamp();
        *self != before
    }

    pub fn scroll_to(&mut self, row: usize, column: usize) -> bool {
        let before = *self;
        self.top_row = row;
        self.horizontal_columns = column;
        self.clamp();
        *self != before
    }

    pub fn visible_rows(&self) -> Range<usize> {
        let end = self
            .top_row
            .saturating_add(self.viewport_rows)
            .min(self.content_rows);
        self.top_row.min(self.content_rows)..end
    }

    fn clamp(&mut self) {
        let max_top = self.content_rows.saturating_sub(self.viewport_rows);
        let max_left = self.content_columns.saturating_sub(self.viewport_columns);
        self.top_row = self.top_row.min(max_top);
        self.horizontal_columns = self.horizontal_columns.min(max_left);
    }
}

fn offset(value: usize, delta: isize) -> usize {
    if delta.is_negative() {
        value.saturating_sub(delta.unsigned_abs())
    } else {
        value.saturating_add(delta as usize)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidationKind {
    Document,
    Decoration,
    Selection,
    Viewport,
    Scroll,
    Composition,
    Geometry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalidation {
    pub kind: InvalidationKind,
    pub rows: Option<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InvalidationState {
    revision: u64,
    pending: Vec<Invalidation>,
}

impl InvalidationState {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn pending(&self) -> &[Invalidation] {
        &self.pending
    }

    pub fn take(&mut self) -> InvalidationBatch {
        let invalidations = std::mem::take(&mut self.pending);
        InvalidationBatch {
            revision: self.revision,
            invalidations,
        }
    }

    fn push(&mut self, kind: InvalidationKind, rows: Option<Range<usize>>) {
        self.revision = self.revision.saturating_add(1);
        self.pending.push(Invalidation { kind, rows });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidationBatch {
    pub revision: u64,
    pub invalidations: Vec<Invalidation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl SurfaceColor {
    pub const fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceTheme {
    pub background: SurfaceColor,
    pub gutter_background: SurfaceColor,
    pub foreground: SurfaceColor,
    pub gutter_foreground: SurfaceColor,
    pub selection: SurfaceColor,
    pub primary_selection: SurfaceColor,
    pub caret: SurfaceColor,
    pub decoration: SurfaceColor,
    pub active_line: SurfaceColor,
    pub syntax: Theme,
}

impl Default for SurfaceTheme {
    fn default() -> Self {
        Self {
            background: SurfaceColor::rgba(30, 30, 30, 255),
            gutter_background: SurfaceColor::rgba(37, 37, 38, 255),
            foreground: SurfaceColor::rgba(220, 220, 220, 255),
            gutter_foreground: SurfaceColor::rgba(128, 128, 128, 255),
            selection: SurfaceColor::rgba(55, 95, 145, 180),
            primary_selection: SurfaceColor::rgba(70, 115, 180, 210),
            caret: SurfaceColor::rgba(235, 235, 235, 255),
            decoration: SurfaceColor::rgba(220, 170, 70, 190),
            active_line: SurfaceColor::rgba(36, 44, 58, 150),
            syntax: syntax_theme(),
        }
    }
}

fn syntax_theme() -> Theme {
    let mut theme = Theme::default();
    let style = |red, green, blue| TokenStyle {
        foreground: Rgba::rgb(red, green, blue),
        background: None,
        font: FontStyle::default(),
    };
    for kind in ["keyword", "keyword.operator"] {
        theme.set_token_style(kind, style(198, 120, 221));
    }
    for kind in ["function", "function.method", "function.macro"] {
        theme.set_token_style(kind, style(97, 175, 239));
    }
    for kind in ["type", "type.builtin", "constructor"] {
        theme.set_token_style(kind, style(229, 192, 123));
    }
    for kind in ["string", "character", "escape"] {
        theme.set_token_style(kind, style(152, 195, 121));
    }
    for kind in ["comment", "comment.documentation"] {
        theme.set_token_style(kind, style(92, 160, 102));
    }
    for kind in ["constant", "constant.builtin", "number"] {
        theme.set_token_style(kind, style(209, 154, 102));
    }
    for kind in ["attribute", "property", "variable.parameter"] {
        theme.set_token_style(kind, style(86, 182, 194));
    }
    for kind in ["operator", "punctuation.bracket", "punctuation.delimiter"] {
        theme.set_token_style(kind, style(171, 178, 191));
    }
    for kind in ["tag", "title", "label"] {
        theme.set_token_style(kind, style(224, 108, 117));
    }
    for kind in ["variable", "variable.builtin", "link_text", "emphasis"] {
        theme.set_token_style(kind, style(224, 108, 117));
    }
    for kind in ["link_uri", "punctuation.list_marker", "emphasis.strong"] {
        theme.set_token_style(kind, style(86, 182, 194));
    }
    theme
}

#[derive(Debug, Clone, PartialEq)]
pub struct PaintRow {
    pub display_row: usize,
    pub buffer_line: usize,
    pub source_range: Range<usize>,
    pub text: String,
    pub y: f32,
    pub height: f32,
    pub continuation: bool,
    pub folded: bool,
    pub truncated: bool,
    pub active: bool,
    pub indent_guides: Vec<usize>,
    pub tokens: Vec<PaintToken>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaintToken {
    pub range: Range<usize>,
    pub kind: TokenKind,
    pub style: Option<TokenStyle>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectionGeometry {
    pub display_row: usize,
    pub start_column: usize,
    pub end_column: usize,
    pub x: f32,
    pub width: f32,
    pub y: f32,
    pub height: f32,
    pub primary: bool,
    pub source_range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaretGeometry {
    pub display_row: usize,
    pub column: usize,
    pub x: f32,
    pub y: f32,
    pub height: f32,
    pub primary: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecorationGeometry {
    pub display_row: usize,
    pub x: f32,
    pub width: f32,
    pub y: f32,
    pub height: f32,
    pub style: mockaco_renderer::DecorationStyle,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderFrame {
    pub document_version: u64,
    pub viewport: DisplayViewport,
    pub rows: Vec<PaintRow>,
    pub gutter: GutterLayout,
    pub selections: Vec<SelectionGeometry>,
    pub carets: Vec<CaretGeometry>,
    pub decorations: Vec<ProjectedDecoration>,
    pub decoration_geometry: Vec<DecorationGeometry>,
    pub theme: SurfaceTheme,
    pub invalidation_revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Primary,
    Secondary,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseEvent {
    Down {
        x: f32,
        y: f32,
        button: MouseButton,
        click_count: usize,
    },
    Drag {
        x: f32,
        y: f32,
    },
    /// Shift+click: extends the primary selection from its anchor to the
    /// pointer and starts a drag from that anchor.
    Extend {
        x: f32,
        y: f32,
    },
    Up {
        x: f32,
        y: f32,
        button: MouseButton,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyModifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub command: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Character(String),
    Backspace,
    Delete,
    Enter,
    Tab,
    Escape,
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Unsupported(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub modifiers: KeyModifiers,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImeEvent {
    Update { range: Range<usize>, text: String },
    Commit,
    Cancel,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    Key(KeyEvent),
    Text(String),
    Paste(String),
    Mouse(MouseEvent),
    Scroll { vertical: isize, horizontal: isize },
    Ime(ImeEvent),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditCommand {
    InsertText(String),
    Copy,
    Cut,
    Paste(String),
    DeleteBackward,
    DeleteForward,
    MoveLeft { extend: bool },
    MoveRight { extend: bool },
    MoveUp { extend: bool },
    MoveDown { extend: bool },
    MovePageUp { extend: bool },
    MovePageDown { extend: bool },
    MoveHome { extend: bool },
    MoveEnd { extend: bool },
    MoveWordLeft { extend: bool },
    MoveWordRight { extend: bool },
    MoveDocumentStart { extend: bool },
    MoveDocumentEnd { extend: bool },
    DeleteWordBackward,
    DeleteWordForward,
    SelectAll,
    ToggleFold,
    UnfoldAll,
    Newline,
    Tab,
    Outdent,
    CancelComposition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputError {
    UnsupportedKey(String),
    InvalidMouseCoordinate,
    InvalidComposition(String),
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedKey(key) => write!(f, "unsupported key {key}"),
            Self::InvalidMouseCoordinate => f.write_str("mouse coordinate is outside the editor"),
            Self::InvalidComposition(error) => f.write_str(error),
        }
    }
}

impl std::error::Error for InputError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceError {
    Transaction(TransactionError),
    Display(DisplayMapError),
    Diff(DiffError),
    Input(InputError),
    StaleDocumentVersion { expected: u64, actual: u64 },
}

impl fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transaction(error) => error.fmt(f),
            Self::Display(error) => error.fmt(f),
            Self::Diff(error) => error.fmt(f),
            Self::Input(error) => error.fmt(f),
            Self::StaleDocumentVersion { expected, actual } => {
                write!(
                    f,
                    "stale surface version: expected {expected}, got {actual}"
                )
            }
        }
    }
}

impl std::error::Error for SurfaceError {}

impl From<TransactionError> for SurfaceError {
    fn from(error: TransactionError) -> Self {
        Self::Transaction(error)
    }
}

impl From<DisplayMapError> for SurfaceError {
    fn from(error: DisplayMapError) -> Self {
        Self::Display(error)
    }
}

impl From<DiffError> for SurfaceError {
    fn from(error: DiffError) -> Self {
        Self::Diff(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputOutcome {
    pub handled: bool,
    pub document_changed: bool,
    pub selection_changed: bool,
    pub composition_changed: bool,
    pub scroll_changed: bool,
}

impl InputOutcome {
    fn handled() -> Self {
        Self {
            handled: true,
            document_changed: false,
            selection_changed: false,
            composition_changed: false,
            scroll_changed: false,
        }
    }
}

/// Framework-independent editor presentation state.
#[derive(Debug)]
pub struct EditorSurface {
    editor: EditorState,
    display: DisplayMap,
    highlighter: Highlighter,
    language: Language,
    highlights: IncrementalHighlights,
    language_coverage: Option<(u64, Range<usize>)>,
    scroll: ScrollState,
    geometry: SurfaceGeometry,
    theme: SurfaceTheme,
    decorations: Vec<Decoration>,
    invalidation: InvalidationState,
}

impl EditorSurface {
    pub fn new(
        text: impl Into<String>,
        display_config: mockaco_renderer::DisplayConfig,
        geometry: SurfaceGeometry,
    ) -> Self {
        Self::with_language(text, display_config, geometry, Language::Rust)
    }

    pub fn with_language(
        text: impl Into<String>,
        display_config: mockaco_renderer::DisplayConfig,
        geometry: SurfaceGeometry,
        language: Language,
    ) -> Self {
        let editor = EditorState::new(text);
        let snapshot = editor.snapshot();
        let highlighter = Highlighter::new(language);
        let highlights = IncrementalHighlights::new();
        let display = DisplayMap::new(&snapshot, display_config);
        let mut surface = Self {
            editor,
            display,
            highlighter,
            language,
            highlights,
            language_coverage: None,
            scroll: ScrollState::default(),
            geometry,
            theme: SurfaceTheme::default(),
            decorations: Vec::new(),
            invalidation: InvalidationState::default(),
        };
        surface.recompute_scroll_viewport();
        surface.ensure_language_state_for_viewport();
        surface
    }

    pub fn language(&self) -> Language {
        self.language
    }

    /// Switches the highlighting/folding language and re-derives the
    /// language state for the current viewport.
    pub fn set_language(&mut self, language: Language) {
        if self.language == language {
            return;
        }
        self.language = language;
        self.highlighter = Highlighter::new(language);
        let snapshot = self.editor.snapshot();
        self.refresh_language_state(&snapshot);
        self.invalidation.push(InvalidationKind::Document, None);
    }

    pub fn editor(&self) -> &EditorState {
        &self.editor
    }

    pub fn editor_mut(&mut self) -> &mut EditorState {
        &mut self.editor
    }

    pub fn document(&self) -> &DocumentSnapshot {
        self.display.snapshot()
    }

    pub fn display(&self) -> &DisplayMap {
        &self.display
    }

    pub fn scroll(&self) -> ScrollState {
        self.scroll
    }

    pub fn geometry(&self) -> SurfaceGeometry {
        self.geometry
    }

    pub fn theme(&self) -> SurfaceTheme {
        self.theme.clone()
    }

    pub fn set_theme(&mut self, theme: SurfaceTheme) {
        self.theme = theme;
        self.invalidation.push(InvalidationKind::Geometry, None);
    }

    pub fn set_geometry(&mut self, geometry: SurfaceGeometry) {
        self.geometry = geometry;
        self.recompute_scroll_viewport();
        self.ensure_language_state_for_viewport();
        self.invalidation.push(InvalidationKind::Geometry, None);
    }

    pub fn set_decorations(&mut self, decorations: Vec<Decoration>) {
        let old_rows = self.decoration_rows(&self.decorations);
        let new_rows = self.decoration_rows(&decorations);
        self.decorations = decorations;
        self.decorations
            .sort_by_key(|decoration| (decoration.range.start, decoration.range.end));
        self.invalidation.push(
            InvalidationKind::Decoration,
            merge_ranges(old_rows, new_rows),
        );
    }

    /// Replaces the surface document for host-driven load/external-change
    /// flows while preserving the current display configuration.
    pub fn replace_text(&mut self, text: impl Into<String>) {
        self.editor = EditorState::new(text);
        let snapshot = self.editor.snapshot();
        let config = self.display.config().clone();
        self.display = DisplayMap::new(&snapshot, config);
        self.refresh_language_state(&snapshot);
        self.recompute_scroll_viewport();
        self.invalidation.push(InvalidationKind::Document, None);
    }

    /// Applies an undo and rebuilds the renderer-neutral display map.
    pub fn undo(&mut self) -> bool {
        if !self.editor.undo() {
            return false;
        }
        self.rebuild_display_after_history_change();
        true
    }

    /// Applies a redo and rebuilds the renderer-neutral display map.
    pub fn redo(&mut self) -> bool {
        if !self.editor.redo() {
            return false;
        }
        self.rebuild_display_after_history_change();
        true
    }

    pub fn set_folds(&mut self, folds: mockaco_renderer::FoldSet) {
        self.display.set_folds(folds);
        self.recompute_scroll_viewport();
        self.invalidation.push(InvalidationKind::Geometry, None);
    }

    pub fn set_foldable_regions(
        &mut self,
        folds: impl IntoIterator<Item = mockaco_renderer::FoldRegion>,
    ) {
        self.display.set_foldable_regions(folds);
        self.recompute_scroll_viewport();
        self.invalidation.push(InvalidationKind::Geometry, None);
    }

    pub fn toggle_fold_at_line(&mut self, line: usize) -> bool {
        let Some(region) = self.display.folds().foldable_starting_at(line).cloned() else {
            return false;
        };
        let mut active = self.display.folds().regions().to_vec();
        if let Some(index) = active.iter().position(|fold| fold.start_line == line) {
            active.remove(index);
        } else {
            active.push(region);
        }
        self.display
            .set_folds(mockaco_renderer::FoldSet::with_foldable(
                active,
                self.display.folds().foldable_regions().to_vec(),
            ));
        self.recompute_scroll_viewport();
        self.invalidation.push(InvalidationKind::Geometry, None);
        true
    }

    pub fn unfold_all(&mut self) {
        let foldable = self.display.folds().foldable_regions().to_vec();
        self.display
            .set_folds(mockaco_renderer::FoldSet::with_foldable([], foldable));
        self.recompute_scroll_viewport();
        self.invalidation.push(InvalidationKind::Geometry, None);
    }

    /// Total gutter width: marker column, line numbers, padding, fold chevron.
    /// Paint and hit testing both derive from this so they cannot drift.
    pub fn gutter_width(&self) -> f32 {
        GUTTER_MARKER_WIDTH
            + self.number_column_width()
            + GUTTER_NUMBER_PADDING
            + GUTTER_FOLD_WIDTH
    }

    /// Width of the right-aligned line-number column.
    pub fn number_column_width(&self) -> f32 {
        self.display.line_number_width(GUTTER_MIN_DIGITS) as f32
            * self.geometry.gutter_character_width
    }

    pub fn select_word_at(&mut self, byte: usize) -> bool {
        let text = self.editor.document().text();
        if byte > text.len() || !text.is_char_boundary(byte) {
            return false;
        }
        let is_word = |character: char| character.is_alphanumeric() || character == '_';
        let start = text[..byte]
            .char_indices()
            .rev()
            .take_while(|(_, character)| is_word(*character))
            .last()
            .map_or(byte, |(offset, _)| offset);
        let end = text[byte..]
            .char_indices()
            .take_while(|(_, character)| is_word(*character))
            .last()
            .map_or(byte, |(offset, character)| {
                byte + offset + character.len_utf8()
            });
        let range = if start == end { byte..byte } else { start..end };
        self.editor
            .set_selections(SelectionSet::new([Selection::range(
                range.start,
                range.end,
            )]));
        self.invalidation.push(InvalidationKind::Selection, None);
        start != end
    }

    pub fn select_line_at(&mut self, line: usize) -> bool {
        let map = self.editor.document().position_map();
        let Ok(start) = map.line_start(line) else {
            return false;
        };
        let Ok(end) = map.line_end(line) else {
            return false;
        };
        self.editor
            .set_selections(SelectionSet::new([Selection::range(start, end)]));
        self.invalidation.push(InvalidationKind::Selection, None);
        true
    }

    /// Returns the text represented by the current selections for a platform
    /// clipboard. Multiple carets are separated by a newline, matching the
    /// conventional editor behavior for a multi-selection copy.
    pub fn clipboard_text(&self) -> String {
        let text = self.editor.document().text();
        let selections = self.editor.selections().selections();
        let selected = selections
            .iter()
            .filter(|selection| !selection.is_caret())
            .map(|selection| text[selection.ordered_range()].to_owned())
            .collect::<Vec<_>>();
        if !selected.is_empty() {
            return selected.join("\n");
        }
        selections
            .iter()
            .filter_map(|selection| {
                let line = self
                    .editor
                    .document()
                    .position_map()
                    .byte_to_line(selection.cursor())
                    .ok()?;
                let start = self
                    .editor
                    .document()
                    .position_map()
                    .line_start(line)
                    .ok()?;
                let end = self.editor.document().position_map().line_end(line).ok()?;
                Some(text[start..end].to_owned())
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn set_search_and_diagnostic_decorations(
        &mut self,
        search: Option<&SearchSession>,
        diagnostics: Option<&DiagnosticSet>,
    ) {
        let mut decorations = Vec::new();
        if let Some(search) = search {
            decorations.extend(search_decorations(search));
        }
        if let Some(diagnostics) = diagnostics {
            decorations.extend(diagnostic_decorations(diagnostics));
        }
        self.set_decorations(decorations);
    }

    pub fn set_viewport(&mut self, rows: usize, columns: usize) {
        self.scroll.set_viewport(rows, columns);
        self.ensure_language_state_for_viewport();
        self.invalidation.push(InvalidationKind::Viewport, None);
    }

    pub fn scroll_by(&mut self, vertical: isize, horizontal: isize) -> bool {
        let changed = self.scroll.scroll_by(vertical, horizontal);
        if changed {
            self.ensure_language_state_for_viewport();
            self.invalidation.push(InvalidationKind::Scroll, None);
        }
        changed
    }

    pub fn scroll_to(&mut self, row: usize, column: usize) -> bool {
        let changed = self.scroll.scroll_to(row, column);
        if changed {
            self.ensure_language_state_for_viewport();
            self.invalidation.push(InvalidationKind::Scroll, None);
        }
        changed
    }

    /// Keeps the primary caret inside the logical viewport after navigation or
    /// an edit, in both axes. The native adapter animates the resulting
    /// change, but the invariant is owned by the framework-independent
    /// surface. Only fully visible rows count, so the caret is never revealed
    /// into a clipped row.
    pub fn reveal_primary_caret(&mut self) -> bool {
        let Some(selection) = self.editor.selections().primary() else {
            return false;
        };
        let Ok(point) = self
            .display
            .buffer_to_display(selection.cursor(), Affinity::After)
        else {
            return false;
        };
        let full_rows = ((self.geometry.height / self.geometry.line_height.max(1.0)).floor()
            as usize)
            .clamp(1, self.scroll.viewport_rows.max(1));
        let target_row = if point.row < self.scroll.top_row {
            point.row
        } else if point.row >= self.scroll.top_row.saturating_add(full_rows) {
            point.row.saturating_sub(full_rows.saturating_sub(1))
        } else {
            self.scroll.top_row
        };
        let columns = self.scroll.viewport_columns.max(1);
        let margin = 2.min(columns / 4);
        let left = self.scroll.horizontal_columns;
        let target_column = if point.column < left.saturating_add(margin) {
            point.column.saturating_sub(margin)
        } else if point.column + margin >= left.saturating_add(columns) {
            (point.column + margin + 1).saturating_sub(columns)
        } else {
            left
        };
        self.scroll_to(target_row, target_column)
    }

    pub fn invalidation(&self) -> &InvalidationState {
        &self.invalidation
    }

    pub fn take_invalidations(&mut self) -> InvalidationBatch {
        self.invalidation.take()
    }

    pub fn apply_transaction(
        &mut self,
        transaction: &Transaction,
        grouping: Grouping,
    ) -> Result<mockaco_core::AppliedTransaction, SurfaceError> {
        let before_snapshot = self.editor.snapshot();
        let applied = self.editor.apply_with_result(transaction, grouping)?;
        let snapshot = self.editor.snapshot();
        let mut display = self.display.clone();
        let update = display.update(&snapshot, &applied)?;
        self.display = display;
        let _ = self
            .highlights
            .apply_transaction(&before_snapshot, &applied);
        self.language_coverage = None;
        match &self.highlighter {
            Highlighter::Rust(provider) => {
                if let Ok(result) = provider.apply_transaction(
                    &before_snapshot,
                    &snapshot,
                    &applied,
                    update.new_range.clone(),
                ) {
                    let _ = self.highlights.accept_range(result);
                }
            }
            // Lexical results can change anywhere below an edit (an opened
            // string or comment), so they are re-derived for the viewport.
            Highlighter::Generic(_) | Highlighter::Plain => {
                self.highlights = IncrementalHighlights::new();
            }
        }
        self.recompute_scroll_viewport();
        self.ensure_language_state_for_viewport();
        self.invalidation
            .push(InvalidationKind::Document, Some(update.new_range));
        Ok(applied)
    }

    pub fn commit_composition(&mut self) -> Result<bool, SurfaceError> {
        let Some(applied) = self.editor.commit_composition_with_result()? else {
            return Ok(false);
        };
        let snapshot = self.editor.snapshot();
        let mut display = self.display.clone();
        let update = display.update(&snapshot, &applied)?;
        self.display = display;
        self.refresh_language_state(&snapshot);
        self.recompute_scroll_viewport();
        self.invalidation
            .push(InvalidationKind::Composition, Some(update.new_range));
        Ok(true)
    }

    pub fn accept_external_display_update(
        &mut self,
        snapshot: &DocumentSnapshot,
        transaction: &mockaco_core::AppliedTransaction,
    ) -> Result<(), SurfaceError> {
        let expected = self.editor.snapshot().version();
        if transaction.before_version != expected {
            return Err(SurfaceError::StaleDocumentVersion {
                expected,
                actual: transaction.before_version,
            });
        }
        if snapshot.version() != transaction.after_version {
            return Err(SurfaceError::StaleDocumentVersion {
                expected: transaction.after_version,
                actual: snapshot.version(),
            });
        }
        let mut display = self.display.clone();
        let update = display.update(snapshot, transaction)?;
        self.display = display;
        self.refresh_language_state(snapshot);
        self.recompute_scroll_viewport();
        self.invalidation
            .push(InvalidationKind::Document, Some(update.new_range));
        Ok(())
    }

    pub fn render_frame(&self) -> RenderFrame {
        self.render_frame_with_buffer(0)
    }

    /// Renders the logical viewport plus presentation-only buffer rows around
    /// it. Native scrolling uses one row on each side so fractional
    /// translation can reveal content continuously while the editor viewport
    /// clips the out-of-bounds rows.
    pub fn render_frame_with_buffer(&self, buffer_rows: usize) -> RenderFrame {
        let display_row_count = self.display.row_count();
        let render_start = self
            .scroll
            .top_row
            .saturating_sub(buffer_rows)
            .min(display_row_count);
        let render_end = self
            .scroll
            .top_row
            .saturating_add(self.scroll.viewport_rows)
            .saturating_add(buffer_rows)
            .min(self.scroll.content_rows)
            .min(display_row_count);
        let render_range = render_start..render_end;
        let viewport = DisplayViewport::new(self.scroll.top_row, self.scroll.viewport_rows);
        let snapshot = self.display.snapshot();
        let text = snapshot.text();
        let position_map = snapshot.position_map();
        let active_buffer_line = self
            .editor
            .selections()
            .primary()
            .and_then(|selection| position_map.byte_to_line(selection.cursor()).ok());
        let rows = render_range
            .clone()
            .map(|display_row| {
                let row = &self.display.rows()[display_row];
                PaintRow {
                    display_row,
                    buffer_line: row.buffer_line,
                    source_range: row.start_byte..row.end_byte,
                    text: text[row.start_byte..row.end_byte].to_owned(),
                    y: display_y(display_row, self.scroll.top_row, self.geometry.line_height),
                    height: self.geometry.line_height,
                    continuation: row.continuation,
                    folded: row.folded,
                    truncated: row.truncated,
                    active: active_buffer_line == Some(row.buffer_line),
                    indent_guides: indent_guides_for_line(
                        text,
                        position_map
                            .line_start(row.buffer_line)
                            .unwrap_or(row.start_byte),
                        position_map
                            .line_end(row.buffer_line)
                            .unwrap_or(row.end_byte),
                        self.display.config().tab_width,
                    ),
                    tokens: self
                        .highlights
                        .tokens_in_range(row.start_byte..row.end_byte)
                        .iter()
                        .map(|token| PaintToken {
                            range: token.range.clone(),
                            kind: token.kind.clone(),
                            style: self.theme.syntax.style_for(&token.kind),
                        })
                        .collect(),
                }
            })
            .collect::<Vec<_>>();
        let gutter = self
            .display
            .gutter_for_display_range(GUTTER_MIN_DIGITS, render_range.clone());
        let selections = self.selection_geometry(&render_range);
        let carets = self.caret_geometry(&render_range);
        let decorations: Vec<ProjectedDecoration> = self
            .display
            .project_decorations_in_display_range(&self.decorations, render_range.clone());
        let decoration_geometry = decorations
            .iter()
            .map(|decoration| DecorationGeometry {
                display_row: decoration.display_row,
                x: decoration.start_column as f32 * self.geometry.character_width,
                width: decoration
                    .end_column
                    .saturating_sub(decoration.start_column) as f32
                    * self.geometry.character_width,
                y: display_y(
                    decoration.display_row,
                    self.scroll.top_row,
                    self.geometry.line_height,
                ),
                height: self.geometry.line_height,
                style: decoration.style,
            })
            .collect();
        RenderFrame {
            document_version: self.editor.snapshot().version(),
            viewport,
            rows,
            gutter,
            selections,
            carets,
            decorations,
            decoration_geometry,
            theme: self.theme.clone(),
            invalidation_revision: self.invalidation.revision(),
        }
    }

    fn selection_geometry(&self, visible: &Range<usize>) -> Vec<SelectionGeometry> {
        let selections = self.editor.selections();
        selections
            .selections()
            .iter()
            .enumerate()
            .filter(|(_, selection)| !selection.is_caret())
            .flat_map(|(index, selection)| {
                self.display
                    .project_decorations_in_display_range(
                        &[Decoration::new(selection.ordered_range(), index as u32)],
                        visible.clone(),
                    )
                    .into_iter()
                    .map(move |projection| SelectionGeometry {
                        display_row: projection.display_row,
                        start_column: projection.start_column,
                        end_column: projection.end_column,
                        x: projection.start_column as f32 * self.geometry.character_width,
                        width: projection
                            .end_column
                            .saturating_sub(projection.start_column)
                            as f32
                            * self.geometry.character_width,
                        y: display_y(
                            projection.display_row,
                            self.scroll.top_row,
                            self.geometry.line_height,
                        ),
                        height: self.geometry.line_height,
                        primary: index + 1 == selections.len(),
                        source_range: projection.source_range,
                    })
            })
            .collect()
    }

    fn decoration_rows(&self, decorations: &[Decoration]) -> Option<Range<usize>> {
        let start = decorations
            .iter()
            .filter_map(|decoration| {
                self.display
                    .buffer_to_display(decoration.range.start, Affinity::Before)
                    .ok()
                    .map(|point| point.row)
            })
            .min()?;
        let end = decorations
            .iter()
            .filter_map(|decoration| {
                self.display
                    .buffer_to_display(decoration.range.end, Affinity::After)
                    .ok()
                    .map(|point| point.row.saturating_add(1))
            })
            .max()?;
        Some(start..end)
    }

    fn caret_geometry(&self, visible: &Range<usize>) -> Vec<CaretGeometry> {
        let selections = self.editor.selections();
        selections
            .selections()
            .iter()
            .enumerate()
            .filter_map(|(index, selection)| {
                let point = self
                    .display
                    .buffer_to_display(selection.cursor(), Affinity::After)
                    .ok()?;
                if !visible.contains(&point.row) {
                    return None;
                }
                Some(CaretGeometry {
                    display_row: point.row,
                    column: point.column,
                    x: point.column as f32 * self.geometry.character_width,
                    y: display_y(point.row, self.scroll.top_row, self.geometry.line_height),
                    height: self.geometry.line_height,
                    primary: index + 1 == selections.len(),
                })
            })
            .collect()
    }

    fn recompute_scroll_viewport(&mut self) {
        // A partially visible bottom row still counts as viewport so it is
        // rendered and highlighted while smooth scrolling.
        let rows = ((self.geometry.height / self.geometry.line_height.max(1.0)).ceil() as usize)
            .max(1);
        let columns =
            (self.geometry.width / self.geometry.character_width.max(1.0)).floor() as usize;
        self.scroll.set_viewport(rows, columns);
        // Like VS Code's `scrollBeyondLastLine`, the final line can scroll up
        // to the top edge: reserve (viewport - 1) presentation-only rows.
        self.scroll.set_content_with_bottom_padding(
            self.display.row_count(),
            self.display.max_display_width() + HORIZONTAL_SCROLL_SLACK,
            rows.saturating_sub(1),
        );
    }

    fn rebuild_display_after_history_change(&mut self) {
        let snapshot = self.editor.snapshot();
        let config = self.display.config().clone();
        let folds = self.display.folds().clone();
        self.display = DisplayMap::with_folds(&snapshot, config, folds);
        self.refresh_language_state(&snapshot);
        self.recompute_scroll_viewport();
        self.invalidation.push(InvalidationKind::Document, None);
    }

    fn refresh_language_state(&mut self, snapshot: &DocumentSnapshot) {
        self.highlights = IncrementalHighlights::new();
        self.language_coverage = None;
        self.display.set_foldable_regions([]);
        debug_assert_eq!(snapshot.version(), self.display.snapshot().version());
        self.ensure_language_state_for_viewport();
    }

    fn ensure_language_state_for_viewport(&mut self) {
        if matches!(self.highlighter, Highlighter::Plain) {
            return;
        }
        let document_version = self.display.snapshot().version();
        let rows = self.display.rows();
        if rows.is_empty() {
            return;
        }
        // Rows the frame will paint: the viewport plus the smooth-scroll
        // buffer on both sides.
        let top = self.scroll.top_row.min(rows.len() - 1);
        let needed_start = top.saturating_sub(1);
        let needed_end = top
            .saturating_add(self.scroll.viewport_rows.saturating_add(2))
            .min(rows.len());
        let needed = rows[needed_start].start_byte..rows[needed_end - 1].end_byte;
        if self
            .language_coverage
            .as_ref()
            .is_some_and(|(version, covered)| {
                *version == document_version
                    && covered.start <= needed.start
                    && covered.end >= needed.end
            })
        {
            return;
        }

        // Highlight a generous window around the viewport so ordinary
        // scrolling never leaves unhighlighted rows or fold markers behind.
        let pad = self.scroll.viewport_rows.max(48);
        let start_row = top.saturating_sub(pad);
        let end_row = top
            .saturating_add(self.scroll.viewport_rows)
            .saturating_add(pad)
            .min(rows.len());
        let first = &rows[start_row];
        let last = &rows[end_row - 1];
        let requested = first.start_byte..last.end_byte;

        if let Ok(result) = self
            .highlighter
            .highlight(self.display.snapshot(), requested.clone())
        {
            let _ = self.highlights.accept_range(result);
        }
        let line_start = first.buffer_line;
        let line_end = last.buffer_line.saturating_add(1);
        if let Ok(folds) = self
            .highlighter
            .fold_ranges(self.display.snapshot(), requested.clone())
        {
            self.display.set_foldable_regions_in_line_range(
                line_start..line_end,
                folds.into_iter().map(|fold| {
                    mockaco_renderer::FoldRegion::new(fold.start_line, fold.end_line)
                        .placeholder(fold.placeholder)
                }),
            );
        }
        self.language_coverage = Some((document_version, requested));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffPaintRow {
    pub display_row: usize,
    pub side: DiffSide,
    pub line: Option<usize>,
    pub text: Option<String>,
    pub kind: DiffRowKind,
    pub hunk: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRenderFrame {
    pub original_rows: Vec<DiffPaintRow>,
    pub modified_rows: Vec<DiffPaintRow>,
    pub original_visible_rows: Range<usize>,
    pub modified_visible_rows: Range<usize>,
    pub modified_document_version: u64,
    pub invalidation_revision: u64,
}

/// Framework-neutral presentation state for a split original/modified view.
///
/// The original side is never exposed as an editable `EditorState`; all edit
/// methods route exclusively to the modified side of [`DiffEditor`]. Native
/// WGPUI code can consume [`DiffRenderFrame`] without owning diff computation.
#[derive(Debug, Clone)]
pub struct DiffSplitSurface {
    editor: DiffEditor,
    scroll: DiffScrollState,
    invalidation: InvalidationState,
}

impl DiffSplitSurface {
    pub fn new(original: &DocumentSnapshot, modified_text: impl Into<String>) -> Self {
        let editor = DiffEditor::new(original, modified_text);
        let scroll =
            DiffScrollState::new(DiffScrollMode::Synchronized, editor.diff().rows().len(), 0);
        Self {
            editor,
            scroll,
            invalidation: InvalidationState::default(),
        }
    }

    pub fn editor(&self) -> &DiffEditor {
        &self.editor
    }

    pub fn diff(&self) -> &DiffResult {
        self.editor.diff()
    }

    pub fn scroll(&self) -> DiffScrollState {
        self.scroll
    }

    pub fn set_scroll_mode(&mut self, mode: DiffScrollMode) {
        self.scroll.set_mode(mode);
        self.invalidation.push(InvalidationKind::Scroll, None);
    }

    pub fn set_viewport(&mut self, rows: usize) {
        self.scroll.set_viewport(rows);
        self.invalidation.push(InvalidationKind::Viewport, None);
    }

    pub fn scroll_by(&mut self, side: DiffSide, delta: isize) -> bool {
        let changed = self.scroll.scroll_by(side, delta);
        if changed {
            self.invalidation.push(InvalidationKind::Scroll, None);
        }
        changed
    }

    pub fn scroll_to(&mut self, side: DiffSide, row: usize) -> bool {
        let changed = self.scroll.scroll_to(side, row);
        if changed {
            self.invalidation.push(InvalidationKind::Scroll, None);
        }
        changed
    }

    pub fn apply_modified_edit(
        &mut self,
        range: Range<usize>,
        replacement: impl Into<String>,
    ) -> Result<(), SurfaceError> {
        self.editor.apply_modified_edit(range, replacement)?;
        self.scroll.set_content(self.editor.diff().rows().len());
        self.invalidation.push(InvalidationKind::Document, None);
        Ok(())
    }

    pub fn accept_diff(&mut self, result: DiffResult) -> Result<(), SurfaceError> {
        self.editor.accept_diff(result)?;
        self.scroll.set_content(self.editor.diff().rows().len());
        self.invalidation.push(InvalidationKind::Document, None);
        Ok(())
    }

    pub fn set_original(&mut self, original: &DocumentSnapshot) {
        self.editor.set_original(original);
        self.scroll.set_content(self.editor.diff().rows().len());
        self.invalidation.push(InvalidationKind::Document, None);
    }

    pub fn invalidation(&self) -> &InvalidationState {
        &self.invalidation
    }

    pub fn take_invalidations(&mut self) -> InvalidationBatch {
        self.invalidation.take()
    }

    pub fn render_frame(&self) -> DiffRenderFrame {
        let original_visible = self.scroll.visible_rows(DiffSide::Original);
        let modified_visible = self.scroll.visible_rows(DiffSide::Modified);
        let original_rows = original_visible
            .clone()
            .map(|row| paint_diff_row(self.editor.diff(), row, DiffSide::Original))
            .collect();
        let modified_rows = modified_visible
            .clone()
            .map(|row| paint_diff_row(self.editor.diff(), row, DiffSide::Modified))
            .collect();
        DiffRenderFrame {
            original_rows,
            modified_rows,
            original_visible_rows: original_visible,
            modified_visible_rows: modified_visible,
            modified_document_version: self.editor.modified().snapshot().version(),
            invalidation_revision: self.invalidation.revision(),
        }
    }

    pub fn map_position(
        &self,
        side: DiffSide,
        position: DiffPosition,
        target_side: DiffSide,
    ) -> Result<mockaco_diff::DiffPositionMapping, SurfaceError> {
        Ok(self
            .editor
            .diff()
            .map_position(side, position, target_side)?)
    }
}

fn paint_diff_row(result: &DiffResult, row: usize, side: DiffSide) -> DiffPaintRow {
    let diff_row = &result.rows()[row];
    let line = match side {
        DiffSide::Original => diff_row.original.as_ref(),
        DiffSide::Modified => diff_row.modified.as_ref(),
    };
    DiffPaintRow {
        display_row: row,
        side,
        line: line.map(|line| line.line),
        text: line.map(|line| line.text.clone()),
        kind: diff_row.kind,
        hunk: diff_row.hunk,
    }
}

fn merge_ranges(left: Option<Range<usize>>, right: Option<Range<usize>>) -> Option<Range<usize>> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.start.min(right.start)..left.end.max(right.end)),
        (Some(range), None) | (None, Some(range)) => Some(range),
        (None, None) => None,
    }
}

fn display_y(display_row: usize, top_row: usize, line_height: f32) -> f32 {
    (display_row as f32 - top_row as f32) * line_height
}

#[derive(Debug, Clone, Default)]
pub struct InputRouter {
    drag_anchor: Option<usize>,
    goal_column: Option<usize>,
    /// The previous routed event was a single typed character.
    typing: bool,
    /// The event being routed continues the previous typing run, so its
    /// edit merges into the current undo group.
    group_next: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Space,
    Word,
    Punctuation,
}

fn char_class(character: char) -> CharClass {
    if character.is_whitespace() {
        CharClass::Space
    } else if character.is_alphanumeric() || character == '_' {
        CharClass::Word
    } else {
        CharClass::Punctuation
    }
}

/// Next word boundary from `from`: skip whitespace, then one run of a single
/// character class.
fn word_boundary(text: &str, from: usize, forward: bool) -> usize {
    let from = from.min(text.len());
    if forward {
        let mut index = from;
        let mut chars = text[from..].chars().peekable();
        while let Some(character) = chars.next_if(|c| char_class(*c) == CharClass::Space) {
            index += character.len_utf8();
        }
        if let Some(class) = chars.peek().map(|c| char_class(*c)) {
            while let Some(character) = chars.next_if(|c| char_class(*c) == class) {
                index += character.len_utf8();
            }
        }
        index
    } else {
        let mut index = from;
        let mut chars = text[..from].chars().rev().peekable();
        while let Some(character) = chars.next_if(|c| char_class(*c) == CharClass::Space) {
            index -= character.len_utf8();
        }
        if let Some(class) = chars.peek().map(|c| char_class(*c)) {
            while let Some(character) = chars.next_if(|c| char_class(*c) == class) {
                index -= character.len_utf8();
            }
        }
        index
    }
}

impl InputRouter {
    pub fn translate_key(event: &KeyEvent) -> Result<Option<EditCommand>, InputError> {
        let extend = event.modifiers.shift;
        // Ctrl (or Cmd) moves by word / to the document edge.
        let word = event.modifiers.control || event.modifiers.command;
        let command = match &event.key {
            Key::Character(text)
                if (event.modifiers.control || event.modifiers.command)
                    && text.eq_ignore_ascii_case("c") =>
            {
                Some(EditCommand::Copy)
            }
            Key::Character(text)
                if (event.modifiers.control || event.modifiers.command)
                    && text.eq_ignore_ascii_case("x") =>
            {
                Some(EditCommand::Cut)
            }
            Key::Character(text)
                if (event.modifiers.control || event.modifiers.command)
                    && text.eq_ignore_ascii_case("v") =>
            {
                None
            }
            Key::Character(text)
                if (event.modifiers.control || event.modifiers.command)
                    && text.eq_ignore_ascii_case("a") =>
            {
                Some(EditCommand::SelectAll)
            }
            Key::Character(text) if event.modifiers.control && text == "[" => {
                Some(EditCommand::ToggleFold)
            }
            Key::Character(text) if event.modifiers.control && text == "]" => {
                Some(EditCommand::UnfoldAll)
            }
            Key::Character(text) if !event.modifiers.control && !event.modifiers.command => {
                Some(EditCommand::InsertText(text.clone()))
            }
            Key::Backspace if word => Some(EditCommand::DeleteWordBackward),
            Key::Delete if word => Some(EditCommand::DeleteWordForward),
            Key::Backspace => Some(EditCommand::DeleteBackward),
            Key::Delete => Some(EditCommand::DeleteForward),
            Key::Enter => Some(EditCommand::Newline),
            Key::Tab if event.modifiers.shift => Some(EditCommand::Outdent),
            Key::Tab => Some(EditCommand::Tab),
            Key::Escape => Some(EditCommand::CancelComposition),
            Key::Left if word => Some(EditCommand::MoveWordLeft { extend }),
            Key::Right if word => Some(EditCommand::MoveWordRight { extend }),
            Key::Left => Some(EditCommand::MoveLeft { extend }),
            Key::Right => Some(EditCommand::MoveRight { extend }),
            Key::Up => Some(EditCommand::MoveUp { extend }),
            Key::Down => Some(EditCommand::MoveDown { extend }),
            Key::PageUp => Some(EditCommand::MovePageUp { extend }),
            Key::PageDown => Some(EditCommand::MovePageDown { extend }),
            Key::Home if word => Some(EditCommand::MoveDocumentStart { extend }),
            Key::End if word => Some(EditCommand::MoveDocumentEnd { extend }),
            Key::Home => Some(EditCommand::MoveHome { extend }),
            Key::End => Some(EditCommand::MoveEnd { extend }),
            Key::Character(_) => None,
            Key::Unsupported(key) => return Err(InputError::UnsupportedKey(key.clone())),
        };
        Ok(command)
    }

    pub fn route(
        &mut self,
        surface: &mut EditorSurface,
        event: InputEvent,
    ) -> Result<InputOutcome, SurfaceError> {
        // Consecutive single-character typing merges into one undo step.
        let typed = matches!(&event, InputEvent::Text(text)
            if text.chars().count() == 1 && !text.contains('\n'));
        self.group_next = typed && self.typing;
        if !matches!(event, InputEvent::Scroll { .. }) {
            self.typing = false;
        }
        let mut outcome = match event {
            InputEvent::Text(text) => self.apply_command(surface, EditCommand::InsertText(text)),
            InputEvent::Paste(text) => self.apply_command(surface, EditCommand::Paste(text)),
            InputEvent::Key(event) => {
                match Self::translate_key(&event).map_err(SurfaceError::Input)? {
                    Some(command) => self.apply_command(surface, command),
                    None => Ok(InputOutcome::handled()),
                }
            }
            InputEvent::Scroll {
                vertical,
                horizontal,
            } => {
                let mut outcome = InputOutcome::handled();
                outcome.scroll_changed = surface.scroll_by(vertical, horizontal);
                Ok(outcome)
            }
            InputEvent::Ime(event) => self.route_ime(surface, event),
            InputEvent::Mouse(event) => self.route_mouse(surface, event),
        }?;
        self.typing = typed && outcome.document_changed;
        if outcome.selection_changed || outcome.document_changed {
            outcome.scroll_changed |= surface.reveal_primary_caret();
        }
        Ok(outcome)
    }

    fn grouping(&self) -> Grouping {
        if self.group_next {
            Grouping::CurrentGroup
        } else {
            Grouping::Separate
        }
    }

    fn tab_width(surface: &EditorSurface) -> usize {
        surface.display.config().tab_width.max(1)
    }

    /// Moves every selection head with `target`, extending when requested.
    fn move_to(
        &mut self,
        surface: &mut EditorSurface,
        extend: bool,
        target: impl Fn(&str, usize) -> usize,
    ) -> Result<InputOutcome, SurfaceError> {
        let text = surface.editor.document().text();
        let next = surface
            .editor
            .selections()
            .selections()
            .iter()
            .map(|selection| {
                let to = target(text, selection.head);
                if extend {
                    Selection::range(selection.anchor, to)
                } else {
                    Selection::caret(to)
                }
            })
            .collect::<Vec<_>>();
        surface.editor.set_selections(SelectionSet::new(next));
        self.goal_column = None;
        surface.invalidation.push(InvalidationKind::Selection, None);
        Ok(InputOutcome {
            selection_changed: true,
            ..InputOutcome::handled()
        })
    }

    fn delete_word(
        &mut self,
        surface: &mut EditorSurface,
        forward: bool,
    ) -> Result<InputOutcome, SurfaceError> {
        let text = surface.editor.document().text();
        let selections = surface.editor.selections().clone();
        let edits = selections
            .selections()
            .iter()
            .filter_map(|selection| {
                let range = selection.ordered_range();
                if range.start != range.end {
                    return Some(range);
                }
                let other = word_boundary(text, selection.head, forward);
                let range = other.min(selection.head)..other.max(selection.head);
                (range.start != range.end).then_some(range)
            })
            .map(Edit::delete)
            .collect::<Vec<_>>();
        let transaction = Transaction::from_edits(edits);
        if transaction.is_empty() {
            return Ok(InputOutcome::handled());
        }
        let applied = surface.apply_transaction(&transaction, Grouping::Separate)?;
        surface
            .editor
            .set_selections(selections.map(&applied.change_map).collapse_to_carets());
        surface.invalidation.push(InvalidationKind::Selection, None);
        Ok(InputOutcome {
            document_changed: true,
            selection_changed: true,
            ..InputOutcome::handled()
        })
    }

    /// Enter: keeps the current line's indentation, adds one level after an
    /// opening bracket, and splits `{|}` onto three lines.
    fn newline(&mut self, surface: &mut EditorSurface) -> Result<InputOutcome, SurfaceError> {
        let unit = " ".repeat(Self::tab_width(surface));
        let text = surface.editor.document().text();
        let map = surface.editor.document().position_map();
        let selections = surface.editor.selections().clone();
        let mut edits = Vec::new();
        let mut caret_offsets = Vec::new();
        for selection in selections.selections() {
            let range = selection.ordered_range();
            let line = map.byte_to_line(range.start).unwrap_or(0);
            let line_start = map.line_start(line).unwrap_or(0);
            let before_caret = &text[line_start..range.start];
            let leading: String = before_caret
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            let before = before_caret.trim_end().chars().last();
            let after = text[range.end..].chars().next();
            let opens = matches!(before, Some('{' | '(' | '['));
            let closes = matches!(
                (before, after),
                (Some('{'), Some('}')) | (Some('('), Some(')')) | (Some('['), Some(']'))
            );
            let mut first = format!("\n{leading}");
            if opens {
                first.push_str(&unit);
            }
            let caret = first.len();
            let insert = if closes {
                format!("{first}\n{leading}")
            } else {
                first
            };
            caret_offsets.push(caret);
            edits.push(Edit::replace(range, insert));
        }
        let transaction = Transaction::from_edits(edits);
        let applied = surface.apply_transaction(&transaction, Grouping::Separate)?;
        let carets = selections
            .selections()
            .iter()
            .zip(caret_offsets)
            .map(|(selection, offset)| {
                let start = applied
                    .change_map
                    .map_offset(selection.ordered_range().start, Affinity::Before);
                Selection::caret(start + offset)
            })
            .collect::<Vec<_>>();
        surface.editor.set_selections(SelectionSet::new(carets));
        surface.invalidation.push(InvalidationKind::Selection, None);
        Ok(InputOutcome {
            document_changed: true,
            selection_changed: true,
            ..InputOutcome::handled()
        })
    }

    /// Tab: indents every touched line for a multi-line selection, otherwise
    /// inserts spaces up to the next tab stop.
    fn indent(&mut self, surface: &mut EditorSurface) -> Result<InputOutcome, SurfaceError> {
        let width = Self::tab_width(surface);
        let unit = " ".repeat(width);
        let text = surface.editor.document().text();
        let map = surface.editor.document().position_map();
        let selections = surface.editor.selections().clone();
        let multiline = selections.selections().iter().any(|selection| {
            let range = selection.ordered_range();
            range.start != range.end
                && map.byte_to_line(range.start).ok() != map.byte_to_line(range.end).ok()
        });
        if !multiline {
            let edits = selections.selections().iter().map(|selection| {
                let range = selection.ordered_range();
                let line = map.byte_to_line(range.start).unwrap_or(0);
                let line_start = map.line_start(line).unwrap_or(0);
                let column = text[line_start..range.start]
                    .chars()
                    .fold(0, |column, c| {
                        column + if c == '\t' { width - column % width } else { 1 }
                    });
                Edit::replace(range, " ".repeat(width - column % width))
            });
            let transaction = Transaction::from_edits(edits);
            let applied = surface.apply_transaction(&transaction, self.grouping())?;
            let carets = selections.selections().iter().map(|selection| {
                Selection::caret(
                    applied
                        .change_map
                        .map_offset(selection.ordered_range().end, Affinity::After),
                )
            });
            surface.editor.set_selections(SelectionSet::new(carets));
        } else {
            let mut lines = std::collections::BTreeSet::new();
            for selection in selections.selections() {
                let range = selection.ordered_range();
                let first = map.byte_to_line(range.start).unwrap_or(0);
                let mut last = map.byte_to_line(range.end).unwrap_or(first);
                // A selection ending at a line's very start excludes that line.
                if last > first && map.line_start(last).ok() == Some(range.end) {
                    last -= 1;
                }
                lines.extend(first..=last);
            }
            let edits = lines
                .iter()
                .filter_map(|line| map.line_start(*line).ok())
                .map(|start| Edit::insert(start, unit.clone()))
                .collect::<Vec<_>>();
            let transaction = Transaction::from_edits(edits);
            let applied = surface.apply_transaction(&transaction, Grouping::Separate)?;
            surface
                .editor
                .set_selections(selections.map(&applied.change_map));
        }
        surface.invalidation.push(InvalidationKind::Selection, None);
        Ok(InputOutcome {
            document_changed: true,
            selection_changed: true,
            ..InputOutcome::handled()
        })
    }

    /// Shift+Tab: removes one indent level from every touched line.
    fn outdent(&mut self, surface: &mut EditorSurface) -> Result<InputOutcome, SurfaceError> {
        let width = Self::tab_width(surface);
        let text = surface.editor.document().text();
        let map = surface.editor.document().position_map();
        let selections = surface.editor.selections().clone();
        let mut lines = std::collections::BTreeSet::new();
        for selection in selections.selections() {
            let range = selection.ordered_range();
            let first = map.byte_to_line(range.start).unwrap_or(0);
            let mut last = map.byte_to_line(range.end).unwrap_or(first);
            if last > first && map.line_start(last).ok() == Some(range.end) {
                last -= 1;
            }
            lines.extend(first..=last);
        }
        let edits = lines
            .iter()
            .filter_map(|line| {
                let start = map.line_start(*line).ok()?;
                let leading = &text[start..];
                let removable = if leading.starts_with('\t') {
                    1
                } else {
                    leading.bytes().take(width).take_while(|b| *b == b' ').count()
                };
                (removable > 0).then(|| Edit::delete(start..start + removable))
            })
            .collect::<Vec<_>>();
        let transaction = Transaction::from_edits(edits);
        if transaction.is_empty() {
            return Ok(InputOutcome::handled());
        }
        let applied = surface.apply_transaction(&transaction, Grouping::Separate)?;
        surface
            .editor
            .set_selections(selections.map(&applied.change_map));
        surface.invalidation.push(InvalidationKind::Selection, None);
        Ok(InputOutcome {
            document_changed: true,
            selection_changed: true,
            ..InputOutcome::handled()
        })
    }

    fn apply_command(
        &mut self,
        surface: &mut EditorSurface,
        command: EditCommand,
    ) -> Result<InputOutcome, SurfaceError> {
        match command {
            EditCommand::InsertText(text) => self.insert_text(surface, text),
            EditCommand::Copy => Ok(InputOutcome::handled()),
            EditCommand::Cut => self.cut(surface),
            EditCommand::Paste(text) => self.insert_text(surface, text),
            EditCommand::Newline => self.newline(surface),
            EditCommand::Tab => self.indent(surface),
            EditCommand::Outdent => self.outdent(surface),
            EditCommand::SelectAll => {
                let end = surface.editor.document().text().len();
                surface
                    .editor
                    .set_selections(SelectionSet::new([Selection::range(0, end)]));
                surface.invalidation.push(InvalidationKind::Selection, None);
                self.goal_column = None;
                Ok(InputOutcome {
                    selection_changed: true,
                    ..InputOutcome::handled()
                })
            }
            EditCommand::MoveWordLeft { extend } => {
                self.move_to(surface, extend, |text, head| word_boundary(text, head, false))
            }
            EditCommand::MoveWordRight { extend } => {
                self.move_to(surface, extend, |text, head| word_boundary(text, head, true))
            }
            EditCommand::MoveDocumentStart { extend } => {
                self.move_to(surface, extend, |_, _| 0)
            }
            EditCommand::MoveDocumentEnd { extend } => {
                self.move_to(surface, extend, |text, _| text.len())
            }
            EditCommand::DeleteWordBackward => self.delete_word(surface, false),
            EditCommand::DeleteWordForward => self.delete_word(surface, true),
            EditCommand::DeleteBackward => self.delete(surface, true),
            EditCommand::DeleteForward => self.delete(surface, false),
            EditCommand::MoveLeft { extend } => self.move_horizontal(surface, false, extend),
            EditCommand::MoveRight { extend } => self.move_horizontal(surface, true, extend),
            EditCommand::MoveUp { extend } => self.move_vertical(surface, -1, extend),
            EditCommand::MoveDown { extend } => self.move_vertical(surface, 1, extend),
            EditCommand::MovePageUp { extend } => self.move_vertical(
                surface,
                -(surface.scroll.viewport_rows as isize).max(1),
                extend,
            ),
            EditCommand::MovePageDown { extend } => self.move_vertical(
                surface,
                (surface.scroll.viewport_rows as isize).max(1),
                extend,
            ),
            EditCommand::MoveHome { extend } => self.move_line_edge(surface, false, extend),
            EditCommand::MoveEnd { extend } => self.move_line_edge(surface, true, extend),
            EditCommand::ToggleFold => {
                let line = surface.editor.selections().primary().and_then(|selection| {
                    surface
                        .editor
                        .document()
                        .position_map()
                        .byte_to_line(selection.cursor())
                        .ok()
                });
                let changed = line.is_some_and(|line| surface.toggle_fold_at_line(line));
                Ok(InputOutcome {
                    handled: true,
                    selection_changed: changed,
                    ..InputOutcome::handled()
                })
            }
            EditCommand::UnfoldAll => {
                surface.unfold_all();
                Ok(InputOutcome::handled())
            }
            EditCommand::CancelComposition => {
                let mut outcome = InputOutcome::handled();
                outcome.composition_changed = surface.editor.cancel_composition();
                if outcome.composition_changed {
                    surface
                        .invalidation
                        .push(InvalidationKind::Composition, None);
                }
                Ok(outcome)
            }
        }
    }

    fn insert_text(
        &mut self,
        surface: &mut EditorSurface,
        text: String,
    ) -> Result<InputOutcome, SurfaceError> {
        if text.is_empty() {
            return Ok(InputOutcome::handled());
        }
        let selections = surface.editor.selections().clone();
        let transaction = Transaction::from_edits(
            selections
                .selections()
                .iter()
                .map(|selection| Edit::replace(selection.ordered_range(), text.clone())),
        );
        let applied = surface.apply_transaction(&transaction, self.grouping())?;
        let carets = selections.selections().iter().map(|selection| {
            let end = applied
                .change_map
                .map_offset(selection.ordered_range().end, Affinity::After);
            Selection::caret(end)
        });
        surface.editor.set_selections(SelectionSet::new(carets));
        surface.invalidation.push(InvalidationKind::Selection, None);
        Ok(InputOutcome {
            handled: true,
            document_changed: true,
            selection_changed: true,
            composition_changed: false,
            scroll_changed: false,
        })
    }

    fn delete(
        &mut self,
        surface: &mut EditorSurface,
        backward: bool,
    ) -> Result<InputOutcome, SurfaceError> {
        let text = surface.editor.document().text();
        let selections = surface.editor.selections().clone();
        let edits = selections.selections().iter().filter_map(|selection| {
            let range = selection.ordered_range();
            if range.start != range.end {
                return Some(range);
            }
            if backward {
                text[..selection.head]
                    .char_indices()
                    .next_back()
                    .map(|(offset, _)| offset..selection.head)
            } else {
                text[selection.head..]
                    .chars()
                    .next()
                    .map(|character| selection.head..selection.head + character.len_utf8())
            }
        });
        let transaction = Transaction::from_edits(edits.map(mockaco_core::Edit::delete));
        if transaction.is_empty() {
            return Ok(InputOutcome::handled());
        }
        let applied = surface.apply_transaction(&transaction, Grouping::Separate)?;
        let mapped = selections.map(&applied.change_map);
        surface.editor.set_selections(mapped.collapse_to_carets());
        surface.invalidation.push(InvalidationKind::Selection, None);
        Ok(InputOutcome {
            handled: true,
            document_changed: true,
            selection_changed: true,
            composition_changed: false,
            scroll_changed: false,
        })
    }

    fn cut(&mut self, surface: &mut EditorSurface) -> Result<InputOutcome, SurfaceError> {
        let text = surface.editor.document().text();
        let selections = surface.editor.selections().clone();
        let mut edits = Vec::new();
        for selection in selections.selections() {
            let range = selection.ordered_range();
            if range.start != range.end {
                edits.push(Edit::delete(range));
                continue;
            }
            let line = surface
                .editor
                .document()
                .position_map()
                .byte_to_line(selection.cursor())
                .unwrap_or(0);
            let start = surface
                .editor
                .document()
                .position_map()
                .line_start(line)
                .unwrap_or(selection.cursor());
            let mut end = surface
                .editor
                .document()
                .position_map()
                .line_end(line)
                .unwrap_or(selection.cursor());
            if end < text.len() {
                end += text[end..].chars().next().map_or(0, char::len_utf8);
            }
            if start != end {
                edits.push(Edit::delete(start..end));
            }
        }
        let transaction = Transaction::from_edits(edits);
        if transaction.is_empty() {
            return Ok(InputOutcome::handled());
        }
        let applied = surface.apply_transaction(&transaction, Grouping::Separate)?;
        surface
            .editor
            .set_selections(selections.map(&applied.change_map).collapse_to_carets());
        surface.invalidation.push(InvalidationKind::Selection, None);
        Ok(InputOutcome {
            handled: true,
            document_changed: true,
            selection_changed: true,
            ..InputOutcome::handled()
        })
    }

    fn move_horizontal(
        &mut self,
        surface: &mut EditorSurface,
        right: bool,
        extend: bool,
    ) -> Result<InputOutcome, SurfaceError> {
        let text = surface.editor.document().text();
        let next = surface
            .editor
            .selections()
            .selections()
            .iter()
            .map(|selection| {
                let cursor = if !extend && !selection.is_caret() {
                    if right {
                        selection.ordered_range().end
                    } else {
                        selection.ordered_range().start
                    }
                } else {
                    selection.head
                };
                let target = if right {
                    text[cursor..]
                        .chars()
                        .next()
                        .map(|character| cursor + character.len_utf8())
                        .unwrap_or(cursor)
                } else {
                    text[..cursor]
                        .char_indices()
                        .next_back()
                        .map(|(offset, _)| offset)
                        .unwrap_or(0)
                };
                if extend {
                    Selection::range(selection.anchor, target)
                } else {
                    Selection::caret(target)
                }
            });
        surface.editor.set_selections(SelectionSet::new(next));
        self.goal_column = None;
        surface.invalidation.push(InvalidationKind::Selection, None);
        Ok(InputOutcome {
            handled: true,
            document_changed: false,
            selection_changed: true,
            composition_changed: false,
            scroll_changed: false,
        })
    }

    fn move_vertical(
        &mut self,
        surface: &mut EditorSurface,
        delta: isize,
        extend: bool,
    ) -> Result<InputOutcome, SurfaceError> {
        let mut goal = self.goal_column;
        let selections = surface.editor.selections().clone();
        let mut moved = Vec::new();
        for selection in selections.selections() {
            let point = surface
                .display
                .buffer_to_display(selection.head, Affinity::After)
                .unwrap_or(DisplayPoint { row: 0, column: 0 });
            let desired = goal.get_or_insert(point.column);
            let row = if delta.is_negative() {
                point.row.saturating_sub(delta.unsigned_abs())
            } else {
                point.row.saturating_add(delta as usize)
            }
            .min(surface.display.row_count().saturating_sub(1));
            let target_column = (*desired).min(
                surface
                    .display
                    .rows()
                    .get(row)
                    .map_or(0, |display_row| display_row.display_width),
            );
            let target = surface
                .display
                .display_to_buffer(
                    DisplayPoint {
                        row,
                        column: target_column,
                    },
                    Affinity::After,
                )
                .map(|point| point.byte_offset)
                .unwrap_or(selection.head);
            moved.push(if extend {
                Selection::range(selection.anchor, target)
            } else {
                Selection::caret(target)
            });
        }
        self.goal_column = goal;
        surface.editor.set_selections(SelectionSet::new(moved));
        surface.invalidation.push(InvalidationKind::Selection, None);
        Ok(InputOutcome {
            handled: true,
            selection_changed: true,
            ..InputOutcome::handled()
        })
    }

    fn move_line_edge(
        &mut self,
        surface: &mut EditorSurface,
        end: bool,
        extend: bool,
    ) -> Result<InputOutcome, SurfaceError> {
        let position_map = surface.editor.document().position_map();
        let next = surface
            .editor
            .selections()
            .selections()
            .iter()
            .map(|selection| {
                let target = surface
                    .display
                    .buffer_to_display(selection.head, Affinity::After)
                    .ok()
                    .and_then(|point| {
                        let row = &surface.display.rows()[point.row];
                        surface
                            .display
                            .display_to_buffer(
                                DisplayPoint {
                                    row: point.row,
                                    column: if end { row.display_width } else { 0 },
                                },
                                Affinity::After,
                            )
                            .ok()
                            .map(|mapped| mapped.byte_offset)
                    })
                    .unwrap_or_else(|| {
                        let line = position_map.byte_to_line(selection.head).unwrap_or(0);
                        if end {
                            position_map.line_end(line).unwrap_or(selection.head)
                        } else {
                            position_map.line_start(line).unwrap_or(selection.head)
                        }
                    });
                if extend {
                    Selection::range(selection.anchor, target)
                } else {
                    Selection::caret(target)
                }
            });
        surface.editor.set_selections(SelectionSet::new(next));
        self.goal_column = None;
        surface.invalidation.push(InvalidationKind::Selection, None);
        Ok(InputOutcome {
            handled: true,
            document_changed: false,
            selection_changed: true,
            composition_changed: false,
            scroll_changed: false,
        })
    }

    fn route_ime(
        &mut self,
        surface: &mut EditorSurface,
        event: ImeEvent,
    ) -> Result<InputOutcome, SurfaceError> {
        match event {
            ImeEvent::Update { range, text } => {
                surface
                    .editor
                    .update_composition(range, text)
                    .map_err(|error| {
                        SurfaceError::Input(InputError::InvalidComposition(error.to_string()))
                    })?;
                surface
                    .invalidation
                    .push(InvalidationKind::Composition, None);
                Ok(InputOutcome {
                    composition_changed: true,
                    ..InputOutcome::handled()
                })
            }
            ImeEvent::Commit => {
                let changed = surface.commit_composition()?;
                Ok(InputOutcome {
                    document_changed: changed,
                    composition_changed: changed,
                    ..InputOutcome::handled()
                })
            }
            ImeEvent::Cancel => {
                let changed = surface.editor.cancel_composition();
                if changed {
                    surface
                        .invalidation
                        .push(InvalidationKind::Composition, None);
                }
                Ok(InputOutcome {
                    composition_changed: changed,
                    ..InputOutcome::handled()
                })
            }
        }
    }

    fn route_mouse(
        &mut self,
        surface: &mut EditorSurface,
        event: MouseEvent,
    ) -> Result<InputOutcome, SurfaceError> {
        match event {
            MouseEvent::Down {
                x,
                y,
                button: MouseButton::Primary,
                click_count,
            } => {
                let row = surface.scroll.top_row.saturating_add(
                    (y.max(0.0) / surface.geometry.line_height.max(1.0)).floor() as usize,
                );
                if x < surface.gutter_width() {
                    if row >= surface.display.row_count() {
                        return Ok(InputOutcome::handled());
                    }
                    let chevron_start = surface.gutter_width() - GUTTER_FOLD_WIDTH;
                    if x >= chevron_start - GUTTER_NUMBER_PADDING {
                        // Fold chevron column (and its padding): toggle.
                        if let Some(gutter) = surface
                            .display
                            .gutter_for_display_range(GUTTER_MIN_DIGITS, row..row + 1)
                            .rows
                            .first()
                        {
                            surface.toggle_fold_at_line(gutter.buffer_line);
                        }
                    } else if x >= GUTTER_MARKER_WIDTH {
                        // Line-number column: select the whole line.
                        let line = surface.display.rows()[row].buffer_line;
                        surface.select_line_at(line);
                        self.drag_anchor = None;
                        return Ok(InputOutcome {
                            selection_changed: true,
                            ..InputOutcome::handled()
                        });
                    }
                    return Ok(InputOutcome::handled());
                }
                let byte = mouse_to_byte(surface, x, y)?;
                if click_count >= 3 {
                    let line = surface
                        .editor
                        .document()
                        .position_map()
                        .byte_to_line(byte)
                        .unwrap_or(0);
                    surface.select_line_at(line);
                    self.drag_anchor = None;
                    return Ok(InputOutcome::handled());
                }
                if click_count == 2 {
                    surface.select_word_at(byte);
                    self.drag_anchor = None;
                    return Ok(InputOutcome::handled());
                }
                self.drag_anchor = Some(byte);
                surface.editor.set_selections(SelectionSet::caret(byte));
                surface.invalidation.push(InvalidationKind::Selection, None);
                Ok(InputOutcome {
                    selection_changed: true,
                    ..InputOutcome::handled()
                })
            }
            MouseEvent::Extend { x, y } => {
                let byte = mouse_to_byte(surface, x, y)?;
                let anchor = surface
                    .editor
                    .selections()
                    .primary()
                    .map_or(byte, |selection| selection.anchor);
                self.drag_anchor = Some(anchor);
                surface
                    .editor
                    .set_selections(SelectionSet::new([Selection::range(anchor, byte)]));
                surface.invalidation.push(InvalidationKind::Selection, None);
                Ok(InputOutcome {
                    selection_changed: true,
                    ..InputOutcome::handled()
                })
            }
            MouseEvent::Drag { x, y } => {
                let Some(anchor) = self.drag_anchor else {
                    return Ok(InputOutcome::handled());
                };
                let byte = mouse_to_byte(surface, x, y)?;
                surface
                    .editor
                    .set_selections(SelectionSet::new([Selection::range(anchor, byte)]));
                surface.invalidation.push(InvalidationKind::Selection, None);
                Ok(InputOutcome {
                    selection_changed: true,
                    ..InputOutcome::handled()
                })
            }
            MouseEvent::Up { .. } => {
                self.drag_anchor = None;
                Ok(InputOutcome::handled())
            }
            MouseEvent::Down { .. } => Ok(InputOutcome::handled()),
        }
    }
}

/// Maps editor-local pointer coordinates to a byte offset. The pointer may be
/// anywhere (outside the text, past a line's end, above or below the
/// document); it clamps to the nearest valid caret position, exactly like a
/// desktop editor. `x` is relative to the editor's left edge (gutter
/// included) plus any sub-column scroll remainder; `y` is relative to the top
/// of the first rendered row.
fn mouse_to_byte(surface: &EditorSurface, x: f32, y: f32) -> Result<usize, SurfaceError> {
    if !x.is_finite() || !y.is_finite() || surface.geometry.line_height <= 0.0 {
        return Err(SurfaceError::Input(InputError::InvalidMouseCoordinate));
    }
    let row_count = surface.display.row_count();
    if row_count == 0 {
        return Ok(0);
    }
    let row = surface
        .scroll
        .top_row
        .saturating_add((y.max(0.0) / surface.geometry.line_height).floor() as usize)
        .min(row_count - 1);
    let code_x = (x - surface.gutter_width() - CODE_PADDING).max(0.0);
    // Round to the nearest column boundary so clicking the right half of a
    // glyph places the caret after it.
    let column = ((code_x / surface.geometry.character_width.max(1.0)) + 0.5).floor() as usize
        + surface.scroll.horizontal_columns;
    let column = column.min(surface.display.rows()[row].display_width);
    surface
        .display
        .display_to_buffer(DisplayPoint { row, column }, Affinity::Before)
        .map(|point| point.byte_offset)
        .map_err(SurfaceError::Display)
}

fn indent_guides_for_line(text: &str, start: usize, end: usize, tab_width: usize) -> Vec<usize> {
    let Some(line) = text.get(start.min(text.len())..end.min(text.len())) else {
        return Vec::new();
    };
    let tab_width = tab_width.max(1);
    let mut column = 0;
    let mut first_code_column = None;
    for character in line.chars() {
        match character {
            ' ' => column += 1,
            '\t' => column += tab_width - (column % tab_width),
            _ => {
                first_code_column = Some(column);
                break;
            }
        }
    }
    let mut guides = Vec::new();
    let limit = first_code_column.unwrap_or(column);
    let end = if first_code_column.is_some() {
        limit
    } else {
        limit.saturating_add(1)
    };
    for guide in (tab_width..end).step_by(tab_width) {
        guides.push(guide);
    }
    guides
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockaco_renderer::{DisplayConfig, WrapConfig};

    fn surface(text: &str) -> EditorSurface {
        EditorSurface::new(text, DisplayConfig::wrapped(20), SurfaceGeometry::default())
    }

    #[test]
    fn scroll_state_clamps_in_both_directions() {
        let mut scroll = ScrollState::default();
        scroll.set_viewport(3, 4);
        scroll.set_content(10, 12);
        assert!(scroll.scroll_to(99, 99));
        assert_eq!(scroll.top_row, 7);
        assert_eq!(scroll.horizontal_columns, 8);
        assert!(scroll.scroll_by(-99, -99));
        assert_eq!(scroll.top_row, 0);
        assert_eq!(scroll.horizontal_columns, 0);
    }

    #[test]
    fn render_frame_contains_visible_rows_carets_and_gutter_data() {
        let mut surface = surface("first line\nsecond line\nthird line");
        surface.set_viewport(1, 20);
        let frame = surface.render_frame();
        assert_eq!(frame.rows.len(), 1);
        assert_eq!(frame.rows[0].buffer_line, 0);
        assert_eq!(frame.carets.len(), 1);
        assert_eq!(frame.gutter.rows[0].line_number, 1);
    }

    #[test]
    fn buffered_render_frame_adds_one_clipped_row_on_each_side() {
        let mut surface = surface("a\nb\nc\nd");
        surface.set_viewport(2, 20);
        surface.scroll_to(1, 0);
        let frame = surface.render_frame_with_buffer(1);
        assert_eq!(
            frame
                .rows
                .iter()
                .map(|row| row.display_row)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert_eq!(frame.rows[0].y, -20.0);
        assert_eq!(frame.rows[1].y, 0.0);
        assert_eq!(frame.rows[3].y, 40.0);
        assert_eq!(frame.viewport.top_row, 1);
    }

    #[test]
    fn bottom_scroll_padding_allows_the_last_line_to_clear_the_edge() {
        let mut surface = surface("a\nb\nc\nd");
        // Two-row viewport: the final line may scroll up to the top edge, so
        // (viewport - 1) presentation rows are reserved after the content.
        let mut geometry = surface.geometry();
        geometry.height = 40.0;
        surface.set_geometry(geometry);
        assert_eq!(surface.scroll().viewport_rows, 2);
        assert_eq!(
            surface.scroll().content_rows,
            surface.display().row_count() + 1
        );
        surface.scroll_to(usize::MAX, 0);
        let frame = surface.render_frame_with_buffer(1);

        assert_eq!(frame.viewport.top_row, 3);
        assert_eq!(frame.rows.last().map(|row| row.display_row), Some(3));
        assert!(frame
            .rows
            .iter()
            .all(|row| row.display_row < surface.display().row_count()));
    }

    #[test]
    fn render_frame_contains_text_and_visual_geometry() {
        let mut surface = surface("first line");
        surface.editor_mut().set_selections(SelectionSet::new([
            Selection::range(0, 5),
            Selection::caret(6),
        ]));
        surface.set_decorations(vec![Decoration::new(6..10, 7)]);
        let frame = surface.render_frame();

        assert_eq!(frame.rows[0].text, "first line");
        assert_eq!(frame.selections.len(), 1);
        assert_eq!(frame.selections[0].width, 5.0 * 8.0);
        assert_eq!(frame.carets.len(), 2);
        assert_eq!(frame.decoration_geometry.len(), 1);
        assert_eq!(frame.decoration_geometry[0].style.id, 7);
    }

    #[test]
    fn changing_theme_invalidates_the_presentation() {
        let mut surface = surface("text");
        let before = surface.invalidation().revision();
        let mut theme = surface.theme();
        theme.caret = SurfaceColor::rgba(255, 0, 0, 255);
        surface.set_theme(theme);

        assert_eq!(surface.theme().caret, SurfaceColor::rgba(255, 0, 0, 255));
        assert_eq!(surface.invalidation().revision(), before + 1);
    }

    #[test]
    fn decoration_updates_invalidate_only_projected_display_rows() {
        let mut surface = EditorSurface::new(
            "abcdef",
            DisplayConfig::wrapped(3).with_wrap(Some(WrapConfig::new(3))),
            SurfaceGeometry::default(),
        );
        surface.set_decorations(vec![Decoration::new(1..5, 9)]);
        let invalidations = surface.take_invalidations();
        assert!(matches!(
            invalidations.invalidations.last(),
            Some(Invalidation {
                kind: InvalidationKind::Decoration,
                rows: Some(rows),
            }) if rows == &(0..2)
        ));
    }

    #[test]
    fn multi_cursor_text_input_produces_one_edit_per_caret() {
        let mut surface = surface("ab cd");
        surface.editor_mut().set_selections(SelectionSet::new([
            Selection::caret(0),
            Selection::caret(3),
        ]));
        let mut router = InputRouter::default();
        let outcome = router
            .route(&mut surface, InputEvent::Text("X".to_owned()))
            .unwrap();
        assert!(outcome.document_changed);
        assert_eq!(surface.editor().document().text(), "Xab Xcd");
        assert_eq!(surface.editor().selections().len(), 2);
    }

    #[test]
    fn keyboard_translation_moves_and_extends_selection() {
        let mut surface = surface("abc");
        surface.editor_mut().set_selections(SelectionSet::caret(1));
        let mut router = InputRouter::default();
        router
            .route(
                &mut surface,
                InputEvent::Key(KeyEvent {
                    key: Key::Right,
                    modifiers: KeyModifiers::default(),
                }),
            )
            .unwrap();
        router
            .route(
                &mut surface,
                InputEvent::Key(KeyEvent {
                    key: Key::Left,
                    modifiers: KeyModifiers {
                        shift: true,
                        ..KeyModifiers::default()
                    },
                }),
            )
            .unwrap();
        assert_eq!(
            surface.editor().selections().selections()[0],
            Selection::range(2, 1)
        );
    }

    #[test]
    fn navigation_and_paste_reveal_the_primary_caret() {
        let mut surface = EditorSurface::new(
            (0..12)
                .map(|line| format!("line {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
            DisplayConfig::unwrapped(),
            SurfaceGeometry {
                height: 66.0,
                ..SurfaceGeometry::default()
            },
        );
        surface.set_viewport(3, 40);
        let mut router = InputRouter::default();
        for _ in 0..10 {
            router
                .route(
                    &mut surface,
                    InputEvent::Key(KeyEvent {
                        key: Key::Down,
                        modifiers: KeyModifiers::default(),
                    }),
                )
                .unwrap();
        }
        assert!(surface.scroll().top_row > 0);
        router
            .route(&mut surface, InputEvent::Paste("\n".repeat(12)))
            .unwrap();
        let caret_row = surface
            .display()
            .buffer_to_display(
                surface.editor().selections().primary().unwrap().cursor(),
                Affinity::After,
            )
            .unwrap()
            .row;
        assert!(caret_row >= surface.scroll().top_row);
        assert!(caret_row < surface.scroll().top_row + surface.scroll().viewport_rows);
        assert!(surface.scroll().top_row > 0);
    }

    #[test]
    fn navigation_handles_all_directions_and_preserves_vertical_goal() {
        let mut surface = EditorSurface::new(
            "abcdef\nab\nabcdef\n",
            DisplayConfig::unwrapped(),
            SurfaceGeometry::default(),
        );
        surface.editor_mut().set_selections(SelectionSet::caret(5));
        let mut router = InputRouter::default();
        for key in [
            Key::Down,
            Key::Down,
            Key::Up,
            Key::Up,
            Key::Left,
            Key::Right,
        ] {
            router
                .route(
                    &mut surface,
                    InputEvent::Key(KeyEvent {
                        key,
                        modifiers: KeyModifiers::default(),
                    }),
                )
                .unwrap();
        }
        assert_eq!(surface.editor().selections().primary().unwrap().head, 5);
        assert_eq!(
            InputRouter::translate_key(&KeyEvent {
                key: Key::PageDown,
                modifiers: KeyModifiers::default(),
            })
            .unwrap(),
            Some(EditCommand::MovePageDown { extend: false })
        );
    }

    #[test]
    fn vertical_navigation_clamps_to_the_last_slot_on_shorter_lines() {
        let mut surface = EditorSurface::new(
            "abcdef\nx\n",
            DisplayConfig::unwrapped(),
            SurfaceGeometry::default(),
        );
        surface.editor_mut().set_selections(SelectionSet::caret(5));
        InputRouter::default()
            .route(
                &mut surface,
                InputEvent::Key(KeyEvent {
                    key: Key::Down,
                    modifiers: KeyModifiers::default(),
                }),
            )
            .unwrap();
        assert_eq!(surface.editor().selections().primary().unwrap().head, 8);
    }

    #[test]
    fn render_frame_exposes_four_column_indent_guides_for_spaces_and_tabs() {
        let surface = surface("fn main() {\n\t    let value = 1;\n}\n");
        let frame = surface.render_frame();
        assert_eq!(frame.rows[1].indent_guides, vec![4]);
    }

    #[test]
    fn clipboard_copy_paste_and_cut_preserve_unicode_and_selections() {
        let mut surface = surface("alpha\nβeta\n");
        surface
            .editor_mut()
            .set_selections(SelectionSet::new([Selection::range(0, 5)]));
        assert_eq!(surface.clipboard_text(), "alpha");

        let mut router = InputRouter::default();
        router
            .route(&mut surface, InputEvent::Paste("Ω".to_owned()))
            .unwrap();
        assert_eq!(surface.editor().document().text(), "Ω\nβeta\n");

        surface
            .editor_mut()
            .set_selections(SelectionSet::new([Selection::range(0, 2)]));
        router
            .route(
                &mut surface,
                InputEvent::Key(KeyEvent {
                    key: Key::Character("x".to_owned()),
                    modifiers: KeyModifiers {
                        control: true,
                        ..KeyModifiers::default()
                    },
                }),
            )
            .unwrap();
        assert_eq!(surface.editor().document().text(), "\nβeta\n");
    }

    #[test]
    fn fold_markers_toggle_from_the_dedicated_gutter_hit_area() {
        let mut surface = EditorSurface::new(
            "fn main() {\n    let value = 1;\n}\n",
            DisplayConfig::unwrapped(),
            SurfaceGeometry::default(),
        );
        assert!(surface.display().gutter(GUTTER_MIN_DIGITS).rows[0].foldable);
        let chevron_x = surface.gutter_width() - 4.0;
        let mut router = InputRouter::default();
        router
            .route(
                &mut surface,
                InputEvent::Mouse(MouseEvent::Down {
                    // Inside the fold-chevron column at the gutter's edge.
                    x: chevron_x,
                    y: 0.0,
                    button: MouseButton::Primary,
                    click_count: 1,
                }),
            )
            .unwrap();
        assert_eq!(surface.display().row_count(), 2);
        assert!(surface.display().gutter(GUTTER_MIN_DIGITS).rows[0].folded);
        router
            .route(
                &mut surface,
                InputEvent::Key(KeyEvent {
                    key: Key::Character("]".to_owned()),
                    modifiers: KeyModifiers {
                        control: true,
                        ..KeyModifiers::default()
                    },
                }),
            )
            .unwrap();
        assert!(!surface.display().gutter(GUTTER_MIN_DIGITS).rows.is_empty());
    }

    #[test]
    fn renderer_frame_contains_syntax_tokens_active_line_and_contrasting_selection() {
        let mut surface = surface("fn main() {\n    let value = 7;\n}\n");
        surface.editor_mut().set_selections(SelectionSet::new([
            Selection::range(0, 2),
            Selection::caret(3),
        ]));
        let frame = surface.render_frame();
        assert!(frame.rows.iter().any(|row| !row.tokens.is_empty()));
        assert!(frame.rows.iter().any(|row| row.active));
        assert!(frame.theme.primary_selection != frame.theme.background);
        assert!(frame.gutter.rows.iter().any(|row| row.foldable));
    }

    #[test]
    fn large_documents_keep_paint_data_and_highlight_cache_viewport_bounded() {
        let text = (0..40_000)
            .map(|line| format!("fn item_{line}() {{ let value = {line}; }}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut surface =
            EditorSurface::new(text, DisplayConfig::unwrapped(), SurfaceGeometry::default());
        surface.set_viewport(20, 120);
        surface.scroll_to(39_990, 0);

        let frame = surface.render_frame_with_buffer(1);
        assert!(frame.rows.len() <= 22);
        assert_eq!(frame.rows.len(), frame.gutter.rows.len());
        assert!(frame.rows.iter().any(|row| !row.tokens.is_empty()));
        // Only the padded viewport window is highlighted, never the whole
        // 40k-line document (which would hold hundreds of thousands).
        assert!(surface.highlights.tokens().len() < 3_000);
        assert!(frame.decorations.iter().all(|decoration| frame
            .rows
            .iter()
            .any(|row| row.display_row == decoration.display_row)));
    }

    #[test]
    fn ime_update_commit_and_cancel_route_without_stale_text() {
        let mut surface = surface("abc");
        surface.editor_mut().set_selections(SelectionSet::caret(3));
        let mut router = InputRouter::default();
        router
            .route(
                &mut surface,
                InputEvent::Ime(ImeEvent::Update {
                    range: 3..3,
                    text: "漢".to_owned(),
                }),
            )
            .unwrap();
        assert_eq!(surface.editor().document().text(), "abc");
        router
            .route(&mut surface, InputEvent::Ime(ImeEvent::Commit))
            .unwrap();
        assert_eq!(surface.editor().document().text(), "abc漢");
        router
            .route(
                &mut surface,
                InputEvent::Ime(ImeEvent::Update {
                    range: 6..6,
                    text: "!".to_owned(),
                }),
            )
            .unwrap();
        router
            .route(&mut surface, InputEvent::Ime(ImeEvent::Cancel))
            .unwrap();
        assert_eq!(surface.editor().document().text(), "abc漢");
    }

    #[test]
    fn stale_external_update_is_rejected() {
        let mut surface = surface("abc");
        let mut document = mockaco_core::Document::new("abc");
        let applied = document.apply(&Transaction::new().insert(1, "x")).unwrap();
        document.apply(&Transaction::new().insert(2, "y")).unwrap();
        let error = surface
            .accept_external_display_update(&document.snapshot(), &applied)
            .unwrap_err();
        assert!(matches!(error, SurfaceError::StaleDocumentVersion { .. }));
    }

    #[test]
    fn mouse_drag_translates_display_coordinates_to_selection() {
        let mut surface = EditorSurface::new(
            "abcdef",
            DisplayConfig::wrapped(3).with_wrap(Some(WrapConfig::new(3))),
            SurfaceGeometry::default(),
        );
        let mut router = InputRouter::default();
        // One full column into the text area (rounds to column 1).
        let x = surface.gutter_width() + CODE_PADDING + surface.geometry().character_width;
        router
            .route(
                &mut surface,
                InputEvent::Mouse(MouseEvent::Down {
                    x,
                    y: 0.0,
                    button: MouseButton::Primary,
                    click_count: 1,
                }),
            )
            .unwrap();
        router
            .route(
                &mut surface,
                InputEvent::Mouse(MouseEvent::Drag { x, y: 20.0 }),
            )
            .unwrap();
        assert_eq!(
            surface.editor().selections().selections()[0],
            Selection::range(1, 4)
        );
    }

    #[test]
    fn diff_split_surface_presents_both_sides_and_synchronizes_scroll() {
        let original = mockaco_core::Document::new("same\nold").snapshot();
        let mut surface = DiffSplitSurface::new(&original, "same\nnew");
        surface.set_viewport(1);
        let _frame = surface.render_frame();
        surface.scroll_to(DiffSide::Original, 1);
        let frame = surface.render_frame();
        assert_eq!(frame.original_rows.len(), 1);
        assert_eq!(frame.modified_rows.len(), 1);
        assert_eq!(frame.original_rows[0].text.as_deref(), Some("old"));
        assert_eq!(frame.modified_rows[0].text.as_deref(), Some("new"));
        assert_eq!(frame.modified_rows[0].kind, DiffRowKind::Replace);
        assert_eq!(surface.scroll().top_row(DiffSide::Modified), 1);
    }

    #[test]
    fn diff_split_surface_edits_only_modified_and_rejects_stale_results() {
        let original = mockaco_core::Document::new("same").snapshot();
        let mut surface = DiffSplitSurface::new(&original, "same");
        let stale = surface.diff().clone();
        surface.apply_modified_edit(0..4, "changed").unwrap();
        assert_eq!(surface.editor().original().text(), "same");
        assert_eq!(surface.editor().modified().document().text(), "changed");
        assert!(matches!(
            surface.accept_diff(stale),
            Err(SurfaceError::Diff(DiffError::StaleResult { .. }))
        ));
        assert_eq!(surface.take_invalidations().invalidations.len(), 1);
    }
}
