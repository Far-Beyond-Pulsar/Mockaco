//! Framework-independent split-screen diff editor model.
//!
//! Diff computation owns neither files nor UI. The original side is represented
//! by an immutable [`DocumentSnapshot`], while edits are applied only through
//! the modified [`EditorState`]. The model exposes aligned rows, hunk ranges,
//! position conversion, decorations, and scroll policy for presentation crates.

use mockaco_core::{DocumentSnapshot, Edit, EditorState, Grouping, Transaction, TransactionError};
use similar::{DiffTag, TextDiff};
use std::fmt;
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffSide {
    Original,
    Modified,
}

impl DiffSide {
    fn other(self) -> Self {
        match self {
            Self::Original => Self::Modified,
            Self::Modified => Self::Original,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRowKind {
    Equal,
    Insert,
    Delete,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub line: usize,
    pub byte_range: Range<usize>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    pub original: Option<DiffLine>,
    pub modified: Option<DiffLine>,
    pub kind: DiffRowKind,
    pub hunk: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffHunkKind {
    Insert,
    Delete,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub kind: DiffHunkKind,
    pub original_range: Range<usize>,
    pub modified_range: Range<usize>,
    pub row_range: Range<usize>,
}

/// A complete, versioned diff result that can be computed off-thread and
/// published only if both source versions are still current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffResult {
    pub original_version: u64,
    pub modified_version: u64,
    pub original_line_count: usize,
    pub modified_line_count: usize,
    rows: Vec<DiffRow>,
    hunks: Vec<DiffHunk>,
    original_line_rows: Vec<usize>,
    modified_line_rows: Vec<usize>,
}

impl DiffResult {
    pub fn rows(&self) -> &[DiffRow] {
        &self.rows
    }

    pub fn hunks(&self) -> &[DiffHunk] {
        &self.hunks
    }

    pub fn is_equal(&self) -> bool {
        self.hunks.is_empty()
    }

    pub fn row_for_line(&self, side: DiffSide, line: usize) -> Option<usize> {
        self.line_rows(side).get(line).copied()
    }

    pub fn line_mapping(&self, side: DiffSide, line: usize) -> Result<DiffLineMapping, DiffError> {
        let row = self
            .row_for_line(side, line)
            .ok_or(DiffError::LineOutOfBounds {
                side,
                line,
                line_count: self.line_count(side),
            })?;
        let diff_row = &self.rows[row];
        Ok(DiffLineMapping {
            row,
            side_line: line,
            counterpart_line: line_in_row(diff_row, side.other()).map(|line| line.line),
            hunk: diff_row.hunk,
        })
    }

    pub fn map_position(
        &self,
        side: DiffSide,
        position: DiffPosition,
        target_side: DiffSide,
    ) -> Result<DiffPositionMapping, DiffError> {
        let row = self
            .row_for_line(side, position.line)
            .ok_or(DiffError::LineOutOfBounds {
                side,
                line: position.line,
                line_count: self.line_count(side),
            })?;
        let source_line = line_in_row(&self.rows[row], side).expect("line map points to its row");
        if position.column > source_line.text.len()
            || !source_line.text.is_char_boundary(position.column)
        {
            return Err(DiffError::ColumnOutOfBounds {
                side,
                line: position.line,
                column: position.column,
                line_length: source_line.text.len(),
            });
        }

        let diff_row = &self.rows[row];
        let target_line = line_in_row(diff_row, target_side);
        let boundary_line = target_line.map(|line| line.line).unwrap_or_else(|| {
            diff_row
                .hunk
                .map(|hunk| {
                    let range = match target_side {
                        DiffSide::Original => &self.hunks[hunk].original_range,
                        DiffSide::Modified => &self.hunks[hunk].modified_range,
                    };
                    range.start.min(self.line_count(target_side))
                })
                .unwrap_or(0)
        });
        let column = target_line
            .map(|line| position.column.min(line.text.len()))
            .unwrap_or(0);
        Ok(DiffPositionMapping {
            row,
            side: target_side,
            line: target_line.map(|line| line.line),
            boundary_line,
            column,
            hunk: diff_row.hunk,
        })
    }

    pub fn map_selection(
        &self,
        side: DiffSide,
        selection: DiffSelection,
        target_side: DiffSide,
    ) -> Result<DiffSelectionMapping, DiffError> {
        Ok(DiffSelectionMapping {
            anchor: self.map_position(side, selection.anchor, target_side)?,
            head: self.map_position(side, selection.head, target_side)?,
        })
    }

    fn line_rows(&self, side: DiffSide) -> &[usize] {
        match side {
            DiffSide::Original => &self.original_line_rows,
            DiffSide::Modified => &self.modified_line_rows,
        }
    }

    fn line_count(&self, side: DiffSide) -> usize {
        match side {
            DiffSide::Original => self.original_line_count,
            DiffSide::Modified => self.modified_line_count,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffPosition {
    /// Line number, zero-based.
    pub line: usize,
    /// UTF-8 byte column on that line.
    pub column: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffLineMapping {
    pub row: usize,
    pub side_line: usize,
    pub counterpart_line: Option<usize>,
    pub hunk: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffPositionMapping {
    pub row: usize,
    pub side: DiffSide,
    pub line: Option<usize>,
    /// The target-side insertion boundary when `line` is missing.
    pub boundary_line: usize,
    pub column: usize,
    pub hunk: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffSelection {
    pub anchor: DiffPosition,
    pub head: DiffPosition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffSelectionMapping {
    pub anchor: DiffPositionMapping,
    pub head: DiffPositionMapping,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffDecorationKind {
    Insert,
    Delete,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffDecoration {
    pub side: DiffSide,
    pub row: usize,
    pub line: usize,
    pub byte_range: Range<usize>,
    pub kind: DiffDecorationKind,
    pub hunk: usize,
}

pub fn diff_decorations(result: &DiffResult) -> Vec<DiffDecoration> {
    result
        .rows
        .iter()
        .enumerate()
        .filter_map(|(row, diff_row)| {
            let hunk = diff_row.hunk?;
            let kind = match diff_row.kind {
                DiffRowKind::Equal => return None,
                DiffRowKind::Insert => DiffDecorationKind::Insert,
                DiffRowKind::Delete => DiffDecorationKind::Delete,
                DiffRowKind::Replace => DiffDecorationKind::Replace,
            };
            let side = match diff_row.kind {
                DiffRowKind::Insert => DiffSide::Modified,
                DiffRowKind::Delete => DiffSide::Original,
                DiffRowKind::Replace => DiffSide::Modified,
                DiffRowKind::Equal => unreachable!(),
            };
            let line = line_in_row(diff_row, side)?;
            Some(DiffDecoration {
                side,
                row,
                line: line.line,
                byte_range: line.byte_range.clone(),
                kind,
                hunk,
            })
        })
        .chain(
            result
                .rows
                .iter()
                .enumerate()
                .filter_map(|(row, diff_row)| {
                    if diff_row.kind != DiffRowKind::Replace {
                        return None;
                    }
                    let hunk = diff_row.hunk?;
                    let line = diff_row.original.as_ref()?;
                    Some(DiffDecoration {
                        side: DiffSide::Original,
                        row,
                        line: line.line,
                        byte_range: line.byte_range.clone(),
                        kind: DiffDecorationKind::Replace,
                        hunk,
                    })
                }),
        )
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffScrollMode {
    Independent,
    Synchronized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffScrollState {
    mode: DiffScrollMode,
    original_top_row: usize,
    modified_top_row: usize,
    viewport_rows: usize,
    content_rows: usize,
}

impl DiffScrollState {
    pub fn new(mode: DiffScrollMode, content_rows: usize, viewport_rows: usize) -> Self {
        Self {
            mode,
            original_top_row: 0,
            modified_top_row: 0,
            viewport_rows,
            content_rows,
        }
    }

    pub fn mode(&self) -> DiffScrollMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: DiffScrollMode) {
        self.mode = mode;
        if mode == DiffScrollMode::Synchronized {
            let row = self.original_top_row.min(self.modified_top_row);
            self.original_top_row = row;
            self.modified_top_row = row;
        }
        self.clamp();
    }

    pub fn set_content(&mut self, content_rows: usize) {
        self.content_rows = content_rows;
        self.clamp();
    }

    pub fn set_viewport(&mut self, viewport_rows: usize) {
        self.viewport_rows = viewport_rows;
        self.clamp();
    }

    pub fn top_row(&self, side: DiffSide) -> usize {
        match side {
            DiffSide::Original => self.original_top_row,
            DiffSide::Modified => self.modified_top_row,
        }
    }

    pub fn visible_rows(&self, side: DiffSide) -> Range<usize> {
        let top = self.top_row(side);
        top..top
            .saturating_add(self.viewport_rows)
            .min(self.content_rows)
    }

    pub fn scroll_by(&mut self, side: DiffSide, delta: isize) -> bool {
        let before = *self;
        let current = self.top_row(side);
        let next = offset(current, delta);
        match self.mode {
            DiffScrollMode::Independent => self.set_top_row(side, next),
            DiffScrollMode::Synchronized => {
                self.original_top_row = next;
                self.modified_top_row = next;
            }
        }
        self.clamp();
        *self != before
    }

    pub fn scroll_to(&mut self, side: DiffSide, row: usize) -> bool {
        let before = *self;
        match self.mode {
            DiffScrollMode::Independent => self.set_top_row(side, row),
            DiffScrollMode::Synchronized => {
                self.original_top_row = row;
                self.modified_top_row = row;
            }
        }
        self.clamp();
        *self != before
    }

    fn set_top_row(&mut self, side: DiffSide, row: usize) {
        match side {
            DiffSide::Original => self.original_top_row = row,
            DiffSide::Modified => self.modified_top_row = row,
        }
    }

    fn clamp(&mut self) {
        let max = self.content_rows.saturating_sub(self.viewport_rows);
        self.original_top_row = self.original_top_row.min(max);
        self.modified_top_row = self.modified_top_row.min(max);
        if self.mode == DiffScrollMode::Synchronized {
            self.modified_top_row = self.original_top_row;
        }
    }
}

fn offset(value: usize, delta: isize) -> usize {
    if delta.is_negative() {
        value.saturating_sub(delta.unsigned_abs())
    } else {
        value.saturating_add(delta as usize)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffError {
    LineOutOfBounds {
        side: DiffSide,
        line: usize,
        line_count: usize,
    },
    ColumnOutOfBounds {
        side: DiffSide,
        line: usize,
        column: usize,
        line_length: usize,
    },
    StaleResult {
        expected_original: u64,
        actual_original: u64,
        expected_modified: u64,
        actual_modified: u64,
    },
    Transaction(TransactionError),
}

impl fmt::Display for DiffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LineOutOfBounds {
                side,
                line,
                line_count,
            } => write!(f, "{side:?} line {line} is outside {line_count} lines"),
            Self::ColumnOutOfBounds {
                side,
                line,
                column,
                line_length,
            } => write!(
                f,
                "{side:?} position ({line}, {column}) is outside byte length {line_length}"
            ),
            Self::StaleResult {
                expected_original,
                actual_original,
                expected_modified,
                actual_modified,
            } => write!(
                f,
                "stale diff: expected versions ({expected_original}, {expected_modified}), got ({actual_original}, {actual_modified})"
            ),
            Self::Transaction(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for DiffError {}

impl From<TransactionError> for DiffError {
    fn from(error: TransactionError) -> Self {
        Self::Transaction(error)
    }
}

/// The original side is immutable; all editing methods target `modified`.
#[derive(Debug, Clone)]
pub struct DiffEditor {
    original: DocumentSnapshot,
    modified: EditorState,
    diff: DiffResult,
}

impl DiffEditor {
    pub fn new(original: &DocumentSnapshot, modified_text: impl Into<String>) -> Self {
        let modified = EditorState::new(modified_text);
        let diff = compute_diff(original, &modified.snapshot());
        Self {
            original: original.clone(),
            modified,
            diff,
        }
    }

    pub fn original(&self) -> &DocumentSnapshot {
        &self.original
    }

    pub fn modified(&self) -> &EditorState {
        &self.modified
    }

    pub fn diff(&self) -> &DiffResult {
        &self.diff
    }

    pub fn current_versions(&self) -> (u64, u64) {
        (self.original.version(), self.modified.snapshot().version())
    }

    pub fn set_original(&mut self, original: &DocumentSnapshot) {
        self.original = original.clone();
        self.diff = compute_diff(&self.original, &self.modified.snapshot());
    }

    pub fn apply_modified_edit(
        &mut self,
        range: Range<usize>,
        replacement: impl Into<String>,
    ) -> Result<(), DiffError> {
        let transaction = Transaction::from_edits([Edit::replace(range, replacement)]);
        self.apply_modified_transaction(&transaction, Grouping::Separate)
            .map(|_| ())
    }

    pub fn apply_modified_transaction(
        &mut self,
        transaction: &Transaction,
        grouping: Grouping,
    ) -> Result<mockaco_core::AppliedTransaction, DiffError> {
        let applied = self.modified.apply_with_result(transaction, grouping)?;
        self.diff = compute_diff(&self.original, &self.modified.snapshot());
        Ok(applied)
    }

    /// Publishes a background result only if it still describes this pair.
    pub fn accept_diff(&mut self, result: DiffResult) -> Result<(), DiffError> {
        let (actual_original, actual_modified) = self.current_versions();
        if result.original_version != actual_original || result.modified_version != actual_modified
        {
            return Err(DiffError::StaleResult {
                expected_original: result.original_version,
                actual_original,
                expected_modified: result.modified_version,
                actual_modified,
            });
        }
        self.diff = result;
        Ok(())
    }
}

pub fn compute_diff(original: &DocumentSnapshot, modified: &DocumentSnapshot) -> DiffResult {
    let original_lines = source_lines(original);
    let modified_lines = source_lines(modified);
    let original_values: Vec<_> = original_lines.iter().map(|line| line.text).collect();
    let modified_values: Vec<_> = modified_lines.iter().map(|line| line.text).collect();
    let text_diff = TextDiff::from_slices(&original_values, &modified_values);
    let mut rows = Vec::new();
    let mut hunks = Vec::new();

    for operation in text_diff.ops() {
        let old_range = operation.old_range();
        let new_range = operation.new_range();
        match operation.tag() {
            DiffTag::Equal => {
                for offset in 0..old_range.len() {
                    rows.push(DiffRow {
                        original: Some(to_diff_line(&original_lines[old_range.start + offset])),
                        modified: Some(to_diff_line(&modified_lines[new_range.start + offset])),
                        kind: DiffRowKind::Equal,
                        hunk: None,
                    });
                }
            }
            DiffTag::Insert | DiffTag::Delete | DiffTag::Replace => {
                let hunk_index = hunks.len();
                let row_start = rows.len();
                let row_count = old_range.len().max(new_range.len());
                for offset in 0..row_count {
                    let original_line = (offset < old_range.len())
                        .then(|| to_diff_line(&original_lines[old_range.start + offset]));
                    let modified_line = (offset < new_range.len())
                        .then(|| to_diff_line(&modified_lines[new_range.start + offset]));
                    let kind = match (&original_line, &modified_line) {
                        (Some(_), Some(_)) => DiffRowKind::Replace,
                        (Some(_), None) => DiffRowKind::Delete,
                        (None, Some(_)) => DiffRowKind::Insert,
                        (None, None) => continue,
                    };
                    rows.push(DiffRow {
                        original: original_line,
                        modified: modified_line,
                        kind,
                        hunk: Some(hunk_index),
                    });
                }
                let kind = match (old_range.is_empty(), new_range.is_empty()) {
                    (true, false) => DiffHunkKind::Insert,
                    (false, true) => DiffHunkKind::Delete,
                    (false, false) => DiffHunkKind::Replace,
                    (true, true) => unreachable!(),
                };
                hunks.push(DiffHunk {
                    kind,
                    original_range: old_range,
                    modified_range: new_range,
                    row_range: row_start..rows.len(),
                });
            }
        }
    }

    let mut original_line_rows = vec![0; original_lines.len()];
    let mut modified_line_rows = vec![0; modified_lines.len()];
    for (row, diff_row) in rows.iter().enumerate() {
        if let Some(line) = &diff_row.original {
            original_line_rows[line.line] = row;
        }
        if let Some(line) = &diff_row.modified {
            modified_line_rows[line.line] = row;
        }
    }
    DiffResult {
        original_version: original.version(),
        modified_version: modified.version(),
        original_line_count: original_lines.len(),
        modified_line_count: modified_lines.len(),
        rows,
        hunks,
        original_line_rows,
        modified_line_rows,
    }
}

#[derive(Debug, Clone)]
struct SourceLine<'a> {
    line: usize,
    byte_range: Range<usize>,
    text: &'a str,
}

fn source_lines(snapshot: &DocumentSnapshot) -> Vec<SourceLine<'_>> {
    (0..snapshot.position_map().line_count())
        .filter_map(|line| {
            let start = snapshot.position_map().line_start(line).ok()?;
            let end = snapshot.position_map().line_end(line).ok()?;
            Some(SourceLine {
                line,
                byte_range: start..end,
                text: &snapshot.text()[start..end],
            })
        })
        .filter(|line| !(snapshot.text().is_empty() && line.line == 0))
        .collect()
}

fn to_diff_line(line: &SourceLine<'_>) -> DiffLine {
    DiffLine {
        line: line.line,
        byte_range: line.byte_range.clone(),
        text: line.text.to_owned(),
    }
}

fn line_in_row(row: &DiffRow, side: DiffSide) -> Option<&DiffLine> {
    match side {
        DiffSide::Original => row.original.as_ref(),
        DiffSide::Modified => row.modified.as_ref(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockaco_core::{Document, Selection, SelectionSet};

    fn result(original: &str, modified: &str) -> DiffResult {
        let original = Document::new(original).snapshot();
        let modified = Document::new(modified).snapshot();
        compute_diff(&original, &modified)
    }

    #[test]
    fn equal_files_have_stable_aligned_rows() {
        let diff = result("one\ntwo\n", "one\ntwo\n");
        assert!(diff.is_equal());
        assert_eq!(diff.rows().len(), 3);
        assert!(diff.rows().iter().all(|row| row.kind == DiffRowKind::Equal));
        assert_eq!(diff.row_for_line(DiffSide::Original, 1), Some(1));
        assert_eq!(diff.row_for_line(DiffSide::Modified, 1), Some(1));
    }

    #[test]
    fn insert_delete_and_replace_hunks_are_deterministic() {
        let insert = result("a\nc", "a\nb\nc");
        assert_eq!(insert.hunks()[0].kind, DiffHunkKind::Insert);
        assert_eq!(insert.hunks()[0].original_range, 1..1);
        assert_eq!(insert.hunks()[0].modified_range, 1..2);
        assert_eq!(insert.rows()[1].kind, DiffRowKind::Insert);

        let delete = result("a\nb\nc", "a\nc");
        assert_eq!(delete.hunks()[0].kind, DiffHunkKind::Delete);
        assert_eq!(delete.rows()[1].kind, DiffRowKind::Delete);
        assert!(delete.rows()[1].modified.is_none());

        let replace = result("a\nb\nc", "a\nB\nc");
        assert_eq!(replace.hunks()[0].kind, DiffHunkKind::Replace);
        assert_eq!(replace.rows()[1].kind, DiffRowKind::Replace);
        assert_eq!(replace.hunks()[0].row_range, 1..2);
    }

    #[test]
    fn empty_sides_and_unicode_lines_are_mapped_without_mutation() {
        let empty = result("", "new");
        assert_eq!(empty.original_line_count, 0);
        assert_eq!(empty.modified_line_count, 1);
        assert_eq!(empty.rows()[0].kind, DiffRowKind::Insert);

        let original = Document::new("α\n猫");
        let before = original.snapshot();
        let modified = Document::new("α\n犬").snapshot();
        let diff = compute_diff(&before, &modified);
        assert_eq!(diff.rows()[1].original.as_ref().unwrap().text, "猫");
        assert_eq!(diff.rows()[1].modified.as_ref().unwrap().text, "犬");
        assert_eq!(original.snapshot(), before);
    }

    #[test]
    fn positions_and_selections_map_through_missing_lines_and_hunks() {
        let diff = result("a\nb\nc", "a\nc");
        let mapping = diff
            .map_position(
                DiffSide::Original,
                DiffPosition { line: 1, column: 1 },
                DiffSide::Modified,
            )
            .unwrap();
        assert_eq!(mapping.row, 1);
        assert_eq!(mapping.line, None);
        assert_eq!(mapping.boundary_line, 1);
        assert_eq!(mapping.column, 0);
        assert_eq!(mapping.hunk, Some(0));

        let selection = DiffSelection {
            anchor: DiffPosition { line: 0, column: 0 },
            head: DiffPosition { line: 1, column: 1 },
        };
        let mapped = diff
            .map_selection(DiffSide::Original, selection, DiffSide::Modified)
            .unwrap();
        assert_eq!(mapped.anchor.line, Some(0));
        assert_eq!(mapped.head.line, None);
        assert_eq!(mapped.head.boundary_line, 1);
    }

    #[test]
    fn scroll_modes_keep_hunks_aligned_when_synchronized() {
        let mut scroll = DiffScrollState::new(DiffScrollMode::Synchronized, 20, 5);
        assert!(scroll.scroll_to(DiffSide::Original, 8));
        assert_eq!(scroll.top_row(DiffSide::Modified), 8);
        scroll.set_mode(DiffScrollMode::Independent);
        assert!(scroll.scroll_to(DiffSide::Modified, 2));
        assert_eq!(scroll.top_row(DiffSide::Original), 8);
        assert_eq!(scroll.top_row(DiffSide::Modified), 2);
    }

    #[test]
    fn stale_results_cannot_replace_a_newer_modified_document() {
        let original = Document::new("a").snapshot();
        let mut editor = DiffEditor::new(&original, "b");
        let stale = editor.diff().clone();
        editor.apply_modified_edit(0..1, "c").unwrap();
        assert!(matches!(
            editor.accept_diff(stale),
            Err(DiffError::StaleResult { .. })
        ));
    }

    #[test]
    fn edits_while_visible_refresh_only_the_modified_side() {
        let original = Document::new("same\nline").snapshot();
        let mut editor = DiffEditor::new(&original, "same\nline");
        let original_before = editor.original().clone();
        editor.apply_modified_edit(5..9, "LINE").unwrap();
        assert_eq!(editor.original(), &original_before);
        assert_eq!(editor.modified().document().text(), "same\nLINE");
        assert_eq!(editor.diff().hunks().len(), 1);
    }

    #[test]
    fn decorations_are_side_specific_and_renderer_neutral() {
        let diff = result("a\nb", "a\nB\nc");
        let decorations = diff_decorations(&diff);
        assert!(decorations
            .iter()
            .any(|decoration| decoration.side == DiffSide::Original));
        assert!(decorations
            .iter()
            .any(|decoration| decoration.side == DiffSide::Modified));
    }

    #[test]
    fn modified_selection_remains_editable_through_core_state() {
        let original = Document::new("a").snapshot();
        let mut editor = DiffEditor::new(&original, "a");
        editor
            .modified
            .set_selections(SelectionSet::new([Selection::caret(1)]));
        editor.apply_modified_edit(1..1, "!").unwrap();
        assert_eq!(editor.modified().document().text(), "a!");
        assert_eq!(
            editor.modified().selections().primary().unwrap().cursor(),
            2
        );
    }
}
