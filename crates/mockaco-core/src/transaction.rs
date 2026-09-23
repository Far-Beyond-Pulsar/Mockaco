use crate::{Document, DocumentError, DocumentSnapshot};
use std::fmt;
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Affinity {
    Before,
    After,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub replacement: String,
}

impl Edit {
    pub fn insert(offset: usize, text: impl Into<String>) -> Self {
        Self {
            range: offset..offset,
            replacement: text.into(),
        }
    }

    pub fn delete(range: Range<usize>) -> Self {
        Self {
            range,
            replacement: String::new(),
        }
    }

    pub fn replace(range: Range<usize>, text: impl Into<String>) -> Self {
        Self {
            range,
            replacement: text.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    InvalidUtf8,
    OutOfBounds {
        range: Range<usize>,
        len: usize,
    },
    NotCharBoundary(usize),
    Overlapping {
        previous: Range<usize>,
        next: Range<usize>,
    },
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUtf8 => f.write_str("replacement is not valid UTF-8"),
            Self::OutOfBounds { range, len } => {
                write!(f, "edit range {range:?} is outside document length {len}")
            }
            Self::NotCharBoundary(offset) => write!(f, "offset {offset} is not a UTF-8 boundary"),
            Self::Overlapping { previous, next } => {
                write!(f, "edit ranges {previous:?} and {next:?} overlap")
            }
        }
    }
}

impl std::error::Error for EditError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionError {
    Invalid(EditError),
    Document(DocumentError),
}

impl fmt::Display for TransactionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(error) => error.fmt(f),
            Self::Document(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for TransactionError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    edits: Vec<Edit>,
}

impl Transaction {
    pub fn new() -> Self {
        Self { edits: Vec::new() }
    }

    pub fn from_edits(edits: impl IntoIterator<Item = Edit>) -> Self {
        Self {
            edits: edits.into_iter().collect(),
        }
    }

    pub fn insert(mut self, offset: usize, text: impl Into<String>) -> Self {
        self.edits.push(Edit::insert(offset, text));
        self
    }

    pub fn delete(mut self, range: Range<usize>) -> Self {
        self.edits.push(Edit::delete(range));
        self
    }

    pub fn replace(mut self, range: Range<usize>, text: impl Into<String>) -> Self {
        self.edits.push(Edit::replace(range, text));
        self
    }

    pub fn push(&mut self, edit: Edit) {
        self.edits.push(edit);
    }

    pub fn edits(&self) -> &[Edit] {
        &self.edits
    }

    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    pub fn validate(&self, source: &str) -> Result<Vec<Edit>, EditError> {
        let mut edits = self.edits.clone();
        edits.sort_by_key(|edit| edit.range.start);
        let len = source.len();
        let mut previous: Option<Range<usize>> = None;
        for edit in &edits {
            if edit.range.start > edit.range.end || edit.range.end > len {
                return Err(EditError::OutOfBounds {
                    range: edit.range.clone(),
                    len,
                });
            }
            if !source.is_char_boundary(edit.range.start) {
                return Err(EditError::NotCharBoundary(edit.range.start));
            }
            if !source.is_char_boundary(edit.range.end) {
                return Err(EditError::NotCharBoundary(edit.range.end));
            }
            if !edit.replacement.is_char_boundary(edit.replacement.len()) {
                return Err(EditError::InvalidUtf8);
            }
            if let Some(previous) = &previous {
                let previous_is_insert = previous.start == previous.end;
                let current_is_insert = edit.range.start == edit.range.end;
                let overlaps = edit.range.start < previous.end
                    || (previous_is_insert
                        && !current_is_insert
                        && edit.range.start == previous.start)
                    || (!previous_is_insert
                        && current_is_insert
                        && edit.range.start == previous.start);
                if overlaps {
                    return Err(EditError::Overlapping {
                        previous: previous.clone(),
                        next: edit.range.clone(),
                    });
                }
            }
            previous = Some(edit.range.clone());
        }
        Ok(edits)
    }

