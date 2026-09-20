# Timeseries Part 3: `geode-chart` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A new crate, `geode-chart`, that turns a `SeriesResult`-shaped model (buckets, per-slot values, stats) into a painted chart: a pure core (scales, a session time axis, a two-pane layout, min-max decimation, a view, a crosshair, a floored palette) and one gpui-component `Plot` element that paints lines, dashed percentile lines, density bars, axes, a grid and a crosshair readout without allocating on an unchanged frame.

**Architecture:** `geode_chart::core` is window-free f32/f64 arithmetic over slices (`Rect`, `Point`, `LinearScale`, `TimeScale`, `Layout`, `View`, `decimate`, `Crosshair`, `Palette`, `Axis`). `geode_chart::model::ChartModel` is the gpui-aware immutable input a tile builds once per delivery and hands to `geode_chart::element::ChartElement`, which implements gpui-component's `Plot` trait (`#[derive(IntoPlot)]`) and keeps its tessellated paths in the component's `PathCaches` keyed on `(model version, view, bounds)`. The crate depends on `geode-core` (the colour floor), `gpui`, `gpui-component` and `chrono`; never on the shell or the data tier. Part 4 (the module) builds the model from a `SeriesResult` and owns the `View`.

**Tech Stack:** Rust 2024, gpui-pre 0.3.5 / gpui-component 0.6.2 (pinned, registry source at `~/.cargo/registry/src/*/gpui-component-0.6.2/src/plot/`), chrono, criterion, proptest.

**Spec:** `docs/superpowers/specs/2026-09-19-geode-timeseries-viewer-design.md` §8 (all of it), §1.2 done-state item 2, §9.2 (the `Axis` enum the module will import from here), §11.1 "Chart core", §11.2, §11.3. The chart rendering spike `docs/superpowers/spikes/2026-08-29-gpui-chart-rendering-spike.md` is the routing rule (paths for vectors, quads only in the hundreds, proper min-max decimation, never lyon's collapse).

## Global Constraints

- **Lens, not brain** (`docs/PHILOSOPHY.md`): the chart computes geometry only. Percentiles and bins arrive computed in the model; the crate never derives a statistic from values.
- **Nothing stalls the render thread; per-frame heap churn is a defect** (spec §8.1, §8.3): on a frame whose `(model version, view, bounds)` key is unchanged the element pushes cached paths and rebuilds nothing on the data path — no decimation, no tessellation, no `xs` refill. The pure core allocates only into caller-owned `&mut Vec`s. Chrome labels (axis ticks, percentile tags) go through gpui-component's own `PlotAxis`/`PlotLabel`, which collect a small `Vec` per axis per frame — the same cost every shipped chart in the component pays; that is the one exception and the as-built section names it.
- **No shell dependency** (spec §8): the rem scale is a parameter. `DESIGN_REM` = 12.0 is duplicated here as a literal with a test pinning it to 12.0 and a comment naming `geode_shell::shell::scale::DESIGN_REM` as the value it mirrors.
- **Design-pixel constants at the `FontSize::Medium` rem** (spec §8): `DENSITY_STRIP` = 80, `TICK_GAP` = 64, `DASH` = 4, `GAP` = 3, `AXIS_WIDTH` = 44, `PANE_GAP` = 6. Added by this plan: `X_AXIS_HEIGHT` = 18 (gpui-component's own `AXIS_GAP`), `Y_TICK_GAP` = 40. Every one resolved through `core::design_px(v, rem_px)`.
- **The readability floor** (spec §8.1): every palette colour clears 3:1 against the theme background through `geode_core::colour::readable_on`; the sweep runs over every bundled theme with no exception list.
- **Layout invariants** (spec §8.1, ruling 12): an axis rect exists only when a visible slot uses that side; the density strip only when density is on; the lower pane only while a visible slot uses `BottomLeft`/`BottomRight`; `split` clamped to `0.2..=0.8`, default 0.7; both panes share one x mapping (same `plot.x` and `plot.w`).
- **Decimation** (spec §8.1): min-max per pixel column, breaking at `NaN`; no column's finite extreme is lost; at most two points per column.
- **Every gpui-kit crate is `=`-pinned in the root `Cargo.toml`** (CLAUDE.md): use `.workspace = true` for `gpui`, `gpui-component`, `gpui_platform`. Never add a git dependency.
- **`bench = false` on the lib and the example; `harness = false` on the bench** (CLAUDE.md workspace invariant).
- **Test-feature parity** (memory `sccache-and-build-cost`): dev-dependencies ask for the same geode-* features `--workspace` resolves (`geode-core` and `geode-shell` with `test-support`, `gpui` with `test-support`).
- **Both macOS and Windows build** (CI): no platform-specific code.
- **Harness** (CLAUDE.md): one `scripts/mutation-check.sh` entry per behaviour, each naming its test (6th argument), package `geode-chart`; `--anchors-only` clean before merge; commit before mutating; never run the harness unfiltered or with `--changed` from a task.
- **No display in the sandbox**: pixel claims are unverified until the example window is run by hand; each task's report says so where it applies.
- **Verification per task**: `cargo fmt --check`, `cargo clippy -p geode-chart --all-targets -- -D warnings`, `cargo test -p geode-chart`. Task 9 runs the workspace forms. Use a private `CARGO_TARGET_DIR` if the shared build lock is held.

## File Structure

```
crates/geode-chart/
  Cargo.toml
  src/lib.rs                 pub mod core; pub mod model; pub mod element; pub use the door types
  src/core/mod.rs            Rect, Point, design_px, DESIGN_REM, the design constants, re-exports
  src/core/axis.rs           Axis, Pane, Side, AxisMode (pure; Part 4 imports these)
  src/core/scale.rs          LinearScale, nice ticks, fmt_tick, fmt_value, axis_domain
  src/core/view.rs           View (unit-agnostic window with clamp)
  src/core/time.rs           TimeScale (session | continuous), Crosshair, ticks + unit chooser
  src/core/layout.rs         LayoutOptions, Pane rects, Layout::solve
  src/core/decimate.rs       decimate + Point::BREAK
  src/core/palette.rs        Palette (floored theme chart colours), to_rgb/to_hsla (private copies)
  src/model.rs               ChartModel, ChartSlot (gpui types: Hsla, SharedString)
  src/element.rs             ChartElement: Plot impl, PathCaches, ChromeCache, REBUILDS counter
  benches/decimate.rs        500,000 points → 1,600 columns + path rebuild
  examples/chart.rs          a window over a generated three-slot model (the display-check vehicle)
scripts/mutation-check.sh    + 12 entries (Task 9)
docs/perf.md                 + "Timeseries chart (spec §8, Part 3)"
docs/superpowers/specs/2026-09-19-geode-timeseries-viewer-design.md   + §8.5 As built (Part 3)
docs/phase-history.md, CLAUDE.md (status row + rules + harness count)
Cargo.toml (workspace members + `geode-chart` workspace dependency)
```

---

### Task 1: Crate scaffold, `Rect`/`Point`, design constants, `LinearScale`

**Files:**
- Create: `crates/geode-chart/Cargo.toml`, `crates/geode-chart/src/lib.rs`, `crates/geode-chart/src/core/mod.rs`, `crates/geode-chart/src/core/scale.rs`
- Modify: `Cargo.toml` (root: members + workspace dependency)

**Interfaces:**
- Produces: `core::{Rect, Point, design_px, DESIGN_REM, DENSITY_STRIP, TICK_GAP, Y_TICK_GAP, DASH, GAP, AXIS_WIDTH, PANE_GAP, X_AXIS_HEIGHT}`, `core::scale::{LinearScale, fmt_tick, fmt_value, axis_domain}`.

- [ ] **Step 1: Manifest and workspace wiring**

`crates/geode-chart/Cargo.toml`:

```toml
[package]
name = "geode-chart"
version.workspace = true
edition.workspace = true
publish.workspace = true

[lib]
bench = false

# `geode-core` for the colour floor (`colour::readable_on`) alone; `gpui`
# and `gpui-component` for the element (`Plot`, `PathCaches`, the tooltip
# plumbing); `chrono` for the time axis's unit boundaries and labels. The
# crate knows nothing of series, sources, the data tier or the shell
# (spec §8): the rem scale arrives as a parameter.
[dependencies]
geode-core.workspace = true
gpui.workspace = true
gpui-component.workspace = true
chrono = "0.4.42"

# `geode-shell` (test-support) is here for ONE test — the palette sweep
# over every bundled theme (`theme::load_bundled`) — the same way
# `geode-marketdata` reaches the theme list. `gpui_platform` is the
# example window's entry point.
[dev-dependencies]
geode-core = { workspace = true, features = ["test-support"] }
geode-shell = { workspace = true, features = ["test-support"] }
gpui = { workspace = true, features = ["test-support"] }
gpui_platform.workspace = true
criterion = "0.8.2"
proptest = "1"

[[bench]]
name = "decimate"
harness = false

# Workspace invariant (CLAUDE.md): every target opts out of the built-in
# libtest harness so `cargo bench` runs criterion cleanly.
[[example]]
name = "chart"
bench = false
```

Root `Cargo.toml`: add `"crates/geode-chart",` to `members` after `"crates/geode-marketdata",` and `geode-chart = { path = "crates/geode-chart" }` to `[workspace.dependencies]` beside `geode-pricing`.

`src/lib.rs`:

```rust
//! `geode-chart`: the chart crate behind the timeseries viewer (spec §8).
//!
//! `core` is window-free geometry over slices; `model` is the immutable
//! input a tile builds per delivery; `element` paints it through
//! gpui-component's `Plot` trait. Nothing here knows a series, a source
//! or the shell — the rem scale is a parameter.

pub mod core;
pub mod element;
pub mod model;

pub use core::axis::{Axis, AxisMode, Pane, Side};
pub use core::view::View;
pub use element::ChartElement;
pub use model::{ChartModel, ChartSlot};
```

(`element` and `model` are created in Task 7; until then `lib.rs` declares only `core` — add the other two lines in Task 7.)

- [ ] **Step 2: `core/mod.rs` — geometry types and the design constants**

```rust
//! Window-free geometry (spec §8.1). Everything is `Copy` or borrows;
//! nothing allocates except into a caller-owned buffer.

pub mod axis;
pub mod decimate;
pub mod layout;
pub mod palette;
pub mod scale;
pub mod time;
pub mod view;

/// The rem every design-pixel constant below is authored at — the value
/// of `geode_shell::shell::scale::DESIGN_REM` (`FontSize::Medium`),
/// duplicated because this crate must not depend on the shell.
pub const DESIGN_REM: f32 = 12.0;

/// Spec §8 constants, in design pixels at [`DESIGN_REM`].
pub const DENSITY_STRIP: f32 = 80.0;
pub const TICK_GAP: f32 = 64.0;
pub const DASH: f32 = 4.0;
pub const GAP: f32 = 3.0;
pub const AXIS_WIDTH: f32 = 44.0;
pub const PANE_GAP: f32 = 6.0;
/// The shared x-axis strip under the lowest pane — gpui-component's own
/// `AXIS_GAP`, so our axis text sits where the component's charts put it.
pub const X_AXIS_HEIGHT: f32 = 18.0;
/// Minimum vertical distance between two y ticks.
pub const Y_TICK_GAP: f32 = 40.0;

/// A design length resolved for the window's rem.
pub fn design_px(px_at_design_rem: f32, rem_px: f32) -> f32 {
    px_at_design_rem * rem_px / DESIGN_REM
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
    /// A polyline break: the decimator emits one where a `NaN` value
    /// ends a segment; the path builder starts a new subpath after it.
    pub const BREAK: Point = Point { x: f32::NAN, y: f32::NAN };
    pub fn is_break(&self) -> bool {
        self.x.is_nan()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn design_rem_mirrors_the_shells_medium_rem() {
        assert_eq!(DESIGN_REM, 12.0);
        assert_eq!(design_px(44.0, 12.0), 44.0, "identity at the design rem");
        assert_eq!(design_px(12.0, 14.0), 14.0, "scales with the rem");
    }
}
```

(Create empty `axis.rs`, `decimate.rs`, `layout.rs`, `palette.rs`, `time.rs`, `view.rs` files with a one-line `//!` doc so the crate compiles; each later task fills its own.)

- [ ] **Step 3: Write the failing `LinearScale` tests**

`src/core/scale.rs`:

```rust
//! Value ↔ pixel on a y axis, with 1-2-5 nice ticks (spec §8.1).

/// `lo` maps to `bottom`, `hi` to `top` (pixel y grows downward).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinearScale {
    pub lo: f64,
    pub hi: f64,
    pub top: f32,
    pub bottom: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_domain_onto_the_range_bottom_up() {
        let s = LinearScale::new((0.0, 100.0), 10.0, 110.0);
        assert_eq!(s.y(0.0), 110.0);
        assert_eq!(s.y(100.0), 10.0);
        assert_eq!(s.y(50.0), 60.0);
        assert!((s.value(60.0) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn a_degenerate_domain_is_padded_so_a_flat_series_still_paints() {
        let s = LinearScale::new((5.0, 5.0), 0.0, 100.0);
        assert!(s.lo < 5.0 && s.hi > 5.0);
        assert_eq!(s.y(5.0), 50.0);
        let s = LinearScale::new((f64::NAN, 1.0), 0.0, 100.0);
        assert!(s.lo.is_finite() && s.hi.is_finite() && s.lo < s.hi);
    }

    #[test]
    fn ticks_step_by_one_two_five_and_stay_inside_the_domain() {
        let s = LinearScale::new((0.0, 100.0), 0.0, 200.0);
        let mut out = Vec::new();
        s.ticks(5, &mut out);
        assert_eq!(out, vec![0.0, 20.0, 40.0, 60.0, 80.0, 100.0]);
        let s = LinearScale::new((0.13, 0.87), 0.0, 200.0);
        s.ticks(4, &mut out);
        assert_eq!(out, vec![0.2, 0.4, 0.6, 0.8]);
        for w in out.windows(2) {
            assert!(w[1] > w[0]);
        }
        assert_eq!(LinearScale::nice_step(100.0, 5), 20.0);
        assert_eq!(LinearScale::nice_step(100.0, 3), 50.0);
        assert_eq!(LinearScale::nice_step(0.74, 4), 0.2);
        assert_eq!(LinearScale::nice_step(7.0, 7), 1.0);
    }

    #[test]
    fn ticks_with_a_zero_hint_are_empty_not_a_division_by_zero() {
        let s = LinearScale::new((0.0, 1.0), 0.0, 10.0);
        let mut out = vec![1.0];
        s.ticks(0, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn tick_labels_carry_the_steps_decimals() {
        assert_eq!(fmt_tick(20.0, 20.0), "20");
        assert_eq!(fmt_tick(0.6, 0.2), "0.6");
        assert_eq!(fmt_tick(1250.0, 250.0), "1250");
        assert_eq!(fmt_tick(0.05, 0.05), "0.05");
        assert_eq!(fmt_tick(-0.5, 0.5), "-0.5");
    }

    #[test]
    fn readout_values_take_more_decimals_as_they_shrink() {
        assert_eq!(fmt_value(f64::NAN), "—");
        assert_eq!(fmt_value(4512.3456), "4512.35");
        assert_eq!(fmt_value(12.3456789), "12.3457");
        assert_eq!(fmt_value(0.123456789), "0.123457");
        assert_eq!(fmt_value(-0.5), "-0.500000");
    }

    #[test]
    fn axis_domain_pads_the_finite_extremes_and_ignores_nan() {
        let d = axis_domain([f64::NAN, 10.0, 30.0, f64::NAN].iter().copied()).unwrap();
        assert!((d.0 - 9.0).abs() < 1e-9, "{d:?}");
        assert!((d.1 - 31.0).abs() < 1e-9, "{d:?}");
        assert_eq!(axis_domain([f64::NAN].iter().copied()), None);
        assert_eq!(axis_domain(std::iter::empty()), None);
        let flat = axis_domain([7.0, 7.0].iter().copied()).unwrap();
        assert!(flat.0 < 7.0 && flat.1 > 7.0);
    }
}
```

- [ ] **Step 4: Run the tests to see them fail**

Run: `cargo test -p geode-chart scale`
Expected: compile errors (`new`, `y`, `ticks`, `fmt_tick`… missing).

- [ ] **Step 5: Implement**

Append to `src/core/scale.rs` above the tests:

```rust
/// Padding either side of a data domain, as a fraction of its span.
pub const DOMAIN_PAD: f64 = 0.05;

impl LinearScale {
    /// A non-finite or empty domain is made a unit span around its
    /// finite end (or zero) so a flat series still paints mid-pane.
    pub fn new(domain: (f64, f64), top: f32, bottom: f32) -> Self {
        let (mut lo, mut hi) = domain;
        if !lo.is_finite() {
            lo = if hi.is_finite() { hi } else { 0.0 };
        }
        if !hi.is_finite() {
            hi = lo;
        }
        if lo > hi {
            std::mem::swap(&mut lo, &mut hi);
        }
        if hi - lo <= 0.0 {
            lo -= 1.0;
            hi += 1.0;
        }
        Self { lo, hi, top, bottom }
    }

    pub fn y(&self, value: f64) -> f32 {
        let t = ((value - self.lo) / (self.hi - self.lo)) as f32;
        self.bottom - t * (self.bottom - self.top)
    }

    pub fn value(&self, y: f32) -> f64 {
        let t = ((self.bottom - y) / (self.bottom - self.top)) as f64;
        self.lo + t * (self.hi - self.lo)
    }

    /// The 1-2-5 step that gives about `count` ticks over `span`.
    pub fn nice_step(span: f64, count: usize) -> f64 {
        let raw = span / count.max(1) as f64;
        let magnitude = 10f64.powf(raw.log10().floor());
        let residual = raw / magnitude;
        let factor = if residual <= 1.0 {
            1.0
        } else if residual <= 2.0 {
            2.0
        } else if residual <= 5.0 {
            5.0
        } else {
            10.0
        };
        factor * magnitude
    }

    /// Nice ticks inside `[lo, hi]`, about `count_hint` of them, into
    /// `out` (cleared first). A zero hint yields none.
    pub fn ticks(&self, count_hint: usize, out: &mut Vec<f64>) {
        out.clear();
        if count_hint == 0 {
            return;
        }
        let step = Self::nice_step(self.hi - self.lo, count_hint);
        if !(step > 0.0) || !step.is_finite() {
            return;
        }
        let first = (self.lo / step).ceil();
        let last = (self.hi / step).floor();
        let mut k = first;
        while k <= last {
            // Round to the step's own decimals so 0.1 * 3 reads 0.3.
            let v = (k * step * 1e9).round() / 1e9;
            out.push(v);
            k += 1.0;
        }
    }

    pub fn step_for(&self, count_hint: usize) -> f64 {
        Self::nice_step(self.hi - self.lo, count_hint.max(1))
    }
}

/// A tick label with the decimals its step needs and no more.
pub fn fmt_tick(value: f64, step: f64) -> String {
    let decimals = if step >= 1.0 || step <= 0.0 || !step.is_finite() {
        0
    } else {
        (-step.log10().floor()) as usize
    };
    format!("{value:.decimals$}")
}

/// A readout value: 2 decimals from 100 up, 4 from 1 up, 6 below.
pub fn fmt_value(value: f64) -> String {
    if value.is_nan() {
        return "—".to_string();
    }
    let a = value.abs();
    if a >= 100.0 {
        format!("{value:.2}")
    } else if a >= 1.0 {
        format!("{value:.4}")
    } else {
        format!("{value:.6}")
    }
}

/// The finite min/max of `values`, padded by [`DOMAIN_PAD`] of the span
/// (a unit either side when flat); `None` when nothing is finite.
pub fn axis_domain(values: impl Iterator<Item = f64>) -> Option<(f64, f64)> {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for v in values {
        if v.is_finite() {
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    if lo > hi {
        return None;
    }
    let pad = if hi > lo { (hi - lo) * DOMAIN_PAD } else { 1.0 };
    Some((lo - pad, hi + pad))
}
```

- [ ] **Step 6: Run the tests, format, lint**

Run: `cargo test -p geode-chart && cargo fmt --check && cargo clippy -p geode-chart --all-targets -- -D warnings`
Expected: all green (the bench and example files do not exist yet — create `benches/decimate.rs` and `examples/chart.rs` as `fn main() {}` placeholders so `--all-targets` compiles; Task 8 replaces them).

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/geode-chart
git commit -m "chart: the crate, geometry types, design constants and LinearScale"
```

---

### Task 2: `Axis`, `View`, `TimeScale` and `Crosshair`

**Files:**
- Modify: `crates/geode-chart/src/core/axis.rs`, `crates/geode-chart/src/core/view.rs`, `crates/geode-chart/src/core/time.rs`

**Interfaces:**
- Consumes: `core::Rect`.
- Produces: `axis::{Axis, Pane, Side, AxisMode}`; `view::{View, MIN_SPAN}`; `time::{TimeScale, Crosshair}` (`full`, `x_of`, `visible`, `index_at`). Task 3 adds ticks to `time.rs`.

- [ ] **Step 1: `axis.rs` — the vocabulary Part 4 will import (spec §9.2, ruling 12)**

```rust
//! Where a slot paints (ruling 12): four y axes over two panes.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Axis {
    #[default]
    Left,
    Right,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pane {
    Upper,
    Lower,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum AxisMode {
    #[default]
    Session,
    Continuous,
}

impl Axis {
    pub const ALL: [Axis; 4] = [Axis::Left, Axis::Right, Axis::BottomLeft, Axis::BottomRight];

    pub fn pane(self) -> Pane {
        match self {
            Axis::Left | Axis::Right => Pane::Upper,
            Axis::BottomLeft | Axis::BottomRight => Pane::Lower,
        }
    }
    pub fn side(self) -> Side {
        match self {
            Axis::Left | Axis::BottomLeft => Side::Left,
            Axis::Right | Axis::BottomRight => Side::Right,
        }
    }
    /// `left → right → bottomleft → bottomright → left` (the `y` key).
    pub fn next(self) -> Axis {
        let i = Axis::ALL.iter().position(|a| *a == self).unwrap();
        Axis::ALL[(i + 1) % 4]
    }
    pub fn prev(self) -> Axis {
        let i = Axis::ALL.iter().position(|a| *a == self).unwrap();
        Axis::ALL[(i + 3) % 4]
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Axis::Left => "left",
            Axis::Right => "right",
            Axis::BottomLeft => "bottomleft",
            Axis::BottomRight => "bottomright",
        }
    }
    /// The chip letter(s) (spec §9.3).
    pub fn letter(self) -> &'static str {
        match self {
            Axis::Left => "L",
            Axis::Right => "R",
            Axis::BottomLeft => "BL",
            Axis::BottomRight => "BR",
        }
    }
    pub fn parse(s: &str) -> Option<Axis> {
        Axis::ALL.into_iter().find(|a| a.as_str() == s)
    }
}

impl AxisMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AxisMode::Session => "session",
            AxisMode::Continuous => "time",
        }
    }
    pub fn parse(s: &str) -> Option<AxisMode> {
        match s {
            "session" => Some(AxisMode::Session),
            "time" => Some(AxisMode::Continuous),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axes_cycle_and_round_trip() {
        assert_eq!(Axis::Left.next(), Axis::Right);
        assert_eq!(Axis::BottomRight.next(), Axis::Left);
        assert_eq!(Axis::Left.prev(), Axis::BottomRight);
        for a in Axis::ALL {
            assert_eq!(Axis::parse(a.as_str()), Some(a));
        }
        assert_eq!(Axis::BottomLeft.pane(), Pane::Lower);
        assert_eq!(Axis::BottomLeft.side(), Side::Left);
        assert_eq!(Axis::Right.pane(), Pane::Upper);
        assert_eq!(Axis::BottomRight.letter(), "BR");
        assert_eq!(AxisMode::parse("time"), Some(AxisMode::Continuous));
        assert_eq!(AxisMode::parse("wall"), None);
    }
}
```

- [ ] **Step 2: `view.rs` — failing tests**

```rust
//! The visible window along x (spec §8.1): an index window under
//! `Session` (bucket `i` occupies `[i, i+1)`), a micros window under
//! `Continuous`. Unit-agnostic: every method takes the loaded `full`
//! range in the same units and clamps to it.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub lo: f64,
    pub hi: f64,
}

