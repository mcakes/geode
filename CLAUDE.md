# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What Geode is

Geode is the everything-tool for an index exotic equity derivatives desk: risk, pricing, execution, and data visualization in one permanent, keyboard-driven shell, built in Rust on gpui (Zed's UI framework) + gpui-component. **Phase 1 (the shell) is complete**: the binary opens a working i3-style tiling shell — layered TOML config with provenance (`geode-core::config`), the action registry + keymap engine (contexts, chords, sequences, layered override/unbind), the pure tiling tree + global workspaces (`geode-shell::tiling`), gpui rendering with click-to-focus and a status bar, the command palette (`ctrl+k`) over all actions and themes with fuzzy-match highlighting, and gpui-component theming with 38 bundled themes (`assets/themes/`, config keys `[theme] name/mode` in the `app` doc, `mod+shift+t` toggles mode). Phase 1c polish is also complete: config hot reload (mtime watcher, last-good semantics), session layout persistence (`session.toml`), directional bindings (`mod+h/j/k/l` focus, `ctrl+alt+arrows` move, `shift+arrows` resize, `ctrl+h`/`ctrl+v` splits, `mod+e` orientation toggle, `ctrl+w` close), title-bar toolbar with an inert filter field, workspace sidebar with a settings modal (`ctrl+,`), which-key hints, and bundled fonts (Inter = UI face, JetBrains Mono = data face via `shell::fonts::MONO`). Multi-window is deliberately not built yet (spec §3.6).

**Phase 2 (DataService) is complete**, governed by `docs/superpowers/specs/2026-08-30-geode-phase-2-data-design.md`. Phase 2a built storage and ingest: sentinel-gated discovery, CSV → DuckDB load, the grain split (`Position`/`Instrument`/`Underlying`/`UnderlyingPair`, so `SUM` cannot double-count), per-file generations with a live/archive pair per `(dataset, grain)`, `(dataset, batch, book)` partitioning, retention, and the freshness catalog. Phase 2b built the query path: the restricted scope grammar and its compiler, views and derived dimensions, `Attribution`/`ScopeSemantics`, the grain-aware tree compiler (one statement per view, `GROUPING SETS` bounded to the expanded depth), cross-dataset joins, ENUM dictionary encoding, the query pool with cancellation and coalescing, as-of routing, `Snapshot`, and the `DataService` facade. The §7.1 <50ms requery contract holds at 1M rows — numbers in `docs/perf.md`.

**Phase 3 (blotter) is next, and its prerequisites are done** — `docs/phase-3-prerequisites.md` records what was fixed and the three items deliberately not implemented as written. Phase 3 deletes the throwaway data probe (`geode-shell::dataprobe` + `geode-app/src/probe.rs`, `mod+shift+d`, opt-in via `GEODE_PROBE_DIR`), which exists only because the §7.1 budget is specified through a painted frame and the benchmarks stop at the snapshot. The probe can be run against a working sample config: see `examples/probe-config/datasets.toml`.

**Phase 3a is done:** `[sources]` is read (`sources.toml`, `SourceSpec::from_doc`), `DataService` owns a discovery scheduler and one ingest runner, and modules reach it through `DataHandle` (`geode_data::handle`). The probe now rides the handle; it is deleted in Phase 3c.

**Phase 3b is done:** the shell now hosts module occupants through the hosting contract (`geode_shell::module`), with one shared `Frame` entity every tile observes, count prefixes in the keymap engine (`KeyContext::counts`), session `tiles` restored from layout, a per-tile command line (`/` and `:` in the `tile` context), slot numbering via `ctrl+0..9` (`ctrl+0` clears a slot; under `keymap.mod = "ctrl"` the shipped `workspace::switch_N` bindings win `ctrl+1..9`, so setting a slot is reached via the palette instead — `ctrl+0` still works, since no `mod+0` binding exists to collide with it), and `RequeryStats` displayed in the perf overlay. Plan 3c (the blotter, `--demo`, probe deletion) follows.

**Sequencing constraint, satisfied by Phase 3a:** `[sources]` landed before the probe's deletion. The probe (`geode-app/src/probe.rs`) now builds its `SourceSpec`s from `sources.toml` when present and from `GEODE_PROBE_DIR` otherwise; Phase 3c deletes it.

**Cold start is on hold pending measurement — read `docs/ingest-cold-start-handoff.md` before touching it.** The 1.87× parallel-staging figure in `docs/perf.md` measures `read_csv` alone, not the real staging path (`read_csv` + `split_by_grain`), so it describes a narrower operation than the change would affect. That handoff also records what implementation hits — chiefly that `staging_raw` and `staging_{grain}` are fixed global names created with `create or replace table`, so concurrent staging would overwrite itself.

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
cargo bench -p geode-demo-data                         # run criterion benchmarks (data generator)
cargo bench -p geode-shell                             # run criterion benchmarks (shell pure cores — see docs/perf.md)
zsh scripts/mutation-check.sh                          # mutation harness (177 entries) — see below
zsh scripts/mutation-check.sh "scope:"                 # just the entries whose name contains a substring
zsh scripts/mutation-check.sh --changed                # only entries whose file changed since main (the everyday form)
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

**A green suite proves less here than you would expect, and `scripts/mutation-check.sh` is the answer.** Five review rounds on the query path each found Critical, silent wrong-data defects, and the cause was the same every time: a fixture that could not reach the defect. The harness breaks one load-bearing behaviour at a time and runs the suite — a `SURVIVED` line is a branch no test can see. Run it after touching the compiler, scope lowering, as-of routing, publish, retention, discovery or the grain vocabulary, and **add an entry for every behaviour you change**. Its header documents the rules learned the hard way; the two worth knowing up front are that a test asserting on *markers* will not notice a wrong *value*, and that an entry can lie in three ways — no test behind it, two defences overlapping so neither is isolated, or a mutation that breaks something other than what its name claims and is "caught" for the wrong reason. An entry's optional 6th argument names the specific test expected to catch it, so day-to-day runs (especially `--changed`) check that one test before falling back to the full crate suite. **Commit before you mutate:** restoring a mutated file with `git checkout` discards uncommitted work with it.

## Workspace invariants and gotchas

- **gpui / gpui_platform git deps must stay unpinned** (no `rev`) in `crates/geode-app/Cargo.toml`. gpui-component references gpui unpinned; pinning our copy would make cargo build two incompatible gpui copies. Reproducibility comes from the committed `Cargo.lock`. gpui-component itself IS pinned by rev — upgrade both together, deliberately, on a branch. The full explanation is in a comment in that Cargo.toml.
- **Every new lib/bin target needs `bench = false`** (and `[[bench]]` targets need `harness = false`) so `cargo bench` runs criterion cleanly instead of the built-in libtest harness. This is a workspace-wide invariant — copy the pattern from any existing crate.
- `gpui_platform` uses the `runtime_shaders` feature so Metal shaders compile at runtime without a full Xcode install (no-op off macOS).
- In `main`, `gpui_component::init(cx)` must run before any component use, and the root view is wrapped in `gpui_component::Root` (which also renders the dialog/notification layers).
- Dialogs open through `shell::dialog::open_shell_dialog`, never `window.open_dialog` directly — it's the one standard door that cancels pending keymap sequences and closes an open palette before setting `ShellView`'s own `modal` state. Modals are Geode's own instant, self-owned overlay (backdrop + panel painted directly in `ShellView::render`, palette-style — no animation), not gpui-component's `Dialog`: that component hardwires a 250ms entrance animation with no opt-out at the pinned rev, which read as slow next to the instant palette, so this crate stopped routing through it for its own modals.
- `geode_demo_data::write_csv` does no quoting/escaping — it relies on all string columns drawing from fixed comma-free vocabularies. Revisit if a vocabulary ever grows free-form values.
- Release and bench profiles keep debug symbols on purpose (profiling support, spec §7.4).
- **A tile occupant that tracks its own focus handle takes focus on click; the tile's mouse-down handler re-arms `pending_focus_restore` so the shell's chords survive. Keep it.**

## gpui skills

The `gpui` and `gpui-component` skills (available via the Skill tool) are vendored into this repo from longbridge/gpui-component and tracked in `skills-lock.json`. Use them when touching any gpui rendering, entity, async, focus, or component code.
