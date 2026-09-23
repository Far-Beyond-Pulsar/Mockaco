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
    FoldingProvider, FontStyle, IncrementalHighlights, Rgba, RustTreeSitterProvider,
    SyntaxHighlightProvider, Theme, TokenKind, TokenStyle,
};
use mockaco_renderer::{
    diagnostic_decorations, search_decorations, Decoration, DisplayMap, DisplayMapError,
    DisplayPoint, DisplayViewport, GutterLayout, ProjectedDecoration,
};
use std::fmt;
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceGeometry {
    pub width: f32,
    pub height: f32,
    pub line_height: f32,
    pub character_width: f32,
    pub gutter_character_width: f32,
}

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
    ToggleFold,
    UnfoldAll,
    Newline,
    Tab,
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
    highlighter: RustTreeSitterProvider,
    highlights: IncrementalHighlights,
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
        let editor = EditorState::new(text);
        let snapshot = editor.snapshot();
        let highlighter = RustTreeSitterProvider::new().expect("Rust Tree-sitter provider loads");
        let highlights = highlighter
            .highlight(&snapshot, 0..snapshot.len_bytes())
            .map(IncrementalHighlights::from_result)
            .unwrap_or_default();
        let mut display = DisplayMap::new(&snapshot, display_config);
        if let Ok(folds) = highlighter.fold_ranges(&snapshot, 0..snapshot.len_bytes()) {
            display.set_foldable_regions(folds.into_iter().map(|fold| {
                mockaco_renderer::FoldRegion::new(fold.start_line, fold.end_line)
                    .placeholder(fold.placeholder)
            }));
        }
        let mut surface = Self {
            editor,
            display,
            highlighter,
            highlights,
            scroll: ScrollState::default(),
            geometry,
            theme: SurfaceTheme::default(),
            decorations: Vec::new(),
            invalidation: InvalidationState::default(),
        };
        surface.recompute_scroll_viewport();
        surface
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
        self.invalidation.push(InvalidationKind::Geometry, None);
    }

    pub fn set_decorations(&mut self, decorations: Vec<Decoration>) {
        let old_rows = self.decoration_rows(&self.decorations);
        let new_rows = self.decoration_rows(&decorations);
        self.decorations = decorations;
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

    pub fn gutter_width(&self) -> f32 {
        (self.display.gutter(4).line_number_width as f32 + 3.0)
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
        self.invalidation.push(InvalidationKind::Viewport, None);
    }

    pub fn scroll_by(&mut self, vertical: isize, horizontal: isize) -> bool {
        let changed = self.scroll.scroll_by(vertical, horizontal);
        if changed {
            self.invalidation.push(InvalidationKind::Scroll, None);
        }
        changed
    }

    pub fn scroll_to(&mut self, row: usize, column: usize) -> bool {
        let changed = self.scroll.scroll_to(row, column);
        if changed {
            self.invalidation.push(InvalidationKind::Scroll, None);
        }
        changed
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
        if let Ok(result) = self.highlighter.apply_transaction(
            &before_snapshot,
            &snapshot,
            &applied,
            0..snapshot.len_bytes(),
        ) {
            let _ = self.highlights.accept(result);
        } else if let Ok(result) = self
            .highlighter
            .highlight(&snapshot, 0..snapshot.len_bytes())
        {
            let _ = self.highlights.accept(result);
        }
        self.refresh_language_state(&snapshot);
        self.recompute_scroll_viewport();
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
        let viewport_range = self.scroll.visible_rows();
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
        let rows = render_range
            .clone()
            .map(|display_row| {
                let row = &self.display.rows()[display_row];
                PaintRow {
                    display_row,
                    buffer_line: row.buffer_line,
                    source_range: row.start_byte..row.end_byte,
                    text: self.display.snapshot().text()[row.start_byte..row.end_byte].to_owned(),
                    y: (display_row as isize - self.scroll.top_row as isize) as f32
                        * self.geometry.line_height,
                    height: self.geometry.line_height,
                    continuation: row.continuation,
                    folded: row.folded,
                    truncated: row.truncated,
                    active: self.editor.selections().primary().and_then(|selection| {
                        self.display
                            .snapshot()
                            .position_map()
                            .byte_to_line(selection.cursor())
                            .ok()
                    }) == Some(row.buffer_line),
                    indent_guides: indent_guides_for_line(
                        self.display.snapshot().text(),
                        self.display
                            .snapshot()
                            .position_map()
                            .line_start(row.buffer_line)
                            .unwrap_or(row.start_byte),
                        self.display
                            .snapshot()
                            .position_map()
                            .line_end(row.buffer_line)
                            .unwrap_or(row.end_byte),
                        self.display.config().tab_width,
                    ),
                    tokens: self
                        .highlights
                        .tokens()
                        .iter()
                        .filter(|token| {
                            token.range.start < row.end_byte && token.range.end > row.start_byte
                        })
                        .map(|token| PaintToken {
                            range: token.range.clone(),
                            kind: token.kind.clone(),
                            style: self.theme.syntax.style_for(&token.kind),
                        })
                        .collect(),
                }
            })
            .collect::<Vec<_>>();
        let gutter = self.display.gutter(4);
        let selections = self.selection_geometry(&viewport_range);
        let carets = self.caret_geometry(&viewport_range);
        let decorations: Vec<ProjectedDecoration> = self
            .display
            .project_decorations(&self.decorations)
            .into_iter()
            .filter(|decoration| viewport_range.contains(&decoration.display_row))
            .collect();
        let decoration_geometry = decorations
            .iter()
            .map(|decoration| DecorationGeometry {
                display_row: decoration.display_row,
                x: decoration.start_column as f32 * self.geometry.character_width,
                width: decoration
                    .end_column
                    .saturating_sub(decoration.start_column) as f32
                    * self.geometry.character_width,
                y: (decoration.display_row - self.scroll.top_row) as f32
                    * self.geometry.line_height,
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
                    .project_decorations(&[Decoration::new(
                        selection.ordered_range(),
                        index as u32,
                    )])
                    .into_iter()
                    .filter(|projection| visible.contains(&projection.display_row))
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
                        y: (projection.display_row - self.scroll.top_row) as f32
                            * self.geometry.line_height,
                        height: self.geometry.line_height,
                        primary: index + 1 == selections.len(),
                        source_range: projection.source_range,
                    })
            })
            .collect()
    }

    fn decoration_rows(&self, decorations: &[Decoration]) -> Option<Range<usize>> {
        let projected = self.display.project_decorations(decorations);
        let start = projected
            .iter()
            .map(|decoration| decoration.display_row)
            .min()?;
        let end = projected
            .iter()
            .map(|decoration| decoration.display_row)
            .max()?
            .saturating_add(1);
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
                    y: (point.row - self.scroll.top_row) as f32 * self.geometry.line_height,
                    height: self.geometry.line_height,
                    primary: index + 1 == selections.len(),
                })
            })
            .collect()
    }

    fn recompute_scroll_viewport(&mut self) {
        let rows = (self.geometry.height / self.geometry.line_height.max(1.0)).floor() as usize;
        let columns =
            (self.geometry.width / self.geometry.character_width.max(1.0)).floor() as usize;
        let max_width = self
            .display
            .rows()
            .iter()
            .map(|row| row.display_width)
            .max()
            .unwrap_or(0);
        self.scroll.set_viewport(rows, columns);
        self.scroll
            .set_content_with_bottom_padding(self.display.row_count(), max_width, 1);
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
        if let Ok(result) = self
            .highlighter
            .highlight(snapshot, 0..snapshot.len_bytes())
        {
            let _ = self.highlights.accept(result);
        }
        if let Ok(folds) = self
            .highlighter
            .fold_ranges(snapshot, 0..snapshot.len_bytes())
        {
            self.display
                .set_foldable_regions(folds.into_iter().map(|fold| {
                    mockaco_renderer::FoldRegion::new(fold.start_line, fold.end_line)
                        .placeholder(fold.placeholder)
                }));
        }
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

#[derive(Debug, Clone, Default)]
pub struct InputRouter {
    drag_anchor: Option<usize>,
    goal_column: Option<usize>,
}

impl InputRouter {
    pub fn translate_key(event: &KeyEvent) -> Result<Option<EditCommand>, InputError> {
        let extend = event.modifiers.shift;
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
            Key::Character(text) if event.modifiers.control && text == "[" => {
                Some(EditCommand::ToggleFold)
            }
            Key::Character(text) if event.modifiers.control && text == "]" => {
                Some(EditCommand::UnfoldAll)
            }
            Key::Character(text) if !event.modifiers.control && !event.modifiers.command => {
                Some(EditCommand::InsertText(text.clone()))
            }
            Key::Backspace => Some(EditCommand::DeleteBackward),
            Key::Delete => Some(EditCommand::DeleteForward),
            Key::Enter => Some(EditCommand::Newline),
            Key::Tab => Some(EditCommand::Tab),
            Key::Escape => Some(EditCommand::CancelComposition),
            Key::Left => Some(EditCommand::MoveLeft { extend }),
            Key::Right => Some(EditCommand::MoveRight { extend }),
            Key::Up => Some(EditCommand::MoveUp { extend }),
            Key::Down => Some(EditCommand::MoveDown { extend }),
            Key::PageUp => Some(EditCommand::MovePageUp { extend }),
            Key::PageDown => Some(EditCommand::MovePageDown { extend }),
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
        match event {
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
        }
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
            EditCommand::Newline => self.insert_text(surface, "\n".to_owned()),
            EditCommand::Tab => self.insert_text(surface, "\t".to_owned()),
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
        let applied = surface.apply_transaction(&transaction, Grouping::Separate)?;
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
                let row = surface
                    .scroll
                    .top_row
                    .saturating_add((y / surface.geometry.line_height).floor() as usize);
                if x < surface.gutter_width() {
                    if let Some(gutter) = surface
                        .display
                        .gutter(4)
                        .rows
                        .iter()
                        .find(|gutter| gutter.display_row == row)
                    {
                        surface.toggle_fold_at_line(gutter.buffer_line);
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

fn mouse_to_byte(surface: &EditorSurface, x: f32, y: f32) -> Result<usize, SurfaceError> {
    if x.is_sign_negative() || y.is_sign_negative() || surface.geometry.line_height <= 0.0 {
        return Err(SurfaceError::Input(InputError::InvalidMouseCoordinate));
    }
    let row = surface
        .scroll
        .top_row
        .saturating_add((y / surface.geometry.line_height).floor() as usize);
    let code_x = (x - surface.gutter_width()).max(0.0);
    let column = (code_x / surface.geometry.character_width.max(1.0)).floor() as usize;
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

#[cfg(feature = "native-wgpui")]
pub mod native {
    use super::{
        EditorSurface, InputEvent, InputRouter, Key, KeyEvent, KeyModifiers, MouseButton,
        MouseEvent, RenderFrame, SurfaceColor,
    };
    use mockaco_renderer::{
        DIAGNOSTIC_ERROR_STYLE_ID, DIAGNOSTIC_HINT_STYLE_ID, DIAGNOSTIC_INFO_STYLE_ID,
        DIAGNOSTIC_WARNING_STYLE_ID, SEARCH_CURRENT_MATCH_STYLE_ID, SEARCH_MATCH_STYLE_ID,
    };
    use wgpui::{
        div, font, px, ClipboardItem, Context, FocusHandle, FontFallbacks, HighlightStyle,
        InteractiveElement, IntoElement, ParentElement, Render, ScrollDelta, ScrollWheelEvent,
        StatefulInteractiveElement, Styled, StyledText, Window,
    };

    /// Native WGPUI editor view backed by the framework-independent surface.
    pub struct WgpuiEditorView {
        pub surface: EditorSurface,
        pub input_router: InputRouter,
        pub read_only: bool,
        focus_handle: Option<FocusHandle>,
        dragging: bool,
        scroll_remainder: f32,
    }

    impl WgpuiEditorView {
        pub fn new(surface: EditorSurface) -> Self {
            Self {
                surface,
                input_router: InputRouter::default(),
                read_only: false,
                focus_handle: None,
                dragging: false,
                scroll_remainder: 0.0,
            }
        }

        pub fn new_read_only(surface: EditorSurface) -> Self {
            Self {
                surface,
                input_router: InputRouter::default(),
                read_only: true,
                focus_handle: None,
                dragging: false,
                scroll_remainder: 0.0,
            }
        }

        pub fn render_frame(&self) -> RenderFrame {
            self.surface.render_frame()
        }

        fn route(&mut self, event: InputEvent, cx: &mut Context<Self>) {
            if self.read_only
                && matches!(
                    &event,
                    InputEvent::Text(_)
                        | InputEvent::Paste(_)
                        | InputEvent::Key(_)
                        | InputEvent::Ime(_)
                )
            {
                return;
            }
            if self.input_router.route(&mut self.surface, event).is_ok() {
                cx.notify();
            }
        }

        fn on_key_down(
            &mut self,
            event: &wgpui::KeyDownEvent,
            _window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            let keystroke = &event.keystroke;
            let modifiers = KeyModifiers {
                shift: keystroke.modifiers.shift,
                control: keystroke.modifiers.control,
                alt: keystroke.modifiers.alt,
                command: keystroke.modifiers.platform,
            };
            if modifiers.control || modifiers.command {
                if let Some(key_char) = keystroke.key_char.as_deref() {
                    if key_char.eq_ignore_ascii_case("c") {
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            self.surface.clipboard_text(),
                        ));
                        cx.notify();
                        return;
                    }
                    if key_char.eq_ignore_ascii_case("x") {
                        if !self.read_only {
                            cx.write_to_clipboard(ClipboardItem::new_string(
                                self.surface.clipboard_text(),
                            ));
                            self.route(
                                InputEvent::Key(KeyEvent {
                                    key: Key::Character("x".to_owned()),
                                    modifiers,
                                }),
                                cx,
                            );
                        }
                        return;
                    }
                    if key_char.eq_ignore_ascii_case("v") {
                        if !self.read_only {
                            if let Some(text) =
                                cx.read_from_clipboard().and_then(|item| item.text())
                            {
                                self.route(InputEvent::Paste(text), cx);
                            }
                        }
                        return;
                    }
                }
            }
            let key = match keystroke.key.as_str() {
                "backspace" => Key::Backspace,
                "delete" => Key::Delete,
                "enter" => Key::Enter,
                "tab" => Key::Tab,
                "escape" => Key::Escape,
                "left" => Key::Left,
                "right" => Key::Right,
                "up" => Key::Up,
                "down" => Key::Down,
                "pageup" => Key::PageUp,
                "pagedown" => Key::PageDown,
                "home" => Key::Home,
                "end" => Key::End,
                _ => keystroke
                    .key_char
                    .clone()
                    .map(Key::Character)
                    .unwrap_or_else(|| Key::Unsupported(keystroke.key.clone())),
            };
            self.route(InputEvent::Key(KeyEvent { key, modifiers }), cx);
        }

        fn on_mouse_down(
            &mut self,
            event: &wgpui::MouseDownEvent,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            if event.button == wgpui::MouseButton::Left {
                self.dragging = true;
                if let Some(handle) = &self.focus_handle {
                    window.focus(handle, cx);
                }
                self.route(
                    InputEvent::Mouse(MouseEvent::Down {
                        x: event.position.x.as_f32(),
                        y: event.position.y.as_f32() + self.scroll_remainder,
                        button: MouseButton::Primary,
                        click_count: event.click_count,
                    }),
                    cx,
                );
            }
        }

        fn on_mouse_move(
            &mut self,
            event: &wgpui::MouseMoveEvent,
            _window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            if self.dragging || event.dragging() {
                self.route(
                    InputEvent::Mouse(MouseEvent::Drag {
                        x: event.position.x.as_f32(),
                        y: event.position.y.as_f32() + self.scroll_remainder,
                    }),
                    cx,
                );
            }
        }

        fn on_mouse_up(
            &mut self,
            event: &wgpui::MouseUpEvent,
            _window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            if event.button == wgpui::MouseButton::Left {
                self.dragging = false;
                self.route(
                    InputEvent::Mouse(MouseEvent::Up {
                        x: event.position.x.as_f32(),
                        y: event.position.y.as_f32() + self.scroll_remainder,
                        button: MouseButton::Primary,
                    }),
                    cx,
                );
            }
        }

        fn on_scroll(
            &mut self,
            event: &ScrollWheelEvent,
            _window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            let geometry = self.surface.geometry();
            let (vertical, horizontal) = match event.delta {
                ScrollDelta::Lines(point) => {
                    // Ordinary wheel devices report line notches. Keep those
                    // notches fractional so the translated row layer can move
                    // between line boundaries instead of snapping immediately.
                    let (vertical, remainder) = fractional_line_scroll(
                        self.scroll_remainder,
                        -point.y,
                        geometry.line_height,
                    );
                    self.scroll_remainder = remainder;
                    (vertical, point.x.round() as isize)
                }
                ScrollDelta::Pixels(point) => {
                    let total = self.scroll_remainder - point.y.as_f32();
                    let vertical = (total / geometry.line_height.max(1.0)).trunc() as isize;
                    self.scroll_remainder = total - vertical as f32 * geometry.line_height.max(1.0);
                    (
                        -vertical,
                        (point.x.as_f32() / geometry.character_width.max(1.0)).round() as isize,
                    )
                }
            };
            let scroll = self.surface.scroll();
            let max_row = scroll.content_rows.saturating_sub(scroll.viewport_rows);
            if (vertical < 0 && scroll.top_row == 0)
                || (vertical > 0 && scroll.top_row >= max_row)
                || (vertical == 0
                    && ((self.scroll_remainder < 0.0 && scroll.top_row == 0)
                        || (self.scroll_remainder > 0.0 && scroll.top_row >= max_row)))
            {
                self.scroll_remainder = 0.0;
            }
            self.route(
                InputEvent::Scroll {
                    vertical,
                    horizontal,
                },
                cx,
            );
        }
    }

    fn fractional_line_scroll(remainder: f32, lines: f32, line_height: f32) -> (isize, f32) {
        let line_height = line_height.max(1.0);
        let total = remainder + lines * line_height * 0.45;
        let vertical = (total / line_height).trunc() as isize;
        (vertical, total - vertical as f32 * line_height)
    }

    #[derive(Debug, Clone, Copy, PartialEq)]
    struct ScrollbarGeometry {
        thumb_top: f32,
        thumb_height: f32,
    }

    fn scrollbar_geometry(
        scroll: super::ScrollState,
        track_height: f32,
    ) -> Option<ScrollbarGeometry> {
        if scroll.content_rows <= scroll.viewport_rows || track_height <= 0.0 {
            return None;
        }
        let track_height = track_height.max(1.0);
        let ratio = scroll.viewport_rows as f32 / scroll.content_rows as f32;
        let thumb_height = (track_height * ratio).clamp(28.0, track_height);
        let travel = (track_height - thumb_height).max(0.0);
        let max_top = scroll.content_rows.saturating_sub(scroll.viewport_rows);
        let thumb_top = if max_top == 0 {
            0.0
        } else {
            travel * scroll.top_row.min(max_top) as f32 / max_top as f32
        };
        Some(ScrollbarGeometry {
            thumb_top,
            thumb_height,
        })
    }

    impl Render for WgpuiEditorView {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            if self.focus_handle.is_none() {
                self.focus_handle = Some(cx.focus_handle());
            }
            let editor_font = editor_font();
            let mut geometry = self.surface.geometry();
            let font_id = window.text_system().resolve_font(&editor_font);
            if let Ok(width) = window.text_system().ch_advance(font_id, px(14.0)) {
                let measured_width = width.as_f32();
                if measured_width.is_finite()
                    && measured_width > 0.0
                    && (measured_width - geometry.character_width).abs() > 0.05
                {
                    geometry.character_width = measured_width;
                    geometry.gutter_character_width = measured_width;
                    self.surface.set_geometry(geometry);
                }
            }
            let viewport_rows = (geometry.height / geometry.line_height.max(1.0)).floor() as usize;
            if self.surface.scroll().viewport_rows <= viewport_rows {
                self.surface.set_viewport(
                    viewport_rows.saturating_add(1),
                    self.surface.scroll().viewport_columns,
                );
            }
            let frame = self.surface.render_frame_with_buffer(1);
            geometry = self.surface.geometry();
            let gutter_width = self.surface.gutter_width();
            let code_padding = 12.0;
            let theme = frame.theme;
            let root = div()
                .flex()
                .flex_col()
                .size_full()
                .bg(color(theme.background))
                .text_color(color(theme.foreground))
                .font(editor_font.clone())
                .text_size(px(14.0))
                .line_height(px(geometry.line_height))
                .track_focus(self.focus_handle.as_ref().unwrap())
                .on_key_down(cx.listener(Self::on_key_down))
                .on_mouse_down(wgpui::MouseButton::Left, cx.listener(Self::on_mouse_down))
                .on_mouse_move(cx.listener(Self::on_mouse_move))
                .on_mouse_up(wgpui::MouseButton::Left, cx.listener(Self::on_mouse_up))
                .on_mouse_up_out(wgpui::MouseButton::Left, cx.listener(Self::on_mouse_up))
                .on_scroll_wheel(cx.listener(Self::on_scroll));
            let mut row_stack = div()
                .flex()
                .flex_col()
                .relative()
                .top(px(-self.scroll_remainder));
            for row in frame.rows {
                let gutter = frame
                    .gutter
                    .rows
                    .iter()
                    .find(|gutter| gutter.display_row == row.display_row);
                let gutter_marker = frame
                    .decoration_geometry
                    .iter()
                    .find(|decoration| decoration.display_row == row.display_row)
                    .map(|decoration| {
                        let color = match decoration.style.id {
                            DIAGNOSTIC_ERROR_STYLE_ID => SurfaceColor::rgba(248, 113, 113, 255),
                            DIAGNOSTIC_WARNING_STYLE_ID => SurfaceColor::rgba(251, 191, 36, 255),
                            DIAGNOSTIC_INFO_STYLE_ID | DIAGNOSTIC_HINT_STYLE_ID => {
                                SurfaceColor::rgba(96, 165, 250, 255)
                            }
                            SEARCH_CURRENT_MATCH_STYLE_ID | SEARCH_MATCH_STYLE_ID => {
                                SurfaceColor::rgba(192, 132, 252, 255)
                            }
                            _ => SurfaceColor::rgba(125, 151, 184, 255),
                        };
                        ("●", color)
                    });
                let mut code = div()
                    .relative()
                    .flex_1()
                    .h(px(row.height))
                    .pl(px(code_padding))
                    .pr(px(code_padding))
                    .font(editor_font.clone())
                    .bg(color(if row.active {
                        theme.active_line
                    } else {
                        theme.background
                    }))
                    .whitespace_nowrap();
                for guide in &row.indent_guides {
                    code = code.child(
                        div()
                            .absolute()
                            .left(px(
                                code_padding + *guide as f32 * geometry.character_width - 0.5
                            ))
                            .top(px(0.0))
                            .w(px(1.0))
                            .h(px(row.height))
                            .bg(color(SurfaceColor::rgba(57, 72, 96, 150))),
                    );
                }
                for selection in frame
                    .selections
                    .iter()
                    .filter(|selection| selection.display_row == row.display_row)
                {
                    code = code.child(
                        div()
                            .absolute()
                            .left(px(selection.x + code_padding))
                            .top(px(selection.y - row.y))
                            .w(px(selection.width.max(1.0)))
                            .h(px(selection.height))
                            .bg(color(if selection.primary {
                                theme.primary_selection
                            } else {
                                theme.selection
                            })),
                    );
                }
                for decoration in frame
                    .decoration_geometry
                    .iter()
                    .filter(|decoration| decoration.display_row == row.display_row)
                {
                    code = code.child(
                        div()
                            .absolute()
                            .left(px(decoration.x + code_padding))
                            .top(px(decoration.y - row.y))
                            .w(px(decoration.width.max(1.0)))
                            .h(px(decoration.height))
                            .border_b_1()
                            .border_color(color(decoration_color(&theme, decoration.style.id))),
                    );
                }
                let highlights = row.tokens.iter().filter_map(|token| {
                    let start = token.range.start.max(row.source_range.start);
                    let end = token.range.end.min(row.source_range.end);
                    token.style.map(|style| {
                        (
                            start - row.source_range.start..end - row.source_range.start,
                            HighlightStyle {
                                color: Some(color(SurfaceColor::rgba(
                                    style.foreground.red,
                                    style.foreground.green,
                                    style.foreground.blue,
                                    style.foreground.alpha,
                                ))),
                                ..HighlightStyle::default()
                            },
                        )
                    })
                });
                code = code.child(StyledText::new(row.text.clone()).with_highlights(highlights));
                for caret in frame
                    .carets
                    .iter()
                    .filter(|caret| caret.display_row == row.display_row)
                {
                    code = code.child(
                        div()
                            .absolute()
                            .left(px(caret.x + code_padding))
                            .top(px(caret.y - row.y))
                            .w(px(if caret.primary { 2.0 } else { 1.0 }))
                            .h(px(caret.height))
                            .bg(color(theme.caret)),
                    );
                }
                row_stack = row_stack.child(
                    div()
                        .flex()
                        .flex_row()
                        .h(px(row.height))
                        .child(
                            div()
                                .h(px(row.height))
                                .w(px(gutter_width))
                                .bg(color(theme.background))
                                .font(editor_font.clone())
                                .text_size(px(13.0))
                                .border_r_1()
                                .border_color(color(theme.background))
                                .hover(|style| {
                                    style.border_color(color(SurfaceColor::rgba(77, 104, 137, 255)))
                                })
                                .text_color(color(if row.active {
                                    theme.foreground
                                } else {
                                    theme.gutter_foreground
                                }))
                                .whitespace_nowrap()
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .justify_end()
                                        .child(
                                            div()
                                                .w(px(10.0))
                                                .text_center()
                                                .text_color(color(
                                                    gutter_marker
                                                        .map(|(_, color)| color)
                                                        .unwrap_or(SurfaceColor::rgba(0, 0, 0, 0)),
                                                ))
                                                .child(
                                                    gutter_marker
                                                        .map(|(marker, _)| marker)
                                                        .unwrap_or(" "),
                                                ),
                                        )
                                        .child({
                                            let fold_line = gutter
                                                .filter(|gutter| gutter.foldable)
                                                .map(|gutter| gutter.buffer_line);
                                            div()
                                                .w(px(14.0))
                                                .text_center()
                                                .text_color(color(SurfaceColor::rgba(
                                                    125, 151, 184, 255,
                                                )))
                                                .child(
                                                    if gutter
                                                        .map(|gutter| gutter.foldable)
                                                        .unwrap_or(false)
                                                    {
                                                        if gutter
                                                            .map(|gutter| gutter.folded)
                                                            .unwrap_or(false)
                                                        {
                                                            "›"
                                                        } else {
                                                            "⌄"
                                                        }
                                                    } else {
                                                        " "
                                                    },
                                                )
                                                .id(format!(
                                                    "fold-toggle-{}",
                                                    fold_line.unwrap_or(usize::MAX)
                                                ))
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    if let Some(line) = fold_line {
                                                        this.surface.toggle_fold_at_line(line);
                                                        cx.notify();
                                                    }
                                                }))
                                        })
                                        .child(
                                            div()
                                                .w(px(frame.gutter.line_number_width as f32
                                                    * geometry.gutter_character_width))
                                                .text_right()
                                                .child(format!(
                                                    "{}",
                                                    gutter
                                                        .map(|gutter| gutter.line_number)
                                                        .unwrap_or(0)
                                                )),
                                        ),
                                ),
                        )
                        .child(code),
                );
            }
            // Keep the clip rectangle in a fixed-size sibling wrapper. The
            // translated stack is deliberately larger than this wrapper so
            // its one-row presentation buffer can never paint outside the
            // editor viewport.
            let rows = div()
                .flex_1()
                .h(px(geometry.height))
                .overflow_hidden()
                .relative()
                .child(row_stack);
            let mut rows = rows;
            if let Some(scrollbar) = scrollbar_geometry(self.surface.scroll(), geometry.height) {
                rows = rows.child(
                    div()
                        .absolute()
                        .top(px(0.0))
                        .right(px(4.0))
                        .w(px(8.0))
                        .h(px(geometry.height))
                        .bg(color(SurfaceColor::rgba(13, 20, 32, 80)))
                        .child(
                            div()
                                .absolute()
                                .top(px(scrollbar.thumb_top))
                                .right(px(0.0))
                                .w(px(8.0))
                                .h(px(scrollbar.thumb_height))
                                .rounded_sm()
                                .bg(color(SurfaceColor::rgba(122, 151, 187, 190))),
                        ),
                );
            }
            root.child(rows)
        }
    }

    fn editor_font() -> wgpui::Font {
        let mut font = font("JetBrains Mono");
        font.fallbacks = Some(FontFallbacks::from_fonts(vec![
            "Cascadia Mono".to_owned(),
            "Consolas".to_owned(),
            "monospace".to_owned(),
        ]));
        font
    }

    fn color(color: SurfaceColor) -> wgpui::Hsla {
        wgpui::Rgba {
            r: f32::from(color.red) / 255.0,
            g: f32::from(color.green) / 255.0,
            b: f32::from(color.blue) / 255.0,
            a: f32::from(color.alpha) / 255.0,
        }
        .into()
    }

    fn decoration_color(theme: &super::SurfaceTheme, style_id: u32) -> SurfaceColor {
        match style_id {
            SEARCH_MATCH_STYLE_ID => theme.selection,
            SEARCH_CURRENT_MATCH_STYLE_ID => theme.primary_selection,
            DIAGNOSTIC_ERROR_STYLE_ID => SurfaceColor::rgba(220, 80, 80, 255),
            DIAGNOSTIC_WARNING_STYLE_ID => SurfaceColor::rgba(220, 170, 70, 255),
            DIAGNOSTIC_INFO_STYLE_ID => SurfaceColor::rgba(80, 150, 220, 255),
            DIAGNOSTIC_HINT_STYLE_ID => SurfaceColor::rgba(120, 190, 140, 255),
            _ => theme.decoration,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use mockaco_renderer::{DisplayConfig, FoldRegion};

        #[test]
        fn native_presentation_keeps_gutter_and_code_geometry_aligned() {
            let surface = EditorSurface::new(
                "fn main() {\n    // visible\n    let value = 7;\n}\n",
                DisplayConfig::unwrapped(),
                Default::default(),
            );
            let frame = surface.render_frame();
            assert!(surface.gutter_width() >= 10.0);
            assert_eq!(frame.gutter.rows.len(), frame.rows.len());
            assert!(frame.gutter.rows.iter().any(|row| row.foldable));
            assert!(frame.rows.iter().any(|row| !row.tokens.is_empty()));
        }

        #[test]
        fn native_presentation_exposes_fold_marker_and_active_selection_contrast() {
            let mut surface = EditorSurface::new(
                "fn main() {\n    let value = 7;\n}\n",
                DisplayConfig::unwrapped(),
                Default::default(),
            );
            surface.set_folds(mockaco_renderer::FoldSet::with_foldable(
                [FoldRegion::new(0, 3)],
                [FoldRegion::new(0, 3)],
            ));
            let frame = surface.render_frame();
            assert!(frame.gutter.rows[0].folded);
            assert_ne!(frame.theme.active_line, frame.theme.background);
            assert_ne!(frame.theme.primary_selection, frame.theme.background);
        }

        #[test]
        fn line_wheel_scroll_keeps_a_fractional_visual_remainder() {
            let (rows, remainder) = fractional_line_scroll(0.0, -1.0, 20.0);
            assert_eq!(rows, 0);
            assert!((remainder + 9.0).abs() < f32::EPSILON);

            let (rows, remainder) = fractional_line_scroll(remainder, -1.0, 20.0);
            assert_eq!(rows, 0);
            assert!((remainder + 18.0).abs() < f32::EPSILON);
        }

        #[test]
        fn scrollbar_thumb_tracks_logical_scroll_and_has_a_minimum_size() {
            let mut scroll = ScrollState::default();
            scroll.set_viewport(10, 80);
            scroll.set_content_with_bottom_padding(100, 80, 1);
            let top = scrollbar_geometry(scroll, 400.0).expect("scrollbar is visible");
            scroll.scroll_to(usize::MAX, 0);
            let bottom = scrollbar_geometry(scroll, 400.0).expect("scrollbar is visible");

            assert!(top.thumb_height >= 28.0);
            assert!(bottom.thumb_top > top.thumb_top);
            assert!(bottom.thumb_top + bottom.thumb_height <= 400.0);
            assert!(scrollbar_geometry(scroll, 0.0).is_none());
        }
    }
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
        surface.set_viewport(2, 20);
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
        assert!(surface.display().gutter(4).rows[0].foldable);
        let mut router = InputRouter::default();
        router
            .route(
                &mut surface,
                InputEvent::Mouse(MouseEvent::Down {
                    x: 4.0,
                    y: 0.0,
                    button: MouseButton::Primary,
                    click_count: 1,
                }),
            )
            .unwrap();
        assert_eq!(surface.display().row_count(), 2);
        assert!(surface.display().gutter(4).rows[0].folded);
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
        assert!(!surface.display().gutter(4).rows.is_empty());
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
        router
            .route(
                &mut surface,
                InputEvent::Mouse(MouseEvent::Down {
                    x: 64.0,
                    y: 0.0,
                    button: MouseButton::Primary,
                    click_count: 1,
                }),
            )
            .unwrap();
        router
            .route(
                &mut surface,
                InputEvent::Mouse(MouseEvent::Drag { x: 64.0, y: 20.0 }),
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
