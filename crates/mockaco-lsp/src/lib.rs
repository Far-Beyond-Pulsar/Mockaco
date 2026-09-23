//! Framework-independent LSP integration seams for Mockaco.
//!
//! The crate deliberately stops at protocol-shaped values, document versions,
//! and byte ranges. A transport, async runtime, filesystem, and UI can be
//! supplied by the host application without leaking into the editor core.

use lsp_types::{
    AnnotatedTextEdit, CodeActionOrCommand, Diagnostic as LspDiagnostic,
    DiagnosticSeverity as LspDiagnosticSeverity, DocumentChanges, GotoDefinitionResponse, Location,
    LocationLink, OneOf, Position, PublishDiagnosticsParams, Range, SemanticTokens,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem, TextEdit, Uri,
    WorkspaceEdit,
};
use mockaco_core::{
    Diagnostic as CoreDiagnostic, DiagnosticError, DiagnosticSet, DiagnosticSeverity,
    DocumentSnapshot, Edit, EditorState, Grouping, PositionError, SaveEvent, TextPosition,
    Transaction, TransactionError,
};
use std::collections::HashMap;
use std::fmt;
use std::ops::Range as ByteRange;

pub use lsp_types::{
    CompletionResponse, Hover, InlineCompletionResponse, SemanticToken, SemanticTokensLegend,
};

/// The monotonically increasing, editor-side document version.
pub type DocumentVersion = u64;

/// LSP request ids are deterministic within one scheduler instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(u64);

