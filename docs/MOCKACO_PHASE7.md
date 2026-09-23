# Mockaco Phase 7

Status: framework-independent split-screen diff editor
Date: 2026-09-23

Phase 7 adds the Monaco-style original/modified diff surface without turning
Mockaco into a workspace shell. File loading, saving, tabs, split-group
management, and persistence remain host-owned.

## Scope delivered

- New `mockaco-diff` crate built on the real `similar` line-diff algorithm.
- Immutable original snapshots and independently editable modified editor
  state, with diff computation guaranteed not to mutate the original side.
- Deterministic equal, insert, delete, and replace rows and hunks, including
  empty sides, trailing lines, Unicode text, and side-specific byte ranges.
- Stable original/modified line-to-row mapping and caret/selection conversion
  across missing lines and hunk boundaries.
- Independent and synchronized scroll modes. Synchronized mode scrolls shared
  diff rows, so inserted/deleted blank counterparts keep changed hunks aligned.
- Renderer-neutral diff decorations and a framework-independent
  `mockaco-gpui::DiffSplitSurface` seam that produces separate original and
  modified paint rows.
- Version-aware background diff publication and immediate refresh after edits
  to the modified side. Stale results cannot replace a newer document pair.
- Focused tests for equal files, insert/delete/replace hunks, empty sides,
  Unicode mapping, position/selection conversion, scroll behavior, stale
  results, visible edits, original-side immutability, and the GPUI split seam.

## Deliberate remaining gaps

The milestone does not implement file loading/saving, patch export, merge
commands, conflict resolution, tabs, split-group workspace management, or
filesystem watchers. Native WGPUI painting can consume the presentation seam;
the diff model itself remains independent of WGPUI and filesystem APIs.
