//! Deterministic state model used by the native Mockaco showcase.

use mockaco_core::{
    CloseDecision, DocumentMetadata, DocumentSession, ExternalChangeDecision, HostSaveError,
    HostSaveResult, SaveCompletion,
};

pub const DEMO_SOURCE: &str = "fn main() {\n    let answer = 42;\n    println!(\"answer = {answer}\");\n}\n\n// Try typing, selecting, scrolling, and the controls above.\n";
pub const EXTERNAL_SOURCE: &str =
    "fn main() {\n    let answer = 99;\n    println!(\"remote value = {answer}\");\n}\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemoAction {
    Load,
    LocalEdit,
    ReplaceAll,
    SaveSuccess,
    SaveFailure,
    ExternalConflict,
    KeepLocal,
    ReloadExternal,
    CloseSave,
    CloseDiscard,
    CloseCancel,
    LspResults,
}

#[derive(Debug, Clone)]
pub struct DemoState {
    session: DocumentSession,
    status: String,
    lsp_status: String,
}

impl DemoState {
    pub fn new() -> Self {
        Self {
            session: new_session(DEMO_SOURCE),
            status: "Loaded demo://memory/main.rs (clean)".into(),
            lsp_status: "Mock LSP: idle".into(),
        }
    }

    pub fn session(&self) -> &DocumentSession {
        &self.session
    }

    pub fn text(&self) -> &str {
        self.session.text()
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn lsp_status(&self) -> &str {
        &self.lsp_status
    }

    pub fn apply(&mut self, action: DemoAction) {
        match action {
            DemoAction::Load => {
                self.session
                    .replace_loaded(
                        Some("demo://memory/main.rs".into()),
                        DEMO_SOURCE.into(),
                        DocumentMetadata::detect_utf8(DEMO_SOURCE),
                    )
                    .expect("demo load is valid");
                self.status = "Load succeeded: clean UTF-8 document".into();
            }
            DemoAction::LocalEdit => {
                self.session.apply_edit(0..0, "// local edit\n").unwrap();
                self.status = "Local edit applied: dirty".into();
            }
            DemoAction::ReplaceAll => {
                let range = self
                    .session
                    .text()
                    .find("answer")
                    .map(|start| start..start + "answer".len());
                if let Some(range) = range {
                    self.session.apply_edit(range, "result").unwrap();
                }
                self.status = "Replace-current demo applied through a core transaction".into();
            }
            DemoAction::SaveSuccess => {
                self.ensure_dirty();
                let request = self.session.request_save().expect("demo has a location");
                match self.session.complete_save(
                    request.id,
                    HostSaveResult::Success {
                        location: "demo://memory/main.rs".into(),
                    },
                ) {
                    SaveCompletion::Saved(receipt) => {
                        self.status = format!(
                            "Save succeeded: didSave snapshot v{} ({} bytes)",
                            receipt.request.identity.document_version,
                            receipt.request.text.len()
                        );
                    }
                    other => self.status = format!("Unexpected save state: {other:?}"),
                }
            }
            DemoAction::SaveFailure => {
                self.ensure_dirty();
                let request = self.session.request_save().expect("demo has a location");
                let completion = self.session.complete_save(
                    request.id,
                    HostSaveResult::Failure(HostSaveError::new("demo provider: read-only")),
                );
                self.status = format!("Save failed; edits preserved: {completion:?}");
            }
            DemoAction::ExternalConflict => {
                self.ensure_dirty();
                let outcome = self
                    .session
                    .notify_external_change(
                        EXTERNAL_SOURCE,
                        DocumentMetadata::detect_utf8(EXTERNAL_SOURCE),
                    )
                    .expect("demo external change is valid");
                self.status = format!("External change: {outcome:?}");
            }
            DemoAction::KeepLocal => {
                let outcome = self
                    .session
                    .resolve_external_change(ExternalChangeDecision::KeepLocal);
                self.status = format!("Keep-local decision: {outcome:?}");
            }
            DemoAction::ReloadExternal => {
                let outcome = self
                    .session
                    .resolve_external_change(ExternalChangeDecision::ReloadExternal);
                self.status = format!("Reload-external decision: {outcome:?}");
            }
            DemoAction::CloseSave => {
                self.status = format!(
                    "Close/save decision: {:?}",
                    self.session.close(CloseDecision::Save)
                );
            }
            DemoAction::CloseDiscard => {
                self.status = format!(
                    "Close/discard decision: {:?}",
                    self.session.close(CloseDecision::Discard)
                );
            }
            DemoAction::CloseCancel => {
                self.status = format!(
                    "Close/cancel decision: {:?}",
                    self.session.close(CloseDecision::Cancel)
                );
            }
            DemoAction::LspResults => {
                self.lsp_status = "Mock LSP: completion, hover, navigation, code action, semantic tokens, diagnostics".into();
                self.status = "Mock LSP results are deterministic demo data".into();
            }
        }
    }

    fn ensure_dirty(&mut self) {
        if !self.session.is_dirty() {
            self.session
                .apply_edit(0..0, "// unsaved demo edit\n")
                .unwrap();
        }
    }
}

impl Default for DemoState {
    fn default() -> Self {
        Self::new()
    }
}

fn new_session(text: &str) -> DocumentSession {
    DocumentSession::new(
        Some("demo://memory/main.rs".into()),
        text,
        DocumentMetadata::detect_utf8(text),
    )
    .expect("demo text is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_failure_preserves_demo_edits_and_success_clears_dirty() {
        let mut demo = DemoState::new();
        demo.apply(DemoAction::LocalEdit);
        let edited = demo.text().to_owned();
        demo.apply(DemoAction::SaveFailure);
        assert_eq!(demo.text(), edited);
        assert!(demo.session().is_dirty());
        demo.apply(DemoAction::SaveSuccess);
        assert!(!demo.session().is_dirty());
    }

    #[test]
    fn external_conflict_requires_an_explicit_decision() {
        let mut demo = DemoState::new();
        demo.apply(DemoAction::LocalEdit);
        demo.apply(DemoAction::ExternalConflict);
        assert_eq!(demo.text(), "// local edit\n".to_owned() + DEMO_SOURCE);
        demo.apply(DemoAction::ReloadExternal);
        assert_eq!(demo.text(), EXTERNAL_SOURCE);
        assert!(!demo.session().is_dirty());
    }

    #[test]
    fn close_actions_are_visible_to_the_demo() {
        let mut demo = DemoState::new();
        demo.apply(DemoAction::CloseCancel);
        assert!(demo.status().contains("Cancelled"));
        demo.apply(DemoAction::CloseDiscard);
        assert!(demo.status().contains("CloseNow"));
    }
}

#[cfg(feature = "native-wgpui")]
mod native_app;

#[cfg(feature = "native-wgpui")]
pub use native_app::run;
