//! Renderer-neutral display mapping for Mockaco.
//!
//! This crate owns buffer-to-display layout only. It has no knowledge of a UI
//! framework, glyph library, filesystem, language parser, or host application.

use mockaco_core::{Affinity, AppliedTransaction, DocumentSnapshot, PositionError};
use std::fmt;
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
}

impl FoldSet {
    pub fn new(regions: impl IntoIterator<Item = FoldRegion>) -> Self {
        let mut regions: Vec<_> = regions
            .into_iter()
            .filter(|region| region.start_line < region.end_line)
            .collect();
        regions.sort_by_key(|region| (region.start_line, region.end_line));
        regions.dedup_by_key(|region| (region.start_line, region.end_line));
        Self { regions }
    }

    pub fn regions(&self) -> &[FoldRegion] {
        &self.regions
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    pub fn region_starting_at(&self, line: usize) -> Option<&FoldRegion> {
        self.regions.iter().find(|region| region.start_line == line)
    }

    pub fn hidden_by(&self, line: usize) -> Option<&FoldRegion> {
        self.regions.iter().find(|region| region.hides_line(line))
    }
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
#[derive(Debug, Clone)]
pub struct DisplayMap {
    snapshot: DocumentSnapshot,
    config: DisplayConfig,
    folds: FoldSet,
    rows: Vec<DisplayRow>,
    revision: u64,
    last_update: Option<DisplayUpdate>,
}

impl DisplayMap {
    pub fn new(snapshot: &DocumentSnapshot, config: DisplayConfig) -> Self {
        let rows = build_rows(
            snapshot,
            &config,
            &FoldSet::default(),
            0,
            snapshot.position_map().line_count(),
        );
        Self {
            snapshot: snapshot.clone(),
            config,
            folds: FoldSet::default(),
            rows,
            revision: 0,
            last_update: None,
        }
    }

    pub fn with_folds(snapshot: &DocumentSnapshot, config: DisplayConfig, folds: FoldSet) -> Self {
        let rows = build_rows(
            snapshot,
            &config,
            &folds,
            0,
            snapshot.position_map().line_count(),
        );
        Self {
            snapshot: snapshot.clone(),
            config,
            folds,
            rows,
            revision: 0,
            last_update: None,
        }
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

    pub fn rows(&self) -> &[DisplayRow] {
        &self.rows
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn last_update(&self) -> Option<&DisplayUpdate> {
        self.last_update.as_ref()
    }

    pub fn set_folds(&mut self, folds: FoldSet) {
        self.folds = folds;
        self.rows = build_rows(
            &self.snapshot,
            &self.config,
            &self.folds,
            0,
            self.snapshot.position_map().line_count(),
        );
        self.revision = self.revision.saturating_add(1);
        self.last_update = None;
    }

    pub fn visible_rows(&self, viewport: DisplayViewport) -> &[DisplayRow] {
        let range = viewport.rows(self.rows.len());
        &self.rows[range]
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
        if transaction.change_map.edits().is_empty() {
            self.snapshot = snapshot.clone();
            let update = DisplayUpdate {
                old_range: 0..0,
                new_range: 0..0,
                reused_prefix_rows: self.rows.len(),
                reused_suffix_rows: 0,
                recomputed_rows: 0,
                full_rebuild: false,
                revision: self.revision,
            };
            self.last_update = Some(update.clone());
            return Ok(update);
        }

        let old_start = transaction
            .change_map
            .edits()
            .iter()
            .map(|edit| edit.range.start)
            .min()
            .unwrap();
        let old_end = transaction
            .change_map
            .edits()
            .iter()
            .map(|edit| edit.range.end)
            .max()
            .unwrap();
        let old_range = old_start..old_end;
        let new_range = transaction.change_map.map_range(old_range.clone());

        if !self.folds.is_empty() {
            self.folds = map_folds(
                &self.snapshot,
                snapshot,
                &self.folds,
                &transaction.change_map,
            );
            self.snapshot = snapshot.clone();
            self.rows = build_rows(
                snapshot,
                &self.config,
                &self.folds,
                0,
                snapshot.position_map().line_count(),
            );
            self.revision = self.revision.saturating_add(1);
            let update = DisplayUpdate {
                old_range,
                new_range,
                reused_prefix_rows: 0,
                reused_suffix_rows: 0,
                recomputed_rows: self.rows.len(),
                full_rebuild: true,
                revision: self.revision,
            };
            self.last_update = Some(update.clone());
            return Ok(update);
        }

        let old_first_line = self.snapshot.position_map().byte_to_line(old_start)?;
        let old_last_line = self.snapshot.position_map().byte_to_line(old_end)?;
        let new_first_line = snapshot.position_map().byte_to_line(new_range.start)?;
        let new_last_line = snapshot.position_map().byte_to_line(new_range.end)?;
        let replacement = build_rows(
            snapshot,
            &self.config,
            &self.folds,
            new_first_line,
            new_last_line.saturating_add(1),
        );

        let old_rows = std::mem::take(&mut self.rows);
        let prefix: Vec<_> = old_rows
            .iter()
            .filter(|row| row.buffer_line < old_first_line)
            .cloned()
            .collect();
        let suffix: Vec<_> = old_rows
            .iter()
            .filter(|row| row.buffer_line > old_last_line)
            .map(|row| remap_row(row, snapshot, &transaction.change_map))
            .collect::<Result<_, _>>()?;
        let reused_prefix_rows = prefix.len();
        let reused_suffix_rows = suffix.len();
        let recomputed_rows = replacement.len();
        self.rows = prefix;
        self.rows.extend(replacement);
        self.rows.extend(suffix);
        self.snapshot = snapshot.clone();
        self.revision = self.revision.saturating_add(1);
        let update = DisplayUpdate {
            old_range,
            new_range,
            reused_prefix_rows,
            reused_suffix_rows,
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
            .rows
            .get(point.row)
            .ok_or(DisplayMapError::RowOutOfBounds(point.row))?;
        let text = self.snapshot.text();
        let byte = display_column_to_byte(text, row, point.column, affinity, self.config.tab_width)
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
        if byte > self.snapshot.len_bytes() || !self.snapshot.text().is_char_boundary(byte) {
            return Err(DisplayMapError::Position(
                PositionError::InvalidUtf8Boundary(byte),
            ));
        }
        for (index, row) in self.rows.iter().enumerate() {
            if byte < row.start_byte || byte > row.end_byte {
                continue;
            }
            if byte == row.end_byte && affinity == Affinity::After {
                if let Some(next) = self.rows.get(index + 1) {
                    if next.buffer_line == row.buffer_line {
                        continue;
                    }
                }
            }
            let column =
                byte_to_display_column(self.snapshot.text(), row, byte, self.config.tab_width);
            return Ok(DisplayPoint { row: index, column });
        }
        let line = self.snapshot.position_map().byte_to_line(byte)?;
        if let Some(fold) = self.folds.hidden_by(line) {
            if let Some(row) = self
                .rows
                .iter()
                .position(|row| row.buffer_line == fold.start_line)
            {
                return Ok(DisplayPoint {
                    row,
                    column: self.rows[row].display_width,
                });
            }
        }
        Err(DisplayMapError::InvalidDisplayColumn {
            row: 0,
            column: byte,
        })
    }

    pub fn gutter(&self, min_width: usize) -> GutterLayout {
        let max_line = self
            .rows
            .iter()
            .map(|row| row.buffer_line + 1)
            .max()
            .unwrap_or(1);
        let line_number_width = min_width.max(max_line.to_string().len());
        let rows = self
            .rows
            .iter()
            .enumerate()
            .map(|(display_row, row)| GutterRow {
                display_row,
                buffer_line: row.buffer_line,
                line_number: row.buffer_line + 1,
                continuation: row.continuation,
                folded: row.folded,
            })
            .collect();
        GutterLayout {
            line_number_width,
            rows,
        }
    }

    pub fn project_decorations(&self, decorations: &[Decoration]) -> Vec<ProjectedDecoration> {
        let mut projected = Vec::new();
        for (display_row, row) in self.rows.iter().enumerate() {
            for decoration in decorations {
                let start = decoration.range.start.max(row.start_byte);
                let end = decoration.range.end.min(row.end_byte);
                let overlaps = start < end
                    || (start == end
                        && decoration.range.start == decoration.range.end
                        && start >= row.start_byte
                        && start <= row.end_byte);
                if overlaps {
                    projected.push(ProjectedDecoration {
                        display_row,
                        start_column: byte_to_display_column(
                            self.snapshot.text(),
                            row,
                            start,
                            self.config.tab_width,
                        ),
                        end_column: byte_to_display_column(
                            self.snapshot.text(),
                            row,
                            end,
                            self.config.tab_width,
                        ),
                        source_range: start..end,
                        style: decoration.style,
                    });
                }
            }
        }
        projected
    }
}

fn remap_row(
    row: &DisplayRow,
    snapshot: &DocumentSnapshot,
    changes: &mockaco_core::ChangeMap,
) -> Result<DisplayRow, DisplayMapError> {
    let start = changes.map_offset(row.start_byte, Affinity::Before);
    let end = changes.map_offset(row.end_byte, Affinity::After);
    let line = snapshot.position_map().byte_to_line(start)?;
    Ok(DisplayRow {
        buffer_line: line,
        start_byte: start,
        end_byte: end,
        ..row.clone()
    })
}

fn map_folds(
    old_snapshot: &DocumentSnapshot,
    new_snapshot: &DocumentSnapshot,
    folds: &FoldSet,
    changes: &mockaco_core::ChangeMap,
) -> FoldSet {
    let old_line_count = old_snapshot.position_map().line_count();
    let new_line_count = new_snapshot.position_map().line_count();
    FoldSet::new(folds.regions().iter().filter_map(|region| {
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
    }))
}

fn build_rows(
    snapshot: &DocumentSnapshot,
    config: &DisplayConfig,
    folds: &FoldSet,
    first_line: usize,
    end_line_exclusive: usize,
) -> Vec<DisplayRow> {
    let line_count = snapshot.position_map().line_count();
    let first_line = first_line.min(line_count);
    let end_line_exclusive = end_line_exclusive.min(line_count);
    let mut rows = Vec::new();
    let mut line = first_line;
    while line < end_line_exclusive {
        if folds.hidden_by(line).is_some() {
            line += 1;
            continue;
        }
        let start = snapshot.position_map().line_start(line).unwrap();
        let end = snapshot.position_map().line_end(line).unwrap();
        let folded = folds.region_starting_at(line).is_some();
        let segments = layout_line(
            &snapshot.text()[start..end],
            start,
            config.wrap.as_ref(),
            config.max_line_scan_bytes,
            config.tab_width,
        );
        for (index, segment) in segments.into_iter().enumerate() {
            rows.push(DisplayRow {
                buffer_line: line,
                start_byte: segment.start,
                end_byte: segment.end,
                continuation: index > 0,
                folded,
                truncated: segment.truncated,
                display_width: segment.width,
            });
        }
        line += 1;
    }
    rows
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

fn byte_to_display_column(text: &str, row: &DisplayRow, byte: usize, tab_width: usize) -> usize {
    let end = byte.min(row.end_byte);
    let mut column: usize = 0;
    for character in text[row.start_byte..end].chars() {
        column = column.saturating_add(display_char_width(character, column, tab_width));
    }
    column
}

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
    for (offset, character) in text[row.start_byte..row.end_byte].char_indices() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use mockaco_core::{Document, Transaction};

    #[test]
    fn wrapping_and_buffer_display_mapping_are_stable() {
        let document = Document::new("abcdef\nxy");
        let map = DisplayMap::new(
            &document.snapshot(),
            DisplayConfig::wrapped(3).with_wrap(Some(WrapConfig::new(3).tab_width(4))),
        );
        assert_eq!(map.row_count(), 3);
        assert_eq!(map.rows()[0].start_byte..map.rows()[0].end_byte, 0..3);
        assert_eq!(map.rows()[1].start_byte..map.rows()[1].end_byte, 3..6);
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
        assert_eq!(map.rows()[0].buffer_line, 0);
        assert!(map.rows()[0].folded);
        assert_eq!(map.rows()[1].buffer_line, 3);
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
        assert!(map.rows().last().unwrap().truncated);
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
        assert_eq!(map.rows()[1].start_byte..map.rows()[1].end_byte, 5..8);
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
        assert_eq!(map.rows()[0].buffer_line, 0);
        assert_eq!(map.rows()[1].buffer_line, 1);
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
