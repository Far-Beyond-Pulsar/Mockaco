# Mockaco Phase 9 — Validation and Performance

Phase 9 closes the validation gaps around the reusable editor component while
keeping the core framework-independent. It adds no tabs, split-group workspace
manager, explorer, persistence, filesystem watcher, or Pulsar adapter.

## New validation surfaces

### Deterministic fixture corpus

`mockaco-fixtures` is an integration-test crate covering the public contracts
of core, renderer, language, LSP, diff, and the framework-neutral GPUI adapter.
Run it with:

```powershell
cargo test -p mockaco-fixtures --test behavior -- --nocapture
```

The ten fixture cases cover editing, multiple selections, IME commit/cancel,
undo/redo, search/replace, folding, wrapping, pathological-line bounds,
diagnostics, incremental language results, LSP synchronization/cancellation,
split diff mapping, host lifecycle failures/conflicts, and runtime-free
presentation rendering.

### Standalone harness

The harness exercises load/edit/save, display preparation, and diff contracts
without a window, filesystem, WGPUI runtime, or Pulsar:

```powershell
cargo run --release -p mockaco-fixtures --bin mockaco-standalone
```

It prints fixture count, document bytes/version, visible render rows, diff
hunks, and saved-event size.

### Benchmark workloads

`mockaco-bench` generates deterministic source-like and pathological documents
of exactly 1,024, 102,400, 1,048,576, and 1,048,577 bytes. Each report records
byte count, line count, maximum line length, SHA-256, viewport size, and
per-operation timings for open/snapshot, edit/transaction mapping, bounded
viewport/display preparation, search, folding, diff, and lifecycle save.

Quick JSON smoke report:

```powershell
cargo run --release -p mockaco-bench -- --implementation mockaco --case 1k --viewport-lines 60 --warmup 2 --iterations 10 --json target/mockaco-bench/1k.json
```

Run every required workload:

```powershell
$cases = @('1k', '100k', '1m', 'pathological-line')
foreach ($case in $cases) {
    cargo run --release -p mockaco-bench -- --implementation mockaco --case $case --viewport-lines 60 --warmup 20 --iterations 100 --json "target/mockaco-bench/$case.json"
}
```

Criterion uses the same workload functions for statistical measurements:

```powershell
cargo bench -p mockaco-bench --bench editor -- 1k --sample-size 10
```

The `current` implementation option is intentionally rejected because the
legacy editor source is not present in this checkout. No comparative legacy
numbers are claimed until that source and its pinned dependency snapshot are
available.

## Compatibility matrix

| Contract | Deterministic coverage | Benchmark coverage | External responsibility |
|---|---|---|---|
| Text editing, selections, IME, undo/redo | `mockaco-fixtures/tests/behavior.rs` | edit/transaction mapping | host input routing |
| Search, folding, wrapping, viewport bounds | behavior fixture | search, folding, viewport preparation | host rendering policy |
| Diagnostics and language versioning | behavior fixture plus crate tests | not timed as parser work | parser/highlighter provider |
| LSP synchronization/cancellation | behavior fixture plus crate tests | lifecycle path only | transport/process/runtime |
| Split diff mapping and stale results | behavior fixture plus crate tests | diff workload | workspace presentation |
| Load/save/dirty/external/close lifecycle | behavior fixture plus crate tests | lifecycle save workload | host bytes, I/O, dialogs, watcher |
| Large files/pathological lines | exact generated workloads and bounded viewport fixture | 1K/100K/1M/pathological cases | benchmark machine/environment |
| Legacy compatibility/cutover | not measurable in this checkout | not measured | recovered legacy submodules and Pulsar adapter |

## Measurement policy

The harness reports measurements from the machine and build profile on which it
runs; it does not check in performance claims or compare unrelated machines.
Use release builds, retain each JSON report's generator SHA-256, and compare
like-for-like workload metadata. Open, edit, viewport, search, diff, and save
remain separate measurements so a fast average cannot hide a slow path.

## Deliberate remaining gaps

The legacy editor and WGPUI-Component submodules remain unavailable, so there
is no current-vs-Mockaco compatibility result. Filesystem access, watcher
coalescing, save dialogs, tabs, split groups, explorer state, persistence, and
the Pulsar adapter/cutover remain host-owned follow-up work.
