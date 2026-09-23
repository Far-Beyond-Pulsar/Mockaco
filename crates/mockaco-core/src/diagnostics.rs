use std::fmt;
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
    Hint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: Range<usize>,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub source: Option<String>,
}

impl Diagnostic {
    pub fn new(
        range: Range<usize>,
        severity: DiagnosticSeverity,
        message: impl Into<String>,
    ) -> Self {
        Self {
            range,
            severity,
            message: message.into(),
            source: None,
        }
    }

    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticError {
    StaleDocumentVersion { expected: u64, actual: u64 },
    InvalidRange(Range<usize>),
}

impl fmt::Display for DiagnosticError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleDocumentVersion { expected, actual } => {
                write!(
                    f,
                    "diagnostics expect document version {expected}, got {actual}"
                )
            }
            Self::InvalidRange(range) => write!(f, "diagnostic range {range:?} is invalid"),
        }
    }
}

impl std::error::Error for DiagnosticError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticSet {
    document_version: u64,
    diagnostics: Vec<Diagnostic>,
    revision: u64,
}

impl DiagnosticSet {
    pub fn new(document_version: u64) -> Self {
        Self {
            document_version,
            diagnostics: Vec::new(),
            revision: 0,
        }
    }

    pub fn document_version(&self) -> u64 {
        self.document_version
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn publish(
        &mut self,
        snapshot: &crate::DocumentSnapshot,
        diagnostics: impl IntoIterator<Item = Diagnostic>,
    ) -> Result<(), DiagnosticError> {
        self.require_version(snapshot.version())?;
        let mut diagnostics: Vec<_> = diagnostics.into_iter().collect();
        for diagnostic in &diagnostics {
            if diagnostic.range.start > diagnostic.range.end
                || diagnostic.range.end > snapshot.len_bytes()
                || !snapshot.text().is_char_boundary(diagnostic.range.start)
                || !snapshot.text().is_char_boundary(diagnostic.range.end)
            {
                return Err(DiagnosticError::InvalidRange(diagnostic.range.clone()));
            }
        }
        diagnostics.sort_by_key(|diagnostic| {
            (
                diagnostic.range.start,
                diagnostic.range.end,
                diagnostic.severity,
            )
        });
        self.diagnostics = diagnostics;
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    pub fn advance_document_version(&mut self, version: u64) -> Result<(), DiagnosticError> {
        if version < self.document_version {
            return Err(DiagnosticError::StaleDocumentVersion {
                expected: self.document_version,
                actual: version,
            });
        }
        if version != self.document_version {
            self.document_version = version;
            self.diagnostics.clear();
            self.revision = self.revision.saturating_add(1);
        }
        Ok(())
    }

    pub fn clear(&mut self) {
        if !self.diagnostics.is_empty() {
            self.diagnostics.clear();
            self.revision = self.revision.saturating_add(1);
        }
    }

    pub fn at(&self, offset: usize) -> Vec<&Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|diagnostic| {
                if diagnostic.range.is_empty() {
                    diagnostic.range.start == offset
                } else {
                    diagnostic.range.contains(&offset)
                }
            })
            .collect()
    }

    pub fn overlapping(&self, range: Range<usize>) -> Vec<&Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|diagnostic| {
                (diagnostic.range.is_empty() && range.start <= diagnostic.range.start)
                    || (range.is_empty() && diagnostic.range.start <= range.start)
                    || (diagnostic.range.start < range.end && range.start < diagnostic.range.end)
            })
            .collect()
    }

    fn require_version(&self, actual: u64) -> Result<(), DiagnosticError> {
        if actual == self.document_version {
            Ok(())
        } else {
            Err(DiagnosticError::StaleDocumentVersion {
                expected: self.document_version,
                actual,
            })
        }
    }
}
