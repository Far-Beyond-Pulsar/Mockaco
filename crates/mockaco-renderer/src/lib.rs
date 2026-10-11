//! Renderer-neutral display mapping for Mockaco.
//!
//! This crate owns buffer-to-display layout only. It has no knowledge of a UI
//! framework, glyph library, filesystem, language parser, or host application.

use mockaco_core::{
    Affinity, AppliedTransaction, DiagnosticSet, DiagnosticSeverity, DocumentSnapshot,
    PositionError, SearchSession,
};
use std::fmt;

mod line_index;
use line_index::{LineIndex, LineInfo};
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WrapConfig {
    pub width: usize,
    pub tab_width: usize,
    pub max_segments_per_line: usize,
    pub max_line_scan_bytes: usize,
}

impl WrapConfig {
    pub fn new(width: usize) -> Self {
        Self {
            width: width.max(1),
            tab_width: 4,
            max_segments_per_line: 4096,
            max_line_scan_bytes: 64 * 1024,
        }
    }

    pub fn tab_width(mut self, width: usize) -> Self {
        self.tab_width = width.max(1);
        self
    }

    pub fn max_segments_per_line(mut self, max: usize) -> Self {
        self.max_segments_per_line = max.max(1);
        self
    }

    pub fn max_line_scan_bytes(mut self, max: usize) -> Self {
        self.max_line_scan_bytes = max.max(1);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayConfig {
    pub wrap: Option<WrapConfig>,
    pub max_line_scan_bytes: usize,
    pub tab_width: usize,
}

impl DisplayConfig {
    pub fn unwrapped() -> Self {
        Self {
            wrap: None,
            max_line_scan_bytes: 64 * 1024,
            tab_width: 4,
        }
    }

    pub fn wrapped(width: usize) -> Self {
        Self {
            wrap: Some(WrapConfig::new(width)),
            max_line_scan_bytes: 64 * 1024,
            tab_width: 4,
        }
    }

    pub fn with_wrap(mut self, wrap: Option<WrapConfig>) -> Self {
        self.wrap = wrap;
        self
    }

    pub fn max_line_scan_bytes(mut self, max: usize) -> Self {
        self.max_line_scan_bytes = max.max(1);
        self
    }

    pub fn tab_width(mut self, width: usize) -> Self {
        self.tab_width = width.max(1);
        self
    }
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self::unwrapped()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldRegion {
    pub start_line: usize,
    pub end_line: usize,
    pub placeholder: String,
}

impl FoldRegion {
    pub fn new(start_line: usize, end_line: usize) -> Self {
        Self {
            start_line,
            end_line,
            placeholder: "…".to_owned(),
        }
    }

    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    pub fn hides_line(&self, line: usize) -> bool {
        self.start_line < self.end_line && line > self.start_line && line < self.end_line
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FoldSet {
    regions: Vec<FoldRegion>,
    foldable: Vec<FoldRegion>,
}

impl FoldSet {
    pub fn new(regions: impl IntoIterator<Item = FoldRegion>) -> Self {
        let mut regions: Vec<_> = regions
            .into_iter()
            .filter(|region| region.start_line < region.end_line)
            .collect();
        regions.sort_by_key(|region| (region.start_line, region.end_line));
        regions.dedup_by_key(|region| (region.start_line, region.end_line));
        Self {
            regions,
            foldable: Vec::new(),
        }
    }

    pub fn with_foldable(
        active: impl IntoIterator<Item = FoldRegion>,
        foldable: impl IntoIterator<Item = FoldRegion>,
    ) -> Self {
        let mut set = Self::new(active);
        set.foldable = normalize_folds(foldable);
        set
    }

    pub fn regions(&self) -> &[FoldRegion] {
        &self.regions
    }

    pub fn foldable_regions(&self) -> &[FoldRegion] {
        &self.foldable
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    pub fn region_starting_at(&self, line: usize) -> Option<&FoldRegion> {
        let index = self
            .regions
            .partition_point(|region| region.start_line < line);
        self.regions
            .get(index)
            .filter(|region| region.start_line == line)
    }

    pub fn foldable_starting_at(&self, line: usize) -> Option<&FoldRegion> {
        let index = self
            .foldable
            .partition_point(|region| region.start_line < line);
        self.foldable
            .get(index)
            .filter(|region| region.start_line == line)
            .or_else(|| self.region_starting_at(line))
    }

    pub fn is_foldable(&self, line: usize) -> bool {
        self.foldable_starting_at(line).is_some()
    }

    pub fn hidden_by(&self, line: usize) -> Option<&FoldRegion> {
        self.regions.iter().find(|region| region.hides_line(line))
    }
}

fn normalize_folds(regions: impl IntoIterator<Item = FoldRegion>) -> Vec<FoldRegion> {
    let mut regions: Vec<_> = regions
        .into_iter()
        .filter(|region| region.start_line < region.end_line)
        .collect();
    regions.sort_by_key(|region| (region.start_line, region.end_line));
    regions.dedup_by_key(|region| (region.start_line, region.end_line));
    regions
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayRow {
    pub buffer_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub continuation: bool,
    pub folded: bool,
    pub truncated: bool,
    pub display_width: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayPoint {
    pub row: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferPoint {
    pub byte_offset: usize,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayViewport {
    pub top_row: usize,
    pub row_count: usize,
}

impl DisplayViewport {
    pub fn new(top_row: usize, row_count: usize) -> Self {
        Self { top_row, row_count }
    }

    pub fn rows(self, total_rows: usize) -> Range<usize> {
        let start = self.top_row.min(total_rows);
        let end = start.saturating_add(self.row_count).min(total_rows);
        start..end
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GutterLayout {
    pub line_number_width: usize,
    pub rows: Vec<GutterRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GutterRow {
    pub display_row: usize,
    pub buffer_line: usize,
    pub line_number: usize,
    pub continuation: bool,
    pub folded: bool,
    pub foldable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecorationStyle {
    pub id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoration {
    pub range: Range<usize>,
    pub style: DecorationStyle,
}

pub const SEARCH_MATCH_STYLE_ID: u32 = 0x1000;
pub const SEARCH_CURRENT_MATCH_STYLE_ID: u32 = 0x1001;
pub const DIAGNOSTIC_ERROR_STYLE_ID: u32 = 0x2000;
pub const DIAGNOSTIC_WARNING_STYLE_ID: u32 = 0x2001;
pub const DIAGNOSTIC_INFO_STYLE_ID: u32 = 0x2002;
pub const DIAGNOSTIC_HINT_STYLE_ID: u32 = 0x2003;

pub fn search_decorations(session: &SearchSession) -> Vec<Decoration> {
    session
        .matches()
        .iter()
        .enumerate()
        .map(|(index, range)| {
            let style = if session.current_match_index() == Some(index) {
                SEARCH_CURRENT_MATCH_STYLE_ID
            } else {
                SEARCH_MATCH_STYLE_ID
            };
            Decoration::new(range.clone(), style)
        })
        .collect()
}

pub fn diagnostic_decorations(diagnostics: &DiagnosticSet) -> Vec<Decoration> {
    diagnostics
        .diagnostics()
        .iter()
        .map(|diagnostic| {
            let style = match diagnostic.severity {
                DiagnosticSeverity::Error => DIAGNOSTIC_ERROR_STYLE_ID,
                DiagnosticSeverity::Warning => DIAGNOSTIC_WARNING_STYLE_ID,
                DiagnosticSeverity::Info => DIAGNOSTIC_INFO_STYLE_ID,
                DiagnosticSeverity::Hint => DIAGNOSTIC_HINT_STYLE_ID,
            };
            Decoration::new(diagnostic.range.clone(), style)
        })
        .collect()
}

impl Decoration {
    pub fn new(range: Range<usize>, id: u32) -> Self {
        Self {
            range,
            style: DecorationStyle { id },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedDecoration {
    pub display_row: usize,
    pub start_column: usize,
    pub end_column: usize,
    pub source_range: Range<usize>,
    pub style: DecorationStyle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayUpdate {
    pub old_range: Range<usize>,
    pub new_range: Range<usize>,
    pub reused_prefix_rows: usize,
    pub reused_suffix_rows: usize,
    pub recomputed_rows: usize,
    pub full_rebuild: bool,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayMapError {
    SnapshotVersion { expected: u64, actual: u64 },
    TransactionVersion { expected: u64, actual: u64 },
    RowOutOfBounds(usize),
    InvalidDisplayColumn { row: usize, column: usize },
    Position(PositionError),
}

impl fmt::Display for DisplayMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SnapshotVersion { expected, actual } => {
                write!(
                    f,
                    "display map expects snapshot version {expected}, got {actual}"
                )
            }
            Self::TransactionVersion { expected, actual } => {
                write!(f, "transaction expects version {expected}, got {actual}")
            }
            Self::RowOutOfBounds(row) => write!(f, "display row {row} is out of bounds"),
            Self::InvalidDisplayColumn { row, column } => {
                write!(f, "display column {column} is invalid on row {row}")
            }
            Self::Position(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for DisplayMapError {}

impl From<PositionError> for DisplayMapError {
    fn from(error: PositionError) -> Self {
        Self::Position(error)
    }
}

/// A renderer-neutral mapping from document bytes to visible display rows.
///
/// Rows are not stored. The map keeps, per buffer line, how many display rows
/// it takes and how wide it is ([`LineIndex`]), plus the folded line ranges;
/// a row's byte range is derived from the document's rope when it is asked
/// for. Asking for the rows on screen, and applying an edit, both cost
/// O(log n + the lines touched): document length does not enter into it.
#[derive(Debug, Clone)]
pub struct DisplayMap {
    snapshot: DocumentSnapshot,
    config: DisplayConfig,
    folds: FoldSet,
    index: LineIndex,
    /// Hidden line ranges: the inner lines of active folds, merged, sorted.
    hidden: Vec<Range<usize>>,
    /// `hidden_rows[k]`: display rows inside `hidden[..k]`.
    hidden_rows: Vec<usize>,
    /// `hidden_starts[k]`: the visible row index where `hidden[k]` begins,
    /// which is the row its first line would take if it were shown.
    hidden_starts: Vec<usize>,
    revision: u64,
    last_update: Option<DisplayUpdate>,
}

impl DisplayMap {
    pub fn new(snapshot: &DocumentSnapshot, config: DisplayConfig) -> Self {
        Self::with_folds(snapshot, config, FoldSet::default())
    }

    pub fn with_folds(snapshot: &DocumentSnapshot, config: DisplayConfig, folds: FoldSet) -> Self {
        let index = LineIndex::new(
            snapshot
                .text()
                .lines()
                .map(|line| line_info(&line, &config)),
        );
        let mut map = Self {
            snapshot: snapshot.clone(),
            config,
            folds,
            index,
            hidden: Vec::new(),
            hidden_rows: vec![0],
            hidden_starts: Vec::new(),
            revision: 0,
            last_update: None,
        };
        map.refresh_hidden();
        map
    }

    pub fn snapshot(&self) -> &DocumentSnapshot {
        &self.snapshot
    }

    pub fn config(&self) -> &DisplayConfig {
        &self.config
    }

    pub fn folds(&self) -> &FoldSet {
        &self.folds
    }

    pub fn row_count(&self) -> usize {
        self.index.row_count() - self.hidden_rows.last().copied().unwrap_or(0)
    }

    /// The widest line, in display columns. O(log n).
    pub fn max_display_width(&self) -> usize {
        self.index.max_width()
    }

    /// The line-number width: enough digits for the last line.
    pub fn line_number_width(&self, min_width: usize) -> usize {
        min_width.max(self.snapshot.position_map().line_count().to_string().len())
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn last_update(&self) -> Option<&DisplayUpdate> {
        self.last_update.as_ref()
    }

    pub fn set_folds(&mut self, folds: FoldSet) {
        self.folds = folds;
        self.refresh_hidden();
        self.revision = self.revision.saturating_add(1);
        self.last_update = None;
    }

    pub fn set_foldable_regions(&mut self, foldable: impl IntoIterator<Item = FoldRegion>) {
        self.folds = FoldSet::with_foldable(self.folds.regions().to_vec(), foldable);
        self.revision = self.revision.saturating_add(1);
        self.last_update = None;
    }

    /// Replaces foldability (not the active folds). Foldability does not
    /// change any row, so nothing is relaid out.
    pub fn set_foldable_regions_in_line_range(
        &mut self,
        _line_range: Range<usize>,
        foldable: impl IntoIterator<Item = FoldRegion>,
    ) {
        self.set_foldable_regions(foldable);
    }

    /// Row `row`, or `None` past the end.
    pub fn row(&self, row: usize) -> Option<DisplayRow> {
        if row >= self.row_count() {
            return None;
        }
        let hidden_before = self.hidden_starts.partition_point(|&start| start <= row);
        let absolute = row + self.hidden_rows[hidden_before];
        let (line, segment) = self.index.row_to_line(absolute)?;
        let segments = self.segments(line);
        let segment_index = segment.min(segments.len().saturating_sub(1));
        let segment = segments.get(segment_index)?;
        Some(DisplayRow {
            buffer_line: line,
            start_byte: segment.start,
            end_byte: segment.end,
            continuation: segment_index > 0,
            folded: self.folds.region_starting_at(line).is_some(),
            truncated: segment.truncated,
            display_width: segment.width,
        })
    }

    /// The rows in `range` (clamped to the document).
    ///
    /// Locates the first row once, then walks forward line by line (skipping
    /// folded lines), so a screenful costs one lookup plus its own lines.
    pub fn rows_in(&self, range: Range<usize>) -> Vec<DisplayRow> {
        let end = range.end.min(self.row_count());
        let start = range.start.min(end);
        let mut rows = Vec::with_capacity(end - start);
        if start == end {
            return rows;
        }
        let hidden_before = self.hidden_starts.partition_point(|&row| row <= start);
        let Some((mut line, mut segment)) =
            self.index.row_to_line(start + self.hidden_rows[hidden_before])
        else {
            return rows;
        };
        let mut segments = self.segments(line);
        while rows.len() < end - start {
            if segment >= segments.len() {
                line += 1;
                if let Some(hidden) = self.hidden_range(line) {
                    line = hidden.end;
                }
                if line >= self.index.line_count() {
                    break;
                }
                segments = self.segments(line);
                segment = 0;
                if segments.is_empty() {
                    break;
                }
                continue;
            }
            let current = &segments[segment];
            rows.push(DisplayRow {
                buffer_line: line,
                start_byte: current.start,
                end_byte: current.end,
                continuation: segment > 0,
                folded: self.folds.region_starting_at(line).is_some(),
                truncated: current.truncated,
                display_width: current.width,
            });
            segment += 1;
        }
        rows
    }

    pub fn visible_rows(&self, viewport: DisplayViewport) -> Vec<DisplayRow> {
        self.rows_in(viewport.rows(self.row_count()))
    }

    /// The visible row a shown `line` starts at, or `None` when a fold hides
    /// it.
    pub fn line_to_row(&self, line: usize) -> Option<usize> {
        let interval = self.hidden.partition_point(|hidden| hidden.end <= line);
        if self
            .hidden
            .get(interval)
            .is_some_and(|hidden| hidden.contains(&line))
        {
            return None;
        }
        Some(self.index.rows_before(line) - self.hidden_rows[interval])
    }

    /// The fold-hidden range containing `line`, if any.
    fn hidden_range(&self, line: usize) -> Option<&Range<usize>> {
        let interval = self.hidden.partition_point(|hidden| hidden.end <= line);
        self.hidden
            .get(interval)
            .filter(|hidden| hidden.contains(&line))
    }

    pub fn update(
        &mut self,
        snapshot: &DocumentSnapshot,
        transaction: &AppliedTransaction,
    ) -> Result<DisplayUpdate, DisplayMapError> {
        if self.snapshot.version() != transaction.before_version {
            return Err(DisplayMapError::TransactionVersion {
                expected: self.snapshot.version(),
                actual: transaction.before_version,
            });
        }
        if snapshot.version() != transaction.after_version {
            return Err(DisplayMapError::SnapshotVersion {
                expected: transaction.after_version,
                actual: snapshot.version(),
            });
        }
        let edits = transaction.change_map.edits();
        if edits.is_empty() {
            self.snapshot = snapshot.clone();
            let update = DisplayUpdate {
                old_range: 0..0,
                new_range: 0..0,
                reused_prefix_rows: self.row_count(),
                reused_suffix_rows: 0,
                recomputed_rows: 0,
                full_rebuild: false,
                revision: self.revision,
            };
            self.last_update = Some(update.clone());
            return Ok(update);
        }

        let old_start = edits.iter().map(|edit| edit.range.start).min().unwrap();
        let old_end = edits.iter().map(|edit| edit.range.end).max().unwrap();
        let old_range = old_start..old_end;
        let new_range = transaction.change_map.map_range(old_range.clone());
        let old_text = self.snapshot.position_map();
        let new_text = snapshot.position_map();
        let old_first_line = old_text.byte_to_line(old_start)?;
        let old_last_line = old_text.byte_to_line(old_end)?;
        let new_first_line = new_text.byte_to_line(new_range.start)?;
        let new_last_line = new_text.byte_to_line(new_range.end)?;

        let infos = (new_first_line..=new_last_line)
            .map(|line| {
                new_text
                    .line(line)
                    .map(|text| line_info(&text, &self.config))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let recomputed_rows = infos.iter().map(|info| info.rows as usize).sum();
        self.index
            .splice(old_first_line..old_last_line + 1, infos);

        if !self.folds.regions().is_empty() || !self.folds.foldable_regions().is_empty() {
            self.folds = map_folds(
                &self.snapshot,
                snapshot,
                &self.folds,
                &transaction.change_map,
            );
        }
        self.snapshot = snapshot.clone();
        self.refresh_hidden();
        self.revision = self.revision.saturating_add(1);
        let reused_prefix_rows = self.index.rows_before(new_first_line);
        let update = DisplayUpdate {
            old_range,
            new_range,
            reused_prefix_rows,
            reused_suffix_rows: self
                .index
                .row_count()
                .saturating_sub(reused_prefix_rows + recomputed_rows),
            recomputed_rows,
            full_rebuild: false,
            revision: self.revision,
        };
        self.last_update = Some(update.clone());
        Ok(update)
    }

    pub fn display_to_buffer(
        &self,
        point: DisplayPoint,
        affinity: Affinity,
    ) -> Result<BufferPoint, DisplayMapError> {
        let row = self
            .row(point.row)
            .ok_or(DisplayMapError::RowOutOfBounds(point.row))?;
        let text = self.snapshot.text().slice(row.start_byte..row.end_byte);
        let byte = display_column_to_byte(&text, &row, point.column, affinity, self.config.tab_width)
            .ok_or(DisplayMapError::InvalidDisplayColumn {
                row: point.row,
                column: point.column,
            })?;
        let position = self.snapshot.position_map().byte_to_line_column(byte)?;
        Ok(BufferPoint {
            byte_offset: byte,
            line: position.line,
            column: position.column,
        })
    }

    pub fn buffer_to_display(
        &self,
        byte: usize,
        affinity: Affinity,
    ) -> Result<DisplayPoint, DisplayMapError> {
        let text = self.snapshot.text();
        if byte > text.len() || !text.is_char_boundary(byte) {
            return Err(DisplayMapError::Position(
                PositionError::InvalidUtf8Boundary(byte),
            ));
        }
        let line = text.byte_to_line(byte)?;
        if let Some(hidden) = self.hidden_range(line) {
            // Inside a fold: the end of the fold's (shown) first line.
            let fold_line = hidden.start - 1;
            let row = self
                .line_to_row(fold_line)
                .ok_or(DisplayMapError::InvalidDisplayColumn { row: 0, column: byte })?;
            let width = self.row(row).map_or(0, |row| row.display_width);
            return Ok(DisplayPoint { row, column: width });
        }
        let first_row = self
            .line_to_row(line)
            .ok_or(DisplayMapError::InvalidDisplayColumn { row: 0, column: byte })?;
        let segments = self.segments(line);
        for (index, segment) in segments.iter().enumerate() {
            if byte < segment.start || byte > segment.end {
                continue;
            }
            if byte == segment.end && affinity == Affinity::After && index + 1 < segments.len() {
                continue;
            }
            let segment_text = text.slice(segment.start..byte);
            return Ok(DisplayPoint {
                row: first_row + index,
                column: display_width_of(&segment_text, self.config.tab_width),
            });
        }
        // Past a truncated line's scanned prefix: its last row's end.
        let last = segments.len().saturating_sub(1);
        Ok(DisplayPoint {
            row: first_row + last,
            column: segments.last().map_or(0, |segment| segment.width),
        })
    }

    pub fn gutter(&self, min_width: usize) -> GutterLayout {
        self.gutter_for_display_range(min_width, 0..self.row_count())
    }

    /// Gutter rows for a paint range only.
    pub fn gutter_for_display_range(
        &self,
        min_width: usize,
        display_range: Range<usize>,
    ) -> GutterLayout {
        let start = display_range.start;
        let rows = self
            .rows_in(display_range)
            .into_iter()
            .enumerate()
            .map(|(offset, row)| GutterRow {
                display_row: start + offset,
                buffer_line: row.buffer_line,
                line_number: row.buffer_line + 1,
                continuation: row.continuation,
                folded: row.folded,
                foldable: self.folds.is_foldable(row.buffer_line),
            })
            .collect();
        GutterLayout {
            line_number_width: self.line_number_width(min_width),
            rows,
        }
    }

    pub fn project_decorations(&self, decorations: &[Decoration]) -> Vec<ProjectedDecoration> {
        self.project_decorations_in_display_range(decorations, 0..self.row_count())
    }

    /// Projects decorations across the requested display rows only.
    pub fn project_decorations_in_display_range(
        &self,
        decorations: &[Decoration],
        display_range: Range<usize>,
    ) -> Vec<ProjectedDecoration> {
        let mut projected = Vec::new();
        let start_row = display_range.start;
        let rows = self.rows_in(display_range);
        let (Some(first), Some(last)) = (rows.first(), rows.last()) else {
            return projected;
        };
        let byte_start = first.start_byte;
        let byte_end = last.end_byte;
        let first_decoration =
            decorations.partition_point(|decoration| decoration.range.end <= byte_start);
        let text = self.snapshot.text();
        for (offset, row) in rows.iter().enumerate() {
            let display_row = start_row + offset;
            for decoration in &decorations[first_decoration..] {
                if decoration.range.start > byte_end {
                    break;
                }
                let start = decoration.range.start.max(row.start_byte);
                let end = decoration.range.end.min(row.end_byte);
                let overlaps = start < end
                    || (start == end
                        && decoration.range.start == decoration.range.end
                        && start >= row.start_byte
                        && start <= row.end_byte);
                if !overlaps {
                    continue;
                }
                let (Some(before_start), Some(before_end)) = (
                    text.get(row.start_byte..start),
                    text.get(row.start_byte..end),
                ) else {
                    continue;
                };
                projected.push(ProjectedDecoration {
                    display_row,
                    start_column: display_width_of(&before_start, self.config.tab_width),
                    end_column: display_width_of(&before_end, self.config.tab_width),
                    source_range: start..end,
                    style: decoration.style,
                });
            }
        }
        projected
    }

    /// `line`'s display segments: one when unwrapped, its wrap segments
    /// otherwise.
    fn segments(&self, line: usize) -> Vec<Segment> {
        let text = self.snapshot.text();
        let Ok(range) = text.line_range(line) else {
            return Vec::new();
        };
        let content = text.slice(range.clone());
        layout_line(
            &content,
            range.start,
            self.config.wrap.as_ref(),
            self.config.max_line_scan_bytes,
            self.config.tab_width,
        )
    }

    /// Rebuilds the hidden ranges and their row prefixes from the active
    /// folds. O(folds × log n).
    fn refresh_hidden(&mut self) {
        let line_count = self.index.line_count();
        let mut hidden: Vec<Range<usize>> = self
            .folds
            .regions()
            .iter()
            .filter(|region| region.start_line < region.end_line)
            .map(|region| (region.start_line + 1).min(line_count)..region.end_line.min(line_count))
            .filter(|range| !range.is_empty())
            .collect();
        hidden.sort_by_key(|range| range.start);
        let mut merged: Vec<Range<usize>> = Vec::with_capacity(hidden.len());
        for range in hidden {
            match merged.last_mut() {
                Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                _ => merged.push(range),
            }
        }
        let mut hidden_rows = Vec::with_capacity(merged.len() + 1);
        let mut hidden_starts = Vec::with_capacity(merged.len());
        hidden_rows.push(0);
        for range in &merged {
            let before = self.index.rows_before(range.start);
            let inside = self.index.rows_before(range.end) - before;
            let so_far = *hidden_rows.last().unwrap();
            hidden_starts.push(before - so_far);
            hidden_rows.push(so_far + inside);
        }
        self.hidden = merged;
        self.hidden_rows = hidden_rows;
        self.hidden_starts = hidden_starts;
    }
}

/// The display metrics of one line's content.
fn line_info(line: &str, config: &DisplayConfig) -> LineInfo {
    let segments = layout_line(
        line,
        0,
        config.wrap.as_ref(),
        config.max_line_scan_bytes,
        config.tab_width,
    );
    let width = match &config.wrap {
        None => segments.first().map_or(0, |segment| segment.width),
        Some(_) => {
            measure_width(line, 0, line.len(), config.tab_width, config.max_line_scan_bytes).0
        }
    };
    LineInfo {
        rows: segments.len().max(1) as u32,
        width: width.min(u32::MAX as usize) as u32,
    }
}

/// Display width of `text`, which starts at a row's first column.
fn display_width_of(text: &str, tab_width: usize) -> usize {
    let mut column = 0;
    for character in text.chars() {
        column += display_char_width(character, column, tab_width);
    }
    column
}

/// The byte at display `column` of `row`, whose text is `text`.
fn display_column_to_byte(
    text: &str,
    row: &DisplayRow,
    column: usize,
    affinity: Affinity,
    tab_width: usize,
) -> Option<usize> {
    if column > row.display_width && !row.truncated {
        return None;
    }
    let mut current = 0;
    for (offset, character) in text.char_indices() {
        if current == column {
            return Some(row.start_byte + offset);
        }
        let next = current.saturating_add(display_char_width(character, current, tab_width));
        if column < next {
            return Some(match affinity {
                Affinity::Before => row.start_byte + offset,
                Affinity::After => row.start_byte + offset + character.len_utf8(),
            });
        }
        current = next;
    }
    if column == current || row.truncated {
        Some(row.end_byte)
    } else {
        None
    }
}

fn map_folds(
    old_snapshot: &DocumentSnapshot,
    new_snapshot: &DocumentSnapshot,
    folds: &FoldSet,
    changes: &mockaco_core::ChangeMap,
) -> FoldSet {
    let old_line_count = old_snapshot.position_map().line_count();
    let new_line_count = new_snapshot.position_map().line_count();
    let map_region = |region: &FoldRegion| {
        let start_byte = old_snapshot
            .position_map()
            .line_start(region.start_line)
            .ok()?;
        let end_byte = if region.end_line >= old_line_count {
            old_snapshot.len_bytes()
        } else {
            old_snapshot
                .position_map()
                .line_start(region.end_line)
                .ok()?
        };
        let new_start = changes.map_offset(start_byte, Affinity::After);
        let new_end = changes.map_offset(end_byte, Affinity::After);
        let start_line = new_snapshot.position_map().byte_to_line(new_start).ok()?;
        let end_line = if region.end_line >= old_line_count {
            new_line_count
        } else {
            new_snapshot.position_map().byte_to_line(new_end).ok()?
        };
        if start_line < end_line {
            Some(FoldRegion {
                start_line,
                end_line,
                placeholder: region.placeholder.clone(),
            })
        } else {
            None
        }
    };
    FoldSet::with_foldable(
        folds.regions().iter().filter_map(map_region),
        folds.foldable_regions().iter().filter_map(map_region),
    )
}


#[derive(Debug, Clone, Copy)]
struct Segment {
    start: usize,
    end: usize,
    width: usize,
    truncated: bool,
}

fn layout_line(
    text: &str,
    base: usize,
    wrap: Option<&WrapConfig>,
    max_line_scan_bytes: usize,
    default_tab_width: usize,
) -> Vec<Segment> {
    let Some(config) = wrap else {
        let (width, truncated) =
            measure_width(text, 0, text.len(), default_tab_width, max_line_scan_bytes);
        return vec![Segment {
            start: base,
            end: base + text.len(),
            width,
            truncated,
        }];
    };
    let mut segments = Vec::new();
    let mut segment_start = 0;
    let mut width = 0;
    let mut scanned: usize = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((offset, character)) = chars.next() {
        let next = offset + character.len_utf8();
        scanned = scanned.saturating_add(character.len_utf8());
        if scanned > config.max_line_scan_bytes {
            segments.push(Segment {
                start: base + segment_start,
                end: base + text.len(),
                width,
                truncated: true,
            });
            return segments;
        }
        let character_width = display_char_width(character, width, config.tab_width);
        if width > 0 && width.saturating_add(character_width) > config.width {
            segments.push(Segment {
                start: base + segment_start,
                end: base + offset,
                width,
                truncated: false,
            });
            segment_start = offset;
            width = 0;
            if segments.len() >= config.max_segments_per_line {
                segments.push(Segment {
                    start: base + segment_start,
                    end: base + text.len(),
                    width: 0,
                    truncated: true,
                });
                return segments;
            }
        }
        width = width.saturating_add(display_char_width(character, width, config.tab_width));
        if chars.peek().is_none() {
            segments.push(Segment {
                start: base + segment_start,
                end: base + next,
                width,
                truncated: false,
            });
        }
    }
    if segments.is_empty() {
        segments.push(Segment {
            start: base,
            end: base,
            width: 0,
            truncated: false,
        });
    }
    segments
}

fn measure_width(
    text: &str,
    start: usize,
    end: usize,
    tab_width: usize,
    scan_limit: usize,
) -> (usize, bool) {
    let mut width = 0;
    let mut scanned = 0;
    for character in text[start..end].chars() {
        scanned += character.len_utf8();
        if scanned > scan_limit {
            return (width, true);
        }
        width = width.saturating_add(display_char_width(character, width, tab_width));
    }
    (width, false)
}

fn display_char_width(character: char, current: usize, tab_width: usize) -> usize {
    if character == '\t' {
        tab_width - (current % tab_width)
    } else {
        1
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use mockaco_core::{
        Diagnostic, DiagnosticSet, DiagnosticSeverity, Document, SearchQuery, SearchSession,
        Transaction,
    };

    #[test]
    fn wrapping_and_buffer_display_mapping_are_stable() {
        let document = Document::new("abcdef\nxy");
        let map = DisplayMap::new(
            &document.snapshot(),
            DisplayConfig::wrapped(3).with_wrap(Some(WrapConfig::new(3).tab_width(4))),
        );
        assert_eq!(map.row_count(), 3);
        assert_eq!({ let row = map.row(0).unwrap(); row.start_byte..row.end_byte }, 0..3);
        assert_eq!({ let row = map.row(1).unwrap(); row.start_byte..row.end_byte }, 3..6);
        assert_eq!(
            map.buffer_to_display(4, Affinity::Before).unwrap(),
            DisplayPoint { row: 1, column: 1 }
        );
        assert_eq!(
            map.display_to_buffer(DisplayPoint { row: 1, column: 1 }, Affinity::Before)
                .unwrap()
                .byte_offset,
            4
        );
    }

    #[test]
    fn folds_hide_inner_lines_and_gutter_uses_one_based_numbers() {
        let document = Document::new("one\ntwo\nthree\nfour");
        let map = DisplayMap::with_folds(
            &document.snapshot(),
            DisplayConfig::unwrapped(),
            FoldSet::new([FoldRegion::new(0, 3).placeholder("...")]),
        );
        assert_eq!(map.row_count(), 2);
        assert_eq!(map.row(0).unwrap().buffer_line, 0);
        assert!(map.row(0).unwrap().folded);
        assert_eq!(map.row(1).unwrap().buffer_line, 3);
        assert_eq!(map.gutter(2).line_number_width, 2);
        assert_eq!(map.gutter(2).rows[1].line_number, 4);
    }

    #[test]
    fn viewport_bounds_are_clamped_without_panicking() {
        let document = Document::new("one\ntwo\nthree");
        let map = DisplayMap::new(&document.snapshot(), DisplayConfig::unwrapped());
        assert_eq!(map.visible_rows(DisplayViewport::new(1, 1)).len(), 1);
        assert_eq!(map.visible_rows(DisplayViewport::new(99, 5)).len(), 0);
        assert_eq!(DisplayViewport::new(usize::MAX, usize::MAX).rows(3), 3..3);
    }

    #[test]
    fn pathological_lines_are_bounded() {
        let document = Document::new("x".repeat(10_000));
        let map = DisplayMap::new(
            &document.snapshot(),
            DisplayConfig::wrapped(4).with_wrap(Some(
                WrapConfig::new(4)
                    .max_segments_per_line(8)
                    .max_line_scan_bytes(64),
            )),
        );
        assert!(map.row_count() <= 9);
        assert!(map.row(map.row_count() - 1).unwrap().truncated);
    }

    #[test]
    fn decorations_project_only_intersecting_display_rows() {
        let document = Document::new("abcdef");
        let map = DisplayMap::new(&document.snapshot(), DisplayConfig::wrapped(3));
        let projected = map.project_decorations(&[Decoration::new(1..5, 7)]);
        assert_eq!(projected.len(), 2);
        assert_eq!(projected[0].display_row, 0);
        assert_eq!(projected[0].start_column..projected[0].end_column, 1..3);
        assert_eq!(projected[1].display_row, 1);
        assert_eq!(projected[1].start_column..projected[1].end_column, 0..2);
    }

    #[test]
    fn ranged_paint_helpers_do_not_materialize_the_document_gutter() {
        let document = Document::new(
            (0..100)
                .map(|line| format!("line {line}\n"))
                .collect::<String>(),
        );
        let map = DisplayMap::new(&document.snapshot(), DisplayConfig::unwrapped());

        let gutter = map.gutter_for_display_range(4, 40..45);
        assert_eq!(gutter.rows.len(), 5);
        assert_eq!(gutter.rows.first().unwrap().display_row, 40);
        assert_eq!(gutter.rows.first().unwrap().line_number, 41);

        let projected = map.project_decorations_in_display_range(
            &[Decoration::new(0..document.len_bytes(), 7)],
            40..45,
        );
        assert_eq!(projected.len(), 5);
        assert!(projected
            .iter()
            .all(|decoration| (40..45).contains(&decoration.display_row)));
    }

    #[test]
    fn search_and_diagnostic_seams_preserve_source_ranges_and_styles() {
        let document = Document::new("one two one");
        let mut search = SearchSession::new(&document.snapshot(), SearchQuery::new("one"));
        search.next_match();
        let search_decorations = search_decorations(&search);
        assert_eq!(search_decorations.len(), 2);
        assert_eq!(
            search_decorations[0].style.id,
            SEARCH_CURRENT_MATCH_STYLE_ID
        );
        assert_eq!(search_decorations[1].style.id, SEARCH_MATCH_STYLE_ID);

        let mut diagnostics = DiagnosticSet::new(document.version());
        diagnostics
            .publish(
                &document.snapshot(),
                [Diagnostic::new(4..7, DiagnosticSeverity::Error, "error")],
            )
            .unwrap();
        let diagnostic_decorations = diagnostic_decorations(&diagnostics);
        assert_eq!(diagnostic_decorations[0].range, 4..7);
        assert_eq!(
            diagnostic_decorations[0].style.id,
            DIAGNOSTIC_ERROR_STYLE_ID
        );
    }

    #[test]
    fn transaction_updates_only_changed_rows_when_unfolded() {
        let mut document = Document::new("zero\none\ntwo\nthree");
        let initial = document.snapshot();
        let mut map = DisplayMap::new(&initial, DisplayConfig::unwrapped());
        let applied = document
            .apply(&Transaction::new().replace(5..8, "ONE"))
            .unwrap();
        let update = map.update(&document.snapshot(), &applied).unwrap();
        assert!(!update.full_rebuild);
        assert_eq!(update.reused_prefix_rows, 1);
        assert_eq!(update.reused_suffix_rows, 2);
        assert_eq!({ let row = map.row(1).unwrap(); row.start_byte..row.end_byte }, 5..8);
    }

    #[test]
    fn folded_ranges_follow_line_edits() {
        let mut document = Document::new("start\ninside\nend\nafter");
        let initial = document.snapshot();
        let mut map = DisplayMap::with_folds(
            &initial,
            DisplayConfig::unwrapped(),
            FoldSet::new([FoldRegion::new(0, 3)]),
        );
        let applied = document
            .apply(&Transaction::new().insert(0, "new\n"))
            .unwrap();
        map.update(&document.snapshot(), &applied).unwrap();
        assert_eq!(map.folds().regions()[0].start_line, 1);
        assert_eq!(map.folds().regions()[0].end_line, 4);
        assert_eq!(map.row(0).unwrap().buffer_line, 0);
        assert_eq!(map.row(1).unwrap().buffer_line, 1);
    }

    #[test]
    fn stale_transaction_is_rejected() {
        let mut document = Document::new("abc");
        let initial = document.snapshot();
        let mut map = DisplayMap::new(&initial, DisplayConfig::unwrapped());
        let applied = document.apply(&Transaction::new().insert(1, "x")).unwrap();
        let newer = document.apply(&Transaction::new().insert(2, "y")).unwrap();
        assert!(matches!(
            map.update(&document.snapshot(), &applied),
            Err(DisplayMapError::SnapshotVersion { .. })
        ));
        let _ = newer;
    }
}
