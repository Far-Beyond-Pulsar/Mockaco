# mockaco-gpui

`mockaco-gpui` is the presentation and input boundary for Mockaco. Its
default build is framework-independent and provides a deterministic
`EditorSurface`, render-frame data, scrolling, input translation, mouse
selection, and IME routing model. This keeps those behaviors testable without
creating a window or GPU context.

The `native-wgpui` feature enables the real crates.io `wgpui` dependency at
version `0.3.6` and exposes `native::WgpuiEditorView`, a thin native rendering
adapter. The native view intentionally consumes the same render-frame model;
document editing, display mapping, and language contracts remain in their
framework-independent crates.