impl RequestId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestKind {
    Completion,
    InlineCompletion,
    Hover,
    Definition,
    References,
    CodeAction,
    SemanticTokens,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError(pub String);

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TransportError {}

/// A runtime-neutral notification emitted by the document synchronizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundNotification {
    DidOpen(TextDocumentItem),
    DidChange {
        uri: Uri,
        version: i32,
        content_changes: Vec<TextDocumentContentChangeEvent>,
    },
    DidSave {
        uri: Uri,
        text: Option<String>,
    },
    DidClose(TextDocumentIdentifier),
    Cancel(RequestId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundRequest {
    pub id: RequestId,
    pub kind: RequestKind,
    pub uri: Uri,
    pub document_version: DocumentVersion,
}

/// The only transport contract needed by the scheduler and synchronizer.
pub trait LspTransport {
    fn send_notification(
        &mut self,
        notification: OutboundNotification,
    ) -> Result<(), TransportError>;

    fn send_request(&mut self, request: OutboundRequest) -> Result<(), TransportError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncError {
    AlreadyOpen(Uri),
    NotOpen(Uri),
    VersionMismatch {
        uri: Uri,
        expected: DocumentVersion,
        actual: DocumentVersion,
    },
    VersionOverflow(DocumentVersion),
    Transport(TransportError),
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyOpen(uri) => write!(f, "document {uri:?} is already open"),
            Self::NotOpen(uri) => write!(f, "document {uri:?} is not open"),
            Self::VersionMismatch {
                uri,
                expected,
                actual,
            } => write!(
                f,
                "document {uri:?} expects version {expected}, got {actual}"
            ),
            Self::VersionOverflow(version) => write!(f, "version {version} does not fit LSP i32"),
            Self::Transport(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for SyncError {}

#[derive(Debug, Clone)]
struct OpenDocument {
    language_id: String,
    version: DocumentVersion,
    text: String,
}

/// Tracks the exact text/version pair mirrored to an LSP server.
#[derive(Debug, Default, Clone)]
pub struct DocumentSync {
    documents: HashMap<Uri, OpenDocument>,
}

impl DocumentSync {
    pub fn open<T: LspTransport>(
        &mut self,
        transport: &mut T,
        uri: Uri,
        language_id: impl Into<String>,
        version: DocumentVersion,
        text: impl Into<String>,
    ) -> Result<(), SyncError> {
        if self.documents.contains_key(&uri) {
            return Err(SyncError::AlreadyOpen(uri));
        }
        let version_i32 = lsp_version(version)?;
        let language_id = language_id.into();
        let text = text.into();
        transport
            .send_notification(OutboundNotification::DidOpen(TextDocumentItem::new(
                uri.clone(),
                language_id.clone(),
                version_i32,
                text.clone(),
            )))
            .map_err(SyncError::Transport)?;
        self.documents.insert(
            uri,
            OpenDocument {
                language_id,
                version,
                text,
            },
        );
        Ok(())
    }

    pub fn change_full<T: LspTransport>(
        &mut self,
        transport: &mut T,
        uri: &Uri,
        version: DocumentVersion,
        text: impl Into<String>,
    ) -> Result<(), SyncError> {
        let document = self
            .documents
            .get(uri)
            .ok_or_else(|| SyncError::NotOpen(uri.clone()))?;
        require_next_version(uri, document.version, version)?;
        let version_i32 = lsp_version(version)?;
        let text = text.into();
        transport
            .send_notification(OutboundNotification::DidChange {
                uri: uri.clone(),
                version: version_i32,
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: text.clone(),
                }],
            })
            .map_err(SyncError::Transport)?;
        let document = self.documents.get_mut(uri).expect("checked above");
        document.version = version;
        document.text = text;
        Ok(())
    }

    pub fn save<T: LspTransport>(&self, transport: &mut T, uri: &Uri) -> Result<(), SyncError> {
        let document = self
            .documents
            .get(uri)
            .ok_or_else(|| SyncError::NotOpen(uri.clone()))?;
        transport
            .send_notification(OutboundNotification::DidSave {
                uri: uri.clone(),
                text: Some(document.text.clone()),
            })
            .map_err(SyncError::Transport)
    }

    pub fn close<T: LspTransport>(
        &mut self,
        transport: &mut T,
        uri: &Uri,
    ) -> Result<(), SyncError> {
        if !self.documents.contains_key(uri) {
            return Err(SyncError::NotOpen(uri.clone()));
        }
        transport
            .send_notification(OutboundNotification::DidClose(TextDocumentIdentifier::new(
                uri.clone(),
            )))
            .map_err(SyncError::Transport)?;
        self.documents.remove(uri);
        Ok(())
    }

    pub fn version(&self, uri: &Uri) -> Option<DocumentVersion> {
        self.documents.get(uri).map(|document| document.version)
    }

    pub fn text(&self, uri: &Uri) -> Option<&str> {
        self.documents
            .get(uri)
            .map(|document| document.text.as_str())
    }

    pub fn language_id(&self, uri: &Uri) -> Option<&str> {
        self.documents
            .get(uri)
            .map(|document| document.language_id.as_str())
    }
}

/// Sends the LSP save notification represented by a successfully completed
/// host save. The core lifecycle remains unaware of LSP and only exposes the
/// exact saved text through [`SaveEvent`].
pub fn send_save_event<T: LspTransport>(
    transport: &mut T,
    uri: &Uri,
    event: &SaveEvent,
) -> Result<(), TransportError> {
    transport.send_notification(OutboundNotification::DidSave {
        uri: uri.clone(),
        text: Some(event.text.clone()),
    })
}

fn lsp_version(version: DocumentVersion) -> Result<i32, SyncError> {
    i32::try_from(version).map_err(|_| SyncError::VersionOverflow(version))
}

fn require_next_version(
    uri: &Uri,
    expected: DocumentVersion,
    actual: DocumentVersion,
) -> Result<(), SyncError> {
    let next = expected
        .checked_add(1)
        .ok_or(SyncError::VersionOverflow(expected))?;
    if actual == next {
        Ok(())
    } else {
        Err(SyncError::VersionMismatch {
            uri: uri.clone(),
            expected: next,
            actual,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestTicket {
    pub id: RequestId,
    pub kind: RequestKind,
    pub uri: Uri,
    pub document_version: DocumentVersion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    Transport(TransportError),
    IdOverflow,
    Unknown(RequestId),
    StaleDocumentVersion {
        id: RequestId,
        expected: DocumentVersion,
        actual: DocumentVersion,
    },
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => error.fmt(f),
            Self::IdOverflow => f.write_str("request id space exhausted"),
            Self::Unknown(id) => write!(f, "unknown or cancelled request {}", id.get()),
            Self::StaleDocumentVersion {
                id,
                expected,
                actual,
            } => write!(
                f,
                "request {} belongs to version {expected}, current version is {actual}",
                id.get()
            ),
        }
    }
}

impl std::error::Error for RequestError {}

#[derive(Debug, Clone)]
struct PendingRequest {
    kind: RequestKind,
    uri: Uri,
    document_version: DocumentVersion,
}

/// Issues deterministic request ids and rejects stale or out-of-order results.
#[derive(Debug)]
pub struct RequestScheduler {
    next_id: u64,
    pending: HashMap<RequestId, PendingRequest>,
}

impl Default for RequestScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestScheduler {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            pending: HashMap::new(),
        }
    }

    pub fn issue<T: LspTransport>(
        &mut self,
        transport: &mut T,
        kind: RequestKind,
        uri: Uri,
        document_version: DocumentVersion,
    ) -> Result<RequestTicket, RequestError> {
        let id = RequestId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(RequestError::IdOverflow)?;
        let request = OutboundRequest {
            id,
            kind,
            uri: uri.clone(),
            document_version,
        };
        transport
            .send_request(request)
            .map_err(RequestError::Transport)?;
        self.pending.insert(
            id,
            PendingRequest {
                kind,
                uri: uri.clone(),
                document_version,
            },
        );
        Ok(RequestTicket {
            id,
            kind,
            uri,
            document_version,
        })
    }

    pub fn issue_completion<T: LspTransport>(
        &mut self,
        transport: &mut T,
        uri: Uri,
        document_version: DocumentVersion,
    ) -> Result<RequestTicket, RequestError> {
        self.issue(transport, RequestKind::Completion, uri, document_version)
    }

    pub fn issue_inline_completion<T: LspTransport>(
        &mut self,
        transport: &mut T,
        uri: Uri,
        document_version: DocumentVersion,
    ) -> Result<RequestTicket, RequestError> {
        self.issue(
            transport,
            RequestKind::InlineCompletion,
            uri,
            document_version,
        )
    }

    pub fn issue_hover<T: LspTransport>(
        &mut self,
        transport: &mut T,
        uri: Uri,
        document_version: DocumentVersion,
    ) -> Result<RequestTicket, RequestError> {
        self.issue(transport, RequestKind::Hover, uri, document_version)
    }

    pub fn issue_definition<T: LspTransport>(
        &mut self,
        transport: &mut T,
        uri: Uri,
        document_version: DocumentVersion,
    ) -> Result<RequestTicket, RequestError> {
        self.issue(transport, RequestKind::Definition, uri, document_version)
    }

    pub fn issue_references<T: LspTransport>(
        &mut self,
        transport: &mut T,
        uri: Uri,
        document_version: DocumentVersion,
    ) -> Result<RequestTicket, RequestError> {
        self.issue(transport, RequestKind::References, uri, document_version)
    }

    pub fn issue_code_action<T: LspTransport>(
        &mut self,
        transport: &mut T,
        uri: Uri,
        document_version: DocumentVersion,
    ) -> Result<RequestTicket, RequestError> {
        self.issue(transport, RequestKind::CodeAction, uri, document_version)
    }

    pub fn issue_semantic_tokens<T: LspTransport>(
        &mut self,
        transport: &mut T,
        uri: Uri,
        document_version: DocumentVersion,
    ) -> Result<RequestTicket, RequestError> {
        self.issue(
            transport,
            RequestKind::SemanticTokens,
            uri,
            document_version,
        )
    }

    pub fn cancel<T: LspTransport>(
        &mut self,
        transport: &mut T,
        ticket: RequestTicket,
    ) -> Result<(), RequestError> {
        if self.pending.remove(&ticket.id).is_none() {
            return Err(RequestError::Unknown(ticket.id));
        }
        transport
            .send_notification(OutboundNotification::Cancel(ticket.id))
            .map_err(RequestError::Transport)
    }

    pub fn accept<T>(
        &mut self,
        ticket: RequestTicket,
        current_document_version: DocumentVersion,
        result: T,
    ) -> Result<T, RequestError> {
        let pending = self
            .pending
            .remove(&ticket.id)
            .ok_or(RequestError::Unknown(ticket.id))?;
        debug_assert_eq!(pending.kind, ticket.kind);
        debug_assert_eq!(pending.uri, ticket.uri);
        if pending.document_version != current_document_version {
            return Err(RequestError::StaleDocumentVersion {
                id: ticket.id,
                expected: pending.document_version,
                actual: current_document_version,
            });
        }
        Ok(result)
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrationError {
    StaleDocumentVersion {
        expected: DocumentVersion,
        actual: DocumentVersion,
    },
    InvalidLspVersion(i32),
    Position(PositionError),
    Diagnostics(DiagnosticError),
    ArithmeticOverflow,
}

impl fmt::Display for IntegrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleDocumentVersion { expected, actual } => {
                write!(f, "expected document version {expected}, got {actual}")
            }
            Self::InvalidLspVersion(version) => write!(f, "invalid LSP document version {version}"),
            Self::Position(error) => error.fmt(f),
            Self::Diagnostics(error) => error.fmt(f),
            Self::ArithmeticOverflow => f.write_str("LSP position arithmetic overflowed"),
        }
    }
}

impl std::error::Error for IntegrationError {}

impl From<PositionError> for IntegrationError {
    fn from(error: PositionError) -> Self {
        Self::Position(error)
    }
}

impl From<DiagnosticError> for IntegrationError {
    fn from(error: DiagnosticError) -> Self {
        Self::Diagnostics(error)
    }
}

/// Converts an LSP position using UTF-16 code units into the core's byte offset.
pub fn lsp_position_to_byte(
    snapshot: &DocumentSnapshot,
    position: Position,
) -> Result<usize, IntegrationError> {
    snapshot
        .position_map()
        .utf16_position_to_byte(
            snapshot.text(),
            TextPosition {
                line: usize::try_from(position.line)
                    .map_err(|_| IntegrationError::ArithmeticOverflow)?,
                column: usize::try_from(position.character)
                    .map_err(|_| IntegrationError::ArithmeticOverflow)?,
            },
        )
        .map_err(IntegrationError::Position)
}

pub fn lsp_range_to_byte_range(
    snapshot: &DocumentSnapshot,
    range: Range,
) -> Result<ByteRange<usize>, IntegrationError> {
    let start = lsp_position_to_byte(snapshot, range.start)?;
    let end = lsp_position_to_byte(snapshot, range.end)?;
    if start > end {
        return Err(IntegrationError::ArithmeticOverflow);
    }
    Ok(start..end)
}

fn core_severity(severity: Option<LspDiagnosticSeverity>) -> DiagnosticSeverity {
    match severity {
        Some(LspDiagnosticSeverity::ERROR) => DiagnosticSeverity::Error,
        Some(LspDiagnosticSeverity::WARNING) => DiagnosticSeverity::Warning,
        Some(LspDiagnosticSeverity::HINT) => DiagnosticSeverity::Hint,
        Some(LspDiagnosticSeverity::INFORMATION) | None => DiagnosticSeverity::Info,
        Some(_) => DiagnosticSeverity::Info,
    }
}

/// Publishes a version-checked LSP diagnostic notification into the core set.
pub fn publish_diagnostics(
    set: &mut DiagnosticSet,
    snapshot: &DocumentSnapshot,
    params: PublishDiagnosticsParams,
) -> Result<(), IntegrationError> {
    if let Some(version) = params.version {
        let version =
            u64::try_from(version).map_err(|_| IntegrationError::InvalidLspVersion(version))?;
        if version != snapshot.version() {
            return Err(IntegrationError::StaleDocumentVersion {
                expected: snapshot.version(),
                actual: version,
            });
        }
    }
    let diagnostics = params
        .diagnostics
        .into_iter()
        .map(|diagnostic| lsp_diagnostic_to_core(snapshot, diagnostic))
        .collect::<Result<Vec<_>, _>>()?;
    set.publish(snapshot, diagnostics)?;
    Ok(())
}

fn lsp_diagnostic_to_core(
    snapshot: &DocumentSnapshot,
    diagnostic: LspDiagnostic,
) -> Result<CoreDiagnostic, IntegrationError> {
    let range = lsp_range_to_byte_range(snapshot, diagnostic.range)?;
    let mut result = CoreDiagnostic::new(
        range,
        core_severity(diagnostic.severity),
        diagnostic.message,
    );
    if let Some(source) = diagnostic.source {
        result = result.source(source);
    }
    Ok(result)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticDecoration {
    pub range: ByteRange<usize>,
    pub token_type: u32,
    pub modifiers: u32,
}

/// Decodes the LSP delta stream into renderer-neutral byte-range decorations.
pub fn semantic_token_decorations(
    snapshot: &DocumentSnapshot,
    tokens: &SemanticTokens,
) -> Result<Vec<SemanticDecoration>, IntegrationError> {
    let mut line = 0u32;
    let mut start = 0u32;
    let mut decorations = Vec::with_capacity(tokens.data.len());
    for token in &tokens.data {
        line = line
            .checked_add(token.delta_line)
            .ok_or(IntegrationError::ArithmeticOverflow)?;
        start = if token.delta_line == 0 {
            start
                .checked_add(token.delta_start)
                .ok_or(IntegrationError::ArithmeticOverflow)?
        } else {
            token.delta_start
        };
        let end_character = start
            .checked_add(token.length)
            .ok_or(IntegrationError::ArithmeticOverflow)?;
        let range = lsp_range_to_byte_range(
            snapshot,
            Range::new(
                Position::new(line, start),
                Position::new(line, end_character),
            ),
        )?;
        decorations.push(SemanticDecoration {
            range,
            token_type: token.token_type,
            modifiers: token.token_modifiers_bitset,
        });
    }
    Ok(decorations)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteLocation {
    pub uri: Uri,
    pub range: ByteRange<usize>,
}

pub fn location_to_byte_location(
    location: &Location,
    snapshot: &DocumentSnapshot,
) -> Result<ByteLocation, IntegrationError> {
    Ok(ByteLocation {
        uri: location.uri.clone(),
        range: lsp_range_to_byte_range(snapshot, location.range)?,
    })
}

pub fn location_link_to_byte_location(
    location: &LocationLink,
    snapshot: &DocumentSnapshot,
) -> Result<ByteLocation, IntegrationError> {
    Ok(ByteLocation {
        uri: location.target_uri.clone(),
        range: lsp_range_to_byte_range(snapshot, location.target_range)?,
    })
}

/// Converts definition responses, references, and navigation links using the same seam.
pub fn definition_response_to_byte_locations(
    response: GotoDefinitionResponse,
    snapshot: &DocumentSnapshot,
) -> Result<Vec<ByteLocation>, IntegrationError> {
    match response {
        GotoDefinitionResponse::Scalar(location) => {
            Ok(vec![location_to_byte_location(&location, snapshot)?])
        }
        GotoDefinitionResponse::Array(locations) => locations
            .iter()
            .map(|location| location_to_byte_location(location, snapshot))
            .collect(),
        GotoDefinitionResponse::Link(locations) => locations
            .iter()
            .map(|location| location_link_to_byte_location(location, snapshot))
            .collect(),
    }
}

pub fn references_to_byte_locations(
    locations: &[Location],
    snapshot: &DocumentSnapshot,
) -> Result<Vec<ByteLocation>, IntegrationError> {
    locations
        .iter()
        .map(|location| location_to_byte_location(location, snapshot))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceEditError {
    MissingDocument(Uri),
    StaleVersion {
        uri: Uri,
        expected: DocumentVersion,
        actual: DocumentVersion,
    },
    UnsupportedResourceOperation,
    Position(IntegrationError),
    InvalidEdit(TransactionError),
    VersionOverflow(DocumentVersion),
}

impl fmt::Display for WorkspaceEditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDocument(uri) => {
                write!(f, "workspace edit targets unopened document {uri:?}")
            }
            Self::StaleVersion {
                uri,
                expected,
                actual,
            } => write!(
                f,
                "workspace edit for {uri:?} expects version {expected}, got {actual}"
            ),
            Self::UnsupportedResourceOperation => {
                f.write_str("resource operations are not supported")
            }
            Self::Position(error) => error.fmt(f),
            Self::InvalidEdit(error) => error.fmt(f),
            Self::VersionOverflow(version) => write!(f, "document version {version} overflowed"),
        }
    }
}

impl std::error::Error for WorkspaceEditError {}

/// An editable core document with an explicit host-side version.
#[derive(Debug, Clone)]
pub struct EditableDocument {
    state: EditorState,
    version: DocumentVersion,
}

/// Workspace documents keyed internally by their stable URI text.
///
/// `lsp-types::Uri` intentionally carries cached parser state, so keeping its
/// string form as the map key avoids mutable-key aliasing concerns while the
/// public API continues to use the protocol's `Uri` type.
#[derive(Debug, Default, Clone)]
pub struct WorkspaceDocuments {
    documents: HashMap<String, (Uri, EditableDocument)>,
}

impl WorkspaceDocuments {
    pub fn insert(&mut self, uri: Uri, document: EditableDocument) -> Option<EditableDocument> {
        self.documents
            .insert(uri.as_str().to_owned(), (uri, document))
            .map(|(_, document)| document)
    }

    pub fn get(&self, uri: &Uri) -> Option<&EditableDocument> {
        self.documents
            .get(uri.as_str())
            .map(|(_, document)| document)
    }

    fn get_mut(&mut self, uri: &Uri) -> Option<&mut EditableDocument> {
        self.documents
            .get_mut(uri.as_str())
            .map(|(_, document)| document)
    }
}

impl EditableDocument {
    pub fn new(text: impl Into<String>, version: DocumentVersion) -> Self {
        Self {
            state: EditorState::new(text),
            version,
        }
    }

    pub fn text(&self) -> &str {
        self.state.document().text()
    }

    pub fn version(&self) -> DocumentVersion {
        self.version
    }

    pub fn snapshot(&self) -> DocumentSnapshot {
        self.state.snapshot()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedWorkspaceEdit {
    pub uri: Uri,
    pub before_version: DocumentVersion,
    pub after_version: DocumentVersion,
}

/// Applies only textual workspace edits, checking optional LSP document versions.
pub fn apply_workspace_edit(
    edit: &WorkspaceEdit,
    documents: &mut WorkspaceDocuments,
) -> Result<Vec<AppliedWorkspaceEdit>, WorkspaceEditError> {
    let mut edits_by_uri: HashMap<String, (Uri, Option<i32>, Vec<TextEdit>)> = HashMap::new();
    if let Some(changes) = &edit.changes {
        for (uri, edits) in changes {
            edits_by_uri
                .entry(uri.as_str().to_owned())
                .or_insert_with(|| (uri.clone(), None, Vec::new()))
                .2
                .extend(edits.clone());
        }
    }
    if let Some(document_changes) = &edit.document_changes {
        match document_changes {
            DocumentChanges::Edits(changes) => {
                for change in changes {
                    let edits = change
                        .edits
                        .iter()
                        .map(one_of_text_edit)
                        .collect::<Result<Vec<_>, _>>()?;
                    let entry = edits_by_uri
                        .entry(change.text_document.uri.as_str().to_owned())
                        .or_insert_with(|| {
                            (
                                change.text_document.uri.clone(),
                                change.text_document.version,
                                Vec::new(),
                            )
                        });
                    if entry.1.is_none() {
                        entry.1 = change.text_document.version;
                    }
                    entry.2.extend(edits);
                }
            }
            DocumentChanges::Operations(_) => {
                return Err(WorkspaceEditError::UnsupportedResourceOperation)
            }
        }
    }

    let mut applied = Vec::with_capacity(edits_by_uri.len());
    for (_, (uri, expected_lsp_version, edits)) in edits_by_uri {
        let document = documents
            .get_mut(&uri)
            .ok_or_else(|| WorkspaceEditError::MissingDocument(uri.clone()))?;
        if let Some(expected) = expected_lsp_version {
            let expected =
                u64::try_from(expected).map_err(|_| WorkspaceEditError::StaleVersion {
                    uri: uri.clone(),
                    expected: 0,
                    actual: document.version,
                })?;
            if expected != document.version {
                return Err(WorkspaceEditError::StaleVersion {
                    uri,
                    expected,
                    actual: document.version,
                });
            }
        }
        let snapshot = document.snapshot();
        let after_version = document
            .version
            .checked_add(1)
            .ok_or(WorkspaceEditError::VersionOverflow(document.version))?;
        let mut transaction = Transaction::new();
        for edit in edits {
            let range = lsp_range_to_byte_range(&snapshot, edit.range)
                .map_err(WorkspaceEditError::Position)?;
            transaction.push(Edit::replace(range, edit.new_text));
        }
        let before_version = document.version;
        document
            .state
            .apply_with_result(&transaction, Grouping::Separate)
            .map_err(WorkspaceEditError::InvalidEdit)?;
        document.version = after_version;
        applied.push(AppliedWorkspaceEdit {
            uri,
            before_version,
            after_version,
        });
    }
    Ok(applied)
}

fn one_of_text_edit(
    edit: &OneOf<TextEdit, AnnotatedTextEdit>,
) -> Result<TextEdit, WorkspaceEditError> {
    Ok(match edit {
        OneOf::Left(edit) => edit.clone(),
        OneOf::Right(edit) => edit.text_edit.clone(),
    })
}

/// Applies the edit carried by a code action; command-only actions stay host-owned.
pub fn apply_code_action(
    action: &CodeActionOrCommand,
    documents: &mut WorkspaceDocuments,
) -> Result<Option<Vec<AppliedWorkspaceEdit>>, WorkspaceEditError> {
    match action {
        CodeActionOrCommand::Command(_) => Ok(None),
        CodeActionOrCommand::CodeAction(action) => action
            .edit
            .as_ref()
            .map(|edit| apply_workspace_edit(edit, documents))
            .transpose(),
    }
}

/// A deterministic fake transport useful to host tests and integration tests.
#[derive(Debug, Default)]
pub struct RecordingTransport {
    pub notifications: Vec<OutboundNotification>,
    pub requests: Vec<OutboundRequest>,
    pub fail_next: Option<String>,
}

impl LspTransport for RecordingTransport {
    fn send_notification(
        &mut self,
        notification: OutboundNotification,
    ) -> Result<(), TransportError> {
        if let Some(message) = self.fail_next.take() {
            return Err(TransportError(message));
        }
        self.notifications.push(notification);
        Ok(())
    }

    fn send_request(&mut self, request: OutboundRequest) -> Result<(), TransportError> {
        if let Some(message) = self.fail_next.take() {
            return Err(TransportError(message));
        }
        self.requests.push(request);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{
        CodeAction, CompletionItem, CompletionList, DocumentChanges,
        OptionalVersionedTextDocumentIdentifier, SemanticToken, TextDocumentEdit,
    };

    fn uri(value: &str) -> Uri {
        value.parse().expect("valid URI")
    }

    #[test]
    fn synchronizes_lifecycle_with_explicit_versions() {
        let document_uri = uri("file:///main.rs");
        let mut sync = DocumentSync::default();
        let mut transport = RecordingTransport::default();
        sync.open(
            &mut transport,
            document_uri.clone(),
            "rust",
            0,
            "fn main() {}",
        )
        .unwrap();
        sync.change_full(&mut transport, &document_uri, 1, "fn main() { 1 }")
            .unwrap();
        sync.save(&mut transport, &document_uri).unwrap();
        sync.close(&mut transport, &document_uri).unwrap();
        assert_eq!(transport.notifications.len(), 4);
        assert!(matches!(
            sync.change_full(&mut transport, &document_uri, 2, ""),
            Err(SyncError::NotOpen(_))
        ));
    }

    #[test]
    fn core_save_receipt_emits_did_save_notification() {
        let mut session = mockaco_core::DocumentSession::new(
            Some(mockaco_core::DocumentLocation::new("doc://main")),
            "x",
            mockaco_core::DocumentMetadata::detect_utf8("x"),
        )
        .unwrap();
        session.apply_edit(0..1, "saved").unwrap();
        let request = session.request_save().unwrap();
        let receipt = match session.complete_save(
            request.id,
            mockaco_core::HostSaveResult::Success {
                location: mockaco_core::DocumentLocation::new("doc://main"),
            },
        ) {
            mockaco_core::SaveCompletion::Saved(receipt) => receipt,
            completion => panic!("expected saved receipt, got {completion:?}"),
        };

        let mut transport = RecordingTransport::default();
        let document_uri = uri("file:///main.txt");
        send_save_event(&mut transport, &document_uri, &receipt.event()).unwrap();

        assert!(matches!(
            transport.notifications.as_slice(),
            [OutboundNotification::DidSave { uri, text: Some(text) }]
                if uri == &document_uri && text == "saved"
        ));
    }

    #[test]
    fn scheduler_rejects_cancelled_and_stale_out_of_order_results() {
        let document_uri = uri("file:///main.rs");
        let mut scheduler = RequestScheduler::new();
        let mut transport = RecordingTransport::default();
        let first = scheduler
            .issue(&mut transport, RequestKind::Hover, document_uri.clone(), 1)
            .unwrap();
        let second = scheduler
            .issue(&mut transport, RequestKind::Completion, document_uri, 2)
            .unwrap();
        assert_eq!(first.id.get(), 1);
        assert_eq!(second.id.get(), 2);
        assert!(matches!(
            scheduler.accept(first.clone(), 2, "old"),
            Err(RequestError::StaleDocumentVersion { .. })
        ));
        assert_eq!(scheduler.accept(second, 2, "new").unwrap(), "new");
        assert!(matches!(
            scheduler.accept(first, 2, "again"),
            Err(RequestError::Unknown(_))
        ));

        let cancelled = scheduler
            .issue_hover(&mut transport, uri("file:///main.rs"), 2)
            .unwrap();
        scheduler.cancel(&mut transport, cancelled.clone()).unwrap();
        assert!(matches!(
            scheduler.accept(cancelled, 2, "cancelled"),
            Err(RequestError::Unknown(_))
        ));
        assert!(matches!(
            transport.notifications.last(),
            Some(OutboundNotification::Cancel(_))
        ));
    }

    #[test]
    fn diagnostics_are_published_with_utf16_ranges_and_versions() {
        let snapshot = mockaco_core::Document::new("🙂 warning").snapshot();
        let mut set = DiagnosticSet::new(0);
        let params = PublishDiagnosticsParams::new(
            uri("file:///main.rs"),
            vec![LspDiagnostic::new(
                Range::new(Position::new(0, 3), Position::new(0, 10)),
                Some(LspDiagnosticSeverity::WARNING),
                None,
                None,
                "warning".into(),
                None,
                None,
            )],
            Some(0),
        );
        publish_diagnostics(&mut set, &snapshot, params).unwrap();
        assert_eq!(set.diagnostics()[0].range, 5..12);
        assert_eq!(set.diagnostics()[0].severity, DiagnosticSeverity::Warning);
    }

    #[test]
    fn semantic_delta_tokens_become_byte_ranges() {
        let snapshot = mockaco_core::Document::new("🙂 let").snapshot();
        let tokens = SemanticTokens {
            result_id: None,
            data: vec![SemanticToken {
                delta_line: 0,
                delta_start: 2,
                length: 3,
                token_type: 4,
                token_modifiers_bitset: 1,
            }],
        };
        let decorations = semantic_token_decorations(&snapshot, &tokens).unwrap();
        assert_eq!(decorations[0].range, 4..7);
        assert_eq!(decorations[0].token_type, 4);
    }

    #[test]
    fn navigation_and_workspace_edits_use_core_byte_offsets() {
        let document_uri = uri("file:///main.rs");
        let snapshot = mockaco_core::Document::new("🙂 let value").snapshot();
        let location = Location::new(
            document_uri.clone(),
            Range::new(Position::new(0, 3), Position::new(0, 6)),
        );
        let mapped = location_to_byte_location(&location, &snapshot).unwrap();
        assert_eq!(mapped.range, 5..8);

        let mut documents = WorkspaceDocuments::default();
        documents.insert(document_uri.clone(), EditableDocument::new("🙂 let", 3));
        let edits = vec![OneOf::Left(TextEdit::new(
            Range::new(Position::new(0, 3), Position::new(0, 6)),
            "const".into(),
        ))];
        let workspace_edit = WorkspaceEdit {
            changes: None,
            document_changes: Some(DocumentChanges::Edits(vec![TextDocumentEdit {
                text_document: OptionalVersionedTextDocumentIdentifier::new(
                    document_uri.clone(),
                    3,
                ),
                edits,
            }])),
            change_annotations: None,
        };
        let applied = apply_workspace_edit(&workspace_edit, &mut documents).unwrap();
        assert_eq!(applied[0].after_version, 4);
        assert_eq!(documents.get(&document_uri).unwrap().text(), "🙂 const");
    }

    #[test]
    fn completion_and_code_action_types_are_real_lsp_values() {
        let completion = CompletionResponse::List(CompletionList {
            is_incomplete: false,
            items: vec![CompletionItem::new_simple("main".into(), "main".into())],
        });
        assert!(matches!(completion, CompletionResponse::List(_)));
        let action = CodeActionOrCommand::CodeAction(CodeAction {
            title: "fix".into(),
            ..Default::default()
        });
        let mut documents = WorkspaceDocuments::default();
        assert_eq!(apply_code_action(&action, &mut documents).unwrap(), None);
    }
}
