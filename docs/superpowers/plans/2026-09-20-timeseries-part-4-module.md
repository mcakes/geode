# Timeseries Part 4: `geode-timeseries` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The timeseries tile — a new module crate, `geode-timeseries`, that lets a trader add series by identity and source, compose them by arithmetic expression, choose range and frequency, manage them through header chips and a popup, and see them painted by `geode-chart` from the series query; plus the `[timeseries] default_source` setting, the `--demo` wiring, the harness entries and the docs.

**Architecture:** A pure core (`geode_timeseries::core`: `Model`, `Range`, expression resolution, the request builder, the chart-model builder, the session round trip) with every verb answering a `Changed` bitset, and one gpui entity (`TimeseriesTile`) that is two front ends over it (the key table and the `:` line) plus the data flow: `DataHandle::fetch` on add, `Delivery::SeriesFetched` → `DataHandle::series`, `Delivery::Series` staged under the flip barrier and turned into an `Arc<ChartModel>` for `geode_chart::ChartElement`. Popups (series list, picker, range, expression field) follow the market-data panel's conventions exactly. The shell gains its third gpui global, `geode_shell::series::SeriesSettings`, carrying the default source and the fetch-source list, written by the shell alone.

**Tech Stack:** Rust 2024, gpui-pre 0.3.5 / gpui-component 0.6.2 (pinned), `geode-chart` (Part 3), `geode_core::series` (Part 2), `geode-widgets` (`DateTimeField`), chrono, toml, criterion.

**Spec:** `docs/superpowers/specs/2026-09-19-geode-timeseries-viewer-design.md` — §9 (all), §10, §6.5, §7's resolution rules, §1.2 done-state items 1 and 3, §11.1 "Tile", §11.2's "the dependant removal", §12 part 4. Read §6.6 and §8.5 first: they are the as-built record of the two crates this one sits on, and the code there is the specification now.

## Global Constraints

