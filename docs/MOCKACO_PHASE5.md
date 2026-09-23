# Mockaco Phase 5

Status: framework-independent search and diagnostics
Date: 2026-09-23

Phase 5 adds document-versioned search and diagnostics without introducing UI,
WGPUI, filesystem, workspace, or language-server dependencies into the core.

## Scope delivered

- Literal `SearchSession` queries with sensitive and Unicode-aware insensitive
  matching, byte-safe match ranges, current-match tracking, and wrapping next /
  previous navigation.
- Deterministic search refresh after an applied transaction, with explicit
  stale-version rejection for results that no longer describe the session's
  document.
- Replace-current and replace-all transactions, including Unicode-safe edit
  ordering and selection/caret mapping through the resulting change map.
- `DiagnosticSet` with error, warning, info, and hint severity, sorted
  replacement publication, overlapping and point-range queries, clear/update
  behavior, and versioned stale-result rejection.
- Renderer-neutral search and diagnostic decoration adapters with stable style
  IDs, plus a `mockaco-gpui` convenience seam that combines both decoration
  sources without importing WGPUI into the core logic.
- Focused coverage for Unicode, empty results, navigation wrapping, edits after
  query setup, replacement behavior, overlapping diagnostics, invalid ranges,
  version mismatches, and stale publication.

## Deliberate remaining gaps

Search remains literal; regular expressions, whole-word matching, preserve-case
replacement, and replace-preview UI are not part of this milestone.

Diagnostics are an in-memory publication model. LSP transport, cancellation,
diagnostic source ownership, code actions, workspace files, tabs, split groups,
and persistence remain later integration work. Search and diagnostics also do
not yet provide a host-level event stream; callers explicitly publish the
versioned results and pass renderer decorations to the presentation boundary.
