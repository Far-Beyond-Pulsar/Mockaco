//! Hunk-by-hunk review of a proposed rewrite (an AI edit, a refactor).
//!
//! The proposed text is applied to the buffer up front, so what the user sees
//! is what they would get by accepting everything. The review remembers, for
//! each changed region, the lines the proposal replaced. Accepting a hunk
//! only forgets it; rejecting one yields the edit that puts the original
//! lines back. The model works on lines and never touches a buffer itself:
//! hosts apply the returned [`ReviewEdit`]s as ordinary transactions.

use crate::compute_diff;
use mockaco_core::{Document, DocumentSnapshot};
use std::ops::Range;

/// One changed region still awaiting a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewHunk {
    pub id: u64,
    /// The lines of the base text the proposal replaced.
    pub original: Range<usize>,
    /// The original lines the proposal replaced (empty for a pure addition).
    pub removed: Vec<String>,
    /// Where the replacement sits in the buffer now, as lines (empty for a
    /// pure deletion, in which case `start` is where the lines were).
    pub added: Range<usize>,
}

/// A byte-range replacement against the buffer the review was last told about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewEdit {
    pub range: Range<usize>,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Review {
    hunks: Vec<ReviewHunk>,
    eol: &'static str,
}

impl Review {
    /// Reviews `proposed` against `base`. No hunks when they are equal.
    pub fn new(base: &str, proposed: &str) -> Self {
        let original = Document::new(base).snapshot();
        let modified = Document::new(proposed).snapshot();
        let diff = compute_diff(&original, &modified);
        let base_lines = lines_of(&original);
        let hunks = diff
            .hunks()
            .iter()
            .enumerate()
            .map(|(index, hunk)| ReviewHunk {
                id: index as u64,
                original: hunk.original_range.clone(),
                removed: base_lines[hunk.original_range.clone()].to_vec(),
                added: hunk.modified_range.clone(),
            })
            .collect();
        Self {
            hunks,
            eol: if proposed.contains("\r\n") { "\r\n" } else { "\n" },
        }
    }

    /// Hunks not yet decided, top to bottom.
    pub fn pending(&self) -> &[ReviewHunk] {
        &self.hunks
    }

    pub fn is_done(&self) -> bool {
        self.hunks.is_empty()
    }

    /// Keeps the proposal's version of hunk `id`. The buffer is unchanged.
    pub fn accept(&mut self, id: u64) -> bool {
        let before = self.hunks.len();
        self.hunks.retain(|hunk| hunk.id != id);
        self.hunks.len() != before
    }

    /// The edits that turn `base` into `proposed` (the two texts passed to
    /// [`Review::new`]), one per hunk and non-overlapping. Call before any
    /// decision is made: apply them as one transaction to put the proposal in
    /// the buffer.
    pub fn forward_edits(&self, base: &DocumentSnapshot, proposed: &DocumentSnapshot) -> Vec<ReviewEdit> {
        let proposed_lines = lines_of(proposed);
        self.hunks
            .iter()
            .map(|hunk| {
                let end = hunk.added.end.min(proposed_lines.len());
                let start = hunk.added.start.min(end);
                replace_lines(base, hunk.original.clone(), &proposed_lines[start..end], self.eol)
            })
            .collect()
    }

    pub fn accept_all(&mut self) {
        self.hunks.clear();
    }

    /// Puts hunk `id`'s original lines back. `buffer` is the current text;
    /// apply the returned edit to it. Later hunks shift to match.
    pub fn reject(&mut self, id: u64, buffer: &DocumentSnapshot) -> Option<ReviewEdit> {
        let index = self.hunks.iter().position(|hunk| hunk.id == id)?;
        let hunk = self.hunks.remove(index);
        let edit = replace_lines(buffer, hunk.added.clone(), &hunk.removed, self.eol);
        let delta = hunk.removed.len() as isize - hunk.added.len() as isize;
        for later in &mut self.hunks[index..] {
            later.added = shift(later.added.start, delta)..shift(later.added.end, delta);
        }
        Some(edit)
    }

    /// Every pending hunk's reject edit, for one atomic transaction, and the
    /// review is finished. Edits do not overlap.
    pub fn reject_all(&mut self, buffer: &DocumentSnapshot) -> Vec<ReviewEdit> {
        let eol = self.eol;
        self.hunks
            .drain(..)
            .map(|hunk| replace_lines(buffer, hunk.added, &hunk.removed, eol))
            .collect()
    }
}

fn shift(line: usize, delta: isize) -> usize {
    (line as isize + delta).max(0) as usize
}

fn lines_of(snapshot: &DocumentSnapshot) -> Vec<String> {
    // An empty document has no lines in the diff model.
    if snapshot.text().is_empty() {
        return Vec::new();
    }
    snapshot
        .text()
        .lines()
        .map(|line| line.into_owned())
        .collect()
}

