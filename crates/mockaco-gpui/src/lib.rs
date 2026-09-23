//! Presentation and input adapter for Mockaco.
//!
//! The default API contains no WGPUI types. It produces render data and routes
//! input through `mockaco-core` and `mockaco-renderer`; the optional
//! `native-wgpui` module is the only place that imports the real WGPUI crate.

use mockaco_core::{
    Affinity, DocumentSnapshot, Edit, EditorState, Grouping, Selection, SelectionSet, Transaction,
    TransactionError,
};
use mockaco_renderer::{
    Decoration, DisplayMap, DisplayMapError, DisplayPoint, DisplayViewport, GutterLayout,
    ProjectedDecoration,
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
        self.content_rows = rows;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceTheme {
    pub background: SurfaceColor,
    pub gutter_background: SurfaceColor,
    pub foreground: SurfaceColor,
    pub gutter_foreground: SurfaceColor,
    pub selection: SurfaceColor,
    pub primary_selection: SurfaceColor,
    pub caret: SurfaceColor,
    pub decoration: SurfaceColor,
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
        }
    }
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
    Down { x: f32, y: f32, button: MouseButton },
    Drag { x: f32, y: f32 },
    Up { x: f32, y: f32, button: MouseButton },
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
    Mouse(MouseEvent),
    Scroll { vertical: isize, horizontal: isize },
    Ime(ImeEvent),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditCommand {
    InsertText(String),
    DeleteBackward,
    DeleteForward,
    MoveLeft { extend: bool },
    MoveRight { extend: bool },
    MoveHome { extend: bool },
    MoveEnd { extend: bool },
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
    Input(InputError),
    StaleDocumentVersion { expected: u64, actual: u64 },
}

impl fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transaction(error) => error.fmt(f),
            Self::Display(error) => error.fmt(f),
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
#[derive(Debug, Clone)]
pub struct EditorSurface {
    editor: EditorState,
    display: DisplayMap,
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
        let display = DisplayMap::new(&editor.snapshot(), display_config);
        let mut surface = Self {
            editor,
            display,
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
        self.theme
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
        self.decorations = decorations;
        self.invalidation.push(InvalidationKind::Document, None);
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
        let applied = self.editor.apply_with_result(transaction, grouping)?;
        let snapshot = self.editor.snapshot();
        let mut display = self.display.clone();
        let update = display.update(&snapshot, &applied)?;
        self.display = display;
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
        self.recompute_scroll_viewport();
        self.invalidation
            .push(InvalidationKind::Document, Some(update.new_range));
        Ok(())
    }