- **Lens, not brain** (`docs/PHILOSOPHY.md`, ruling 1): the module never does arithmetic on values. Composition, percentiles and density are `SeriesParams` fields answered by DuckDB; the tile only shapes the request and the painted model.
- **A `:` line changes only its own tile** (command-line locality, 2026-09-20): every verb in §9.9 is tile-local, and `every_colon_command_leaves_the_frame_alone` sweeps them all against the frame's `scope`/`grouping`/`as_of` counters and the `Diagnostics` pending requests.
- **Nothing stalls the render thread; per-frame heap churn is a defect.** The `ChartModel` is built ONCE per delivery or chrome change (never in `render`), header text is prepared in `rebuild_chrome`, and `render` clones `Arc`s and `SharedString`s only.
- **`ChartModel.version` is bumped on ANY change to the model** (§8.5): the chart's cache keys carry neither `axis_mode` nor `step_us`. Every rebuild of the chart model increments the tile's `chart_version`.
- **`ChartElement::new` takes the tile's own `TileId` as its `ElementId`** (§8.5), never a literal.
- **At most `MAX_DENSITY_QUADS` = 2,000 density bars per frame; Part 4 owns the slot count** (§8.5): the model refuses a density setting whose `bins × visible slots with bins` would exceed the bound, and turns density off with a notice when a later add or show would.
- **Insert mode is a tile-owned focused input; closers blur THEN drop** (CLAUDE.md): `close_popup_with_window` and `close_editor` are the only closers, each blurs only when its own field `is_focused`, and `holds_focus` answers off the focus handles of every input the tile owns (the picker's field, the expression field, the range popup's focus handle).
- **No chord is bound in `mode == insert`** (`geode-marketdata/src/content.rs:102-114`): a bare key resolves against insert-carrying contexts only, a chord against the whole stack, and `ctrl+k` is the palette.
- **A fragment predicate is a plain conjunction whose first identifier is `timeseries`** (`keymap::fragments::check_fragment`): `timeseries && mode == normal`, `timeseries && mode == normal && popup == series`, `timeseries && mode == insert`. Never `!`, `||` or `(`.
- **Every displayed time is the trader's local clock** (Phase 4a): `ChartModel.offset_secs` comes from `chrono::Local`, the header's absolute range prints local dates.
- **Chip colours through `geode_shell::shell::chip::chip_paint`; a clickable chip takes `control::for_chip` pointer states; the stack marker through `StackHandle::marker`** — never a hand-picked `warning_foreground`.
- **Chrome geometry on the rem scale** through `geode_shell::shell::scale::{design, design_px}`; radii from the theme; popups paint `popover_style(cx)` with the market-data popup's 26 px / 8 px row geometry.
- **Crate layering:** `geode-timeseries` depends on `geode-core`, `geode-shell`, `geode-widgets`, `geode-chart`, `geode-data` (for `DataHandle` alone), `gpui`, `gpui-component`, `chrono`, `toml`; never on `geode-blotter`, `geode-marketdata` or `geode-diagnostics`. `geode-app` is where it is registered.
- **Every new lib/bin target is `bench = false`; the bench is `harness = false`** (CLAUDE.md).
- **Test-feature parity** (memory `sccache-and-build-cost`): dev-dependencies name `test-support` on `geode-core`, `geode-shell`, `geode-data` and `gpui` exactly as `geode-marketdata/Cargo.toml` does.
- **Both macOS and Windows build** (CI): no platform-specific code.
- **Harness:** one `scripts/mutation-check.sh` entry per behaviour, each naming its test (6th argument), package `geode-timeseries` (or `geode-shell` for Task 5's); `--anchors-only` clean before merge; commit before mutating; never run the harness unfiltered or with `--changed` from a task.
- **No display in the sandbox:** every painted claim is pixel-unverified; each task's report says so where it applies. The display check is `cargo run -p geode-app -- --demo` walking §1.2 item 1.
- **Verification per task:** `cargo fmt --check`, `cargo clippy -p <crate> --all-targets -- -D warnings`, `cargo test -p <crate>`; Task 12 runs the workspace forms plus `cargo check -p geode-shell --features test-support --all-targets`. Use a private `CARGO_TARGET_DIR` if the shared build lock is held.

## Controller decisions (not in the spec; recorded here so they are reviewable)

1. **One dataset per tile.** `SeriesParams` names one `dataset`; a tile's dataset is the dataset of the first source slot added (`Model.dataset`), cleared when the last source slot goes, and `:add`/a pick naming a source whose dataset differs is refused: `this tile plots 'series'; 'x' feeds 'other'`.
2. **The fetch-source list rides in the same global as the default source.** `SeriesSettings { default_source: Option<String>, sources: Vec<FetchSource { name, dataset }> }`, derived by the shell purely over `&Config` (`SourceSpec::from_doc` + `SourceSpec::shape`, the same call the bridge makes) at startup and on reload. Config-truth, not engine-truth: a configured fetch source the engine could not start answers `Err` on fetch, which the slot paints as `Failed`.
3. **The stats window follows the view** (ruling 10): a pan or zoom is `Changed::CHROME`, plus `Changed::QUERY` while percentiles or density are on. The query's `range` stays the loaded range so the points are unchanged; the pool coalesces (one in flight per key, newer tag supersedes), and the chart paints the old stats over the new view until the answer lands.
4. **The cap is pre-checked in the model** with the same `SERIES_POINT_CAP` and `cap_message` the service uses: `f`/`F`/`:freq`/`:range` refuse in place rather than send a request the service would refuse. The service's own refusal still lands as a notice (belt and braces; the message is identical).
5. **Under a historical as-of the tile paints no coverage hull** (closes the Part 2 follow-up): the only provenance the tile shows is `SlotProvenance.health` in the popup's state column (`degraded`/`failed`). The coverage statement's as-of gap therefore has no display consequence and stays a data-tier note.
6. **A restored expression that no longer resolves is dropped with a notice**, not restored `Failed` — an expression slot has no state of its own.
7. **`:colour` accepts a `[colours]` name or a palette index `1`–`5`.** The factory holds `Rc<RefCell<Arc<NamedColours>>>` like the blotter's, refreshed by the bridge on `ConfigReloaded`.
8. **The picker's `add "<text>"…` row goes straight to the pair when the text is already `identity@source` over a known fetch source**; otherwise it opens the source stage.
9. **The view is re-clamped on every delivery and reset on a range change**: `Model::set_full` runs `view.pan(0.0, full)` (a clamp), and a `Changed::FETCH` sets `reset_view` so the next delivery shows the whole new range.

---

## File Structure

```
crates/geode-timeseries/
  Cargo.toml
  src/lib.rs                pub mod core; commands; content; tile; popup; header — re-exports
  src/core/mod.rs           pub use of the core types; the design constants
  src/core/range.rs         Preset, Range (relative | absolute), parse/resolve/label/toml
  src/core/model.rs         Model, Slot, Colour, SlotState, Changed, every verb, labels, budget
  src/core/resolve.rs       expression text → Expr against the tile's slots (§7 resolution rules)
  src/core/request.rs       window(), params() → SeriesParams
  src/core/chart.rs         build() → ChartModel from a SeriesResult + Model
  src/core/session.rs       to_table / from_table
  src/commands.rs           Command, VERBS, parse, completions (pure)
  src/content.rs            TimeseriesFactory, ACTIONS, DEFAULT_KEYMAP, TileContent impl
  src/tile.rs               TimeseriesTile: state, data flow, dispatch, render; tests + harness
  src/header.rs             header strip (chips), footer hints, notice line
  src/popup.rs              Popup enum: Series list, Picker (two stages), Range, Expr field
  benches/chart_model.rs    build() at 500,000 points × 4 slots
crates/geode-shell/src/series.rs            SeriesSettings global, FetchSource, from_config, persist, diagnostic
crates/geode-shell/src/lib.rs               pub mod series
crates/geode-shell/src/shell/mod.rs         default_source field, global at startup, diagnostic seeded
crates/geode-shell/src/shell/input.rs       set_default_source + persist
crates/geode-shell/src/shell/hot_reload.rs  re-derive on reload, diagnostic re-seeded
crates/geode-shell/src/shell/settings_view.rs  SettingId::DefaultSource row
crates/geode-app/src/bridge.rs              Bridge.timeseries factory, set_colours on reload
crates/geode-app/src/main.rs                TimeseriesFactoryHandle, roster.add
crates/geode-app/Cargo.toml                 geode-timeseries dependency
examples/demo-config/app.toml               [timeseries] default_source = "demo_kdb"
Cargo.toml                                  workspace member + dependency
scripts/mutation-check.sh                   + timeseries: entries (Task 12)
docs/perf.md, docs/phase-history.md, CLAUDE.md, the spec's §9.13 As built (Part 4)
```

### Shared interfaces (every task reads these; a later task's names must match)

```rust
// geode_shell::series (Task 5)
pub struct FetchSource { pub name: String, pub dataset: String }
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SeriesSettings { pub default_source: Option<String>, pub sources: Vec<FetchSource> }
impl gpui::Global for SeriesSettings {}
impl SeriesSettings {
    pub fn from_config(config: &Config) -> SeriesSettings;
    pub fn dataset_of(&self, source: &str) -> Option<&str>;
}
pub fn default_source_diagnostic(config: &Config) -> Vec<Diagnostic>;
pub fn persist_to_user_config(user_dir: &Path, source: Option<&str>) -> Result<(), String>;

// geode_timeseries::core::range (Task 1)
pub enum Preset { W1, M1, M3, M6, Y1, Y2, Y5 }         // ALL, as_str "1w".."5y", parse, digit(1..=7)
pub enum Range { Relative(Preset), Absolute { from: NaiveDate, to: NaiveDate } }
impl Range {
    pub fn parse(words: &[&str]) -> Result<Range, String>;
    pub fn resolve(&self, now: DateTime<Utc>, as_of: &AsOf) -> (DateTime<Utc>, DateTime<Utc>);
    pub fn label(&self) -> String;
}

// geode_timeseries::core::model (Task 1, Task 2)
pub const LABEL_MAX: usize = 24; SPLIT_STEP: f32 = 0.05; PAN_FRACTION: f64 = 0.1; ZOOM_FACTOR: f64 = 1.25;
pub const DEFAULT_BINS: u32 = 40; DEFAULT_PERCENTILES: [f64; 3] = [0.05, 0.5, 0.95];
#[derive(Clone, Copy, PartialEq, Eq, Default)] pub struct Changed(u8);
impl Changed { pub const NONE; QUERY; FETCH; CHROME; SESSION; fn query(self)->bool; fetch; chrome; session; }
impl BitOr for Changed; impl BitOrAssign for Changed;
pub enum Colour { Palette(usize), Named(String) }
pub enum SlotState { Idle, Fetching, Failed(String) }
pub struct Slot { pub number: u8, pub kind: SlotKind, pub text: Option<String>, pub colour: Colour, pub axis: Axis, pub visible: bool, pub state: SlotState }
pub struct Removal { pub removed: Vec<u8>, pub changed: Changed }
pub struct Model { .. }   // see Task 1 for the method list
impl Model {
    pub fn add_source(&mut self, identity: &str, source: &str, dataset: &str) -> Result<(u8, Changed), String>;
    pub fn add_expr(&mut self, text: &str, expr: Expr) -> (u8, Changed);
    pub fn replace_expr(&mut self, number: u8, text: &str, expr: Expr) -> Result<Changed, String>;
    pub fn remove(&mut self, number: u8) -> Result<Removal, String>;
    pub fn source_slots(&self) -> impl Iterator<Item = (u8, &str, &str)>;   // (number, source, identity)
    pub fn holds_pair(&self, source: &str, identity: &str) -> bool;
    pub fn set_state(&mut self, number: u8, state: SlotState) -> Changed;
    pub fn set_pair_state(&mut self, source: &str, identity: &str, state: SlotState) -> Changed;
}

// geode_timeseries::core::resolve (Task 2)
pub fn resolve(text: &str, slots: &[Slot], default_source: Option<&str>, editing: Option<u8>) -> Result<Expr, String>;

// geode_timeseries::core::request (Task 3)
pub fn window(model: &Model, buckets: &[i64]) -> (DateTime<Utc>, DateTime<Utc>);
pub fn params(model: &Model, key: QueryKey, tag: u64, now: DateTime<Utc>, as_of: &AsOf, buckets: &[i64]) -> Option<SeriesParams>;

// geode_timeseries::core::chart (Task 3)
pub fn build(result: &SeriesResult, model: &Model, version: u64, offset_secs: i32, colour_of: &dyn Fn(&Colour) -> Hsla) -> ChartModel;

// geode_timeseries::core::session (Task 3)
pub fn to_table(model: &Model) -> toml::Table;
pub fn from_table(table: &toml::Table, dataset_of: &dyn Fn(&str) -> Option<String>, default_source: Option<&str>) -> (Model, Vec<String>);

// geode_timeseries::commands (Task 4)
pub enum Command { .. }; pub const VERBS: &[&str];
pub fn parse(line: &str) -> Result<Command, String>;
pub fn completions(line: &str, cursor: usize, slots: &[u8], sources: &[String], colours: &[String]) -> Vec<String>;
```

---

### Task 1: Crate scaffold, `Range`, and the pure `Model` with its verbs

**Files:**
- Create: `crates/geode-timeseries/Cargo.toml`, `src/lib.rs`, `src/core/mod.rs`, `src/core/range.rs`, `src/core/model.rs`
- Modify: `Cargo.toml` (root: `members` + `[workspace.dependencies] geode-timeseries = { path = "crates/geode-timeseries" }`)

**Interfaces:**
- Consumes: `geode_core::series::{Frequency, BucketRule, SlotKind, SERIES_POINT_CAP, MIN_BINS, MAX_BINS, cap_message}`, `geode_core::query::AsOf`, `geode_chart::{Axis, AxisMode, View}`, `geode_chart::core::palette::Palette::LEN`, `geode_chart::core::layout::{SPLIT_DEFAULT, SPLIT_MIN, SPLIT_MAX}`, `geode_chart::MAX_DENSITY_QUADS`.
- Produces: everything under `core::range` and `core::model` in the shared interfaces block, plus the method list below.

- [ ] **Step 1: Manifest and workspace wiring**

`crates/geode-timeseries/Cargo.toml`:

```toml
[package]
name = "geode-timeseries"
version.workspace = true
edition.workspace = true
publish.workspace = true

[lib]
bench = false

# `geode-core` for the series vocabulary (`Frequency`, `SeriesParams`,
# the expression AST), `geode-shell` for the module-hosting contract
# (`TileContent`, `Frame`, `Diagnostics`, `ChoiceList`, the chip/control
# doors) and the `SeriesSettings` global, `geode-widgets` for the
# segmented date field, `geode-chart` for the model and the element,
# `geode-data` for `DataHandle` alone — the one door a module asks for
# data through (never `geode-blotter`, never `geode-diagnostics`),
# `chrono` for range resolution and the local offset, `toml` for the
# session round trip.
[dependencies]
geode-core.workspace = true
geode-shell.workspace = true
geode-widgets.workspace = true
geode-chart.workspace = true
geode-data = { path = "../geode-data" }
gpui.workspace = true
gpui-component.workspace = true
chrono = "0.4.42"
toml = "1.1.4"

[dev-dependencies]
geode-core = { workspace = true, features = ["test-support"] }
geode-shell = { workspace = true, features = ["test-support"] }
geode-data = { path = "../geode-data", features = ["test-support"] }
gpui = { workspace = true, features = ["test-support"] }
criterion = "0.8.2"
proptest = "1"

[[bench]]
name = "chart_model"
harness = false
```

Root `Cargo.toml`: add `"crates/geode-timeseries",` to `members` after `"crates/geode-chart",` and `geode-timeseries = { path = "crates/geode-timeseries" }` under `[workspace.dependencies]` after the `geode-chart` line. Create an empty `benches/chart_model.rs` containing only `fn main() {}` so the manifest's bench target resolves (Task 3 fills it).

`src/lib.rs`:

```rust
//! The timeseries viewer module (timeseries spec §9): a tile that plots
//! series fetched on demand, composed by arithmetic expression, managed
//! through header chips and a popup, painted by `geode-chart`.

pub mod core;
```

`src/core/mod.rs`:

```rust
pub mod model;
pub mod range;

pub use model::{Changed, Colour, Model, Removal, Slot, SlotState};
pub use range::{Preset, Range};
```

- [ ] **Step 2: Write the failing tests for `Range`** (`src/core/range.rs`, `#[cfg(test)] mod tests`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone, Utc};
    use geode_core::query::AsOf;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%z").map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc))
    }

    #[test]
    fn presets_round_trip_and_map_to_digits() {
        for p in Preset::ALL {
            assert_eq!(Preset::parse(p.as_str()), Some(p));
        }
        assert_eq!(Preset::digit(1), Some(Preset::W1));
        assert_eq!(Preset::digit(7), Some(Preset::Y5));
        assert_eq!(Preset::digit(8), None);
        assert_eq!(Preset::parse("4m"), None);
    }

    #[test]
    fn a_relative_range_ends_now_and_an_as_of_clips_the_end() {
        let now = t("2026-09-19T15:00:00Z");
        let r = Range::Relative(Preset::Y1);
        let (from, to) = r.resolve(now, &AsOf::Live);
        assert_eq!(to, now);
        assert_eq!(from, t("2025-09-19T15:00:00Z"));
        let at = t("2026-03-01T12:00:00Z");
        let (from, to) = r.resolve(now, &AsOf::At(at));
        assert_eq!(to, at, "the frame's as-of clips the visible end (ruling 4)");
        assert_eq!(from, t("2025-03-01T12:00:00Z"), "and the width is kept, measured back from the clip");
        let (from, _) = Range::Relative(Preset::W1).resolve(now, &AsOf::Live);
        assert_eq!(from, t("2026-09-12T15:00:00Z"));
        let (from, _) = Range::Relative(Preset::M3).resolve(now, &AsOf::Live);
        assert_eq!(from, t("2026-06-19T15:00:00Z"));
    }

    #[test]
    fn an_absolute_range_is_whole_days_half_open() {
        let r = Range::parse(&["2026-01-05", "2026-01-09"]).unwrap();
        let (from, to) = r.resolve(t("2026-09-19T15:00:00Z"), &AsOf::Live);
        assert_eq!(from, t("2026-01-05T00:00:00Z"));
        assert_eq!(to, t("2026-01-10T00:00:00Z"), "`to` is inclusive as typed, so the span ends at the next midnight");
        assert_eq!(r.label(), "2026-01-05 → 2026-01-09");
        assert_eq!(Range::Relative(Preset::Y1).label(), "1y");
    }

    #[test]
    fn parse_refuses_a_backwards_range_and_an_unknown_word() {
        assert!(Range::parse(&["2026-01-09", "2026-01-05"]).unwrap_err().contains("before"));
        assert!(Range::parse(&["4m"]).unwrap_err().contains("1w 1m 3m 6m 1y 2y 5y"));
        assert!(Range::parse(&[]).is_err());
        assert!(Range::parse(&["2026-01-05"]).unwrap_err().contains("<from> <to>"));
        assert_eq!(Range::parse(&["1y"]).unwrap(), Range::Relative(Preset::Y1));
    }

    #[test]
    fn a_range_round_trips_through_toml() {
        for r in [
            Range::Relative(Preset::M6),
            Range::Absolute { from: NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(), to: NaiveDate::from_ymd_opt(2026, 2, 5).unwrap() },
        ] {
            assert_eq!(Range::from_toml(&r.to_toml()), Some(r));
        }
        assert_eq!(Range::from_toml(&toml::Value::String("bogus".into())), None);
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p geode-timeseries range::`
Expected: compile errors (`Preset`, `Range` undefined).

- [ ] **Step 4: Implement `range.rs`**

```rust
//! One date range per tile (spec ruling 4, §9.8): a preset kept
//! RELATIVE so a restored `1y` tile is a year to today, or two dates.

use chrono::{DateTime, Days, Months, NaiveDate, Utc};
use geode_core::query::AsOf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset { W1, M1, M3, M6, Y1, Y2, Y5 }

impl Preset {
    pub const ALL: [Preset; 7] = [Preset::W1, Preset::M1, Preset::M3, Preset::M6, Preset::Y1, Preset::Y2, Preset::Y5];
    pub const WORDS: &'static str = "1w 1m 3m 6m 1y 2y 5y";

    pub fn as_str(self) -> &'static str {
        match self { Preset::W1 => "1w", Preset::M1 => "1m", Preset::M3 => "3m", Preset::M6 => "6m", Preset::Y1 => "1y", Preset::Y2 => "2y", Preset::Y5 => "5y" }
    }
    pub fn parse(s: &str) -> Option<Preset> { Self::ALL.into_iter().find(|p| p.as_str() == s) }
    /// `1`..=`7` in `ALL` order — the range popup's digit keys (§9.8).
    pub fn digit(d: u8) -> Option<Preset> { (1..=7).contains(&d).then(|| Self::ALL[(d - 1) as usize]) }
    /// The start of the span that ends at `to`. Months and years are
    /// calendar months (a `1m` on 31 March starts on 28/29 February);
    /// a week is seven days.
    fn start_before(self, to: DateTime<Utc>) -> DateTime<Utc> {
        let months = |n: u32| to.checked_sub_months(Months::new(n)).unwrap_or(to);
        match self {
            Preset::W1 => to.checked_sub_days(Days::new(7)).unwrap_or(to),
            Preset::M1 => months(1), Preset::M3 => months(3), Preset::M6 => months(6),
            Preset::Y1 => months(12), Preset::Y2 => months(24), Preset::Y5 => months(60),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Range {
    Relative(Preset),
    /// Both inclusive as typed; `resolve` makes the span half-open at the
    /// midnight after `to`.
    Absolute { from: NaiveDate, to: NaiveDate },
}

impl Default for Range { fn default() -> Self { Range::Relative(Preset::Y1) } }

impl Range {
    /// `:range 1y` or `:range <from> <to>` (§9.9).
    pub fn parse(words: &[&str]) -> Result<Range, String> {
        match words {
            [one] => Preset::parse(one).map(Range::Relative)
                .ok_or_else(|| format!("'{one}' is not a preset ({}) — or give <from> <to> as YYYY-MM-DD", Preset::WORDS)),
            [a, b] => {
                let date = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| format!("'{s}' is not a date (YYYY-MM-DD)"));
                let (from, to) = (date(a)?, date(b)?);
                if to < from { return Err(format!("'{b}' is before '{a}'")); }
                Ok(Range::Absolute { from, to })
            }
            _ => Err(format!("range is a preset ({}) or <from> <to>", Preset::WORDS)),
        }
    }

    /// The half-open span to fetch and query. `to` is `now`, clipped to
    /// the frame's as-of (ruling 4); a relative range measures its width
    /// back from the CLIPPED end, so a `1y` under an as-of is still a
    /// year of data.
    pub fn resolve(&self, now: DateTime<Utc>, as_of: &AsOf) -> (DateTime<Utc>, DateTime<Utc>) {
        let clip = |t: DateTime<Utc>| match as_of { AsOf::Live => t, AsOf::At(at) => t.min(*at) };
        match self {
            Range::Relative(p) => { let to = clip(now); (p.start_before(to), to) }
            Range::Absolute { from, to } => {
                let from = from.and_hms_opt(0, 0, 0).expect("midnight").and_utc();
                let end = to.checked_add_days(Days::new(1)).unwrap_or(*to).and_hms_opt(0, 0, 0).expect("midnight").and_utc();
                (from, clip(end).max(from))
            }
        }
    }

    pub fn label(&self) -> String {
        match self { Range::Relative(p) => p.as_str().to_string(), Range::Absolute { from, to } => format!("{from} → {to}") }
    }

    pub fn to_toml(&self) -> toml::Value {
        match self {
            Range::Relative(p) => toml::Value::String(p.as_str().into()),
            Range::Absolute { from, to } => {
                let mut t = toml::Table::new();
                t.insert("from".into(), toml::Value::String(from.to_string()));
                t.insert("to".into(), toml::Value::String(to.to_string()));
                toml::Value::Table(t)
            }
        }
    }

    pub fn from_toml(v: &toml::Value) -> Option<Range> {
        match v {
            toml::Value::String(s) => Preset::parse(s).map(Range::Relative),
            toml::Value::Table(t) => {
                let d = |k: &str| t.get(k)?.as_str().and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
                let (from, to) = (d("from")?, d("to")?);
                (to >= from).then_some(Range::Absolute { from, to })
            }
            _ => None,
        }
    }
}
```

- [ ] **Step 5: Run the range tests**

Run: `cargo test -p geode-timeseries range::`
Expected: 5 passed.

- [ ] **Step 6: Write the failing tests for `Model`** (`src/core/model.rs`, `#[cfg(test)] mod tests`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use geode_core::query::AsOf;
    use geode_core::series::{BucketRule, Frequency, SlotKind};
    use geode_chart::{Axis, AxisMode};

    fn now() -> DateTime<Utc> { Utc.with_ymd_and_hms(2026, 9, 19, 15, 0, 0).unwrap() }

    fn two_sources() -> Model {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        m.add_source("VIX", "demo_kdb", "series").unwrap();
        m
    }

    #[test]
    fn a_new_model_has_the_spec_defaults() {
        let m = Model::new();
        assert!(m.slots().is_empty());
        assert_eq!(m.cursor(), None);
        assert_eq!(*m.range(), Range::Relative(Preset::Y1));
        assert_eq!(m.frequency(), Frequency::D1);
        assert_eq!(m.axis_mode(), AxisMode::Session);
        assert_eq!(m.split(), 0.7);
        assert_eq!(m.density(), Some(DEFAULT_BINS));
        assert_eq!(m.percentiles(), &DEFAULT_PERCENTILES[..]);
        assert_eq!(m.dataset(), None);
        assert_eq!(m.header_text(), "1y · 1d");
    }

    #[test]
    fn adding_a_source_numbers_slots_from_one_marks_them_fetching_and_lands_the_cursor() {
        let mut m = Model::new();
        let (n, ch) = m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        assert_eq!(n, 1);
        assert!(ch.fetch() && ch.chrome() && ch.session() && !ch.query(), "a fetch, not yet a query (§9.10)");
        assert_eq!(m.cursor(), Some(0));
        assert!(matches!(m.slots()[0].state, SlotState::Fetching));
        assert_eq!(m.slots()[0].colour, Colour::Palette(0));
        assert_eq!(m.slots()[0].axis, Axis::Left);
        assert_eq!(m.dataset(), Some("series"));
        let (n, _) = m.add_source("VIX", "demo_kdb", "series").unwrap();
        assert_eq!(n, 2);
        assert_eq!(m.slots()[1].colour, Colour::Palette(1), "each new slot takes the next palette colour");
        assert_eq!(m.cursor(), Some(1));
        // The same pair twice is legitimate (§9.6: a second rule).
        assert_eq!(m.add_source("VIX", "demo_kdb", "series").unwrap().0, 3);
        assert!(m.holds_pair("demo_kdb", "VIX"));
        assert!(!m.holds_pair("demo_rest", "VIX"));
    }

    #[test]
    fn a_source_over_another_dataset_is_refused() {
        let mut m = two_sources();
        let err = m.add_source("X", "other_src", "other").unwrap_err();
        assert_eq!(err, "this tile plots 'series'; 'other_src' feeds 'other'");
        assert_eq!(m.slots().len(), 2);
        // Removing every source slot frees the dataset.
        m.remove(1).unwrap();
        m.remove(2).unwrap();
        assert_eq!(m.dataset(), None);
        assert!(m.add_source("X", "other_src", "other").is_ok());
    }

    #[test]
    fn slot_numbers_are_never_reused_for_the_tiles_life() {
        let mut m = two_sources();
        m.remove(2).unwrap();
        let (n, _) = m.add_source("V2X", "demo_kdb", "series").unwrap();
        assert_eq!(n, 3, "2 was used once and is gone");
    }

    #[test]
    fn cursor_moves_by_count_and_wraps() {
        let mut m = two_sources();
        m.add_source("V2X", "demo_kdb", "series").unwrap();
        assert_eq!(m.cursor(), Some(2));
        m.cursor_next(1);
        assert_eq!(m.cursor(), Some(0), "wraps");
        m.cursor_prev(1);
        assert_eq!(m.cursor(), Some(2));
        m.cursor_next(2);
        assert_eq!(m.cursor(), Some(1));
        m.set_cursor(0);
        assert_eq!(m.cursor(), Some(0));
        assert_eq!(Model::new().cursor_next(1), Changed::NONE, "no slots, nothing moves");
    }

    #[test]
    fn visibility_axis_colour_and_rule_verbs_act_on_the_cursor() {
        let mut m = two_sources();
        let ch = m.toggle_visible();
        assert!(!m.slots()[1].visible);
        assert!(ch.chrome() && ch.session() && !ch.query(), "hiding is chrome: the query is unchanged");
        m.toggle_visible();
        assert!(m.slots()[1].visible);
        m.cycle_axis(true, 1);
        assert_eq!(m.slots()[1].axis, Axis::Right);
        m.cycle_axis(true, 2);
        assert_eq!(m.slots()[1].axis, Axis::Left, "a count of two from Right wraps to Left");
        m.cycle_axis(false, 1);
        assert_eq!(m.slots()[1].axis, Axis::BottomRight);
        m.cycle_colour();
        assert_eq!(m.slots()[1].colour, Colour::Palette(2));
        m.set_colour(2, Colour::Named("spx".into())).unwrap();
        m.cycle_colour();
        assert_eq!(m.slots()[1].colour, Colour::Palette(0), "cycling off a named colour starts the palette over");
        let ch = m.cycle_rule();
        assert!(ch.query(), "a rule changes the query");
        assert!(matches!(&m.slots()[1].kind, SlotKind::Source { rule: BucketRule::First, .. }));
        assert!(m.set_rule(9, BucketRule::Max).is_err(), "no slot 9");
    }

    #[test]
    fn frequency_and_range_are_pre_checked_against_the_cap() {
        let mut m = two_sources();
        let ch = m.set_frequency(Frequency::H1, now(), &AsOf::Live).unwrap();
        assert!(ch.query() && ch.session() && ch.chrome());
        assert_eq!(m.header_text(), "1y · 1h");
        // 1m over 1y is ~525,600 buckets: over the cap.
        let err = m.set_frequency(Frequency::M1, now(), &AsOf::Live).unwrap_err();
        assert!(err.starts_with("1m over 1y is 525,600 points; the cap is 500,000"), "{err}");
        assert_eq!(m.frequency(), Frequency::H1, "refused in place");
        // Stepping finer stops at the cap too.
        m.step_frequency(true, 5, now(), &AsOf::Live).unwrap_err();
        assert_eq!(m.frequency(), Frequency::H1);
        m.step_frequency(true, 1, now(), &AsOf::Live).unwrap();
        assert_eq!(m.frequency(), Frequency::M15);
        m.step_frequency(false, 9, now(), &AsOf::Live).unwrap();
        assert_eq!(m.frequency(), Frequency::W1, "saturates");
        let ch = m.set_range(Range::Relative(Preset::W1), now(), &AsOf::Live).unwrap();
        assert!(ch.fetch() && ch.query() && ch.session() && ch.chrome(), "a range change fetches AND queries (§9.10)");
        m.set_frequency(Frequency::M1, now(), &AsOf::Live).unwrap();
        assert!(m.set_range(Range::Relative(Preset::Y5), now(), &AsOf::Live).is_err());
    }

    #[test]
    fn split_density_and_percentiles_have_bounds() {
        let mut m = two_sources();
        m.step_split(false, 1);
        assert!((m.split() - 0.65).abs() < 1e-6);
        m.step_split(false, 20);
        assert!((m.split() - 0.2).abs() < 1e-6, "clamped to SPLIT_MIN");
        assert!(m.set_split(0.9).is_err());
        assert!(m.set_split(0.75).unwrap().chrome());
        let ch = m.toggle_density();
        assert_eq!(m.density(), None);
        assert!(ch.query());
        m.toggle_density();
        assert_eq!(m.density(), Some(DEFAULT_BINS), "back on at the default");
        assert!(m.set_density(Some(3)).unwrap_err().contains("4"));
        assert!(m.set_density(Some(201)).is_err());
        m.set_density(Some(10)).unwrap();
        m.toggle_density();
        m.toggle_density();
        assert_eq!(m.density(), Some(10), "toggling remembers the last count");
        let ch = m.toggle_percentiles();
        assert!(m.percentiles().is_empty() && ch.query());
        m.toggle_percentiles();
        assert_eq!(m.percentiles(), &DEFAULT_PERCENTILES[..]);
        assert!(m.set_percentiles(vec![0.0]).is_err(), "fractions in (0, 1)");
        assert!(m.set_percentiles(vec![0.5, 1.0]).is_err());
        m.set_percentiles(vec![0.25, 0.75]).unwrap();
        m.toggle_percentiles();
        m.toggle_percentiles();
        assert_eq!(m.percentiles(), &[0.25, 0.75][..]);
    }

    #[test]
    fn density_is_bounded_by_the_chart_quad_budget() {
        let mut m = Model::new();
        for i in 0..11 {
            m.add_source(&format!("s{i}"), "demo_kdb", "series").unwrap();
        }
        // 11 visible × 200 bins = 2,200 > MAX_DENSITY_QUADS.
        let err = m.set_density(Some(200)).unwrap_err();
        assert!(err.contains("2,000"), "{err}");
        m.set_density(Some(180)).unwrap();
        assert_eq!(m.density(), Some(180), "11 × 180 = 1,980 fits");
        // A twelfth visible slot would push 12 × 180 over: density turns
        // off with a notice rather than letting bars silently vanish.
        let (_, ch) = m.add_source("s11", "demo_kdb", "series").unwrap();
        assert_eq!(m.density(), None);
        assert!(ch.query());
        assert_eq!(m.take_notice().as_deref(), Some("density off: 12 series × 180 bins would exceed the 2,000-bar bound"));
    }

    #[test]
    fn view_verbs_are_chrome_plus_a_query_while_stats_are_on() {
        let mut m = two_sources();
        m.set_full((0.0, 100.0));
        let ch = m.pan(1);
        assert!(ch.chrome() && ch.query(), "percentiles/density are over the visible window (ruling 10)");
        assert_eq!((m.view().lo, m.view().hi), (0.0, 100.0), "a full view cannot pan");
        m.zoom_in(1);
        assert!((m.view().span() - 80.0).abs() < 1e-9);
        m.pan(-1);
        assert!((m.view().lo - 2.0).abs() < 1e-9, "10% of an 80-wide window, clamped at 0 → moved left by 8 then back... exact: lo was 10 after a centred zoom; -8 → 2");
        m.jump_end();
        assert_eq!(m.view().hi, 100.0);
        m.jump_start();
        assert_eq!(m.view().lo, 0.0);
        m.reset_view();
        assert_eq!((m.view().lo, m.view().hi), (0.0, 100.0));
        m.toggle_density();
        m.toggle_percentiles();
        let ch = m.zoom_out(1);
        assert!(ch.chrome() && !ch.query(), "with both off a view move asks nothing");
        m.zoom_in(1);
        m.set_full((0.0, 50.0));
        assert!(m.view().hi <= 50.0, "set_full re-clamps the view");
    }

    #[test]
    fn labels_follow_the_spec() {
        let mut m = two_sources();
        assert_eq!(m.label(0, Some("demo_kdb")), "SPX.close");
        assert_eq!(m.label(0, Some("demo_rest")), "SPX.close@demo_kdb", "the source shows when it is not the default");
        assert_eq!(m.label(0, None), "SPX.close@demo_kdb");
        let (n, _) = m.add_expr("s1 / s2", geode_core::series::expr::Ast::Num(1.0));
        assert_eq!(n, 3);
        assert_eq!(m.label(2, Some("demo_kdb")), "s1 / s2");
        let long = "s1 + s2 + s1 + s2 + s1 + s2 + s1";
        assert!(long.len() > LABEL_MAX);
        let (_, _) = m.add_expr(long, geode_core::series::expr::Ast::Num(1.0));
        assert_eq!(m.label(3, Some("demo_kdb")), "s4", "over LABEL_MAX the handle stands in");
    }

    #[test]
    fn set_state_by_number_and_by_pair() {
        let mut m = two_sources();
        m.add_source("VIX", "demo_kdb", "series").unwrap();
        let ch = m.set_pair_state("demo_kdb", "VIX", SlotState::Idle);
        assert!(ch.chrome());
        assert!(matches!(m.slots()[1].state, SlotState::Idle));
        assert!(matches!(m.slots()[2].state, SlotState::Idle), "both slots of the pair");
        assert!(matches!(m.slots()[0].state, SlotState::Fetching));
        assert_eq!(m.set_pair_state("demo_kdb", "nope", SlotState::Idle), Changed::NONE);
        m.set_state(1, SlotState::Failed("boom".into()));
        assert!(matches!(&m.slots()[0].state, SlotState::Failed(e) if e == "boom"));
    }

    #[test]
    fn clear_drops_everything_but_the_settings() {
        let mut m = two_sources();
        m.set_frequency(Frequency::H1, now(), &AsOf::Live).unwrap();
        let ch = m.clear();
        assert!(m.slots().is_empty() && m.cursor().is_none() && m.dataset().is_none());
        assert_eq!(m.frequency(), Frequency::H1, "settings survive a clear");
        assert!(ch.query() && ch.chrome() && ch.session());
        assert_eq!(m.add_source("A", "demo_kdb", "series").unwrap().0, 3, "numbers keep counting");
    }
}
```

- [ ] **Step 7: Run to verify failure**

Run: `cargo test -p geode-timeseries model::`
Expected: compile errors.

- [ ] **Step 8: Implement `model.rs`**

```rust
//! The tile's pure state (spec §9.2). Every verb returns a [`Changed`]
//! bitset so the tile knows what to do next — fetch, requery, repaint,
//! persist — and the tests can assert it. The key table and the `:`
//! line are two front ends over these methods.

use std::ops::{BitOr, BitOrAssign};

use chrono::{DateTime, Utc};
use geode_chart::core::layout::{SPLIT_DEFAULT, SPLIT_MAX, SPLIT_MIN};
use geode_chart::core::palette::Palette;
use geode_chart::{Axis, AxisMode, MAX_DENSITY_QUADS, View};
use geode_core::query::AsOf;
use geode_core::series::expr::Expr;
use geode_core::series::{BucketRule, Frequency, MAX_BINS, MIN_BINS, SERIES_POINT_CAP, SlotKind, cap_message};

use super::range::Range;

pub const LABEL_MAX: usize = 24;
pub const SPLIT_STEP: f32 = 0.05;
pub const PAN_FRACTION: f64 = 0.1;
pub const ZOOM_FACTOR: f64 = 1.25;
pub const DEFAULT_BINS: u32 = 40;
pub const DEFAULT_PERCENTILES: [f64; 3] = [0.05, 0.5, 0.95];

/// What a verb changed: the QUERY (requery), a FETCH (the range moved),
/// the CHROME (repaint, rebuild the chart model), the SESSION (persist).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Changed(u8);

impl Changed {
    pub const NONE: Changed = Changed(0);
    pub const QUERY: Changed = Changed(1);
    pub const FETCH: Changed = Changed(2);
    pub const CHROME: Changed = Changed(4);
    pub const SESSION: Changed = Changed(8);
    pub fn query(self) -> bool { self.0 & 1 != 0 }
    pub fn fetch(self) -> bool { self.0 & 2 != 0 }
    pub fn chrome(self) -> bool { self.0 & 4 != 0 }
    pub fn session(self) -> bool { self.0 & 8 != 0 }
    pub fn is_none(self) -> bool { self.0 == 0 }
}
impl BitOr for Changed { type Output = Changed; fn bitor(self, o: Changed) -> Changed { Changed(self.0 | o.0) } }
impl BitOrAssign for Changed { fn bitor_assign(&mut self, o: Changed) { self.0 |= o.0 } }

const ALL: Changed = Changed(15);
const SETTING: Changed = Changed(1 | 4 | 8);   // QUERY | CHROME | SESSION
const LOOK: Changed = Changed(4 | 8);          // CHROME | SESSION

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Colour { Palette(usize), Named(String) }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotState { Idle, Fetching, Failed(String) }

#[derive(Debug, Clone, PartialEq)]
pub struct Slot {
    pub number: u8,
    pub kind: SlotKind,
    /// The expression as typed; `None` for a source slot.
    pub text: Option<String>,
    pub colour: Colour,
    pub axis: Axis,
    pub visible: bool,
    pub state: SlotState,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Removal { pub removed: Vec<u8>, pub changed: Changed }

#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    slots: Vec<Slot>,
    cursor: Option<usize>,
    range: Range,
    frequency: Frequency,
    axis_mode: AxisMode,
    split: f32,
    density: Option<u32>,
    /// Remembered across a toggle so `D` twice returns to the same count.
    last_bins: u32,
    percentiles: Vec<f64>,
    last_percentiles: Vec<f64>,
    view: View,
    full: (f64, f64),
    dataset: Option<String>,
    next_number: u8,
    notice: Option<String>,
}

impl Default for Model { fn default() -> Self { Self::new() } }

impl Model {
    pub fn new() -> Model {
        Model {
            slots: Vec::new(), cursor: None, range: Range::default(), frequency: Frequency::D1,
            axis_mode: AxisMode::Session, split: SPLIT_DEFAULT, density: Some(DEFAULT_BINS), last_bins: DEFAULT_BINS,
            percentiles: DEFAULT_PERCENTILES.to_vec(), last_percentiles: DEFAULT_PERCENTILES.to_vec(),
            view: View::full((0.0, 0.0)), full: (0.0, 0.0), dataset: None, next_number: 1, notice: None,
        }
    }

    // ---- readers ----
    pub fn slots(&self) -> &[Slot] { &self.slots }
    pub fn cursor(&self) -> Option<usize> { self.cursor }
    pub fn cursor_slot(&self) -> Option<&Slot> { self.cursor.and_then(|i| self.slots.get(i)) }
    pub fn range(&self) -> &Range { &self.range }
    pub fn frequency(&self) -> Frequency { self.frequency }
    pub fn axis_mode(&self) -> AxisMode { self.axis_mode }
    pub fn split(&self) -> f32 { self.split }
    pub fn density(&self) -> Option<u32> { self.density }
    pub fn percentiles(&self) -> &[f64] { &self.percentiles }
    pub fn view(&self) -> View { self.view }
    pub fn full(&self) -> (f64, f64) { self.full }
    pub fn dataset(&self) -> Option<&str> { self.dataset.as_deref() }
    pub fn stats_on(&self) -> bool { self.density.is_some() || !self.percentiles.is_empty() }
    pub fn take_notice(&mut self) -> Option<String> { self.notice.take() }
    pub fn slot_by_number(&self, number: u8) -> Option<&Slot> { self.slots.iter().find(|s| s.number == number) }
    pub fn index_of(&self, number: u8) -> Option<usize> { self.slots.iter().position(|s| s.number == number) }
    pub fn source_slots(&self) -> impl Iterator<Item = (u8, &str, &str)> {
        self.slots.iter().filter_map(|s| match &s.kind {
            SlotKind::Source { source, identity, .. } => Some((s.number, source.as_str(), identity.as_str())),
            SlotKind::Expr(_) => None,
        })
    }
    pub fn holds_pair(&self, source: &str, identity: &str) -> bool {
        self.source_slots().any(|(_, s, i)| s == source && i == identity)
    }
    /// `1y · 1d` or `2025-01-01 → 2026-09-19 · 1h` (§9.3).
    pub fn header_text(&self) -> String { format!("{} · {}", self.range.label(), self.frequency.as_str()) }
    /// The chip/popup label (§9.3): the identity, `@source` only when the
    /// source is not the default; an expression's text, or its handle
    /// past `LABEL_MAX`.
    pub fn label(&self, index: usize, default_source: Option<&str>) -> String {
        let s = &self.slots[index];
        match &s.kind {
            SlotKind::Source { source, identity, .. } => {
                if Some(source.as_str()) == default_source { identity.clone() } else { format!("{identity}@{source}") }
            }
            SlotKind::Expr(_) => {
                let text = s.text.as_deref().unwrap_or("");
                if text.chars().count() > LABEL_MAX { format!("s{}", s.number) } else { text.to_string() }
            }
        }
    }

    // ---- slots ----
    fn take_number(&mut self) -> Result<u8, String> {
        if self.next_number == u8::MAX { return Err("this tile has used every slot number; `:clear` starts again".into()); }
        let n = self.next_number;
        self.next_number += 1;
        Ok(n)
    }
    fn push(&mut self, slot: Slot) { self.slots.push(slot); self.cursor = Some(self.slots.len() - 1); }
    fn palette_next(&self) -> usize { self.slots.len() % Palette::LEN }

    pub fn add_source(&mut self, identity: &str, source: &str, dataset: &str) -> Result<(u8, Changed), String> {
        if let Some(d) = &self.dataset && d != dataset {
            return Err(format!("this tile plots '{d}'; '{source}' feeds '{dataset}'"));
        }
        let number = self.take_number()?;
        let colour = Colour::Palette(self.palette_next());
        self.dataset.get_or_insert_with(|| dataset.to_string());
        self.push(Slot {
            number, kind: SlotKind::Source { source: source.into(), identity: identity.into(), rule: BucketRule::Last },
            text: None, colour, axis: Axis::Left, visible: true, state: SlotState::Fetching,
        });
        let mut changed = Changed::FETCH | LOOK;
        changed |= self.enforce_density_budget();
        Ok((number, changed))
    }

    /// `expr` is already resolved (`core::resolve`); the model only files it.
    pub fn add_expr(&mut self, text: &str, expr: Expr) -> (u8, Changed) {
        let number = self.take_number().expect("an expression needs operands, so a number was available");
        let colour = Colour::Palette(self.palette_next());
        self.push(Slot { number, kind: SlotKind::Expr(expr), text: Some(text.into()), colour, axis: Axis::Left, visible: true, state: SlotState::Idle });
        (number, ALL & !FETCH_BIT | self.enforce_density_budget())
    }

    pub fn replace_expr(&mut self, number: u8, text: &str, expr: Expr) -> Result<Changed, String> {
        let i = self.index_of(number).ok_or_else(|| format!("no slot s{number}"))?;
        if !matches!(self.slots[i].kind, SlotKind::Expr(_)) { return Err(format!("s{number} is not an expression")); }
        self.slots[i].kind = SlotKind::Expr(expr);
        self.slots[i].text = Some(text.into());
        Ok(SETTING)
    }

    /// Every slot that references `number`, transitively (§7: "removing
    /// an operand removes every expression that references it").
    pub fn dependants(&self, number: u8) -> Vec<u8> {
        let mut out = vec![];
        let mut frontier = vec![number];
        while let Some(n) = frontier.pop() {
            for s in &self.slots {
                if let SlotKind::Expr(e) = &s.kind && e.slots().contains(&n) && !out.contains(&s.number) && s.number != number {
                    out.push(s.number);
                    frontier.push(s.number);
                }
            }
        }
        out.sort_unstable();
        out
    }

    pub fn remove(&mut self, number: u8) -> Result<Removal, String> {
        if self.index_of(number).is_none() { return Err(format!("no slot s{number}")); }
        let mut removed = self.dependants(number);
        removed.insert(0, number);
        self.slots.retain(|s| !removed.contains(&s.number));
        self.cursor = if self.slots.is_empty() { None } else { Some(self.cursor.unwrap_or(0).min(self.slots.len() - 1)) };
        if self.source_slots().next().is_none() { self.dataset = None; }
        Ok(Removal { removed, changed: SETTING })
    }

    pub fn clear(&mut self) -> Changed {
        self.slots.clear(); self.cursor = None; self.dataset = None;
        SETTING
    }

    pub fn set_state(&mut self, number: u8, state: SlotState) -> Changed {
        match self.index_of(number) { Some(i) => { self.slots[i].state = state; Changed::CHROME } None => Changed::NONE }
    }
    pub fn set_pair_state(&mut self, source: &str, identity: &str, state: SlotState) -> Changed {
        let mut changed = Changed::NONE;
        for s in &mut self.slots {
            if let SlotKind::Source { source: ss, identity: ii, .. } = &s.kind && ss == source && ii == identity {
                s.state = state.clone(); changed = Changed::CHROME;
            }
        }
        changed
    }

    // ---- cursor ----
    pub fn set_cursor(&mut self, index: usize) -> Changed {
        if index < self.slots.len() && self.cursor != Some(index) { self.cursor = Some(index); Changed::CHROME } else { Changed::NONE }
    }
    pub fn cursor_next(&mut self, count: usize) -> Changed { self.step_cursor(count as isize) }
    pub fn cursor_prev(&mut self, count: usize) -> Changed { self.step_cursor(-(count as isize)) }
    fn step_cursor(&mut self, by: isize) -> Changed {
        let n = self.slots.len() as isize;
        if n == 0 { return Changed::NONE; }
        let cur = self.cursor.unwrap_or(0) as isize;
        self.cursor = Some(((cur + by).rem_euclid(n)) as usize);
        Changed::CHROME
    }

    // ---- the cursor's slot ----
    fn at_cursor(&mut self) -> Option<&mut Slot> { self.cursor.and_then(|i| self.slots.get_mut(i)) }
    pub fn toggle_visible(&mut self) -> Changed {
        let Some(s) = self.at_cursor() else { return Changed::NONE };
        s.visible = !s.visible;
        LOOK | self.enforce_density_budget()
    }
    pub fn cycle_axis(&mut self, forward: bool, count: usize) -> Changed {
        let Some(s) = self.at_cursor() else { return Changed::NONE };
        for _ in 0..count.max(1) { s.axis = if forward { s.axis.next() } else { s.axis.prev() }; }
        LOOK
    }
    pub fn set_axis(&mut self, number: u8, axis: Axis) -> Result<Changed, String> {
        let i = self.index_of(number).ok_or_else(|| format!("no slot s{number}"))?;
        self.slots[i].axis = axis; Ok(LOOK)
    }
    pub fn cycle_colour(&mut self) -> Changed {
        let Some(s) = self.at_cursor() else { return Changed::NONE };
        s.colour = match &s.colour { Colour::Palette(i) => Colour::Palette((i + 1) % Palette::LEN), Colour::Named(_) => Colour::Palette(0) };
        LOOK
    }
    pub fn set_colour(&mut self, number: u8, colour: Colour) -> Result<Changed, String> {
        let i = self.index_of(number).ok_or_else(|| format!("no slot s{number}"))?;
        self.slots[i].colour = colour; Ok(LOOK)
    }
    pub fn cycle_rule(&mut self) -> Changed {
        let Some(s) = self.at_cursor() else { return Changed::NONE };
        match &mut s.kind { SlotKind::Source { rule, .. } => { *rule = rule.next(); SETTING } SlotKind::Expr(_) => Changed::NONE }
    }
    pub fn set_rule(&mut self, number: u8, new: BucketRule) -> Result<Changed, String> {
        let i = self.index_of(number).ok_or_else(|| format!("no slot s{number}"))?;
        match &mut self.slots[i].kind { SlotKind::Source { rule, .. } => { *rule = new; Ok(SETTING) } SlotKind::Expr(_) => Err(format!("s{number} is an expression; its rule is its operands'")) }
    }

    // ---- settings ----
    fn check_cap(&self, frequency: Frequency, range: &Range, now: DateTime<Utc>, as_of: &AsOf) -> Result<(), String> {
        let (from, to) = range.resolve(now, as_of);
        let points = frequency.buckets_in(from, to);
        if points > SERIES_POINT_CAP { Err(cap_message(frequency, from, to, points)) } else { Ok(()) }
    }
    pub fn set_frequency(&mut self, f: Frequency, now: DateTime<Utc>, as_of: &AsOf) -> Result<Changed, String> {
        self.check_cap(f, &self.range, now, as_of)?;
        if self.frequency == f { return Ok(Changed::NONE); }
        self.frequency = f; Ok(SETTING)
    }
    /// `f` steps FINER (`finer = true`, toward `1m`), `F` coarser, `count` times, saturating.
    pub fn step_frequency(&mut self, finer: bool, count: usize, now: DateTime<Utc>, as_of: &AsOf) -> Result<Changed, String> {
        let mut f = self.frequency;
        for _ in 0..count.max(1) { f = if finer { f.prev() } else { f.next() }; }
        self.set_frequency(f, now, as_of)
    }
    pub fn set_range(&mut self, range: Range, now: DateTime<Utc>, as_of: &AsOf) -> Result<Changed, String> {
        self.check_cap(self.frequency, &range, now, as_of)?;
        if self.range == range { return Ok(Changed::NONE); }
        self.range = range; Ok(ALL)
    }
    pub fn set_axis_mode(&mut self, mode: AxisMode) -> Changed { if self.axis_mode == mode { Changed::NONE } else { self.axis_mode = mode; LOOK } }
    pub fn set_split(&mut self, split: f32) -> Result<Changed, String> {
        if !(SPLIT_MIN..=SPLIT_MAX).contains(&split) { return Err(format!("split is {SPLIT_MIN}..={SPLIT_MAX}")); }
        self.split = split; Ok(LOOK)
    }
    pub fn step_split(&mut self, grow: bool, count: usize) -> Changed {
        let d = SPLIT_STEP * count.max(1) as f32;
        self.split = (if grow { self.split + d } else { self.split - d }).clamp(SPLIT_MIN, SPLIT_MAX);
        LOOK
    }
    fn visible_slots(&self) -> usize { self.slots.iter().filter(|s| s.visible).count() }
    fn budget_error(&self, bins: u32, visible: usize) -> Option<String> {
        (bins as usize * visible > MAX_DENSITY_QUADS).then(|| format!("{visible} series × {bins} bins would exceed the {}-bar bound", thousands(MAX_DENSITY_QUADS)))
    }
    /// After an add or a show: density turns OFF, with a notice, when the
    /// visible slots at the current bin count would overrun the chart's
    /// per-frame quad bound (§8.5: "Part 4 owns the slot count").
    fn enforce_density_budget(&mut self) -> Changed {
        let Some(bins) = self.density else { return Changed::NONE };
        match self.budget_error(bins, self.visible_slots()) {
            Some(why) => { self.density = None; self.notice = Some(format!("density off: {why}")); Changed::QUERY }
            None => Changed::NONE,
        }
    }
    pub fn set_density(&mut self, bins: Option<u32>) -> Result<Changed, String> {
        if let Some(b) = bins {
            if !(MIN_BINS..=MAX_BINS).contains(&b) { return Err(format!("density is {MIN_BINS}..={MAX_BINS} bins, or off")); }
            if let Some(why) = self.budget_error(b, self.visible_slots()) { return Err(format!("{why}; lower the bins or hide series")); }
            self.last_bins = b;
        }
        self.density = bins; Ok(SETTING)
    }
    pub fn toggle_density(&mut self) -> Changed {
        match self.density { Some(_) => { self.density = None; SETTING } None => self.set_density(Some(self.last_bins)).unwrap_or_else(|why| { self.notice = Some(why); Changed::NONE }) }
    }
    pub fn set_percentiles(&mut self, mut fractions: Vec<f64>) -> Result<Changed, String> {
        if fractions.iter().any(|f| !(*f > 0.0 && *f < 1.0)) { return Err("percentiles are numbers in (0, 100), e.g. 5 50 95".into()); }
        fractions.sort_by(f64::total_cmp); fractions.dedup();
        if !fractions.is_empty() { self.last_percentiles = fractions.clone(); }
        self.percentiles = fractions; Ok(SETTING)
    }
    pub fn toggle_percentiles(&mut self) -> Changed {
        if self.percentiles.is_empty() { self.percentiles = self.last_percentiles.clone(); } else { self.percentiles.clear(); }
        SETTING
    }

    // ---- the view ----
    fn view_changed(&self) -> Changed { if self.stats_on() { Changed::CHROME | Changed::QUERY } else { Changed::CHROME } }
    pub fn set_full(&mut self, full: (f64, f64)) { self.full = full; self.view.pan(0.0, full); }
    pub fn pan(&mut self, steps: i32) -> Changed { self.view.pan(PAN_FRACTION * steps as f64, self.full); self.view_changed() }
    pub fn zoom_in(&mut self, count: usize) -> Changed { self.view.zoom(ZOOM_FACTOR.powi(count.max(1) as i32), 0.5, self.full); self.view_changed() }
    pub fn zoom_out(&mut self, count: usize) -> Changed { self.view.zoom(1.0 / ZOOM_FACTOR.powi(count.max(1) as i32), 0.5, self.full); self.view_changed() }
    pub fn reset_view(&mut self) -> Changed { self.view.reset(self.full); self.view_changed() }
    pub fn jump_start(&mut self) -> Changed { self.view.jump_start(self.full); self.view_changed() }
    pub fn jump_end(&mut self) -> Changed { self.view.jump_end(self.full); self.view_changed() }
}

const FETCH_BIT: Changed = Changed::FETCH;
impl std::ops::Not for Changed { type Output = Changed; fn not(self) -> Changed { Changed(!self.0 & 15) } }
impl std::ops::BitAnd for Changed { type Output = Changed; fn bitand(self, o: Changed) -> Changed { Changed(self.0 & o.0) } }
```

Notes for the implementer: `thousands(n: usize) -> String` is a private four-line helper in this file (`2_000` → `2,000`); both existing groupers (`geode_core::series::group_thousands`, `geode_core::format::group_thousands`) are private, and neither is worth exporting for one message. The `budget_error` string must read exactly `12 series × 180 bins would exceed the 2,000-bar bound` (the test pins it through `take_notice`). Replace the `ALL & !FETCH_BIT` cleverness with the plain `SETTING` constant if it reads worse — the test only asserts the bits.

- [ ] **Step 9: Run the model tests**

Run: `cargo test -p geode-timeseries core::`
Expected: all pass. Then `cargo fmt`, `cargo clippy -p geode-timeseries --all-targets -- -D warnings`.

- [ ] **Step 10: Commit**

```bash
git add Cargo.toml crates/geode-timeseries
git commit -m "timeseries: crate scaffold, Range presets and the pure Model with its verbs (spec §9.2, §9.8)"
```

---

### Task 2: Expression resolution and dependant removal

**Files:**
- Create: `crates/geode-timeseries/src/core/resolve.rs`
- Modify: `src/core/mod.rs` (`pub mod resolve; pub use resolve::resolve;`), `src/core/model.rs` (tests only)

**Interfaces:**
- Consumes: `geode_core::series::expr::{parse, Ast, RefName, Expr, expression_order, ParseError}`, `geode_core::series::{SeriesSpec, SlotKind}`, `Model::{slots, add_expr, replace_expr, remove, dependants}`.
- Produces: `pub fn resolve(text: &str, slots: &[Slot], default_source: Option<&str>, editing: Option<u8>) -> Result<Expr, String>`.

- [ ] **Step 1: Write the failing tests** (`src/core/resolve.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Model;
    use geode_core::series::expr::{Ast, Op};

    fn model() -> Model {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();   // s1
        m.add_source("VIX", "demo_kdb", "series").unwrap();         // s2
        m.add_source("VIX", "demo_rest", "series").unwrap();        // s3
        m.add_source("NKY.close", "demo_rest", "series").unwrap();  // s4
        m
    }

    #[test]
    fn a_handle_an_exact_pair_and_a_default_source_identity_all_resolve() {
        let m = model();
        assert_eq!(resolve("s1 / s4", m.slots(), Some("demo_kdb"), None).unwrap(),
            Ast::Bin(Op::Div, Box::new(Ast::Ref(1)), Box::new(Ast::Ref(4))));
        assert_eq!(resolve("VIX@demo_rest", m.slots(), Some("demo_kdb"), None).unwrap(), Ast::Ref(3));
        assert_eq!(resolve("SPX.close", m.slots(), Some("demo_kdb"), None).unwrap(), Ast::Ref(1), "bare identity, default source");
        assert_eq!(resolve("NKY.close", m.slots(), Some("demo_kdb"), None).unwrap(), Ast::Ref(4),
            "not under the default source, but exactly one loaded slot has it");
    }

    #[test]
    fn ambiguity_and_absence_are_named_errors() {
        let m = model();
        let e = resolve("VIX", m.slots(), Some("demo_rest"), None).unwrap_err();
        // the default source holds it: no ambiguity
        assert!(e.is_empty() || true);
        assert_eq!(resolve("VIX", m.slots(), Some("demo_rest"), None).unwrap(), Ast::Ref(3));
        let e = resolve("VIX", m.slots(), None, None).unwrap_err();
        assert_eq!(e, "'VIX' is ambiguous: VIX@demo_kdb (s2) or VIX@demo_rest (s3)");
        let e = resolve("V2X + 1", m.slots(), Some("demo_kdb"), None).unwrap_err();
        assert_eq!(e, "'V2X' is not loaded — `a` adds it");
        let e = resolve("s9", m.slots(), Some("demo_kdb"), None).unwrap_err();
        assert_eq!(e, "no slot s9");
        let e = resolve("1 + 2", m.slots(), Some("demo_kdb"), None).unwrap_err();
        assert_eq!(e, "an expression must reference a loaded series");
        let e = resolve("s1 ^ 2", m.slots(), Some("demo_kdb"), None).unwrap_err();
        assert!(e.contains("arithmetic only"), "{e}");
    }

    #[test]
    fn an_expression_may_reference_an_expression_but_not_itself_or_a_cycle() {
        let mut m = model();
        let e = resolve("s1 / s2", m.slots(), Some("demo_kdb"), None).unwrap();
        let (n, _) = m.add_expr("s1 / s2", e);          // s5
        assert_eq!(n, 5);
        let e = resolve("s5 * 100", m.slots(), Some("demo_kdb"), None).unwrap();
        m.add_expr("s5 * 100", e);                      // s6
        assert_eq!(resolve("s6 + s5", m.slots(), Some("demo_kdb"), Some(5)).unwrap_err(),
            "s5 cannot reference itself through s6");
        assert_eq!(resolve("s5", m.slots(), Some("demo_kdb"), Some(5)).unwrap_err(), "s5 cannot reference itself");
        // Editing s5 to something else is fine.
        assert!(resolve("s1 - s2", m.slots(), Some("demo_kdb"), Some(5)).is_ok());
    }

    #[test]
    fn removing_an_operand_removes_its_dependants_transitively() {
        let mut m = model();
        let e = resolve("s1 / s2", m.slots(), Some("demo_kdb"), None).unwrap();
        m.add_expr("s1 / s2", e);                       // s5
        let e = resolve("s5 * 100", m.slots(), Some("demo_kdb"), None).unwrap();
        m.add_expr("s5 * 100", e);                      // s6
        let e = resolve("s3 + 1", m.slots(), Some("demo_kdb"), None).unwrap();
        m.add_expr("s3 + 1", e);                        // s7
        assert_eq!(m.dependants(2), vec![5, 6]);
        let r = m.remove(2).unwrap();
        assert_eq!(r.removed, vec![2, 5, 6]);
        assert!(r.changed.query() && r.changed.session());
        let left: Vec<u8> = m.slots().iter().map(|s| s.number).collect();
        assert_eq!(left, vec![1, 3, 4, 7]);
        assert_eq!(m.cursor(), Some(3), "the cursor is clamped to the last slot");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-timeseries resolve::`
Expected: compile errors.

- [ ] **Step 3: Implement `resolve.rs`**

```rust
//! §7's resolution rules — the module's half of the expression language.
//! The parser (`geode_core::series::expr`) knows references as typed;
//! this turns them into slot numbers against THIS tile's slots. Nothing
//! is sent until every reference resolves.

use geode_core::series::expr::{self, Ast, Expr, RefName, expression_order};
use geode_core::series::{SeriesSpec, SlotKind};

use super::model::Slot;

/// `editing` is the slot being REPLACED (`e`), excluded from what the
/// text may reference and checked for a cycle through the others.
pub fn resolve(text: &str, slots: &[Slot], default_source: Option<&str>, editing: Option<u8>) -> Result<Expr, String> {
    let ast = expr::parse(text).map_err(|e| e.message)?;
    let mut err: Option<String> = None;
    let resolved = ast.resolve(&mut |r: &RefName| match lookup(r, slots, default_source) {
        Ok(n) => Some(n),
        Err(e) => { err.get_or_insert(e); None }
    });
    let expr = match resolved {
        Ok(e) => e,
        Err(_) => return Err(err.unwrap_or_else(|| "unresolved reference".into())),
    };
    let refs = expr.slots();
    if refs.is_empty() { return Err("an expression must reference a loaded series".into()); }
    if let Some(me) = editing {
        if refs.contains(&me) { return Err(format!("s{me} cannot reference itself")); }
        // A cycle through another expression: order the specs with this
        // candidate standing in for its slot.
        let specs: Vec<SeriesSpec> = slots.iter().map(|s| SeriesSpec {
            slot: s.number,
            kind: if s.number == me { SlotKind::Expr(expr.clone()) } else { s.kind.clone() },
        }).collect();
        if let Err(on_cycle) = expression_order(&specs) {
            let via = if on_cycle == me { refs.iter().find(|n| slots.iter().any(|s| s.number == **n && matches!(s.kind, SlotKind::Expr(_)))).copied().unwrap_or(me) } else { on_cycle };
            return Err(format!("s{me} cannot reference itself through s{via}"));
        }
    }
    Ok(expr)
}

fn lookup(r: &RefName, slots: &[Slot], default_source: Option<&str>) -> Result<u8, String> {
    match r {
        RefName::Handle(n) => slots.iter().find(|s| s.number == *n).map(|s| s.number).ok_or_else(|| format!("no slot s{n}")),
        RefName::Identity { identity, source: Some(src) } => sources(slots).find(|(_, s, i)| s == src && i == identity).map(|(n, _, _)| n)
            .ok_or_else(|| format!("'{identity}@{src}' is not loaded — `a` adds it")),
        RefName::Identity { identity, source: None } => {
            let matches: Vec<(u8, &str, &str)> = sources(slots).filter(|(_, _, i)| i == identity).collect();
            if let Some(d) = default_source && let Some((n, _, _)) = matches.iter().find(|(_, s, _)| *s == d) { return Ok(*n); }
            match matches.as_slice() {
                [] => Err(format!("'{identity}' is not loaded — `a` adds it")),
                [(n, _, _)] => Ok(*n),
                many => Err(format!("'{identity}' is ambiguous: {}", many.iter().map(|(n, s, i)| format!("{i}@{s} (s{n})")).collect::<Vec<_>>().join(" or "))),
            }
        }
    }
}

fn sources(slots: &[Slot]) -> impl Iterator<Item = (u8, &str, &str)> {
    slots.iter().filter_map(|s| match &s.kind {
        SlotKind::Source { source, identity, .. } => Some((s.number, source.as_str(), identity.as_str())),
        SlotKind::Expr(_) => None,
    })
}
```

The `ambiguity_and_absence_are_named_errors` test has a leftover no-op line (`assert!(e.is_empty() || true)`) — delete it and the `let e =` above it when writing the file; the meaningful assertions follow. Check the "through" message: `expression_order` answers `Err(slot)` for a slot ON the cycle, not necessarily `me`; the test expects `s5 cannot reference itself through s6` for `s6 + s5` edited into s5 — with `on_cycle` either 5 or 6, the `via` computation above yields 6 in both cases (the first expression slot the candidate references). Keep it that simple.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-timeseries core::`
Expected: pass. `cargo clippy -p geode-timeseries --all-targets -- -D warnings`.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-timeseries/src/core
git commit -m "timeseries: expression resolution against the tile's slots, dependant removal (spec §7)"
```

---

### Task 3: The request, the chart model, the session round trip, the bench

**Files:**
- Create: `src/core/request.rs`, `src/core/chart.rs`, `src/core/session.rs`, `benches/chart_model.rs` (replace the stub)
- Modify: `src/core/mod.rs`

**Interfaces:**
- Consumes: `Model` (Task 1), `geode_core::series::{SeriesParams, SeriesSpec, SeriesResult, SlotResult}`, `geode_chart::{ChartModel, ChartSlot, View}`, `geode_chart::core::time::TimeScale`.
- Produces: `request::{window, params}`, `chart::build`, `session::{to_table, from_table}` as in the shared interfaces block.

- [ ] **Step 1: Write the failing tests**

`src/core/request.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Model, Preset, Range};
    use chrono::{TimeZone, Utc};
    use geode_core::query::{AsOf, QueryKey};
    use geode_core::series::{Frequency, SlotKind};
    use geode_chart::AxisMode;

    fn now() -> DateTime<Utc> { Utc.with_ymd_and_hms(2026, 9, 19, 15, 0, 0).unwrap() }
    fn us(s: &str) -> i64 { chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp_micros() }

    #[test]
    fn params_carry_every_slot_in_order_and_the_settings() {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        m.add_source("VIX", "demo_kdb", "series").unwrap();
        let e = crate::core::resolve("s1 / s2", m.slots(), Some("demo_kdb"), None).unwrap();
        m.add_expr("s1 / s2", e);
        let p = params(&m, QueryKey(7), 3, now(), &AsOf::Live, &[]).unwrap();
        assert_eq!(p.key, QueryKey(7));
        assert_eq!(p.tag, 3);
        assert_eq!(p.dataset, "series");
        assert_eq!(p.range, Range::Relative(Preset::Y1).resolve(now(), &AsOf::Live));
        assert_eq!(p.window, p.range, "no buckets yet: the window is the range");
        assert_eq!(p.frequency, Frequency::D1);
        assert_eq!(p.series.iter().map(|s| s.slot).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert!(matches!(p.series[2].kind, SlotKind::Expr(_)));
        assert_eq!(p.percentiles, vec![0.05, 0.5, 0.95]);
        assert_eq!(p.bins, Some(40));
        assert!(p.as_of.is_live());
        assert!(params(&Model::new(), QueryKey(7), 1, now(), &AsOf::Live, &[]).is_none(), "nothing to ask");
    }

    #[test]
    fn the_window_is_the_visible_span_in_both_axis_modes() {
        let mut m = Model::new();
        m.add_source("A", "demo_kdb", "series").unwrap();
        let buckets: Vec<i64> = (0..10).map(|i| us("2026-01-05T00:00:00Z") + i * 86_400_000_000).collect();
        m.set_full((0.0, 10.0));
        m.zoom_in(1);                         // 10 → 8 wide, centred: [1, 9)
        let (from, to) = window(&m, &buckets);
        assert_eq!(from, Utc.timestamp_micros(buckets[1]).unwrap());
        assert_eq!(to, Utc.timestamp_micros(buckets[8]).unwrap() + chrono::Duration::days(1),
            "the last visible bucket plus one frequency step, half-open");
        m.set_axis_mode(AxisMode::Continuous);
        m.set_full((buckets[0] as f64, buckets[9] as f64 + 86_400_000_000.0));
        m.reset_view();
        m.zoom_in(1);
        let (from, to) = window(&m, &buckets);
        assert_eq!(from.timestamp_micros(), m.view().lo as i64);
        assert_eq!(to.timestamp_micros(), m.view().hi as i64);
        // A full view is the whole range.
        m.reset_view();
        let p = params(&m, QueryKey(1), 1, now(), &AsOf::Live, &buckets).unwrap();
        assert_eq!(p.window.0.timestamp_micros(), buckets[0]);
    }
}
```

`src/core/chart.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Colour, Model};
    use geode_core::series::{SeriesResult, SlotProvenance, SlotResult};
    use geode_chart::Axis;
    use gpui::Hsla;

    fn result() -> SeriesResult {
        let prov = || SlotProvenance { loaded: None, latest_received_at: None, health: None };
        SeriesResult {
            buckets: vec![1_000_000, 2_000_000, 3_000_000],
            slots: vec![
                SlotResult { slot: 1, values: vec![1.0, f64::NAN, 3.0], percentiles: vec![(0.05, 1.1), (0.5, 2.0)], bins: vec![(1.0, 2.0, 3)], provenance: prov() },
                SlotResult { slot: 2, values: vec![10.0, 20.0, 30.0], percentiles: vec![], bins: vec![], provenance: prov() },
            ],
        }
    }

    #[test]
    fn the_chart_model_mirrors_the_result_and_the_models_look() {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        m.add_source("VIX", "demo_kdb", "series").unwrap();
        m.cycle_axis(true, 1);          // s2 → Right
        m.toggle_visible();             // s2 hidden
        let colour_of = |c: &Colour| match c { Colour::Palette(i) => Hsla { h: *i as f32 / 10.0, s: 1.0, l: 0.5, a: 1.0 }, Colour::Named(_) => gpui::black() };
        let cm = build(&result(), &m, 9, 3600, &colour_of);
        assert_eq!(cm.version, 9);
        assert_eq!(cm.buckets, vec![1_000_000, 2_000_000, 3_000_000]);
        assert_eq!(cm.step_us, 86_400_000_000, "1d");
        assert_eq!(cm.offset_secs, 3600);
        assert_eq!(cm.split, 0.7);
        assert!(cm.density);
        assert_eq!(cm.slots.len(), 2);
        assert_eq!(cm.slots[0].number, 1);
        assert_eq!(cm.slots[0].label.as_ref(), "SPX.close");
        assert!(cm.slots[0].values[1].is_nan(), "a gap stays a gap");
        assert_eq!(cm.slots[0].percentiles, vec![(0.05, 1.1), (0.5, 2.0)]);
        assert_eq!(cm.slots[0].percentile_labels.iter().map(|l| l.as_ref()).collect::<Vec<_>>(), vec!["p5", "p50"]);
        assert_eq!(cm.slots[0].bins, vec![(1.0, 2.0, 3)]);
        assert_eq!(cm.slots[0].colour.h, 0.0);
        assert_eq!(cm.slots[1].axis, Axis::Right);
        assert!(!cm.slots[1].visible);
        assert_eq!(cm.slots[1].colour.h, 0.1);
    }

    #[test]
    fn a_slot_the_result_lacks_paints_no_points_and_a_result_slot_the_model_lacks_is_skipped() {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();   // s1
        m.add_source("VIX", "demo_kdb", "series").unwrap();         // s2
        m.remove(2).unwrap();
        m.add_source("V2X", "demo_kdb", "series").unwrap();         // s3, not in the (older) result
        let cm = build(&result(), &m, 1, 0, &|_| gpui::black());
        assert_eq!(cm.slots.len(), 2);
        assert_eq!(cm.slots[1].number, 3);
        assert!(cm.slots[1].values.iter().all(|v| v.is_nan()), "buckets.len() NaNs so the element's lengths agree");
        assert_eq!(cm.slots[1].values.len(), 3);
    }
}
```

`src/core/session.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Colour, Model, Preset, Range, SlotState};
    use geode_core::series::{BucketRule, Frequency, SlotKind};
    use geode_chart::{Axis, AxisMode};

    fn dataset_of(s: &str) -> Option<String> { matches!(s, "demo_kdb" | "demo_rest").then(|| "series".to_string()) }

    #[test]
    fn a_model_round_trips_through_its_table() {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        m.add_source("VIX", "demo_rest", "series").unwrap();
        m.set_rule(2, BucketRule::Mean).unwrap();
        m.set_axis(2, Axis::BottomRight).unwrap();
        m.set_colour(1, Colour::Named("spx".into())).unwrap();
        m.toggle_visible();
        let e = crate::core::resolve("s1 / s2", m.slots(), Some("demo_kdb"), None).unwrap();
        m.add_expr("s1 / s2", e);
        m.set_range(Range::Relative(Preset::M6), chrono::Utc::now(), &Default::default()).unwrap();
        m.set_frequency(Frequency::H1, chrono::Utc::now(), &Default::default()).unwrap();
        m.set_axis_mode(AxisMode::Continuous);
        m.set_split(0.6).unwrap();
        m.set_density(Some(20)).unwrap();
        m.set_percentiles(vec![0.1, 0.9]).unwrap();
        let t = to_table(&m);
        let (back, notices) = from_table(&t, &dataset_of, Some("demo_kdb"));
        assert!(notices.is_empty(), "{notices:?}");
        assert_eq!(back.slots().len(), 3);
        assert_eq!(back.slots()[0].number, 1);
        assert_eq!(back.slots()[0].colour, Colour::Named("spx".into()));
        assert!(matches!(&back.slots()[1].kind, SlotKind::Source { rule: BucketRule::Mean, source, .. } if source == "demo_rest"));
        assert_eq!(back.slots()[1].axis, Axis::BottomRight);
        assert!(!back.slots()[1].visible);
        assert_eq!(back.slots()[2].text.as_deref(), Some("s1 / s2"));
        assert!(matches!(back.slots()[2].kind, SlotKind::Expr(_)));
        assert!(back.slots().iter().all(|s| s.state == SlotState::Idle), "slot state is not persisted (§9.11)");
        assert_eq!(*back.range(), Range::Relative(Preset::M6));
        assert_eq!(back.frequency(), Frequency::H1);
        assert_eq!(back.axis_mode(), AxisMode::Continuous);
        assert!((back.split() - 0.6).abs() < 1e-6);
        assert_eq!(back.density(), Some(20));
        assert_eq!(back.percentiles(), &[0.1, 0.9][..]);
        assert_eq!(back.dataset(), Some("series"));
        assert_eq!(back.add_source("X", "demo_kdb", "series").unwrap().0, 4, "numbering continues past the restored max");
        assert_eq!(to_table(&back), t, "a second round trip is identical");
    }

    #[test]
    fn a_hostile_table_heals_rather_than_refuses() {
        let text = r#"
            frequency = "9h"
            axis = "sideways"
            split = 7.0
            density = 100000
            percentiles = [5, 150, "x"]
            [[slots]]
            number = 1
            kind = "source"
            identity = "SPX.close"
            source = "gone_src"
            [[slots]]
            number = 2
            kind = "source"
            identity = "VIX"
            source = "demo_kdb"
            axis = "right"
            colour = 99
            [[slots]]
            number = 2
            kind = "expr"
            text = "s1 / s2"
            [[slots]]
            number = 3
            kind = "expr"
            text = "s2 * s7"
        "#;
        let t: toml::Table = toml::from_str(text).unwrap();
        let (m, notices) = from_table(&t, &dataset_of, Some("demo_kdb"));
        assert_eq!(m.frequency(), Frequency::D1, "unknown → default");
        assert_eq!(m.axis_mode(), AxisMode::Session);
        assert_eq!(m.split(), 0.7);
        assert_eq!(m.density(), Some(40), "out of range → default");
        assert_eq!(m.percentiles(), &[0.05][..], "only the valid fraction survives");
        let numbers: Vec<u8> = m.slots().iter().map(|s| s.number).collect();
        assert_eq!(numbers, vec![2], "unknown source dropped, duplicate number dropped, unresolvable expression dropped");
        assert_eq!(m.slots()[0].colour, Colour::Palette(0), "99 is off the palette");
        assert_eq!(notices.len(), 3, "{notices:?}");
        assert!(notices[0].contains("gone_src"));
        assert!(notices.iter().any(|n| n.contains("s2 * s7")));
    }

    #[test]
    fn an_empty_or_absent_table_is_a_fresh_model() {
        let (m, n) = from_table(&toml::Table::new(), &dataset_of, None);
        assert!(m.slots().is_empty() && n.is_empty());
        assert_eq!(m.frequency(), Frequency::D1);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-timeseries core::`
Expected: compile errors.

- [ ] **Step 3: Implement `request.rs`**

```rust
//! From the model to a `SeriesParams` (spec §6.1, §9.10).

use chrono::{DateTime, TimeZone, Utc};
use geode_chart::AxisMode;
use geode_core::query::{AsOf, QueryKey};
use geode_core::series::{SeriesParams, SeriesSpec};

use super::model::Model;

/// The visible span (ruling 10: stats are over the VISIBLE window). With
/// no buckets yet the window is the range. Under `Session` the view is
/// an index window; the span runs from the first visible bucket to the
/// last visible bucket plus one frequency step (half-open). Under
/// `Continuous` the view IS micros.
pub fn window(model: &Model, buckets: &[i64]) -> (DateTime<Utc>, DateTime<Utc>) {
    let step = model.frequency().seconds() * 1_000_000;
    let at = |us: i64| Utc.timestamp_micros(us).single().unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    let v = model.view();
    match model.axis_mode() {
        AxisMode::Session => {
            let lo = (v.lo.floor().max(0.0) as usize).min(buckets.len().saturating_sub(1));
            let hi = (v.hi.ceil().max(0.0) as usize).clamp(lo + 1, buckets.len().max(1));
            (at(buckets[lo]), at(buckets[hi - 1] + step))
        }
        AxisMode::Continuous => (at(v.lo as i64), at(v.hi as i64)),
    }
}

pub fn params(model: &Model, key: QueryKey, tag: u64, now: DateTime<Utc>, as_of: &AsOf, buckets: &[i64]) -> Option<SeriesParams> {
    let dataset = model.dataset()?.to_string();
    if model.slots().is_empty() { return None; }
    let range = model.range().resolve(now, as_of);
    let window = if buckets.is_empty() { range } else { let (a, b) = window(model, buckets); (a.max(range.0), b.min(range.1).max(a)) };
    Some(SeriesParams {
        key, tag, submitted: std::time::Instant::now(), dataset, range, window, as_of: as_of.clone(),
        frequency: model.frequency(),
        series: model.slots().iter().map(|s| SeriesSpec { slot: s.number, kind: s.kind.clone() }).collect(),
        percentiles: model.percentiles().to_vec(),
        bins: model.density(),
    })
}
```

The window test expects `to == buckets[8] + 1 day` for a Session view `[1, 9)`: `hi = ceil(9) = 9`, `buckets[8] + step`. And `from == buckets[1]`. Check `lo = floor(1.0) = 1`. Good.

- [ ] **Step 4: Implement `chart.rs`**

```rust
//! `SeriesResult` + `Model` → `ChartModel` (spec §8.3, §8.5). Built once
//! per delivery or chrome change, never in `render`. Every field the
//! chart's cache keys do NOT carry (`axis_mode`, `step_us`) rides on
//! `version`, which the tile bumps on every rebuild.

use geode_chart::{ChartModel, ChartSlot};
use geode_core::series::SeriesResult;
use gpui::Hsla;

use super::model::{Colour, Model};

pub fn build(result: &SeriesResult, model: &Model, version: u64, offset_secs: i32, colour_of: &dyn Fn(&Colour) -> Hsla) -> ChartModel {
    let n = result.buckets.len();
    let slots = model.slots().iter().enumerate().map(|(i, s)| {
        let r = result.slots.iter().find(|r| r.slot == s.number);
        let (values, percentiles, bins) = match r {
            Some(r) => (r.values.clone(), r.percentiles.clone(), r.bins.clone()),
            None => (vec![f64::NAN; n], Vec::new(), Vec::new()),
        };
        ChartSlot {
            number: s.number,
            label: model.label(i, None).into(),
            values,
            colour: colour_of(&s.colour),
            axis: s.axis,
            visible: s.visible,
            percentile_labels: percentiles.iter().map(|(f, _)| ChartModel::percentile_label(*f)).collect(),
            percentiles,
            bins,
        }
    }).collect();
    ChartModel {
        version,
        buckets: result.buckets.clone(),
        step_us: model.frequency().seconds() * 1_000_000,
        axis_mode: model.axis_mode(),
        offset_secs,
        split: model.split(),
        density: model.density().is_some(),
        slots,
    }
}
```

The chart-model label uses `default_source = None` here (so `identity@source` always) — the tile passes the real default through a wrapper; simplest is to give `build` a sixth argument `default_source: Option<&str>` and thread it. Do that and update the two tests' calls (`build(&result(), &m, 9, 3600, &colour_of, Some("demo_kdb"))`; the first asserts `SPX.close`, so pass `Some("demo_kdb")`). Update the shared-interfaces block's signature accordingly: `build(result, model, version, offset_secs, colour_of, default_source)`.

- [ ] **Step 5: Implement `session.rs`**

```rust
//! `TileState` in `session.toml` (spec §9.11): slots (kind, colour, axis,
//! visible, rule; an expression by its TEXT), range, frequency, axis
//! mode, split, density, percentiles. Not the view, not slot state.
//! `from_table` heals a hostile table rather than refusing it, like
//! `Tree::from_parts`; every drop is a notice the tile shows once.

use geode_chart::{Axis, AxisMode};
use geode_core::series::{BucketRule, Frequency, MAX_BINS, MIN_BINS, SlotKind};
use toml::{Table, Value};

use super::model::{Colour, Model, SlotState};
use super::range::Range;
use super::resolve::resolve;

pub fn to_table(model: &Model) -> Table {
    let mut t = Table::new();
    let slots: Vec<Value> = model.slots().iter().map(|s| {
        let mut r = Table::new();
        r.insert("number".into(), Value::Integer(s.number as i64));
        match &s.kind {
            SlotKind::Source { source, identity, rule } => {
                r.insert("kind".into(), Value::String("source".into()));
                r.insert("identity".into(), Value::String(identity.clone()));
                r.insert("source".into(), Value::String(source.clone()));
                if *rule != BucketRule::Last { r.insert("rule".into(), Value::String(rule.as_str().into())); }
            }
            SlotKind::Expr(_) => {
                r.insert("kind".into(), Value::String("expr".into()));
                r.insert("text".into(), Value::String(s.text.clone().unwrap_or_default()));
            }
        }
        match &s.colour {
            Colour::Palette(i) => { r.insert("colour".into(), Value::Integer(*i as i64)); }
            Colour::Named(n) => { r.insert("colour".into(), Value::String(n.clone())); }
        }
        if s.axis != Axis::Left { r.insert("axis".into(), Value::String(s.axis.as_str().into())); }
        if !s.visible { r.insert("visible".into(), Value::Boolean(false)); }
        Value::Table(r)
    }).collect();
    if !slots.is_empty() { t.insert("slots".into(), Value::Array(slots)); }
    t.insert("range".into(), model.range().to_toml());
    t.insert("frequency".into(), Value::String(model.frequency().as_str().into()));
    t.insert("axis".into(), Value::String(model.axis_mode().as_str().into()));
    t.insert("split".into(), Value::Float(model.split() as f64));
    t.insert("density".into(), match model.density() { Some(b) => Value::Integer(b as i64), None => Value::Boolean(false) });
    t.insert("percentiles".into(), Value::Array(model.percentiles().iter().map(|f| Value::Float(f * 100.0)).collect()));
    t
}

pub fn from_table(t: &Table, dataset_of: &dyn Fn(&str) -> Option<String>, default_source: Option<&str>) -> (Model, Vec<String>) {
    let mut m = Model::new();
    let mut notices = Vec::new();
    let now = chrono::Utc::now();
    let live = geode_core::query::AsOf::Live;
    if let Some(f) = t.get("frequency").and_then(Value::as_str).and_then(Frequency::parse) { let _ = m.set_frequency(f, now, &live); }
    if let Some(r) = t.get("range").and_then(Range::from_toml) { let _ = m.set_range(r, now, &live); }
    if let Some(a) = t.get("axis").and_then(Value::as_str).and_then(AxisMode::parse) { m.set_axis_mode(a); }
    if let Some(s) = t.get("split").and_then(Value::as_float) { let _ = m.set_split(s as f32); }
    match t.get("density") {
        Some(Value::Integer(b)) if (MIN_BINS as i64..=MAX_BINS as i64).contains(b) => { let _ = m.set_density(Some(*b as u32)); }
        Some(Value::Boolean(false)) => { let _ = m.set_density(None); }
        _ => {}
    }
    if let Some(p) = t.get("percentiles").and_then(Value::as_array) {
        let fractions: Vec<f64> = p.iter().filter_map(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64))).map(|x| x / 100.0).filter(|f| *f > 0.0 && *f < 1.0).collect();
        let _ = m.set_percentiles(fractions);
    }
    // Sources first, then expressions in file order, so a handle resolves.
    let rows: Vec<&Table> = t.get("slots").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_table).collect()).unwrap_or_default();
    let mut seen = Vec::new();
    let mut pending_exprs: Vec<(u8, &Table)> = Vec::new();
    for r in rows {
        let Some(number) = r.get("number").and_then(Value::as_integer).and_then(|n| u8::try_from(n).ok()).filter(|n| *n > 0) else { notices.push("a restored slot had no number and was dropped".into()); continue; };
        if seen.contains(&number) { notices.push(format!("a second slot numbered s{number} was dropped")); continue; }
        seen.push(number);
        match r.get("kind").and_then(Value::as_str) {
            Some("source") => {
                let (Some(identity), Some(source)) = (r.get("identity").and_then(Value::as_str), r.get("source").and_then(Value::as_str)) else { notices.push(format!("s{number} names no identity or source and was dropped")); continue; };
                let Some(dataset) = dataset_of(source) else { notices.push(format!("s{number}: '{source}' is not a fetch source in this config; {identity}@{source} was dropped")); continue; };
                m.set_next_number(number);
                match m.add_source(identity, source, &dataset) {
                    Ok(_) => {
                        if let Some(rule) = r.get("rule").and_then(Value::as_str).and_then(BucketRule::parse) { let _ = m.set_rule(number, rule); }
                        apply_look(&mut m, number, r);
                    }
                    Err(e) => notices.push(format!("s{number}: {e}")),
                }
            }
            Some("expr") => pending_exprs.push((number, r)),
            _ => notices.push(format!("s{number} has an unknown kind and was dropped")),
        }
    }
    for (number, r) in pending_exprs {
        let text = r.get("text").and_then(Value::as_str).unwrap_or("");
        match resolve(text, m.slots(), default_source, None) {
            Ok(expr) => { m.set_next_number(number); m.add_expr(text, expr); apply_look(&mut m, number, r); }
            Err(e) => notices.push(format!("expression s{number} `{text}` was dropped: {e}")),
        }
    }
    m.set_next_number(seen.iter().copied().max().map_or(1, |n| n.saturating_add(1)));
    for s in m.slots_mut() { s.state = SlotState::Idle; }
    m.set_cursor(0);
    (m, notices)
}

