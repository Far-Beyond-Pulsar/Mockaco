# mockaco-gpui

`mockaco-gpui` is the presentation and input boundary for Mockaco. Its
default build is framework-independent and provides a deterministic
`EditorSurface`, render-frame data, scrolling, input translation, mouse
selection, and IME routing model. This keeps those behaviors testable without
creating a window or GPU context.

The `native-wgpui` feature enables the real crates.io `wgpui` dependency at
version `0.3.6` and exposes `native::WgpuiEditorView`. The native view uses
WGPUI's real text elements and shaping pipeline, paints the gutter, selection,
caret, and decoration geometry, and translates WGPUI keyboard, mouse, and
scroll events into the tested router. Document editing, display mapping, and
language contracts remain in their framework-independent crates.
