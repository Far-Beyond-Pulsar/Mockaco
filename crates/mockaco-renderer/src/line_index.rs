//! Per-line display metrics with cheap splicing.
//!
//! Lines are kept in chunks of about [`TARGET`] entries, each chunk carrying
//! its first line and first display row. An edit replaces the entries of the
//! lines it touched inside the chunks holding them and refreshes the chunk
//! prefixes: O(edit + chunk size + line count / chunk size). At ten million
//! lines that is ~10k chunk prefixes, a few microseconds, so an edit costs the
//! same regardless of where it lands or how long the document is. Lookups are
//! a binary search over chunks plus a scan inside one chunk.

use std::collections::BTreeMap;
use std::ops::Range;

/// What the display map keeps about one buffer line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct LineInfo {
    /// Display rows the line occupies when shown: 1, or its wrap segments.
    pub rows: u32,
    /// Display width (columns, tabs expanded), up to the scan limit.
    pub width: u32,
}

const TARGET: usize = 1024;
const MAX: usize = 2 * TARGET;

#[derive(Debug, Clone, Default)]
pub(crate) struct LineIndex {
    chunks: Vec<Vec<LineInfo>>,
    /// First line of each chunk.
    line_starts: Vec<usize>,
    /// Display rows in each chunk.
    chunk_rows: Vec<usize>,
    /// First display row of each chunk (folds aside).
    row_starts: Vec<usize>,
    lines: usize,
    rows: usize,
    /// How many lines have each width, for the widest line in O(log n).
    widths: BTreeMap<u32, u32>,
}

impl LineIndex {
    pub(crate) fn new(infos: impl IntoIterator<Item = LineInfo>) -> Self {
        let mut index = Self::default();
        let mut chunk = Vec::with_capacity(TARGET);
        for info in infos {
            *index.widths.entry(info.width).or_insert(0) += 1;
            chunk.push(info);
            if chunk.len() == TARGET {
                index.chunks.push(std::mem::replace(
                    &mut chunk,
                    Vec::with_capacity(TARGET),
                ));
            }
        }
        if !chunk.is_empty() || index.chunks.is_empty() {
            index.chunks.push(chunk);
        }
        index.recount_all();
        index.refresh_prefixes();
        index
    }

    pub(crate) fn line_count(&self) -> usize {
        self.lines
    }

    pub(crate) fn row_count(&self) -> usize {
        self.rows
    }

    pub(crate) fn max_width(&self) -> usize {
        self.widths
            .last_key_value()
            .map_or(0, |(width, _)| *width as usize)
    }

    fn chunk_of_line(&self, line: usize) -> usize {
        self.line_starts
            .partition_point(|&start| start <= line)
            .saturating_sub(1)
    }

    #[cfg(test)]
    pub(crate) fn get(&self, line: usize) -> Option<LineInfo> {
        if line >= self.lines {
            return None;
        }
        let chunk = self.chunk_of_line(line);
        self.chunks[chunk]
            .get(line - self.line_starts[chunk])
            .copied()
    }

    /// Display rows of all lines before `line` (folds aside).
    pub(crate) fn rows_before(&self, line: usize) -> usize {
        if line >= self.lines {
            return self.rows;
        }
        let chunk = self.chunk_of_line(line);
        let within = line - self.line_starts[chunk];
        self.row_starts[chunk]
            + self.chunks[chunk][..within]
                .iter()
                .map(|info| info.rows as usize)
                .sum::<usize>()
    }

    /// The line holding display row `row` (folds aside) and the row's index
    /// within that line.
    pub(crate) fn row_to_line(&self, row: usize) -> Option<(usize, usize)> {
        if row >= self.rows {
            return None;
        }
        let chunk = self
            .row_starts
            .partition_point(|&start| start <= row)
            .saturating_sub(1);
        let mut at = self.row_starts[chunk];
        for (offset, info) in self.chunks[chunk].iter().enumerate() {
            let rows = info.rows as usize;
            if row < at + rows {
                return Some((self.line_starts[chunk] + offset, row - at));
            }
            at += rows;
        }
        None
    }