    pub fn apply(
        &self,
        source: &str,
        version: u64,
    ) -> Result<AppliedTransaction, TransactionError> {
        let edits = self.validate(source).map_err(TransactionError::Invalid)?;
        let mut text = String::with_capacity(source.len());
        let mut inverse = Vec::with_capacity(edits.len());
        let mut cursor = 0;
        for edit in &edits {
            text.push_str(&source[cursor..edit.range.start]);
            let old_text = &source[edit.range.clone()];
            let new_start = text.len();
            text.push_str(&edit.replacement);
            let new_end = text.len();
            inverse.push(Edit::replace(new_start..new_end, old_text));
            cursor = edit.range.end;
        }
        text.push_str(&source[cursor..]);
        let change_map = ChangeMap::new(edits.clone());
        let inverse = Transaction::from_edits(inverse);
        Ok(AppliedTransaction {
            before_version: version,
            after_version: version
                .checked_add(1)
                .ok_or(TransactionError::Document(DocumentError::VersionOverflow))?,
            text,
            change_map,
            inverse,
        })
    }

    pub fn map_offset(&self, offset: usize, affinity: Affinity) -> Result<usize, EditError> {
        Ok(self.change_map_for_mapping()?.map_offset(offset, affinity))
    }

    fn change_map_for_mapping(&self) -> Result<ChangeMap, EditError> {
        let mut edits = self.edits.clone();
        edits.sort_by_key(|edit| edit.range.start);
        for pair in edits.windows(2) {
            if pair[0].range.end > pair[1].range.start {
                return Err(EditError::Overlapping {
                    previous: pair[0].range.clone(),
                    next: pair[1].range.clone(),
                });
            }
        }
        Ok(ChangeMap::new(edits))
    }
}

impl Default for Transaction {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeMap {
    edits: Vec<Edit>,
}

impl ChangeMap {
    pub(crate) fn new(edits: Vec<Edit>) -> Self {
        Self { edits }
    }

    pub fn edits(&self) -> &[Edit] {
        &self.edits
    }

    pub fn map_offset(&self, offset: usize, affinity: Affinity) -> usize {
        let mut delta: isize = 0;
        for edit in &self.edits {
            if offset < edit.range.start {
                break;
            }
            let old_len = edit.range.end - edit.range.start;
            let new_len = edit.replacement.len();
            if offset > edit.range.end {
                delta += new_len as isize - old_len as isize;
                continue;
            }
            if offset == edit.range.start && edit.range.start == edit.range.end {
                match affinity {
                    Affinity::Before => {
                        return (offset as isize + delta).max(0) as usize;
                    }
                    Affinity::After => {
                        delta += new_len as isize;
                        continue;
                    }
                }
            }
            if offset == edit.range.end {
                return match affinity {
                    Affinity::Before => {
                        (edit.range.start as isize + delta + new_len as isize).max(0) as usize
                    }
                    Affinity::After => {
                        delta += new_len as isize - old_len as isize;
                        continue;
                    }
                };
            }
            return match affinity {
                Affinity::Before => (edit.range.start as isize + delta).max(0) as usize,
                Affinity::After => {
                    (edit.range.start as isize + delta + new_len as isize).max(0) as usize
                }
            };
        }
        (offset as isize + delta).max(0) as usize
    }

    pub fn map_range(&self, range: Range<usize>) -> Range<usize> {
        self.map_offset(range.start, Affinity::Before)..self.map_offset(range.end, Affinity::After)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedTransaction {
    pub before_version: u64,
    pub after_version: u64,
    pub text: String,
    pub change_map: ChangeMap,
    pub inverse: Transaction,
}

impl AppliedTransaction {
    pub fn snapshot(&self) -> DocumentSnapshot {
        Document::snapshot_with_version(self.text.clone(), self.after_version)
    }
}
