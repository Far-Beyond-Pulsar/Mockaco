use crate::{DocumentSnapshot, SelectionSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grouping {
    Separate,
    CurrentGroup,
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub before: DocumentSnapshot,
    pub after: DocumentSnapshot,
    pub before_selections: SelectionSet,
    pub after_selections: SelectionSet,
}

#[derive(Debug, Clone)]
pub struct HistoryAction {
    pub snapshot: DocumentSnapshot,
    pub selections: SelectionSet,
}

#[derive(Debug, Clone, Default)]
pub struct UndoHistory {
    entries: Vec<HistoryEntry>,
    cursor: usize,
    grouping: bool,
    group_entry: Option<usize>,
}

impl UndoHistory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor < self.entries.len()
    }

    pub fn position(&self) -> usize {
        self.cursor
    }

    pub fn begin_group(&mut self) {
        self.grouping = true;
        self.group_entry = None;
    }

    pub fn end_group(&mut self) {
        self.grouping = false;
        self.group_entry = None;
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.cursor = 0;
        self.grouping = false;
        self.group_entry = None;
    }

    pub fn record(
        &mut self,
        before: DocumentSnapshot,
        after: DocumentSnapshot,
        before_selections: SelectionSet,
        after_selections: SelectionSet,
        grouping: Grouping,
    ) {
        if before.text() == after.text() && before_selections == after_selections {
            return;
        }
        if self.cursor < self.entries.len() {
            self.entries.truncate(self.cursor);
        }
        let at_tip = self.cursor == self.entries.len();
        let should_join = if self.grouping {
            self.group_entry == Some(self.entries.len().saturating_sub(1)) && at_tip
        } else {
            grouping == Grouping::CurrentGroup && at_tip && !self.entries.is_empty()
        };
        if should_join {
            if let Some(last) = self.entries.last_mut() {
                last.after = after;
                last.after_selections = after_selections;
                self.cursor = self.entries.len();
                return;
            }
        }
        self.entries.push(HistoryEntry {
            before,
            after,
            before_selections,
            after_selections,
        });
        self.cursor = self.entries.len();
        if self.grouping {
            self.group_entry = Some(self.entries.len() - 1);
        }
    }

    pub fn push(&mut self, before: DocumentSnapshot, after: DocumentSnapshot) {
        self.record(
            before,
            after,
            SelectionSet::default(),
            SelectionSet::default(),
            Grouping::Separate,
        );
    }

    pub fn undo(&mut self) -> Option<HistoryAction> {
        if self.cursor == 0 {
            return None;
        }
        self.cursor -= 1;
        let entry = &self.entries[self.cursor];
        Some(HistoryAction {
            snapshot: entry.before.clone(),
            selections: entry.before_selections.clone(),
        })
    }

    pub fn redo(&mut self) -> Option<HistoryAction> {
        if self.cursor >= self.entries.len() {
            return None;
        }
        let entry = &self.entries[self.cursor];
        self.cursor += 1;
        Some(HistoryAction {
            snapshot: entry.after.clone(),
            selections: entry.after_selections.clone(),
        })
    }

    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }
}
