# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What Geode is

Geode is the everything-tool for an index exotic equity derivatives desk: risk, pricing, execution, and data visualization in one permanent, keyboard-driven shell, built in Rust on gpui (Zed's UI framework) + gpui-component. **Phase 1 (the shell) is complete**: the binary opens a working i3-style tiling shell — layered TOML config with provenance (`geode-core::config`), the action registry + keymap engine (contexts, chords, sequences, layered override/unbind), the pure tiling tree + global workspaces (`geode-shell::tiling`), gpui rendering with click-to-focus and a status bar, the command palette (`ctrl+k`) over all actions and themes with fuzzy-match highlighting, and gpui-component theming with 38 bundled themes (`assets/themes/`, config keys `[theme] name/mode` in the `app` doc, `mod+shift+t` toggles mode). Phase 1c polish is also complete: config hot reload (mtime watcher, last-good semantics), session layout persistence (`session.toml`), vim-idiom bindings (`ctrl+w h/j/k/l` focus, `ctrl+w shift+…` move, `shift+h/j/k/l` resize, `ctrl+h`/`ctrl+v` splits, `ctrl+shift+w` close), title-bar toolbar with an inert filter field, workspace sidebar with a settings modal (`mod+,`), which-key hints, and bundled fonts (Inter = UI face, JetBrains Mono = data face via `shell::fonts::MONO`). Multi-window is deliberately not built yet (spec §3.6). `geode-data` is still empty — **Phase 2 (DataService: CSV adapter → DuckDB → generations → scope compilation → snapshots) is next**, then Phase 3 (blotter).

Two documents govern all work and are worth reading before non-trivial changes:

- `docs/PHILOSOPHY.md` — the charter. Key rules: Geode is a lens, not a brain (no financial computation in-app, only view-shaping); every action must be keyboard-reachable; nothing may stall the render thread; data-oriented design throughout (struct-of-arrays, allocation-free hot paths, per-frame heap churn is a defect).
- `docs/superpowers/specs/2026-08-28-geode-foundation-design.md` — the full architecture spec (section references like "spec §7.4" in code comments point here). Implementation plans live in `docs/superpowers/plans/`.

## Commands

```sh
cargo run -p geode-app                                 # run the app (binary is named `geode`)
cargo test --workspace                                 # all tests
cargo test -p geode-demo-data same_seed                # single test by name filter
cargo fmt --check                                      # format check (CI-enforced)
cargo clippy --workspace --all-targets -- -D warnings  # lint (CI-enforced, warnings are errors)
cargo bench --workspace --no-run                       # compile benches (CI-enforced)
cargo bench -p geode-demo-data                         # run criterion benchmarks
```

CI (`.github/workflows/ci.yml`) runs all four checks on **both macOS and Windows** — keep both platforms building.

## Architecture

Cargo workspace with strict layering, enforced by crate visibility:

```
geode-app          the binary: wires shell + modules + services together
  ├─ geode-shell   tiling WM, workspaces, palette, keymap engine, scope/as-of state, theming
  ├─ (modules)     future per-module crates: blotter, config editor, diagnostics…
  ├─ geode-data    DataService: sources, ingestion, DuckDB, archive, query API
  └─ geode-core    shared vocabulary: types, config model, ids, errors, perf utilities
geode-demo-data    deterministic seeded synthetic risk data (SoA) + the criterion bench harness
```

**Dependency rules:** `shell` and `data` never depend on each other, and never on modules; `geode-app` is the only crate where everything meets. No crate other than `geode-data` may open a file or socket — modules only ask DataService.

**Threading model (spec §2):** UI thread (gpui — renders, owns entity state, only ever reads prepared immutable snapshots) / query pool (DuckDB read connections, results delivered as immutable columnar snapshots over channels) / ingest (background writers publishing generation-stamped tables). Performance budgets in spec §7 are contracts: <8ms pure-UI actions, <50ms requery at 1M rows, ingest never drops a foreground frame.

**Testing strategy (spec §10.3):** test weight goes data layer ≫ shell logic ≫ modules. Shell logic (tiling tree, keymap resolution, config merging) is pure logic designed to be testable without a window; modules use gpui `TestAppContext`. TDD per house rules.

## Workspace invariants and gotchas

- **gpui / gpui_platform git deps must stay unpinned** (no `rev`) in `crates/geode-app/Cargo.toml`. gpui-component references gpui unpinned; pinning our copy would make cargo build two incompatible gpui copies. Reproducibility comes from the committed `Cargo.lock`. gpui-component itself IS pinned by rev — upgrade both together, deliberately, on a branch. The full explanation is in a comment in that Cargo.toml.
- **Every new lib/bin target needs `bench = false`** (and `[[bench]]` targets need `harness = false`) so `cargo bench` runs criterion cleanly instead of the built-in libtest harness. This is a workspace-wide invariant — copy the pattern from any existing crate.
- `gpui_platform` uses the `runtime_shaders` feature so Metal shaders compile at runtime without a full Xcode install (no-op off macOS).
- In `main`, `gpui_component::init(cx)` must run before any component use, and the root view is wrapped in `gpui_component::Root` (which also renders the dialog/notification layers).
- Dialogs open through `shell::dialog::open_shell_dialog`, never `window.open_dialog` directly — it's the one standard door that cancels pending keymap sequences and closes an open palette before setting `ShellView`'s own `modal` state. Modals are Geode's own instant, self-owned overlay (backdrop + panel painted directly in `ShellView::render`, palette-style — no animation), not gpui-component's `Dialog`: that component hardwires a 250ms entrance animation with no opt-out at the pinned rev, which read as slow next to the instant palette, so this crate stopped routing through it for its own modals.
- `geode_demo_data::write_csv` does no quoting/escaping — it relies on all string columns drawing from fixed comma-free vocabularies. Revisit if a vocabulary ever grows free-form values.
- Release and bench profiles keep debug symbols on purpose (profiling support, spec §7.4).

## gpui skills

The `gpui` and `gpui-component` skills (available via the Skill tool) are vendored into this repo from longbridge/gpui-component and tracked in `skills-lock.json`. Use them when touching any gpui rendering, entity, async, focus, or component code.
