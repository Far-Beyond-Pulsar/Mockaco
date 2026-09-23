# Mockaco Phase 4

Status: native WGPUI presentation boundary
Date: 2026-09-23

Phase 4 turns the Phase 3 render model into a usable native WGPUI surface while
keeping editing and display mapping framework-independent.

## Scope delivered

- `PaintRow` now carries the visible source text, so the native view uses real
  WGPUI text elements and WGPUI's text shaping pipeline.
- `SurfaceTheme` provides framework-neutral colors for the document, gutter,
  selections, carets, and decorations.
- The native view paints line-number gutters, source text, selection fills,
  decoration underlines, and primary/secondary carets.
- Native WGPUI key, mouse drag, mouse release, and scroll-wheel events are
  translated into the tested `InputRouter` event model.
- Focus is tracked by WGPUI and the editor surface is focused on primary mouse
  interaction.
- Render-frame tests cover source text and visual geometry in addition to the
  existing scrolling, editing, IME, stale-update, and mouse-selection tests.

## Deliberate remaining gaps

This milestone does not add a platform text-input handler for composition
events, clipboard commands, or accessibility exposure. The framework-neutral
IME contract remains available and tested, but host-specific WGPUI IME wiring
needs a follow-up against the eventual host window lifecycle.

Syntax-colored glyph runs, diagnostics from LSP, folding controls, minimap,
completion/hover surfaces, workspace tabs, and file persistence remain in the
LSP/workspace milestone described by `MOCKACO_ARCHITECTURE.md`.