/// The edit replacing lines `range` of `buffer` with `replacement`.
fn replace_lines(
    buffer: &DocumentSnapshot,
    range: Range<usize>,
    replacement: &[String],
    eol: &str,
) -> ReviewEdit {
    let text = buffer.text();
    if text.is_empty() {
        return ReviewEdit {
            range: 0..0,
            text: replacement.join(eol),
        };
    }
    let line_count = text.line_count();
    let start = range.start.min(line_count);
    let end = range.end.min(line_count);
    let joined = replacement.join(eol);
    if end < line_count {
        // Whole lines in the middle: swap them, keeping each one's terminator.
        let from = text.line_start(start).unwrap_or(text.len());
        let to = text.line_start(end).unwrap_or(text.len());
        let mut out = joined;
        if !replacement.is_empty() {
            out.push_str(eol);
        }
        return ReviewEdit {
            range: from..to,
            text: out,
        };
    }
    // Through the last line, which has no terminator of its own.
    if start == 0 {
        return ReviewEdit {
            range: 0..text.len(),
            text: joined,
        };
    }
    let from = if start >= line_count {
        text.len()
    } else {
        text.line_end(start - 1).unwrap_or(text.len())
    };
    let out = if replacement.is_empty() {
        String::new()
    } else {
        format!("{eol}{joined}")
    };
    ReviewEdit {
        range: from..text.len(),
        text: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str, edits: &[ReviewEdit]) -> String {
        let mut edits = edits.to_vec();
        edits.sort_by_key(|edit| std::cmp::Reverse(edit.range.start));
        let mut out = text.to_owned();
        for edit in edits {
            out.replace_range(edit.range, &edit.text);
        }
        out
    }

    fn snap(text: &str) -> DocumentSnapshot {
        Document::new(text).snapshot()
    }

    #[test]
    fn equal_texts_have_nothing_to_review() {
        assert!(Review::new("a\nb\n", "a\nb\n").is_done());
    }

    #[test]
    fn rejecting_every_hunk_restores_the_base() {
        let base = "one\ntwo\nthree\nfour\nfive\n";
        let proposed = "zero\none\nTWO\nthree\nfive\nsix\n";
        let mut review = Review::new(base, proposed);
        assert!(review.pending().len() >= 3);
        let edits = review.reject_all(&snap(proposed));
        assert_eq!(apply(proposed, &edits), base);
        assert!(review.is_done());
    }

    #[test]
    fn rejecting_one_by_one_tracks_shifting_lines() {
        let base = "a\nb\nc\nd\ne\nf\n";
        let proposed = "a\nB1\nB2\nc\nd\nF\n";
        let mut review = Review::new(base, proposed);
        let mut buffer = proposed.to_owned();
        // Reject from the top; every later hunk must still land correctly.
        while let Some(id) = review.pending().first().map(|hunk| hunk.id) {
            let edit = review.reject(id, &snap(&buffer)).unwrap();
            buffer = apply(&buffer, &[edit]);
        }
        assert_eq!(buffer, base);
    }

    #[test]
    fn accepting_keeps_text_and_mixed_decisions_compose() {
        let base = "a\nb\nc\nd\n";
        let proposed = "A\nb\nc\nD\n";
        let mut review = Review::new(base, proposed);
        assert_eq!(review.pending().len(), 2);
        let first = review.pending()[0].id;
        let second = review.pending()[1].id;
        assert!(review.accept(first));
        let edit = review.reject(second, &snap(proposed)).unwrap();
        assert_eq!(apply(proposed, &[edit]), "A\nb\nc\nd\n");
        assert!(review.is_done());
    }

    #[test]
    fn pure_deletions_and_additions_at_the_edges_round_trip() {
        for (base, proposed) in [
            ("a\nb\nc", "b\nc"),
            ("a\nb\nc", "a\nb"),
            ("a\nb", "x\na\nb"),
            ("a\nb", "a\nb\nx"),
            ("", "x"),
            ("x", ""),
            ("a\r\nb\r\n", "a\r\nB\r\n"),
        ] {
            let mut review = Review::new(base, proposed);
            let edits = review.reject_all(&snap(proposed));
            assert_eq!(apply(proposed, &edits), base, "{base:?} -> {proposed:?}");
        }
    }

    #[test]
    fn forward_edits_produce_the_proposal() {
        for (base, proposed) in [
            ("one\ntwo\nthree\nfour\nfive\n", "zero\none\nTWO\nthree\nfive\nsix\n"),
            ("a\nb\nc", "b\nc"),
            ("a\nb", "x\na\nb"),
            ("a\nb", "a\nb\nx"),
            ("", "x"),
            ("x", ""),
            ("a\r\nb\r\n", "a\r\nB\r\n"),
        ] {
            let review = Review::new(base, proposed);
            let edits = review.forward_edits(&snap(base), &snap(proposed));
            assert_eq!(apply(base, &edits), proposed, "{base:?} -> {proposed:?}");
        }
    }
}
