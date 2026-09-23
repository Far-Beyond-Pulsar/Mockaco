use crate::{Edit, EditorState, Grouping, Transaction, TransactionError};
use std::collections::HashMap;
use std::fmt;
use std::str;

/// A host-owned logical location. It is intentionally not a filesystem path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct DocumentLocation(String);

impl DocumentLocation {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for DocumentLocation {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for DocumentLocation {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodingMetadata {
    Utf8,
    HostDecoded { label: String, lossless: bool },
    Unsupported { label: String, byte_len: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewlineStyle {
    None,
    Lf,
    CrLf,
    Cr,
    Mixed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentMetadata {
    pub encoding: EncodingMetadata,
    pub newline: NewlineStyle,
    pub trailing_newline: bool,
}

impl DocumentMetadata {
    pub fn detect_utf8(text: &str) -> Self {
        Self {
            encoding: EncodingMetadata::Utf8,
            newline: detect_newline_style(text),
            trailing_newline: has_trailing_newline(text),
        }
    }

    pub fn host_decoded(text: &str, label: impl Into<String>, lossless: bool) -> Self {
        Self {
            encoding: EncodingMetadata::HostDecoded {
                label: label.into(),
                lossless,
            },
            newline: detect_newline_style(text),
            trailing_newline: has_trailing_newline(text),
        }
    }

    fn is_supported(&self) -> bool {
        !matches!(self.encoding, EncodingMetadata::Unsupported { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    InvalidUtf8 { offset: usize, byte_len: usize },
    UnsupportedEncoding { metadata: EncodingMetadata },
    GenerationOverflow,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUtf8 { offset, byte_len } => {
                write!(f, "invalid UTF-8 at byte {offset} of {byte_len}")
            }
            Self::UnsupportedEncoding { metadata } => {
                write!(f, "unsupported document encoding: {metadata:?}")
            }
            Self::GenerationOverflow => f.write_str("document generation overflow"),
        }
    }
}

impl std::error::Error for LoadError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentIdentity {
    pub document_version: u64,
    pub content_hash: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadReceipt {
    pub generation: u64,
    pub location: Option<DocumentLocation>,
    pub identity: ContentIdentity,
    pub metadata: DocumentMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SaveRequestId(u64);

impl SaveRequestId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveKind {
    Save,
    SaveAs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveRequest {
    pub id: SaveRequestId,
    pub generation: u64,
    pub kind: SaveKind,
    pub target: DocumentLocation,
    pub identity: ContentIdentity,
    pub text: String,
    pub metadata: DocumentMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSaveError {
    pub message: String,
}

impl HostSaveError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostSaveResult {
    Success { location: DocumentLocation },
    Failure(HostSaveError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveEvent {
    pub request_id: SaveRequestId,
    pub location: DocumentLocation,
    pub document_version: u64,
    pub content_hash: u64,
    pub text: String,
    pub metadata: DocumentMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveReceipt {
    pub request: SaveRequest,
    pub location: DocumentLocation,
    pub current_identity: ContentIdentity,
    pub current_dirty: bool,
}

impl SaveReceipt {
    pub fn event(&self) -> SaveEvent {
        SaveEvent {
            request_id: self.request.id,
            location: self.location.clone(),
            document_version: self.request.identity.document_version,
            content_hash: self.request.identity.content_hash,
            text: self.request.text.clone(),
            metadata: self.request.metadata.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveCompletion {
    Saved(SaveReceipt),
    Failed {
        request: SaveRequest,
        error: HostSaveError,
        still_dirty: bool,
    },
    Stale {
        request: SaveRequest,
        current_generation: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveRequestError {
    NoLocation,
    RequestIdExhausted,
}

impl fmt::Display for SaveRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoLocation => f.write_str("document has no location; use save-as"),
            Self::RequestIdExhausted => f.write_str("save request id space exhausted"),
        }
    }
}

impl std::error::Error for SaveRequestError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalChange {
    pub generation: u64,
    pub identity: ContentIdentity,
    pub text: String,
    pub metadata: DocumentMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalConflict {
    pub change: ExternalChange,
    pub current_identity: ContentIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalChangeOutcome {
    Reloaded(LoadReceipt),
    Conflict(ExternalConflict),
    KeptLocal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalChangeDecision {
    KeepLocal,
    ReloadExternal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalChangeError {
    NoPendingChange,
    StaleGeneration { expected: u64, actual: u64 },
    UnsupportedEncoding(EncodingMetadata),
    GenerationOverflow,
}

impl fmt::Display for ExternalChangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPendingChange => f.write_str("no external change is pending"),
            Self::StaleGeneration { expected, actual } => {
                write!(
                    f,
                    "external change expects generation {expected}, got {actual}"
                )
            }
            Self::UnsupportedEncoding(metadata) => {
                write!(f, "external change has unsupported encoding: {metadata:?}")
            }
            Self::GenerationOverflow => f.write_str("document generation overflow"),
        }
    }
}

impl std::error::Error for ExternalChangeError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseDecision {
    Discard,
    Save,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseOutcome {
    CloseNow,
    Cancelled,
    SaveRequired(SaveRequest),
}

/// Host lifecycle state for one editor document. It owns no I/O and no UI.
#[derive(Debug, Clone)]
pub struct DocumentSession {
    editor: EditorState,
    location: Option<DocumentLocation>,
    metadata: DocumentMetadata,
    saved_content_hash: u64,
    generation: u64,
    next_save_id: u64,
    pending_saves: HashMap<SaveRequestId, SaveRequest>,
    pending_external: Option<ExternalChange>,
}

impl DocumentSession {
    pub fn new(
        location: Option<DocumentLocation>,
        text: impl Into<String>,
        metadata: DocumentMetadata,
    ) -> Result<Self, LoadError> {
        if !metadata.is_supported() {
            return Err(LoadError::UnsupportedEncoding {
                metadata: metadata.encoding.clone(),
            });
        }
        let text = text.into();
        let saved_content_hash = content_hash(&text);
        Ok(Self {
            editor: EditorState::new(text),
            location,
            metadata,
            saved_content_hash,
            generation: 0,
            next_save_id: 1,
            pending_saves: HashMap::new(),
            pending_external: None,
        })
    }

    pub fn location(&self) -> Option<&DocumentLocation> {
        self.location.as_ref()
    }

    pub fn metadata(&self) -> &DocumentMetadata {
        &self.metadata
    }

    pub fn editor(&self) -> &EditorState {
        &self.editor
    }

    pub fn snapshot(&self) -> crate::DocumentSnapshot {
        self.editor.snapshot()
    }

    pub fn text(&self) -> &str {
        self.editor.document().text()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn current_identity(&self) -> ContentIdentity {
        ContentIdentity {
            document_version: self.editor.snapshot().version(),
            content_hash: content_hash(self.text()),
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.current_identity().content_hash != self.saved_content_hash
    }

    pub fn apply_transaction(
        &mut self,
        transaction: &Transaction,
        grouping: Grouping,
    ) -> Result<crate::AppliedTransaction, TransactionError> {
        self.editor.apply_with_result(transaction, grouping)
    }

    pub fn apply_edit(
        &mut self,
        range: std::ops::Range<usize>,
        replacement: impl Into<String>,
    ) -> Result<crate::AppliedTransaction, TransactionError> {
        self.apply_transaction(
            &Transaction::from_edits([Edit::replace(range, replacement)]),
            Grouping::Separate,
        )
    }

    pub fn replace_from_utf8_bytes(
        &mut self,
        location: Option<DocumentLocation>,
        bytes: &[u8],
    ) -> Result<LoadReceipt, LoadError> {
        let text = str::from_utf8(bytes).map_err(|error| LoadError::InvalidUtf8 {
            offset: error.valid_up_to(),
            byte_len: bytes.len(),
        })?;
        self.replace_loaded(
            location,
            text.to_owned(),
            DocumentMetadata::detect_utf8(text),
        )
    }

    pub fn replace_from_host_decoded(
        &mut self,
        location: Option<DocumentLocation>,
        text: impl Into<String>,
        metadata: DocumentMetadata,
    ) -> Result<LoadReceipt, LoadError> {
        if !metadata.is_supported() {
            return Err(LoadError::UnsupportedEncoding {
                metadata: metadata.encoding.clone(),
            });
        }
        self.replace_loaded(location, text.into(), metadata)
    }

    pub fn replace_loaded(
        &mut self,
        location: Option<DocumentLocation>,
        text: String,
        metadata: DocumentMetadata,
    ) -> Result<LoadReceipt, LoadError> {
        if !metadata.is_supported() {
            return Err(LoadError::UnsupportedEncoding {
                metadata: metadata.encoding.clone(),
            });
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(LoadError::GenerationOverflow)?;
        self.editor = EditorState::new(text.clone());
        self.location = location;
        self.metadata = metadata;
        self.saved_content_hash = content_hash(&text);
        self.generation = generation;
        self.pending_saves.clear();
        self.pending_external = None;
        Ok(LoadReceipt {
            generation,
            location: self.location.clone(),
            identity: self.current_identity(),
            metadata: self.metadata.clone(),
        })
    }

    pub fn request_save(&mut self) -> Result<SaveRequest, SaveRequestError> {
        let target = self.location.clone().ok_or(SaveRequestError::NoLocation)?;
        self.create_save_request(SaveKind::Save, target)
    }

    pub fn request_save_as(
        &mut self,
        target: impl Into<DocumentLocation>,
    ) -> Result<SaveRequest, SaveRequestError> {
        self.create_save_request(SaveKind::SaveAs, target.into())
    }

    pub fn complete_save(
        &mut self,
        request_id: SaveRequestId,
        result: HostSaveResult,
    ) -> SaveCompletion {
        let Some(request) = self.pending_saves.remove(&request_id) else {
            return SaveCompletion::Stale {
                request: SaveRequest {
                    id: request_id,
                    generation: self.generation,
                    kind: SaveKind::Save,
                    target: self
                        .location
                        .clone()
                        .unwrap_or_else(|| DocumentLocation::new("<unknown>")),
                    identity: self.current_identity(),
                    text: self.text().to_owned(),
                    metadata: self.metadata.clone(),
                },
                current_generation: self.generation,
            };
        };
        if request.generation != self.generation {
            return SaveCompletion::Stale {
                request,
                current_generation: self.generation,
            };
        }
        match result {
            HostSaveResult::Failure(error) => SaveCompletion::Failed {
                request,
                error,
                still_dirty: self.is_dirty(),
            },
            HostSaveResult::Success { location } => {
                self.location = Some(location.clone());
                self.saved_content_hash = request.identity.content_hash;
                SaveCompletion::Saved(SaveReceipt {
                    request,
                    location,
                    current_identity: self.current_identity(),
                    current_dirty: self.is_dirty(),
                })
            }
        }
    }

    pub fn notify_external_change(
        &mut self,
        text: impl Into<String>,
        metadata: DocumentMetadata,
    ) -> Result<ExternalChangeOutcome, ExternalChangeError> {
        self.notify_external_change_at(self.generation, text, metadata)
    }

    pub fn notify_external_change_at(
        &mut self,
        expected_generation: u64,
        text: impl Into<String>,
        metadata: DocumentMetadata,
    ) -> Result<ExternalChangeOutcome, ExternalChangeError> {
        if expected_generation != self.generation {
            return Err(ExternalChangeError::StaleGeneration {
                expected: expected_generation,
                actual: self.generation,
            });
        }
        if !metadata.is_supported() {
            return Err(ExternalChangeError::UnsupportedEncoding(
                metadata.encoding.clone(),
            ));
        }
        let text = text.into();
        let change = ExternalChange {
            generation: self.generation,
            identity: ContentIdentity {
                document_version: self.editor.snapshot().version(),
                content_hash: content_hash(&text),
            },
            text,
            metadata,
        };
        if self.is_dirty() {
            let conflict = ExternalConflict {
                change: change.clone(),
                current_identity: self.current_identity(),
            };
            self.pending_external = Some(change);
            Ok(ExternalChangeOutcome::Conflict(conflict))
        } else {
            let receipt = self
                .replace_external(change)
                .map_err(|_| ExternalChangeError::GenerationOverflow)?;
            Ok(ExternalChangeOutcome::Reloaded(receipt))
        }
    }

    pub fn pending_external_change(&self) -> Option<&ExternalChange> {
        self.pending_external.as_ref()
    }

    pub fn resolve_external_change(
        &mut self,
        decision: ExternalChangeDecision,
    ) -> Result<ExternalChangeOutcome, ExternalChangeError> {
        let Some(change) = self.pending_external.take() else {
            return Err(ExternalChangeError::NoPendingChange);
        };
        match decision {
            ExternalChangeDecision::KeepLocal => Ok(ExternalChangeOutcome::KeptLocal),
            ExternalChangeDecision::ReloadExternal => {
                let receipt = self
                    .replace_external(change)
                    .map_err(|_| ExternalChangeError::GenerationOverflow)?;
                Ok(ExternalChangeOutcome::Reloaded(receipt))
            }
        }
    }

    pub fn close(&mut self, decision: CloseDecision) -> Result<CloseOutcome, SaveRequestError> {
        match decision {
            CloseDecision::Cancel => Ok(CloseOutcome::Cancelled),
            CloseDecision::Discard => Ok(CloseOutcome::CloseNow),
            CloseDecision::Save if !self.is_dirty() => Ok(CloseOutcome::CloseNow),
            CloseDecision::Save => self.request_save().map(CloseOutcome::SaveRequired),
        }
    }

    fn create_save_request(
        &mut self,
        kind: SaveKind,
        target: DocumentLocation,
    ) -> Result<SaveRequest, SaveRequestError> {
        let id = SaveRequestId(self.next_save_id);
        self.next_save_id = self
            .next_save_id
            .checked_add(1)
            .ok_or(SaveRequestError::RequestIdExhausted)?;
        let request = SaveRequest {
            id,
            generation: self.generation,
            kind,
            target,
            identity: self.current_identity(),
            text: self.text().to_owned(),
            metadata: self.metadata.clone(),
        };
        self.pending_saves.insert(id, request.clone());
        Ok(request)
    }

    fn replace_external(&mut self, change: ExternalChange) -> Result<LoadReceipt, LoadError> {
        self.replace_loaded(self.location.clone(), change.text, change.metadata)
    }
}

fn detect_newline_style(text: &str) -> NewlineStyle {
    let bytes = text.as_bytes();
    let mut lf = 0;
    let mut crlf = 0;
    let mut cr = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                crlf += 1;
                index += 2;
            }
            b'\r' => {
                cr += 1;
                index += 1;
            }
            b'\n' => {
                lf += 1;
                index += 1;
            }
            _ => index += 1,
        }
    }
    match (lf > 0, crlf > 0, cr > 0) {
        (false, false, false) => NewlineStyle::None,
        (true, false, false) => NewlineStyle::Lf,
        (false, true, false) => NewlineStyle::CrLf,
        (false, false, true) => NewlineStyle::Cr,
        _ => NewlineStyle::Mixed,
    }
}

fn has_trailing_newline(text: &str) -> bool {
    text.ends_with('\n') || text.ends_with('\r')
}

fn content_hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        hash.wrapping_mul(0x100000001b3)
            .wrapping_add(u64::from(byte))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf8_metadata(text: &str) -> DocumentMetadata {
        DocumentMetadata::detect_utf8(text)
    }

    fn session(text: &str) -> DocumentSession {
        DocumentSession::new(
            Some(DocumentLocation::new("doc://main")),
            text,
            utf8_metadata(text),
        )
        .unwrap()
    }

    #[test]
    fn load_failure_preserves_the_existing_document() {
        let mut document = session("safe");
        let before = document.snapshot();
        let error = document.replace_from_utf8_bytes(None, &[0xff]).unwrap_err();
        assert!(matches!(error, LoadError::InvalidUtf8 { .. }));
        assert_eq!(document.snapshot(), before);
        assert!(!document.is_dirty());
    }

    #[test]
    fn load_replacement_resets_dirty_state_and_records_metadata() {
        let mut document = session("old");
        document.apply_edit(0..3, "edited").unwrap();
        let receipt = document
            .replace_from_utf8_bytes(Some(DocumentLocation::new("doc://new")), b"a\r\nb")
            .unwrap();
        assert!(!document.is_dirty());
        assert_eq!(document.location().unwrap().as_str(), "doc://new");
        assert_eq!(receipt.metadata.newline, NewlineStyle::CrLf);
        assert_eq!(receipt.identity.document_version, 0);
    }

    #[test]
    fn dirty_and_clean_transitions_follow_content_identity() {
        let mut document = session("abc");
        assert!(!document.is_dirty());
        document.apply_edit(3..3, "!").unwrap();
        assert!(document.is_dirty());
        document.apply_edit(3..4, "").unwrap();
        assert!(!document.is_dirty());
    }

    #[test]
    fn save_failure_preserves_state_and_success_marks_the_requested_content() {
        let mut document = session("abc");
        document.apply_edit(3..3, "!").unwrap();
        let request = document.request_save().unwrap();
        let failure = document.complete_save(
            request.id,
            HostSaveResult::Failure(HostSaveError::new("read-only")),
        );
        assert!(matches!(
            failure,
            SaveCompletion::Failed {
                still_dirty: true,
                ..
            }
        ));
        assert!(document.is_dirty());

        let request = document.request_save().unwrap();
        let completion = document.complete_save(
            request.id,
            HostSaveResult::Success {
                location: DocumentLocation::new("doc://main"),
            },
        );
        let receipt = match completion {
            SaveCompletion::Saved(receipt) => receipt,
            other => panic!("unexpected completion: {other:?}"),
        };
        assert!(!receipt.current_dirty);
        assert!(!document.is_dirty());
    }

    #[test]
    fn save_as_updates_host_location_without_filesystem_ownership() {
        let mut document = session("text");
        document.apply_edit(0..0, "new ").unwrap();
        let request = document.request_save_as("doc://copy").unwrap();
        assert_eq!(request.kind, SaveKind::SaveAs);
        document.complete_save(
            request.id,
            HostSaveResult::Success {
                location: DocumentLocation::new("doc://copy"),
            },
        );
        assert_eq!(document.location().unwrap().as_str(), "doc://copy");
    }

    #[test]
    fn newline_and_encoding_metadata_are_explicit() {
        let mut document = session("a\nb");
        let metadata = DocumentMetadata::host_decoded("a\rb", "windows-1252", true);
        document
            .replace_from_host_decoded(None, "a\rb", metadata.clone())
            .unwrap();
        assert_eq!(document.metadata(), &metadata);
        let unsupported = DocumentMetadata {
            encoding: EncodingMetadata::Unsupported {
                label: "binary".into(),
                byte_len: 2,
            },
            newline: NewlineStyle::None,
            trailing_newline: false,
        };
        assert!(matches!(
            document.replace_from_host_decoded(None, "x", unsupported),
            Err(LoadError::UnsupportedEncoding { .. })
        ));
    }

    #[test]
    fn external_changes_reload_clean_documents_and_conflict_when_dirty() {
        let mut clean = session("one");
        let outcome = clean
            .notify_external_change("two", utf8_metadata("two"))
            .unwrap();
        assert!(matches!(outcome, ExternalChangeOutcome::Reloaded(_)));
        assert_eq!(clean.text(), "two");
        assert!(!clean.is_dirty());

        let mut dirty = session("one");
        dirty.apply_edit(0..3, "local").unwrap();
        let outcome = dirty
            .notify_external_change("remote", utf8_metadata("remote"))
            .unwrap();
        assert!(matches!(outcome, ExternalChangeOutcome::Conflict(_)));
        assert_eq!(dirty.text(), "local");
        dirty
            .resolve_external_change(ExternalChangeDecision::KeepLocal)
            .unwrap();
        assert_eq!(dirty.text(), "local");
        assert!(dirty.pending_external_change().is_none());
    }

    #[test]
    fn reload_external_is_an_explicit_conflict_decision() {
        let mut document = session("local");
        document.apply_edit(0..5, "dirty").unwrap();
        document
            .notify_external_change("remote", utf8_metadata("remote"))
            .unwrap();
        let outcome = document
            .resolve_external_change(ExternalChangeDecision::ReloadExternal)
            .unwrap();
        assert!(matches!(outcome, ExternalChangeOutcome::Reloaded(_)));
        assert_eq!(document.text(), "remote");
        assert!(!document.is_dirty());
    }

    #[test]
    fn close_decisions_are_typed_and_save_remains_host_driven() {
        let mut document = session("text");
        assert_eq!(
            document.close(CloseDecision::Cancel).unwrap(),
            CloseOutcome::Cancelled
        );
        assert_eq!(
            document.close(CloseDecision::Discard).unwrap(),
            CloseOutcome::CloseNow
        );
        document.apply_edit(0..0, "dirty ").unwrap();
        assert!(matches!(
            document.close(CloseDecision::Save).unwrap(),
            CloseOutcome::SaveRequired(_)
        ));
    }

    #[test]
    fn stale_save_completion_cannot_mutate_a_replaced_document() {
        let mut document = session("one");
        document.apply_edit(0..3, "dirty").unwrap();
        let request = document.request_save().unwrap();
        document
            .replace_from_utf8_bytes(Some(DocumentLocation::new("doc://new")), b"new")
            .unwrap();
        let completion = document.complete_save(
            request.id,
            HostSaveResult::Success {
                location: DocumentLocation::new("doc://old"),
            },
        );
        assert!(matches!(completion, SaveCompletion::Stale { .. }));
        assert_eq!(document.location().unwrap().as_str(), "doc://new");
        assert_eq!(document.text(), "new");
    }

    #[test]
    fn save_event_contains_the_exact_host_persisted_snapshot() {
        let mut document = session("before");
        document.apply_edit(0..6, "after").unwrap();
        let request = document.request_save().unwrap();
        let receipt = match document.complete_save(
            request.id,
            HostSaveResult::Success {
                location: DocumentLocation::new("doc://main"),
            },
        ) {
            SaveCompletion::Saved(receipt) => receipt,
            other => panic!("unexpected completion: {other:?}"),
        };
        let event = receipt.event();
        assert_eq!(event.text, "after");
        assert_eq!(event.document_version, request.identity.document_version);
        assert_eq!(event.location.as_str(), "doc://main");
    }

    #[test]
    fn stale_external_notifications_are_rejected_by_generation() {
        let mut document = session("one");
        let generation = document.generation();
        document.replace_from_utf8_bytes(None, b"two").unwrap();
        assert!(matches!(
            document.notify_external_change_at(generation, "old", utf8_metadata("old")),
            Err(ExternalChangeError::StaleGeneration { .. })
        ));
    }
}