/// Narrowest window a zoom can reach (units): two buckets, or two micros.
pub const MIN_SPAN: f64 = 2.0;

#[cfg(test)]
mod tests {
    use super::*;
    const FULL: (f64, f64) = (0.0, 100.0);

    #[test]
    fn full_covers_the_range_and_reset_returns_to_it() {
        let mut v = View::full(FULL);
        assert_eq!((v.lo, v.hi), FULL);
        v.zoom(2.0, 0.5, FULL);
        assert_eq!((v.lo, v.hi), (25.0, 75.0));
        v.reset(FULL);
        assert_eq!((v.lo, v.hi), FULL);
    }

    #[test]
    fn a_pan_past_the_end_clamps() {
        let mut v = View { lo: 40.0, hi: 60.0 };
        v.pan(0.5, FULL);
        assert_eq!((v.lo, v.hi), (50.0, 70.0));
        v.pan(5.0, FULL);
        assert_eq!((v.lo, v.hi), (80.0, 100.0), "keeps its width, stops at the end");
        v.pan(-9.0, FULL);
        assert_eq!((v.lo, v.hi), (0.0, 20.0));
    }

    #[test]
    fn a_zoom_about_a_point_keeps_that_point_still_and_never_narrows_below_min_span() {
        let mut v = View { lo: 0.0, hi: 100.0 };
        v.zoom(2.0, 0.25, FULL);
        assert_eq!((v.lo, v.hi), (12.5, 62.5));
        for _ in 0..40 {
            v.zoom(2.0, 0.5, FULL);
        }
        assert!((v.hi - v.lo - MIN_SPAN).abs() < 1e-9, "{v:?}");
        v.zoom(0.001, 0.5, FULL);
        assert_eq!((v.lo, v.hi), FULL, "a zoom out never exceeds the range");
    }

    #[test]
    fn a_range_narrower_than_min_span_is_shown_whole() {
        let full = (0.0, 1.0);
        let mut v = View::full(full);
        v.zoom(4.0, 0.5, full);
        assert_eq!((v.lo, v.hi), full);
    }

    #[test]
    fn jumps_keep_the_width() {
        let mut v = View { lo: 40.0, hi: 60.0 };
        v.jump_end(FULL);
        assert_eq!((v.lo, v.hi), (80.0, 100.0));
        v.jump_start(FULL);
        assert_eq!((v.lo, v.hi), (0.0, 20.0));
    }

