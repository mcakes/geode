# Geode Phase 0 (Skeleton) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A building, testing, benchmarked Cargo workspace with the spec's crate boundaries, a deterministic demo-data generator, CI on macOS + Windows, and an empty gpui shell window.

**Architecture:** Cargo workspace with the layered crates from the spec (§2): `geode-core`, `geode-data`, `geode-shell`, `geode-app`, plus `geode-demo-data` (the §7.4/§10.3 synthetic data generator, used by benchmarks now and `--demo` later). Phase 0 crates other than `geode-demo-data` and `geode-app` are intentionally near-empty — the deliverable is the skeleton, boundaries, and toolchain, not features.

**Tech Stack:** Rust (stable, edition 2024), gpui + gpui-component (git deps, pinned by rev), rand, criterion, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-08-28-geode-foundation-design.md`

## Global Constraints

- Both platforms must always work: Windows (primary deployment) and macOS (development). CI builds both.
- Crate dependency rules (spec §2): `shell` and `data` never depend on each other or on modules; only `geode-app` depends on everything. No crate outside `geode-app`/`geode-shell` may depend on gpui.
- Data-oriented design (philosophy §6): generated data is struct-of-arrays, never a `Vec<Row>` of structs.
- No fixture files (spec §7.4): benchmark/demo data comes from the checked-in generator, seeded and deterministic.
- gpui and gpui-component are pinned to explicit git revs in `Cargo.toml` (spec §11); upgrades are deliberate, never implicit.
- All commits: end the message with the project's standard co-author trailer used in prior commits.

## File Structure

```
Cargo.toml                          workspace root: members, shared package keys, profiles
rust-toolchain.toml                 pin channel = stable
rustfmt.toml                        default rustfmt, explicitly committed
.gitignore                          /target
.github/workflows/ci.yml            fmt + clippy + test + bench-compile on macOS & Windows
crates/geode-core/                  shared vocabulary (near-empty in phase 0)
crates/geode-data/                  DataService home (near-empty in phase 0)
crates/geode-shell/                 shell home (near-empty in phase 0)
crates/geode-demo-data/             deterministic generator + CSV writer + criterion bench
crates/geode-app/                   the binary: empty shell window
```

---

### Task 1: Workspace skeleton

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `rustfmt.toml`, `.gitignore`
- Create: `crates/geode-core/Cargo.toml`, `crates/geode-core/src/lib.rs`
- Create: `crates/geode-data/Cargo.toml`, `crates/geode-data/src/lib.rs`
- Create: `crates/geode-shell/Cargo.toml`, `crates/geode-shell/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: a `cargo build`-able workspace; later tasks add members `geode-demo-data` (Task 2) and `geode-app` (Task 4) to the `members` list created here.

- [ ] **Step 1: Write root `Cargo.toml`**

```toml
[workspace]
resolver = "3"
members = [
    "crates/geode-core",
    "crates/geode-data",
    "crates/geode-shell",
]

[workspace.package]
version = "0.1.0"
edition = "2024"
publish = false

[workspace.dependencies]
geode-core = { path = "crates/geode-core" }

# Release builds keep debug symbols: profiling support from day one (spec §7.4).
[profile.release]
debug = true
lto = "thin"

[profile.bench]
debug = true
```

- [ ] **Step 2: Write `rust-toolchain.toml`, `rustfmt.toml`, `.gitignore`**

`rust-toolchain.toml`:
```toml
[toolchain]
channel = "stable"
```

`rustfmt.toml` (defaults, committed so everyone formats identically):
```toml
edition = "2024"
```

`.gitignore`:
```
/target
```

- [ ] **Step 3: Write the three library crates**

`crates/geode-core/Cargo.toml`:
```toml
[package]
name = "geode-core"
version.workspace = true
edition.workspace = true
publish.workspace = true

[dependencies]
```

`crates/geode-core/src/lib.rs`:
```rust
//! Shared vocabulary for Geode: core types, config model, ids, errors,
//! and performance utilities. See docs/superpowers/specs/ §2.
//! Intentionally near-empty in phase 0.
```

`crates/geode-data/Cargo.toml`:
```toml
[package]
name = "geode-data"
version.workspace = true
edition.workspace = true
publish.workspace = true

[dependencies]
geode-core.workspace = true
```

`crates/geode-data/src/lib.rs`:
```rust
//! DataService: sources, ingestion, DuckDB storage, archive, and the
//! query API. See docs/superpowers/specs/ §5. The only door to data —
//! no other crate opens files or sockets.
//! Intentionally near-empty in phase 0.
```

