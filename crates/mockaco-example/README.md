# Mockaco native showcase

Run the real WGPUI editor surface with:

```powershell
cargo run -p mockaco-example --features native-wgpui
```

The window starts with an in-memory Rust-like document. It is an editor
component showcase, not a workspace shell: there are no tabs, explorer,
persistence, file watcher, or filesystem backend.

Controls demonstrate:

- direct native WGPUI editing, caret movement, mouse selection, scrolling, and
  wrapping;
- multi-cursor selection, the framework-neutral IME commit contract, undo/redo,
  folding, and diagnostics/decorations;
- deterministic host lifecycle actions for load, local edit, save success,
  save failure, external conflict, keep-local/reload-external, and close
  decisions;
- deterministic mock LSP status for completion, hover, navigation, code action,
  semantic token, and diagnostic result categories;
- an original/modified split diff view. The original side is read-only and the
  modified side is an actual editable native editor surface.

All provider data is labeled as demo/in-memory data. The WGPUI host IME event
plumbing remains intentionally minimal; the IME button exercises the core
contract directly.

Deterministic state tests live in `src/lib.rs` and run with:

```powershell
cargo test -p mockaco-example
```