    /// Replaces the entries of `lines` with `infos`.
    pub(crate) fn splice(&mut self, lines: Range<usize>, infos: Vec<LineInfo>) {
        let start = lines.start.min(self.lines);
        let end = lines.end.clamp(start, self.lines);
        let first = self.chunk_of_line(start);
        // The chunk holding the last replaced line (or the first chunk when
        // nothing is removed).
        let last = if end > start {
            self.chunk_of_line(end - 1)
        } else {
            first
        };
        let local_start = start - self.line_starts[first];
        let local_end = end - self.line_starts[last];

        for info in &self.chunks[first][local_start..]
            .iter()
            .chain(self.chunks[first + 1..=last].iter().flatten())
            .take(end - start)
            .copied()
            .collect::<Vec<_>>()
        {
            self.remove_width(info.width);
        }
        for info in &infos {
            *self.widths.entry(info.width).or_insert(0) += 1;
        }

        let mut merged = Vec::with_capacity(local_start + infos.len() + TARGET);
        merged.extend_from_slice(&self.chunks[first][..local_start]);
        merged.extend(infos);
        merged.extend_from_slice(&self.chunks[last][local_end..]);
        let replacement: Vec<Vec<LineInfo>> = if merged.len() > MAX {
            merged.chunks(TARGET).map(<[LineInfo]>::to_vec).collect()
        } else {
            vec![merged]
        };
        // An edit that empties its chunks leaves none behind.
        let replacement: Vec<Vec<LineInfo>> = replacement
            .into_iter()
            .filter(|chunk| !chunk.is_empty())
            .collect();
        let replacement_rows: Vec<usize> = replacement.iter().map(|chunk| rows_of(chunk)).collect();
        self.chunks.splice(first..=last, replacement);
        self.chunk_rows.splice(first..=last, replacement_rows);
        if self.chunks.is_empty() {
            self.chunks.push(Vec::new());
            self.chunk_rows.push(0);
        }
        self.refresh_prefixes();
    }

    fn remove_width(&mut self, width: u32) {
        if let Some(count) = self.widths.get_mut(&width) {
            *count -= 1;
            if *count == 0 {
                self.widths.remove(&width);
            }
        }
    }

    /// Recounts the rows of every chunk; for building only.
    fn recount_all(&mut self) {
        self.chunk_rows = self.chunks.iter().map(|chunk| rows_of(chunk)).collect();
    }

    /// Rebuilds the chunk prefixes from the cached per-chunk counts:
    /// O(chunks), never O(lines).
    fn refresh_prefixes(&mut self) {
        self.line_starts.clear();
        self.row_starts.clear();
        let (mut lines, mut rows) = (0, 0);
        for (chunk, chunk_rows) in self.chunks.iter().zip(&self.chunk_rows) {
            self.line_starts.push(lines);
            self.row_starts.push(rows);
            lines += chunk.len();
            rows += chunk_rows;
        }
        self.lines = lines;
        self.rows = rows;
    }
}

fn rows_of(chunk: &[LineInfo]) -> usize {
    chunk.iter().map(|info| info.rows as usize).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(rows: u32, width: u32) -> LineInfo {
        LineInfo { rows, width }
    }

    #[test]
    fn lookups_span_chunks() {
        let index = LineIndex::new((0..5000).map(|n| info(1 + (n % 2) as u32, n as u32)));
        assert_eq!(index.line_count(), 5000);
        assert_eq!(index.row_count(), 7500);
        assert_eq!(index.rows_before(3000), 4500);
        assert_eq!(index.row_to_line(4500), Some((3000, 0)));
        assert_eq!(index.row_to_line(4502), Some((3001, 1)));
        assert_eq!(index.max_width(), 4999);
    }

    #[test]
    fn splices_insert_remove_and_cross_chunks() {
        let mut index = LineIndex::new((0..3000).map(|_| info(1, 10)));
        index.splice(1000..1000, vec![info(3, 99); 5]);
        assert_eq!(index.line_count(), 3005);
        assert_eq!(index.row_count(), 3015);
        assert_eq!(index.max_width(), 99);
        assert_eq!(index.get(1002), Some(info(3, 99)));
        index.splice(1000..1005, Vec::new());
        assert_eq!(index.max_width(), 10);
        // Across a chunk boundary.
        index.splice(1020..1030, vec![info(1, 5)]);
        assert_eq!(index.line_count(), 2991);
        assert_eq!(index.rows_before(2990), 2990);
        // Everything.
        let lines = index.line_count();
        index.splice(0..lines, vec![info(1, 0)]);
        assert_eq!((index.line_count(), index.row_count()), (1, 1));
        index.splice(1..1, vec![info(1, 7); 3000]);
        assert_eq!(index.line_count(), 3001);
        assert_eq!(index.row_to_line(3000), Some((3000, 0)));
    }
}