`crates/geode-shell/Cargo.toml`:
```toml
[package]
name = "geode-shell"
version.workspace = true
edition.workspace = true
publish.workspace = true

[dependencies]
geode-core.workspace = true
```

`crates/geode-shell/src/lib.rs`:
```rust
//! The Geode shell: tiling window management, workspaces, keymap engine,
//! command palette, shared frame state (scope/grouping/as-of), theming.
//! See docs/superpowers/specs/ §2–§4.
//! Intentionally near-empty in phase 0.
//!
//! Dependency rule: this crate never depends on geode-data or on modules.
```

- [ ] **Step 4: Verify the workspace builds and is clean**

Run: `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: all succeed with no warnings.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock rust-toolchain.toml rustfmt.toml .gitignore crates/
git commit -m "feat: cargo workspace skeleton with spec crate boundaries"
```

---

### Task 2: Demo-data generator

**Files:**
- Create: `crates/geode-demo-data/Cargo.toml`
- Create: `crates/geode-demo-data/src/lib.rs`
- Modify: root `Cargo.toml` (add `"crates/geode-demo-data"` to `members`)

**Interfaces:**
- Consumes: nothing from other crates (deliberately — the generator must be usable from benches and tests everywhere).
- Produces:
  - `GeneratorConfig { rows: usize, seed: u64 }` (with `Default`: 100_000 rows, seed 42)
  - `RiskBatch` — struct-of-arrays risk snapshot; `RiskBatch::len(&self) -> usize`
  - `generate(config: &GeneratorConfig) -> RiskBatch`
  - `write_csv<W: std::io::Write>(batch: &RiskBatch, out: &mut W) -> std::io::Result<()>`
  - Task 3 benches `generate`; phase 2's CSV adapter tests will consume `write_csv` output.

- [ ] **Step 1: Create the crate and register it**

`crates/geode-demo-data/Cargo.toml`:
```toml
[package]
name = "geode-demo-data"
version.workspace = true
edition.workspace = true
publish.workspace = true

[dependencies]
rand = "0.9"
```

Add `"crates/geode-demo-data"` to `members` in the root `Cargo.toml`.

`crates/geode-demo-data/src/lib.rs` (docs and empty module only for now):
```rust
//! Deterministic synthetic risk data for benchmarks, tests, and `--demo`
//! mode (spec §7.4, §10.3). Seeded: same config always yields identical
//! data. Struct-of-arrays per the performance philosophy — no row objects.
```

- [ ] **Step 2: Write the failing tests**

Append to `crates/geode-demo-data/src/lib.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_requested_row_count() {
        let batch = generate(&GeneratorConfig { rows: 1_000, seed: 42 });
        assert_eq!(batch.len(), 1_000);
        assert_eq!(batch.book.len(), 1_000);
        assert_eq!(batch.npv.len(), 1_000);
    }

    #[test]
    fn same_seed_yields_identical_data() {
        let cfg = GeneratorConfig { rows: 500, seed: 7 };
        let a = generate(&cfg);
        let b = generate(&cfg);
        assert_eq!(a.position_id, b.position_id);
        assert_eq!(a.book, b.book);
        assert_eq!(a.npv, b.npv);
        assert_eq!(a.delta, b.delta);
    }

    #[test]
    fn different_seeds_yield_different_data() {
        let a = generate(&GeneratorConfig { rows: 500, seed: 1 });
        let b = generate(&GeneratorConfig { rows: 500, seed: 2 });
        assert_ne!(a.npv, b.npv);
    }

    #[test]
    fn dimensions_have_realistic_bounded_cardinality() {
        use std::collections::HashSet;
        let batch = generate(&GeneratorConfig { rows: 10_000, seed: 42 });
        let books: HashSet<_> = batch.book.iter().collect();
        let underlyings: HashSet<_> = batch.underlying.iter().collect();
        assert!(books.len() > 1 && books.len() <= 20, "books: {}", books.len());
        assert!(underlyings.len() > 1 && underlyings.len() <= 10);
    }

    #[test]
    fn csv_has_header_and_one_line_per_row() {
        let batch = generate(&GeneratorConfig { rows: 10, seed: 42 });
        let mut out = Vec::new();
        write_csv(&batch, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 11);
        assert_eq!(
            lines[0],
            "position_id,book,desk,model_code,underlying,instrument,npv,pnl,delta,gamma,vega,theta,rho"
        );
        assert!(lines[1].starts_with("0,"));
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p geode-demo-data`
Expected: compile error — `GeneratorConfig`, `RiskBatch`, `generate`, `write_csv` not defined.