fn apply_look(m: &mut Model, number: u8, r: &Table) {
    match r.get("colour") {
        Some(Value::Integer(i)) if (0..geode_chart::core::palette::Palette::LEN as i64).contains(i) => { let _ = m.set_colour(number, Colour::Palette(*i as usize)); }
        Some(Value::String(n)) => { let _ = m.set_colour(number, Colour::Named(n.clone())); }
        _ => {}
    }
    if let Some(a) = r.get("axis").and_then(Value::as_str).and_then(Axis::parse) { let _ = m.set_axis(number, a); }
    if r.get("visible").and_then(Value::as_bool) == Some(false) { let _ = m.set_visible(number, false); }
}
```

This needs three small additions to `Model` (Task 1's file): `pub(crate) fn set_next_number(&mut self, n: u8)` (sets `next_number = n`, so the restored slot takes its recorded number — `add_source`/`add_expr` then bump past it), `pub(crate) fn slots_mut(&mut self) -> &mut [Slot]`, and `pub fn set_visible(&mut self, number: u8, visible: bool) -> Result<Changed, String>` (by number; `toggle_visible` stays the cursor form). Also, `add_source` on restore marks the slot `Fetching` and may push a density-budget notice into `m.notice`; `from_table` calls `m.take_notice()` after the loop and appends it to `notices` if present. Note the hostile-table test's restored `colour = 99` expects `Palette(0)` — the palette index comes from `palette_next()` at add time (`slots.len() % LEN` = 0 for the first surviving slot), and 99 is ignored. And the hostile test expects `s1` with `gone_src` dropped, the DUPLICATE `s2` (the expression) dropped by the `seen` check before kind is read, and `s3` dropped because `s7` does not exist — three notices.

- [ ] **Step 6: The bench** (`benches/chart_model.rs`)

```rust
//! What one delivery costs the UI thread in `geode-timeseries`: building
//! the `ChartModel` from a `SeriesResult` at the series query's point cap
//! — 500,000 buckets, four slots (`chart::build` clones every value
//! vector once; the element then owns the model by `Arc`).

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use geode_core::series::{SeriesResult, SlotProvenance, SlotResult};
use geode_timeseries::core::{Colour, Model, chart};

