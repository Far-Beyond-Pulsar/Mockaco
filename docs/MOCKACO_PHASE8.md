# Mockaco Phase 8 — Editor Lifecycle Contract

Phase 8 adds the reusable, framework-independent lifecycle for one editor
document. It intentionally stops before tabs, split groups, workspace
persistence, file watching, dialogs, or a concrete filesystem provider.

## Delivered

- `mockaco-core::DocumentSession` owns text, document version, content
  identity, generation, metadata, and dirty state.
- Hosts can load or replace UTF-8 bytes, host-decoded text, or explicitly
  reject unsupported encodings without silently corrupting content.
- `SaveRequest` and `HostSaveResult` model save/save-as handoff and typed
  failure. A failed save leaves the document and dirty state unchanged.
- `SaveReceipt` captures the exact requested snapshot. It yields a
  framework-independent `SaveEvent` for integrations such as LSP.
- External changes distinguish clean reloads from dirty conflicts and expose
  explicit keep-local/reload-external decisions.
- Close decisions are typed as discard, save, or cancel; the session never
  owns a dialog or implicitly discards edits.
- Save and external-change completions carry generation/content identity so
  stale asynchronous results are rejected or reported without mutation.
- `mockaco-lsp::send_save_event` adapts a successful core save event into
  `textDocument/didSave` while keeping core free of LSP types.

## Deliberate gaps

The host still owns filesystem access, encoding detection/decoding, file
watchers, dialogs, tabs, split layouts, explorer state, persistence, and
workspace policy. Those are application-shell responsibilities and are not
part of the Phase 8 core contract.

## Verification

The phase includes deterministic tests for load failure and replacement,
dirty transitions, save success/failure/save-as, newline and encoding
metadata, external conflicts, close decisions, stale generations, exact save
events, and the LSP `didSave` bridge.