- [ ] **Step 4: Implement the generator**

Insert above the tests in `crates/geode-demo-data/src/lib.rs`:
```rust
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

pub struct GeneratorConfig {
    pub rows: usize,
    pub seed: u64,
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        Self { rows: 100_000, seed: 42 }
    }
}

/// Struct-of-arrays risk snapshot: one Vec per column, index = row.
pub struct RiskBatch {
    pub position_id: Vec<u64>,
    pub book: Vec<String>,
    pub desk: Vec<String>,
    pub model_code: Vec<String>,
    pub underlying: Vec<String>,
    pub instrument: Vec<String>,
    pub npv: Vec<f64>,
    pub pnl: Vec<f64>,
    pub delta: Vec<f64>,
    pub gamma: Vec<f64>,
    pub vega: Vec<f64>,
    pub theta: Vec<f64>,
    pub rho: Vec<f64>,
}

impl RiskBatch {
    pub fn len(&self) -> usize {
        self.position_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

const UNDERLYINGS: &[&str] = &[
    "SPX", "SX5E", "NKY", "UKX", "NDX", "RTY", "DAX", "SMI", "HSI", "KOSPI2",
];
const MODEL_CODES: &[&str] = &[
    "EURP", "AMRP", "VSWP", "AUTO", "CLIQ", "BARR", "DIGI", "VANL",
];
const DESKS: &[&str] = &["IDX_EXO_EU", "IDX_EXO_US", "IDX_EXO_AS"];
const BOOK_COUNT: usize = 20;

pub fn generate(config: &GeneratorConfig) -> RiskBatch {
    let mut rng = StdRng::seed_from_u64(config.seed);
    let n = config.rows;

    let mut batch = RiskBatch {
        position_id: Vec::with_capacity(n),
        book: Vec::with_capacity(n),
        desk: Vec::with_capacity(n),
        model_code: Vec::with_capacity(n),
        underlying: Vec::with_capacity(n),
        instrument: Vec::with_capacity(n),
        npv: Vec::with_capacity(n),
        pnl: Vec::with_capacity(n),
        delta: Vec::with_capacity(n),
        gamma: Vec::with_capacity(n),
        vega: Vec::with_capacity(n),
        theta: Vec::with_capacity(n),
        rho: Vec::with_capacity(n),
    };

    for i in 0..n {
        batch.position_id.push(i as u64);
        batch.book.push(format!("BK{:03}", rng.random_range(0..BOOK_COUNT)));
        batch.desk.push(DESKS[rng.random_range(0..DESKS.len())].to_string());
        batch
            .model_code
            .push(MODEL_CODES[rng.random_range(0..MODEL_CODES.len())].to_string());
        batch
            .underlying
            .push(UNDERLYINGS[rng.random_range(0..UNDERLYINGS.len())].to_string());
        batch.instrument.push(format!("INST{i:08}"));
        batch.npv.push(rng.random_range(-5_000_000.0..5_000_000.0));
        batch.pnl.push(rng.random_range(-500_000.0..500_000.0));
        batch.delta.push(rng.random_range(-100_000.0..100_000.0));
        batch.gamma.push(rng.random_range(-5_000.0..5_000.0));
        batch.vega.push(rng.random_range(-50_000.0..50_000.0));
        batch.theta.push(rng.random_range(-10_000.0..10_000.0));
        batch.rho.push(rng.random_range(-20_000.0..20_000.0));
    }

    batch
}

pub fn write_csv<W: std::io::Write>(batch: &RiskBatch, out: &mut W) -> std::io::Result<()> {
    writeln!(
        out,
        "position_id,book,desk,model_code,underlying,instrument,npv,pnl,delta,gamma,vega,theta,rho"
    )?;
    for i in 0..batch.len() {
        writeln!(
            out,
            "{},{},{},{},{},{},{},{},{},{},{},{},{}",
            batch.position_id[i],
            batch.book[i],
            batch.desk[i],
            batch.model_code[i],
            batch.underlying[i],
            batch.instrument[i],
            batch.npv[i],
            batch.pnl[i],
            batch.delta[i],
            batch.gamma[i],
            batch.vega[i],
            batch.theta[i],
            batch.rho[i],
        )?;
    }
    Ok(())
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p geode-demo-data`
Expected: 5 tests PASS.