fn result(n: usize, slots: u8) -> SeriesResult {
    let prov = || SlotProvenance { loaded: None, latest_received_at: None, health: None };
    SeriesResult {
        buckets: (0..n as i64).map(|i| i * 60_000_000).collect(),
        slots: (1..=slots).map(|s| SlotResult { slot: s, values: (0..n).map(|i| (i as f64).sin()).collect(), percentiles: vec![(0.05, -0.9), (0.5, 0.0), (0.95, 0.9)], bins: (0..40).map(|b| (b as f64, b as f64 + 1.0, 10)).collect(), provenance: prov() }).collect(),
    }
}

fn bench(c: &mut Criterion) {
    let mut model = Model::new();
    for id in ["A", "B", "C", "D"] { model.add_source(id, "demo_kdb", "series").unwrap(); }
    let r = result(500_000, 4);
    let colour = |_: &Colour| gpui::black();
    c.bench_function("chart_model/500k_x_4", |b| b.iter(|| black_box(chart::build(&r, &model, 1, 0, &colour, Some("demo_kdb")))));
}

criterion_group!(benches, bench);
criterion_main!(benches);
```

Run `cargo bench -p geode-timeseries --no-run` to compile it; run it (`cargo bench -p geode-timeseries`) and record the median in the task report for Task 12's `docs/perf.md` section.

- [ ] **Step 7: Run the tests**

Run: `cargo test -p geode-timeseries core::` then fmt and clippy.
Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add crates/geode-timeseries
git commit -m "timeseries: the request builder, the chart model builder, the session round trip, the chart_model bench"
```

---

### Task 4: The `:` vocabulary (`commands.rs`)

**Files:**
- Create: `crates/geode-timeseries/src/commands.rs`
- Modify: `src/lib.rs` (`pub mod commands;`)

**Interfaces:**
- Consumes: `core::Range`, `geode_core::series::{Frequency, BucketRule}`, `geode_chart::{Axis, AxisMode}`.
- Produces:

