use crate::{AppliedTransaction, PositionMap, Transaction, TransactionError};
use std::fmt;
use std::sync::Arc;

#[derive(Clone)]
struct DocumentData {
    text: Arc<str>,
    version: u64,
    position_map: Arc<PositionMap>,
}

/// A mutable document whose snapshots share their text and coordinate map.
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

    pub fn new(text: impl Into<String>) -> Self {
        let text: Arc<str> = Arc::from(text.into());
        let position_map = Arc::new(PositionMap::new(&text));
        Self {
            data: Arc::new(DocumentData {
                text,
                version: 0,
                position_map,
            }),
        }
    }

    pub fn text(&self) -> &str {
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

    pub(crate) fn snapshot_with_version(text: String, version: u64) -> DocumentSnapshot {
        let text: Arc<str> = Arc::from(text);
        DocumentSnapshot {
            data: Arc::new(DocumentData {
                position_map: Arc::new(PositionMap::new(&text)),
                text,
                version,
            }),
        }
    }

    pub fn position_map(&self) -> &PositionMap {
        &self.data.position_map
    }

    pub fn apply(
        &mut self,
        transaction: &Transaction,
    ) -> Result<AppliedTransaction, TransactionError> {
        let applied = transaction.apply(self.text(), self.version())?;
        let version = applied.after_version;
        let text: Arc<str> = Arc::from(applied.text.clone());
        self.data = Arc::new(DocumentData {
            position_map: Arc::new(PositionMap::new(&text)),
            text,
            version,
        });
        Ok(AppliedTransaction {
            before_version: applied.before_version,
            after_version: version,
            text: applied.text,
            change_map: applied.change_map,
            inverse: applied.inverse,
        })
    }
}

impl DocumentSnapshot {
    pub fn text(&self) -> &str {
        &self.data.text
    }

    pub fn version(&self) -> u64 {
        self.data.version
    }

    pub fn len_bytes(&self) -> usize {
        self.data.text.len()
    }

    pub fn position_map(&self) -> &PositionMap {
        &self.data.position_map
    }
}

impl fmt::Debug for Document {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Document")
            .field("version", &self.version())
            .field("text", &self.text())
            .finish()
    }
}

impl fmt::Debug for DocumentSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DocumentSnapshot")
            .field("version", &self.version())
            .field("text", &self.text())
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