- [ ] **Step 6: Lint, format, commit**

Run: `cargo clippy -p geode-demo-data --all-targets -- -D warnings && cargo fmt`
Expected: clean.

```bash
git add Cargo.toml Cargo.lock crates/geode-demo-data
git commit -m "feat: deterministic struct-of-arrays demo risk data generator"
```

---

### Task 3: Benchmark harness

**Files:**
- Create: `crates/geode-demo-data/benches/generate.rs`
- Modify: `crates/geode-demo-data/Cargo.toml` (add criterion dev-dependency and bench target)

**Interfaces:**
- Consumes: `generate`, `GeneratorConfig` from Task 2.
- Produces: the workspace's criterion harness pattern — phase 2's query/snapshot benchmarks copy this shape; CI (Task 5) compiles benches via `cargo bench --no-run`.

- [ ] **Step 1: Add criterion to the crate**

Run: `cargo add --dev criterion -p geode-demo-data`

Then append to `crates/geode-demo-data/Cargo.toml`:
```toml
[[bench]]
name = "generate"
harness = false
```

- [ ] **Step 2: Write the benchmark**

`crates/geode-demo-data/benches/generate.rs`:
```rust
//! Benchmarks data generation itself. Exists in phase 0 primarily to
//! establish the workspace's criterion harness; phase 2 adds the
//! query/snapshot pipeline benchmarks that the spec's budgets (§7) gate on.

use criterion::{criterion_group, criterion_main, Criterion};
use geode_demo_data::{generate, GeneratorConfig};
use std::hint::black_box;

fn bench_generate(c: &mut Criterion) {
    let mut group = c.benchmark_group("generate");
    group.sample_size(10);
    group.bench_function("100k_rows", |b| {
        b.iter(|| black_box(generate(&GeneratorConfig { rows: 100_000, seed: 42 })))
    });
    group.bench_function("1m_rows", |b| {
        b.iter(|| black_box(generate(&GeneratorConfig { rows: 1_000_000, seed: 42 })))
    });
    group.finish();
}

criterion_group!(benches, bench_generate);
criterion_main!(benches);
```

- [ ] **Step 3: Verify the bench compiles and runs**

Run: `cargo bench -p geode-demo-data -- --quick`
Expected: both benchmarks run and report times (1m_rows plausibly hundreds of ms — it allocates ~13 Vecs of 1M entries; that's fine, it's not a hot path).

- [ ] **Step 4: Lint, format, commit**

Run: `cargo clippy -p geode-demo-data --all-targets -- -D warnings && cargo fmt`
Expected: clean.

```bash
git add Cargo.toml Cargo.lock crates/geode-demo-data
git commit -m "feat: criterion benchmark harness over the demo-data generator"
```

---

### Task 4: Empty shell window (geode-app)

**Files:**
- Create: `crates/geode-app/Cargo.toml`, `crates/geode-app/src/main.rs`
- Modify: root `Cargo.toml` (add `"crates/geode-app"` to `members`)

**Interfaces:**
- Consumes: nothing from other crates yet (wiring `geode-shell`/`geode-data` into the app is phase 1/2 work).
- Produces: the runnable `geode` binary; the pinned gpui/gpui-component revs that every later UI task builds against.

- [ ] **Step 1: Create the app crate**

`crates/geode-app/Cargo.toml`:
```toml
[package]
name = "geode-app"
version.workspace = true
edition.workspace = true
publish.workspace = true

[[bin]]
name = "geode"
path = "src/main.rs"

[dependencies]
gpui = { git = "https://github.com/zed-industries/zed" }
gpui_platform = { git = "https://github.com/zed-industries/zed", features = ["font-kit"] }
gpui-component = { git = "https://github.com/longbridge/gpui-component" }
gpui-component-assets = { git = "https://github.com/longbridge/gpui-component" }
```

Add `"crates/geode-app"` to `members` in the root `Cargo.toml`.

- [ ] **Step 2: Write `main.rs`**