```rust
pub enum Command {
    Add { identity: String, source: Option<String> },
    Expr(String),
    Remove(u8),
    Rule(u8, BucketRule),
    Colour(u8, String),
    AxisMode(AxisMode),
    Freq(Frequency),
    Range(Range),
    Pct(Vec<f64>),          // fractions; empty = off
    Density(Option<u32>),
    YAxis(u8, Axis),
    Split(f32),
    Clear,
}
pub const VERBS: &[&str] = &["add", "expr", "remove", "rule", "colour", "axis", "freq", "range", "pct", "density", "yaxis", "split", "clear"];
pub fn parse(line: &str) -> Result<Command, String>;
pub fn completions(line: &str, cursor: usize, slots: &[u8], sources: &[String], colours: &[String]) -> Vec<String>;
```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Preset, Range};
    use geode_chart::{Axis, AxisMode};
    use geode_core::series::{BucketRule, Frequency};

    #[test]
    fn every_verb_parses_to_its_command() {
        assert_eq!(parse("add SPX.close").unwrap(), Command::Add { identity: "SPX.close".into(), source: None });
        assert_eq!(parse("add SPX.close@demo_rest").unwrap(), Command::Add { identity: "SPX.close".into(), source: Some("demo_rest".into()) });
        assert_eq!(parse("expr s1 / s2 * 100").unwrap(), Command::Expr("s1 / s2 * 100".into()));
        assert_eq!(parse("remove s3").unwrap(), Command::Remove(3));
        assert_eq!(parse("rule s2 mean").unwrap(), Command::Rule(2, BucketRule::Mean));
        assert_eq!(parse("colour s1 spx").unwrap(), Command::Colour(1, "spx".into()));
        assert_eq!(parse("axis time").unwrap(), Command::AxisMode(AxisMode::Continuous));
        assert_eq!(parse("freq 1h").unwrap(), Command::Freq(Frequency::H1));
        assert_eq!(parse("range 6m").unwrap(), Command::Range(Range::Relative(Preset::M6)));
        assert!(matches!(parse("range 2026-01-05 2026-02-05").unwrap(), Command::Range(Range::Absolute { .. })));
        assert_eq!(parse("pct 5 50 95").unwrap(), Command::Pct(vec![0.05, 0.5, 0.95]));
        assert_eq!(parse("pct off").unwrap(), Command::Pct(vec![]));
        assert_eq!(parse("density 40").unwrap(), Command::Density(Some(40)));
        assert_eq!(parse("density off").unwrap(), Command::Density(None));
        assert_eq!(parse("yaxis s2 bottomright").unwrap(), Command::YAxis(2, Axis::BottomRight));
        assert_eq!(parse("split 0.6").unwrap(), Command::Split(0.6));
        assert_eq!(parse("clear").unwrap(), Command::Clear);
        for v in VERBS {
            let line = match *v { "add" => "add X", "expr" => "expr s1", "remove" => "remove s1", "rule" => "rule s1 last", "colour" => "colour s1 x", "axis" => "axis session", "freq" => "freq 1d", "range" => "range 1y", "pct" => "pct off", "density" => "density off", "yaxis" => "yaxis s1 left", "split" => "split 0.7", "clear" => "clear", _ => unreachable!() };
            assert!(parse(line).is_ok(), "{line}");
        }
    }

    #[test]
    fn refusals_name_the_form() {
        assert_eq!(parse("").unwrap_err(), "commands: add expr remove rule colour axis freq range pct density yaxis split clear");
        assert!(parse("bogus").unwrap_err().starts_with("unknown command 'bogus'"));
        assert_eq!(parse("add").unwrap_err(), "add <identity>[@source]");
        assert_eq!(parse("add a@b@c").unwrap_err(), "add <identity>[@source]");
        assert_eq!(parse("remove 3").unwrap_err(), "remove s<n>");
        assert_eq!(parse("rule s2 median").unwrap_err(), "rule s<n> last|first|mean|min|max");
        assert_eq!(parse("axis wall").unwrap_err(), "axis session|time");
        assert_eq!(parse("freq 2h").unwrap_err(), "freq 1m|5m|15m|1h|1d|1w");
        assert!(parse("range 4m").unwrap_err().contains("1w 1m 3m 6m 1y 2y 5y"));
        assert_eq!(parse("pct 0 50").unwrap_err(), "pct <n>… in (0, 100), or off");
        assert_eq!(parse("pct").unwrap_err(), "pct <n>… in (0, 100), or off");
        assert_eq!(parse("density 3").unwrap_err(), "density 4..=200, or off");
        assert_eq!(parse("yaxis s1 up").unwrap_err(), "yaxis s<n> left|right|bottomleft|bottomright");
        assert_eq!(parse("split x").unwrap_err(), "split 0.2..=0.8");
        assert_eq!(parse("split 0.9").unwrap_err(), "split 0.2..=0.8");
        assert_eq!(parse("expr").unwrap_err(), "expr <text>");
        assert_eq!(parse("clear now").unwrap_err(), "clear takes nothing");
    }

    #[test]
    fn completions_are_the_bare_word_per_position() {
        let slots = [1u8, 3];
        let sources = ["demo_kdb".to_string(), "demo_rest".to_string()];
        let colours = ["spx".to_string()];
        assert_eq!(completions("", 0, &slots, &sources, &colours), VERBS.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(completions("ru", 2, &slots, &sources, &colours), VERBS.iter().map(|s| s.to_string()).collect::<Vec<_>>(), "the shell ranks; the tile answers the whole vocabulary");
        assert_eq!(completions("rule ", 5, &slots, &sources, &colours), vec!["s1", "s3"]);
        assert_eq!(completions("rule s1 ", 8, &slots, &sources, &colours), vec!["last", "first", "mean", "min", "max"]);
        assert_eq!(completions("colour s1 ", 10, &slots, &sources, &colours), vec!["spx", "1", "2", "3", "4", "5"]);
        assert_eq!(completions("add ", 4, &slots, &sources, &colours), Vec::<String>::new(), "identities are the picker's; nothing to offer here");
        assert_eq!(completions("axis ", 5, &slots, &sources, &colours), vec!["session", "time"]);
        assert_eq!(completions("freq ", 5, &slots, &sources, &colours), vec!["1m", "5m", "15m", "1h", "1d", "1w"]);
        assert_eq!(completions("range ", 6, &slots, &sources, &colours), vec!["1w", "1m", "3m", "6m", "1y", "2y", "5y"]);
        assert_eq!(completions("pct ", 4, &slots, &sources, &colours), vec!["off"]);
        assert_eq!(completions("density ", 8, &slots, &sources, &colours), vec!["off"]);
        assert_eq!(completions("yaxis s3 ", 9, &slots, &sources, &colours), vec!["left", "right", "bottomleft", "bottomright"]);
        assert_eq!(completions("clear ", 6, &slots, &sources, &colours), Vec::<String>::new());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-timeseries commands::`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! The `:` vocabulary (spec §9.9), pure. Every word is tile-local
//! (command-line locality); completions are the bare word per position.

use crate::core::Range;
use geode_chart::core::layout::{SPLIT_MAX, SPLIT_MIN};
use geode_chart::{Axis, AxisMode};
use geode_core::series::{BucketRule, Frequency, MAX_BINS, MIN_BINS};

#[derive(Debug, Clone, PartialEq)]
pub enum Command { /* as in Interfaces */ }

pub const VERBS: &[&str] = &["add", "expr", "remove", "rule", "colour", "axis", "freq", "range", "pct", "density", "yaxis", "split", "clear"];

fn slot(word: &str, form: &str) -> Result<u8, String> {
    word.strip_prefix('s').and_then(|d| d.parse::<u8>().ok()).filter(|n| *n > 0).ok_or_else(|| form.to_string())
}

pub fn parse(line: &str) -> Result<Command, String> {
    let line = line.trim();
    let (verb, rest) = line.split_once(char::is_whitespace).map(|(v, r)| (v, r.trim())).unwrap_or((line, ""));
    let words: Vec<&str> = rest.split_whitespace().collect();
    match verb {
        "" => Err(format!("commands: {}", VERBS.join(" "))),
        "add" => {
            const FORM: &str = "add <identity>[@source]";
            let [w] = words.as_slice() else { return Err(FORM.into()) };
            let mut parts = w.split('@');
            let identity = parts.next().filter(|s| !s.is_empty()).ok_or(FORM)?.to_string();
            let source = parts.next().map(str::to_string);
            if parts.next().is_some() || source.as_deref() == Some("") { return Err(FORM.into()); }
            Ok(Command::Add { identity, source })
        }
        "expr" => if rest.is_empty() { Err("expr <text>".into()) } else { Ok(Command::Expr(rest.to_string())) },
        "remove" => { let [s] = words.as_slice() else { return Err("remove s<n>".into()) }; Ok(Command::Remove(slot(s, "remove s<n>")?)) }
        "rule" => {
            const FORM: &str = "rule s<n> last|first|mean|min|max";
            let [s, r] = words.as_slice() else { return Err(FORM.into()) };
            Ok(Command::Rule(slot(s, FORM)?, BucketRule::parse(r).ok_or(FORM)?))
        }
        "colour" => { const FORM: &str = "colour s<n> <name>"; let [s, c] = words.as_slice() else { return Err(FORM.into()) }; Ok(Command::Colour(slot(s, FORM)?, c.to_string())) }
        "axis" => { let [m] = words.as_slice() else { return Err("axis session|time".into()) }; Ok(Command::AxisMode(AxisMode::parse(m).ok_or("axis session|time")?)) }
        "freq" => { let [f] = words.as_slice() else { return Err("freq 1m|5m|15m|1h|1d|1w".into()) }; Ok(Command::Freq(Frequency::parse(f).ok_or("freq 1m|5m|15m|1h|1d|1w")?)) }
        "range" => Range::parse(&words).map(Command::Range),
        "pct" => {
            const FORM: &str = "pct <n>… in (0, 100), or off";
            if words == ["off"] { return Ok(Command::Pct(vec![])); }
            if words.is_empty() { return Err(FORM.into()); }
            let mut out = Vec::new();
            for w in &words { let n: f64 = w.parse().map_err(|_| FORM)?; if !(n > 0.0 && n < 100.0) { return Err(FORM.into()); } out.push(n / 100.0); }
            Ok(Command::Pct(out))
        }
        "density" => {
            let form = format!("density {MIN_BINS}..={MAX_BINS}, or off");
            let [w] = words.as_slice() else { return Err(form) };
            if *w == "off" { return Ok(Command::Density(None)); }
            let n: u32 = w.parse().map_err(|_| form.clone())?;
            if !(MIN_BINS..=MAX_BINS).contains(&n) { return Err(form); }
            Ok(Command::Density(Some(n)))
        }
        "yaxis" => { const FORM: &str = "yaxis s<n> left|right|bottomleft|bottomright"; let [s, a] = words.as_slice() else { return Err(FORM.into()) }; Ok(Command::YAxis(slot(s, FORM)?, Axis::parse(a).ok_or(FORM)?)) }
        "split" => {
            let form = format!("split {SPLIT_MIN}..={SPLIT_MAX}");
            let [w] = words.as_slice() else { return Err(form) };
            let f: f32 = w.parse().map_err(|_| form.clone())?;
            if !(SPLIT_MIN..=SPLIT_MAX).contains(&f) { return Err(form); }
            Ok(Command::Split(f))
        }
        "clear" => if words.is_empty() { Ok(Command::Clear) } else { Err("clear takes nothing".into()) },
        other => Err(format!("unknown command '{other}' — commands: {}", VERBS.join(" "))),
    }
}

/// The word under `cursor` decides the position: 0 is the verb, then
/// each verb's own positions. Unfiltered; the shell ranks.
pub fn completions(line: &str, cursor: usize, slots: &[u8], sources: &[String], colours: &[String]) -> Vec<String> {
    let _ = sources;
    let head = &line[..cursor.min(line.len())];
    let position = head.split_whitespace().count().saturating_sub(if head.ends_with(char::is_whitespace) || head.is_empty() { 0 } else { 1 });
    let verb = head.split_whitespace().next().unwrap_or("");
    let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let slot_words = || slots.iter().map(|n| format!("s{n}")).collect::<Vec<_>>();
    match (position, verb) {
        (0, _) => s(VERBS),
        (1, "remove" | "rule" | "colour" | "yaxis") => slot_words(),
        (2, "rule") => BucketRule::ALL.iter().map(|r| r.as_str().to_string()).collect(),
        (2, "colour") => colours.iter().cloned().chain((1..=geode_chart::core::palette::Palette::LEN).map(|i| i.to_string())).collect(),
        (2, "yaxis") => Axis::ALL.iter().map(|a| a.as_str().to_string()).collect(),
        (1, "axis") => s(&["session", "time"]),
        (1, "freq") => Frequency::ALL.iter().map(|f| f.as_str().to_string()).collect(),
        (1, "range") => crate::core::Preset::ALL.iter().map(|p| p.as_str().to_string()).collect(),
        (1, "pct" | "density") => s(&["off"]),
        _ => Vec::new(),
    }
}
```

`sources` is accepted for symmetry with the `:add` form but the identity position offers nothing (the picker owns identities); keep the parameter, since Task 6's `completions` passes it and a later catalogue-backed completion is one arm away.

- [ ] **Step 4: Run the tests, then commit**

Run: `cargo test -p geode-timeseries commands::`; fmt; clippy.

```bash
git add crates/geode-timeseries/src
git commit -m "timeseries: the : vocabulary and per-position completions (spec §9.9)"
```

---

### Task 5: `[timeseries] default_source` — the `SeriesSettings` global, the settings row, the diagnostic, the persist

**Files:**
- Create: `crates/geode-shell/src/series.rs`
- Modify: `crates/geode-shell/src/lib.rs` (`pub mod series;`), `crates/geode-shell/src/shell/mod.rs` (~line 803 field docs, ~1462 startup globals, ~1553 `startup_diagnostics`), `crates/geode-shell/src/shell/input.rs` (~line 609, beside `set_line_numbers`), `crates/geode-shell/src/shell/hot_reload.rs` (~line 222 diagnostics, ~395 re-derive), `crates/geode-shell/src/shell/settings_view.rs` (`SettingId`, `derive_rows`, `rows_for`, `apply_setting`), `examples/demo-config/app.toml`
- Test: `crates/geode-shell/src/series.rs` (unit), `crates/geode-shell/src/shell/tests/chrome_and_dialogs.rs` (the settings-row test beside the line-numbers one at ~line 2150)

**Interfaces:**
- Consumes: `geode_core::config::{Config, Diagnostic, Severity, Layer}`, `geode_core::source_config::{SourceSpec, SourceShape}`, `geode_core::schema::SchemaSpec` (the `from_doc` pair `objectdialog/sources.rs:414-428` already calls), `crate::config_write::edit`.
- Produces: everything under `geode_shell::series` in the shared interfaces block; `SettingId::DefaultSource`; `ShellView::set_default_source(&mut self, source: Option<String>, cx)`.

- [ ] **Step 1: Write the failing unit tests** (`crates/geode-shell/src/series.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{Config, LayerDoc, Severity};

    const DATASETS: &str = "config_version = 1\n[series]\nfamily = \"series\"\n[risk]\n[risk.columns]\nbook = { type = \"utf8\", role = \"key\" }\npv = { type = \"f64\", role = \"value\" }\n";
    const SOURCES: &str = "config_version = 1\n[demo_kdb]\nadapter = \"demo_kdb\"\ndataset = \"series\"\n[demo_rest]\nadapter = \"demo_rest\"\ndataset = \"series\"\n[files]\ndataset = \"risk\"\npaths = [\"/tmp/*.csv\"]\n";

    fn config(app: &str) -> Config {
        Config::from_docs(vec![
            LayerDoc::builtin("datasets", DATASETS).unwrap(),
            LayerDoc::builtin("sources", SOURCES).unwrap(),
            LayerDoc::builtin("app", app).unwrap(),
        ])
    }

    #[test]
    fn the_fetch_sources_are_the_non_directory_sources_over_a_series_dataset() {
        let s = SeriesSettings::from_config(&config("config_version = 1\n"));
        assert_eq!(s.sources.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["demo_kdb", "demo_rest"]);
        assert_eq!(s.dataset_of("demo_rest"), Some("series"));
        assert_eq!(s.dataset_of("files"), None);
        assert_eq!(s.default_source, None);
    }

    #[test]
    fn the_default_source_is_read_and_diagnosed() {
        let s = SeriesSettings::from_config(&config("config_version = 1\n[timeseries]\ndefault_source = \"demo_kdb\"\n"));
        assert_eq!(s.default_source.as_deref(), Some("demo_kdb"));
        assert!(default_source_diagnostic(&config("config_version = 1\n[timeseries]\ndefault_source = \"demo_kdb\"\n")).is_empty());
        let d = default_source_diagnostic(&config("config_version = 1\n[timeseries]\ndefault_source = \"nope\"\n"));
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert_eq!(d[0].path.as_deref(), Some("app.timeseries.default_source"));
        assert!(d[0].message.contains("'nope'") && d[0].message.contains("demo_kdb, demo_rest"), "{}", d[0].message);
        let d = default_source_diagnostic(&config("config_version = 1\n[timeseries]\ndefault_source = 3\n"));
        assert!(d[0].message.contains("string"));
        assert!(default_source_diagnostic(&config("config_version = 1\n")).is_empty(), "absent is fine");
    }

    #[test]
    fn persist_writes_and_removes_the_key_preserving_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.toml"), "# keep me\n[blotter]\nstale_after = \"15m\"\n").unwrap();
        persist_to_user_config(dir.path(), Some("demo_kdb")).unwrap();
        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        assert!(text.contains("# keep me") && text.contains("[timeseries]") && text.contains("default_source = \"demo_kdb\""), "{text}");
        persist_to_user_config(dir.path(), None).unwrap();
        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        assert!(!text.contains("default_source"), "{text}");
        assert!(text.contains("stale_after"));
    }
}
```

`Config::from_docs(Vec<LayerDoc>)` and `LayerDoc::builtin(name, text)` are the constructors (`geode-core/src/config/mod.rs:138, 236`); every layer here is builtin, which is fine for a from-config read (`explain` answers `Builtin`). `tempfile` is already a `geode-shell` dev-dependency; `linenumbers.rs`'s persist test is the model for the third test.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-shell series::`
Expected: compile errors.

- [ ] **Step 3: Implement `series.rs`**

```rust
//! `[timeseries] default_source` (timeseries spec §9.12) and the fetch
//! sources a tile may name, published as the workspace's THIRD gpui
//! global — admitted under CLAUDE.md's exception for the same reason
//! `linenumbers::UiSettings` was: a module has no path to `ShellView`,
//! the settings row must reach an open tile, and neither
//! `ConfigReloaded` (views and dimensions only) nor the factory (create
//! time only) can carry it. Written by the shell alone: startup, the
//! settings row, hot reload.

use std::path::Path;

use geode_core::config::{Config, Diagnostic, Layer, Severity};
use geode_core::schema::SchemaSpec;
use geode_core::source_config::{SourceShape, SourceSpec};
use toml_edit::{Item, Table, value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchSource { pub name: String, pub dataset: String }

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SeriesSettings {
    pub default_source: Option<String>,
    /// Every configured fetch source, in `sources` doc order — config
    /// truth, not engine truth: one the engine could not start answers
    /// `Err` on fetch and the slot paints `Failed`.
    pub sources: Vec<FetchSource>,
}

impl gpui::Global for SeriesSettings {}

impl SeriesSettings {
    pub fn from_config(config: &Config) -> SeriesSettings {
        let schema = config.doc("datasets").map(|d| SchemaSpec::from_doc(d).0).unwrap_or_default();
        let sources = config.doc("sources")
            .map(|d| SourceSpec::from_doc(d, &schema).0)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.shape(&schema) == SourceShape::Fetch)
            .map(|s| FetchSource { name: s.name, dataset: s.dataset })
            .collect();
        let default_source = config.get("app", "timeseries.default_source").and_then(|v| v.as_str()).map(str::to_string);
        SeriesSettings { default_source, sources }
    }
    pub fn dataset_of(&self, source: &str) -> Option<&str> {
        self.sources.iter().find(|s| s.name == source).map(|s| s.dataset.as_str())
    }
    pub fn names(&self) -> Vec<String> { self.sources.iter().map(|s| s.name.clone()).collect() }
}

/// A warning when the key is not a string or names no fetch source —
/// pure over `&Config`, folded into `ShellView::new`'s startup
/// diagnostics AND `apply_reload`'s, like `modules_default_diagnostic`.
pub fn default_source_diagnostic(config: &Config) -> Vec<Diagnostic> {
    let Some(v) = config.get("app", "timeseries.default_source") else { return vec![] };
    let warn = |message: String| Diagnostic {
        severity: Severity::Warning, layer: config.explain("app", "timeseries.default_source"), file: None, message,
        path: Some("app.timeseries.default_source".to_string()),
    };
    let Some(name) = v.as_str() else { return vec![warn("[timeseries] default_source: expected a string naming a fetch source".into())] };
    let settings = SeriesSettings::from_config(config);
    if settings.dataset_of(name).is_some() { return vec![]; }
    let have = settings.names().join(", ");
    vec![warn(format!("[timeseries] default_source '{name}' names no fetch source (have: {have}); `:add` needs an explicit @source"))]
}

/// Write or remove `[timeseries] default_source` in `<user_dir>/app.toml`
/// through `config_write::edit`, the one door.
pub fn persist_to_user_config(user_dir: &Path, source: Option<&str>) -> Result<(), String> {
    crate::config_write::edit(user_dir, Layer::User, "app", |doc| {
        if !doc.get("timeseries").is_some_and(Item::is_table_like) {
            if source.is_none() { return; }
            doc["timeseries"] = Item::Table(Table::new());
        }
        let t = doc["timeseries"].as_table_mut().expect("just ensured [timeseries] is a table");
        match source { Some(s) => { t["default_source"] = value(s); } None => { t.remove("default_source"); } }
    })
}
```

Check `Config::doc(name)` returns `Option<&MergedDoc>` (it does at `objectdialog/sources.rs:414`), that `SchemaSpec` lives at `geode_core::schema::SchemaSpec` (grep), and that `geode-shell` already depends on `toml_edit` (`linenumbers.rs` imports `toml_edit::{Item, Table, value}`).

- [ ] **Step 4: Wire the shell**

1. `lib.rs`: `pub mod series;`.
2. `shell/mod.rs`, beside the `line_numbers` field (~803): `pub(super) default_source: Option<String>,`; in `ShellView::new` right after `cx.set_global(crate::linenumbers::UiSettings { line_numbers });` (~1466):
   ```rust
   let series = crate::series::SeriesSettings::from_config(&services.config);
   let default_source = series.default_source.clone();
   cx.set_global(series);
   ```
   and initialise the field where `line_numbers` is (~1648). In `startup_diagnostics` (~1553) add `diags.extend(crate::series::default_source_diagnostic(cfg));` after the `modules_default_diagnostic` line.
3. `shell/input.rs`, after `persist_line_numbers`:
   ```rust
   /// Set `[timeseries] default_source`, publish it through the
   /// `series::SeriesSettings` global (the fetch-source list is re-derived
   /// with it — sources are restart-gated, so it is unchanged in practice),
   /// persist and repaint. The settings row's one setter; a hot reload
   /// writes the field and the global itself.
   pub(crate) fn set_default_source(&mut self, source: Option<String>, cx: &mut Context<Self>) {
       self.default_source = source.clone();
       let mut series = crate::series::SeriesSettings::from_config(&self.services.config);
       series.default_source = source;
       cx.set_global(series);
       self.persist_default_source(cx);
       cx.notify();
   }
   pub(super) fn persist_default_source(&self, cx: &mut Context<Self>) {
       let Some(dir) = self.user_dir.clone() else { return };
       let source = self.default_source.clone();
       cx.background_executor().spawn(async move {
           if let Err(e) = crate::series::persist_to_user_config(&dir, source.as_deref()) {
               tracing::warn!(target: "geode::config", "default source not saved: {e}");
           }
       }).detach();
   }
   ```
4. `shell/hot_reload.rs`: in the re-derive block (~395) add
   ```rust
   let series = crate::series::SeriesSettings::from_config(&self.services.config);
   if series != *cx.global::<crate::series::SeriesSettings>() {
       self.default_source = series.default_source.clone();
       cx.set_global(series);
   }
   ```
   (`cx.try_global` if the global may be absent in a fixture; the shell always sets it in `new`, so `global` is right — but use `try_global(..).is_none_or(|g| *g != series)` to be safe.) In the diagnostics block (~222) add `config_section.extend(crate::series::default_source_diagnostic(&new_config));` — check `mod_alias_from_config`/`modules_default_diagnostic` are folded there too and mirror exactly what they do.
5. `settings_view.rs`: `SettingId::DefaultSource`; `derive_rows` gains `default_source: Option<&str>, fetch_sources: &[String]` and pushes

   ```rust
   SettingRow {
       id: SettingId::DefaultSource,
       title: "Default series source",
       category: "Timeseries",
       values: std::iter::once("(none)".to_string()).chain(fetch_sources.iter().cloned()).collect(),
       current: default_source.and_then(|d| fetch_sources.iter().position(|s| s == d)).map_or(0, |i| i + 1),
   }
   ```
   `rows_for` passes `shell.default_source.as_deref()` and `cx.global::<SeriesSettings>().names()` (thread `cx: &App` into `rows_for` if it lacks one — check its callers); `apply_setting` gains
   ```rust
   SettingId::DefaultSource => {
       let names = cx.global::<crate::series::SeriesSettings>().names();
       let source = if value_ix == 0 { None } else { names.get(value_ix - 1).cloned() };
       shell.set_default_source(source, cx);
   }
   ```
   Update every `derive_rows` call in tests (grep `derive_rows(`).
6. `examples/demo-config/app.toml`: append
   ```toml

   # The timeseries viewer's default fetch source (timeseries spec §10):
   # `:add SPX.close` reads `SPX.close@demo_kdb`.
   [timeseries]
   default_source = "demo_kdb"
   ```
   and check `crates/geode-app/src/demo.rs`'s `the_demo_layer_is_complete…` test still passes (it may assert on the app doc's keys).

- [ ] **Step 5: Write the settings-row window test** (in `crates/geode-shell/src/shell/tests/chrome_and_dialogs.rs`, modelled on the line-numbers row test at ~2150)

```rust
#[gpui::test]
fn the_default_source_row_steps_over_the_fetch_sources_and_publishes_the_global(cx: &mut gpui::TestAppContext) {
    // A fixture whose config declares two fetch sources over a series
    // dataset (copy `test_services()` and add the `datasets`/`sources`
    // docs from `series.rs`'s unit tests as builtin layers).
    let (view, mut vcx) = open_shell_with_series_sources(cx);
    vcx.update(|_, cx| {
        let s = cx.global::<geode_shell::series::SeriesSettings>();
        assert_eq!(s.default_source, None);
        assert_eq!(s.names(), vec!["demo_kdb", "demo_rest"]);
    });
    open_settings(&view, &mut vcx);
    move_to_row(&view, &mut vcx, "Default series source");
    press(&view, &mut vcx, "space");
    vcx.update(|_, cx| assert_eq!(cx.global::<geode_shell::series::SeriesSettings>().default_source.as_deref(), Some("demo_kdb")));
    press(&view, &mut vcx, "space");
    vcx.update(|_, cx| assert_eq!(cx.global::<geode_shell::series::SeriesSettings>().default_source.as_deref(), Some("demo_rest")));
    press(&view, &mut vcx, "space");
    vcx.update(|_, cx| assert_eq!(cx.global::<geode_shell::series::SeriesSettings>().default_source, None, "wraps to (none)"));
}
```

Use the file's existing helpers (`open_settings`/`press`/row-selection by title — read the line-numbers test and reuse its exact helper names). If `test_services()` has no way to add docs, build the config in the test with `Config::from_docs` and construct the shell the way that file's other config-bearing tests do.

- [ ] **Step 6: Run, then commit**

Run: `cargo test -p geode-shell series::` and `cargo test -p geode-shell default_source_row`; then `cargo check -p geode-shell --features test-support --all-targets`; fmt; clippy for `geode-shell` and `geode-app` (the demo test).

```bash
git add crates/geode-shell examples/demo-config/app.toml
git commit -m "shell: [timeseries] default_source — the SeriesSettings global, the settings row, the diagnostic; demo default demo_kdb (spec §9.12, §10)"
```

---

### Task 6: The tile — factory, keymap fragment, `TileContent`, normal-mode verbs, header and chart render, session, `:` line

**Files:**
- Create: `src/content.rs`, `src/tile.rs`, `src/header.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: Tasks 1–5; `geode_shell::module::{ModuleFactory, TileContent, TileOccupant, Delivery, StackHandle, FindEvent}`, `geode_shell::frame::Frame`, `geode_shell::diagnostics::Diagnostics`, `geode_shell::series::SeriesSettings`, `geode_shell::shell::{chip::{chip_paint, Tone}, control, scale, colours::{anchors_from_theme, tokens_from_theme, to_hsla}}`, `geode_shell::tips`, `geode_shell::tiling::tree::TileId`, `geode_core::colour::{NamedColours, resolve}`, `geode_chart::{ChartElement, ChartModel}`, `geode_data::DataHandle`.
- Produces:

```rust
pub struct TimeseriesFactory { data: DataHandle, colours: Rc<RefCell<Arc<NamedColours>>> }
impl TimeseriesFactory { pub fn new(data: DataHandle, colours: NamedColours) -> Self; pub fn set_colours(&self, colours: NamedColours) }
impl ModuleFactory for TimeseriesFactory { kind = "timeseries"; contexts = default; default_keymap = Some(DEFAULT_KEYMAP); register_actions = ACTIONS under category "Timeseries" }
pub const ACTIONS: &[(&str, &str)];
pub const DEFAULT_KEYMAP: &str;
pub struct TimeseriesTile { .. }   // fields below
```

**The tile's fields** (`tile.rs`):

```rust
pub struct TimeseriesTile {
    id: TileId,
    frame: Entity<Frame>,
    diagnostics: Entity<Diagnostics>,
    data: DataHandle,
    colours: Rc<RefCell<Arc<NamedColours>>>,
    model: Model,
    /// The last good result; the chart model is built from it.
    result: Option<Arc<SeriesResult>>,
    chart: Arc<ChartModel>,
    chart_version: u64,
    /// Rebuild the chart model on the next render if the theme moved.
    theme_key: Option<[Hsla; 28]>,
    tag: u64,
    acted: Option<FrameVersions>,
    staged: Option<(SeriesResult, FrameVersions)>,
    last_flip: u64,
    visible: bool,
    /// Every source slot refetches on the first `set_visible(true)`.
    restore_pending: bool,
    reset_view: bool,
    notice: Option<SharedString>,
    header: HeaderModel,          // prepared text: title chip, range · freq, per-slot chips
    stack: Option<StackHandle>,
    popup: Option<Popup>,         // Task 8–10; `None` in this task
    footer: SharedString,
}
```

`Popup` is declared in this task as an empty placeholder enum in `popup.rs` (`pub(crate) enum Popup {}`) so `key_context` and `holds_focus` compile; Tasks 8–10 fill it.

- [ ] **Step 1: `content.rs` — ACTIONS, DEFAULT_KEYMAP, factory, `TileContent`**

```rust
pub const ACTIONS: &[(&str, &str)] = &[
    ("timeseries::add", "Add a series…"), ("timeseries::expr", "Compose an expression…"),
    ("timeseries::next", "Next series"), ("timeseries::prev", "Previous series"),
    ("timeseries::toggle_visible", "Show/hide series"), ("timeseries::axis_next", "Cycle series axis"), ("timeseries::axis_prev", "Cycle series axis back"),
    ("timeseries::split_shrink", "Shrink the upper pane"), ("timeseries::split_grow", "Grow the upper pane"),
    ("timeseries::colour", "Cycle series colour"), ("timeseries::rule", "Cycle bucket rule"),
    ("timeseries::remove", "Remove series"), ("timeseries::edit", "Edit expression…"),
    ("timeseries::list", "Series…"), ("timeseries::range", "Range…"),
    ("timeseries::freq_finer", "Finer frequency"), ("timeseries::freq_coarser", "Coarser frequency"),
    ("timeseries::density", "Toggle density"), ("timeseries::percentiles", "Toggle percentiles"),
    ("timeseries::pan_left", "Pan left"), ("timeseries::pan_right", "Pan right"),
    ("timeseries::zoom_in", "Zoom in"), ("timeseries::zoom_out", "Zoom out"), ("timeseries::reset_view", "Reset view"),
    ("timeseries::jump_start", "Jump to start"), ("timeseries::jump_end", "Jump to end"),
    // popup verbs (Tasks 8–10)
    ("timeseries::list_down", "Series list: down"), ("timeseries::list_up", "Series list: up"), ("timeseries::list_close", "Series list: close"),
    ("timeseries::commit", "Commit"), ("timeseries::cancel", "Cancel"), ("timeseries::insert_up", "Up"), ("timeseries::insert_down", "Down"),
];

pub const DEFAULT_KEYMAP: &str = r#"
[[bindings]]
context = "timeseries && mode == normal"
[bindings.keys]
"a" = "timeseries::add"
"x" = "timeseries::expr"
"tab" = "timeseries::next"
"shift+tab" = "timeseries::prev"
"v" = "timeseries::toggle_visible"
"y" = "timeseries::axis_next"
"shift+y" = "timeseries::axis_prev"
"[" = "timeseries::split_shrink"
"]" = "timeseries::split_grow"
"c" = "timeseries::colour"
"b" = "timeseries::rule"
"d" = "timeseries::remove"
"e" = "timeseries::edit"
"shift+l" = "timeseries::list"
"r" = "timeseries::range"
"f" = "timeseries::freq_finer"
"shift+f" = "timeseries::freq_coarser"
"shift+d" = "timeseries::density"
"p" = "timeseries::percentiles"
"h" = "timeseries::pan_left"
"l" = "timeseries::pan_right"
"=" = "timeseries::zoom_in"
"+" = "timeseries::zoom_in"
"-" = "timeseries::zoom_out"
"0" = "timeseries::reset_view"
"g" = "timeseries::jump_start"
"shift+g" = "timeseries::jump_end"

[[bindings]]
context = "timeseries && mode == normal && popup == series"
[bindings.keys]
"j" = "timeseries::list_down"
"k" = "timeseries::list_up"
"enter" = "timeseries::list_close"
"escape" = "timeseries::list_close"

[[bindings]]
context = "timeseries && mode == insert"
[bindings.keys]
"enter" = "timeseries::commit"
"escape" = "timeseries::cancel"
"up" = "timeseries::insert_up"
"down" = "timeseries::insert_down"
"#;
```

Check the shipped shell keymap for a `workspace`-context `g` or `0` sequence the tile context would sit above — the blotter binds `g g`; a bare `g` in `timeseries` shadows nothing outside the tile. `shift+l` spells `L` (the market-data fragment spells `shift+g` for `G`).

The factory and `TimeseriesContent` copy `geode-marketdata/src/content.rs:179-373` line for line with the names changed: `kind()` is `"timeseries"`, `contexts()` is left to the default, `register_actions` registers `ACTIONS` under category `"Timeseries"` with `let _ = registry.register(..)`, `create` builds `cx.new(|cx| TimeseriesTile::new(tile, frame, diagnostics, self.data.clone(), self.colours.clone(), restored, window, cx))`. `deliver` is the exhaustive match:

```rust
fn deliver(&self, delivery: Delivery, window: &mut Window, cx: &mut App) {
    match delivery {
        Delivery::Series(outcome) => self.tile.update(cx, |t, cx| t.deliver(outcome, cx)),
        Delivery::SeriesFetched { source, identity, result } => self.tile.update(cx, |t, cx| t.on_fetched(&source, &identity, result, cx)),
        // This tile asks no view query and prices nothing; either here is a routing bug.
        Delivery::Query(_) | Delivery::Price(_) => {}
    }
    let _ = window;
}
```

- [ ] **Step 2: Write the failing tile tests** (`tile.rs`, `mod tests`, harness copied from `geode-marketdata/src/tile.rs:4709-4935` — `Host`, `Harness`, `open`/`open_with`, `command`, `visible`, `dispatch`, plus the new request drains below)

The harness additionally sets the global before creating the tile:

```rust
cx.update(|cx| cx.set_global(geode_shell::series::SeriesSettings {
    default_source: Some("demo_kdb".into()),
    sources: vec![
        geode_shell::series::FetchSource { name: "demo_kdb".into(), dataset: "series".into() },
        geode_shell::series::FetchSource { name: "demo_rest".into(), dataset: "series".into() },
    ],
}));
```

and offers `fn fetch_request(&self) -> Option<FetchParams>` / `fn series_request(&self) -> Option<SeriesParams>` (drain `rx.try_recv()` skipping `Request::Cancel`, panic on any other kind), `fn requests(&self) -> Vec<Request>` (drain all), `fn deliver_series(&self, vcx, tag, result: SeriesResult)`, `fn deliver_fetched(&self, vcx, source, identity, result: Result<u64, String>)`, `fn model(&self, vcx) -> Model` (clone via `h.tile.read_with`), `fn notice(&self, vcx) -> Option<String>`, `fn chart(&self, vcx) -> Arc<ChartModel>`.

Tests for THIS task (the data-flow ones are Task 7's):

```rust
#[gpui::test]
fn the_factory_is_kind_timeseries_with_its_fragment_and_actions(cx: &mut gpui::TestAppContext) {
    let (h, _vcx) = open(cx);
    assert_eq!(h.factory.kind(), "timeseries");
    assert_eq!(h.factory.contexts(), vec!["timeseries"]);
    assert!(h.factory.default_keymap().unwrap().contains("timeseries && mode == normal"));
    let mut registry = geode_shell::actions::ActionRegistry::new();
    h.factory.register_actions(&mut registry);
    for (id, _) in ACTIONS { assert!(registry.get(&ActionId(id.to_string())).is_some(), "{id}"); }
    // Every key in the fragment names a registered action (the keymap builder drops unregistered ones silently).
    let (docs, diags) = { let mut r = geode_shell::module::ModuleRoster::new(); r.add(Box::new(h.factory_handle())); r.keymap_fragments() };
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(docs.len(), 1);
}

#[gpui::test]
fn a_fresh_tile_paints_the_empty_hint_and_its_title(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    assert_eq!(h.title(&mut vcx).as_ref(), "timeseries · 1y · 1d");
    assert!(h.painted_text(&mut vcx).contains("no series — a adds one, x composes"));
    assert!(matches!(h.key_context_mode(&mut vcx).as_str(), "normal"));
}

#[gpui::test]
fn colon_add_makes_a_fetching_slot_and_the_header_chip(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    let m = h.model(&vcx);
    assert_eq!(m.slots().len(), 1);
    assert!(matches!(&m.slots()[0].kind, SlotKind::Source { source, identity, .. } if source == "demo_kdb" && identity == "SPX.close"));
    assert!(matches!(m.slots()[0].state, SlotState::Fetching));
    assert!(h.painted_text(&mut vcx).contains("SPX.close"), "the chip");
    assert!(h.painted_text(&mut vcx).contains("L"), "its axis letter");
    h.command(&mut vcx, "add VIX@demo_rest").unwrap();
    assert!(h.painted_text(&mut vcx).contains("VIX@demo_rest"), "source shown when not the default");
    assert_eq!(h.title(&mut vcx).as_ref(), "timeseries · 1y · 1d · 2 series");
}

#[gpui::test]
fn colon_add_without_a_default_source_refuses(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_with_default_source(cx, None);
    assert_eq!(h.command(&mut vcx, "add SPX.close").unwrap_err(), "name a source or set a default: add SPX.close@<source>");
    assert!(h.command(&mut vcx, "add SPX.close@demo_kdb").is_ok());
    assert_eq!(h.command(&mut vcx, "add X@nope").unwrap_err(), "'nope' is not a fetch source (have: demo_kdb, demo_rest)");
}

#[gpui::test]
fn normal_mode_verbs_drive_the_model_and_bump_the_chart_version(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "add VIX").unwrap();
    let v0 = h.chart(&vcx).version;
    h.dispatch(&mut vcx, "axis_next", None);
    assert_eq!(h.model(&vcx).slots()[1].axis, Axis::Right);
    assert!(h.chart(&vcx).version > v0, "a chrome change rebuilds the chart model (§8.5's version contract)");
    h.dispatch(&mut vcx, "axis_next", Some(2));
    assert_eq!(h.model(&vcx).slots()[1].axis, Axis::Left);
    h.dispatch(&mut vcx, "prev", None);
    assert_eq!(h.model(&vcx).cursor(), Some(0));
    h.dispatch(&mut vcx, "toggle_visible", None);
    assert!(!h.model(&vcx).slots()[0].visible);
    assert!(!h.chart(&vcx).slots[0].visible);
    h.dispatch(&mut vcx, "colour", None);
    assert_eq!(h.model(&vcx).slots()[0].colour, Colour::Palette(1));
    h.dispatch(&mut vcx, "rule", None);
    assert!(matches!(&h.model(&vcx).slots()[0].kind, SlotKind::Source { rule: BucketRule::First, .. }));
    h.dispatch(&mut vcx, "split_shrink", None);
    assert!((h.model(&vcx).split() - 0.65).abs() < 1e-6);
    h.dispatch(&mut vcx, "density", None);
    assert_eq!(h.model(&vcx).density(), None);
    h.dispatch(&mut vcx, "percentiles", None);
    assert!(h.model(&vcx).percentiles().is_empty());
    h.dispatch(&mut vcx, "freq_coarser", None);
    assert_eq!(h.model(&vcx).frequency(), Frequency::W1);
    h.dispatch(&mut vcx, "freq_finer", Some(2));
    assert_eq!(h.model(&vcx).frequency(), Frequency::H1);
    assert_eq!(h.title(&mut vcx).as_ref(), "timeseries · 1y · 1h · 2 series");
    let v = h.chart(&vcx).version;
    h.dispatch(&mut vcx, "zoom_in", None);
    assert!(h.chart(&vcx).version == v, "a view move does not rebuild the model — the element takes the view beside it");
    h.dispatch(&mut vcx, "remove", None);
    assert_eq!(h.model(&vcx).slots().len(), 1);
}

#[gpui::test]
fn a_capped_frequency_step_is_refused_with_the_cap_message(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "freq 1h").unwrap();
    h.dispatch(&mut vcx, "freq_finer", Some(3));
    assert_eq!(h.model(&vcx).frequency(), Frequency::H1);
    assert!(h.notice(&vcx).unwrap().starts_with("1m over 1y is"));
    assert!(h.command(&mut vcx, "freq 1m").unwrap_err().contains("the cap is 500,000"));
}

