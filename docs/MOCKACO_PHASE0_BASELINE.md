# Mockaco Phase 0 Baseline

Status: contract and measurements audit only
Date: 2026-09-23
Scope: Pulsar-Native host integration, wgpui-base editor infrastructure, and
the intended plugins/vendor/code_editor / WGPUI-Component integration.

This document records what is present in the checkout and defines the fixture
and benchmark contract for Phase 1. It does not change editor or production
implementation code.

## Audit conditions

The worktree was already dirty before this audit:

- Cargo.lock was modified.
- docs/MOCKACO_ARCHITECTURE.md was untracked.
- new_capture.db and new_capture_230257.db were untracked.

Those files were left untouched. The three relevant submodule directories are
also empty in this checkout:

| Path | Git role | Observed state |
|---|---|---|
| crates/ui/wgpui | WGPUI/GPUI implementation | gitlink exists; directory has zero files |
| crates/ui/wgpui-component | shared WGPUI-Component UI | gitlink exists; directory has zero files |
| plugins/vendor/code_editor | script/code editor plugin | gitlink exists at dd397a43905a8d516c2d03dc5a1bf744672fc977; directory has zero files |

Consequently, the external code editor and WGPUI-Component internals could not
be behavior-tested or read from this worktree. The claims below distinguish
repository evidence from behavior that remains to be recovered after the
submodules are populated.

cargo metadata --no-deps --format-version 1 currently fails before graph
construction because
crates/ui/wgpui-component/crates/ui/Cargo.toml is missing. No latency or
memory numbers are claimed in this baseline.

## 1. Current behavior surface and public entry points

### 1.1 File-to-panel path

The current host path is plugin-driven:

~~~text
PulsarApp::open_path
  -> PluginManager::create_editor_for_file
  -> FileTypeRegistry::get_file_type_for_path
  -> EditorRegistry::get_editor_for_file_type
  -> BuiltinEditorRegistry::create_editor (or DLL plugin path)
  -> Arc<dyn ui::dock::PanelView>
  -> center_tabs.add_panel
~~~

Relevant entry points:

| Surface | Current contract | Evidence |
|---|---|---|
| Open a file | PulsarApp::open_path(path, window, cx) activates an existing panel by path or asks the plugin manager to create one. | crates/editor/ui_core/src/app/tab_management.rs |
| File/editor selection | PluginManager::create_editor_for_file resolves file type, editor ID, owner, and then creates a built-in or DLL editor. | crates/core/plugin_manager/src/lib.rs |
| Built-in provider | BuiltinEditorProvider supplies provider ID, file types, editor metadata, and create_editor. | crates/core/plugin_manager/src/builtin.rs |
| Script provider | Provider ID is com.pulsar.script-editor; editor ID is script-editor. It registers rs, js, ts, py, lua, toml, and md. | crates/editor/ui_core/src/builtin_editors.rs |
| Script panel creation | ScriptEditorBuiltinProvider::create_editor constructs script_editor_plugin::ScriptEditorPanel, injects the shared RustAnalyzerManager when available, then calls open_file. | crates/editor/ui_core/src/builtin_editors.rs |
| Dock boundary | The host receives only Arc<dyn PanelView>. PanelView exposes view, focus, visibility, tab identity, persistence dump, and lifecycle hooks; it does not expose generic text, dirty, save, or LSP methods. | crates/ui/wgpui-base/src/dock/panel.rs |
| Registration | register_all_builtin_editors is called during PulsarApp construction and registers provider file types and editor metadata. | crates/editor/ui_core/src/builtin_editors.rs, crates/editor/ui_core/src/app/constructors.rs |

The PanelView boundary means a Mockaco adapter can preserve the existing
provider/editor IDs without exposing Mockaco internals. It also means save,
dirty-state, external-file-change, and editor-event behavior must be defined
explicitly in the adapter; they are not supplied by the dock contract.

### 1.2 In-tree WGPUI editor foundation

crates/ui/wgpui-base has an editor-capable input stack organized behind the
public wgpui_base::input module. The public presentation entry points are:

~~~rust
let state = cx.new(|cx| {
    wgpui_base::input::EditorState::new(window, cx)
        .language("rust")
        .folding(true)
        .line_number(true)
});
let element = wgpui_base::input::Editor::new(&state);
~~~

The actual implementation is a generic InputBaseState<EditorMode> alias, not
a standalone, framework-independent document core. The state currently
contains or coordinates:

- ropey::Rope text with UTF-8/UTF-16/line metrics through RopeExt.
- Selections, including multiple carets and columnar-selection state.
- IME marked-range state and native input handling.
- UndoManager, including typing/backspace/delete coalescing, explicit
  transactions, selection snapshots, auto-close snapshots, undo/redo stacks,
  and limits of 1,000 transactions and 1,000 changes per transaction.