    pub fn render_frame(&self) -> RenderFrame {
        let viewport_range = self.scroll.visible_rows();
        let viewport = DisplayViewport::new(self.scroll.top_row, self.scroll.viewport_rows);
        let rows = viewport_range
            .clone()
            .map(|display_row| {
                let row = &self.display.rows()[display_row];
                PaintRow {
                    display_row,
                    buffer_line: row.buffer_line,
                    source_range: row.start_byte..row.end_byte,
                    text: self.display.snapshot().text()[row.start_byte..row.end_byte].to_owned(),
                    y: (display_row - self.scroll.top_row) as f32 * self.geometry.line_height,
                    height: self.geometry.line_height,
                    continuation: row.continuation,
                    folded: row.folded,
                    truncated: row.truncated,
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
            theme: self.theme,
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
        self.scroll.set_content(self.display.row_count(), max_width);
    }
}

#[derive(Debug, Clone, Default)]
pub struct InputRouter {
    drag_anchor: Option<usize>,
}

impl InputRouter {
    pub fn translate_key(event: &KeyEvent) -> Result<Option<EditCommand>, InputError> {
        let extend = event.modifiers.shift;
        let command = match &event.key {
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
            EditCommand::Newline => self.insert_text(surface, "\n".to_owned()),
            EditCommand::Tab => self.insert_text(surface, "\t".to_owned()),
            EditCommand::DeleteBackward => self.delete(surface, true),
            EditCommand::DeleteForward => self.delete(surface, false),
            EditCommand::MoveLeft { extend } => self.move_horizontal(surface, false, extend),
            EditCommand::MoveRight { extend } => self.move_horizontal(surface, true, extend),
            EditCommand::MoveHome { extend } => self.move_line_edge(surface, false, extend),
            EditCommand::MoveEnd { extend } => self.move_line_edge(surface, true, extend),
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
                let cursor = selection.head;
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
        surface.invalidation.push(InvalidationKind::Selection, None);
        Ok(InputOutcome {
            handled: true,
            document_changed: false,
            selection_changed: true,
            composition_changed: false,
            scroll_changed: false,
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
                let line = position_map.byte_to_line(selection.head).unwrap_or(0);
                let target = if end {
                    position_map.line_end(line).unwrap_or(selection.head)
                } else {
                    position_map.line_start(line).unwrap_or(selection.head)
                };
                if extend {
                    Selection::range(selection.anchor, target)
                } else {
                    Selection::caret(target)
                }
            });
        surface.editor.set_selections(SelectionSet::new(next));
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
            } => {
                let byte = mouse_to_byte(surface, x, y)?;
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
    let column = (x / surface.geometry.character_width.max(1.0)).floor() as usize;
    surface
        .display
        .display_to_buffer(DisplayPoint { row, column }, Affinity::Before)
        .map(|point| point.byte_offset)
        .map_err(SurfaceError::Display)
}

#[cfg(feature = "native-wgpui")]
pub mod native {
    use super::{
        EditorSurface, InputEvent, InputRouter, Key, KeyEvent, KeyModifiers, MouseButton,
        MouseEvent, RenderFrame, SurfaceColor,
    };
    use wgpui::{
        div, px, Context, FocusHandle, InteractiveElement, IntoElement, ParentElement, Render,
        ScrollDelta, ScrollWheelEvent, Styled, Window,
    };

    /// Native WGPUI editor view backed by the framework-independent surface.
    pub struct WgpuiEditorView {
        pub surface: EditorSurface,
        pub input_router: InputRouter,
        focus_handle: Option<FocusHandle>,
    }

    impl WgpuiEditorView {
        pub fn new(surface: EditorSurface) -> Self {
            Self {
                surface,
                input_router: InputRouter::default(),
                focus_handle: None,
            }
        }

        pub fn render_frame(&self) -> RenderFrame {
            self.surface.render_frame()
        }

        fn route(&mut self, event: InputEvent, cx: &mut Context<Self>) {
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
            let key = match keystroke.key.as_str() {
                "backspace" => Key::Backspace,
                "delete" => Key::Delete,
                "enter" => Key::Enter,
                "tab" => Key::Tab,
                "escape" => Key::Escape,
                "left" => Key::Left,
                "right" => Key::Right,
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
                if let Some(handle) = &self.focus_handle {
                    window.focus(handle, cx);
                }
                self.route(
                    InputEvent::Mouse(MouseEvent::Down {
                        x: event.position.x.as_f32(),
                        y: event.position.y.as_f32(),
                        button: MouseButton::Primary,
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
            if event.dragging() {
                self.route(
                    InputEvent::Mouse(MouseEvent::Drag {
                        x: event.position.x.as_f32(),
                        y: event.position.y.as_f32(),
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
                self.route(
                    InputEvent::Mouse(MouseEvent::Up {
                        x: event.position.x.as_f32(),
                        y: event.position.y.as_f32(),
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
            let (vertical, horizontal) = match event.delta {
                ScrollDelta::Lines(point) => (point.y.round() as isize, point.x.round() as isize),
                ScrollDelta::Pixels(point) => {
                    let geometry = self.surface.geometry();
                    (
                        (point.y.as_f32() / geometry.line_height.max(1.0)).round() as isize,
                        (point.x.as_f32() / geometry.character_width.max(1.0)).round() as isize,
                    )
                }
            };
            self.route(
                InputEvent::Scroll {
                    vertical,
                    horizontal,
                },
                cx,
            );
        }
    }

    impl Render for WgpuiEditorView {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            if self.focus_handle.is_none() {
                self.focus_handle = Some(cx.focus_handle());
            }
            let frame = self.surface.render_frame();
            let geometry = self.surface.geometry();
            let gutter_width =
                (frame.gutter.line_number_width as f32 + 1.0) * geometry.gutter_character_width;
            let theme = frame.theme;
            let mut root = div()
                .flex()
                .flex_col()
                .size_full()
                .bg(color(theme.background))
                .text_color(color(theme.foreground))
                .track_focus(self.focus_handle.as_ref().unwrap())
                .on_key_down(cx.listener(Self::on_key_down))
                .on_mouse_down(wgpui::MouseButton::Left, cx.listener(Self::on_mouse_down))
                .on_mouse_move(cx.listener(Self::on_mouse_move))
                .on_mouse_up(wgpui::MouseButton::Left, cx.listener(Self::on_mouse_up))
                .on_scroll_wheel(cx.listener(Self::on_scroll));
            for row in frame.rows {
                let gutter = frame
                    .gutter
                    .rows
                    .iter()
                    .find(|gutter| gutter.display_row == row.display_row);
                let mut code = div()
                    .relative()
                    .flex_1()
                    .h(px(row.height))
                    .whitespace_nowrap();
                for selection in frame
                    .selections
                    .iter()
                    .filter(|selection| selection.display_row == row.display_row)
                {
                    code = code.child(
                        div()
                            .absolute()
                            .left(px(selection.x))
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
                            .left(px(decoration.x))
                            .top(px(decoration.y - row.y))
                            .w(px(decoration.width.max(1.0)))
                            .h(px(decoration.height))
                            .border_b_1()
                            .border_color(color(theme.decoration)),
                    );
                }
                code = code.child(row.text.clone());
                for caret in frame
                    .carets
                    .iter()
                    .filter(|caret| caret.display_row == row.display_row)
                {
                    code = code.child(
                        div()
                            .absolute()
                            .left(px(caret.x))
                            .top(px(caret.y - row.y))
                            .w(px(if caret.primary { 2.0 } else { 1.0 }))
                            .h(px(caret.height))
                            .bg(color(theme.caret)),
                    );
                }
                root = root.child(
                    div()
                        .flex()
                        .flex_row()
                        .h(px(row.height))
                        .child(
                            div()
                                .h(px(row.height))
                                .w(px(gutter_width))
                                .bg(color(theme.gutter_background))
                                .text_color(color(theme.gutter_foreground))
                                .text_right()
                                .whitespace_nowrap()
                                .child(format!(
                                    "{}",
                                    gutter.map(|gutter| gutter.line_number).unwrap_or(0)
                                )),
                        )
                        .child(code),
                );
            }
            root
        }
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
                    x: 8.0,
                    y: 0.0,
                    button: MouseButton::Primary,
                }),
            )
            .unwrap();
        router
            .route(
                &mut surface,
                InputEvent::Mouse(MouseEvent::Drag { x: 8.0, y: 20.0 }),
            )
            .unwrap();
        assert_eq!(
            surface.editor().selections().selections()[0],
            Selection::range(1, 4)
        );
    }
}
