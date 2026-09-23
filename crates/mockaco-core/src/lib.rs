//! Framework-independent editing primitives for Mockaco.
//!
//! The crate deliberately has no UI, parser, language-server, filesystem, or
//! host-application dependencies. Coordinates are byte offsets unless an API
//! explicitly says UTF-16 or line/column.

mod diagnostics;
mod document;
mod editor_state;
mod lifecycle;
mod position_map;
mod search;
mod selection;
mod transaction;
mod undo;

pub use diagnostics::{Diagnostic, DiagnosticError, DiagnosticSet, DiagnosticSeverity};
pub use document::{Document, DocumentError, DocumentSnapshot};
pub use editor_state::{EditorState, ImeComposition, ImeError, ViewportIntent};
pub use lifecycle::{
    CloseDecision, CloseOutcome, ContentIdentity, DocumentLocation, DocumentMetadata,
    DocumentSession, EncodingMetadata, ExternalChange, ExternalChangeDecision, ExternalChangeError,
    ExternalChangeOutcome, HostSaveError, HostSaveResult, LoadError, LoadReceipt, NewlineStyle,
    SaveCompletion, SaveEvent, SaveKind, SaveReceipt, SaveRequest, SaveRequestError, SaveRequestId,
};
pub use position_map::{
    ByteOffset, ColumnEncoding, LineColumn, Position, PositionError, PositionMap, TextPosition,
    Utf16Offset,
};
pub use search::{CaseSensitivity, ReplaceResult, SearchError, SearchQuery, SearchSession};
pub use selection::{Selection, SelectionSet};
pub use transaction::{
    Affinity, AppliedTransaction, ChangeMap, Edit, EditError, Transaction, TransactionError,
};
pub use undo::{Grouping, HistoryAction, HistoryEntry, UndoHistory};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_utf16_and_line_coordinates_round_trip() {
        let text = "a😀e\r\ncombining e\u{301}\nlast";
        let map = PositionMap::new(text);
        let emoji_end = "a😀".len();
        assert_eq!(map.byte_to_utf16(text, emoji_end).unwrap(), 3);
        assert_eq!(map.utf16_to_byte(text, 3).unwrap(), emoji_end);
        assert_eq!(
            map.byte_to_utf16_position(text, emoji_end).unwrap(),
            TextPosition { line: 0, column: 3 }
        );
        assert_eq!(
            map.utf16_position_to_byte(text, TextPosition { line: 0, column: 3 })
                .unwrap(),
            emoji_end
        );
        assert_eq!(map.line_count(), 3);
        assert_eq!(map.line_end(0).unwrap(), "a😀e".len());
        let e_combining = "combining e".len();
        assert_eq!(
            map.line_column_to_byte(LineColumn {
                line: 1,
                column: e_combining
            })
            .unwrap(),
            "a😀e\r\n".len() + e_combining
        );
        assert!(map
            .utf16_position_to_byte(text, TextPosition { line: 0, column: 2 })
            .is_err());
    }

    #[test]
    fn transaction_applies_and_maps_multiple_edits() {
        let mut document = Document::new("hello world");
        let transaction = Transaction::new()
            .replace(0..5, "hi")
            .insert(5, " brave")
            .delete(10..11);
        let applied = document.apply(&transaction).unwrap();
        assert_eq!(document.text(), "hi brave worl");
        assert_eq!(applied.change_map.map_offset(5, Affinity::Before), 2);
        assert_eq!(applied.change_map.map_offset(5, Affinity::After), 8);
        assert_eq!(applied.change_map.map_range(6..10), 9..13);
        let mut restored = Document::new(document.text());
        restored.apply(&applied.inverse).unwrap();
        assert_eq!(restored.text(), "hello world");
    }

    #[test]
    fn edit_mapping_property_holds_for_every_ascii_insertion_point() {
        for length in 0..32 {
            let source: String = (0..length)
                .map(|index| (b'a' + (index % 26) as u8) as char)
                .collect();
            for offset in 0..=length {
                let transaction = Transaction::new().insert(offset, "XYZ");
                let applied = transaction.apply(&source, 0).unwrap();
                assert_eq!(
                    applied.change_map.map_offset(offset, Affinity::Before),
                    offset
                );
                assert_eq!(
                    applied.change_map.map_offset(offset, Affinity::After),
                    offset + 3
                );
                let mut restored = Document::new(&applied.text);
                restored.apply(&applied.inverse).unwrap();
                assert_eq!(restored.text(), source);
            }
        }
    }

    #[test]
    fn selections_preserve_direction_and_map_insertions() {
        let selections = SelectionSet::new([Selection::range(2, 4), Selection::range(8, 5)]);
        let transaction = Transaction::new().insert(3, "xyz");
        let mapped = selections.map(&transaction.apply("0123456789", 0).unwrap().change_map);
        assert_eq!(mapped.selections()[0], Selection::range(2, 7));
        assert_eq!(mapped.selections()[1], Selection::range(11, 8));
    }

    #[test]
    fn snapshots_are_cheap_and_stable() {
        let mut document = Document::new("before");
        let snapshot = document.snapshot();
        document
            .apply(&Transaction::new().replace(0..6, "after"))
            .unwrap();
        assert_eq!(snapshot.text(), "before");
        assert_eq!(snapshot.version(), 0);
        assert_eq!(document.version(), 1);
    }

    #[test]
    fn grouped_history_and_branching_are_deterministic() {
        let mut state = EditorState::new("");
        state.begin_undo_group();
        state
            .apply(&Transaction::new().insert(0, "a"), Grouping::Separate)
            .unwrap();
        state
            .apply(&Transaction::new().insert(1, "b"), Grouping::Separate)
            .unwrap();
        state.end_undo_group();
        assert_eq!(state.undo_history().len(), 1);
        assert_eq!(state.document().text(), "ab");
        assert!(state.undo());
        assert_eq!(state.document().text(), "");
        assert!(state.redo());
        assert_eq!(state.document().text(), "ab");
        assert!(state.undo());
        state
            .apply(&Transaction::new().insert(0, "x"), Grouping::Separate)
            .unwrap();
        assert!(!state.undo_history().can_redo());
        assert_eq!(state.document().text(), "x");
    }

    #[test]
    fn ime_composition_is_pending_until_commit_and_undoable() {
        let mut state = EditorState::new("hello");
        state.set_selections(SelectionSet::caret(5));
        state.update_composition(5..5, " 世界").unwrap();
        assert_eq!(state.document().text(), "hello");
        assert_eq!(state.pending_composition().unwrap().text, " 世界");
        assert!(state.commit_composition().unwrap());
        assert_eq!(state.document().text(), "hello 世界");
        assert_eq!(
            state.selections().primary().unwrap().cursor(),
            "hello 世界".len()
        );
        assert!(state.undo());
        assert_eq!(state.document().text(), "hello");
        state.update_composition(5..5, "!").unwrap();
        assert!(state.cancel_composition());
        assert!(state.pending_composition().is_none());
    }

    #[test]
    fn search_matches_unicode_byte_ranges_without_splitting_codepoints() {
        let document = Document::new("a😀 café CAFÉ");
        let mut session = SearchSession::new(&document.snapshot(), SearchQuery::new("é"));
        assert_eq!(session.matches().len(), 1);
        assert_eq!(&document.text()[session.matches()[0].clone()], "é");

        session
            .set_query(
                &document.snapshot(),
                SearchQuery::new("é").case_sensitivity(CaseSensitivity::Insensitive),
            )
            .unwrap();
        assert_eq!(session.matches().len(), 2);
        assert!(session.matches().iter().all(|range| {
            &document.text()[range.clone()] == "é" || &document.text()[range.clone()] == "É"
        }));
    }

    #[test]
    fn search_navigation_wraps_and_reports_no_matches() {
        let document = Document::new("one two one");
        let mut session = SearchSession::new(&document.snapshot(), SearchQuery::new("one"));
        assert_eq!(session.previous_match(), Some(&(8..11)));
        assert_eq!(session.next_match(), Some(&(0..3)));
        assert_eq!(session.next_match(), Some(&(8..11)));

        let empty = SearchSession::new(&document.snapshot(), SearchQuery::new("missing"));
        assert!(empty.matches().is_empty());
    }

    #[test]
    fn search_refreshes_deterministically_after_a_document_edit() {
        let mut document = Document::new("one two");
        let initial = document.snapshot();
        let mut session = SearchSession::new(&initial, SearchQuery::new("two"));
        assert_eq!(session.next_match(), Some(&(4..7)));
        let applied = document
            .apply(&Transaction::new().insert(0, "new "))
            .unwrap();
        session.refresh(&document.snapshot(), &applied).unwrap();
        assert_eq!(session.current_match(), Some(&(8..11)));
    }

    #[test]
    fn replace_current_updates_text_and_caret_for_unicode() {
        let mut editor = EditorState::new("α β α");
        let mut session = SearchSession::new(&editor.snapshot(), SearchQuery::new("α"));
        session.next_match();
        let result = session.replace_current(&mut editor, "Ω").unwrap().unwrap();
        assert_eq!(result.replaced_ranges, vec![0..2]);
        assert_eq!(editor.document().text(), "Ω β α");
        assert_eq!(editor.selections().primary().unwrap(), Selection::caret(2));
    }

    #[test]
    fn replace_all_orders_edits_and_preserves_mapped_selections() {
        let mut editor = EditorState::new("cat 🐈 cat");
        editor.set_selections(SelectionSet::caret(11));
        let mut session = SearchSession::new(&editor.snapshot(), SearchQuery::new("cat"));
        let result = session.replace_all(&mut editor, "dog").unwrap().unwrap();
        assert_eq!(result.replaced_ranges, vec![0..3, 9..12]);
        assert_eq!(editor.document().text(), "dog 🐈 dog");
        assert_eq!(editor.selections().primary().unwrap().cursor(), 12);
    }

    #[test]
    fn diagnostics_replace_query_overlap_and_reject_stale_publication() {
        let document = Document::new("abcdef");
        let mut diagnostics = DiagnosticSet::new(document.version());
        diagnostics
            .publish(
                &document.snapshot(),
                [
                    Diagnostic::new(1..4, DiagnosticSeverity::Error, "bad"),
                    Diagnostic::new(3..5, DiagnosticSeverity::Warning, "also bad"),
                    Diagnostic::new(3..3, DiagnosticSeverity::Info, "point"),
                ],
            )
            .unwrap();
        assert_eq!(diagnostics.at(3).len(), 3);
        assert_eq!(diagnostics.overlapping(2..4).len(), 3);
        assert!(matches!(
            diagnostics.publish(
                &document.snapshot(),
                [Diagnostic::new(6..7, DiagnosticSeverity::Error, "outside")],
            ),
            Err(DiagnosticError::InvalidRange(_))
        ));

        let applied = Document::from_snapshot(&document.snapshot())
            .apply(&Transaction::new().insert(0, "x"))
            .unwrap();
        diagnostics
            .advance_document_version(applied.after_version)
            .unwrap();
        assert!(diagnostics.diagnostics().is_empty());
        assert!(matches!(
            diagnostics.publish(&document.snapshot(), []),
            Err(DiagnosticError::StaleDocumentVersion { .. })
        ));
    }
}
