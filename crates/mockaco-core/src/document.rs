use crate::{AppliedTransaction, PositionMap, Text, Transaction, TransactionError};
use std::fmt;
use std::sync::Arc;

#[derive(Clone)]
struct DocumentData {
    text: Text,
    version: u64,
}

/// A mutable document whose snapshots share their text.
///
/// The text is a rope, so applying an edit and taking a snapshot are both
/// O(log n): versions share every chunk the edit did not touch.
#[derive(Clone)]
pub struct Document {
    data: Arc<DocumentData>,
}

/// A cheap, immutable view of a document version.
#[derive(Clone)]
pub struct DocumentSnapshot {
    data: Arc<DocumentData>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentError {
    VersionOverflow,
}

impl fmt::Display for DocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VersionOverflow => f.write_str("document version overflow"),
        }
    }
}

impl std::error::Error for DocumentError {}

impl Document {
    pub fn empty() -> Self {
        Self::new("")
    }

    pub fn new(text: impl Into<Text>) -> Self {
        Self {
            data: Arc::new(DocumentData {
                text: text.into(),
                version: 0,
            }),
        }
    }

    pub fn text(&self) -> &Text {
        &self.data.text
    }

    pub fn version(&self) -> u64 {
        self.data.version
    }

    pub fn len_bytes(&self) -> usize {
        self.data.text.len()
    }

    pub fn snapshot(&self) -> DocumentSnapshot {
        DocumentSnapshot {
            data: Arc::clone(&self.data),
        }
    }

    pub fn from_snapshot(snapshot: &DocumentSnapshot) -> Self {
        Self {
            data: Arc::clone(&snapshot.data),
        }
    }

    pub(crate) fn snapshot_with_version(text: Text, version: u64) -> DocumentSnapshot {
        DocumentSnapshot {
            data: Arc::new(DocumentData { text, version }),
        }
    }

    /// Line and position conversions; the text itself.
    pub fn position_map(&self) -> &PositionMap {
        &self.data.text
    }

    pub fn apply(
        &mut self,
        transaction: &Transaction,
    ) -> Result<AppliedTransaction, TransactionError> {
        let applied = transaction.apply(self.text(), self.version())?;
        self.data = Arc::new(DocumentData {
            text: applied.text.clone(),
            version: applied.after_version,
        });
        Ok(applied)
    }
}

impl DocumentSnapshot {
    pub fn text(&self) -> &Text {
        &self.data.text
    }

    pub fn version(&self) -> u64 {
        self.data.version
    }

    pub fn len_bytes(&self) -> usize {
        self.data.text.len()
    }

    pub fn position_map(&self) -> &PositionMap {
        &self.data.text
    }
}

impl fmt::Debug for Document {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Document")
            .field("version", &self.version())
            .field("len_bytes", &self.len_bytes())
            .finish()
    }
}

impl fmt::Debug for DocumentSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DocumentSnapshot")
            .field("version", &self.version())
            .field("len_bytes", &self.len_bytes())
            .finish()
    }
}

impl PartialEq for Document {
    fn eq(&self, other: &Self) -> bool {
        self.version() == other.version() && self.text() == other.text()
    }
}

impl Eq for Document {}

impl PartialEq for DocumentSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.version() == other.version() && self.text() == other.text()
    }
}

impl Eq for DocumentSnapshot {}
