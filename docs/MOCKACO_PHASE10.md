# Mockaco Phase 10: Tree-sitter editing surface and visual polish

This phase makes the native showcase a usable Monaco-style editor surface
while keeping the framework-independent boundaries intact.

## Delivered contract

- `mockaco-language` contains a real `RustTreeSitterProvider` using
  `tree-sitter`, `tree-sitter-rust`, and the grammar's highlight query.
  Highlight captures are versioned byte ranges and are clamped to UTF-8
  boundaries. Parser trees receive `InputEdit` updates when the core
  transaction mapping is current; stale/out-of-order snapshots are rejected.
- `LanguageProviderRegistry` is the explicit provider seam. Rust is the only
  registered concrete grammar in this phase; adding another grammar means
  registering its highlighter and folding provider without changing core or
  renderer APIs.
- Rust block/function/impl/struct/enum/trait constructs derive foldable ranges.
  Fold ends are exclusive in the display contract, so a collapsed region has
  one obvious placeholder row and does not leave a misleading blank closing
  row. Foldable markers remain available after collapse and are mapped through
  edits.
- `mockaco-renderer` owns the gutter geometry and display-row mapping. It
  provides stable right-aligned line numbers, foldable/folded marker state,
  active rows, selection/caret geometry, and decoration projections without
  knowing WGPUI or Tree-sitter.
- `mockaco-gpui` keeps parser work out of painting. It translates native
  arrows, Home/End, page movement, Shift extension, mouse click/drag,
  word/line selection, multi-caret editing, and fold commands into the
  renderer/core model. The native surface paints syntax-colored `StyledText`,
  active-line and selection contrast, diagnostic/search decoration contrast,
  gutter chevrons, and restrained diff panes.

## Visual contract

The showcase uses a dark blue-gray editor canvas with a dedicated gutter per
pane. Gutter numbers are muted and right-aligned with a measured separator;
fold chevrons appear only on foldable lines; active lines, diagnostics,
search ranges, selections, and carets remain distinguishable from syntax
colors. The toolbar is grouped into lifecycle, editor, and showcase actions.
Diff panes have matching gutters, explicit original/modified headers, and a
subtle divider instead of a second undifferentiated button strip.

## Verification focus

Deterministic tests cover Rust comments, strings, keywords, numbers, nested
constructs, Unicode boundaries, incremental/stale versions, fold mapping,
gutter hit testing, folded and wrapped display movement, selection extension,
goal-column movement, multi-caret editing, and renderer/native presentation
data. The native example is the visual smoke test when a desktop session is
available.

## Deliberate remaining gaps

Only Rust is shipped as a concrete grammar. Semantic tokens and LSP-provided
themes are not merged with syntax tokens yet. Rectangular selection,
minimap, tabs, file explorer, persistence, workspace shell, and a background
worker/executor for parser scheduling remain outside this phase. The current
provider API is ready for that worker: callers must continue to publish only
version-matching results and never parse during paint.
