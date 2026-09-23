use mockaco_core::{
    CloseDecision, Diagnostic, DiagnosticSet, DiagnosticSeverity, Document, DocumentMetadata,
    DocumentSession, Edit, EditorState, EncodingMetadata, ExternalChangeDecision, Grouping,
    HostSaveError, HostSaveResult, LoadError, SaveCompletion, SearchQuery, SearchSession,
    Selection, SelectionSet, Transaction,
};
use mockaco_diff::{compute_diff, DiffEditor, DiffRowKind, DiffSide};
use mockaco_fixtures::{pathological_document, session, snapshot, PLAIN_FIXTURE, RUST_FIXTURE};
use mockaco_gpui::{EditorSurface, SurfaceGeometry};
use mockaco_language::{
    HighlightResult, IncrementalHighlights, LanguageConfig, LanguageRegistry, SyntaxToken,
};
use mockaco_lsp::{
    send_save_event, DocumentSync, OutboundNotification, RecordingTransport, RequestKind,
    RequestScheduler,
};
use mockaco_renderer::{
    diagnostic_decorations, search_decorations, DisplayConfig, DisplayMap, FoldRegion, FoldSet,
    WrapConfig,
};

#[test]
fn editing_selection_ime_and_undo_are_fixture_locked() {
    let mut editor = EditorState::new("one two");
    editor.set_selections(SelectionSet::new([
        Selection::caret(0),
        Selection::caret("one two".len()),
    ]));
    editor
        .apply(
            &Transaction::from_edits([Edit::insert(0, "x"), Edit::insert(7, "!")]),
            Grouping::Separate,
        )
        .unwrap();
    assert_eq!(editor.document().text(), "xone two!");

    editor.update_composition(1..1, "😀").unwrap();
    assert_eq!(editor.document().text(), "xone two!");
    editor.commit_composition().unwrap();
    assert!(editor.document().text().contains("😀"));
    assert!(editor.undo());
    assert_eq!(editor.document().text(), "xone two!");
    assert!(editor.redo());
    assert!(editor.document().text().contains("😀"));
    editor.update_composition(1..5, "cancelled").unwrap();
    assert!(editor.cancel_composition());
    assert!(editor.pending_composition().is_none());
}

#[test]
fn search_replace_folding_wrapping_and_pathological_lines_are_bounded() {
    let mut editor = EditorState::new(PLAIN_FIXTURE);
    let mut search = SearchSession::new(&editor.snapshot(), SearchQuery::new("alpha"));
    assert_eq!(search.matches().len(), 2);
    let replacement = search.replace_all(&mut editor, "omega").unwrap().unwrap();
    assert_eq!(replacement.replaced_ranges.len(), 2);
    assert_eq!(editor.document().text(), "omega\nbeta\nomega\n");

    let document = Document::new("one\ntwo\nthree\nfour");
    let folded = DisplayMap::with_folds(
        &document.snapshot(),
        DisplayConfig::wrapped(4),
        FoldSet::new([FoldRegion::new(0, 3).placeholder("...")]),
    );
    assert_eq!(folded.row_count(), 2);

    let pathological = Document::new(pathological_document(1_048_577));
    let bounded = DisplayMap::new(
        &pathological.snapshot(),
        DisplayConfig::wrapped(80).with_wrap(Some(
            WrapConfig::new(80)
                .max_segments_per_line(8)
                .max_line_scan_bytes(512),
        )),
    );
    assert!(bounded.row_count() <= 9);
    assert!(bounded.rows().iter().any(|row| row.truncated));
}

#[test]
fn diagnostics_and_incremental_language_results_reject_stale_versions() {
    let mut editor = EditorState::new(RUST_FIXTURE);
    let before = editor.snapshot();
    let mut diagnostics = DiagnosticSet::new(before.version());
    diagnostics
        .publish(
            &before,
            [Diagnostic::new(0..2, DiagnosticSeverity::Error, "bad")],
        )
        .unwrap();
    assert_eq!(diagnostic_decorations(&diagnostics).len(), 1);

    let mut highlights = IncrementalHighlights::from_result(HighlightResult {
        version: before.version(),
        range: 0..2,
        tokens: vec![SyntaxToken::new(0..2, "keyword")],
    });
    let applied = editor
        .apply_with_result(&Transaction::new().insert(0, "// "), Grouping::Separate)
        .unwrap();
    highlights
        .apply_transaction(&editor.snapshot(), &applied)
        .unwrap();
    assert!(highlights
        .accept(HighlightResult {
            version: before.version(),
            range: 0..2,
            tokens: Vec::new(),
        })
        .is_err());
}

#[test]
fn language_registry_and_renderer_decoration_contracts_are_stable() {
    let mut registry = LanguageRegistry::new();
    registry
        .register(
            LanguageConfig::new("Rust")
                .extension("rs")
                .line_comment("//")
                .tab_width(4),
        )
        .unwrap();
    assert_eq!(
        registry
            .language_for_path("src/main.RS")
            .unwrap()
            .id
            .as_str(),
        "rust"
    );

    let document = Document::new("alpha beta alpha");
    let mut search = SearchSession::new(&document.snapshot(), SearchQuery::new("alpha"));
    search.next_match();
    assert_eq!(search_decorations(&search).len(), 2);
    let mut diagnostics = DiagnosticSet::new(document.version());
    diagnostics
        .publish(
            &document.snapshot(),
            [Diagnostic::new(
                6..10,
                DiagnosticSeverity::Warning,
                "warning",
            )],
        )
        .unwrap();
    assert_eq!(diagnostic_decorations(&diagnostics)[0].range, 6..10);
}