    #[test]
    fn the_key_changes_with_the_window() {
        let a = View { lo: 0.0, hi: 1.0 }.key();
        let b = View { lo: 0.0, hi: 2.0 }.key();
        assert_ne!(a, b);
        assert_eq!(a, View { lo: 0.0, hi: 1.0 }.key());
    }
}
```

- [ ] **Step 3: Implement `View`**

```rust
impl View {
    pub fn full(full: (f64, f64)) -> Self {
        Self { lo: full.0, hi: full.1 }
    }
    pub fn span(&self) -> f64 {
        self.hi - self.lo
    }
    pub fn reset(&mut self, full: (f64, f64)) {
        *self = Self::full(full);
    }
    /// Shift by `fraction` of the current width (negative = left).
    pub fn pan(&mut self, fraction: f64, full: (f64, f64)) {
        let d = self.span() * fraction;
        self.lo += d;
        self.hi += d;
        self.clamp(full);
    }
    /// Narrow by `factor` (> 1 zooms in) keeping the point at `about`
    /// (0 = left edge, 1 = right edge) where it is.
    pub fn zoom(&mut self, factor: f64, about: f64, full: (f64, f64)) {
        if !(factor > 0.0) || !factor.is_finite() {
            return;
        }
        let about = about.clamp(0.0, 1.0);
        let pivot = self.lo + self.span() * about;
        let width = (self.span() / factor).max(MIN_SPAN);
        self.lo = pivot - width * about;
        self.hi = self.lo + width;
        self.clamp(full);
    }
    pub fn jump_start(&mut self, full: (f64, f64)) {
        let w = self.span();
        self.lo = full.0;
        self.hi = full.0 + w;
        self.clamp(full);
    }
    pub fn jump_end(&mut self, full: (f64, f64)) {
        let w = self.span();
        self.hi = full.1;
        self.lo = full.1 - w;
        self.clamp(full);
    }
    /// Never wider than `full`, never outside it, never narrower than
    /// `MIN_SPAN` unless `full` itself is.
    fn clamp(&mut self, full: (f64, f64)) {
        let full_w = (full.1 - full.0).max(0.0);
        let mut w = self.span().max(MIN_SPAN.min(full_w)).min(full_w);
        if !w.is_finite() {
            w = full_w;
        }
        if self.lo < full.0 {
            self.lo = full.0;
        }
        if self.lo + w > full.1 {
            self.lo = full.1 - w;
        }
        self.hi = self.lo + w;
    }
    /// A hashable identity for the path cache key.
    pub fn key(&self) -> (u64, u64) {
        (self.lo.to_bits(), self.hi.to_bits())
    }
}
```

- [ ] **Step 4: `time.rs` — failing tests for the scale and the crosshair**

```rust
//! The x axis (spec §8.2): `Session` maps bucket INDEX to x so a span
//! with no bucket has no width; `Continuous` maps wall-clock micros.
//! Bucket `i` occupies `[i, i+1)` (session) or `[b_i, b_i + step)`
//! (continuous); its centre is where the point paints.

use super::Rect;
use super::view::View;

#[derive(Clone, Copy, Debug)]
pub enum TimeScale<'a> {
    Session { buckets: &'a [i64] },
    Continuous { buckets: &'a [i64], step_us: i64 },
}

/// The nearest bucket to a cursor (spec §8.1).
pub struct Crosshair;

#[cfg(test)]
mod tests {
    use super::*;
    const PLOT: Rect = Rect::new(100.0, 0.0, 1000.0, 100.0);
    const DAY: i64 = 86_400_000_000;
    // Mon..Fri, then Mon (a weekend gap).
    const BUCKETS: [i64; 6] = [0, DAY, 2 * DAY, 3 * DAY, 4 * DAY, 7 * DAY];

    #[test]
    fn a_session_scale_gives_every_bucket_the_same_width_whatever_its_gap() {
        let s = TimeScale::Session { buckets: &BUCKETS };
        assert_eq!(s.full(), (0.0, 6.0));
        let v = View::full(s.full());
        let xs: Vec<f32> = (0..6).map(|i| s.x_of(i, v, PLOT)).collect();
        for w in xs.windows(2) {
            assert!((w[1] - w[0] - 1000.0 / 6.0).abs() < 1e-3, "{xs:?}");
        }
        assert!((xs[0] - (100.0 + 1000.0 / 12.0)).abs() < 1e-3, "centred in its slot");
    }

    #[test]
    fn a_continuous_scale_leaves_the_weekend_its_width() {
        let s = TimeScale::Continuous { buckets: &BUCKETS, step_us: DAY };
        assert_eq!(s.full(), (0.0, 8.0 * DAY as f64));
        let v = View::full(s.full());
        let fri = s.x_of(4, v, PLOT);
        let mon = s.x_of(5, v, PLOT);
        let thu = s.x_of(3, v, PLOT);
        assert!((mon - fri) > 2.5 * (fri - thu), "the gap is three days wide");
    }

    #[test]
    fn visible_is_the_index_window_the_view_intersects() {
        let s = TimeScale::Session { buckets: &BUCKETS };
        assert_eq!(s.visible(View { lo: 0.0, hi: 6.0 }), (0, 6));
        assert_eq!(s.visible(View { lo: 1.5, hi: 3.5 }), (1, 4));
        assert_eq!(s.visible(View { lo: 2.0, hi: 3.0 }), (2, 3));
        let c = TimeScale::Continuous { buckets: &BUCKETS, step_us: DAY };
        assert_eq!(c.visible(View { lo: 0.5 * DAY as f64, hi: 5.0 * DAY as f64 }), (0, 5));
        assert_eq!(c.visible(View { lo: 5.5 * DAY as f64, hi: 6.5 * DAY as f64 }), (5, 5), "the gap holds no bucket");
        let e = TimeScale::Session { buckets: &[] };
        assert_eq!(e.visible(View { lo: 0.0, hi: 1.0 }), (0, 0));
    }

    #[test]
    fn the_crosshair_picks_the_nearest_bucket() {
        let s = TimeScale::Session { buckets: &BUCKETS };
        let v = View::full(s.full());
        let x2 = s.x_of(2, v, PLOT);
        let x3 = s.x_of(3, v, PLOT);
        assert_eq!(Crosshair::at(x2 + 1.0, &s, v, PLOT), Some(2));
        assert_eq!(Crosshair::at(x3 - 1.0, &s, v, PLOT), Some(3));
        assert_eq!(Crosshair::at((x2 + x3) / 2.0 + 0.5, &s, v, PLOT), Some(3));
        assert_eq!(Crosshair::at(PLOT.x - 50.0, &s, v, PLOT), Some(0), "left of the plot snaps to the first");
        assert_eq!(Crosshair::at(PLOT.right() + 50.0, &s, v, PLOT), Some(5));
        let zoomed = View { lo: 2.0, hi: 4.0 };
        assert_eq!(Crosshair::at(PLOT.x + 1.0, &s, zoomed, PLOT), Some(2), "only visible buckets");
        assert_eq!(Crosshair::at(PLOT.right() - 1.0, &s, zoomed, PLOT), Some(3));
        assert_eq!(Crosshair::at(500.0, &TimeScale::Session { buckets: &[] }, v, PLOT), None);
    }
}
```

- [ ] **Step 5: Implement the scale and the crosshair**

```rust
impl<'a> TimeScale<'a> {
    pub fn buckets(&self) -> &'a [i64] {
        match self {
            TimeScale::Session { buckets } | TimeScale::Continuous { buckets, .. } => buckets,
        }
    }

    /// The loaded range in this scale's units: `(0, n)` for session,
    /// `(first, last + step)` for continuous; `(0, 0)` when empty.
    pub fn full(&self) -> (f64, f64) {
        match *self {
            TimeScale::Session { buckets } => (0.0, buckets.len() as f64),
            TimeScale::Continuous { buckets, step_us } => match (buckets.first(), buckets.last()) {
                (Some(&f), Some(&l)) => (f as f64, (l + step_us) as f64),
                _ => (0.0, 0.0),
            },
        }
    }

    /// Bucket `index`'s centre in its own units.
    fn centre(&self, index: usize) -> f64 {
        match *self {
            TimeScale::Session { .. } => index as f64 + 0.5,
            TimeScale::Continuous { buckets, step_us } => buckets[index] as f64 + step_us as f64 / 2.0,
        }
    }

    fn to_x(&self, u: f64, view: View, plot: Rect) -> f32 {
        let span = view.span();
        if !(span > 0.0) {
            return plot.x;
        }
        plot.x + ((u - view.lo) / span) as f32 * plot.w
    }

    /// Bucket `index`'s x on the plot (its centre).
    pub fn x_of(&self, index: usize, view: View, plot: Rect) -> f32 {
        self.to_x(self.centre(index), view, plot)
    }

    /// `[start, end)` of the buckets whose slot intersects the view.
    pub fn visible(&self, view: View) -> (usize, usize) {
        let b = self.buckets();
        if b.is_empty() {
            return (0, 0);
        }
        match *self {
            TimeScale::Session { .. } => {
                let start = view.lo.floor().max(0.0) as usize;
                let end = (view.hi.ceil().max(0.0) as usize).min(b.len());
                (start.min(end), end)
            }
            TimeScale::Continuous { buckets, step_us } => {
                // first bucket whose slot end is after view.lo
                let start = buckets.partition_point(|&t| ((t + step_us) as f64) <= view.lo);
                let end = buckets.partition_point(|&t| (t as f64) < view.hi);
                (start.min(end), end)
            }
        }
    }
}

impl Crosshair {
    /// The visible bucket whose centre is nearest `cursor_x`.
    pub fn at(cursor_x: f32, scale: &TimeScale, view: View, plot: Rect) -> Option<usize> {
        let (start, end) = scale.visible(view);
        if start >= end {
            return None;
        }
        // x is monotone in index: binary search the first centre at or
        // past the cursor, then compare with its predecessor.
        let mut lo = start;
        let mut hi = end;
        while lo < hi {
            let mid = (lo + hi) / 2;
            if scale.x_of(mid, view, plot) < cursor_x {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == start {
            return Some(start);
        }
        if lo == end {
            return Some(end - 1);
        }
        let before = (scale.x_of(lo - 1, view, plot) - cursor_x).abs();
        let after = (scale.x_of(lo, view, plot) - cursor_x).abs();
        Some(if after < before { lo } else { lo - 1 })
    }
}
```

- [ ] **Step 6: Run, format, lint, commit**

Run: `cargo test -p geode-chart && cargo fmt --check && cargo clippy -p geode-chart --all-targets -- -D warnings`

```bash
git add crates/geode-chart
git commit -m "chart: Axis vocabulary, the View window and the session/continuous TimeScale with a nearest-bucket crosshair"
```

---

### Task 3: Time ticks — the unit chooser and labels

**Files:**
- Modify: `crates/geode-chart/src/core/time.rs`

**Interfaces:**
- Consumes: `TimeScale`, `View`, `Rect`.
- Produces: `time::{Unit, Tick, ticks}`: `ticks(scale, view, plot, tick_gap_px, offset_secs, out: &mut Vec<Tick>) -> Option<Unit>`.

Design (spec §8.2, with two additions this plan makes explicit): a tick sits on the first visible bucket at which the unit's value changes from the previous bucket's (the first bucket of all is always a candidate); the unit is chosen as the FINEST that has at least two candidates whose minimum gap is ≥ `tick_gap_px`. If no unit fits, the COARSEST unit with at least two candidates is THINNED (every `k`-th candidate, `k = ceil(tick_gap / min_gap)`) so a day of minute bars still shows every second or third hour rather than one "14 Sep". If no unit has two candidates (one visible bucket), one tick at that bucket labelled by day. Under `Continuous`, candidates are the unit's boundaries within the view (a unit whose boundaries would number more than 4,096 is skipped: it cannot fit at any width the chart paints). Times are read at `offset_secs` from UTC — the trader's local clock (CLAUDE.md's displayed-time ruling) — passed in so the core stays deterministic. Labels: `Year` → `2026`, `Month` → `Sep 26`, `Day` → `14 Sep`, `Hour`/`Minute` → `10:00`.

- [ ] **Step 1: Failing tests**

Add to `time.rs`'s test module (`use chrono::{TimeZone, Utc};`):

```rust
    fn us(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap().timestamp_micros()
    }

    /// Weekdays only, 2026-01-05 (a Monday) onward, `days` of them.
    fn weekdays(days: usize) -> Vec<i64> {
        let mut out = Vec::new();
        let mut day = chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        while out.len() < days {
            if day.weekday().num_days_from_monday() < 5 {
                out.push(day.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_micros());
            }
            day = day.succ_opt().unwrap();
        }
        out
    }

    #[test]
    fn session_ticks_fall_where_the_month_changes() {
        let b = weekdays(250); // ~a year of sessions
        let s = TimeScale::Session { buckets: &b };
        let v = View::full(s.full());
        let plot = Rect::new(0.0, 0.0, 1200.0, 100.0);
        let mut out = Vec::new();
        let unit = ticks(&s, v, plot, 64.0, 0, &mut out);
        assert_eq!(unit, Some(Unit::Month));
        assert_eq!(out.len(), 12, "{:?}", out.iter().map(|t| &t.label).collect::<Vec<_>>());
        assert_eq!(out[0].label, "Jan 26");
        assert_eq!(out[1].label, "Feb 26");
        // the tick is the FIRST bucket of the month, not the last of the old one
        let first_feb = b.iter().position(|&t| t >= us(2026, 2, 1, 0, 0)).unwrap();
        assert!((out[1].x - s.x_of(first_feb, v, plot)).abs() < 1e-3);
    }

    #[test]
    fn ticks_respect_the_gap() {
        let b = weekdays(250);
        let s = TimeScale::Session { buckets: &b };
        let v = View::full(s.full());
        let mut out = Vec::new();
        // 300 px: a month tick every 25 px cannot fit at 64 — the chooser
        // falls through to the coarsest unit with two candidates (Month,
        // since Year has one) and thins it.
        let unit = ticks(&s, v, Rect::new(0.0, 0.0, 300.0, 100.0), 64.0, 0, &mut out);
        assert_eq!(unit, Some(Unit::Month));
        assert!(out.len() >= 3 && out.len() <= 5, "{}", out.len());
        for w in out.windows(2) {
            assert!(w[1].x - w[0].x >= 64.0 - 1e-3, "{:?}", (w[0].x, w[1].x));
        }
        // a wide plot fits days
        let unit = ticks(&s, View { lo: 0.0, hi: 10.0 }, Rect::new(0.0, 0.0, 1200.0, 100.0), 64.0, 0, &mut out);
        assert_eq!(unit, Some(Unit::Day));
        assert_eq!(out[0].label, "5 Jan");
        assert_eq!(out.len(), 10);
    }

    #[test]
    fn a_day_of_minute_bars_shows_thinned_hours() {
        let start = us(2026, 1, 5, 14, 30);
        let b: Vec<i64> = (0..600).map(|i| start + i * 60_000_000).collect();
        let s = TimeScale::Session { buckets: &b };
        let v = View::full(s.full());
        let mut out = Vec::new();
        let unit = ticks(&s, v, Rect::new(0.0, 0.0, 400.0, 100.0), 64.0, 0, &mut out);
        assert_eq!(unit, Some(Unit::Hour));
        assert!(out.len() >= 3, "{}", out.len());
        assert_eq!(out[0].label, "14:30", "the first bucket is always a candidate");
        assert_eq!(out[1].label, "16:00", "every second hour at 400 px");
    }

    #[test]
    fn labels_read_at_the_given_offset() {
        let b = [us(2026, 1, 5, 23, 30), us(2026, 1, 6, 0, 30)];
        let s = TimeScale::Session { buckets: &b };
        let v = View::full(s.full());
        let mut out = Vec::new();
        ticks(&s, v, Rect::new(0.0, 0.0, 400.0, 100.0), 64.0, 0, &mut out);
        assert_eq!(out.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(), ["23:30", "00:30"]);
        // at UTC+1 both buckets sit in the same day: hours still, one hour later
        ticks(&s, v, Rect::new(0.0, 0.0, 400.0, 100.0), 64.0, 3600, &mut out);
        assert_eq!(out.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(), ["00:30", "01:30"]);
    }

    #[test]
    fn continuous_ticks_sit_on_unit_boundaries_not_buckets() {
        let b = weekdays(10);
        let s = TimeScale::Continuous { buckets: &b, step_us: 86_400_000_000 };
        let v = View::full(s.full());
        let plot = Rect::new(0.0, 0.0, 1400.0, 100.0);
        let mut out = Vec::new();
        assert_eq!(ticks(&s, v, plot, 64.0, 0, &mut out), Some(Unit::Day));
        assert_eq!(out.len(), 14, "every calendar day in the span, weekend included");
        let sat = us(2026, 1, 10, 0, 0) as f64;
        assert!(out.iter().any(|t| (t.x - s_to_x(&s, sat, v, plot)).abs() < 1e-3), "a weekend day has a tick");
    }

    fn s_to_x(s: &TimeScale, u: f64, v: View, plot: Rect) -> f32 {
        plot.x + ((u - v.lo) / v.span()) as f32 * plot.w
    }

    #[test]
    fn one_visible_bucket_gets_one_day_tick() {
        let b = [us(2026, 1, 5, 14, 30)];
        let s = TimeScale::Session { buckets: &b };
        let mut out = Vec::new();
        assert_eq!(ticks(&s, View::full(s.full()), Rect::new(0.0, 0.0, 400.0, 100.0), 64.0, 0, &mut out), Some(Unit::Day));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label, "5 Jan");
        assert_eq!(ticks(&TimeScale::Session { buckets: &[] }, View { lo: 0.0, hi: 1.0 }, Rect::new(0.0, 0.0, 400.0, 100.0), 64.0, 0, &mut out), None);
        assert!(out.is_empty());
    }