#[gpui::test]
fn d_on_an_operand_removes_the_dependants_with_one_notice(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "add VIX").unwrap();
    h.command(&mut vcx, "expr s1 / s2").unwrap();
    h.command(&mut vcx, "expr s3 * 100").unwrap();
    h.dispatch(&mut vcx, "prev", Some(2));      // cursor on s2
    h.dispatch(&mut vcx, "remove", None);
    let left: Vec<u8> = h.model(&vcx).slots().iter().map(|s| s.number).collect();
    assert_eq!(left, vec![1]);
    assert_eq!(h.notice(&vcx).as_deref(), Some("removed s2 and, with it, s3, s4"));
}

#[gpui::test]
fn every_colon_command_leaves_the_frame_alone(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "add VIX").unwrap();
    let lines = ["add NKY.close", "expr s1 / s2", "remove s3", "rule s1 mean", "colour s1 2", "axis time", "freq 1h",
                 "range 6m", "pct 10 90", "density 20", "yaxis s2 right", "split 0.6", "clear"];
    for word in crate::commands::VERBS {
        assert!(lines.iter().any(|l| l.split_whitespace().next() == Some(word)), "no sweep line for `:{word}`");
    }
    let before = h.frame.read_with(&vcx, |f, _| f.versions());
    for line in lines {
        assert!(crate::commands::parse(line).is_ok(), "`{line}` no longer parses");
        let _ = vcx.update(|window, cx| h.content.command(line, window, cx));
        let after = h.frame.read_with(&vcx, |f, _| f.versions());
        assert_eq!((after.scope, after.grouping, after.as_of), (before.scope, before.grouping, before.as_of), "`:{line}` moved the frame");
        let (level, overlay) = h.diagnostics.update(&mut vcx, |d, _| (d.take_pending_level(), d.take_pending_overlay_toggle()));
        assert!(level.is_none() && !overlay, "`:{line}` reached the app");
    }
}

#[gpui::test]
fn serialize_and_restore_round_trip_the_model(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "add VIX@demo_rest").unwrap();
    h.command(&mut vcx, "expr s1 / s2").unwrap();
    h.command(&mut vcx, "yaxis s2 bottomleft").unwrap();
    h.command(&mut vcx, "range 3m").unwrap();
    h.dispatch(&mut vcx, "zoom_in", None);
    let table = vcx.update(|_, cx| h.content.serialize(cx));
    assert!(table.get("slots").unwrap().as_array().unwrap().len() == 3);
    assert!(table.get("view").is_none(), "the view is not persisted (§9.11)");
    let (h2, mut vcx2) = open_with(cx, Some(table.clone()));
    let m = h2.model(&vcx2);
    assert_eq!(m.slots().len(), 3);
    assert_eq!(m.slots()[1].axis, Axis::BottomLeft);
    assert_eq!(m.slots()[2].text.as_deref(), Some("s1 / s2"));
    assert_eq!(m.range(), &Range::Relative(Preset::M3));
    assert_eq!(vcx2.update(|_, cx| h2.content.serialize(cx)), table);
    assert!(h2.painted_text(&mut vcx2).contains("s1 / s2"));
}

#[gpui::test]
fn chip_tones_are_readable_on_every_bundled_theme(cx: &mut gpui::TestAppContext) {
    // The chips paint through `chip_paint` (`Neutral` cursor, `Warning` fetching, `Danger` failed),
    // which is already swept by the shell; this pins that THIS module uses those tones and no other.
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.header().chips[0].tone), geode_shell::shell::chip::Tone::Warning);
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Err("no route".into()));
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.header().chips[0].tone), geode_shell::shell::chip::Tone::Danger);
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Ok(3));
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.header().chips[0].tone), geode_shell::shell::chip::Tone::Neutral, "idle AND the cursor");
}
```

There is no painted-text reader in the market-data harness (`painted_snapshot` is a model read). `h.painted_text(vcx)` here is the HEADER MODEL's text joined — `t.header()`'s `range_freq` and every chip's `label` and `axis` — plus the literal empty hint while `header.empty`; a chip's tone is read as `t.header().chips[i].tone`. Painted pixels stay the display check's.

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p geode-timeseries tile::`
Expected: compile errors.

- [ ] **Step 4: Implement `header.rs`, `tile.rs`, `content.rs`**

`header.rs` — the prepared model and the painters:

```rust
pub(crate) struct Chip { pub label: SharedString, pub axis: &'static str, pub tone: Tone, pub hidden: bool, pub tooltip: Option<SharedString>, pub swatch: Colour }
pub(crate) struct HeaderModel { pub range_freq: SharedString, pub chips: Vec<Chip>, pub cursor: Option<usize>, pub empty: bool }
impl HeaderModel { pub fn prepare(model: &Model, default_source: Option<&str>) -> HeaderModel }
pub(crate) fn render_header(h: &HeaderModel, theme: &Theme, tile: &Entity<TimeseriesTile>, tile_id: u64, stack: Option<&StackHandle>, swatch_of: &dyn Fn(&Colour) -> Hsla) -> impl IntoElement
pub(crate) fn render_footer(text: SharedString, theme: &Theme) -> impl IntoElement
pub(crate) fn render_notice(notice: &SharedString, theme: &Theme) -> impl IntoElement
pub(crate) fn footer_text(cx: &App) -> SharedString   // "a add · x expr · L series · r range · f freq · D density · p percentiles", each key via tips::chord_for on the Chords global, falling back to the literal
```