`crates/geode-app/src/main.rs`:
```rust
//! The Geode binary. Phase 0: an empty shell window proving the
//! gpui + gpui-component toolchain on both platforms.

use gpui::prelude::*;
use gpui::{div, App, Context, Window, WindowOptions};
use gpui_component::{ActiveTheme as _, Root};

struct GeodeApp;

impl Render for GeodeApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child("geode — phase 0")
            .children(Root::render_dialog_layer(cx))
            .children(Root::render_notification_layer(cx))
    }
}

fn main() {
    gpui_platform::application()
        .with_assets(gpui_component_assets::Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx); // must run before any component use

            cx.spawn(async move |cx| {
                cx.open_window(WindowOptions::default(), |window, cx| {
                    let view = cx.new(|_| GeodeApp);
                    cx.new(|cx| Root::new(view, window, cx))
                })
                .expect("failed to open window");
            })
            .detach();
        });
}
```

Note for the implementer: gpui's API moves; if names in this snippet have drifted at the pinned revs (e.g. import paths, `open_window` signature), consult the gpui-component repo's `examples/` directory at the resolved rev and adapt — the structure (application → init → open_window → `Root` wrapping the first view, dialog/notification layers in render) is the contract, exact identifiers are not.

- [ ] **Step 3: Build and pin the revs**

Run: `cargo build -p geode-app` (first build compiles the zed dependency tree — expect many minutes).
Expected: success.

Then read the resolved commits out of `Cargo.lock` (the `source = "git+..."` lines for `gpui` and `gpui-component`) and pin them, e.g.:
```toml
gpui = { git = "https://github.com/zed-industries/zed", rev = "<resolved-sha>" }
gpui_platform = { git = "https://github.com/zed-industries/zed", rev = "<resolved-sha>", features = ["font-kit"] }
gpui-component = { git = "https://github.com/longbridge/gpui-component", rev = "<resolved-sha>" }
gpui-component-assets = { git = "https://github.com/longbridge/gpui-component", rev = "<resolved-sha>" }
```

Run: `cargo build -p geode-app` again.
Expected: success, no re-resolution.

- [ ] **Step 4: Run and verify by eye**

Run: `cargo run -p geode-app`
Expected: a window opens titled per platform default, themed background, centered text "geode — phase 0". Close it cleanly. (Windows verification happens when the repo first builds on a Windows machine/CI — Task 5.)

- [ ] **Step 5: Lint, format, commit**

Run: `cargo clippy -p geode-app --all-targets -- -D warnings && cargo fmt`
Expected: clean.

```bash
git add Cargo.toml Cargo.lock crates/geode-app
git commit -m "feat: empty gpui shell window with pinned gpui/gpui-component revs"
```

---

### Task 5: CI on macOS and Windows

**Files:**
- Create: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: the whole workspace from Tasks 1–4.
- Produces: the CI gate later phases extend (benchmark regression gating is phase 2, when the benchmarks that map to spec budgets exist).

- [ ] **Step 1: Write the workflow**

`.github/workflows/ci.yml`:
```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

env:
  CARGO_TERM_COLOR: always

jobs:
  check:
    strategy:
      fail-fast: false
      matrix:
        os: [macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - name: Format
        run: cargo fmt --check
      - name: Clippy
        run: cargo clippy --workspace --all-targets -- -D warnings
      - name: Test
        run: cargo test --workspace
      - name: Benches compile
        run: cargo bench --workspace --no-run
```

- [ ] **Step 2: Validate the workflow file locally**

Run: `ruby -ryaml -e 'YAML.load_file(".github/workflows/ci.yml"); puts "ok"'` (or any available YAML parser).
Expected: `ok`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: fmt, clippy, test, and bench-compile on macOS and Windows"
```

- [ ] **Step 4: Verify on a runner (when a remote exists)**

If the repo has a GitHub remote, push and confirm both matrix legs pass; the first Windows leg is the real "builds on Windows" verification. If no remote exists yet, note this in the task report — runner verification is deferred, not skipped: it becomes the first action after the remote is created. Expect the first uncached run to be long (the zed dependency tree).

---

## Self-Review Notes

- **Spec coverage (Phase 0 scope, spec §12):** workspace + crate boundaries (Task 1), demo-data generator (Task 2), benchmark harness (Task 3), empty shell window with pinned revs (Task 4), CI on both platforms (Task 5). `geode-modules` crates deliberately deferred to phase 3 per YAGNI — nothing would live in them.
- **Type consistency:** `GeneratorConfig`/`RiskBatch`/`generate`/`write_csv` names match across Tasks 2 and 3.
- **Known risk:** exact gpui API identifiers at the pinned revs may drift from the Task 4 snippet; the task carries explicit adapt-from-examples guidance rather than pretending the snippet is gospel.