    #[test]
    fn ticks_strictly_increase_in_x() {
        let b = weekdays(400);
        let s = TimeScale::Session { buckets: &b };
        let mut out = Vec::new();
        for (lo, hi, w) in [(0.0, 400.0, 900.0), (10.0, 30.0, 200.0), (100.0, 101.0, 50.0), (0.0, 400.0, 30.0)] {
            ticks(&s, View { lo, hi }, Rect::new(0.0, 0.0, w, 100.0), 64.0, 0, &mut out);
            for p in out.windows(2) {
                assert!(p[1].x > p[0].x, "{lo}..{hi} at {w}: {:?}", (p[0].x, p[1].x));
            }
        }
    }
```

- [ ] **Step 2: Implement**

```rust
use chrono::{DateTime, Datelike, FixedOffset, TimeZone, Timelike, Utc};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Unit {
    Minute,
    Hour,
    Day,
    Month,
    Year,
}

impl Unit {
    /// Finest first.
    pub const ALL: [Unit; 5] = [Unit::Minute, Unit::Hour, Unit::Day, Unit::Month, Unit::Year];

    /// The value that changes at this unit's boundary.
    fn value(self, t: &DateTime<FixedOffset>) -> (i32, u32, u32, u32, u32) {
        match self {
            Unit::Minute => (t.year(), t.month(), t.day(), t.hour(), t.minute()),
            Unit::Hour => (t.year(), t.month(), t.day(), t.hour(), 0),
            Unit::Day => (t.year(), t.month(), t.day(), 0, 0),
            Unit::Month => (t.year(), t.month(), 0, 0, 0),
            Unit::Year => (t.year(), 0, 0, 0, 0),
        }
    }

    pub fn label(self, t: &DateTime<FixedOffset>) -> String {
        match self {
            Unit::Year => t.format("%Y").to_string(),
            Unit::Month => t.format("%b %y").to_string(),
            Unit::Day => t.format("%-d %b").to_string(),
            Unit::Hour | Unit::Minute => t.format("%H:%M").to_string(),
        }
    }

    /// The boundary at or after `t` (this unit's next roll-over at or
    /// after `t`; `t` itself when on one).
    fn ceil(self, t: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
        let floored = self.floor(t);
        if floored == t { t } else { self.next(floored) }
    }

    fn floor(self, t: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
        let tz = *t.offset();
        let (y, mo, d, h, mi) = (t.year(), t.month(), t.day(), t.hour(), t.minute());
        let out = match self {
            Unit::Minute => tz.with_ymd_and_hms(y, mo, d, h, mi, 0),
            Unit::Hour => tz.with_ymd_and_hms(y, mo, d, h, 0, 0),
            Unit::Day => tz.with_ymd_and_hms(y, mo, d, 0, 0, 0),
            Unit::Month => tz.with_ymd_and_hms(y, mo, 1, 0, 0, 0),
            Unit::Year => tz.with_ymd_and_hms(y, 1, 1, 0, 0, 0),
        };
        out.single().unwrap_or(t)
    }

    fn next(self, t: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
        match self {
            Unit::Minute => t + chrono::Duration::minutes(1),
            Unit::Hour => t + chrono::Duration::hours(1),
            Unit::Day => t + chrono::Duration::days(1),
            Unit::Month => {
                let (y, m) = if t.month() == 12 { (t.year() + 1, 1) } else { (t.year(), t.month() + 1) };
                t.offset().with_ymd_and_hms(y, m, 1, 0, 0, 0).single().unwrap_or(t)
            }
            Unit::Year => t.offset().with_ymd_and_hms(t.year() + 1, 1, 1, 0, 0, 0).single().unwrap_or(t),
        }
    }

