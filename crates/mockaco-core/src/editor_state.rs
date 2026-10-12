use crate::{
    AppliedTransaction, Document, DocumentSnapshot, Grouping, HistoryAction, HistoryStep,
    Selection, SelectionSet, Transaction, TransactionError, UndoHistory,
};
use std::fmt;
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ViewportIntent {
    pub top_line: usize,
    pub left_column: usize,
    pub visible_lines: usize,
    pub visible_columns: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImeComposition {
    pub range: Range<usize>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImeError {
    InvalidRange { range: Range<usize>, len: usize },
    NotCharBoundary(usize),
}

impl fmt::Display for ImeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRange { range, len } => {
                write!(f, "IME range {range:?} is outside length {len}")
            }
            Self::NotCharBoundary(offset) => {
                write!(f, "IME offset {offset} is not a UTF-8 boundary")
            }
        }
    }
}

impl std::error::Error for ImeError {}

/// How an undo or redo changed the document.
#[derive(Debug, Clone)]
pub enum HistoryChange {
    /// The entry's edits were replayed, oldest first. Each carries its change
    /// map, so layout, highlighting and language servers can follow
    /// incrementally, as they do for typing.
    Edits(Vec<AppliedTransaction>),
    /// The document was replaced with the entry's snapshot: the entry had no
    /// recorded edits. Everything derived from the text must be rebuilt.
    Replaced,
}

/// The framework-neutral state needed to connect a document to a renderer.
#[derive(Debug, Clone)]
pub struct EditorState {
    document: Document,
    selections: SelectionSet,
    viewport: ViewportIntent,
    pending_composition: Option<ImeComposition>,
    undo: UndoHistory,
}

impl EditorState {
    pub fn new(text: impl Into<crate::Text>) -> Self {
        let document = Document::new(text);
        Self {
            selections: SelectionSet::caret(0),
            document,
            viewport: ViewportIntent::default(),
            pending_composition: None,
            undo: UndoHistory::new(),
        }
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    pub fn snapshot(&self) -> DocumentSnapshot {
        self.document.snapshot()
    }

    pub fn selections(&self) -> &SelectionSet {
        &self.selections
    }

    pub fn set_selections(&mut self, selections: SelectionSet) {
        self.selections = selections;
    }

    pub fn viewport(&self) -> ViewportIntent {
        self.viewport
    }

    pub fn set_viewport(&mut self, viewport: ViewportIntent) {
        self.viewport = viewport;
    }

    pub fn pending_composition(&self) -> Option<&ImeComposition> {
        self.pending_composition.as_ref()
    }

    pub fn undo_history(&self) -> &UndoHistory {
        &self.undo
    }

    pub fn begin_undo_group(&mut self) {
        self.undo.begin_group();
    }

    pub fn end_undo_group(&mut self) {
        self.undo.end_group();
    }

    pub fn apply(
        &mut self,
        transaction: &Transaction,
        grouping: Grouping,
    ) -> Result<(), TransactionError> {
        self.apply_with_result(transaction, grouping).map(|_| ())
    }

    pub fn apply_with_result(
        &mut self,
        transaction: &Transaction,
        grouping: Grouping,
    ) -> Result<crate::AppliedTransaction, TransactionError> {
        let before = self.document.snapshot();
        let before_selections = self.selections.clone();
        let applied = self.document.apply(transaction)?;
        self.selections = self.selections.map(&applied.change_map);
        let after = self.document.snapshot();
        self.undo.record_step(
            before,
            after,
            before_selections,
            self.selections.clone(),
            HistoryStep {
                forward: transaction.clone(),
                inverse: applied.inverse.clone(),
            },
            grouping,
        );
        Ok(applied)
    }

    pub fn undo(&mut self) -> bool {
        self.undo_with_result().is_some()
    }

    pub fn redo(&mut self) -> bool {
        self.redo_with_result().is_some()
    }

    /// Undoes the last history entry and reports how the document changed,
    /// so a caller can update what it derives from the text incrementally.
    /// `None` if there is nothing to undo.
    pub fn undo_with_result(&mut self) -> Option<HistoryChange> {
        let action = self.undo.undo()?;
        Some(self.restore(action))
    }

    /// Redoes the next history entry; see [`Self::undo_with_result`].
    pub fn redo_with_result(&mut self) -> Option<HistoryChange> {
        let action = self.undo.redo()?;
        Some(self.restore(action))
    }

    /// Brings the document to `action.snapshot` by replaying its edits, or
    /// by replacing the document when it has none (or they no longer apply).
    /// Either way the version moves forward, never back to the snapshot's, so
    /// anything keyed on versions can't mistake the result for an older or
    /// discarded document.
    fn restore(&mut self, action: HistoryAction) -> HistoryChange {
        self.pending_composition = None;
        if let Some(transactions) = action.transactions {
            let start = self.document.clone();
            let mut applied = Vec::with_capacity(transactions.len());
            let replayed = transactions.iter().try_for_each(|transaction| {
                applied.push(self.document.apply(transaction)?);
                Ok::<_, TransactionError>(())
            });
            if replayed.is_ok() && self.document.len_bytes() == action.snapshot.len_bytes() {
                self.selections = action.selections;
                return HistoryChange::Edits(applied);
            }
            self.document = start;
        }
        let version = self.document.version().saturating_add(1);
        self.document = Document::from_snapshot(&Document::snapshot_with_version(
            action.snapshot.text().clone(),
            version,
        ));
        self.selections = action.selections;
        HistoryChange::Replaced
    }

    pub fn update_composition(
        &mut self,
        range: Range<usize>,
        text: impl Into<String>,
    ) -> Result<(), ImeError> {
        if range.start > range.end || range.end > self.document.len_bytes() {
            return Err(ImeError::InvalidRange {
                range,
                len: self.document.len_bytes(),
            });
        }
        if !self.document.text().is_char_boundary(range.start) {
            return Err(ImeError::NotCharBoundary(range.start));
        }
        if !self.document.text().is_char_boundary(range.end) {
            return Err(ImeError::NotCharBoundary(range.end));
        }
        self.pending_composition = Some(ImeComposition {
            range,
            text: text.into(),
        });
        Ok(())
    }

    pub fn commit_composition(&mut self) -> Result<bool, TransactionError> {
        self.commit_composition_with_result()
            .map(|result| result.is_some())
    }

    pub fn commit_composition_with_result(
        &mut self,
    ) -> Result<Option<crate::AppliedTransaction>, TransactionError> {
        let Some(composition) = self.pending_composition.take() else {
            return Ok(None);
        };
        let range_end = composition.range.start + composition.text.len();
        let transaction =
            Transaction::new().replace(composition.range.clone(), composition.text.clone());
        let applied = match self.apply_with_result(&transaction, Grouping::Separate) {
            Ok(applied) => applied,
            Err(error) => {
                self.pending_composition = Some(composition);
                return Err(error);
            }
        };
        self.selections = SelectionSet::new([Selection::caret(range_end)]);
        Ok(Some(applied))
    }

    pub fn cancel_composition(&mut self) -> bool {
        self.pending_composition.take().is_some()
    }
}