- DisplayMap, wrapping, folding, buffer/display coordinate conversion, line
  numbers, scrolling, and visible-row calculations.
- Language configuration, highlighter factories, syntax context, auto-close,
  smart indentation, and incremental highlighter edit descriptions.
- SearchSession with next/previous, replace-current, and replace-all paths.
- DiagnosticSet backed by a SumTree, with range and offset queries.
- Decoration collections and semantic-token/document-color projections.
- LSP provider seams for completion, code actions, hover, definitions,
  document colors, semantic tokens, inline completion, and show-document hooks.

The public module/re-export seam is
crates/ui/wgpui-base/src/input/mod.rs; the implementation organization is
documented in crates/ui/wgpui-base/src/input/README.md. The editor-specific
implementation is under src/input/editor/ and the shared edit engine under
src/input/base/.

No production call site for EditorState::new, Editor::new, or an exported
ui::CodeEditor wrapper was found in the checked-in crates/ tree. Existing
editor UI consumers mostly use InputState for search, settings, forms, and
other single-line fields. The script editor's real consumer is in the empty
external submodule, so the relationship between that panel and this in-tree
editor foundation is unresolved in this checkout.

### 1.3 Language-server path

Pulsar owns a process-level RustAnalyzerManager in
crates/core/pulsar_lsp/src/rust_analyzer/mod.rs. The service exposes
did_open_file, did_change_file, did_save_file, did_close_file, hover,
definition, and code-action requests, plus diagnostic and analysis events.
The current change notification sends the complete document text with a
version, rather than a range edit. Initialization is asynchronous and
did_open_file rejects calls before the initialize handshake completes.

The built-in script provider injects this manager directly into the external
ScriptEditorPanel. That is a host-specific coupling to preserve at the adapter
boundary, not a dependency Mockaco core should inherit.

### 1.4 What is not observable yet

Because plugins/vendor/code_editor is empty, this audit could not establish the
current script panel's exact behavior for file loading, save, dirty state,
keyboard commands, search UI, folding UI, diagnostics, diff, completion, or
IME. Because crates/ui/wgpui-component is empty, its component-level editor,
Tree-sitter queries, theme schema, minimap, and diagnostic treatment could not
be compared. These are Phase 0 blockers, not evidence that the behaviors do
not exist upstream.

## 2. Legacy versus modern systems and migration risks

Here, “legacy” means the behavior that Mockaco is intended to replace or
preserve; “modern candidate” means the newer capability-oriented seams already
present in the repository. This is not a claim about upstream submodule commit
history.

| Area | Legacy/current system | Modern candidate | Main risk |
|---|---|---|---|
| Editor implementation | External script_editor_plugin::ScriptEditorPanel and its WGPUI-Component internals. | New Mockaco core plus renderer/GPUI/workspace crates from MOCKACO_ARCHITECTURE.md. | The source and executable behavior are unavailable until submodules are initialized, so compatibility cannot yet be measured. |
| Text state | wgpui-base InputBaseState<EditorMode> is shared with input/textarea modes and owns GPUI entities, window context, Rope, selections, display map, and undo state. | Framework-independent document, transaction, position-map, selection, and snapshot APIs. | Copying the state wholesale would carry GPUI, parser, LSP, and host lifecycle coupling into mockaco-core. |
| Rendering | wgpui-base Editor delegates to the shared text element; layout and paint are state/GPUI driven. | Renderer-neutral visible-line layout and a GPUI adapter. | A full-document scan or invalidation may be hidden in the existing render path; baseline must measure changed-range and viewport work. |
| Language services | Language provider/highlighter and LSP provider types are attached to editor state; Pulsar also has a direct Rust Analyzer manager. | Replaceable language and LSP providers, versioned snapshots, cancellation, and stale-result rejection. | Two LSP ownership models can produce duplicate changes, diagnostics, or completions unless one adapter owns synchronization. |
| Integration | Built-in provider IDs, file associations, PanelView, dock tabs, and global Rust Analyzer state. | Thin Pulsar adapter preserving provider ID and file associations. | PanelView has no save/dirty/event contract, so a cutover can silently lose save lifecycle or tab semantics. |
| Search/folding/diagnostics | Features exist in wgpui-base; exact script-plugin presentation is unknown. | Mockaco decoration/state domains with typed events and range invalidation. | Matching visuals is not enough: range coordinate, UTF-16, stale-version, and selection behavior must be fixture-locked. |
| Diff/workspace | No checked-in Mockaco or current script-panel implementation is observable; dock panels provide no diff contract. | mockaco-workspace owns diff, tabs, file loading/saving, and workspace commands. | Diff, external changes, and persistence need an explicit owner before migration; otherwise adapter code becomes a second workspace implementation. |

