# Mockaco Phase 6

Status: framework-independent LSP integration seams  
Date: 2026-09-23

Phase 6 adds a dedicated `mockaco-lsp` crate. It uses the real `lsp-types`
protocol data model while keeping transport, async runtime, filesystem, and UI
ownership outside the editor crates.

## Scope delivered

- Explicit-version document synchronization for open, full-content change,
  save, and close notifications.
- Deterministic request ids, typed request kinds for completion,
  inline-completion, hover, definition, references, code actions, and semantic
  tokens, plus cancellation and stale/out-of-order response rejection.
- Version-checked diagnostic publication into the existing core
  `DiagnosticSet`, including UTF-16 LSP positions mapped to UTF-8 byte ranges.
- Real LSP completion, inline-completion, hover, navigation, and code-action
  result types exposed at the integration boundary.
- Definition, reference, and location-link conversion into byte-range
  navigation targets.
- Version-checked textual `WorkspaceEdit` and code-action application through
  core `EditorState` transactions. Resource operations remain explicitly
  host-owned rather than being silently ignored.
- Semantic-token delta decoding into renderer-neutral byte-range decorations,
  preserving token type and modifier bitsets.
- `RecordingTransport` plus focused tests for lifecycle, cancellation,
  deterministic ids, stale results, UTF-16 diagnostics, semantic tokens,
  navigation, workspace edits, and code-action result handling.

## Deliberate remaining gaps

The crate does not select an async runtime or implement JSON-RPC framing; a
host supplies `LspTransport`. It also does not own server process management,
workspace filesystem operations, command execution, or UI presentation. Those
responsibilities remain replaceable application-level adapters.
