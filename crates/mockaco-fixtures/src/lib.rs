//! Deterministic, runtime-free behavior fixtures for the Mockaco contracts.
//!
//! This crate intentionally has no filesystem, window, WGPUI runtime, or
//! Pulsar dependency. Integration tests and the standalone example use the
//! same fixture inputs so behavior remains reproducible across hosts.

use mockaco_core::{Document, DocumentSession, DocumentSnapshot, Transaction};

pub const RUST_FIXTURE: &str =
    "fn main() {\n    let answer = 42;\n    println!(\"{answer}\");\n}\n";
pub const PLAIN_FIXTURE: &str = "alpha\nbeta\nalpha\n";

pub fn fixture_names() -> [&'static str; 2] {
    ["rust", "plain"]
}

pub fn fixture_text(name: &str) -> Option<&'static str> {
    match name {
        "rust" => Some(RUST_FIXTURE),
        "plain" => Some(PLAIN_FIXTURE),
        _ => None,
    }
}

/// Generates stable source-like text with an exact byte target.
pub fn generated_document(target_bytes: usize) -> String {
    generated_document_with_shape(target_bytes, false)
}

/// Generates one logical line plus a final newline for pathological-line work.
pub fn pathological_document(target_bytes: usize) -> String {
    generated_document_with_shape(target_bytes, true)
}

fn generated_document_with_shape(target_bytes: usize, pathological: bool) -> String {
    if target_bytes == 0 {
        return String::new();
    }
    if pathological {
        return format!("{}\n", "x".repeat(target_bytes.saturating_sub(1)));
    }

    let mut text = String::with_capacity(target_bytes);
    let mut line = 0usize;
    while text.len() < target_bytes {
        let candidate = format!("    let value_{line} = {line}; // deterministic fixture\n");
        let remaining = target_bytes - text.len();
        if candidate.len() <= remaining {
            text.push_str(&candidate);
        } else {
            text.push_str(&"x".repeat(remaining));
        }
        line += 1;
    }
    text
}

pub fn session(text: &str) -> DocumentSession {
    DocumentSession::new(
        Some("fixture://document".into()),
        text,
        mockaco_core::DocumentMetadata::detect_utf8(text),
    )
    .expect("fixture text is valid UTF-8")
}

pub fn snapshot(text: &str) -> DocumentSnapshot {
    Document::new(text).snapshot()
}

pub fn insert_transaction(offset: usize, text: &str) -> Transaction {
    Transaction::new().insert(offset, text)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandaloneReport {
    pub fixture_count: usize,
    pub document_bytes: usize,
    pub final_document_version: u64,
    pub render_rows: usize,
    pub diff_hunks: usize,
    pub save_event_text: String,
}

/// Runs a small end-to-end contract exercise without opening a window or file.
pub fn run_standalone() -> StandaloneReport {
    use mockaco_core::{DocumentMetadata, HostSaveResult, SaveCompletion};
    use mockaco_diff::compute_diff;
    use mockaco_gpui::{EditorSurface, SurfaceGeometry};
    use mockaco_renderer::{DisplayConfig, WrapConfig};

    let mut session = session(RUST_FIXTURE);
    session.apply_edit(0..0, "// standalone\n").unwrap();
    let request = session.request_save().unwrap();
    let receipt = match session.complete_save(
        request.id,
        HostSaveResult::Success {
            location: "fixture://saved".into(),
        },
    ) {
        SaveCompletion::Saved(receipt) => receipt,
        other => panic!("unexpected fixture save completion: {other:?}"),
    };

    let mut surface = EditorSurface::new(
        session.text(),
        DisplayConfig::wrapped(48).with_wrap(Some(
            WrapConfig::new(48)
                .max_segments_per_line(8)
                .max_line_scan_bytes(512),
        )),
        SurfaceGeometry {
            width: 640.0,
            height: 240.0,
            line_height: 8.0,
            character_width: 4.0,
            gutter_character_width: 4.0,
        },
    );
    surface.set_viewport(12, 80);
    let render_rows = surface.render_frame().rows.len();
    let original = snapshot(RUST_FIXTURE);
    let diff_hunks = compute_diff(&original, &session.snapshot()).hunks().len();

    // Keep the metadata path exercised by the standalone API itself.
    assert_eq!(
        session.metadata().encoding,
        DocumentMetadata::detect_utf8(session.text()).encoding
    );

    StandaloneReport {
        fixture_count: fixture_names().len(),
        document_bytes: session.text().len(),
        final_document_version: session.snapshot().version(),
        render_rows,
        diff_hunks,
        save_event_text: receipt.event().text,
    }
}