Migration risks to resolve before Phase 1:

1. Do not use the empty submodule state as the legacy behavioral baseline.
   Pin and record the actual WGPUI, WGPUI-Component, and code-editor commits.
2. Preserve com.pulsar.script-editor, script-editor, and the seven current
   file associations during the first adapter.
3. Keep Pulsar types, GPUI entities, filesystem access, and Rust Analyzer
   process management out of Mockaco core.
4. Define one document-version authority. LSP results and diagnostics must be
   rejected when they refer to a stale snapshot.
5. Define save and dirty-state ownership before implementing a workspace shell.
6. Treat diff as a separate behavior surface; it is not implied by ordinary
   selection or decoration support.

## 3. Behavior-fixture plan

Each fixture should run against the recovered current editor and the Mockaco
adapter. A fixture is deterministic JSON or Rust data with:

~~~text
initial document + language/configuration
  -> ordered input/command/provider steps
  -> expected text, selections, events, decorations, and external effects
~~~

Offsets must be asserted in both byte and UTF-16 coordinates where applicable.
Visual assertions should use a fixed font, scale factor, viewport width, and
line height. Async cases must use a fake clock/provider so completion order and
stale-result behavior are deterministic.

| Fixture family | Required cases | Oracle / observable contract |
|---|---|---|
| Editing | Insert, delete backward/forward, replace selection, newline, tab/indent, auto-close, multi-cursor insert, Unicode and combining characters. | Final Rope/text, caret positions, emitted change ranges, UTF-8/UTF-16 mapping, and whether edits group atomically. |
| Selection | Click/drag, word/line selection, shift extension, multiple carets, rectangular selection, selection across folded/wrapped lines. | Ordered selection set, active caret, affinity, selected text, and edit application order. |
| IME | Composition update, replacement of marked text, commit, cancel/Escape, composition across Unicode text, composition plus undo. | Marked range lifecycle, visible text during composition, one logical undo entry, and no residual text after cancel. |
| Undo/redo | Typing coalescing, backspace coalescing, explicit multi-cursor transaction, formatting replacement, undo then new edit, redo invalidation, selection restoration. | Text, selections, auto-close pairs, stack availability, and transaction count after each step. |
| Search | Literal query, case-insensitive query, no match, next/previous wrap, replace current, replace all, query edits after document changes. | Match ranges, current-match index, replacement text, selection/caret behavior, and decoration invalidation range. |
| Folding/display | Fold candidate extraction, toggle/unfold, nested ranges, edit above/below a fold, wrapping, long line, buffer/display coordinate conversion. | Visible row count, hidden lines, fold state adjustment, line-number/gutter state, and bounded work for the viewport. |
| Diagnostics | Error/warning/info ranges, overlapping diagnostics, diagnostics after edits, hover lookup, clear/replacement, stale snapshot. | Diagnostic set contents, byte ranges, severity/style, offset lookup, and stale result rejection. |
| Diff | Equal file, insertion/deletion/replacement hunks, empty sides, Unicode lines, edits while diff is visible. | Stable line/hunk mapping, changed ranges, side-specific selections, and no mutation of the source document. Current implementation owner is undecided. |
| Save | Load, edit, dirty transition, save success/failure, newline/encoding policy, external modification, save-as, close with unsaved changes. | Bytes written, dirty state, save events, error propagation, LSP didSave, and close/overwrite prompt policy. Current PanelView does not define this contract. |
| LSP | Open/change/save/close, completion, hover, definition, code action, semantic tokens, diagnostics, cancellation, out-of-order responses. | Exact provider calls, document versions, cancellation, stale response rejection, applied edits, and host navigation events. |

The first fixture corpus should include at least one Rust file and one plain
text file per family, plus a UTF-16-sensitive case containing astral symbols,
combining marks, CRLF input, an empty document, and a final line without a
newline.

## 4. Benchmark plan

### 4.1 Workloads

Use deterministic generated documents with target sizes measured in bytes:

| Case | Shape | Required size |
|---|---|---:|
| 1k | Normal source-like lines, mixed indentation and comments | 1,024 bytes |
| 100k | Same distribution, enough lines to exercise scrolling and search | 102,400 bytes |
| 1m | Same distribution, enough lines to exercise open, edit, fold, and LSP scheduling | 1,048,576 bytes |
| pathological-line | One logical line of 1,048,576 bytes plus a terminating newline; no line-based shortcut may monopolize the UI thread | 1,048,577 bytes |

The generator must record seed, byte count, line count, maximum line length,
newline style, language ID, and SHA-256. The benchmark must use the same
generated bytes for the recovered current implementation and Mockaco.

### 4.2 Exact command contract