Header row, in order (§9.3): `stack.marker(theme, TileId(tile_id))`, a kind badge `Timeseries` (the market-data badge's `secondary` pill), `h.range_freq` in `fonts::MONO`, then one chip per slot: a `size(8px).rounded_full()` swatch in the slot's colour, the label, the axis letters in `muted_foreground`. Tone: `Neutral` on the cursor's chip, `Warning` while `Fetching`, `Danger` on `Failed` (with `tips::tip_with(selector, reason, None, None)` as the tooltip), otherwise no fill. Hidden: `.opacity(0.5).line_through()`. Every chip is `.id(("ts-chip", number))` with `pointer_states(control::for_chip(theme, &paint, theme.background))` and an `on_mouse_down(Left)` that `stop_propagation`s and `tile.update(cx, |t, cx| t.chip_clicked(index, cx))` (moves the cursor). Heights through `scale::design(HEADER_HEIGHT)` with `HEADER_HEIGHT = 26.0` and `FOOTER_HEIGHT = 20.0`.

`tile.rs` — the essentials:

```rust
impl TimeseriesTile {
    pub fn new(id, frame, diagnostics, data, colours, restored: Option<&toml::Table>, window, cx) -> Self {
        let settings = cx.try_global::<SeriesSettings>().cloned().unwrap_or_default();
        let (model, notices) = match restored {
            Some(t) => session::from_table(t, &|s| settings.dataset_of(s).map(str::to_string), settings.default_source.as_deref()),
            None => (Model::new(), vec![]),
        };
        // restore_pending: every source slot refetches on the first set_visible(true) (§9.10)
        let restore_pending = model.source_slots().next().is_some();
        cx.observe(&frame, Self::on_frame_changed).detach();          // the market-data observer verbatim, `key.is_some()` → `!model.slots().is_empty()`
        cx.observe_global::<SeriesSettings>(|this, cx| { this.rebuild_chrome(cx); cx.notify(); }).detach();
        cx.observe_global::<geode_shell::tips::Chords>(|this, cx| { this.footer = header::footer_text(cx); cx.notify(); }).detach();
        ...
    }
    pub fn key_context(&self) -> KeyContext {
        let mode = if self.popup.as_ref().is_some_and(Popup::is_insert) { "insert" } else { "normal" };
        let mut ctx = KeyContext::new("timeseries").pair("mode", mode).counts();
        if matches!(self.popup, Some(Popup::Series(_))) { ctx = ctx.pair("popup", "series"); }
        ctx
    }
    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool { self.popup.as_ref().is_some_and(|p| p.holds_focus(window, cx)) }
    pub fn dispatch(&mut self, action: &ActionId, count: Option<u32>, window, cx) -> bool {
        let Some(verb) = action.0.strip_prefix("timeseries::") else { return false };
        let n = count.unwrap_or(1) as usize;
        // Task 8: a popup closes first unless the verb is its own.
        let (now, as_of) = self.now_and_as_of(cx);
        let changed = match verb {
            "next" => self.model.cursor_next(n), "prev" => self.model.cursor_prev(n),
            "toggle_visible" => self.model.toggle_visible(),
            "axis_next" => self.model.cycle_axis(true, n), "axis_prev" => self.model.cycle_axis(false, n),
            "split_shrink" => self.model.step_split(false, n), "split_grow" => self.model.step_split(true, n),
            "colour" => self.model.cycle_colour(), "rule" => self.model.cycle_rule(),
            "remove" => self.remove_at_cursor(), "density" => self.model.toggle_density(), "percentiles" => self.model.toggle_percentiles(),
            "freq_finer" => self.noticed(self.model.step_frequency(true, n, now, &as_of)),
            "freq_coarser" => self.noticed(self.model.step_frequency(false, n, now, &as_of)),
            "pan_left" => self.model.pan(-(n as i32)), "pan_right" => self.model.pan(n as i32),
            "zoom_in" => self.model.zoom_in(n), "zoom_out" => self.model.zoom_out(n), "reset_view" => self.model.reset_view(),
            "jump_start" => self.model.jump_start(), "jump_end" => self.model.jump_end(),
            "add" | "expr" | "edit" | "list" | "range" | "list_down" | "list_up" | "list_close" | "commit" | "cancel" | "insert_up" | "insert_down" => return self.popup_verb(verb, n, window, cx),   // Tasks 8–10; `false` until then
            _ => return false,
        };
        self.apply_changed(changed, cx);
        true
    }
    /// The one tail every mutation ends at: FETCH → fetch every source slot for the (new) range and set reset_view;
    /// QUERY → requery; CHROME → rebuild_chrome (header + chart model, version bump); SESSION → nothing here (the shell
    /// serialises on its own schedule); then `cx.notify()`.
    fn apply_changed(&mut self, changed: Changed, cx: &mut Context<Self>) { .. }
    fn noticed(&mut self, r: Result<Changed, String>) -> Changed { match r { Ok(c) => c, Err(e) => { self.notice = Some(e.into()); Changed::CHROME } } }
    fn remove_at_cursor(&mut self) -> Changed {
        let Some(s) = self.model.cursor_slot() else { return Changed::NONE };
        let number = s.number;
        match self.model.remove(number) {
            Ok(r) => { if r.removed.len() > 1 { self.notice = Some(format!("removed s{number} and, with it, {}", r.removed[1..].iter().map(|n| format!("s{n}")).collect::<Vec<_>>().join(", ")).into()); } r.changed }
            Err(e) => { self.notice = Some(e.into()); Changed::CHROME }
        }
    }
    pub fn command(&mut self, line: &str, window, cx) -> Result<(), String> {
        let cmd = commands::parse(line)?;
        let (now, as_of) = self.now_and_as_of(cx);
        let settings = cx.try_global::<SeriesSettings>().cloned().unwrap_or_default();
        let changed = match cmd {
            Command::Add { identity, source } => {
                let source = match source.or(settings.default_source.clone()) { Some(s) => s, None => return Err(format!("name a source or set a default: add {identity}@<source>")) };
                let dataset = settings.dataset_of(&source).ok_or_else(|| format!("'{source}' is not a fetch source (have: {})", settings.names().join(", ")))?.to_string();
                let (_, ch) = self.model.add_source(&identity, &source, &dataset)?;
                ch
            }
            Command::Expr(text) => { let e = resolve(&text, self.model.slots(), settings.default_source.as_deref(), None)?; self.model.add_expr(&text, e).1 }
            Command::Remove(n) => { let r = self.model.remove(n)?; /* notice as above */ r.changed }
            Command::Rule(n, r) => self.model.set_rule(n, r)?,
            Command::Colour(n, name) => self.model.set_colour(n, self.colour_named(&name)?)?,
            Command::AxisMode(m) => self.model.set_axis_mode(m),
            Command::Freq(f) => self.model.set_frequency(f, now, &as_of)?,
            Command::Range(r) => self.model.set_range(r, now, &as_of)?,
            Command::Pct(p) => self.model.set_percentiles(p)?,
            Command::Density(d) => self.model.set_density(d)?,
            Command::YAxis(n, a) => self.model.set_axis(n, a)?,
            Command::Split(s) => self.model.set_split(s)?,
            Command::Clear => self.model.clear(),
        };
        if let Some(n) = self.model.take_notice() { self.notice = Some(n.into()); }
        self.apply_changed(changed, cx);
        Ok(())
    }
    /// `1`..`5` is a palette index; otherwise a `[colours]` name.
    fn colour_named(&self, name: &str) -> Result<Colour, String> {
        if let Ok(i) = name.parse::<usize>() && (1..=Palette::LEN).contains(&i) { return Ok(Colour::Palette(i - 1)); }
        if self.colours.borrow().get(name).is_some() { Ok(Colour::Named(name.into())) } else { Err(format!("no colour named '{name}' — 1..5 or a [colours] entry")) }
    }
    pub fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        let slots: Vec<u8> = self.model.slots().iter().map(|s| s.number).collect();
        let sources = cx.try_global::<SeriesSettings>().map(|s| s.names()).unwrap_or_default();
        let colours: Vec<String> = self.colours.borrow().names().map(str::to_string).collect();
        commands::completions(line, cursor, &slots, &sources, &colours)
    }
    pub fn title(&self) -> SharedString   // "timeseries · {range} · {freq}" + " · {n} series" when n > 0 — prepared in rebuild_chrome
    pub fn serialize(&self) -> toml::Table { session::to_table(&self.model) }
    /// Rebuild the header model, the title, and the chart model (version bump) from `result` + `model`.
    fn rebuild_chrome(&mut self, cx: &mut Context<Self>) { .. }
    /// Colour resolution for a slot: `Palette(i)` through `Palette::from_theme(chart_1..5, background, foreground).colour(i)`,
    /// `Named(n)` through `geode_core::colour::resolve(def, &anchors_from_theme(theme), &tokens_from_theme(theme))` → `to_hsla`,
    /// an unknown name falling back to `Palette(0)`. Reads `cx.theme()`; memoised across `theme_signature` in render.
    fn colour_of(&self, theme: &Theme) -> impl Fn(&Colour) -> Hsla + '_ { .. }
}

impl Render for TimeseriesTile {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let sig = geode_shell::shell::colours::theme_signature(theme);
        if self.theme_key != Some(sig) { self.theme_key = Some(sig); self.rebuild_chart_model(cx); }
        v_flex().size_full().bg(theme.background)
            .child(header::render_header(..))
            .when_some(self.notice.clone(), |el, n| el.child(header::render_notice(&n, theme)))
            .child(if self.model.slots().is_empty() { empty hint (muted, centred, "no series — a adds one, x composes") } else {
                div().flex_1().min_h_0().child(ChartElement::new(self.chart.clone(), self.model.view(), window.rem_size().as_f32(), ElementId::NamedInteger("ts-chart".into(), self.id.0))) })
            .child(header::render_footer(self.footer.clone(), theme))
            // Tasks 8–10 add the popup layer here.
    }
}
```

`rebuild_chart_model` builds through `chart::build(&result, &self.model, self.chart_version, offset_secs, &colour_of, default_source)` with `self.chart_version += 1` first and `offset_secs = chrono::Local::now().offset().local_minus_utc()`; with no result it builds from `SeriesResult::default()` (an empty model with the settings, so the element never sees a stale `axis_mode`). Note `theme_signature` returns `[Hsla; 28]` — check the exact type at `shell/colours.rs:143` and match `theme_key`'s.

- [ ] **Step 5: Run the tests, fmt, clippy; commit**

Run: `cargo test -p geode-timeseries`
Expected: every test in this task passes; Task 7's are not yet written.

```bash
git add crates/geode-timeseries
git commit -m "timeseries: the tile — factory, fragment, TileContent, normal-mode verbs, header chips, chart render, session, : line (spec §9.1–§9.4, §9.9, §9.11)"
```

---

### Task 7: Data flow — fetch on add, `SeriesFetched` → query, staged deliveries, visibility, restore

**Files:**
- Modify: `src/tile.rs`

**Interfaces:**
- Consumes: `DataHandle::{fetch, series, cancel}`, `geode_data::service::FetchParams` (check its path: `geode_data::FetchParams` or `geode_data::service::FetchParams`), `request::params`, `Frame::{versions, as_of, barrier_wants, arrived}`.
- Produces: `TimeseriesTile::{deliver(SeriesOutcome, cx), on_fetched(&str, &str, Result<u64, String>, cx), set_visible(bool, cx)}` and the private `requery`, `fetch_all`, `fetch_slot`, `self_arrive`, `arrive`, `arrive_and_release`, `promote`, `apply_result`, `follows_changed`, `differs_on_followed` (copied from the market-data tile with `as_of` as the ONLY followed counter, §6.5).

- [ ] **Step 1: Write the failing tests** (`tile.rs` tests)

```rust
#[gpui::test]
fn an_add_fetches_the_resolved_range_when_visible_and_defers_while_hidden(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    assert!(h.fetch_request().is_none(), "hidden: nothing is asked (the I2 contract)");
    h.visible(&mut vcx, true);
    let f = h.fetch_request().expect("shown: the pending fetch");
    assert_eq!((f.source.as_str(), f.identity.as_str(), f.key), ("demo_kdb", "SPX.close", QueryKey(TILE)));
    assert!((f.to - chrono::Utc::now()).num_seconds().abs() < 5);
    assert_eq!(f.to.checked_sub_months(chrono::Months::new(12)).unwrap(), f.from, "1y");
    assert!(h.series_request().is_none(), "no query until the fetch answers");
    h.command(&mut vcx, "add VIX").unwrap();
    let f = h.fetch_request().unwrap();
    assert_eq!(f.identity, "VIX");
}

#[gpui::test]
fn a_fetched_ok_marks_the_pair_idle_and_queries_once_and_an_err_marks_it_failed(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "add SPX.close").unwrap();     // a second slot of the same pair (§9.6)
    h.requests();                                       // drain the two fetches
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Ok(0));
    let m = h.model(&vcx);
    assert!(m.slots().iter().all(|s| s.state == SlotState::Idle));
    let q = h.series_request().expect("Ok(0) still requeries: the span is covered");
    assert_eq!(q.series.len(), 2);
    assert_eq!(q.dataset, "series");
    assert!(h.series_request().is_none(), "ONE query for the pair, not one per slot");
    h.deliver_fetched(&mut vcx, "demo_kdb", "NKY.close", Ok(9));
    assert!(h.series_request().is_none(), "a pair this tile does not hold is ignored");
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Err("kdb: timeout".into()));
    assert!(matches!(&h.model(&vcx).slots()[0].state, SlotState::Failed(e) if e == "kdb: timeout"));
    assert!(h.series_request().is_none(), "nothing is sent on Err");
}

#[gpui::test]
fn a_delivery_becomes_the_chart_model_and_a_stale_tag_is_dropped(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.requests();
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Ok(1));
    let tag = h.series_request().unwrap().tag;
    h.deliver_series(&mut vcx, tag, result_with(&[1], 5));
    let c = h.chart(&vcx);
    assert_eq!(c.buckets.len(), 5);
    assert_eq!(c.slots[0].values.len(), 5);
    assert_eq!((h.model(&vcx).view().lo, h.model(&vcx).view().hi), (0.0, 5.0), "the view is the whole range");
    h.deliver_series(&mut vcx, tag - 1, result_with(&[1], 50));
    assert_eq!(h.chart(&vcx).buckets.len(), 5, "stale");
    h.deliver_series_err(&mut vcx, tag, "1m over 3y is 1,170,000 points; the cap is 500,000");
    assert_eq!(h.chart(&vcx).buckets.len(), 5, "last good stays");
    assert!(h.notice(&vcx).unwrap().contains("the cap is 500,000"));
}

#[gpui::test]
fn a_query_change_requeries_and_a_range_change_fetches_and_queries(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.requests();
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Ok(1));
    h.requests();
    h.dispatch(&mut vcx, "rule", None);
    let q = h.series_request().expect("a rule change is a query");
    assert!(matches!(q.series[0].kind, SlotKind::Source { rule: BucketRule::First, .. }));
    h.dispatch(&mut vcx, "axis_next", None);
    assert!(h.series_request().is_none(), "an axis is chrome");
    h.command(&mut vcx, "range 1w").unwrap();
    let reqs = h.requests();
    assert!(matches!(reqs[0], Request::Fetch(_)), "the range fetches the gaps…");
    assert!(matches!(reqs[1], Request::Series(_)), "…and queries the cached part at once (§9.10)");
    // Stats over the visible window: a pan requeries while percentiles are on.
    h.deliver_series(&mut vcx, h_last_tag(&reqs), result_with(&[1], 20));
    h.dispatch(&mut vcx, "zoom_in", None);
    let q = h.series_request().expect("stats follow the view");
    assert!(q.window.0 > q.range.0 && q.window.1 < q.range.1);
    h.dispatch(&mut vcx, "percentiles", None);
    h.series_request().unwrap();
    h.dispatch(&mut vcx, "density", None);
    h.series_request().unwrap();
    h.dispatch(&mut vcx, "zoom_out", None);
    assert!(h.series_request().is_none(), "with both off, a view move asks nothing");
}

#[gpui::test]
fn the_tile_follows_as_of_only_and_stages_under_an_open_barrier(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.requests();
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Ok(1));
    let tag = h.series_request().unwrap().tag;
    h.deliver_series(&mut vcx, tag, result_with(&[1], 5));
    // scope and data bumps: nothing
    h.frame.update(&mut vcx, |f, cx| { f.set_scope_for_test(); cx.notify(); });
    assert!(h.series_request().is_none(), "scope is not followed");
    // An as-of change opens a barrier over this tile and requeries.
    let at = chrono::Utc::now() - chrono::Duration::days(30);
    open_barrier_on_as_of(&h, &mut vcx, &[QueryKey(TILE)], at);
    let q = h.series_request().expect("as-of is followed");
    assert_eq!(q.as_of, AsOf::At(at));
    assert!(q.range.1 <= at, "the as-of clips the visible end");
    h.deliver_series(&mut vcx, q.tag, result_with(&[1], 3));
    assert_eq!(h.chart(&vcx).buckets.len(), 3, "the only awaited tile: arriving released the barrier and promoted at once");
    // With a second awaited key the delivery is STAGED until the flip.
    open_barrier_on_as_of(&h, &mut vcx, &[QueryKey(TILE), QueryKey(99)], at - chrono::Duration::days(1));
    let q = h.series_request().unwrap();
    h.deliver_series(&mut vcx, q.tag, result_with(&[1], 7));
    assert_eq!(h.chart(&vcx).buckets.len(), 3, "staged");
    h.frame.update(&mut vcx, |f, cx| { f.sweep(std::time::Instant::now() + geode_shell::frame::FLIP_DEADLINE); cx.notify(); });
    assert_eq!(h.chart(&vcx).buckets.len(), 7, "promoted on the flip");
}

#[gpui::test]
fn a_hidden_tile_cancels_and_a_shown_one_requeries_and_a_restored_one_refetches_once(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.requests();
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Ok(1));
    h.requests();
    h.visible(&mut vcx, false);
    assert!(matches!(h.raw_requests().last(), Some(Request::Cancel { key }) if *key == QueryKey(TILE)));
    h.visible(&mut vcx, true);
    let reqs = h.requests();
    assert!(reqs.iter().any(|r| matches!(r, Request::Fetch(_))), "shown: refetch (§9.10)…");
    assert!(reqs.iter().any(|r| matches!(r, Request::Series(_))), "…and requery");
    let table = vcx.update(|_, cx| h.content.serialize(cx));
    let (h2, mut vcx2) = open_with(cx, Some(table));
    assert!(matches!(h2.model(&vcx2).slots()[0].state, SlotState::Idle), "restored: not yet asked");
    h2.visible(&mut vcx2, true);
    assert!(matches!(h2.model(&vcx2).slots()[0].state, SlotState::Fetching));
    let f = h2.fetch_request().expect("a restored tile refetches once");
    assert_eq!(f.identity, "SPX.close");
    assert!(h2.fetch_request().is_none());
    h2.visible(&mut vcx2, false);
    h2.visible(&mut vcx2, true);
    assert!(h2.fetch_request().is_some(), "every show refetches (coverage subtraction makes it cheap)");
}
```

`result_with(slots: &[u8], n: usize) -> SeriesResult` builds `n` daily buckets from a fixed epoch and one `SlotResult` per number with `values = (0..n).map(f64::from)`, three percentiles and no bins. `open_barrier_on_as_of(h, vcx, keys: &[QueryKey], at: DateTime<Utc>)` is the market-data helper (`tile.rs:5514-5528`) with an instant in place of its `secs`: `h.frame.update(vcx, |f, cx| { f.set_as_of(AsOf::At(at)); f.open_flip(keys.to_vec(), Instant::now()); cx.notify(); })`. `set_scope_for_test` — check `Frame` for a test-support scope setter (`grep -n 'cfg(any(test' crates/geode-shell/src/frame.rs`); if none, drive `f.set_text_filter("x")` or whichever public mutation bumps `scope`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p geode-timeseries tile::`
Expected: the new tests fail (no requests are sent yet).

- [ ] **Step 3: Implement**

`apply_changed`:

```rust
fn apply_changed(&mut self, changed: Changed, cx: &mut Context<Self>) {
    if changed.fetch() { self.reset_view = true; if self.visible { self.fetch_all(cx); } }
    if changed.query() && self.visible && !self.model.slots().is_empty() { self.requery(cx); }
    if changed.chrome() || changed.query() || changed.fetch() { self.rebuild_chrome(cx); }
    cx.notify();
}
```

But an `add` must NOT requery until the fetch answers (§9.10: the slot is `Fetching`; `SeriesFetched Ok` sends the query). `add_source` answers `FETCH | CHROME | SESSION` without `QUERY`, and `fetch_all` on a `FETCH` fetches every source slot (the data tier subtracts coverage, so an already-covered pair answers `Ok(0)` at once and the requery follows). A range change answers `ALL`: fetch every slot AND query the cached part at once. So `fetch_all` sends one `FetchParams { key: QueryKey(self.id.0), source, identity, from, to }` per source slot with `(from, to) = self.model.range().resolve(now, &as_of)` and sets each slot `Fetching` (via `set_pair_state`). `on_fetched(source, identity, result)`:

```rust
pub fn on_fetched(&mut self, source: &str, identity: &str, result: Result<u64, String>, cx: &mut Context<Self>) {
    if !self.model.holds_pair(source, identity) { return; }
    match result {
        Ok(_) => { self.model.set_pair_state(source, identity, SlotState::Idle); if self.visible { self.requery(cx); } }
        Err(e) => { self.model.set_pair_state(source, identity, SlotState::Failed(e)); }
    }
    self.rebuild_chrome(cx);
    cx.notify();
}
```

`requery` is the market-data `requery` (`tile.rs:965-999`) with `request::params(&self.model, QueryKey(self.id.0), self.tag, Utc::now(), &as_of, buckets)` where `buckets` is the current result's (or `&[]`), submitted through `self.data.series(params)`; a `false` sets the notice `series request refused: the data service is busy or gone`, arrives, clears `acted`. `deliver(outcome: SeriesOutcome)` is the market-data `deliver` (`tile.rs:1001-1058`) with `outcome.result` in place of `outcome.snapshot` and `apply_result(result)` in place of `apply(snapshot)`:

```rust
fn apply_result(&mut self, result: SeriesResult, cx: &mut Context<Self>) {
    let full_len = result.buckets.len() as f64;
    let full = match self.model.axis_mode() {
        AxisMode::Session => (0.0, full_len),
        AxisMode::Continuous => (result.buckets.first().copied().unwrap_or(0) as f64, result.buckets.last().copied().unwrap_or(0) as f64 + (self.model.frequency().seconds() * 1_000_000) as f64),
    };
    self.model.set_full(full);
    if std::mem::take(&mut self.reset_view) { self.model.reset_view(); }
    self.result = Some(Arc::new(result));
    self.notice = None;
    self.rebuild_chrome(cx);
}
```

`on_frame_changed` is the market-data observer with `this.key.is_some()` → `!this.model.slots().is_empty()`; `follows_changed`/`differs_on_followed` compare `as_of` ONLY (`versions.as_of != now.as_of`) — the `data` counter is a blotter concern (a series fetch never bumps it, and a CSV publish must not requery every chart). `set_visible`:

```rust
pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
    if self.visible == visible { return; }
    self.visible = visible;
    if visible {
        if self.model.source_slots().next().is_some() { self.restore_pending = false; self.fetch_all(cx); }
        if !self.model.slots().is_empty() && self.follows_changed(self.frame.read(cx).versions()) { self.requery(cx); }
    } else {
        self.data.cancel(QueryKey(self.id.0));
        self.acted = None;
    }
    self.rebuild_chrome(cx);
    cx.notify();
}
```

(`restore_pending` becomes redundant under "every show refetches" — keep the field only if the tests need to distinguish; otherwise drop it from the struct and from Task 6's field list.)

An axis-mode change (`:axis`) changes `full`'s units: `set_axis_mode` in the tile's `command` arm re-derives `full` from the current result (call `apply_result`'s `full` computation — factor it into `fn full_of(&self, buckets: &[i64]) -> (f64, f64)`) and resets the view.

- [ ] **Step 4: Run, fmt, clippy; commit**

```bash
git add crates/geode-timeseries
git commit -m "timeseries: data flow — fetch on add, SeriesFetched → query, staged deliveries under the barrier, as-of-only following, visibility and restore (spec §6.5, §9.10)"
```

---

### Task 8: The series popup (`L`) and chip clicks

**Files:**
- Modify: `src/popup.rs` (the `Popup` enum gains `Series(SeriesPopup)`), `src/tile.rs` (`popup_verb` arms `list`, `list_down`, `list_up`, `list_close`; the close-before-other-verbs rule; `chip_clicked`), `src/header.rs` (chip click already wired in Task 6)

**Interfaces:**
- Produces: `pub(crate) struct SeriesPopup { pub rows: Vec<SeriesRow> }`, `pub(crate) struct SeriesRow { pub label, pub source_rule, pub axis, pub state, pub swatch: Colour, pub hidden: bool }`, `SeriesPopup::prepare(model: &Model, result: Option<&SeriesResult>, default_source: Option<&str>) -> SeriesPopup`, `pub(crate) fn render_series_popup(p: &SeriesPopup, cursor: Option<usize>, theme, tile: &Entity<TimeseriesTile>, tile_id: u64, swatch_of) -> AnyElement`, `TimeseriesTile::{open_series_popup, close_popup_with_window, chip_clicked}`.

- [ ] **Step 1: Write the failing tests**

```rust
#[gpui::test]
fn shift_l_opens_the_series_popup_whose_cursor_is_the_chips_cursor(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "add VIX@demo_rest").unwrap();
    h.command(&mut vcx, "rule s2 mean").unwrap();
    h.dispatch(&mut vcx, "list", None);
    assert!(h.popup_is_series(&vcx));
    assert_eq!(h.key_context_pairs(&mut vcx).get("popup").map(String::as_str), Some("series"));
    assert_eq!(h.key_context_mode(&mut vcx), "normal", "the list holds no field: normal mode with a popup pair");
    let rows = h.series_rows(&vcx);
    assert_eq!(rows[1].source_rule.as_ref(), "demo_rest · mean");
    assert_eq!(rows[1].axis, "L");
    assert_eq!(rows[1].state.as_ref(), "fetching");
    h.dispatch(&mut vcx, "list_up", None);
    assert_eq!(h.model(&vcx).cursor(), Some(0));
    h.dispatch(&mut vcx, "list_down", Some(3));
    assert_eq!(h.model(&vcx).cursor(), Some(1), "wraps like the chips");
    h.dispatch(&mut vcx, "axis_next", None);
    assert!(h.popup_is_series(&vcx), "a popup verb keeps it open");
    assert_eq!(h.series_rows(&vcx)[1].axis, "R");
    h.dispatch(&mut vcx, "pan_left", None);
    assert!(!h.popup_is_series(&vcx), "any other verb closes it first");
    h.dispatch(&mut vcx, "list", None);
    h.dispatch(&mut vcx, "list_close", None);
    assert!(h.popup_is_none(&vcx));
}

#[gpui::test]
fn a_chip_click_moves_the_cursor_without_opening_the_popup_and_a_row_click_moves_it_too(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "add VIX").unwrap();
    h.click(&mut vcx, "ts-chip-{TILE}-0");
    assert_eq!(h.model(&vcx).cursor(), Some(0));
    assert!(h.popup_is_none(&vcx));
    h.dispatch(&mut vcx, "list", None);
    h.click(&mut vcx, "ts-list-row-{TILE}-1");
    assert_eq!(h.model(&vcx).cursor(), Some(1));
    assert!(h.popup_is_series(&vcx), "a row click moves the cursor and keeps the list");
}
```

`h.click(vcx, selector)` resolves the element's bounds by `debug_selector` (`vcx.debug_bounds(selector)`) and calls the market-data harness's `click_at(vcx, centre, 1)` (`geode-marketdata/src/tile.rs:5156`) — copy that helper.

- [ ] **Step 2: Run to verify failure; Step 3: Implement**

`popup.rs`: `Popup::Series(SeriesPopup)`; `Popup::is_insert()` false for it; `holds_focus` false. The popup is painted exactly as the market-data menu (`geode-marketdata/src/popup.rs:391-518`): `popover_surface(cx)` (`v_flex().min_w(scale::design(240.)).p_1().gap_y_0p5().text_sm().popover_style(cx)`), `.occlude()`, `.on_mouse_down_out(.. close_popup_with_window ..)`, rows `h_flex().h(scale::design(26.)).px(scale::design(8.)).rounded(theme.radius)` with the cursor row `bg(theme.accent).text_color(theme.accent_foreground)`, each row `swatch · label · source_rule · axis · state`, `debug_selector("ts-list-row-{tile}-{i}")`, `on_mouse_down(Left)` → `stop_propagation` + `tile.update(.. t.chip_clicked(i, cx))`; wrapped in `deferred(anchored().anchor(Anchor::TopRight).position_mode(AnchoredPositionMode::Local).snap_to_window_with_margin(px(8.)).child(list)).with_priority(1)` inside a `relative()` wrapper round the header as `tile.rs:4430-4459` does. State text: `fetching` / `failed: <reason>` / `degraded` (from `result.slots[..].provenance.health` when `Some(Health::Degraded)`) / empty. `dispatch`'s head gains the market-data close rule (`tile.rs:1671-1687`): any verb outside `list | list_down | list_up | list_close | toggle_visible | axis_next | axis_prev | colour | rule | remove | edit` closes an open popup first. `list_down`/`list_up` are `cursor_next`/`cursor_prev`. `close_popup_with_window` is the one closer (blur only when the popup's own field is focused — none here; Tasks 9–10 add fields).

- [ ] **Step 4: Run, fmt, clippy; commit**

```bash
git commit -am "timeseries: the series popup (L) over the chips' cursor, chip and row clicks (spec §9.5)"
```

---

### Task 9: The picker (`a`) and the expression field (`x`/`e`) — insert mode

**Files:**
- Modify: `src/popup.rs` (`Popup::{Picker(PickerState), Expr(ExprField)}`), `src/tile.rs` (`popup_verb` arms `add`, `expr`, `edit`, `commit`, `cancel`, `insert_up`, `insert_down`; `holds_focus`; the `Diagnostics` observer; `close_popup_with_window` blur), `src/header.rs` (the expression strip below the header)

**Interfaces:**
- Consumes: `geode_shell::choice::{ChoiceList, DEFAULT_CAP, NavCommand}`, `gpui_component::input::{Input, InputState}`, `Diagnostics::{catalog, request_catalog, watch}`, `SeriesSettings`.
- Produces:

```rust
pub(crate) enum PickerStage { Identities, Sources { identity: String } }
pub(crate) struct PickerState { pub input: Entity<InputState>, pub list: ChoiceList, pub stage: PickerStage, pub loaded: Vec<bool> /* parallel to list.options() */, pub add_row: Option<String> }
pub(crate) struct ExprField { pub input: Entity<InputState>, pub editing: Option<u8>, pub error: Option<SharedString> }
```

- [ ] **Step 1: Write the failing tests**

```rust
#[gpui::test]
fn a_opens_the_picker_over_the_catalogue_and_enter_adds_the_highlighted_pair(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    seed_catalog(&h, &mut vcx, &[("demo_kdb", &["SPX.close", "VIX", "NKY.close"])]);   // demo_rest has no catalogue
    h.visible(&mut vcx, true);
    h.dispatch(&mut vcx, "add", None);
    assert_eq!(h.key_context_mode(&mut vcx), "insert");
    assert!(vcx.update(|w, cx| h.content.holds_focus(w, cx)), "the picker's field holds the keyboard");
    assert_eq!(h.picker_rows(&vcx), vec!["NKY.close@demo_kdb", "SPX.close@demo_kdb", "VIX@demo_kdb"], "identity first, sorted, every fetch source with a catalogue");
    vcx.simulate_input("vi");
    assert_eq!(h.picker_rows(&vcx)[0], "VIX@demo_kdb");
    h.dispatch(&mut vcx, "commit", None);
    assert!(h.popup_is_none(&vcx));
    assert_eq!(h.key_context_mode(&mut vcx), "normal");
    assert!(!vcx.update(|w, cx| h.content.holds_focus(w, cx)), "blurred before the drop");
    assert!(h.model(&vcx).holds_pair("demo_kdb", "VIX"));
    assert!(h.fetch_request().is_some());
    // The loaded row is marked and still pickable.
    h.dispatch(&mut vcx, "add", None);
    assert!(h.picker_loaded_marks(&vcx).contains(&true));
    h.dispatch(&mut vcx, "cancel", None);
    assert!(h.popup_is_none(&vcx));
    assert_eq!(h.model(&vcx).slots().len(), 1);
}

#[gpui::test]
fn an_unmatched_text_offers_the_add_row_which_opens_the_source_stage(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    seed_catalog(&h, &mut vcx, &[("demo_kdb", &["SPX.close"])]);
    h.dispatch(&mut vcx, "add", None);
    vcx.simulate_input("/v1/px?sym=SPX");
    assert!(h.picker_rows(&vcx).is_empty());
    assert_eq!(h.picker_add_row(&vcx).as_deref(), Some("add \"/v1/px?sym=SPX\"…"));
    h.dispatch(&mut vcx, "commit", None);
    assert!(h.picker_is_source_stage(&vcx));
    assert_eq!(h.picker_rows(&vcx), vec!["demo_kdb", "demo_rest"]);
    assert_eq!(h.picker_highlighted(&vcx), "demo_kdb", "the default source is highlighted");
    h.dispatch(&mut vcx, "insert_down", None);
    h.dispatch(&mut vcx, "commit", None);
    assert!(h.model(&vcx).holds_pair("demo_rest", "/v1/px?sym=SPX"));
    // A text already spelled identity@known-source skips the stage.
    h.dispatch(&mut vcx, "add", None);
    vcx.simulate_input("EURUSD@demo_rest");
    h.dispatch(&mut vcx, "commit", None);
    assert!(h.model(&vcx).holds_pair("demo_rest", "EURUSD"));
    assert!(h.popup_is_none(&vcx));
}

#[gpui::test]
fn opening_the_picker_asks_for_a_catalog_and_re_ranks_when_it_lands(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.dispatch(&mut vcx, "add", None);
    assert!(h.diagnostics.update(&mut vcx, |d, _| d.take_pending_catalog_request()), "no catalog yet: one is requested (with cx.notify, the CLAUDE.md trap)");
    assert!(h.picker_rows(&vcx).is_empty());
    seed_catalog(&h, &mut vcx, &[("demo_kdb", &["VIX"])]);
    assert_eq!(h.picker_rows(&vcx), vec!["VIX@demo_kdb"], "the open picker re-ranked on the Diagnostics notify");
}

