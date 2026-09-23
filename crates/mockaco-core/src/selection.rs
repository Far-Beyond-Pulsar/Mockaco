use crate::{Affinity, ChangeMap};
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn caret(offset: usize) -> Self {
        Self {
            anchor: offset,
            head: offset,
        }
    }

    pub fn range(start: usize, end: usize) -> Self {
        Self {
            anchor: start,
            head: end,
        }
    }

    pub fn is_caret(&self) -> bool {
        self.anchor == self.head
    }

    pub fn is_forward(&self) -> bool {
        self.anchor <= self.head
    }

    pub fn ordered_range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub fn cursor(&self) -> usize {
        self.head
    }

    pub fn map(&self, changes: &ChangeMap) -> Self {
        let anchor_affinity = if self.anchor <= self.head {
            Affinity::Before
        } else {
            Affinity::After
        };
        let head_affinity = if self.anchor <= self.head {
            Affinity::After
        } else {
            Affinity::Before
        };
        Self {
            anchor: changes.map_offset(self.anchor, anchor_affinity),
            head: changes.map_offset(self.head, head_affinity),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SelectionSet {
    selections: Vec<Selection>,
}

impl SelectionSet {
    pub fn new(selections: impl IntoIterator<Item = Selection>) -> Self {
        let mut set = Self {
            selections: selections.into_iter().collect(),
        };
        set.normalize();
        set
    }

    pub fn caret(offset: usize) -> Self {
        Self::new([Selection::caret(offset)])
    }

    pub fn selections(&self) -> &[Selection] {
        &self.selections
    }

    pub fn len(&self) -> usize {
        self.selections.len()
    }

    pub fn is_empty(&self) -> bool {
        self.selections.is_empty()
    }

    pub fn primary(&self) -> Option<Selection> {
        self.selections.last().copied()
    }

    pub fn set(&mut self, selections: impl IntoIterator<Item = Selection>) {
        self.selections = selections.into_iter().collect();
        self.normalize();
    }

    pub fn map(&self, changes: &ChangeMap) -> Self {
        Self::new(
            self.selections
                .iter()
                .map(|selection| selection.map(changes)),
        )
    }

    pub fn collapse_to_carets(&self) -> Self {
        Self::new(
            self.selections
                .iter()
                .map(|selection| Selection::caret(selection.head)),
        )
    }

    fn normalize(&mut self) {
        self.selections.sort_by_key(|selection| {
            (
                selection.ordered_range().start,
                selection.ordered_range().end,
                selection.head,
            )
        });
        self.selections.dedup();
    }
}