The repository currently has no Mockaco benchmark crate and cannot resolve the
workspace while the submodules are empty. After the submodules and the Phase 1
harness exist, the commands below are the required command shape; the runner
must accept these options and emit one JSON result per case.

Initialize the pinned dependency snapshot first:

~~~powershell
git submodule update --init --recursive
cargo metadata --no-deps --format-version 1
~~~

Run the current implementation and the Mockaco adapter under identical
conditions:

~~~powershell
$cases = @('1k', '100k', '1m', 'pathological-line')
foreach ($case in $cases) {
    cargo run --release -p mockaco-bench --bin mockaco-bench -- --implementation current --case $case --viewport-lines 60 --warmup 20 --iterations 100 --json "target/mockaco-bench/current-$case.json"
    cargo run --release -p mockaco-bench --bin mockaco-bench -- --implementation mockaco --case $case --viewport-lines 60 --warmup 20 --iterations 100 --json "target/mockaco-bench/mockaco-$case.json"
}
~~~

For repeatability, also run the deterministic behavior suite:

~~~powershell
cargo test -p mockaco-fixtures --test behavior -- --nocapture
cargo test -p wgpui-base --lib -- --nocapture
~~~

The current checkout cannot run those commands yet: mockaco-bench and
mockaco-fixtures do not exist, and cargo metadata stops at the missing
WGPUI-Component manifest. This is intentional documentation of the missing
measurement harness, not a failed production test.

### 4.3 Metrics

Each JSON result must include raw samples and summary statistics for:

- cold open: file read-to-first usable frame;
- first paint: editor construction-to-first frame;
- keypress-to-frame: single-character insert at top, middle, and bottom;
- paste: 1 KiB and 100 KiB insertion at top and middle;
- cursor movement and selection extension over 1, 10, and 1,000 lines;
- vertical scroll and horizontal scroll over a fixed 60-line viewport;
- search query, next match, and replace-all;
- fold/unfold and edit adjacent to a fold;
- undo and redo after single-cursor and multi-cursor edits;
- diagnostic publication and stale-result discard;
- LSP open/change/save request enqueue time with a fake provider;
- save completion time and bytes written;
- peak process working set, allocation count/bytes if available, and visible
  line/glyph/decoration counts.

Report p50, p95, p99, maximum, and sample count for latency; report minimum,
median, maximum, and peak delta for memory/allocation metrics. Record whether
the operation ran on the UI thread, background executor, or fake provider.
The pathological-line case must additionally report maximum frame time and
whether any full-line layout exceeded the configured per-frame budget.

Do not combine open, first paint, steady-state typing, and save into one
number. They exercise different state domains and are needed to detect a
regression hidden by a favorable average.

## 5. Blockers and decisions before Phase 1

### Blocking repository state

1. Populate and pin crates/ui/wgpui, crates/ui/wgpui-component, and
   plugins/vendor/code_editor; record their commits in the baseline.
2. Recover the current script editor's source and standalone harness so the
   behavior fixtures can run against the real legacy implementation.
3. Add a standalone fixture/benchmark harness. There are no Mockaco fixtures,
   benchmark targets, or current-editor baseline numbers in this checkout.

### Decisions required

1. **Save owner:** Should mockaco-workspace own file I/O and dirty state, with
   the Pulsar adapter translating save events, or should the host retain
   ownership? Define external-change and close-with-unsaved-edits behavior.
2. **LSP owner:** Should the adapter wrap RustAnalyzerManager as a generic
   Mockaco provider, or should a host-side synchronization layer translate
   document snapshots? Choose one document-version authority and stale-result
   policy.
3. **Diff owner:** Confirm that diff belongs to mockaco-workspace, including
   side-by-side selection and read-only semantics, rather than to the core
   document or generic WGPUI-Component.
4. **Compatibility surface:** Confirm that the first cutover must preserve
   com.pulsar.script-editor, script-editor, and the current seven file
   associations exactly.
5. **Text policy:** Decide canonical newline, encoding, invalid UTF-8, final
   newline, tab width, and UTF-16 coordinate behavior for load/save and LSP.
6. **Performance environment:** Fix OS, build profile, font, scale factor,
   viewport, GPU/backend, warmup, iteration count, and whether antivirus or
   background indexing is excluded from measurements.
7. **Submodule availability:** Decide whether Phase 1 may proceed against a
   checked-in compatibility fixture only, or must wait for the exact legacy
   plugin and WGPUI-Component sources.

Phase 1 should start only after the first three blockers are cleared and the
save/LSP/versioning decisions are written into the adapter contract.

## Verification record

The artifact was checked for all required Phase 0 sections and for trailing
whitespace. The only intended file change from this audit is this document;
the pre-existing dirty files listed above remain unchanged.