    fn approx_secs(self) -> f64 {
        match self {
            Unit::Minute => 60.0,
            Unit::Hour => 3_600.0,
            Unit::Day => 86_400.0,
            Unit::Month => 30.0 * 86_400.0,
            Unit::Year => 365.0 * 86_400.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tick {
    pub x: f32,
    pub label: String,
}

/// Most boundaries a continuous unit may have inside a view before it is
/// skipped as unfittable.
const MAX_CANDIDATES: usize = 4_096;

fn at(us: i64, offset: FixedOffset) -> DateTime<FixedOffset> {
    DateTime::<Utc>::from_timestamp_micros(us)
        .unwrap_or_default()
        .with_timezone(&offset)
}

/// Ticks for the view into `out` (cleared first); the unit chosen, or
/// `None` when nothing is visible.
pub fn ticks(
    scale: &TimeScale,
    view: View,
    plot: Rect,
    tick_gap_px: f32,
    offset_secs: i32,
    out: &mut Vec<Tick>,
) -> Option<Unit> {
    out.clear();
    let offset = FixedOffset::east_opt(offset_secs).unwrap_or_else(|| FixedOffset::east_opt(0).unwrap());
    let (start, end) = scale.visible(view);
    if start >= end {
        return None;
    }
    // candidates per unit: (x, label time), finest first
    let mut best_fit: Option<(Unit, Vec<(f32, DateTime<FixedOffset>)>)> = None;
    let mut coarsest_pair: Option<(Unit, Vec<(f32, DateTime<FixedOffset>)>)> = None;
    for unit in Unit::ALL {
        let cands = candidates(scale, view, plot, unit, offset, start, end);
        if cands.len() < 2 {
            continue;
        }
        let min_gap = cands.windows(2).map(|w| w[1].0 - w[0].0).fold(f32::INFINITY, f32::min);
        if min_gap >= tick_gap_px && best_fit.is_none() {
            best_fit = Some((unit, cands.clone()));
        }
        coarsest_pair = Some((unit, cands));
    }
    let (unit, cands, thin) = match (best_fit, coarsest_pair) {
        (Some((u, c)), _) => (u, c, 1usize),
        (None, Some((u, c))) => {
            let min_gap = c.windows(2).map(|w| w[1].0 - w[0].0).fold(f32::INFINITY, f32::min);
            let k = if min_gap > 0.0 { (tick_gap_px / min_gap).ceil().max(1.0) as usize } else { c.len() };
            (u, c, k)
        }
        (None, None) => {
            let t = at(scale.buckets()[start], offset);
            out.push(Tick { x: scale.x_of(start, view, plot), label: Unit::Day.label(&t) });
            return Some(Unit::Day);
        }
    };
    let mut last_x = f32::NEG_INFINITY;
    for (i, (x, t)) in cands.iter().enumerate() {
        if i % thin != 0 || *x <= last_x {
            continue;
        }
        out.push(Tick { x: *x, label: unit.label(t) });
        last_x = *x;
    }
    Some(unit)
}

fn candidates(
    scale: &TimeScale,
    view: View,
    plot: Rect,
    unit: Unit,
    offset: FixedOffset,
    start: usize,
    end: usize,
) -> Vec<(f32, DateTime<FixedOffset>)> {
    let b = scale.buckets();
    match *scale {
        TimeScale::Session { .. } => {
            let mut out = Vec::new();
            for i in start..end {
                let t = at(b[i], offset);
                let is_tick = i == 0 || unit.value(&t) != unit.value(&at(b[i - 1], offset));
                if is_tick {
                    out.push((scale.x_of(i, view, plot), t));
                }
            }
            out
        }
        TimeScale::Continuous { .. } => {
            let span_secs = view.span() / 1e6;
            if span_secs / unit.approx_secs() > MAX_CANDIDATES as f64 {
                return Vec::new();
            }
            let mut out = Vec::new();
            let mut t = unit.ceil(at(view.lo.max(0.0) as i64, offset));
            let hi = view.hi as i64;
            while t.timestamp_micros() < hi && out.len() <= MAX_CANDIDATES {
                let u = t.timestamp_micros() as f64;
                let x = plot.x + ((u - view.lo) / view.span()) as f32 * plot.w;
                out.push((x, t));
                t = unit.next(t);
            }
            out
        }
    }
}
```

Note the session comparison reads `b[i - 1]` even when `i - 1` is outside the view: the tick is where the unit CHANGES, and a view starting mid-month must not paint a tick on its first bucket unless the month really changed there. `i == 0` is the one unconditional candidate.

- [ ] **Step 3: Run, format, lint, commit**

Run: `cargo test -p geode-chart time && cargo fmt --check && cargo clippy -p geode-chart --all-targets -- -D warnings`

If `%-d` is rejected on Windows CI, replace it with `format!("{} {}", t.day(), t.format("%b"))` — chrono's `%-d` is a strftime padding flag chrono implements itself, so it should be portable; check the chrono docs in the registry (`chrono-0.4.*/src/format/strftime.rs`) rather than assume.

```bash
git add crates/geode-chart
git commit -m "chart: time ticks — the unit chooser, thinning, local-offset labels"
```

---

### Task 4: `Layout::solve`

**Files:**
- Modify: `crates/geode-chart/src/core/layout.rs`

**Interfaces:**
- Consumes: `Rect`, the design constants, `design_px`.
- Produces: `layout::{LayoutOptions, PaneRects, Layout}`; `Layout::solve(bounds: Rect, o: LayoutOptions) -> Layout`.

- [ ] **Step 1: Failing tests**

```rust
//! One or two panes with their axis rects, a density strip and one
//! shared x axis (spec §8.1, ruling 12).

use super::{AXIS_WIDTH, DENSITY_STRIP, PANE_GAP, Rect, X_AXIS_HEIGHT, design_px};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutOptions {
    pub upper_left: bool,
    pub upper_right: bool,
    pub lower_left: bool,
    pub lower_right: bool,
    pub density: bool,
    /// The upper pane's share of the height while a lower pane exists.
    pub split: f32,
    pub rem_px: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct PaneRects {
    pub plot: Rect,
    pub left_axis: Option<Rect>,
    pub right_axis: Option<Rect>,
    pub density: Option<Rect>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub upper: PaneRects,
    pub lower: Option<PaneRects>,
    pub x_axis: Rect,
}

pub const SPLIT_DEFAULT: f32 = 0.7;
pub const SPLIT_MIN: f32 = 0.2;
pub const SPLIT_MAX: f32 = 0.8;

#[cfg(test)]
mod tests {
    use super::*;
    const B: Rect = Rect::new(0.0, 0.0, 1000.0, 500.0);
    fn o() -> LayoutOptions {
        LayoutOptions { upper_left: true, upper_right: false, lower_left: false, lower_right: false, density: false, split: 0.7, rem_px: 12.0 }
    }

    #[test]
    fn an_axis_rect_exists_only_for_a_side_in_use() {
        let l = Layout::solve(B, o());
        assert!(l.upper.left_axis.is_some());
        assert!(l.upper.right_axis.is_none());
        assert_eq!(l.upper.plot.x, AXIS_WIDTH);
        assert_eq!(l.upper.plot.right(), 1000.0, "no right axis reserved");
        let l = Layout::solve(B, LayoutOptions { upper_left: false, upper_right: true, ..o() });
        assert!(l.upper.left_axis.is_none());
        assert_eq!(l.upper.plot.x, 0.0);
        assert_eq!(l.upper.right_axis.unwrap().x, 1000.0 - AXIS_WIDTH);
        let l = Layout::solve(B, LayoutOptions { upper_left: false, ..o() });
        assert_eq!(l.upper.plot, Rect::new(0.0, 0.0, 1000.0, 500.0 - X_AXIS_HEIGHT), "nothing reserved with no side in use");
    }

    #[test]
    fn the_density_strip_exists_only_when_asked_and_sits_before_the_right_axis() {
        let l = Layout::solve(B, LayoutOptions { density: true, upper_right: true, ..o() });
        let d = l.upper.density.unwrap();
        assert_eq!(d.w, DENSITY_STRIP);
        assert_eq!(d.right(), l.upper.right_axis.unwrap().x);
        assert_eq!(l.upper.plot.right(), d.x);
        assert!(Layout::solve(B, o()).upper.density.is_none());
    }

    #[test]
    fn the_lower_pane_exists_only_while_a_visible_slot_uses_a_bottom_axis() {
        assert!(Layout::solve(B, o()).lower.is_none());
        let l = Layout::solve(B, LayoutOptions { lower_right: true, ..o() });
        let lower = l.lower.unwrap();
        assert!(lower.right_axis.is_some() && lower.left_axis.is_none());
        assert_eq!(l.x_axis.y, lower.plot.bottom(), "the x axis sits under the LOWEST pane");
        assert_eq!(l.x_axis.h, X_AXIS_HEIGHT);
        let avail = 500.0 - X_AXIS_HEIGHT - PANE_GAP;
        assert!((l.upper.plot.h - avail * 0.7).abs() < 1e-3);
        assert!((lower.plot.h - avail * 0.3).abs() < 1e-3);
        assert_eq!(lower.plot.y, l.upper.plot.bottom() + PANE_GAP);
    }

    #[test]
    fn split_is_clamped() {
        let l = Layout::solve(B, LayoutOptions { lower_left: true, split: 0.05, ..o() });
        let avail = 500.0 - X_AXIS_HEIGHT - PANE_GAP;
        assert!((l.upper.plot.h - avail * SPLIT_MIN).abs() < 1e-3);
        let l = Layout::solve(B, LayoutOptions { lower_left: true, split: 0.99, ..o() });
        assert!((l.upper.plot.h - avail * SPLIT_MAX).abs() < 1e-3);
        let l = Layout::solve(B, LayoutOptions { lower_left: true, split: f32::NAN, ..o() });
        assert!((l.upper.plot.h - avail * SPLIT_DEFAULT).abs() < 1e-3, "NaN takes the default");
    }

    #[test]
    fn both_panes_share_one_x_mapping() {
        // the lower pane uses the right side only; the upper the left only —
        // both plots still start and end at the same x
        let l = Layout::solve(B, LayoutOptions { lower_right: true, density: true, ..o() });
        let lower = l.lower.unwrap();
        assert_eq!(l.upper.plot.x, lower.plot.x);
        assert_eq!(l.upper.plot.w, lower.plot.w);
        assert_eq!(l.upper.plot.x, AXIS_WIDTH);
        assert_eq!(l.upper.plot.right(), 1000.0 - AXIS_WIDTH - DENSITY_STRIP);
        assert!(l.upper.right_axis.is_none(), "the column is reserved, the rect is not painted");
        assert!(lower.left_axis.is_none());
        assert_eq!(l.x_axis.x, l.upper.plot.x);
        assert_eq!(l.x_axis.w, l.upper.plot.w);
    }

    #[test]
    fn lengths_follow_the_rem() {
        let l = Layout::solve(B, LayoutOptions { density: true, rem_px: 14.0, ..o() });
        assert!((l.upper.left_axis.unwrap().w - AXIS_WIDTH * 14.0 / 12.0).abs() < 1e-3);
        assert!((l.upper.density.unwrap().w - DENSITY_STRIP * 14.0 / 12.0).abs() < 1e-3);
        assert!((l.x_axis.h - X_AXIS_HEIGHT * 14.0 / 12.0).abs() < 1e-3);
    }

    #[test]
    fn a_tiny_bounds_never_yields_a_negative_rect() {
        let l = Layout::solve(Rect::new(0.0, 0.0, 30.0, 10.0), LayoutOptions { lower_left: true, upper_right: true, density: true, ..o() });
        assert!(l.upper.plot.w >= 0.0 && l.upper.plot.h >= 0.0);
        let lower = l.lower.unwrap();
        assert!(lower.plot.w >= 0.0 && lower.plot.h >= 0.0);
    }
}
```

- [ ] **Step 2: Implement**

```rust
impl Layout {
    pub fn solve(bounds: Rect, o: LayoutOptions) -> Layout {
        let axis_w = design_px(AXIS_WIDTH, o.rem_px);
        let strip_w = if o.density { design_px(DENSITY_STRIP, o.rem_px) } else { 0.0 };
        let x_axis_h = design_px(X_AXIS_HEIGHT, o.rem_px);
        let gap = design_px(PANE_GAP, o.rem_px);

        // Columns are reserved for EITHER pane's use so both share one x.
        let any_left = o.upper_left || o.lower_left;
        let any_right = o.upper_right || o.lower_right;
        let left_w = if any_left { axis_w } else { 0.0 };
        let right_w = if any_right { axis_w } else { 0.0 };
        let plot_x = bounds.x + left_w;
        let plot_w = (bounds.w - left_w - right_w - strip_w).max(0.0);
        let strip_x = plot_x + plot_w;
        let right_x = strip_x + strip_w;

        let has_lower = o.lower_left || o.lower_right;
        let avail = (bounds.h - x_axis_h - if has_lower { gap } else { 0.0 }).max(0.0);
        let split = if o.split.is_finite() { o.split.clamp(SPLIT_MIN, SPLIT_MAX) } else { SPLIT_DEFAULT };
        let upper_h = if has_lower { avail * split } else { avail };
        let lower_h = if has_lower { avail - upper_h } else { 0.0 };

        let pane = |y: f32, h: f32, left: bool, right: bool| PaneRects {
            plot: Rect::new(plot_x, y, plot_w, h),
            left_axis: left.then(|| Rect::new(bounds.x, y, axis_w, h)),
            right_axis: right.then(|| Rect::new(right_x, y, axis_w, h)),
            density: o.density.then(|| Rect::new(strip_x, y, strip_w, h)),
        };
        let upper = pane(bounds.y, upper_h, o.upper_left, o.upper_right);
        let lower = has_lower.then(|| pane(bounds.y + upper_h + gap, lower_h, o.lower_left, o.lower_right));
        let lowest_bottom = lower.map_or(upper.plot.bottom(), |l| l.plot.bottom());
        Layout {
            upper,
            lower,
            x_axis: Rect::new(plot_x, lowest_bottom, plot_w, x_axis_h),
        }
    }

    pub fn lowest_bottom(&self) -> f32 {
        self.x_axis.y
    }
}
```

- [ ] **Step 3: Run, format, lint, commit**

```bash
cargo test -p geode-chart layout && cargo fmt --check && cargo clippy -p geode-chart --all-targets -- -D warnings
git add crates/geode-chart && git commit -m "chart: Layout::solve — two panes, reserved columns, one x axis"
```

---

### Task 5: `decimate`

**Files:**
- Modify: `crates/geode-chart/src/core/decimate.rs`

**Interfaces:**
- Consumes: `Point`, `Point::BREAK`.
- Produces: `decimate(x: &[f32], y: &[f64], columns: usize, out: &mut Vec<Point>)`. `x` is plot-relative pixels (0 = the plot's left edge), ascending; a value's column is `floor(x)` clamped into `0..columns`. Two points per column at most (the column's min and max, in x order), one `Point::BREAK` per run of `NaN`s (never two in a row, never leading or trailing).

- [ ] **Step 1: Failing tests (unit + property)**

```rust
//! Min-max decimation (spec §8.1; the spike's "lyon's collapse is not
//! decimation"): the polyline through the output has every column's
//! extremes, so no spike between two pixels is lost.

use super::Point;

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn xs(n: usize, cols: usize) -> Vec<f32> {
        (0..n).map(|i| i as f32 * cols as f32 / n as f32).collect()
    }

    #[test]
    fn a_column_keeps_its_min_and_max_in_x_order() {
        // ten points in one column: max at index 2, min at index 7
        let x = vec![0.1; 10];
        let mut y = vec![5.0; 10];
        y[2] = 9.0;
        y[7] = 1.0;
        let mut out = Vec::new();
        decimate(&x, &y, 1, &mut out);
        assert_eq!(out, vec![Point::new(0.1, 9.0), Point::new(0.1, 1.0)]);
        // one point in a column: one output
        decimate(&[3.5], &[2.0], 8, &mut out);
        assert_eq!(out, vec![Point::new(3.5, 2.0)]);
    }

    #[test]
    fn a_nan_breaks_the_polyline() {
        let x = xs(6, 6);
        let y = [1.0, 2.0, f64::NAN, f64::NAN, 3.0, 4.0];
        let mut out = Vec::new();
        decimate(&x, &y, 6, &mut out);
        let breaks: Vec<usize> = out.iter().enumerate().filter(|(_, p)| p.is_break()).map(|(i, _)| i).collect();
        assert_eq!(breaks, vec![2], "one break for the run: {out:?}");
        assert_eq!(out.len(), 5);
        assert_eq!(out[4], Point::new(x[5], 4.0));
        // a NaN inside a column splits the column too
        decimate(&[0.2, 0.4, 0.6], &[1.0, f64::NAN, 2.0], 1, &mut out);
        assert_eq!(out, vec![Point::new(0.2, 1.0), Point::BREAK, Point::new(0.6, 2.0)]);
        // never leading, trailing or doubled
        decimate(&xs(4, 4), &[f64::NAN, 1.0, f64::NAN, f64::NAN], 4, &mut out);
        assert_eq!(out, vec![Point::new(1.0, 1.0)]);
        decimate(&xs(3, 3), &[f64::NAN; 3], 3, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn out_is_reused_not_grown() {
        let mut out = Vec::with_capacity(64);
        let ptr = out.as_ptr();
        decimate(&xs(20, 10), &[1.0; 20], 10, &mut out);
        decimate(&xs(20, 10), &[2.0; 20], 10, &mut out);
        assert_eq!(out.as_ptr(), ptr, "no reallocation while capacity suffices");
        assert!(out.iter().all(|p| p.y == 2.0));
    }

    #[test]
    fn points_outside_the_columns_clamp_to_the_edge_columns() {
        let mut out = Vec::new();
        decimate(&[-3.0, 0.5, 12.0], &[1.0, 2.0, 3.0], 10, &mut out);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].x, -3.0);
        assert_eq!(out[2].y, 3.0);
    }

    proptest! {
        #[test]
        fn decimation_keeps_every_columns_min_and_max(
            n in 1usize..400,
            cols in 1usize..40,
            seed in any::<u64>(),
        ) {
            let x = xs(n, cols);
            let mut s = seed | 1;
            let y: Vec<f64> = (0..n).map(|_| {
                s ^= s << 13; s ^= s >> 7; s ^= s << 17;
                if s % 11 == 0 { f64::NAN } else { (s % 1000) as f64 / 10.0 }
            }).collect();
            let mut out = Vec::new();
            decimate(&x, &y, cols, &mut out);
            for c in 0..cols {
                let ys: Vec<f64> = (0..n).filter(|&i| (x[i].floor() as usize).min(cols - 1) == c && y[i].is_finite()).map(|i| y[i]).collect();
                if ys.is_empty() { continue; }
                let lo = ys.iter().copied().fold(f64::INFINITY, f64::min);
                let hi = ys.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let col_out: Vec<f64> = out.iter().filter(|p| !p.is_break() && (p.x.floor() as usize).min(cols - 1) == c).map(|p| p.y as f64).collect();
                prop_assert!(col_out.iter().any(|v| (*v - lo).abs() < 1e-4), "column {c} lost its min {lo}: {col_out:?}");
                prop_assert!(col_out.iter().any(|v| (*v - hi).abs() < 1e-4), "column {c} lost its max {hi}");
                // at most two points per NaN-free run within the column
                let runs = 1 + (0..n).filter(|&i| (x[i].floor() as usize).min(cols - 1) == c).collect::<Vec<_>>().windows(2).filter(|w| y[w[0]].is_finite() != y[w[1]].is_finite()).count();
                prop_assert!(col_out.len() <= 2 * runs, "column {c}: {} points over {runs} runs", col_out.len());
            }
            prop_assert!(!out.first().is_some_and(|p| p.is_break()));
            prop_assert!(!out.last().is_some_and(|p| p.is_break()));
            prop_assert!(!out.windows(2).any(|w| w[0].is_break() && w[1].is_break()));
        }
    }
}
```

- [ ] **Step 2: Implement**

```rust
/// See the module doc. `out` is cleared, never shrunk.
pub fn decimate(x: &[f32], y: &[f64], columns: usize, out: &mut Vec<Point>) {
    out.clear();
    let n = x.len().min(y.len());
    if n == 0 || columns == 0 {
        return;
    }
    let last_col = columns - 1;
    let column_of = |px: f32| -> usize {
        if !(px > 0.0) {
            0
        } else {
            (px.floor() as usize).min(last_col)
        }
    };
    // The open column: (index of min, min y, index of max, max y).
    let mut col: Option<(usize, usize, f64, usize, f64)> = None;
    let mut pending_break = false;

    let flush = |col: &mut Option<(usize, usize, f64, usize, f64)>, out: &mut Vec<Point>| {
        if let Some((_, imin, ymin, imax, ymax)) = col.take() {
            if imin == imax {
                out.push(Point::new(x[imin], ymin as f32));
            } else if imin < imax {
                out.push(Point::new(x[imin], ymin as f32));
                out.push(Point::new(x[imax], ymax as f32));
            } else {
                out.push(Point::new(x[imax], ymax as f32));
                out.push(Point::new(x[imin], ymin as f32));
            }
        }
    };

    for i in 0..n {
        let v = y[i];
        if !v.is_finite() {
            flush(&mut col, out);
            if !out.is_empty() {
                pending_break = true;
            }
            continue;
        }
        if pending_break {
            out.push(Point::BREAK);
            pending_break = false;
        }
        let c = column_of(x[i]);
        match &mut col {
            Some((cc, imin, ymin, imax, ymax)) if *cc == c => {
                if v < *ymin {
                    *imin = i;
                    *ymin = v;
                }
                if v > *ymax {
                    *imax = i;
                    *ymax = v;
                }
            }
            _ => {
                flush(&mut col, out);
                col = Some((c, i, v, i, v));
            }
        }
    }
    flush(&mut col, out);
}
```

(`pending_break` is only ever pushed right before a finite point, so a break is never trailing; it is set only when `out` is non-empty, so never leading; and the flag collapses a run into one.)

- [ ] **Step 3: Run, format, lint, commit**

```bash
cargo test -p geode-chart decimate && cargo fmt --check && cargo clippy -p geode-chart --all-targets -- -D warnings
git add crates/geode-chart && git commit -m "chart: min-max decimation with NaN breaks, property-tested"
```

---

### Task 6: `Palette` and the bundled-theme sweep

**Files:**
- Modify: `crates/geode-chart/src/core/palette.rs`

**Interfaces:**
- Produces: `palette::{Palette, to_rgb, to_hsla}`; `Palette::from_theme(chart: [Hsla; 5], background: Hsla, foreground: Hsla) -> Palette`, `Palette::colour(&self, index: usize) -> Hsla` (cycles), `Palette::LEN`.

This module is the one place `core` touches a gpui type (`Hsla`): the floor needs `geode_core::colour::Rgb` and the theme hands `Hsla`. The two converters are byte-for-byte the shell's `shell::colours::{to_rgb, to_hsla}` — copied, with a comment, because the shell is not a dependency. Spec §8.1 wrote `default_for(slot, theme_chart, background)`; the floor's `readable_on` needs the direction to move in (`toward` = `foreground`), so it takes the foreground too — recorded as an as-built amendment.

- [ ] **Step 1: Failing tests**

```rust
//! The default series colours: the theme's five chart colours, each
//! floored to 3:1 against the background (spec §8.1).

use geode_core::colour::{Rgb, readable_on};
use gpui::Hsla;

pub struct Palette {
    colours: [Hsla; 5],
}

/// Mirrors `geode_shell::shell::colours::to_rgb` — the shell is not a
/// dependency of this crate.
pub fn to_rgb(hsla: Hsla) -> Rgb {
    let c = hsla.to_rgb();
    Rgb { r: c.r, g: c.g, b: c.b }
}

pub fn to_hsla(rgb: Rgb) -> Hsla {
    gpui::Rgba { r: rgb.r, g: rgb.g, b: rgb.b, a: 1.0 }.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::colour::{READABLE_RATIO, contrast_ratio};
    use gpui::{hsla, rgb};

    #[test]
    fn a_faint_chart_colour_is_floored_and_a_clear_one_kept() {
        let bg = hsla(0.0, 0.0, 1.0, 1.0); // white
        let fg = hsla(0.0, 0.0, 0.0, 1.0);
        let faint: Hsla = rgb(0xffff66).into(); // pale yellow on white: ~1.2:1
        let clear: Hsla = rgb(0x1f3a93).into();
        let p = Palette::from_theme([faint, clear, faint, clear, faint], bg, fg);
        assert!(contrast_ratio(to_rgb(p.colour(0)), to_rgb(bg)) >= READABLE_RATIO);
        assert_eq!(p.colour(1), clear, "a clearing colour is untouched");
        assert_eq!(p.colour(5), p.colour(0), "cycles");
        assert_eq!(p.colour(7), p.colour(2));
    }

    #[gpui::test]
    fn every_bundled_themes_palette_is_readable(cx: &mut gpui::TestAppContext) {
        use gpui_component::{ActiveTheme, Theme};
        cx.update(gpui_component::init);
        let (service, _) = geode_shell::theme::load_bundled();
        let mut failures = Vec::new();
        let mut checked = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let t = cx.theme();
                let p = Palette::from_theme([t.chart_1, t.chart_2, t.chart_3, t.chart_4, t.chart_5], t.background, t.foreground);
                for i in 0..Palette::LEN {
                    checked += 1;
                    let ratio = contrast_ratio(to_rgb(p.colour(i)), to_rgb(t.background));
                    if ratio < READABLE_RATIO {
                        failures.push(format!("{name}: chart_{} at {ratio:.2}:1", i + 1));
                    }
                }
            });
        }
        assert!(checked >= 5 * 40, "the sweep saw {checked} checks — bundled themes missing?");
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
```

(Match the exact names `Theme::global_mut`, `apply_config`, `service.names()`, `service.resolve()` against `crates/geode-shell/src/shell/chip.rs`'s sweep and `crates/geode-shell/src/theme.rs` — copy what that test does, do not guess.)

- [ ] **Step 2: Implement**

```rust
impl Palette {
    pub const LEN: usize = 5;

    pub fn from_theme(chart: [Hsla; 5], background: Hsla, foreground: Hsla) -> Self {
        let bg = to_rgb(background);
        let fg = to_rgb(foreground);
        Self { colours: chart.map(|c| to_hsla(readable_on(to_rgb(c), bg, fg))) }
    }

    /// The colour for the `index`-th slot, cycling.
    pub fn colour(&self, index: usize) -> Hsla {
        self.colours[index % Self::LEN]
    }
}
```

If `readable_on` returns an unchanged `Rgb` for a clearing colour but the `to_rgb`/`to_hsla` round trip drifts the `Hsla` by a float ulp, compare `clear` in the first test through `to_rgb` with a tolerance instead of `assert_eq!` — say which in the report.

- [ ] **Step 3: Run, format, lint, commit**

```bash
cargo test -p geode-chart palette && cargo fmt --check && cargo clippy -p geode-chart --all-targets -- -D warnings
git add crates/geode-chart && git commit -m "chart: the floored palette and its bundled-theme sweep"
```

---

### Task 7: `ChartModel` and `ChartElement`

**Files:**
- Create: `crates/geode-chart/src/model.rs`, `crates/geode-chart/src/element.rs`
- Modify: `crates/geode-chart/src/lib.rs` (the `pub mod`/`pub use` lines from Task 1)

**Interfaces:**
- Consumes: everything in `core`.
- Produces: `ChartModel`, `ChartSlot`, `ChartElement::new(model: Arc<ChartModel>, view: View, rem_px: f32, id: impl Into<ElementId>)`, `element::rebuilds() -> usize`.

This is the one task that needs the gpui-kit skill: load `gpui-kit` before starting and read `plot/mod.rs` (the `Plot` trait), `plot/path_cache.rs`, `plot/tooltip.rs`, `plot/axis.rs`, `plot/grid.rs`, `plot/label.rs` and `chart/line_chart.rs` (the worked example of all of them) in the registry copy. Every API name below was read from those files; if one does not match, the file wins and the report says so.

- [ ] **Step 1: `model.rs`**

```rust
//! The immutable input a tile builds once per delivery (spec §8.3).

use std::sync::Arc;

use gpui::{Hsla, SharedString};

use crate::core::axis::{Axis, AxisMode, Pane, Side};
use crate::core::layout::LayoutOptions;
use crate::core::time::TimeScale;

#[derive(Debug, Clone, PartialEq)]
pub struct ChartSlot {
    pub number: u8,
    pub label: SharedString,
    /// `buckets.len()` long; `NaN` where the slot has no bucket.
    pub values: Vec<f64>,
    pub colour: Hsla,
    pub axis: Axis,
    pub visible: bool,
    /// `(fraction, value)`; empty when off.
    pub percentiles: Vec<(f64, f64)>,
    /// Pre-formatted `p5`/`p50`/`p95` tags, parallel to `percentiles`.
    pub percentile_labels: Vec<SharedString>,
    /// `(lo, hi, count)` ascending; empty when off.
    pub bins: Vec<(f64, f64, u32)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChartModel {
    /// Bumped by the builder on every change; the path cache key.
    pub version: u64,
    /// Epoch micros, ascending.
    pub buckets: Vec<i64>,
    /// One display bucket's width in micros (the frequency).
    pub step_us: i64,
    pub axis_mode: AxisMode,
    /// Seconds east of UTC for every displayed time.
    pub offset_secs: i32,
    pub split: f32,
    pub density: bool,
    pub slots: Vec<ChartSlot>,
}

impl ChartModel {
    pub fn empty() -> Arc<Self> {
        Arc::new(Self { version: 0, buckets: Vec::new(), step_us: 1, axis_mode: AxisMode::Session, offset_secs: 0, split: 0.7, density: false, slots: Vec::new() })
    }

    pub fn time_scale(&self) -> TimeScale<'_> {
        match self.axis_mode {
            AxisMode::Session => TimeScale::Session { buckets: &self.buckets },
            AxisMode::Continuous => TimeScale::Continuous { buckets: &self.buckets, step_us: self.step_us },
        }
    }

    pub fn full(&self) -> (f64, f64) {
        self.time_scale().full()
    }

    fn uses(&self, pane: Pane, side: Side) -> bool {
        self.slots.iter().any(|s| s.visible && s.axis.pane() == pane && s.axis.side() == side)
    }

    pub fn layout_options(&self, rem_px: f32) -> LayoutOptions {
        LayoutOptions {
            upper_left: self.uses(Pane::Upper, Side::Left),
            upper_right: self.uses(Pane::Upper, Side::Right),
            lower_left: self.uses(Pane::Lower, Side::Left),
            lower_right: self.uses(Pane::Lower, Side::Right),
            density: self.density && self.slots.iter().any(|s| s.visible && !s.bins.is_empty()),
            split: self.split,
            rem_px,
        }
    }

    /// The tag a percentile line wears: `p5`, `p50`, `p99.5`.
    pub fn percentile_label(fraction: f64) -> SharedString {
        let pct = fraction * 100.0;
        if (pct - pct.round()).abs() < 1e-9 { format!("p{}", pct.round() as i64).into() } else { format!("p{pct}").into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_options_follow_visible_slots_only() {
        let mut m = (*ChartModel::empty()).clone();
        let slot = |axis, visible| ChartSlot { number: 1, label: "s".into(), values: vec![], colour: gpui::black(), axis, visible, percentiles: vec![], percentile_labels: vec![], bins: vec![(0.0, 1.0, 1)] };
        m.slots = vec![slot(Axis::Left, true), slot(Axis::BottomRight, false)];
        m.density = true;
        let o = m.layout_options(12.0);
        assert!(o.upper_left && !o.upper_right && !o.lower_left && !o.lower_right);
        assert!(o.density);
        m.slots[1].visible = true;
        assert!(m.layout_options(12.0).lower_right);
        m.slots.iter_mut().for_each(|s| s.bins.clear());
        assert!(!m.layout_options(12.0).density, "density with no bins reserves nothing");
        assert_eq!(ChartModel::percentile_label(0.05), SharedString::from("p5"));
        assert_eq!(ChartModel::percentile_label(0.995), SharedString::from("p99.5"));
    }
}
```

- [ ] **Step 2: `element.rs` — structure**

```rust
//! One `Plot` element painting a `ChartModel` (spec §8.3).
//!
//! Per frame: solve the layout at a zero origin (`PathCache` translates
//! every path to the frame's origin), derive each side's `LinearScale`
//! from the VISIBLE values, then per pane: grid, axes, each visible
//! slot's decimated polyline through `PathCaches("lines")`, its
//! percentile lines as dashed paths through `PathCaches("percentiles")`,
//! its density bars as quads; then the x axis and, through the
//! component's tooltip plumbing, the crosshair and readout.
//!
//! The data path — `xs` fill, decimation, tessellation — runs only when
//! `(model.version, view, bounds size, slot)` misses the cache;
//! `REBUILDS` counts those misses so a test can pin "an unchanged frame
//! rebuilds nothing". Axis tick and percentile labels go through the
//! component's `PlotAxis`/`PlotLabel`, which own a small `Vec` per
//! frame: the one per-frame allocation, shared with every shipped chart.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::{AnyElement, App, Bounds, ElementId, Hsla, Path, PathBuilder, Pixels, Point as GPoint, SharedString, TextAlign, Window, fill, point, px, size};
use gpui_component::ActiveTheme;
use gpui_component::plot::{AxisLabelSide, AxisText, Grid, IntoPlot, PathCaches, Plot, PlotAxis, PlotLabel, ShapeKey};
use gpui_component::plot::label::Text;
use gpui_component::plot::tooltip::{CrossLine, Tooltip, TooltipState};

use crate::core::axis::{Pane, Side};
use crate::core::decimate::decimate;
use crate::core::layout::{Layout, PaneRects};
use crate::core::scale::{LinearScale, axis_domain, fmt_tick, fmt_value};
use crate::core::time::{Crosshair, Tick, ticks};
use crate::core::view::View;
use crate::core::{DASH, GAP, Point, Rect, TICK_GAP, Y_TICK_GAP, design_px};
use crate::model::ChartModel;

static REBUILDS: AtomicUsize = AtomicUsize::new(0);

/// How many times a slot's polyline or percentile path was rebuilt
/// since the process started — a test's window onto the cache.
pub fn rebuilds() -> usize {
    REBUILDS.load(Ordering::Relaxed)
}

/// Element state kept across frames under the element id: the reused
/// decimation buffers and the tick labels of the last chrome key.
#[derive(Default)]
struct Buffers {
    xs: Vec<f32>,
    pts: Vec<Point>,
    chrome_key: Option<u64>,
    x_ticks: Vec<Tick>,
    y_ticks: Vec<f64>,
}

#[derive(IntoPlot)]
pub struct ChartElement {
    model: Arc<ChartModel>,
    view: View,
    rem_px: f32,
    id: ElementId,
}

impl ChartElement {
    pub fn new(model: Arc<ChartModel>, view: View, rem_px: f32, id: impl Into<ElementId>) -> Self {
        Self { model, view, rem_px, id: id.into() }
    }

    fn layout(&self, bounds: Bounds<Pixels>) -> Layout {
        // Zero origin: paths are cached origin-free and translated by the
        // cache; quads and labels add `bounds.origin` themselves.
        let r = Rect::new(0.0, 0.0, f32::from(bounds.size.width), f32::from(bounds.size.height));
        Layout::solve(r, self.model.layout_options(self.rem_px))
    }

    /// The y scale for a pane's side over the VISIBLE values of the slots
    /// on it; `None` when no visible slot with a finite value uses it.
    fn side_scale(&self, pane: Pane, side: Side, plot: Rect, visible: (usize, usize)) -> Option<LinearScale> {
        let values = self.model.slots.iter()
            .filter(|s| s.visible && s.axis.pane() == pane && s.axis.side() == side)
            .flat_map(|s| s.values[visible.0.min(s.values.len())..visible.1.min(s.values.len())].iter().copied());
        axis_domain(values).map(|d| LinearScale::new(d, plot.y, plot.bottom()))
    }
}
```

- [ ] **Step 3: `element.rs` — the `Plot` impl**

Write `paint` in this order; each helper is a private `fn` on `ChartElement` taking `&Buffers`/`&mut Buffers` where it needs them:

1. `let layout = self.layout(bounds); let scale = self.model.time_scale(); let visible = scale.visible(self.view); let theme = cx.theme();` Colours: `theme.border` for axes and grid, `theme.muted_foreground` for tick text, `theme.background` under the density bars.
2. `let buffers = window.use_keyed_state(("geode-chart-buffers", self.id.clone()), cx, |_, _| Buffers::default());` — chrome: a `chrome_key = ShapeKey::new((self.model.version, self.view.key())).f32(w).f32(h).f32(self.rem_px).finish()`; on a miss recompute `x_ticks` via `ticks(&scale, self.view, layout.x_axis, design_px(TICK_GAP, rem), self.model.offset_secs, &mut b.x_ticks)` (note `x_axis` shares the plot's x and w).
3. For each pane `(Pane::Upper, &layout.upper)` and, if present, `(Pane::Lower, lower)`:
   - left/right `LinearScale`s via `side_scale`.
   - **Grid**: `Grid::new().x(x tick xs as Pixels).y(left-or-right scale's ticks mapped to y).stroke(theme.border).dash_array(&[px(4.), px(2.)]).paint(&bounds_of(pane.plot), window)` where `bounds_of(r)` = `Bounds::new(bounds.origin + point(px(r.x), px(r.y)), size(px(r.w), px(r.h)))`. Tick count hint: `(pane.plot.h / design_px(Y_TICK_GAP, rem)).max(2.) as usize`.
   - **Axes**: for a left axis rect with a scale: `PlotAxis::new().x_axis(false).y(px(r.w)).y_label_side(AxisLabelSide::Start).y_label(ticks.map(|v| AxisText::new(fmt_tick(v, step), px(scale.y(v) - r.y), theme.muted_foreground).align(TextAlign::Right))).stroke(theme.border).paint(&bounds_of(r), window, cx)`. Right axis: `.y(px(0.))` and `AxisLabelSide::End`, left-aligned. Read `axis.rs`'s `y_label` and `label.rs`'s `Text::paint` to confirm how `align` anchors the origin before choosing `Right`; if right alignment anchors elsewhere, left-align at `px(2.)` and note it.
   - **Lines**: `let caches = PathCaches::for_paint(("lines", pane index), window, cx); caches.update(cx, |caches, _| for (k, slot) in visible slots on this pane { let s = the slot's side scale; let key = ShapeKey::new((self.model.version, slot.number, pane as u8)).f32(view.lo as f32)…` — use `.f32(self.view.lo as f32).f32(self.view.hi as f32)`? No: `f64` bits matter; hash `self.view.key()` inside `ShapeKey::new((version, number, pane, view.key()))` then `.f32(plot.w).f32(plot.h).f32(plot.x).f32(plot.y)`; `caches.slot(k).get(key, bounds.origin, || { REBUILDS.fetch_add(1, Relaxed); build the path })`. The builder: fill `b.xs` with `scale.x_of(i, view, plot) - plot.x` for `i in visible`, `decimate(&b.xs, &slot.values[visible], plot.w.max(1.) as usize, &mut b.pts)`, then `PathBuilder::stroke(px(1.5))`, `move_to` on the first point and after every `BREAK`, `line_to` otherwise, each point at `(plot.x + p.x, s.y(p.y as f64))`; `build().ok()`. Paint with `window.paint_path(path, slot.colour)`. (Borrow note: `buffers.update(cx, …)` and `caches.update(cx, …)` cannot nest — take `xs`/`pts` out of `Buffers` with `std::mem::take` before the caches update and put them back after; both are `Vec`s, so this moves no data.)
   - **Percentiles**: `PathCaches::for_paint(("percentiles", pane index), …)`, one cache slot per `(slot position, percentile index)` (`k * 8 + j`, capping at 8 per slot), key `ShapeKey::new((version, number, j)).f32(y).f32(plot.x).f32(plot.w)`, builder `dashed_horizontal(x0, x1, y, dash_px, gap_px)` (a free fn: `move_to`/`line_to` per dash, `PathBuilder::stroke(px(1.))`). Then the label: `PlotLabel::new(vec![Text::new(label.clone(), point(px(plot.right() - 24.), px(y - 11.)), slot.colour)]).paint(&bounds, window, cx)` — one `Vec` per line per frame, the documented exception.
   - **Density**: if `pane.density` is `Some(strip)`: for each visible slot on this pane with bins, `max = max count`; per bin `(lo, hi, n)`: `let top = s.y(hi); let bottom = s.y(lo); let w = strip.w * n as f32 / max as f32;` `window.paint_quad(fill(Bounds::new(origin + (strip.x, top), size(w, (bottom - top).max(1.))), slot.colour.opacity(0.45)))`.
4. **x axis**: `PlotAxis::new().x(px(0.)).x_label(b.x_ticks.iter().map(|t| AxisText::new(t.label.clone(), px(t.x - layout.x_axis.x), theme.muted_foreground))).stroke(theme.border).paint(&bounds_of(layout.x_axis), window, cx)` — `t.x` is plot-relative; the axis rect starts at `plot.x`, so subtract it.

`id`: `Some(self.id.clone())`.

`tooltip_state(position, bounds, cx)`: `let layout = …; let pane = [upper, lower].find(|p| p.plot.contains(x, y))?; let i = Crosshair::at(position.x, &scale, self.view, pane.plot)?; Some(TooltipState::new(i, point(px(scale.x_of(i, view, plot)), position.y), vec![]))`.

`tooltip(state, cursor, bounds, window, cx)`: title from `self.model.buckets.get(state.index)` formatted `%Y-%m-%d %H:%M` at `offset_secs`; `Tooltip::new(cursor, bounds.size).gap(px(8.)).cross_line(CrossLine::new(state.cross_line).span(layout.upper.plot.y, layout.lowest_bottom() - layout.upper.plot.y))` then `.row(slot.colour, slot.label.clone(), fmt_value(slot.values[state.index]))` for every visible slot (either pane) — `into_any_element()`.

- [ ] **Step 4: The smoke test — a test window, two draws, a moved view**

At the bottom of `element.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::axis::Axis;
    use crate::model::ChartSlot;
    use gpui::{Context, Entity, Render, div, prelude::*};

    struct Host {
        model: Arc<ChartModel>,
        view: View,
    }

    impl Render for Host {
        fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(ChartElement::new(self.model.clone(), self.view, f32::from(window.rem_size()), "chart"))
        }
    }

    pub(crate) fn model(n: usize) -> Arc<ChartModel> {
        let day = 86_400_000_000i64;
        let buckets: Vec<i64> = (0..n as i64).map(|i| i * day).collect();
        let walk = |seed: u64| -> Vec<f64> {
            let mut s = seed | 1;
            let mut v = 100.0;
            (0..n).map(|i| { s ^= s << 13; s ^= s >> 7; s ^= s << 17; v += ((s % 200) as f64 - 100.0) / 50.0; if i % 97 == 50 { f64::NAN } else { v } }).collect()
        };
        let slot = |number, axis, seed| ChartSlot {
            number, label: format!("s{number}").into(), values: walk(seed), colour: gpui::red(), axis, visible: true,
            percentiles: vec![(0.05, 95.0), (0.5, 100.0), (0.95, 105.0)],
            percentile_labels: vec!["p5".into(), "p50".into(), "p95".into()],
            bins: (0..40).map(|b| (90.0 + b as f64 * 0.5, 90.5 + b as f64 * 0.5, (b % 7 + 1) as u32)).collect(),
        };
        Arc::new(ChartModel { version: 1, buckets, step_us: day, axis_mode: AxisMode::Session, offset_secs: 0, split: 0.7, density: true, slots: vec![slot(1, Axis::Left, 7), slot(2, Axis::Right, 11), slot(3, Axis::BottomLeft, 13)] })
    }

    fn open(cx: &mut gpui::TestAppContext, model: Arc<ChartModel>) -> (Entity<Host>, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        let mut host = None;
        let window = cx.update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let h = cx.new(|_| Host { view: View::full(model.full()), model: model.clone() });
                host = Some(h.clone());
                cx.new(|cx| gpui_component::Root::new(h, window, cx))
            })
        }).unwrap();
        let vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        (host.unwrap(), vcx)
    }

    fn draw(vcx: &mut gpui::VisualTestContext) {
        vcx.update(|window, cx| { let _ = window.draw(cx); });
    }

    #[gpui::test]
    fn an_unchanged_frame_rebuilds_nothing_and_a_moved_view_rebuilds(cx: &mut gpui::TestAppContext) {
        let (host, mut vcx) = open(cx, model(500));
        let before = rebuilds();
        draw(&mut vcx);
        let first = rebuilds() - before;
        assert_eq!(first, 3 + 9, "three polylines and nine percentile lines on the first frame");
        draw(&mut vcx);
        draw(&mut vcx);
        assert_eq!(rebuilds() - before, first, "an unchanged frame rebuilt nothing");
        host.update(&mut vcx, |h, cx| { let full = h.model.full(); h.view.zoom(2.0, 0.5, full); cx.notify(); });
        draw(&mut vcx);
        assert_eq!(rebuilds() - before, 2 * first, "a moved view rebuilds every path once");
    }

    #[gpui::test]
    fn an_empty_model_and_a_hidden_lower_pane_paint_without_panicking(cx: &mut gpui::TestAppContext) {
        let (host, mut vcx) = open(cx, ChartModel::empty());
        draw(&mut vcx);
        let mut m = (*model(10)).clone();
        m.slots[2].visible = false;
        m.version = 2;
        host.update(&mut vcx, |h, cx| { h.model = Arc::new(m); cx.notify(); });
        draw(&mut vcx);
    }

    #[test]
    fn a_percentile_line_is_dashed_at_dash_and_gap() {
        assert_eq!(dash_count(100.0, 4.0, 3.0), 15, "ceil(100 / 7)");
        assert_eq!(dash_count(7.0, 4.0, 3.0), 1);
        assert_eq!(dash_count(0.0, 4.0, 3.0), 0);
        assert_eq!(dash_count(10.0, 0.0, 3.0), 1, "no dash length: one solid segment");
    }
}
```

`dash_count(width, dash, gap) -> usize` is the pure count `dashed_horizontal` iterates (`if dash <= 0 { 1 } else { (width / (dash + gap)).ceil() }`, zero for a zero width) — factor it out so the dash rule has a test with no window.

- [ ] **Step 5: Run, format, lint**

Run: `cargo test -p geode-chart && cargo fmt --check && cargo clippy -p geode-chart --all-targets -- -D warnings`

If the `IntoPlot` derive fails to resolve `gpui`, it is `proc_macro_crate` looking for a dependency whose PACKAGE name is `gpui-pre` — `gpui.workspace = true` provides it (aliased); check `crate_path.rs` in `gpui-component-macros-0.6.2` and report rather than switching to a hand-written `Element` impl.

- [ ] **Step 6: Commit**

```bash
git add crates/geode-chart && git commit -m "chart: ChartModel and the ChartElement Plot — cached paths, dashed percentiles, density bars, crosshair readout"
```

---

### Task 8: The example window, the bench, `docs/perf.md`

**Files:**
- Modify: `crates/geode-chart/examples/chart.rs`, `crates/geode-chart/benches/decimate.rs`, `docs/perf.md`

- [ ] **Step 1: The example (the display-check vehicle; kept, not deleted)**

`examples/chart.rs`:

```rust
//! A window over a generated three-slot model: `cargo run -p geode-chart --example chart`.
//!
//! Exists because the implementation sandbox cannot paint a window: this
//! is what the display check runs. Two panes (s3 on the lower left),
//! density on, three percentiles, a NaN gap every 97 buckets. Keys:
//! `h`/`l` pan, `=`/`-` zoom, `0` reset — the module's own keys are
//! Part 4's; these are the example's.

use std::sync::Arc;

use geode_chart::core::axis::{Axis, AxisMode};
use geode_chart::{ChartElement, ChartModel, ChartSlot, View};
use gpui::{App, Context, KeyDownEvent, Render, Window, div, prelude::*};
use gpui_component::{ActiveTheme, Root};

struct Demo {
    model: Arc<ChartModel>,
    view: View,
    focus: gpui::FocusHandle,
}

fn model(cx: &App) -> Arc<ChartModel> {
    let n = 2_000usize;
    let minute = 60_000_000i64;
    let start = 1_767_621_000_000_000i64; // 2026-01-05 14:30 UTC
    let buckets: Vec<i64> = (0..n as i64).map(|i| start + (i / 400) * 86_400_000_000 + (i % 400) * minute).collect();
    let t = cx.theme();
    let palette = geode_chart::core::palette::Palette::from_theme([t.chart_1, t.chart_2, t.chart_3, t.chart_4, t.chart_5], t.background, t.foreground);
    let walk = |seed: u64, base: f64| -> Vec<f64> { /* xorshift random walk with a NaN every 97th bucket, as in element.rs's tests */ };
    let slot = |number: u8, axis, seed, base| { /* values, percentiles at the 5/50/95 of the walk (computed here, in the EXAMPLE, never in the crate), 40 bins over the walk's range */ };
    Arc::new(ChartModel { version: 1, buckets, step_us: minute, axis_mode: AxisMode::Session, offset_secs: 0, split: 0.7, density: true, slots: vec![slot(1, Axis::Left, 7, 100.0), slot(2, Axis::Right, 11, 20.0), slot(3, Axis::BottomLeft, 13, 1.0)] })
}

impl Render for Demo {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let full = self.model.full();
        div()
            .track_focus(&self.focus)
            .size_full()
            .bg(cx.theme().background)
            .p_4()
            .on_key_down(cx.listener(move |this, e: &KeyDownEvent, _, cx| {
                match e.keystroke.key.as_str() {
                    "h" => this.view.pan(-0.1, full),
                    "l" => this.view.pan(0.1, full),
                    "=" | "+" => this.view.zoom(1.25, 0.5, full),
                    "-" => this.view.zoom(0.8, 0.5, full),
                    "0" => this.view.reset(full),
                    _ => return,
                }
                cx.notify();
            }))
            .child(ChartElement::new(self.model.clone(), self.view, f32::from(window.rem_size()), "chart"))
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        gpui_component::init(cx);
        cx.open_window(gpui::WindowOptions::default(), |window, cx| {
            let model = model(cx);
            let demo = cx.new(|cx| { let focus = cx.focus_handle(); focus.focus(window); Demo { view: View::full(model.full()), model, focus } });
            cx.new(|cx| Root::new(demo, window, cx))
        })
        .expect("open window");
        cx.activate(true);
    });
}
```

Fill the two elided closures in full (the plan elides them only because they mirror `element.rs`'s test fixture; the example computes its own percentiles and bins with plain sorting — that arithmetic lives in the example binary, which is not the crate). If `focus.focus(window)` is not the API at the pinned rev, read `gpui-pre-0.3.5/src/window.rs` for `FocusHandle::focus`.

- [ ] **Step 2: The bench**

`benches/decimate.rs`:

```rust
//! Spec §8.4: 500,000 points into 1,600 columns plus the path rebuild.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use geode_chart::core::Point;
use geode_chart::core::decimate::decimate;
use gpui::{PathBuilder, point, px};

fn build_path(pts: &[Point]) -> Option<gpui::Path<gpui::Pixels>> {
    let mut b = PathBuilder::stroke(px(1.5));
    let mut pen_up = true;
    for p in pts {
        if p.is_break() { pen_up = true; continue; }
        let q = point(px(p.x), px(p.y));
        if pen_up { b.move_to(q); pen_up = false; } else { b.line_to(q); }
    }
    b.build().ok()
}

fn bench(c: &mut Criterion) {
    let n = 500_000usize;
    let cols = 1_600usize;
    let xs: Vec<f32> = (0..n).map(|i| i as f32 * cols as f32 / n as f32).collect();
    let mut s = 0x9E3779B97F4A7C15u64;
    let mut v = 100.0f64;
    let ys: Vec<f64> = (0..n).map(|i| { s ^= s << 13; s ^= s >> 7; s ^= s << 17; v += ((s % 200) as f64 - 100.0) / 50.0; if i % 5_000 == 2_500 { f64::NAN } else { v } }).collect();
    let mut out = Vec::with_capacity(2 * cols + 200);
    c.bench_function("decimate/500k_into_1600", |b| b.iter(|| { decimate(black_box(&xs), black_box(&ys), cols, &mut out); black_box(out.len()) }));
    c.bench_function("decimate_and_path/500k_into_1600", |b| b.iter(|| { decimate(black_box(&xs), black_box(&ys), cols, &mut out); black_box(build_path(&out).map(|p| p.vertices.len())) }));
}

criterion_group!(benches, bench);
criterion_main!(benches);
```

Run: `cargo bench -p geode-chart` (release; a few minutes). Record both medians.

- [ ] **Step 3: `docs/perf.md`**

Append a section `## Timeseries chart (spec §8, Part 3)` after the Part 2 section: what is timed (decimation alone; decimation plus lyon tessellation of the result), the machine and build, a two-row table with the medians, whether §8.4's 2 ms holds (say so plainly either way), what is NOT measured (paint of a real frame, the sandbox has no window) and the per-frame allocation exception (chrome labels).

- [ ] **Step 4: Verify and commit**

```bash
cargo fmt --check && cargo clippy -p geode-chart --all-targets -- -D warnings && cargo test -p geode-chart && cargo check -p geode-chart --examples
git add crates/geode-chart docs/perf.md && git commit -m "chart: the example window, the decimate bench and its perf.md numbers"
```

---

### Task 9: Harness entries and the docs

**Files:**
- Modify: `scripts/mutation-check.sh`, `CLAUDE.md`, `docs/phase-history.md`, `docs/superpowers/specs/2026-09-19-geode-timeseries-viewer-design.md`

- [ ] **Step 1: Twelve harness entries**

Insert before the anchors-only verification block at the end of the script (find the last `run_mutation` and add after it), package `geode-chart`, each with its test. Anchors below are written against this plan's code; re-read each file and use the EXACT line as landed. Every anchor must match its file once (`--anchors-only`).

```zsh
run_mutation "chart: decimation drops the column maximum" \
  crates/geode-chart/src/core/decimate.rs \
  '                if v > *ymax {' \
  '                if v > *ymax && false {' \
  geode-chart \
  decimation_keeps_every_columns_min_and_max

run_mutation "chart: a NaN no longer breaks the polyline" \
  crates/geode-chart/src/core/decimate.rs \
  '            if !out.is_empty() {\n                pending_break = true;' \
  '            if !out.is_empty() {\n                pending_break = false;' \
  geode-chart \
  a_nan_breaks_the_polyline

run_mutation "chart: session ticks compare the wrong unit for a month" \
  crates/geode-chart/src/core/time.rs \
  '            Unit::Month => (t.year(), t.month(), 0, 0, 0),' \
  '            Unit::Month => (t.year(), 0, 0, 0, 0),' \
  geode-chart \
  session_ticks_fall_where_the_month_changes

run_mutation "chart: ticks ignore the gap" \
  crates/geode-chart/src/core/time.rs \
  '        if min_gap >= tick_gap_px && best_fit.is_none() {' \
  '        if best_fit.is_none() {' \
  geode-chart \
  ticks_respect_the_gap

run_mutation "chart: the first bucket is not a tick candidate" \
  crates/geode-chart/src/core/time.rs \
  '                let is_tick = i == 0 || unit.value(&t)' \
  '                let is_tick = i > 0 && unit.value(&t)' \
  geode-chart \
  a_day_of_minute_bars_shows_thinned_hours

run_mutation "chart: the view does not clamp" \
  crates/geode-chart/src/core/view.rs \
  '        if self.lo + w > full.1 {' \
  '        if false {' \
  geode-chart \
  a_pan_past_the_end_clamps

run_mutation "chart: the lower pane opens without a bottom slot" \
  crates/geode-chart/src/core/layout.rs \
  '        let has_lower = o.lower_left || o.lower_right;' \
  '        let has_lower = true;' \
  geode-chart \
  the_lower_pane_exists_only_while_a_visible_slot_uses_a_bottom_axis

run_mutation "chart: split is not clamped" \
  crates/geode-chart/src/core/layout.rs \
  'o.split.clamp(SPLIT_MIN, SPLIT_MAX)' \
  'o.split' \
  geode-chart \
  split_is_clamped

run_mutation "chart: the panes reserve columns separately" \
  crates/geode-chart/src/core/layout.rs \
  '        let any_right = o.upper_right || o.lower_right;' \
  '        let any_right = o.upper_right;' \
  geode-chart \
  both_panes_share_one_x_mapping

run_mutation "chart: the palette skips the floor" \
  crates/geode-chart/src/core/palette.rs \
  'chart.map(|c| to_hsla(readable_on(to_rgb(c), bg, fg)))' \
  'chart.map(|c| c)' \
  geode-chart \
  a_faint_chart_colour_is_floored_and_a_clear_one_kept

run_mutation "chart: the crosshair takes the first bucket at or past the cursor, not the nearest" \
  crates/geode-chart/src/core/time.rs \
  '        Some(if after < before { lo } else { lo - 1 })' \
  '        Some(lo)' \
  geode-chart \
  the_crosshair_picks_the_nearest_bucket

run_mutation "chart: the path key ignores the view" \
  crates/geode-chart/src/element.rs \
  '<the exact ShapeKey::new((...version, number, pane, self.view.key())) line for the lines cache>' \
  '<the same line with self.view.key() replaced by (0u64, 0u64)>' \
  geode-chart \
  an_unchanged_frame_rebuilds_nothing_and_a_moved_view_rebuilds
```

Run each filtered: `zsh scripts/mutation-check.sh "chart:"` — twelve `caught` lines. Then `zsh scripts/mutation-check.sh --anchors-only`. The layout "reserve columns" entry deserves a check that its test fails for the right reason (the x mapping, not a missing rect): run the mutation by hand once and read the assertion message.

- [ ] **Step 2: Spec §8.5 "As built (Part 3)"**

Add after §8.4: what was built as written; the amendments — `Palette::from_theme` takes `foreground` (the floor needs its direction); `X_AXIS_HEIGHT` = 18 and `Y_TICK_GAP` = 40 added; tick thinning and the "coarsest unit with two candidates" fallback (§8.2 said only "at least `TICK_GAP` apart"); `Continuous` skips a unit with more than 4,096 boundaries; the `Axis`/`AxisMode` enums live here and §9.2's module imports them; percentile labels are painted through `PlotLabel` in `paint`, not `prepaint` children; the one per-frame allocation (axis/percentile label `Vec`s through the component's own painters) and the `REBUILDS` counter that pins the data path; the density strip is reserved only when a visible slot HAS bins; `ChartModel` carries `step_us` and `offset_secs` (the module fills them from the frequency and `Local`); the bench numbers; what is pixel-unverified (everything painted — the example window is the check).

- [ ] **Step 3: CLAUDE.md**

- Status table: a row `Timeseries Part 3 (chart) (2026-09-20)` after the Part 2 row: `geode-chart` — pure core (`LinearScale`, session/continuous `TimeScale` with a unit chooser, two-pane `Layout`, min-max `decimate`, `View`, `Crosshair`, floored `Palette`, the `Axis` vocabulary) and one `Plot` element with cached paths; the example window is the display check; no tile (Part 4). Spec `2026-09-19-…timeseries-viewer` §8.
- Load-bearing rules, a new group **Chart (`geode-chart`)**, three bullets: (1) the crate depends on `geode-core`, `gpui`, `gpui-component`, `chrono` and nothing else; `DESIGN_REM` is a mirrored literal pinned to 12 by a test; every length goes through `core::design_px`. (2) `decimate` is min-max per column with `Point::BREAK` at a `NaN` run, never lyon's collapse; the element's data path runs only on a `(version, view, bounds, slot)` cache miss, `element::rebuilds()` counts misses and `an_unchanged_frame_rebuilds_nothing…` pins it; chrome labels through `PlotAxis`/`PlotLabel` are the documented per-frame allocation. (3) `Layout::solve` reserves an axis column when EITHER pane uses that side so both panes share one x mapping; the lower pane exists only while a visible slot uses a bottom axis; `split` clamps to `0.2..=0.8`; ticks are the finest unit whose candidates fit `TICK_GAP` else the coarsest with two candidates, thinned; a bucket's x is its slot's CENTRE.
- Harness count in the Commands block: recount with `grep -c '^run_mutation "' scripts/mutation-check.sh`.

- [ ] **Step 4: `docs/phase-history.md`**

One paragraph after the Part 2 paragraph: what Part 3 built, the plan's decisions (the ones in §8.5), and the review findings once known (the controller appends those at merge time).

- [ ] **Step 5: Full verification and commit**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p geode-chart && cargo check -p geode-shell --features test-support --all-targets && zsh scripts/mutation-check.sh --anchors-only
git add scripts/mutation-check.sh CLAUDE.md docs && git commit -m "docs: timeseries Part 3 as built — harness entries, status row, rules, §8.5"
```

(`cargo bench --workspace --no-run` is CI's; skip it locally per the build-cost memory. `cargo test --workspace` is the final review's and the merge's.)

---

## Self-review

**Spec coverage (§8):** constants — Task 1; `LinearScale` with 1-2-5 ticks — Task 1; `TimeScale` two modes, session ticks by unit change, `TICK_GAP`, labels, `Continuous` — Tasks 2–3; `Layout::solve` with both panes, per-side axes, the strip, `split` clamp, `PANE_GAP`, one x axis — Task 4; `decimate` — Task 5; `View` with pan/zoom/reset clamped — Task 2; `Crosshair::at` — Task 2; `Palette` floored, sweep with no exception list — Task 6; the element with `Plot`, `PathCache` keyed on (version, view, bounds), dashed percentiles as paths, density as quads under the quad cliff, shared x axis, crosshair spanning both panes with every visible slot in the readout, labels — Task 7; the example window (§12 item 3) — Task 8; the bench (§8.4, §11.3) — Task 8; §11.1 chart-core tests (decimation property, ticks increase and respect the gap, `View` clamps, layout invariants, palette sweep) — Tasks 2–6; §11.2 entries (session tick placement, min-max pair) plus ten more — Task 9. `:axis session|time` is Part 4's command over `AxisMode::parse`.

**Placeholders:** Task 7 Step 3 describes `paint` as an ordered procedure with the exact calls rather than one 300-line listing; every API named is one the implementer reads in the registry files listed at the top of the task. Task 8's example elides two closures whose bodies are the test fixture's; the step says to write them in full. Task 9's twelfth anchor is written as "the exact line" because the line is Task 7's to write.

**Type consistency:** `View::key() -> (u64, u64)`; `TimeScale::visible -> (usize, usize)`; `ticks(...) -> Option<Unit>` with `&mut Vec<Tick>`; `Layout::solve(Rect, LayoutOptions) -> Layout` with `lowest_bottom()`; `decimate(&[f32], &[f64], usize, &mut Vec<Point>)`; `Palette::from_theme([Hsla;5], Hsla, Hsla)`; `ChartElement::new(Arc<ChartModel>, View, f32, impl Into<ElementId>)`; `ChartModel::layout_options(f32) -> LayoutOptions`, `full() -> (f64, f64)`, `time_scale() -> TimeScale`. `Axis::pane()/side()` feed `ChartModel::uses`. `design_px(f32, f32)` everywhere a design constant is read.