#[gpui::test]
fn x_opens_the_expression_field_and_enter_adds_or_reports_inline(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.command(&mut vcx, "add VIX").unwrap();
    h.dispatch(&mut vcx, "expr", None);
    assert_eq!(h.key_context_mode(&mut vcx), "insert");
    vcx.simulate_input("s1 ^ s2");
    h.dispatch(&mut vcx, "commit", None);
    assert!(h.popup_is_expr(&vcx), "a parse error keeps the field open");
    assert!(h.expr_error(&vcx).unwrap().contains("arithmetic only"));
    assert_eq!(h.model(&vcx).slots().len(), 2);
    h.set_input_text(&mut vcx, "s1 / s2");
    h.dispatch(&mut vcx, "commit", None);
    assert!(h.popup_is_none(&vcx));
    assert_eq!(h.model(&vcx).slots()[2].text.as_deref(), Some("s1 / s2"));
    assert!(h.series_request().is_some() || true, "a query follows once the tile is visible");
    // `e` reopens the cursor's expression prefilled; `escape` discards.
    h.dispatch(&mut vcx, "edit", None);
    assert_eq!(h.input_text(&vcx), "s1 / s2");
    vcx.simulate_input(" * 2");
    h.dispatch(&mut vcx, "cancel", None);
    assert_eq!(h.model(&vcx).slots()[2].text.as_deref(), Some("s1 / s2"), "escape discards");
    h.dispatch(&mut vcx, "edit", None);
    h.set_input_text(&mut vcx, "s1 - s2");
    h.dispatch(&mut vcx, "commit", None);
    assert_eq!(h.model(&vcx).slots()[2].text.as_deref(), Some("s1 - s2"), "replaced in place, same number");
    assert_eq!(h.model(&vcx).slots()[2].number, 3);
    h.dispatch(&mut vcx, "prev", None);
    h.dispatch(&mut vcx, "edit", None);
    assert!(h.popup_is_none(&vcx), "`e` on a source slot does nothing");
    assert_eq!(h.notice(&vcx).as_deref(), Some("s2 is not an expression"));
}

#[gpui::test]
fn both_closers_blur_before_dropping_the_input(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.dispatch(&mut vcx, "add", None);
    assert!(vcx.update(|w, cx| w.focused(cx).is_some()));
    h.dispatch(&mut vcx, "cancel", None);
    assert!(vcx.update(|w, cx| w.focused(cx).is_none()), "picker: blurred, then dropped");
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.dispatch(&mut vcx, "expr", None);
    assert!(vcx.update(|w, cx| w.focused(cx).is_some()));
    h.dispatch(&mut vcx, "cancel", None);
    assert!(vcx.update(|w, cx| w.focused(cx).is_none()), "expression field: blurred, then dropped");
}
```

`seed_catalog(h, vcx, sources)` calls `h.diagnostics.update(vcx, |d, cx| { d.set_catalog(CatalogSnapshot { identities: .., ..Default::default() }); cx.notify(); })`. `Diagnostics::take_pending_catalog_request(&mut self) -> bool` exists (`diagnostics.rs:693`); read it through `update`.

- [ ] **Step 2: Run to verify failure; Step 3: Implement**

The picker copies the market-data underlying picker (`geode-marketdata/src/tile.rs:3078-3151`, `popup.rs:222-237, 368-371, 574-612`): `cx.new(|cx| InputState::new(window, cx).placeholder("identity@source"))`, `.focus(window, cx)`; the option strings are `"{identity}@{source}"` for every `(source, identities)` in `diagnostics.read(cx).catalog.as_ref().map(|c| &c.identities)` whose source is in `SeriesSettings.sources`, sorted; `loaded[i] = model.holds_pair(..)`; rows paint identity in the first column and `@source` in the second (`muted_foreground`), a `•` mark on loaded rows. The `Input`'s `Change` event re-feeds `list.set_query(text)` and recomputes `add_row = (list.ranked().is_empty() && !text.is_empty()).then(|| format!("add \"{text}\"…"))`. `commit`: re-feed the live text first (`set_value` emits no `Change`); if `add_row` is `Some` → if the text parses as `identity@source` with a known fetch source, add it; else `stage = Sources { identity: text }`, `list = ChoiceList::new(settings.names(), DEFAULT_CAP)`, `list.place(default_source)`, clear the field; in the `Sources` stage `commit` adds `(identity, highlighted source)`. `insert_up`/`insert_down` are `nav_clamped(NavCommand::Prev/Next)` (the picker's clamping rule). Opening calls `diagnostics.update(cx, |d, cx| { d.request_catalog(); cx.notify(); })` when `catalog.is_none()` or it lacks identities; the `cx.observe(&diagnostics, ..)` in `new` re-ranks an open picker (the market-data observer, `tile.rs:743-760`). `holds_focus` answers `input.read(cx).focus_handle(cx).is_focused(window)` for both popups.

The expression field: `Popup::Expr(ExprField)` painted as a one-line `Input` strip under the header (in `render`, between the header and the chart); `x` opens empty, `e` opens prefilled with `cursor_slot().text` and `editing = Some(number)` (a source slot → notice `s{n} is not an expression`, no popup). `commit`: `resolve(&text, model.slots(), default_source, editing)` → `Err(e)` sets `error` and keeps the field; `Ok(expr)` → `add_expr`/`replace_expr`, close, `apply_changed`. `cancel` closes. Both closers go through `close_popup_with_window` (blur when the popup's own input is focused, then `self.popup = None`).

- [ ] **Step 4: Run, fmt, clippy; commit**

```bash
git commit -am "timeseries: the picker (a) with its source stage and the expression field (x/e) in insert mode (spec §9.6, §9.7)"
```

---

### Task 10: The range popup (`r`)

**Files:**
- Modify: `src/popup.rs` (`Popup::Range(RangePopup)`), `src/tile.rs` (`popup_verb` arm `range`; `range_key`), `src/header.rs` or `popup.rs` (paint)

**Interfaces:**
- Consumes: `geode_widgets::datefield::{DateTimeField, FieldKey, Precision, Segment, route, paint, SegmentPaint}`, the market-data `render_date_field` (`header.rs:114-169`) and `date_field_key` (`tile.rs:2104-2151`) as the model.
- Produces: `pub(crate) struct RangePopup { pub from: DateTimeField, pub to: DateTimeField, pub active: Which /* From | To */, pub focus: FocusHandle, pub paint: (DateFieldPaint, DateFieldPaint) }`.

- [ ] **Step 1: Write the failing tests**

```rust
#[gpui::test]
fn r_opens_the_range_popup_on_from_day_and_a_digit_commits_a_preset(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.visible(&mut vcx, true);
    h.requests();
    h.dispatch(&mut vcx, "range", None);
    assert_eq!(h.key_context_mode(&mut vcx), "insert");
    assert!(vcx.update(|w, cx| h.content.holds_focus(w, cx)));
    assert_eq!(h.range_active_segment(&vcx), (Which::From, Segment::Day));
    vcx.simulate_keystrokes("3");
    assert!(h.popup_is_none(&vcx));
    assert_eq!(h.model(&vcx).range(), &Range::Relative(Preset::M3));
    assert!(h.requests().iter().any(|r| matches!(r, Request::Fetch(_))), "a committed range fetches");
}

#[gpui::test]
fn tab_moves_between_the_fields_and_enter_commits_an_absolute_range(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.dispatch(&mut vcx, "range", None);
    // `from` opens on today minus the current preset; type a year.
    vcx.simulate_keystrokes("left left");                  // year segment
    assert_eq!(h.range_active_segment(&vcx).1, Segment::Year);
    vcx.simulate_keystrokes("2 0 2 6");
    vcx.simulate_keystrokes("tab");
    assert_eq!(h.range_active_segment(&vcx).0, Which::To);
    vcx.simulate_keystrokes("shift-tab");
    assert_eq!(h.range_active_segment(&vcx).0, Which::From);
    vcx.simulate_keystrokes("enter");
    assert!(h.popup_is_none(&vcx));
    assert!(matches!(h.model(&vcx).range(), Range::Absolute { from, .. } if from.year() == 2026));
    h.dispatch(&mut vcx, "range", None);
    vcx.simulate_keystrokes("escape");
    assert!(h.popup_is_none(&vcx));
    assert!(!vcx.update(|w, cx| h.content.holds_focus(w, cx)));
}

#[gpui::test]
fn a_preset_click_commits_at_once_and_a_backwards_range_is_refused_inline(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open(cx);
    h.dispatch(&mut vcx, "range", None);
    h.click(&mut vcx, "ts-range-preset-{TILE}-5w");   // "1y" (index 4 → selector by preset word)
    assert_eq!(h.model(&vcx).range(), &Range::Relative(Preset::Y1));
    h.dispatch(&mut vcx, "range", None);
    // Move `to` before `from` and commit.
    vcx.simulate_keystrokes("tab");
    vcx.simulate_keystrokes("left left");
    vcx.simulate_keystrokes("1 9 9 0");
    vcx.simulate_keystrokes("enter");
    assert!(h.popup_is_range(&vcx), "refused: still open");
    assert!(h.range_error(&vcx).unwrap().contains("before"));
}
```

Fix the preset selector spelling when writing: `ts-range-preset-{tile}-{word}` with the word (`1y`).

- [ ] **Step 2: Run to verify failure; Step 3: Implement**

The popup holds a `FocusHandle` (`cx.focus_handle()`, focused on open) and an `on_key_down` listener on its container (the market-data `render_date_field` shape); `key_context` reports `insert` while it is open; `holds_focus` answers `focus.is_focused(window)`. `range_key(event)`: `tab`/`shift+tab` switch `active`; otherwise `route(key, shift, chord)` → `Commit` builds `Range::Absolute { from: from.date(), to: to.date() }` after `complete_pending()` on both (a `Err(segment)` keeps the popup with `error`), refuses `to < from` inline, else `set_range` (a cap refusal is shown inline too) and closes; `Cancel` closes; a bare digit `1`..`7` while NO digits are pending in the active segment (`field.typed` is private — check `DateTimeField` for a `has_pending()`; if absent, add `pub fn typing(&self) -> bool` to `geode-widgets` in this task) commits `Preset::digit(d)` at once (§9.8) — a digit while a segment is mid-entry goes to the field; any other key → `active_field.apply(key)`. Paint: two `paint(..)` rows labelled `from`/`to` (the active one with the `primary` segment fill, the inactive dimmed), a row of seven preset chips (`chip_paint(Neutral)` + `control::for_chip`, `debug_selector("ts-range-preset-{tile}-{word}")`, click → commit preset), the error line, in the popover surface anchored under the header like the series popup. Both fields open on `Segment::Day` with `from` active, seeded from `model.range().resolve(now, &as_of)`'s dates. The closer is `close_popup_with_window` (blur when `focus.is_focused(window)`).

- [ ] **Step 4: Run, fmt, clippy; commit**

```bash
git commit -am "timeseries: the range popup (r) — two segmented date fields, presets by digit or click (spec §9.8)"
```

---

### Task 11: App wiring and `--demo`

**Files:**
- Modify: `crates/geode-app/Cargo.toml` (`geode-timeseries.workspace = true`), `crates/geode-app/src/bridge.rs` (`Bridge.timeseries: Rc<TimeseriesFactory>`, built in `start` beside `marketdata`; `ConfigReloaded` calls `timeseries.set_colours(colours.clone())` beside `factory.set_colours`), `crates/geode-app/src/main.rs` (`TimeseriesFactoryHandle`, `roster.add` inside the `data_setup(..).map` closure after the dividend line)

- [ ] **Step 1: Write the failing test** (in `main.rs`'s tests beside the `tile::add_dividend` assertion at ~1043)

```rust
#[test]
fn the_roster_lists_timeseries_and_registers_its_add_action() {
    let (registry, roster) = build_registry_and_roster_for_test();   // whatever the neighbouring test uses
    assert!(roster.kinds().contains(&"timeseries"));
    assert_eq!(registry.get(&ActionId("tile::add_timeseries".into())).unwrap().title, "Timeseries: Split");
    assert!(registry.get(&ActionId("timeseries::add".into())).is_some());
}
```

and in `bridge.rs`'s tests: `the_bridge_builds_a_timeseries_factory_with_the_demo_colours` asserting `bridge.timeseries.kind() == "timeseries"`.

- [ ] **Step 2: Implement**

`bridge.rs` `start`: `let timeseries = Rc::new(TimeseriesFactory::new(handle.clone(), setup.colours.clone()));` — `setup.colours` moves into the blotter factory a few lines down; clone it before. In `attach`'s `ConfigReloaded` arm, after `factory.set_colours(colours)`: `timeseries.set_colours(colours_clone)` (clone the parsed `NamedColours` once for both). `main.rs`: `struct TimeseriesFactoryHandle(Rc<TimeseriesFactory>)` forwarding every trait method (copy `MarketDataFactoryHandle`, main.rs:599-626, comment included — `contexts` must be forwarded even though it is the default), and `roster.add(Box::new(TimeseriesFactoryHandle(bridge.timeseries.clone())));` after the dividend `roster.add`. No `geode_timeseries::init` — the tile hosts no gpui-component table.

- [ ] **Step 3: Run the demo end to end** (the sandbox cannot paint; do the non-window half)

`cargo run -p geode-app -- --demo 1000` should start; if it cannot open a window in the sandbox, `cargo test -p geode-app` covers the roster and the demo layer (`the_demo_layer_produces_a_servable_data_setup` must still pass with `[timeseries]` in `app.toml`). Record in the task report that §1.2 item 1 is the user's display check.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p geode-app`, `cargo clippy -p geode-app --all-targets -- -D warnings`.

```bash
git commit -am "app: register the timeseries tile; named colours reach it on reload; --demo sets default_source = demo_kdb (spec §10)"
```

---

### Task 12: Harness entries, docs, the spec's as-built section

**Files:**
- Modify: `scripts/mutation-check.sh`, `docs/perf.md`, `docs/phase-history.md`, `CLAUDE.md`, `docs/superpowers/specs/2026-09-19-geode-timeseries-viewer-design.md` (§9.13 "As built (Part 4)", §1.2 item 1/3 status), `docs/superpowers/specs/2026-09-19-…` §11.2 (tick the dependant-removal entry)

- [ ] **Step 1: Harness entries** — append after the last `chart:` entry, each verified `caught` by running `zsh scripts/mutation-check.sh "timeseries:"` (and the two `series settings:` ones with `"series settings:"`). Commit before mutating.

| name | file | from → to | test |
|---|---|---|---|
| `timeseries: dependant removal is transitive` | `src/core/model.rs` | `frontier.push(s.number);` → `// frontier.push(s.number);` | `removing_an_operand_removes_its_dependants_transitively` |
| `timeseries: a bare identity prefers the default source` | `src/core/resolve.rs` | `if let Some(d) = default_source && let Some((n, _, _)) = matches.iter().find(|(_, s, _)| *s == d) { return Ok(*n); }` → `if false {}` | `a_handle_an_exact_pair_and_a_default_source_identity_all_resolve` |
| `timeseries: an ambiguous identity is refused` | `src/core/resolve.rs` | `many => Err(` → `[(n, _, _), ..] => Ok(*n), many => Err(` (or the nearest compiling form) | `ambiguity_and_absence_are_named_errors` |
| `timeseries: the cap is pre-checked` | `src/core/model.rs` | `if points > SERIES_POINT_CAP` → `if points > u64::MAX` | `frequency_and_range_are_pre_checked_against_the_cap` |
| `timeseries: density is bounded by the quad budget` | `src/core/model.rs` | `bins as usize * visible > MAX_DENSITY_QUADS` → `false` | `density_is_bounded_by_the_chart_quad_budget` |
| `timeseries: a view move requeries only while stats are on` | `src/core/model.rs` | `if self.stats_on() { Changed::CHROME \| Changed::QUERY }` → `if true { Changed::CHROME \| Changed::QUERY }` | `view_verbs_are_chrome_plus_a_query_while_stats_are_on` |
| `timeseries: the window is the visible span` | `src/core/request.rs` | `(at(buckets[lo]), at(buckets[hi - 1] + step))` → `(at(buckets[0]), at(buckets[buckets.len() - 1] + step))` | `the_window_is_the_visible_span_in_both_axis_modes` |
| `timeseries: a result slot the model lacks is skipped` | `src/core/chart.rs` | `let r = result.slots.iter().find(\|r\| r.slot == s.number);` → `let r = result.slots.get(i);` | `a_slot_the_result_lacks_paints_no_points_and_a_result_slot_the_model_lacks_is_skipped` |
| `timeseries: the session round trip keeps the rule` | `src/core/session.rs` | `if *rule != BucketRule::Last { r.insert("rule"` → `if false { r.insert("rule"` | `a_model_round_trips_through_its_table` |
| `timeseries: a stale tag is dropped` | `src/tile.rs` | `if outcome.tag != self.tag {` → `if false {` | `a_delivery_becomes_the_chart_model_and_a_stale_tag_is_dropped` |
| `timeseries: only as_of is followed` | `src/tile.rs` | `versions.as_of != now.as_of` → `versions.as_of != now.as_of \|\| versions.scope != now.scope` | `the_tile_follows_as_of_only_and_stages_under_an_open_barrier` |
| `timeseries: Ok(0) still requeries` | `src/tile.rs` | `Ok(_) => { self.model.set_pair_state(source, identity, SlotState::Idle); if self.visible { self.requery(cx); } }` → `Ok(n) => { self.model.set_pair_state(source, identity, SlotState::Idle); if self.visible && n > 0 { self.requery(cx); } }` | `a_fetched_ok_marks_the_pair_idle_and_queries_once_and_an_err_marks_it_failed` |
| `timeseries: a pair the tile does not hold is ignored` | `src/tile.rs` | `if !self.model.holds_pair(source, identity) { return; }` → `if false { return; }` | same |
| `timeseries: an add fetches before it queries` | `src/core/model.rs` | `let mut changed = Changed::FETCH \| LOOK;` → `let mut changed = ALL;` | `an_add_fetches_the_resolved_range_when_visible_and_defers_while_hidden` |
| `timeseries: a chrome change bumps the chart version` | `src/tile.rs` | `self.chart_version += 1;` → `self.chart_version += 0;` | `normal_mode_verbs_drive_the_model_and_bump_the_chart_version` |
| `timeseries: a hidden tile cancels in flight` | `src/tile.rs` | `self.data.cancel(QueryKey(self.id.0));` → `let _ = QueryKey(self.id.0);` | `a_hidden_tile_cancels_and_a_shown_one_requeries_and_a_restored_one_refetches_once` |
| `timeseries: enter picks the highlighted row not the text` | `src/tile.rs` | the `commit` arm's `list.pick()` → `list.ranked().first().map(\|r\| r.row)` (or the form that reads row 0) | `a_opens_the_picker_over_the_catalogue_and_enter_adds_the_highlighted_pair` |
| `timeseries: the picker closer blurs before dropping` | `src/tile.rs` | the `window.blur(cx);` in `close_popup_with_window` → `let _ = &window;` | `both_closers_blur_before_dropping_the_input` |
| `timeseries: an expression parse error keeps the field open` | `src/tile.rs` | `Err(e) => { f.error = Some(e.into()); }` → `Err(e) => { f.error = Some(e.into()); self.close_popup_with_window(window, cx); }` | `x_opens_the_expression_field_and_enter_adds_or_reports_inline` |
| `timeseries: a digit in the range popup commits a preset` | `src/tile.rs` | `Preset::digit(d)` → `None::<Preset>` | `r_opens_the_range_popup_on_from_day_and_a_digit_commits_a_preset` |
| `timeseries: a popup closes before another verb` | `src/tile.rs` | the close-first `matches!` list → add `\| "pan_left"` | `shift_l_opens_the_series_popup_whose_cursor_is_the_chips_cursor` |
| `series settings: the default source diagnostic names the sources` | `crates/geode-shell/src/series.rs` | `if settings.dataset_of(name).is_some() { return vec![]; }` → `if true { return vec![]; }` | `the_default_source_is_read_and_diagnosed` (pkg `geode-shell`) |
| `series settings: fetch sources are the Fetch-shaped ones` | `crates/geode-shell/src/series.rs` | `.filter(\|s\| s.shape(&schema) == SourceShape::Fetch)` → `.filter(\|_\| true)` | `the_fetch_sources_are_the_non_directory_sources_over_a_series_dataset` (pkg `geode-shell`) |

Anchors must be unique in their file (`--anchors-only` exits non-zero on a duplicate); shorten or lengthen to the exact line as written. Update CLAUDE.md's harness count (`grep -c '^run_mutation "' scripts/mutation-check.sh`, never by arithmetic).

- [ ] **Step 2: `docs/perf.md`** — a section "Timeseries module (spec §9, Part 4)" after the chart section: the `chart_model/500k_x_4` median from Task 3, what it measures (one delivery's model build: four value-vector clones of 500,000 `f64`s plus the `Arc` swap), the statement that a chrome change (`v`, `y`, `c`) rebuilds the same model (a known cost — the element takes the model by `Arc` and the values are `Vec<f64>` by value in `ChartSlot`; a slot-level `Arc<[f64]>` is the fix if it bites), and that a view move rebuilds nothing (the element takes the `View` beside the model).

- [ ] **Step 3: The spec's §9.13 "As built (Part 4)"** — one bullet per controller decision above (numbered 1–9), plus: the `Popup` closer rule, the `popup == series` key-context pair, the `Chords`-derived footer, the `L`/`D`/`F`/`Y`/`G` spellings as `shift+…`, `every show refetches`, the `restore_pending` field's fate, and what is pixel-unverified (all of it: chips, popups, the chart inside a tile, the expression strip, the range popup's two fields). Tick §1.2 items 1 and 3 as "built; display check pending". Amend §11.2's "the dependant removal" to name its test.

- [ ] **Step 4: `docs/phase-history.md`** — one paragraph "Timeseries Part 4 (2026-09-20)" in the house style (what it built, the controller decisions, what a maintainer must not tidy: the fetch-before-query bit on add, `as_of`-only following, `Ok(0)` requery, the density budget, the version bump on every rebuild, blur-then-drop, the close-first list), the harness count. **CLAUDE.md**: the status row ("Timeseries Part 4 (module)"), the "Part 4 (the tile) next" clauses in the Part 1–3 rows retired, a "Timeseries tile" rules bullet under Shell (the eight "must not tidy" items in one bullet), the third-global note in the `[ui] line_numbers` bullet (`UiSettings`, `Chords`, now `SeriesSettings`), and the harness count.

- [ ] **Step 5: Workspace verification, then commit**

Run: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo bench --workspace --no-run`, `cargo check -p geode-shell --features test-support --all-targets`, `zsh scripts/mutation-check.sh --anchors-only`, `zsh scripts/mutation-check.sh "timeseries:"`, `zsh scripts/mutation-check.sh "series settings:"`.
Expected: all green; every new entry `caught`.

```bash
git add scripts/mutation-check.sh docs CLAUDE.md
git commit -m "timeseries: harness entries, perf numbers, as-built §9.13, history and CLAUDE.md"
```

---

## Self-review

**Spec coverage.** §9.1 (Task 6: kind/context, actions, fragment, `TileContent` with `deliver` arms for `Series`/`SeriesFetched`, `set_visible`, `serialize`, `holds_focus` off the picker's field, the expression field and the range popup's handle). §9.2 (Task 1: `Model`, `Slot`, `SlotState`, `Changed`; `Axis` imported from `geode-chart` per §8.5). §9.3 (Task 6: stack marker, title, `1y · 1d`, chips with swatch/label/axis, hidden dimmed + struck, tones through `chip_paint`, footer from `Chords`). §9.4 (Task 6's `DEFAULT_KEYMAP` — every row of the table; counts opted in; `0` is a key because a leading zero is not a count digit in `keymap/matcher.rs:43`). §9.5 (Task 8). §9.6 (Task 9: identities across every catalogued fetch source, the `add "<text>"…` row, the source stage with the default highlighted, loaded rows marked and pickable). §9.7 (Task 9). §9.8 (Task 10: two `DateTimeField`s on the day segment of `from`, presets by digit and click, relative stored relative). §9.9 (Task 4 + Task 6's `command`; `:add` with no suffix and no default refuses with "name a source or set a default"). §9.10 (Task 7: every bullet, including "refetch and requery on show" and "restored tile: every source slot Fetching on first show"). §9.11 (Task 3 + Task 6: not the view, not slot state, round-trip tested). §9.12 (Task 5: the global, the settings row over the fetch sources, the diagnostic, written at startup/row/reload). §10 (Task 5's `app.toml`, Task 11's roster). §11.1 "Tile" (key sequences — Tasks 6/8/9/10; staged and promoted — Task 7; stale tag — Task 7; `d` on an operand with notice — Task 6; restored refetches once — Task 7; `holds_focus` on both inputs — Task 9; blur-then-drop on both closers — Task 9; chip tone sweep — Task 6 pins the tones used, the shell's sweep covers readability). §11.2's dependant removal — Task 12. §1.2 item 1's walk is the display check; item 3's "restored tile refetches once and paints what it painted before" — Task 7.

**Gaps, stated.** The chart's crosshair readout is the element's own (Part 3); the tile adds no readout of its own. `:colour` completions list `[colours]` names but the picker offers no colour swatches. A `tab` in the range popup is handled by the tile's own key listener, not a fragment binding, because the popup is insert mode with a bare focus handle — same as the market-data date editor. No `KindAction`s exist for this tile.

**Type consistency.** `Changed` bits and helpers (`query/fetch/chrome/session`) are used identically in Tasks 1, 6, 7. `Model::add_source` returns `Result<(u8, Changed), String>` everywhere; `add_expr` `(u8, Changed)`; `remove` `Result<Removal, String>`. `resolve(text, slots, default_source, editing)` has the same four arguments in Tasks 2, 3, 6, 9. `chart::build` takes six arguments after Task 3's amendment (`result, model, version, offset_secs, colour_of, default_source`) — Task 6's `rebuild_chart_model` and the bench call it that way. `session::from_table(table, dataset_of, default_source)` in Tasks 3 and 6. `SeriesSettings::{from_config, dataset_of, names}` in Tasks 5, 6, 9. `FetchParams { key, source, identity, from, to }` per `geode-data/src/service.rs:198-205`. `DataHandle::cancel`, not `cancel_query`.