#[test]
fn lsp_sync_versioning_cancellation_and_save_event_are_covered() {
    let uri: lsp_types::Uri = "file:///fixture.rs".parse().unwrap();
    let mut transport = RecordingTransport::default();
    let mut sync = DocumentSync::default();
    sync.open(&mut transport, uri.clone(), "rust", 0, RUST_FIXTURE)
        .unwrap();
    sync.change_full(&mut transport, &uri, 1, "changed")
        .unwrap();
    assert!(sync.change_full(&mut transport, &uri, 1, "stale").is_err());

    let mut scheduler = RequestScheduler::new();
    let ticket = scheduler
        .issue(&mut transport, RequestKind::Hover, uri.clone(), 1)
        .unwrap();
    assert!(scheduler.accept(ticket.clone(), 2, ()).is_err());
    let cancelled = scheduler
        .issue(&mut transport, RequestKind::Completion, uri.clone(), 1)
        .unwrap();
    scheduler.cancel(&mut transport, cancelled).unwrap();

    let mut document = session("old");
    document.apply_edit(0..3, "new").unwrap();
    let request = document.request_save().unwrap();
    let receipt = match document.complete_save(
        request.id,
        HostSaveResult::Success {
            location: "fixture://saved".into(),
        },
    ) {
        SaveCompletion::Saved(receipt) => receipt,
        other => panic!("unexpected completion: {other:?}"),
    };
    send_save_event(&mut transport, &uri, &receipt.event()).unwrap();
    assert!(matches!(
        transport.notifications.last(),
        Some(OutboundNotification::DidSave { text: Some(text), .. }) if text == "new"
    ));
}

#[test]
fn split_diff_mapping_and_stale_result_rejection_are_covered() {
    let original = snapshot("one\ntwo\nthree\n");
    let mut editor = DiffEditor::new(&original, "one\nchanged\nthree\n");
    assert!(editor
        .diff()
        .rows()
        .iter()
        .any(|row| row.kind == DiffRowKind::Replace));
    let stale = compute_diff(&snapshot("old\n"), &snapshot("new\n"));
    editor.apply_modified_edit(0..1, "changed").unwrap();
    assert!(editor.accept_diff(stale).is_err());
    assert!(editor.diff().row_for_line(DiffSide::Modified, 1).is_some());
}

#[test]
fn host_lifecycle_save_failure_external_conflict_and_close_policy_are_typed() {
    let mut document = session("original");
    document.apply_edit(0..0, "local ").unwrap();
    let request = document.request_save().unwrap();
    assert!(matches!(
        document.complete_save(
            request.id,
            HostSaveResult::Failure(HostSaveError::new("read-only")),
        ),
        SaveCompletion::Failed {
            still_dirty: true,
            ..
        }
    ));
    assert!(document.is_dirty());
    assert!(matches!(
        document.notify_external_change("remote", DocumentMetadata::detect_utf8("remote")),
        Ok(mockaco_core::ExternalChangeOutcome::Conflict(_))
    ));
    assert!(matches!(
        document.resolve_external_change(ExternalChangeDecision::KeepLocal),
        Ok(mockaco_core::ExternalChangeOutcome::KeptLocal)
    ));
    assert!(matches!(
        document.close(CloseDecision::Save),
        Ok(mockaco_core::CloseOutcome::SaveRequired(_))
    ));
}

#[test]
fn load_encoding_and_utf16_fixture_contracts_are_preserved() {
    let mut document = session("safe");
    let before = document.snapshot();
    assert!(matches!(
        document.replace_from_utf8_bytes(None, &[0xff]),
        Err(LoadError::InvalidUtf8 { .. })
    ));
    assert_eq!(document.snapshot(), before);
    let metadata = DocumentMetadata::host_decoded("a\rb", "windows-1252", true);
    document
        .replace_from_host_decoded(None, "a\rb", metadata.clone())
        .unwrap();
    assert_eq!(document.metadata(), &metadata);
    assert!(matches!(
        DocumentSession::new(
            None,
            "binary",
            DocumentMetadata {
                encoding: EncodingMetadata::Unsupported {
                    label: "bin".into(),
                    byte_len: 6
                },
                newline: mockaco_core::NewlineStyle::None,
                trailing_newline: false,
            },
        ),
        Err(LoadError::UnsupportedEncoding { .. })
    ));
}

#[test]
fn standalone_surface_exercises_viewport_rendering_without_a_window() {
    let mut surface = EditorSurface::new(
        RUST_FIXTURE,
        DisplayConfig::wrapped(40),
        SurfaceGeometry {
            width: 640.0,
            height: 240.0,
            line_height: 8.0,
            character_width: 4.0,
            gutter_character_width: 4.0,
        },
    );
    surface.set_viewport(8, 80);
    let frame = surface.render_frame();
    assert!(!frame.rows.is_empty());
    assert!(frame.rows.len() <= 8);
    assert!(surface
        .editor_mut()
        .apply(&Transaction::new().insert(0, "// "), Grouping::Separate)
        .is_ok());
}

#[test]
fn fixture_corpus_contains_both_source_and_plain_text_cases() {
    assert!(RUST_FIXTURE.contains("fn main"));
    assert_eq!(PLAIN_FIXTURE.lines().count(), 3);
    assert_eq!(mockaco_fixtures::fixture_names(), ["rust", "plain"]);
}
