# Phase 4c, Part 2c — Column Presentation and Named Colours

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a trader personalise every presentation key of a column (label, width, scale, precision, thousands, negative, colour) in a column stage under the Views dialog without forking the desk's view, and define shared **named colours** that resolve against the active theme, edited in a Colours dialog and painted by the blotter.

**Architecture:** A pure colour module in `geode-core` (`colour`: the `colours` doc reader, OKLab/OKLCH conversion, a resolver from a definition and a theme's anchors to sRGB). `Colour::Named(String)` on a column's format. The overlay `view_presentation.toml` reshaped to one `[view.columns.<col>]` table per column, read alongside the legacy keys and written only where a value differs from the desk. In `shell::objectdialog`, `ListItem` carries a whole `ColumnPresentation`, `Stage::Column` is a projection over the same `Draft` (fields swapped, folded back on every change), and `Domain::Colours` is a thin adapter with a live swatch. Definitions travel to the blotter through the bridge like views and resolve at paint time against `cx.theme()` behind a small cache.

**Tech Stack:** Rust, gpui + gpui-component (pinned rev `0e2fb7a`), `toml_edit`, `scripts/mutation-check.sh`.

**Spec:** `docs/superpowers/specs/2026-09-13-geode-phase-4c-part-2c-columns-and-colours-design.md` — **the whole brief**; §2 the model, §3 a column's colour, §4 the overlay, §5 the column stage, §6 the dialog and the blotter, §7 checks, §8 sequencing. Background: the 4c spec (`2026-09-08-geode-phase-4c-config-dialogs-design.md`) §3–§7, §16–§19.8; Phase 3 §6.2; the interaction-model spec §16.

## Global Constraints

- CI runs on **macOS and Windows**: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo bench --workspace --no-run`, `cargo check -p geode-shell --features test-support --all-targets`. Run all five before every commit that touches Rust. Run cargo in the FOREGROUND with a generous timeout; never background a cargo command.
- **TDD**: write the failing test, run it, watch it fail for the right reason, then implement.
- **A mutation entry for every behaviour changed**, appended to `scripts/mutation-check.sh` immediately after the last `run_mutation` entry (before the `if [[ -n "$changed_ref" ]]` block), each naming its covering test as the 6th argument. **Commit before mutating.** Every anchor must occur exactly once in its file (`grep -c -F '<anchor>' <file>` prints `1`); `zsh scripts/mutation-check.sh --anchors-only` must exit 0 before every commit — re-anchor pre-existing entries your edits move. Run new entries by name; the full `--changed=main` run is Task 8's, detached.
- **Never a raw colour in chrome** — every UI colour from `cx.theme()` tokens. A *resolved named colour* is data, not chrome: it may be painted directly, because it was derived from the theme's own anchors.
- `geode-core` never sees a gpui type: `Rgb`, `Anchors`, `Tokens` are plain structs the caller fills from `cx.theme()`.
- `geode-shell` never depends on `geode-data`; no crate but `geode-data` opens a file or socket, except `geode-shell` writing its own config through `config_write`.
- **Every transition in the object dialog is a pure mutation of `mode`/`query`/`text_entry`/`stage`; only `dialog::sync_dialog_text` moves focus or writes the shared `Input`.**
- Doc comments explain WHY, densely. **A comment that contradicts the code is a defect** — `ColumnPresentation.hidden`'s "set only by `ViewPresentationSpec`", `ViewPresentation`'s field docs, `ListItem.width`'s doc, `Colour`'s doc ("`chart_bullish`/`chart_bearish`"), `Draft::list_items`' doc, the browse painter's item-row comments: each sentence a task makes false is rewritten in that task.
- Renaming an object stays unbuilt. No literal hex colours anywhere in config. No header-drag width write-back.
- `FieldKind::Number` gains `step` and `wrap`; every existing `Number` keeps `step: 1, wrap: false`. 2b's refuse-don't-clamp rule for an already out-of-range value holds.
- Palette-only actions (`config::colours`); no new default key binding.

## As-built vocabulary this plan builds on

```rust
// geode-core view.rs
pub enum Colour { None, Sign }                       // derives Copy today; this plan drops Copy on Colour and ColumnFormat
pub struct ColumnFormat { precision: u8, thousands: bool, negative: Negative, colour: Colour, scale: Scale }  // MEASURE / TEXT consts, with(&ColumnPresentation)
pub struct ColumnPresentation { precision: Option<u8>, thousands: Option<bool>, negative: Option<Negative>, colour: Option<Colour>, scale: Option<Scale>, label: Option<String>, width: Option<f32>, hidden: Option<bool> }
pub struct ViewSpec { name, dataset, joins, columns: Vec<ViewColumn>, grouping, sort, presentation: BTreeMap<String, ColumnPresentation>, is_default }  // presentation_of(col)
pub struct ViewPresentation { order: Vec<String>, hidden: BTreeSet<String>, width: BTreeMap<String, f32> }   // this plan replaces hidden/width with columns
pub struct ViewPresentationSpec { views: BTreeMap<String, ViewPresentation> }  // from_doc(doc), apply(&mut [ViewSpec]) -> Vec<Diagnostic>
// views reader: per column, `warn(key, msg)` closure carries `views.<v>.columns.<i>.<key>`; format keys parsed inline at view.rs ~466-527
// geode-core config: load_views(config) -> (Vec<ViewSpec>, Vec<Diagnostic>) [config/load.rs ~96]; merge::atomic_depth(doc) [merge.rs:22]; check_object_name(name)
// Diagnostic { severity, layer, file, message, path: Option<String> } with_path()
// geode-shell objectdialog/mod.rs
pub enum Domain { Views, Groupings, Scopes, Schema, Sources }
pub enum Stage { Browse, Naming, Edit { object: String } }
pub struct ListItem { name, included, width: Option<f32>, kind: Option<String> }
pub enum FieldKind { Text(String), Number { value: i64, min: i64, max: i64 }, Bool(bool), Choice { options, selected }, MultiChoice{..}, OrderedList { items, available } }
pub struct Field { key, label, kind, dest: Destination, layer: Option<Layer> }
pub struct Draft { name, is_new, fields, source, baseline, baseline_source, selected, query, diagnostics, confirm, text_entry: Option<TextEntry> }
// Draft: rows() row_label() visible_rows() selected_row() list_items(key) available_items(key) choice(key) is_dirty() mark_saved()
//        toggle_selected() step_selected(dir) [Number arm at mod.rs ~1515 with the out-of-range refusal] begin/cancel/apply_text_entry
//        row_for_path(doc, path) resolve_list_index(..) flagged_rows(doc) offers_text_entry(domain) writes_by_destination()
// Domain: doc title crumb_noun summary_fn presentation_doc roster prefix_fn writable text_editable parse_text fields draft new_draft to_table validate name_taken
// ObjectDialogState { domain, stage, selected, query, mode, notice, draft, naming_dataset }: enter_edit enter_edit_with leave_edit begin_naming cancel_naming has_previous_stage
// views.rs: DOC, PRESENTATION_DOC, summary, fields (items from load_views' presentation_of), refresh_available, to_table(draft, dest), doc_table, columns_for, presentation_table, doc_baseline, doc_width, validate
// render.rs: open(); handle_key → handle_browse_key/handle_edit_key; NormalCommand::Commit in the edit stage → edit_commit_notice; escape ladder → leave_edit(shell, cx) at two sites (~957, ~1869)
//            enter_edit_stage(shell, name, new, cx); crumb_text(shell); editing_row(shell); revalidate(shell); commit_or_confirm; open_text_field; hint_i; build_edit; item-row painting ~2580-2640 (entry.width read ~2623)
// dialog.rs: badge(label, fg, border, selector, cx); set_title_extra; state_pill/mode_pill/chain_pill/edit_pill
// defaults.rs: action(reg, "config::sources", "Edit sources", "Configuration"); input.rs dispatch arm
// theme.rs: ThemeService { entries: Vec<Rc<ThemeConfig>>, active_name }; load_bundled(); apply_theme: Theme::global_mut(cx).apply_config(config); Theme::change(mode, None, cx)
// gpui-component Theme (Deref<ThemeColor>): red, red_light, yellow, yellow_light, green, green_light, cyan, cyan_light, blue, blue_light, magenta, magenta_light,
//                                            background, foreground, muted_foreground, primary, accent, danger, warning, success, info, chart_1..chart_5, chart_bullish, chart_bearish (all Hsla)
// gpui: Hsla::to_rgb() -> Rgba { r, g, b, a }; Rgba: Into<Hsla>; Rgba::try_from("#rrggbb")
// geode-blotter: content.rs BlotterFactory { data, views: Rc<RefCell<Vec<ViewSpec>>>, schema, dims, find_style, stale_after } new(..) set_views(..) create(..)
//                tile.rs BlotterTile::new(tile, frame, data, views, schema, dims, find_style, stale_after, restored, window, cx)
//                core/plan.rs PlannedColumn { label, kind, format: ColumnFormat, .. }; plan builds label with scale suffix, format = kind default .with(&presentation)
//                delegate.rs impl TableDelegate for BlotterDelegate (line ~549): column(col_ix, cx) -> Column; render_td (`let colour = plan.columns[col_ix].format.colour` ~724; paints Sign at ~841); no render_th override yet
// geode-app bridge.rs: DataSetup { config: DataServiceConfig, views, dimensions, diagnostics }; data_setup(config, db_path); start(..) builds BlotterFactory::new(handle, views, schema, dims, find_style, stale_after);
//                     ConfigReloaded arm: load_views(config) → factory.set_views(..) / set_dims / set_schema
// geode-shell hot_reload.rs ~275: views_changed = changed("views") || changed("view_presentation") || changed("dimensions"); ~413: if views_changed { cx.emit(ShellEvent::ConfigReloaded) }
// tests: shell/tests/objectdialog.rs — dialog_test_shell_with/in_dir, dialog_state, edit_draft, flush_config_write, services_with_views, open_views_dialog, services_with_sources;
//        blotter tile.rs tests: views() fixture from a views doc; geode-core view.rs tests use merge_docs + LayerDoc::builtin
```

---

## File map

| File | Responsibility after this plan |
|---|---|
| `crates/geode-core/src/colour/mod.rs` | **new** — `Rgb`, `Tone`, `Token`, `Definition`, `NamedColours::from_doc`, `Anchors`, `Tokens`, `resolve`, `interpolate_hue` (T1) |
| `crates/geode-core/src/colour/oklab.rs` | **new** — sRGB ↔ linear ↔ OKLab ↔ OKLCH, gamut clip by chroma (T1) |
| `crates/geode-core/src/view.rs` | `Colour::Named`; `ColumnPresentation::parse_format_keys`/`parse_column_keys` shared by both readers (T1, T2); `ViewPresentation.columns`, both spellings, `apply` (T2) |
| `crates/geode-core/src/config/load.rs` | `load_views` cross-checks named colours with paths (T1) |
| `crates/geode-core/src/config/merge.rs` | `colours` atomic at depth 1 (T1) |
| `crates/geode-core/src/lib.rs` | `pub mod colour` (T1) |
| `crates/geode-shell/src/shell/objectdialog/mod.rs` | `Number.step/wrap` (T3); `ListItem.presentation` (T3); `Stage::Column`, `Draft::{parent_fields, column, enter_column, fold_column, leave_column, field_by_key}` and the parent fallback in `list_items`/`available_items`/`choice` (T4); `Domain::Colours` arms, `reserved_names` (T5) |
| `crates/geode-shell/src/shell/objectdialog/views.rs` | items carry presentation; `presentation_table` writes `[view.columns.<col>]` with differing keys only; `column_fields`, `parse_text`/`text_editable` for `label`/`width`; `column_summary` (T3, T4) |
| `crates/geode-shell/src/shell/objectdialog/colours.rs` | **new** — the Colours adapter (T5) |
| `crates/geode-shell/src/shell/objectdialog/render.rs` | member-row summary (T3); column stage entry/exit, crumb, fold on revalidate, `row_for_path` in the stage (T4); swatches (T5) |
| `crates/geode-shell/src/shell/colours.rs` | **new** — `anchors_from_theme`, `tokens_from_theme`, `to_hsla` (T5) |
| `crates/geode-shell/src/shell/dialog.rs` | `swatch(colour, selector, cx)` (T5) |
| `crates/geode-shell/src/defaults.rs`, `shell/input.rs` | `config::colours` (T5) |
| `crates/geode-shell/src/shell/hot_reload.rs` | `ConfigReloaded` on a `colours` change (T6) |
| `crates/geode-app/src/bridge.rs` | `DataSetup.colours`; factory gets colours at start and on reload (T6) |
| `crates/geode-blotter/src/content.rs`, `tile.rs`, `delegate.rs`, `colour_cache.rs` (**new**) | colours shared like views; per-frame resolution behind a cache; cell and header painting (T6) |
| `crates/geode-shell/src/theme.rs` | the bundled-theme anchor-arc and contrast test (T7) |
| `scripts/mutation-check.sh`, `CLAUDE.md`, the spec's "as built" | T8 |

---

### Task 1: `geode_core::colour` — OKLab, the resolver, the doc reader; `Colour::Named`; the load-time cross-check

**Files:**
- Create: `crates/geode-core/src/colour/mod.rs`, `crates/geode-core/src/colour/oklab.rs`
- Modify: `crates/geode-core/src/lib.rs` (`pub mod colour;`), `crates/geode-core/src/config/merge.rs:22` (add `"colours"`), `crates/geode-core/src/view.rs` (`Colour`, the reader's `colour` arm ~line 500, `ColumnFormat`/`Colour` derives), `crates/geode-core/src/config/load.rs` (`load_views`)
- Modify: `crates/geode-blotter/src/delegate.rs:724-728, 841-845` (the `Copy` removal: `.map(|c| &c.format.colour)` and `Some(Colour::Sign)` → `Some(Colour::Sign)` on a reference — `match (colour, cell.sign) { (Some(Colour::Sign), ..)` works on `Option<&Colour>` with a `&Colour` pattern; let the compiler guide; **a `Colour::Named(_)` arm here is Task 6's — for now it falls to the `_ => el` arm**)
- Test: each new file's `mod tests`; `view.rs` tests; `config/load.rs` tests

**Interfaces:**
- Produces:
  ```rust
  // geode_core::colour
  #[derive(Debug, Clone, Copy, PartialEq)] pub struct Rgb { pub r: f32, pub g: f32, pub b: f32 }     // sRGB 0..=1
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Tone { Normal, Light }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Token { Foreground, Muted, Primary, Accent, Danger, Warning, Success, Info, Chart(u8) /* 1..=5 */, Bullish, Bearish }
  impl Token { pub const ALL: [Token; 15]; pub fn parse(s: &str) -> Option<Token>; pub fn name(self) -> &'static str /* "chart.3", "chart.bullish", "muted" … */ }
  #[derive(Debug, Clone, PartialEq)] pub enum Definition { Hue { degrees: f32, tone: Tone }, Token(Token) }
  impl Definition { pub fn summary(&self) -> String /* "hue 240", "hue 210 · light", "token chart.bullish" */ }
  #[derive(Debug, Clone, Default, PartialEq)] pub struct NamedColours { by_name: BTreeMap<String, Definition> }
  impl NamedColours { pub fn from_doc(doc: &MergedDoc) -> (NamedColours, Vec<Diagnostic>); pub fn get(&self, name: &str) -> Option<&Definition>; pub fn names(&self) -> impl Iterator<Item = &str>; pub fn is_empty(&self) -> bool; pub fn insert(&mut self, name: String, def: Definition) }
  pub const RESERVED_NAMES: [&str; 2] = ["none", "sign"];
  pub const ANCHOR_DEGREES: [f32; 6] = [0.0, 60.0, 120.0, 180.0, 240.0, 300.0];   // red yellow green cyan blue magenta
  #[derive(Debug, Clone, Copy, PartialEq)] pub struct Anchors { pub normal: [Rgb; 6], pub light: [Rgb; 6] }
  #[derive(Debug, Clone, Copy, PartialEq)] pub struct Tokens { pub foreground: Rgb, pub muted: Rgb, pub primary: Rgb, pub accent: Rgb, pub danger: Rgb, pub warning: Rgb, pub success: Rgb, pub info: Rgb, pub chart: [Rgb; 5], pub bullish: Rgb, pub bearish: Rgb }
  impl Tokens { pub fn get(&self, token: Token) -> Rgb }
  pub fn interpolate_hue(degrees: f32, tone: Tone, anchors: &Anchors) -> Rgb;
  pub fn resolve(def: &Definition, anchors: &Anchors, tokens: &Tokens) -> Rgb;
  pub fn contrast_ratio(a: Rgb, b: Rgb) -> f32;                                  // WCAG, for Task 7
  // geode_core::colour::oklab
  #[derive(Debug, Clone, Copy, PartialEq)] pub struct Lab { pub l: f32, pub a: f32, pub b: f32 }
  #[derive(Debug, Clone, Copy, PartialEq)] pub struct Lch { pub l: f32, pub c: f32, pub h: f32 /* radians */ }
  pub fn srgb_to_oklab(rgb: Rgb) -> Lab; pub fn oklab_to_srgb(lab: Lab) -> Rgb /* unclipped, may leave 0..1 */;
  pub fn lab_to_lch(lab: Lab) -> Lch; pub fn lch_to_lab(lch: Lch) -> Lab;
  pub fn to_srgb_in_gamut(lch: Lch) -> Rgb;                                       // pulls chroma in until every channel is in 0..=1
  pub fn relative_luminance(rgb: Rgb) -> f32;
  // geode_core::view
  pub enum Colour { None, Sign, Named(String) }                                   // Clone, PartialEq, Eq — no longer Copy; ColumnFormat likewise
  ```
- `load_views` pushes a `Severity::Warning` with path `views.<view>.columns.<i>.format.colour` (the file index, checked BEFORE the overlay permutes columns) for a view column naming a colour the `colours` doc lacks, and `view_presentation.<view>.columns.<col>.colour` for an overlay entry doing the same (Task 2 adds the overlay's `columns`; in this task the overlay has no `colour` key yet, so only the first check is live — write the second when Task 2 lands, and say so in the doc comment).

- [ ] **Step 1: Failing tests for the conversion** (`oklab.rs`, `mod tests`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool { (a - b).abs() < 2e-3 }

    /// Björn Ottosson's published reference values for OKLab.
    #[test]
    fn oklab_matches_the_reference_values() {
        let white = srgb_to_oklab(Rgb { r: 1.0, g: 1.0, b: 1.0 });
        assert!(close(white.l, 1.0) && close(white.a, 0.0) && close(white.b, 0.0), "{white:?}");
        let red = srgb_to_oklab(Rgb { r: 1.0, g: 0.0, b: 0.0 });
        assert!(close(red.l, 0.6280) && close(red.a, 0.2249) && close(red.b, 0.1258), "{red:?}");
        let green = srgb_to_oklab(Rgb { r: 0.0, g: 1.0, b: 0.0 });
        assert!(close(green.l, 0.8664) && close(green.a, -0.2339) && close(green.b, 0.1795), "{green:?}");
        let blue = srgb_to_oklab(Rgb { r: 0.0, g: 0.0, b: 1.0 });
        assert!(close(blue.l, 0.4520) && close(blue.a, -0.0325) && close(blue.b, -0.3115), "{blue:?}");
    }

    #[test]
    fn oklab_round_trips_a_grid_of_srgb_values() {
        for r in 0..=4 {
            for g in 0..=4 {
                for b in 0..=4 {
                    let rgb = Rgb { r: r as f32 / 4.0, g: g as f32 / 4.0, b: b as f32 / 4.0 };
                    let back = oklab_to_srgb(srgb_to_oklab(rgb));
                    assert!(close(back.r, rgb.r) && close(back.g, rgb.g) && close(back.b, rgb.b), "{rgb:?} -> {back:?}");
                }
            }
        }
    }

    #[test]
    fn lch_round_trips_and_keeps_hue_in_range() {
        let lab = srgb_to_oklab(Rgb { r: 0.2, g: 0.4, b: 0.9 });
        let lch = lab_to_lch(lab);
        assert!((0.0..std::f32::consts::TAU).contains(&lch.h));
        let back = lch_to_lab(lch);
        assert!(close(back.a, lab.a) && close(back.b, lab.b));
    }

    /// A very saturated OKLCH colour has no sRGB counterpart; clipping
    /// pulls chroma, never lightness or hue.
    #[test]
    fn gamut_clip_pulls_chroma_and_keeps_lightness_and_hue() {
        let wild = Lch { l: 0.7, c: 0.5, h: 0.5 };
        let rgb = to_srgb_in_gamut(wild);
        assert!((0.0..=1.0).contains(&rgb.r) && (0.0..=1.0).contains(&rgb.g) && (0.0..=1.0).contains(&rgb.b), "{rgb:?}");
        let got = lab_to_lch(srgb_to_oklab(rgb));
        assert!(close(got.l, 0.7), "lightness kept: {got:?}");
        assert!((got.h - 0.5).abs() < 0.02, "hue kept: {got:?}");
        assert!(got.c < 0.5, "chroma pulled in: {got:?}");
    }

    #[test]
    fn relative_luminance_is_wcag() {
        assert!(close(relative_luminance(Rgb { r: 1.0, g: 1.0, b: 1.0 }), 1.0));
        assert!(close(relative_luminance(Rgb { r: 0.0, g: 0.0, b: 0.0 }), 0.0));
        assert!(close(relative_luminance(Rgb { r: 1.0, g: 0.0, b: 0.0 }), 0.2126));
    }
}
```

- [ ] **Step 2: Run to confirm they fail** — `cargo test -p geode-core colour::oklab 2>&1 | head` → compile errors (module missing).

- [ ] **Step 3: Write `oklab.rs`**

```rust
//! sRGB ↔ OKLab ↔ OKLCH (spec §2.4). Pure arithmetic, no dependency,
//! Björn Ottosson's published matrices. OKLCH is the space named colours
//! interpolate in (§2.2): an HSL midpoint of a theme's yellow and blue is a
//! mud of the wrong lightness, and an OKLab chord passes through lower
//! chroma; OKLCH keeps lightness and chroma even along the arc.

use super::Rgb;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lab { pub l: f32, pub a: f32, pub b: f32 }

/// Hue in radians, `0..TAU`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lch { pub l: f32, pub c: f32, pub h: f32 }

fn to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn to_srgb_channel(l: f32) -> f32 {
    if l <= 0.0031308 { 12.92 * l } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 }
}

pub fn srgb_to_oklab(rgb: Rgb) -> Lab {
    let (r, g, b) = (to_linear(rgb.r), to_linear(rgb.g), to_linear(rgb.b));
    let l = 0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b;
    let m = 0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b;
    let s = 0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b;
    let (l_, m_, s_) = (l.cbrt(), m.cbrt(), s.cbrt());
    Lab {
        l: 0.210_454_255_3 * l_ + 0.793_617_785_0 * m_ - 0.004_072_046_8 * s_,
        a: 1.977_998_495_1 * l_ - 2.428_592_205_0 * m_ + 0.450_593_709_9 * s_,
        b: 0.025_904_037_1 * l_ + 0.782_771_766_2 * m_ - 0.808_675_766_0 * s_,
    }
}

/// Unclipped: a saturated OKLab colour can land outside `0..=1`.
pub fn oklab_to_srgb(lab: Lab) -> Rgb {
    let l_ = lab.l + 0.396_337_777_4 * lab.a + 0.215_803_757_3 * lab.b;
    let m_ = lab.l - 0.105_561_345_8 * lab.a - 0.063_854_172_8 * lab.b;
    let s_ = lab.l - 0.089_484_177_5 * lab.a - 1.291_485_548_0 * lab.b;
    let (l, m, s) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let r = 4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s;
    let g = -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s;
    let b = -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701_0 * s;
    Rgb { r: to_srgb_channel(r), g: to_srgb_channel(g), b: to_srgb_channel(b) }
}

pub fn lab_to_lch(lab: Lab) -> Lch {
    let c = (lab.a * lab.a + lab.b * lab.b).sqrt();
    let h = lab.b.atan2(lab.a).rem_euclid(std::f32::consts::TAU);
    Lch { l: lab.l, c, h }
}

pub fn lch_to_lab(lch: Lch) -> Lab {
    Lab { l: lch.l, a: lch.c * lch.h.cos(), b: lch.c * lch.h.sin() }
}

fn in_gamut(rgb: Rgb) -> bool {
    let ok = |c: f32| (-0.0005..=1.0005).contains(&c);
    ok(rgb.r) && ok(rgb.g) && ok(rgb.b)
}

fn clamp01(rgb: Rgb) -> Rgb {
    Rgb { r: rgb.r.clamp(0.0, 1.0), g: rgb.g.clamp(0.0, 1.0), b: rgb.b.clamp(0.0, 1.0) }
}

/// Convert, and when the colour has no sRGB counterpart pull its chroma
/// in (a bisection over `0..=c`, sixteen steps) until it has one. Chroma,
/// never lightness or hue: the trader asked for a hue at a lightness, and
/// a channel clamp would shift both.
pub fn to_srgb_in_gamut(lch: Lch) -> Rgb {
    let direct = oklab_to_srgb(lch_to_lab(lch));
    if in_gamut(direct) {
        return clamp01(direct);
    }
    let (mut lo, mut hi) = (0.0_f32, lch.c);
    for _ in 0..16 {
        let mid = (lo + hi) / 2.0;
        if in_gamut(oklab_to_srgb(lch_to_lab(Lch { c: mid, ..lch }))) { lo = mid } else { hi = mid }
    }
    clamp01(oklab_to_srgb(lch_to_lab(Lch { c: lo, ..lch })))
}

/// WCAG relative luminance of an sRGB colour.
pub fn relative_luminance(rgb: Rgb) -> f32 {
    0.2126 * to_linear(rgb.r) + 0.7152 * to_linear(rgb.g) + 0.0722 * to_linear(rgb.b)
}
```

(The reference values in the test come from Ottosson's table; if a value differs beyond `2e-3` after a careful transcription of the matrices above, check the transcription against https://bottosson.github.io/posts/oklab/ — do not loosen the tolerance.)

- [ ] **Step 4: Failing tests for the model** (`colour/mod.rs`, `mod tests`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};

    fn doc(text: &str) -> crate::config::MergedDoc {
        merge_docs("colours", &[LayerDoc::builtin("colours", text).unwrap()])
    }
    fn grey(v: f32) -> Rgb { Rgb { r: v, g: v, b: v } }
    /// Six distinct saturated anchors, light tone brighter.
    fn anchors() -> Anchors {
        let normal = [
            Rgb { r: 0.8, g: 0.2, b: 0.2 }, Rgb { r: 0.8, g: 0.8, b: 0.2 }, Rgb { r: 0.2, g: 0.8, b: 0.2 },
            Rgb { r: 0.2, g: 0.8, b: 0.8 }, Rgb { r: 0.2, g: 0.2, b: 0.8 }, Rgb { r: 0.8, g: 0.2, b: 0.8 },
        ];
        let light = normal.map(|c| Rgb { r: c.r * 0.5 + 0.5, g: c.g * 0.5 + 0.5, b: c.b * 0.5 + 0.5 });
        Anchors { normal, light }
    }
    fn tokens() -> Tokens {
        Tokens { foreground: grey(0.9), muted: grey(0.6), primary: grey(0.5), accent: grey(0.4), danger: grey(0.3),
                 warning: grey(0.35), success: grey(0.45), info: grey(0.55), chart: [grey(0.1), grey(0.2), grey(0.3), grey(0.4), grey(0.5)],
                 bullish: Rgb { r: 0.0, g: 1.0, b: 0.0 }, bearish: Rgb { r: 1.0, g: 0.0, b: 0.0 } }
    }

    #[test]
    fn reads_hue_tone_and_token_and_refuses_both_or_neither() {
        let (colours, diags) = NamedColours::from_doc(&doc(
            "[delta]\nhue = 240\n[gamma]\nhue = 210\ntone = \"light\"\n[pnl]\ntoken = \"chart.bullish\"\n\
             [both]\nhue = 1\ntoken = \"danger\"\n[neither]\ntone = \"light\"\n[wrap]\nhue = 360\n",
        ));
        assert_eq!(colours.get("delta"), Some(&Definition::Hue { degrees: 240.0, tone: Tone::Normal }));
        assert_eq!(colours.get("gamma"), Some(&Definition::Hue { degrees: 210.0, tone: Tone::Light }));
        assert_eq!(colours.get("pnl"), Some(&Definition::Token(Token::Bullish)));
        assert_eq!(colours.get("wrap"), Some(&Definition::Hue { degrees: 0.0, tone: Tone::Normal }), "360 is 0");
        assert!(colours.get("both").is_none() && colours.get("neither").is_none());
        let errors: Vec<&str> = diags.iter().filter(|d| d.severity == crate::config::Severity::Error).filter_map(|d| d.path.as_deref()).collect();
        assert_eq!(errors, vec!["colours.both", "colours.neither"]);
        assert_eq!(colours.names().collect::<Vec<_>>(), vec!["delta", "gamma", "pnl", "wrap"], "sorted");
    }

    #[test]
    fn reserved_names_and_bad_values_are_refused_with_paths() {
        let (colours, diags) = NamedColours::from_doc(&doc(
            "[sign]\nhue = 1\n[big]\nhue = 400\n[tok]\ntoken = \"nope\"\n[tone]\ntoken = \"danger\"\ntone = \"light\"\n",
        ));
        assert!(colours.get("sign").is_none() && colours.get("big").is_none() && colours.get("tok").is_none());
        assert_eq!(colours.get("tone"), Some(&Definition::Token(Token::Danger)), "tone beside token is ignored with a warning");
        let paths: Vec<&str> = diags.iter().filter_map(|d| d.path.as_deref()).collect();
        assert!(paths.contains(&"colours.sign") && paths.contains(&"colours.big.hue") && paths.contains(&"colours.tok.token") && paths.contains(&"colours.tone.tone"), "{paths:?}");
    }

    #[test]
    fn an_anchor_hue_is_the_themes_own_colour_exactly() {
        let a = anchors();
        assert_eq!(interpolate_hue(240.0, Tone::Normal, &a), a.normal[4]);
        assert_eq!(interpolate_hue(0.0, Tone::Light, &a), a.light[0]);
        assert_eq!(interpolate_hue(360.0, Tone::Normal, &a), a.normal[0]);
    }

    #[test]
    fn a_hue_between_anchors_interpolates_along_the_shorter_arc() {
        let a = anchors();
        let mid = interpolate_hue(30.0, Tone::Normal, &a);
        let lch = oklab::lab_to_lch(oklab::srgb_to_oklab(mid));
        let red = oklab::lab_to_lch(oklab::srgb_to_oklab(a.normal[0]));
        let yellow = oklab::lab_to_lch(oklab::srgb_to_oklab(a.normal[1]));
        assert!(lch.h > red.h.min(yellow.h) && lch.h < red.h.max(yellow.h), "between red and yellow: {lch:?}");
        assert!(lch.c > 0.6 * red.c.min(yellow.c), "chroma kept, not greyed: {lch:?}");
        // 350 → 10 passes through red, not the long way round through cyan.
        let near_red = interpolate_hue(350.0, Tone::Normal, &a);
        let magenta = oklab::lab_to_lch(oklab::srgb_to_oklab(a.normal[5]));
        let got = oklab::lab_to_lch(oklab::srgb_to_oklab(near_red));
        let arc = |x: f32, y: f32| ((x - y + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI).abs();
        assert!(arc(got.h, red.h) < arc(magenta.h, red.h), "closer to red than magenta is: {got:?}");
    }

    #[test]
    fn resolve_uses_the_token_field_and_the_tone_anchors() {
        let (a, t) = (anchors(), tokens());
        assert_eq!(resolve(&Definition::Token(Token::Bearish), &a, &t), t.bearish);
        assert_eq!(resolve(&Definition::Token(Token::Chart(3)), &a, &t), t.chart[2]);
        assert_eq!(resolve(&Definition::Hue { degrees: 120.0, tone: Tone::Light }, &a, &t), a.light[2]);
    }

    #[test]
    fn contrast_ratio_is_wcag() {
        let ratio = contrast_ratio(Rgb { r: 1.0, g: 1.0, b: 1.0 }, Rgb { r: 0.0, g: 0.0, b: 0.0 });
        assert!((ratio - 21.0).abs() < 0.01, "{ratio}");
    }

    #[test]
    fn token_names_round_trip() {
        for token in Token::ALL {
            assert_eq!(Token::parse(token.name()), Some(token), "{token:?}");
        }
        assert_eq!(Token::parse("chart.6"), None);
        assert_eq!(Definition::Hue { degrees: 210.0, tone: Tone::Light }.summary(), "hue 210 · light");
        assert_eq!(Definition::Token(Token::Bullish).summary(), "token chart.bullish");
    }
}
```

- [ ] **Step 5: Write `colour/mod.rs`**

```rust
//! Named colours (Part 2c spec §2): a config doc of shared colours, each
//! either a hue on a canonical wheel that the active theme transforms, or
//! one of the theme's own semantic tokens. Pure: the caller (the shell's
//! swatches, the blotter's cells) reads the theme's twelve base hues and
//! its token colours into [`Anchors`]/[`Tokens`] and calls [`resolve`];
//! nothing here knows a gpui type. A chart resolves a named colour the
//! same way the blotter does, which is the whole point of the doc.

pub mod oklab;

use crate::config::{Diagnostic, MergedDoc, Severity, check_object_name};
use oklab::{lab_to_lch, lch_to_lab, srgb_to_oklab, to_srgb_in_gamut, Lch};
use std::collections::BTreeMap;
use std::f32::consts::{PI, TAU};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb { pub r: f32, pub g: f32, pub b: f32 }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone { Normal, Light }

/// The theme colours a definition may name directly (spec §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token { Foreground, Muted, Primary, Accent, Danger, Warning, Success, Info, Chart(u8), Bullish, Bearish }

impl Token {
    pub const ALL: [Token; 15] = [
        Token::Foreground, Token::Muted, Token::Primary, Token::Accent, Token::Danger, Token::Warning,
        Token::Success, Token::Info, Token::Chart(1), Token::Chart(2), Token::Chart(3), Token::Chart(4),
        Token::Chart(5), Token::Bullish, Token::Bearish,
    ];

    pub fn parse(s: &str) -> Option<Token> {
        Token::ALL.into_iter().find(|t| t.name() == s)
    }

    pub fn name(self) -> &'static str {
        match self {
            Token::Foreground => "foreground", Token::Muted => "muted", Token::Primary => "primary",
            Token::Accent => "accent", Token::Danger => "danger", Token::Warning => "warning",
            Token::Success => "success", Token::Info => "info", Token::Chart(1) => "chart.1",
            Token::Chart(2) => "chart.2", Token::Chart(3) => "chart.3", Token::Chart(4) => "chart.4",
            Token::Chart(_) => "chart.5", Token::Bullish => "chart.bullish", Token::Bearish => "chart.bearish",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Definition {
    Hue { degrees: f32, tone: Tone },
    Token(Token),
}

impl Definition {
    /// The browse summary: `hue 240`, `hue 210 · light`, `token chart.bullish`.
    pub fn summary(&self) -> String {
        match self {
            Definition::Hue { degrees, tone: Tone::Normal } => format!("hue {}", *degrees as i64),
            Definition::Hue { degrees, tone: Tone::Light } => format!("hue {} · light", *degrees as i64),
            Definition::Token(t) => format!("token {}", t.name()),
        }
    }
}

/// A column's `colour` key already spells these two.
pub const RESERVED_NAMES: [&str; 2] = ["none", "sign"];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NamedColours { by_name: BTreeMap<String, Definition> }

impl NamedColours {
    pub fn from_doc(doc: &MergedDoc) -> (NamedColours, Vec<Diagnostic>) {
        let mut out = NamedColours::default();
        let mut diags = Vec::new();
        let diag = |severity: Severity, path: String, m: String| Diagnostic {
            severity, layer: None, file: None, message: m, path: Some(path),
        };
        for (name, value) in &doc.value {
            if name == "config_version" { continue; }
            let at = |suffix: &str| if suffix.is_empty() { format!("colours.{name}") } else { format!("colours.{name}.{suffix}") };
            if RESERVED_NAMES.contains(&name.as_str()) || check_object_name(name).is_err() {
                diags.push(diag(Severity::Error, at(""), format!("colour '{name}': the name is reserved or not a valid object name — dropped")));
                continue;
            }
            let Some(table) = value.as_table() else {
                diags.push(diag(Severity::Error, at(""), format!("colour '{name}': not a table — dropped")));
                continue;
            };
            let hue = table.get("hue");
            let token = table.get("token");
            let definition = match (hue, token) {
                (Some(_), Some(_)) => { diags.push(diag(Severity::Error, at(""), format!("colour '{name}': both 'hue' and 'token' — a colour is one or the other; dropped"))); continue; }
                (None, None) => { diags.push(diag(Severity::Error, at(""), format!("colour '{name}': neither 'hue' nor 'token'; dropped"))); continue; }
                (Some(h), None) => {
                    let Some(degrees) = h.as_float().or_else(|| h.as_integer().map(|i| i as f64)) else {
                        diags.push(diag(Severity::Error, at("hue"), format!("colour '{name}': 'hue' must be a number (got {h}); dropped")));
                        continue;
                    };
                    if !(0.0..=360.0).contains(&degrees) {
                        diags.push(diag(Severity::Error, at("hue"), format!("colour '{name}': 'hue' must be 0..360 (got {degrees}); dropped")));
                        continue;
                    }
                    let tone = match table.get("tone").and_then(|v| v.as_str()) {
                        None | Some("normal") => Tone::Normal,
                        Some("light") => Tone::Light,
                        Some(other) => {
                            diags.push(diag(Severity::Warning, at("tone"), format!("colour '{name}': 'tone' must be \"normal\" or \"light\" (got {other:?}); using normal")));
                            Tone::Normal
                        }
                    };
                    Definition::Hue { degrees: (degrees % 360.0) as f32, tone }
                }
                (None, Some(t)) => {
                    if table.get("tone").is_some() {
                        diags.push(diag(Severity::Warning, at("tone"), format!("colour '{name}': 'tone' has no effect beside 'token'; ignored")));
                    }
                    match t.as_str().and_then(Token::parse) {
                        Some(token) => Definition::Token(token),
                        None => {
                            diags.push(diag(Severity::Error, at("token"), format!("colour '{name}': unknown token {t}; dropped")));
                            continue;
                        }
                    }
                }
            };
            out.by_name.insert(name.clone(), definition);
        }
        (out, diags)
    }

    pub fn get(&self, name: &str) -> Option<&Definition> { self.by_name.get(name) }
    pub fn names(&self) -> impl Iterator<Item = &str> { self.by_name.keys().map(String::as_str) }
    pub fn is_empty(&self) -> bool { self.by_name.is_empty() }
    pub fn insert(&mut self, name: String, def: Definition) { self.by_name.insert(name, def); }
}

/// red, yellow, green, cyan, blue, magenta — the canonical wheel (§2.2).
pub const ANCHOR_DEGREES: [f32; 6] = [0.0, 60.0, 120.0, 180.0, 240.0, 300.0];

/// A theme's twelve base hues, in `ANCHOR_DEGREES` order, per tone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchors { pub normal: [Rgb; 6], pub light: [Rgb; 6] }

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tokens {
    pub foreground: Rgb, pub muted: Rgb, pub primary: Rgb, pub accent: Rgb, pub danger: Rgb,
    pub warning: Rgb, pub success: Rgb, pub info: Rgb, pub chart: [Rgb; 5], pub bullish: Rgb, pub bearish: Rgb,
}

impl Tokens {
    pub fn get(&self, token: Token) -> Rgb {
        match token {
            Token::Foreground => self.foreground, Token::Muted => self.muted, Token::Primary => self.primary,
            Token::Accent => self.accent, Token::Danger => self.danger, Token::Warning => self.warning,
            Token::Success => self.success, Token::Info => self.info,
            Token::Chart(n) => self.chart[(n.clamp(1, 5) - 1) as usize],
            Token::Bullish => self.bullish, Token::Bearish => self.bearish,
        }
    }
}

/// The hue's two bracketing anchors in the requested tone, interpolated
/// in OKLCH: lightness and chroma linearly, hue along the shorter arc
/// (§2.2). `t == 0` returns the anchor itself, untouched, so a trader who
/// asks for 240 gets the theme's own blue.
pub fn interpolate_hue(degrees: f32, tone: Tone, anchors: &Anchors) -> Rgb {
    let ring = match tone { Tone::Normal => &anchors.normal, Tone::Light => &anchors.light };
    let h = degrees.rem_euclid(360.0);
    let i = ((h / 60.0).floor() as usize) % 6;
    let j = (i + 1) % 6;
    let t = (h - ANCHOR_DEGREES[i]) / 60.0;
    if t <= 0.0 {
        return ring[i];
    }
    let a = lab_to_lch(srgb_to_oklab(ring[i]));
    let b = lab_to_lch(srgb_to_oklab(ring[j]));
    let dh = (b.h - a.h + PI).rem_euclid(TAU) - PI;   // the shorter arc
    let lch = Lch { l: a.l + (b.l - a.l) * t, c: a.c + (b.c - a.c) * t, h: (a.h + dh * t).rem_euclid(TAU) };
    to_srgb_in_gamut(lch)
}

pub fn resolve(def: &Definition, anchors: &Anchors, tokens: &Tokens) -> Rgb {
    match def {
        Definition::Hue { degrees, tone } => interpolate_hue(*degrees, *tone, anchors),
        Definition::Token(token) => tokens.get(*token),
    }
}

/// WCAG contrast ratio, `1..=21`.
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (oklab::relative_luminance(a), oklab::relative_luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}
```

Add `pub mod colour;` to `lib.rs`; add `| "colours"` to `merge.rs`'s depth-1 list with a comment (`// colours (Part 2c §2.1): one named colour per table`). Run `cargo test -p geode-core colour` — all PASS.

- [ ] **Step 6: `Colour::Named` and the reader** (`view.rs`)

Failing test first (`view.rs` tests):

```rust
    #[test]
    fn a_column_colour_may_name_a_named_colour() {
        let doc = merge_docs("views", &[LayerDoc::builtin("views",
            "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\nformat = { colour = \"delta\" }\n").unwrap()]);
        let (views, diags) = ViewSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(views[0].presentation_of("npv").colour, Some(Colour::Named("delta".to_string())));
    }
```

Then: `Colour` becomes `#[derive(Debug, Clone, PartialEq, Eq)] pub enum Colour { None, Sign, Named(String) }` with the doc rewritten ("`sign` paints by sign with `chart_bullish`/`chart_bearish`; a name is a named colour (Part 2c §3), painting the header and every additive value"); `ColumnFormat` drops `Copy`; the reader's `colour` arm: `Some("none") => .. , Some("sign") => .., Some(name) => p.colour = Some(Colour::Named(name.to_string())), None => warn(..)`. Fix every `Copy` use the compiler reports (`ColumnFormat::with` takes `self` — make it `&self` or clone; the blotter's `.map(|c| c.format.colour)` → `.map(|c| c.format.colour.clone())` and `f.colour` asserts in tests). Run `cargo test --workspace` — green.

- [ ] **Step 7: The cross-check in `load_views`** (`config/load.rs`)

Failing test:

```rust
    #[test]
    fn a_column_naming_an_unknown_colour_warns_with_its_path() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "views.toml", "config_version = 1\n[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n[[tree.columns]]\nname = \"d\"\nformat = { colour = \"ghost\" }\n");
        write(dir.path(), "colours.toml", "config_version = 1\n[delta]\nhue = 240\n");
        let config = Config::load(&ConfigSources { builtin: vec![], desk: Some(dir.path().to_path_buf()), user: None });
        let (_views, diags) = load_views(&config);
        assert!(diags.iter().any(|d| d.path.as_deref() == Some("views.tree.columns.1.format.colour") && d.message.contains("ghost")), "{diags:?}");
    }
```

Implement in `load_views`, after `ViewSpec::from_doc` and BEFORE the overlay's `apply` (the index must be the file's):

```rust
    let colours = config.doc("colours").map(|d| crate::colour::NamedColours::from_doc(d).0).unwrap_or_default();
    for view in &views {
        for (i, column) in view.columns.iter().enumerate() {
            if let Some(Colour::Named(name)) = &view.presentation_of(column.name()).colour
                && colours.get(name).is_none()
            {
                diags.push(Diagnostic {
                    severity: Severity::Warning, layer: None, file: None,
                    message: format!("view '{}': column '{}' names colour '{name}', which colours.toml does not define — painted in foreground", view.name, column.name()),
                    path: Some(format!("views.{}.columns.{i}.format.colour", view.name)),
                });
            }
        }
    }
```

(Task 2 adds the overlay's own check beside it.) Run — PASS.

- [ ] **Step 8: Harness entries**

```bash
# 2c §2.1: a colour with both hue and token is dropped, not merged.
run_mutation "colour: both hue and token is refused" \
  crates/geode-core/src/colour/mod.rs \
  '                (Some(_), Some(_)) => { diags.push(diag(Severity::Error, at(""), format!("colour '"'"'{name}'"'"': both '"'"'hue'"'"' and '"'"'token'"'"' — a colour is one or the other; dropped"))); continue; }' \
  '                (Some(_), Some(_)) => { let _ = &diags; Definition::Token(Token::Danger) }' \
  geode-core \
  reads_hue_tone_and_token_and_refuses_both_or_neither

# 2c §2.2: an anchor hue is the theme's colour itself, never re-derived.
run_mutation "colour: an anchor hue returns the anchor untouched" \
  crates/geode-core/src/colour/mod.rs \
  '    if t <= 0.0 {' \
  '    if false && t <= 0.0 {' \
  geode-core \
  an_anchor_hue_is_the_themes_own_colour_exactly

# 2c §2.2: hue interpolates along the SHORTER arc.
run_mutation "colour: hue takes the shorter arc" \
  crates/geode-core/src/colour/mod.rs \
  '    let dh = (b.h - a.h + PI).rem_euclid(TAU) - PI;   // the shorter arc' \
  '    let dh = b.h - a.h;' \
  geode-core \
  a_hue_between_anchors_interpolates_along_the_shorter_arc

# 2c §2.2: gamut overflow pulls chroma, never clamps channels.
run_mutation "colour: gamut clip pulls chroma" \
  crates/geode-core/src/colour/oklab.rs \
  '    if in_gamut(direct) {' \
  '    if true {' \
  geode-core \
  gamut_clip_pulls_chroma_and_keeps_lightness_and_hue

# 2c §3: an unknown named colour warns with the column's file index.
run_mutation "load_views: an unknown colour name warns with its path" \
  crates/geode-core/src/config/load.rs \
  '                && colours.get(name).is_none()' \
  '                && false' \
  geode-core \
  a_column_naming_an_unknown_colour_warns_with_its_path
```

(The first entry's quoting is fragile: if the shell will not take it, restructure the arm to call a one-line helper `refuse_both(&mut diags, &at, name)` and anchor on that call instead.) Anchors unique; `--anchors-only` exit 0; five CI checks.

- [ ] **Step 9: Commit**

```bash
git add crates/geode-core crates/geode-blotter scripts/mutation-check.sh
git commit -m "feat(core): named colours — colours.toml, OKLCH resolver over a theme's base hues, Colour::Named (2c §2–§3)"
```

---

### Task 2: The overlay reshape — `[view.columns.<col>]`, both spellings read, applied

**Files:**
- Modify: `crates/geode-core/src/view.rs` (`ColumnPresentation::parse_format_keys` / `parse_column_keys` factored out of the views reader ~466-542; `ViewPresentation`; `ViewPresentationSpec::from_doc` ~618-720; `apply` ~734-800)
- Modify: `crates/geode-core/src/config/load.rs` (the overlay-side colour check)
- Modify: `crates/geode-shell/src/shell/objectdialog/views.rs` — **only** what the compiler forces: `doc_baseline`/`presentation_table` still emit the legacy `hidden`/`width` keys in this task (the reader accepts both); the new writer is Task 3's
- Test: `view.rs` tests, `load.rs` tests

**Interfaces:**
- Produces:
  ```rust
  impl ColumnPresentation {
      /// precision, thousands, negative, colour, scale — from a `format` table (views.toml) or a column table (the overlay).
      pub fn parse_format_keys(&mut self, table: &toml::Table, warn: &dyn Fn(&str, String));
      /// label, width (and hidden when `read_hidden`).
      pub fn parse_column_keys(&mut self, table: &toml::Table, read_hidden: bool, warn: &dyn Fn(&str, String));
      pub fn merge_over(&mut self, other: &ColumnPresentation);   // every Some in `other` wins
  }
  pub struct ViewPresentation { pub order: Vec<String>, pub columns: BTreeMap<String, ColumnPresentation> }   // hidden/width fields REMOVED
  ```
- Paths: `view_presentation.<view>.columns.<col>.<key>`; the legacy keys keep `view_presentation.<view>.hidden` / `.width.<col>`; a column set in both a legacy key and its table warns at `view_presentation.<view>.columns.<col>.<key>` and the table wins.

- [ ] **Step 1: Failing reader tests** (`view.rs` tests)

```rust
    fn overlay(text: &str) -> (ViewPresentationSpec, Vec<Diagnostic>) {
        let doc = merge_docs("view_presentation", &[LayerDoc::builtin("view_presentation", text).unwrap()]);
        ViewPresentationSpec::from_doc(&doc)
    }

    #[test]
    fn the_overlay_reads_a_column_table_with_every_presentation_key() {
        let (spec, diags) = overlay(
            "[tree]\norder = [\"npv\"]\n[tree.columns.npv]\nscale = \"k\"\nprecision = 0\nthousands = false\nnegative = \"parens\"\ncolour = \"delta\"\nlabel = \"NPV\"\nwidth = 120\nhidden = true\n",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let p = &spec.views["tree"].columns["npv"];
        assert_eq!(p.scale, Some(Scale::Thousands));
        assert_eq!(p.precision, Some(0));
        assert_eq!(p.thousands, Some(false));
        assert_eq!(p.negative, Some(Negative::Parens));
        assert_eq!(p.colour, Some(Colour::Named("delta".to_string())));
        assert_eq!(p.label.as_deref(), Some("NPV"));
        assert_eq!(p.width, Some(120.0));
        assert_eq!(p.hidden, Some(true));
        assert_eq!(spec.views["tree"].order, vec!["npv".to_string()]);
    }

    #[test]
    fn the_legacy_hidden_and_width_keys_still_load_and_the_table_wins_a_conflict() {
        let (spec, diags) = overlay(
            "[tree]\nhidden = [\"book\"]\n[tree.width]\nnpv = 140\nbook = 90\n[tree.columns.npv]\nwidth = 120\n",
        );
        let cols = &spec.views["tree"].columns;
        assert_eq!(cols["book"].hidden, Some(true));
        assert_eq!(cols["book"].width, Some(90.0));
        assert_eq!(cols["npv"].width, Some(120.0), "the table wins");
        assert!(diags.iter().any(|d| d.path.as_deref() == Some("view_presentation.tree.columns.npv.width") && d.message.contains("table wins")), "{diags:?}");
    }

    #[test]
    fn apply_merges_a_column_table_over_the_view() {
        let views_doc = merge_docs("views", &[LayerDoc::builtin("views",
            "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\nformat = { scale = \"k\", precision = 2 }\n").unwrap()]);
        let (mut views, _) = ViewSpec::from_doc(&views_doc);
        let (spec, _) = overlay("[tree.columns.npv]\nprecision = 0\ncolour = \"delta\"\n");
        let diags = spec.apply(&mut views);
        assert!(diags.is_empty(), "{diags:?}");
        let p = views[0].presentation_of("npv");
        assert_eq!(p.scale, Some(Scale::Thousands), "the desk's key survives");
        assert_eq!(p.precision, Some(0), "the trader's key wins");
        assert_eq!(p.colour, Some(Colour::Named("delta".to_string())));
    }
```

Run — compile errors (`columns` field missing). Existing tests that build `ViewPresentation { hidden, width, .. }` or read `.hidden`/`.width` need updating to `columns` — do so as part of Step 2 and keep their assertions' meaning.

- [ ] **Step 2: Implement**

1. Factor the per-column parse: move the body of the views reader's `format` block (precision, thousands, negative, colour, scale) into `ColumnPresentation::parse_format_keys(&mut self, table, warn)` where `warn: &dyn Fn(&str, String)` receives the key (`"precision"` …) and the message; the views reader calls it with a closure that prefixes `format.` (`|key, m| warn(&format!("format.{key}"), m)`). Move `label`/`width` into `parse_column_keys(&mut self, table, read_hidden, warn)`; the views reader passes `read_hidden: false` (rewrite `ColumnPresentation.hidden`'s doc: it is set by the overlay, never the view). Existing reader tests stay green.
2. `ViewPresentation { order, columns }` with docs; `from_doc`: read `order` as today; read `columns` as a table of tables — for each `(col, table)`: a `ColumnPresentation` via `parse_format_keys` + `parse_column_keys(read_hidden: true)` with `warn = |key, m| bad(&format!("columns.{col}.{key}"), format!("column '{col}': {m}"))`; then the legacy keys: `hidden` array → for each name, `columns.entry(name).or_default().hidden = Some(true)` unless the table already set `hidden` (then warn `columns.<col>.hidden` "also in the legacy 'hidden' array — the table wins"); `width` map → same with `width`. Read the table FIRST so the conflict rule can see it.
3. `merge_over`, one line per field so each is its own anchor:

```rust
    pub fn merge_over(&mut self, other: &ColumnPresentation) {
        if other.precision.is_some() { self.precision = other.precision; }
        if other.thousands.is_some() { self.thousands = other.thousands; }
        if other.negative.is_some() { self.negative = other.negative; }
        if other.colour.is_some() { self.colour = other.colour.clone(); }
        if other.scale.is_some() { self.scale = other.scale; }
        if other.label.is_some() { self.label = other.label.clone(); }
        if other.width.is_some() { self.width = other.width; }
        if other.hidden.is_some() { self.hidden = other.hidden; }
    }
```

   and the legacy `width` fold, verbatim so the harness can anchor on its guard:

```rust
                        for (col, w) in t {
                            let entry = p.columns.entry(col.clone()).or_default();
                            if entry.width.is_some() {
                                diags.push(bad(&format!("columns.{col}.width"), format!("column '{col}': 'width' is also set in the legacy 'width' map — the table wins")));
                                continue;
                            }
                            // …the existing positive-number check, writing `entry.width = Some(x as f32)`
```

   (the `hidden` array folds the same way, with `entry.hidden.is_some()` and `columns.{col}.hidden`).
4. `apply`: after the permutation, for each `(col, p)` in `columns`: warn `columns.<col>` "names column … which the view does not have — ignored" when absent, else `view.presentation.entry(col).or_default().merge_over(p)`.
5. `load.rs`: add the overlay-side colour check: for each view in `presentation.views`, each `(col, p)` with `Some(Colour::Named(name))` the doc lacks → warning at `view_presentation.<view>.columns.<col>.colour`; test it (`a_presentation_naming_an_unknown_colour_warns_with_its_path`).
6. `views.rs`: the compiler will report `p.hidden`/`p.width` reads in `views::fields` or elsewhere — none expected (it reads `presentation_of`); fix what it reports without changing the writer.

Run `cargo test --workspace` — green.

- [ ] **Step 3: Harness entries**

```bash
# 2c §4.2: a column set in both spellings takes the TABLE's value.
run_mutation "overlay: the column table wins over a legacy key" \
  crates/geode-core/src/view.rs \
  '                            if entry.width.is_some() {' \
  '                            if entry.width.is_none() {' \
  geode-core \
  the_legacy_hidden_and_width_keys_still_load_and_the_table_wins_a_conflict

# 2c §4.4: apply merges only the keys the overlay SET.
run_mutation "overlay: apply merges Some keys only" \
  crates/geode-core/src/view.rs \
  '        if other.precision.is_some() { self.precision = other.precision; }' \
  '        self.precision = other.precision;' \
  geode-core \
  apply_merges_a_column_table_over_the_view
```

`grep -c -F` = 1 each (the `if entry.width.is_some() {` guard must be the only such line — the `hidden` fold's guard reads `entry.hidden`). Five CI checks; `--anchors-only`.

- [ ] **Step 4: Commit** — `git commit -m "feat(core): view_presentation carries one [view.columns.<col>] table per column; legacy keys still load (2c §4)"`

---

### Task 3: `Number.step`/`wrap`, `ListItem.presentation`, the differing-keys writer, the member-row summary

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`FieldKind::Number`, `step_selected`'s Number arm ~1515, every `Number {` literal — 22 sites incl. tests; `ListItem`, every `width:` literal — 10 sites)
- Modify: `crates/geode-shell/src/shell/objectdialog/views.rs` (`fields`: `presentation: v.presentation_of(name)`; `presentation_table` + `doc_baseline` rewritten; `column_summary`)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (item row paints `column_summary` instead of the width read at ~2623)
- Modify: `crates/geode-shell/src/shell/objectdialog/{sources,groupings,schema,scopes}.rs` and `tests/objectdialog.rs` — `step: 1, wrap: false` and `presentation: ColumnPresentation::default()` where the compiler asks
- Test: `mod.rs` tests, `views.rs` tests

**Interfaces:**
- Produces:
  ```rust
  Number { value: i64, min: i64, max: i64, step: i64, wrap: bool }
  pub struct ListItem { pub name: String, pub included: bool, pub presentation: ColumnPresentation, pub kind: Option<String> }
  // views.rs
  pub fn column_summary(kind_default: &ColumnFormat, p: &ColumnPresentation) -> String;   // "120 px · k · 0 dp · delta" — only keys that differ from the kind default; "" when none
  pub fn kind_default(item: &ListItem) -> ColumnFormat;                                    // MEASURE unless item.kind == Some("dimension")
  fn desk_baseline(draft: &Draft) -> BTreeMap<String, ColumnPresentation>;   // per column: the desk's format/label/width from `draft.source`
  fn presentation_table(draft: &Draft) -> toml_edit::Table;                   // order + [columns.<col>] with differing keys only; hidden whenever set
  ```

- [ ] **Step 1: Failing tests**

`mod.rs` (Number):

```rust
    #[test]
    fn a_number_steps_by_its_step_and_wraps_only_when_asked() {
        let mut draft = single_field_draft(FieldKind::Number { value: 350, min: 0, max: 359, step: 15, wrap: true });
        assert_eq!(draft.toggle_selected(), Step::Changed);
        assert!(matches!(draft.fields[0].kind, FieldKind::Number { value: 5, .. }), "wraps: {:?}", draft.fields[0].kind);
        assert_eq!(draft.toggle_selected_back(), Step::Changed);
        assert!(matches!(draft.fields[0].kind, FieldKind::Number { value: 350, .. }));
        let mut draft = single_field_draft(FieldKind::Number { value: 355, min: 0, max: 359, step: 15, wrap: false });
        assert_eq!(draft.toggle_selected(), Step::Changed);
        assert!(matches!(draft.fields[0].kind, FieldKind::Number { value: 359, .. }), "lands on the bound without wrap");
        assert_eq!(draft.toggle_selected(), Step::Inert, "at the bound, no wrap: inert");
    }
```

(`single_field_draft` exists in the tests module at ~3189 — reuse it.)

`views.rs` (writer):

```rust
    #[test]
    fn the_writer_emits_only_keys_that_differ_from_the_desk() {
        // desk: npv has scale k, precision 2; the trader sets precision 0 and a colour, and hides book.
        let config = config_with_view("[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\nformat = { scale = \"k\", precision = 2 }\n[[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n");
        let mut draft = Domain::Views.draft(&config, "tree");
        {
            let items = items_mut(&mut draft);
            items[0].presentation.precision = Some(0);
            items[0].presentation.colour = Some(Colour::Named("delta".to_string()));
            items[1].included = false;
        }
        let text = super::super::object_text("tree", to_table(&draft, Destination::Presentation));
        assert!(text.contains("[tree.columns.npv]"), "{text}");
        assert!(text.contains("precision = 0") && text.contains("colour = \"delta\""), "{text}");
        assert!(!text.contains("scale"), "the desk's own scale is not copied: {text}");
        assert!(text.contains("[tree.columns.book]") && text.contains("hidden = true"), "{text}");
        assert!(!text.contains("\nwidth = {") && !text.contains("hidden = ["), "no legacy spelling: {text}");
        // Setting precision back to the desk's value drops the key.
        items_mut(&mut draft)[0].presentation.precision = Some(2);
        let text = super::super::object_text("tree", to_table(&draft, Destination::Presentation));
        assert!(!text.contains("precision"), "{text}");
    }
```

(`config_with_view`/`items_mut` — write small helpers in the test module: `config_with_view(views_text)` builds a `Config` from a builtin `views` doc plus a `datasets` doc declaring `risk` with `npv` (f64 measure, grain instrument, aggregate sum) and `book` (utf8 dimension); `items_mut` returns `&mut Vec<ListItem>` of the `columns` field.)

Run — compile errors.

- [ ] **Step 2: Implement**

1. `Number { .., step: i64, wrap: bool }` with a doc ("`step` is the distance one `space` moves; `wrap` makes the range circular — a hue — so a step past `max` lands at `min + overshoot`. Every existing `Number` is `step: 1, wrap: false`."). `step_selected`'s arm keeps the out-of-range refusal, then:

```rust
                        let span = *max - *min + 1;
                        let next = match direction { StepDirection::Forward => *value + *step, StepDirection::Backward => *value - *step };
                        let landed = if *wrap {
                            (next - *min).rem_euclid(span) + *min
                        } else if next > *max { *max } else if next < *min { *min } else { next };
                        if landed == *value { return Step::Inert; }
                        *value = landed;
                        Step::Changed
```

   Add `step: 1, wrap: false` to every literal the compiler reports; `apply_text_entry`'s `Number` arm destructures `{ value, min, max, .. }`; `sources.rs`'s `stable_polls` stays `step: 1`.
2. `ListItem.presentation: ColumnPresentation` (doc: "the column's presentation as the trader sees it — the kind default with the desk's and the overlay's keys applied, i.e. `ViewSpec::presentation_of` after `load_views`"). Every `width: None` literal → `presentation: ColumnPresentation::default()`; `width: presentation.width` in `views::fields` → `presentation`; the two `.width` reads in `mod.rs` tests → `.presentation.width`; `views::refresh_available` likewise. Rewrite `ListItem`'s doc.
3. `views.rs`: replace `doc_baseline`'s width map with `desk_baseline(draft) -> BTreeMap<String, ColumnPresentation>` built from `columns_for(&draft.source, ..)`'s tables via `ColumnPresentation::parse_format_keys`(the `format` sub-table) + `parse_column_keys(read_hidden: false)` with a no-op warn; keep `doc_order`. `presentation_table`: `order` as today; then for each item: `let desk = baseline.get(name).cloned().unwrap_or_default(); let mut t = toml_edit::Table::new();` and for each key, emit when it differs — write the eight comparisons out, one per line, in this exact shape so the harness can anchor on one:

```rust
        if item.presentation.precision != desk.precision {
            if let Some(v) = item.presentation.precision { t["precision"] = toml_edit::value(i64::from(v)); }
        }
```

   (compare the `Option`s directly — an item key set to the desk's value is equal and omitted; an item key `None` where the desk has `Some` cannot happen, since the item was seeded from the merged presentation); `hidden = true` when `!item.included`; spell `scale` as `none`/`k`/`M`, `negative` as `minus`/`parens`, `colour` as `none`/`sign`/name, `width` as an integer when whole. Insert `table["columns"][name] = t` only when `t` is non-empty. Rewrite the function's doc: §4.3's rule and why.
4. `column_summary(kind_default: &ColumnFormat, p: &ColumnPresentation) -> String`: the effective format `kind_default.with(p)`; parts: `"{w} px"` when `p.width` is Some, `k`/`M` when scale ≠ None, `"{n} dp"` when precision ≠ default, `parens` when negative ≠ Minus, `no thousands` when thousands is false and the default true, the colour name or `sign` when colour ≠ default, `"→ {label}"` when label is Some; joined by ` · `. The item row in `render.rs` paints `entry.name` then, muted, the summary (the width read at ~2623 goes; `row_label` stays the name alone, so the filter is unchanged).

Run `cargo test --workspace` — green (fix the `mod.rs` tests that asserted the legacy `[tree.width]` writer output to the new spelling, keeping their intent).

- [ ] **Step 3: Harness entries**

```bash
# 2c §5.4: a wrapping Number goes round; a plain one lands on the bound.
run_mutation "objectdialog: a wrapping number wraps" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '                        let landed = if *wrap {' \
  '                        let landed = if false {' \
  geode-shell \
  a_number_steps_by_its_step_and_wraps_only_when_asked

# 2c §4.3: the writer omits a key equal to the desk baseline.
run_mutation "views: the overlay writer omits keys equal to the desk" \
  crates/geode-shell/src/shell/objectdialog/views.rs \
  '        if item.presentation.precision != desk.precision {' \
  '        if item.presentation.precision != desk.precision || true {' \
  geode-shell \
  the_writer_emits_only_keys_that_differ_from_the_desk
```

Five CI checks; `--anchors-only` (the `Number` arm and `presentation_table` are anchored by 2b entries — re-anchor).

- [ ] **Step 4: Commit** — `git commit -m "feat(objectdialog): Number step/wrap; ListItem carries the column presentation; the overlay writer emits [view.columns.<col>] differing keys only (2c §4.3, §5.1, §5.4)"`

---

### Task 4: The column stage

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`Stage::Column`; `Draft { parent_fields, column }`; `enter_column`/`fold_column`/`leave_column`/`column()`; `field_by_key` fallback in `list_items`/`available_items`/`choice`; `has_previous_stage`; `row_for_path` in the stage)
- Modify: `crates/geode-shell/src/shell/objectdialog/views.rs` (`column_fields(item, colours) -> Vec<Field>`; `text_editable`/`parse_text` for `label`/`width`; `fold_into(item, fields)`)
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs` (the edit stage's `NormalCommand::Commit` arm; `enter_column_stage`/`leave_column_stage`; the escape ladder's `PreviousStage` at both `leave_edit` sites; `crumb_text`; `revalidate` folds first; every `Stage::Edit { object }` match the compiler flags)
- Test: `mod.rs`, `views.rs` unit tests; `tests/objectdialog.rs` window tests

**Interfaces:**
- Produces:
  ```rust
  pub enum Stage { Browse, Naming, Edit { object: String }, Column { object: String, column: String } }
  // Draft
  parent_fields: Option<Vec<Field>>, column: Option<String>   (private; `pub fn column(&self) -> Option<&str>`)
  pub fn enter_column(&mut self, column: &str, fields: Vec<Field>) -> bool;   // false when `column` is not a member
  pub fn fold_column(&mut self);                                              // installed fields → the item's presentation (views::fold_into)
  pub fn leave_column(&mut self);                                             // fold, restore parent fields (cursor on the column's row), baseline = fields
  fn field_by_key(&self, key) -> Option<&Field>   // fields, then parent_fields — what list_items/available_items/choice use
  // views.rs
  pub fn column_fields(item: &ListItem, colours: &[String]) -> Vec<Field>;
  pub fn fold_into(item: &mut ListItem, fields: &[Field]);
  pub const COLUMN_KEYS: [&str; 7] = ["label", "width", "scale", "precision", "thousands", "negative", "colour"];
  // render.rs
  fn enter_column_stage(shell, column: &str, cx); fn leave_column_stage(shell, cx);
  ```
- `Domain::text_editable(Views, "label" | "width") == true`; `parse_text(Views, "width", text)`: `auto` or `20..=2000` → `Ok`, else `Err("width must be auto or 20–2000 px")`; `"label"` → `Ok(trimmed)`.

- [ ] **Step 1: Failing unit tests** (`views.rs` tests)

```rust
    #[test]
    fn column_fields_are_seven_presentation_rows_seeded_from_the_item() {
        let item = ListItem { name: "npv".into(), included: true, kind: Some("measure".into()),
            presentation: ColumnPresentation { scale: Some(Scale::Thousands), precision: Some(0), width: Some(120.0), colour: Some(Colour::Named("delta".into())), ..Default::default() } };
        let fields = column_fields(&item, &["delta".to_string(), "gamma".to_string()]);
        let keys: Vec<&str> = fields.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, COLUMN_KEYS);
        assert!(fields.iter().all(|f| f.dest == Destination::Presentation));
        let by = |k: &str| fields.iter().find(|f| f.key == k).unwrap();
        assert!(matches!(&by("label").kind, FieldKind::Text(t) if t.is_empty()));
        assert!(matches!(&by("width").kind, FieldKind::Text(t) if t == "120"));
        assert!(matches!(&by("scale").kind, FieldKind::Choice { options, selected } if options[*selected] == "k"));
        assert!(matches!(&by("precision").kind, FieldKind::Number { value: 0, min: 0, max: 12, step: 1, wrap: false }));
        assert!(matches!(&by("thousands").kind, FieldKind::Bool(true)), "the measure default");
        assert!(matches!(&by("negative").kind, FieldKind::Choice { options, selected } if options[*selected] == "minus"));
        assert!(matches!(&by("colour").kind, FieldKind::Choice { options, selected } if options == &["none", "sign", "delta", "gamma"] && options[*selected] == "delta"));
    }

    #[test]
    fn fold_into_writes_the_fields_back_and_auto_clears_the_width() {
        let mut item = ListItem { name: "npv".into(), included: true, kind: Some("measure".into()), presentation: ColumnPresentation::default() };
        let mut fields = column_fields(&item, &[]);
        for f in &mut fields {
            match f.key.as_str() {
                "width" => f.kind = FieldKind::Text("auto".into()),
                "precision" => f.kind = FieldKind::Number { value: 4, min: 0, max: 12, step: 1, wrap: false },
                "colour" => f.kind = FieldKind::Choice { options: vec!["none".into(), "sign".into()], selected: 1 },
                "label" => f.kind = FieldKind::Text("NPV".into()),
                _ => {}
            }
        }
        fold_into(&mut item, &fields);
        assert_eq!(item.presentation.width, None);
        assert_eq!(item.presentation.precision, Some(4));
        assert_eq!(item.presentation.colour, Some(Colour::Sign));
        assert_eq!(item.presentation.label.as_deref(), Some("NPV"));
    }

    #[test]
    fn width_text_is_auto_or_a_pixel_count_in_range() {
        assert_eq!(parse_text("width", " 120 ").unwrap(), "120");
        assert_eq!(parse_text("width", "auto").unwrap(), "auto");
        assert!(parse_text("width", "5").is_err() && parse_text("width", "wide").is_err());
        assert!(text_editable("label") && text_editable("width") && !text_editable("dataset"));
    }
```

`mod.rs` tests:

```rust
    #[test]
    fn entering_a_column_swaps_the_fields_and_leaving_restores_them_with_the_fold() {
        let config = config_with_view_and_datasets();   // a `tree` view with columns npv (measure) and book (dimension); write this helper beside config_from
        let mut draft = Domain::Views.draft(&config, "tree");
        let parent_len = draft.fields.len();
        assert!(draft.enter_column("npv", views::column_fields(draft.list_items("columns").unwrap().iter().find(|i| i.name == "npv").unwrap(), &[])));
        assert_eq!(draft.column(), Some("npv"));
        assert_eq!(draft.fields.len(), 7);
        assert!(draft.list_items("columns").is_some(), "the view's list is still reachable through the parent");
        assert_eq!(draft.choice("dataset"), Some("risk"), "so is the dataset");
        let i = draft.fields.iter().position(|f| f.key == "scale").unwrap();
        draft.selected = i;
        assert_eq!(draft.toggle_selected(), Step::Changed);
        draft.fold_column();
        assert_eq!(draft.list_items("columns").unwrap()[0].presentation.scale, Some(Scale::Thousands));
        assert!(draft.writes_by_destination().contains_key(&Destination::Presentation));
        assert!(!draft.writes_by_destination().contains_key(&Destination::Doc));
        draft.mark_saved();
        draft.leave_column();
        assert_eq!(draft.column(), None);
        assert_eq!(draft.fields.len(), parent_len);
        assert!(!draft.is_dirty(), "leaving after a committed change is clean");
        assert!(matches!(draft.selected_row(), Some(EditRow::Item { .. })), "cursor back on the column");
        assert!(!draft.enter_column("ghost", Vec::new()), "not a member");
    }

    #[test]
    fn row_for_path_in_the_column_stage_lands_on_the_format_key() {
        let config = config_with_view_and_datasets();
        let mut draft = Domain::Views.draft(&config, "tree");
        let item = draft.list_items("columns").unwrap()[0].clone();
        draft.enter_column("npv", views::column_fields(&item, &[]));
        let precision = draft.fields.iter().position(|f| f.key == "precision").unwrap();
        assert_eq!(draft.row_for_path("views", "views.tree.columns.0.format.precision"), Some(EditRow::Field(precision)));
        assert_eq!(draft.row_for_path("views", "views.tree.columns.1.format.precision"), None, "another column's path lands nowhere here");
        assert_eq!(draft.row_for_path("views", "views.tree.dataset"), None);
    }
```

Run — compile errors.

- [ ] **Step 2: Implement the pure core**

`mod.rs`:
- `Stage::Column { object: String, column: String }` (doc: "editing one column's presentation — a projection over the same draft, §5.2; the stage is what `escape` steps back from and the crumb reads").
- `Draft` gains `parent_fields: Option<Vec<Field>>` and `column: Option<String>` (`None` in both constructors); `pub fn column(&self) -> Option<&str>`.
- `field_by_key(&self, key) -> Option<&Field>`: `self.fields.iter().find(..).or_else(|| self.parent_fields.as_ref()?.iter().find(..))`; `list_items`, `available_items`, `choice` route through it (rewrite `list_items`' doc: why the parent fallback exists — the column stage swaps `fields`, and the overlay writer still renders from the view's list).
- `enter_column(&mut self, column, fields) -> bool`: the membership check is one line, `let is_member = self.list_items("columns").is_some_and(|items| items.iter().any(|i| i.name == column));` then `if !is_member { return false; }`; `self.parent_fields = Some(std::mem::replace(&mut self.fields, fields))`; `self.column = Some(column.into())`; `self.baseline = self.fields.clone()`; `query.clear()`, `selected = 0`, `text_entry = None`, `confirm = None`; `true`.
- `fold_column(&mut self)`: `let Some(name) = self.column.clone() else { return }`; find the item in `parent_fields`' `columns` list mutably and call `views::fold_into(item, &self.fields)`. (`fold_into` lives in `views.rs`; `mod.rs` may call it — the adapter modules are children.)
- `leave_column(&mut self)`: `fold_column()`; `let parent = self.parent_fields.take()`; find the column's item index in the restored list; `self.fields = parent`; `self.baseline = self.fields.clone()`; `self.column = None`; `query.clear()`; `selected` = the visible position of `EditRow::Item { field, item }` for that column (compute after restoring, via `visible_rows`).
- `row_for_path` in the stage: when `self.column.is_some()`, after stripping `"{doc}.{name}."`, the rest must start with `columns.`; resolve its index by name through `resolve_list_index` against the PARENT's items (`field_by_key("columns")`); written as `if resolved_name != column { return None; }`; else the remainder, with a leading `format.` stripped, matches a field key (`label`/`width` have no `format.` prefix). Add a doc paragraph.
- `has_previous_stage`: `matches!(self.stage, Stage::Edit { .. } | Stage::Naming | Stage::Column { .. })`.

`views.rs`:
- `COLUMN_KEYS`; `column_fields(item, colours)`: `label` Text (item.presentation.label or ""), `width` Text (`format!("{}", w as i64)` or "auto"), `scale` Choice, `precision` Number (value = effective precision from `kind_default(item).with(&item.presentation)`, 0..=12, step 1), `thousands` Bool (effective), `negative` Choice (effective), `colour` Choice (`none`, `sign`, then `colours` sorted; selected = the effective colour, adding the item's own name to the options if the doc lacks it — Views' "keep the object's own value" rule). `kind_default(item)`: `ColumnFormat::MEASURE` unless `item.kind == Some("dimension")`.
- `fold_into(item, fields)`: label → `Some` unless empty; width → `None` for `auto`, else parsed; each format key → `Some(value)` (the item's presentation is the merged one, so a `Some` equal to the desk's is fine — the writer drops equal keys).
- `text_editable(key)`: `label | width`; `parse_text(key, text)`: width rule; label trims.
- `Domain::text_editable`/`parse_text` `Views` arms delegate to these (Task 1 of 2b left them `false` / trim).

Run the unit tests — PASS.

- [ ] **Step 3: The gpui side** (`render.rs`)

- The edit stage's `NormalCommand::Commit` arm: if `state.domain == Domain::Views` and `draft.column().is_none()` and `draft.selected_row()` is `Some(EditRow::Item { .. })` → `enter_column_stage(shell, name, cx)`; otherwise the existing `edit_commit_notice`.
- `enter_column_stage`: read the colour names from `shell.services.config.doc("colours")` via `NamedColours::from_doc(..).0.names()`; build `views::column_fields(item, &names)` (the item cloned from the draft's list); `draft.enter_column(name, fields)`; `state.stage = Stage::Column { object, column }`; `state.mode = Normal`; `scroll_to_item(0)`; `cx.notify()`.
- `leave_column_stage`: `draft.leave_column()`; `state.stage = Stage::Edit { object }`; `scroll_to_cursor`; notify. Wire it into the escape ladder: at both `PreviousStage` sites, `if matches!(state.stage, Stage::Column { .. }) { leave_column_stage } else { leave_edit }`.
- `revalidate`: before `domain.validate(..)`, exactly

```rust
    // 2c §5.2: the column stage is a projection; fold it into the item
    // FIRST so the validator and the overlay writer see this keystroke.
    if draft.column().is_some() {
        draft.fold_column();
    }
```

   — the one fold site every changed value passes through.
- `crumb_text`: `Stage::Column { object, column } => format!("{object} › {column}")`.
- Every `match` on `Stage` the compiler flags (`editing_row`, `leave_edit`'s name read, `run_confirmed`, `jump_to_slot`, `enter_edit_stage`'s "already editing" guard, `create_from_name`): a `Column { object, .. }` arm behaves as `Edit { object }` where the object name is what is wanted.
- `d`/`r`/`o` in the column stage: the action bar reads `editing_row` (the object) — leave them live; they act on the object as before. `n` is browse-only. Drag/tick: no item rows in the stage, so none.
- Footer hint in the stage: `j k move · space shift+space change · i type · / filter · escape back to <object>`; `hint_i` shows since `offers_text_entry` is true (`width`/`label`).

- [ ] **Step 4: Window tests** (`tests/objectdialog.rs`)

```rust
/// §5: enter on a member opens the column stage; a step there writes
/// one [view.columns.<col>] key to the overlay and never forks the view;
/// escape returns to the view's stage with the cursor on the column.
#[gpui::test]
fn the_column_stage_writes_a_differing_key_to_the_overlay(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services_with_views(), dir.path(), "config::views");
    cx.simulate_keystrokes("enter");          // tree
    cx.run_until_parked();
    cx.simulate_keystrokes("j enter");        // first member row (npv)
    cx.run_until_parked();
    assert!(matches!(dialog_state(&shell, &cx, |s| s.stage.clone()), objectdialog::Stage::Column { .. }));
    assert_eq!(shell.read_with(&cx, |s, _| objectdialog::render::crumb_text(s)), "tree › npv");
    assert!(cx.debug_bounds("objectdialog-field-scale").is_some());
    cx.simulate_keystrokes("j j space");      // label, width, scale → k
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-confirm").is_none(), "presentation never forks");
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("view_presentation.toml")).unwrap();
    assert!(written.contains("[tree.columns.npv]") && written.contains("scale = \"k\""), "{written}");
    assert!(!dir.path().join("views.toml").exists(), "the desk's view is untouched");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(matches!(dialog_state(&shell, &cx, |s| s.stage.clone()), objectdialog::Stage::Edit { .. }));
    assert!(edit_draft(&shell, &cx, |d| matches!(d.selected_row(), Some(objectdialog::EditRow::Item { .. }))));
}
```

(`crumb_text` is `pub(crate)` and takes `&ShellView`.) A second window test: `i` on `width`, type `160`, `enter` → the overlay holds `width = 160`; type `wide` → refused with the range named.

- [ ] **Step 5: Harness entries**

```bash
# 2c §5.2: every changed value folds into the item BEFORE validation and commit.
run_mutation "objectdialog: revalidate folds the column stage first" \
  crates/geode-shell/src/shell/objectdialog/render.rs \
  '    if draft.column().is_some() {' \
  '    if draft.column().is_none() {' \
  geode-shell \
  the_column_stage_writes_a_differing_key_to_the_overlay

# 2c §5.5: a path naming ANOTHER column lands nowhere in this stage.
run_mutation "objectdialog: row_for_path in the column stage is scoped to the open column" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        if resolved_name != column {' \
  '        if false && resolved_name != column {' \
  geode-shell \
  row_for_path_in_the_column_stage_lands_on_the_format_key

# 2c §5.2: enter on a non-member is refused.
run_mutation "objectdialog: enter_column refuses a non-member" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        if !is_member {' \
  '        if false && !is_member {' \
  geode-shell \
  entering_a_column_swaps_the_fields_and_leaving_restores_them_with_the_fold
```

Five CI checks; `--anchors-only`.

- [ ] **Step 6: Commit** — `git commit -m "feat(objectdialog): the column stage — seven presentation fields under a member row, folded into the overlay (2c §5)"`

---

### Task 5: The Colours dialog

**Files:**
- Create: `crates/geode-shell/src/shell/objectdialog/colours.rs`, `crates/geode-shell/src/shell/colours.rs`
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs` (`Domain::Colours` + every arm; `Domain::reserved_names`; `name_taken`), `render.rs` (swatch on browse rows and in the edit header), `dialog.rs` (`swatch`), `shell/mod.rs` (`pub mod colours;`), `defaults.rs`, `input.rs`
- Test: `colours.rs` unit tests; `tests/objectdialog.rs` window test

**Interfaces:**
- Produces:
  ```rust
  // shell/colours.rs
  pub fn anchors_from_theme(theme: &gpui_component::Theme) -> geode_core::colour::Anchors;
  pub fn tokens_from_theme(theme: &gpui_component::Theme) -> geode_core::colour::Tokens;
  pub fn to_rgb(hsla: gpui::Hsla) -> Rgb;  pub fn to_hsla(rgb: Rgb) -> gpui::Hsla;
  pub fn resolve_named(colours: &NamedColours, name: &str, theme: &Theme) -> Option<gpui::Hsla>;
  // objectdialog/colours.rs
  pub const DOC: &str = "colours";
  pub fn summary(value: &toml::Value) -> String;             // Definition::summary or "invalid"
  pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field>;   // hue Number{0..=359, step 15, wrap}, tone Choice, token Choice(none + Token::ALL names)
  pub fn to_table(draft: &Draft, dest: Destination) -> toml_edit::Item;
  pub fn validate(draft: &Draft, config: &Config) -> Vec<Diagnostic>;
  pub fn definition_of(draft: &Draft) -> Option<Definition>;  // the fields as a Definition, for the header swatch
  // dialog.rs
  pub(crate) fn swatch(colour: Hsla, selector: String, cx: &App) -> AnyElement;   // px(14) square, rounded, border theme.border
  // mod.rs
  impl Domain { pub fn reserved_names(self) -> &'static [&'static str] }   // Colours → RESERVED_NAMES, else &[]
  ```
- Palette: `config::colours`, "Edit colours", "Configuration".

- [ ] **Step 1: Failing unit tests** (`objectdialog/colours.rs`)

```rust
    #[test]
    fn fields_seed_from_the_definition_and_to_table_writes_only_the_keys_in_force() {
        let config = config_with_colours("[delta]\nhue = 240\n[gamma]\nhue = 210\ntone = \"light\"\n[pnl]\ntoken = \"chart.bullish\"\n");
        let draft = Domain::Colours.draft(&config, "gamma");
        let by = |k: &str| draft.fields.iter().find(|f| f.key == k).unwrap();
        assert!(matches!(by("hue").kind, FieldKind::Number { value: 210, min: 0, max: 359, step: 15, wrap: true }));
        assert!(matches!(&by("tone").kind, FieldKind::Choice { options, selected } if options[*selected] == "light"));
        assert!(matches!(&by("token").kind, FieldKind::Choice { options, selected } if options[*selected] == "none" && options.len() == 16));
        let text = super::super::object_text("gamma", to_table(&draft, Destination::Doc));
        assert!(text.contains("hue = 210") && text.contains("tone = \"light\"") && !text.contains("token"), "{text}");
        let draft = Domain::Colours.draft(&config, "pnl");
        let text = super::super::object_text("pnl", to_table(&draft, Destination::Doc));
        assert!(text.contains("token = \"chart.bullish\"") && !text.contains("hue"), "no dead hue beside a token: {text}");
        assert_eq!(definition_of(&draft), Some(Definition::Token(Token::Bullish)));
    }

    #[test]
    fn reserved_names_are_taken() {
        let config = config_with_colours("[delta]\nhue = 240\n");
        assert!(Domain::Colours.name_taken(&config, "sign") && Domain::Colours.name_taken(&config, "none"));
        assert!(!Domain::Views.name_taken(&config, "sign"));
        let draft = Domain::Colours.new_draft(&config, "fresh");
        assert_eq!(definition_of(&draft), Some(Definition::Hue { degrees: 0.0, tone: Tone::Normal }));
    }
```

- [ ] **Step 2: Implement**

`colours.rs` (adapter): model on `sources.rs`. `fields`: read the object's table; `hue` value from `hue` (default 0), `tone`, `token` (`none` + `Token::ALL.map(name)`); `to_table`: start from `toml_table_to_edit(&draft.source)`, then exactly `table.remove("hue");` `table.remove("tone");` `table.remove("token");` on three lines, then write `token` if the choice ≠ `none`, else `hue` and `tone = "light"` when light. `validate`: `NamedColours::from_doc` over the one-object doc. `summary`: parse the table with `NamedColours::from_doc` over a one-entry doc and use `Definition::summary`, or `"invalid"`. `definition_of`: from the fields.

`mod.rs`: `Domain::Colours` arms (`doc`, `title` "Colours", `crumb_noun` "colours", `summary_fn`, `presentation_doc` None, `roster` None, `prefix_fn` None, `writable` true, `text_editable` false, `parse_text` trim, `fields`, `to_table`, `validate`, `Destination::doc`, `section_header_text`); `reserved_names()`; `name_taken` starts with `if self.reserved_names().contains(&name) { return true; }`; `create_from_name`'s taken-notice reads "'sign' is reserved" when reserved (a third branch beside listed/presentation).

`shell/colours.rs`: `to_rgb(hsla)`: `let c = hsla.to_rgb(); Rgb { r: c.r, g: c.g, b: c.b }`; `to_hsla(rgb)`: `gpui::Rgba { r, g, b, a: 1.0 }.into()`; `anchors_from_theme`: `normal: [red, yellow, green, cyan, blue, magenta]`, `light: [red_light, …]` via `to_rgb`; `tokens_from_theme` per the §2.3 table (`muted` ← `muted_foreground`); `resolve_named` = `colours.get(name).map(|d| to_hsla(resolve(d, &anchors, &tokens)))`.

`dialog.rs`: `swatch(colour, selector, cx)`: `div().w(px(14.)).h(px(14.)).rounded(px(3.)).border_1().border_color(cx.theme().border).bg(colour).debug_selector(..)`.

`render.rs`: browse row — when `state.domain == Domain::Colours`, add a swatch child before the label, resolved by reading the row's value: `NamedColours::from_doc(config.doc("colours"))` once per build, `resolve_named(&colours, &row.name, &cx.theme())`, selector `objectdialog-swatch-{name}` (a dropped/invalid colour paints no swatch). Edit header — when the domain is Colours, `colours::definition_of(draft)` → `resolve(&def, anchors, tokens)` → a swatch beside the name, selector `objectdialog-swatch-header`.

`defaults.rs` / `input.rs`: `config::colours` → `Domain::Colours`.

- [ ] **Step 3: Window test**

```rust
/// §6.1: the browse rows and the edit header carry a swatch resolved
/// against the active theme; stepping the hue repaints it; `n` refuses a
/// reserved name.
#[gpui::test]
fn the_colours_dialog_paints_swatches_and_refuses_reserved_names(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) = dialog_test_shell_in_dir(cx, services_with_colours(), dir.path(), "config::colours");
    assert!(cx.debug_bounds("objectdialog-swatch-delta").is_some());
    cx.simulate_keystrokes("n");
    cx.simulate_input("sign");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(dialog_state(&shell, &cx, |s| s.notice.clone()).unwrap().contains("reserved"));
    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes("enter");          // delta
    cx.run_until_parked();
    assert!(cx.debug_bounds("objectdialog-swatch-header").is_some());
    cx.simulate_keystrokes("space");          // hue 240 → 255
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| matches!(d.fields[0].kind, objectdialog::FieldKind::Number { value: 255, .. })));
    flush_config_write(&mut cx);
    let written = std::fs::read_to_string(dir.path().join("colours.toml")).unwrap();
    assert!(written.contains("[delta]\nhue = 255"), "{written}");
}
```

(`services_with_colours`: a builtin `colours` doc with `[delta] hue = 240` plus the keymap; the desk fork confirm appears on the builtin colour's first edit — press `enter` on it before the flush, as the Sources test does.)

- [ ] **Step 4: Harness entries**

```bash
# 2c §6.1: to_table writes only the keys in force — no dead hue under a token.
run_mutation "colours: to_table omits the hue under a token" \
  crates/geode-shell/src/shell/objectdialog/colours.rs \
  '    table.remove("hue");' \
  '    let _ = "hue";' \
  geode-shell \
  fields_seed_from_the_definition_and_to_table_writes_only_the_keys_in_force

# 2c §6.1: reserved names are taken on Colours alone.
run_mutation "objectdialog: reserved colour names are taken" \
  crates/geode-shell/src/shell/objectdialog/mod.rs \
  '        if self.reserved_names().contains(&name) {' \
  '        if false && self.reserved_names().contains(&name) {' \
  geode-shell \
  reserved_names_are_taken
```

Five CI checks; `--anchors-only`.

- [ ] **Step 5: Commit** — `git commit -m "feat(objectdialog): the Colours dialog — hue, tone, token, live swatches (2c §6.1)"`

---

### Task 6: Definitions travel; the blotter paints named colours

**Files:**
- Modify: `crates/geode-shell/src/shell/hot_reload.rs:275` (`views_changed` includes `changed("colours")`)
- Modify: `crates/geode-app/src/bridge.rs` (`DataSetup.colours`, `data_setup`, `start`, the `ConfigReloaded` arm)
- Modify: `crates/geode-blotter/src/content.rs` (`colours: Rc<RefCell<Arc<NamedColours>>>`, `new`, `set_colours`, `create`), `tile.rs` (`BlotterTile::new` takes it; the delegate receives it on every plan), `delegate.rs` (`render_td` `Colour::Named` arm; `render_th` override)
- Create: `crates/geode-blotter/src/colour_cache.rs`
- Test: `colour_cache.rs` unit tests; `tile.rs`/`delegate.rs` tests; `bridge.rs` test; `hot_reload` test in `shell/tests/reload.rs`

**Interfaces:**
- Produces:
  ```rust
  // geode-blotter colour_cache.rs
  pub struct ColourCache { key: Option<(Anchors, Tokens)>, by_name: HashMap<String, Hsla> }
  impl ColourCache {
      pub fn new() -> Self;
      /// The colour for `name` under `theme`, computed once per (anchors, tokens) and reused until they change.
      pub fn get(&mut self, colours: &NamedColours, name: &str, theme: &Theme) -> Option<Hsla>;
      pub fn misses(&self) -> u64;   // test hook: bumps when a key change empties the map
  }
  // BlotterFactory::set_colours(&self, colours: NamedColours); BlotterTile holds Rc<RefCell<Arc<NamedColours>>>; BlotterDelegate { colours: Arc<NamedColours>, colour_cache: ColourCache, .. }
  // bridge: DataSetup { .., colours: NamedColours }
  ```

- [ ] **Step 1: Failing tests**

`colour_cache.rs`:

```rust
    #[test]
    fn a_steady_theme_costs_no_recompute_and_a_changed_anchor_empties_the_cache() {
        let mut colours = NamedColours::default();
        colours.insert("delta".into(), Definition::Hue { degrees: 240.0, tone: Tone::Normal });
        let theme_a = fake_theme(blue = 0x2040c0);   // build two `Theme`s differing in `blue` — see the note below
        let mut cache = ColourCache::new();
        let first = cache.get(&colours, "delta", &theme_a).unwrap();
        assert_eq!(cache.get(&colours, "delta", &theme_a), Some(first));
        assert_eq!(cache.misses(), 1);
        let theme_b = fake_theme(blue = 0x40c020);
        let second = cache.get(&colours, "delta", &theme_b).unwrap();
        assert_ne!(first, second);
        assert_eq!(cache.misses(), 2);
        assert_eq!(cache.get(&colours, "ghost", &theme_b), None);
    }
```

(A `Theme` is constructible as `Theme::default()` — check `gpui_component::Theme: Default` in the pinned checkout; then set `theme.blue = gpui::rgb(..).into()` on the `ThemeColor` it derefs to. If `Default` is not available, key the cache on `(Anchors, Tokens)` taken as parameters — `get(&mut self, colours, name, anchors: &Anchors, tokens: &Tokens)` — and have the delegate pass `anchors_from_theme(&cx.theme())`; the test then needs no `Theme` at all. Prefer this second shape if in doubt: it keeps the cache pure.)

`shell/tests/reload.rs`: `a_colours_change_fires_config_reloaded` — apply a reload whose only change is the `colours` doc and assert `ShellEvent::ConfigReloaded` was emitted (mirror `apply_reload_with_a_clean_config_applies_it_and_closes_the_palette`'s setup and the picker test's subscribe idiom).

`tile.rs`/`delegate.rs`: `a_named_column_paints_its_resolved_colour` — build a delegate with a plan whose column has `Colour::Named("delta")` and `colours` holding `delta`, call the pure helper the delegate uses (`cell_colour(&mut self, col_ix, anchors, tokens) -> Option<Hsla>`) and assert `Some`; with an unknown name assert `None`.

- [ ] **Step 2: Implement**

- `hot_reload.rs`: the binding becomes two lines, `let views_changed =` then `changed("views") || changed("view_presentation") || changed("dimensions") || changed("colours");` (its own line, the harness anchor), with a comment (a named colour is part of what a tile paints, and it travels like a view).
- `bridge.rs`: `DataSetup.colours: NamedColours` read from `config.doc("colours")` (diagnostics extended); `start` passes it to `BlotterFactory::new(.., colours)`; the `ConfigReloaded` arm re-reads it and calls `factory.set_colours(..)`; the bridge tests' `BlotterFactory::new` calls gain `NamedColours::default()`.
- `content.rs`: the field, constructor parameter, `set_colours`, `create` passes the `Rc` to `BlotterTile::new`.
- `tile.rs`: hold it; wherever the tile builds/refreshes the delegate's plan, hand the delegate `Arc::clone(&*colours.borrow())`.
- `colour_cache.rs` (pure over `Anchors`/`Tokens`):

```rust
    pub fn get(&mut self, colours: &NamedColours, name: &str, anchors: &Anchors, tokens: &Tokens) -> Option<Hsla> {
        let key = (*anchors, *tokens);
        if self.key.as_ref() != Some(&key) {
            self.key = Some(key);
            self.by_name.clear();
            self.misses += 1;
        }
        if let Some(hit) = self.by_name.get(name) { return Some(*hit); }
        let def = colours.get(name)?;
        let hsla = to_hsla(resolve(def, anchors, tokens));
        self.by_name.insert(name.to_string(), hsla);
        Some(hsla)
    }
```

  (`Anchors`/`Tokens` derive `Copy, PartialEq` in Task 1; take the second shape from Step 1's note — the cache never sees a `Theme`.) Register `pub mod colour_cache;`.
- `delegate.rs`:

```rust
    /// The resolved named colour of column `col_ix`, or `None` for `none`,
    /// `sign` and a name the doc lacks (painted in foreground, §6.3).
    pub fn cell_colour(&mut self, col_ix: usize, anchors: &Anchors, tokens: &Tokens) -> Option<Hsla> {
        let name = match self.plan.as_ref().and_then(|p| p.columns.get(col_ix)).map(|c| &c.format.colour) {
            Some(Colour::Named(name)) => name.clone(),
            _ => return None,
        };
        self.colour_cache.get(&self.colours, &name, anchors, tokens)
    }
``` In `render_td`: compute `anchors_from_theme(&theme)`/`tokens_from_theme(&theme)` once at the top (they are twelve+fifteen `to_rgb` calls — cheap; or cache them on the delegate keyed on the theme's `mode` and a hash of the Hsla values, if a profile ever shows it) and, in the `Attribution::Additive` arm, `match (colour, cell.sign)`: `Some(Colour::Named(_))` → `el.text_color(self.cell_colour(..).unwrap_or(theme.foreground))`. Implement `render_th`: the default's `div().size_full().child(name)` plus `.text_color(colour)` when the column resolves to a named colour (read the plan's format for `col_ix`).

Run everything — green.

- [ ] **Step 3: Harness entries**

```bash
# 2c §6.2: a colours change reaches the tiles like a views change.
run_mutation "hot_reload: a colours change fires ConfigReloaded" \
  crates/geode-shell/src/shell/hot_reload.rs \
  '                changed("views") || changed("view_presentation") || changed("dimensions") || changed("colours");' \
  '                changed("views") || changed("view_presentation") || changed("dimensions");' \
  geode-shell \
  a_colours_change_fires_config_reloaded

# 2c §6.3: the cache empties when an anchor changes.
run_mutation "blotter: the colour cache invalidates on a changed anchor" \
  crates/geode-blotter/src/colour_cache.rs \
  '        if self.key.as_ref() != Some(&key) {' \
  '        if self.key.is_none() {' \
  geode-blotter \
  a_steady_theme_costs_no_recompute_and_a_changed_anchor_empties_the_cache

# 2c §6.3: an unknown name paints in foreground, never a stale colour.
run_mutation "blotter: an unknown colour name resolves to none" \
  crates/geode-blotter/src/delegate.rs \
  '        self.colour_cache.get(&self.colours, &name, anchors, tokens)' \
  '        self.colour_cache.get(&self.colours, &name, anchors, tokens).or(Some(gpui::Hsla::default()))' \
  geode-blotter \
  a_named_column_paints_its_resolved_colour
```

Five CI checks; `--anchors-only`.

- [ ] **Step 4: Commit** — `git commit -m "feat(blotter): named colours travel like views and paint at the paint site (2c §6.2–§6.3)"`

---

### Task 7: The theme checks

**Files:**
- Modify: `crates/geode-shell/src/theme.rs` (`mod tests`)

- [ ] **Step 1: The test** (a `#[gpui::test]`, since reading anchors goes through `Theme::global_mut(cx).apply_config`)

```rust
    /// 2c §7: every bundled theme keeps every generated hue readable, in
    /// both tones, and its anchor arcs are reported so a folded palette is
    /// a known number rather than a surprise.
    #[gpui::test]
    fn every_bundled_theme_keeps_generated_hues_readable(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = load_bundled();
        let mut worst: Vec<(String, f32)> = Vec::new();
        for entry in &service.entries {
            let (anchors, tokens, background) = cx.update(|cx| {
                Theme::global_mut(cx).apply_config(entry);
                let theme = cx.theme();
                (crate::shell::colours::anchors_from_theme(&theme), crate::shell::colours::tokens_from_theme(&theme), crate::shell::colours::to_rgb(theme.background))
            });
            let _ = tokens;
            let mut smallest_arc = f32::MAX;
            for i in 0..6 {
                let a = geode_core::colour::oklab::lab_to_lch(geode_core::colour::oklab::srgb_to_oklab(anchors.normal[i]));
                let b = geode_core::colour::oklab::lab_to_lch(geode_core::colour::oklab::srgb_to_oklab(anchors.normal[(i + 1) % 6]));
                let arc = ((b.h - a.h + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI).abs().to_degrees();
                smallest_arc = smallest_arc.min(arc);
            }
            worst.push((entry.name.to_string(), smallest_arc));
            for tone in [geode_core::colour::Tone::Normal, geode_core::colour::Tone::Light] {
                for step in 0..12 {
                    let colour = geode_core::colour::interpolate_hue(step as f32 * 30.0, tone, &anchors);
                    let ratio = geode_core::colour::contrast_ratio(colour, background);
                    assert!(ratio >= 3.0, "{}: hue {} ({tone:?}) reads {ratio:.2}:1 against the background", entry.name, step * 30);
                }
            }
        }
        worst.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        eprintln!("smallest anchor arcs: {:?}", &worst[..worst.len().min(5)]);
    }
```

Run it. **If a bundled theme fails the 3:1 assertion**, that is a real finding: record which theme and hue in the report, and — rather than loosening the ratio — add that theme's anchors to the report and leave the test asserting; the controller rules whether the theme is adjusted (as `theme.rs` already adjusted Nord's `chart_bearish`) or the ratio is lowered for that theme with a named exception. Do not silently relax.

- [ ] **Step 2: Harness entry**

```bash
# 2c §7: the theme check asserts a real ratio, not a formality.
run_mutation "theme: generated hues must clear 3:1" \
  crates/geode-shell/src/theme.rs \
  '                    assert!(ratio >= 3.0, "{}: hue {} ({tone:?}) reads {ratio:.2}:1 against the background", entry.name, step * 30);' \
  '                    assert!(ratio >= 3.0 || true, "{}: hue {} ({tone:?}) reads {ratio:.2}:1 against the background", entry.name, step * 30);' \
  geode-shell \
  every_bundled_theme_keeps_generated_hues_readable
```

(This entry "survives" by construction unless a theme is right at the edge — its value is documenting that the assertion is live; if it survives, replace it with an entry that mutates `contrast_ratio`'s formula (`(hi + 0.05) / (lo + 0.05)` → `hi / lo`) under `geode-core`'s `contrast_ratio_is_wcag` test instead, and say so in the report.)

- [ ] **Step 3: Commit** — `git commit -m "test(theme): every bundled theme keeps generated hues readable; anchor arcs reported (2c §7)"`

---

### Task 8: Docs, harness as a set, full verification

**Files:** `CLAUDE.md`, the 2c spec (a new `## 9. As built` section), `scripts/mutation-check.sh` (count if stated), the 4c spec if any §19/§20 sentence went stale.

- [ ] **Step 1: Reconcile the spec.** Read §1–§8 against the code; write `## 9. As built` in the 4c spec's §19.8 shape: per task what shipped, every deviation with its reason (the theme-check outcome, the cache's key shape, any renamed function), the deferred minors the task reviews collected, the display-pending list (swatches, header colour, member summary, the crumb), the harness count.
- [ ] **Step 2: `CLAUDE.md`.** One paragraph `**Phase 4c Part 2c is done**` after the Part 2b paragraph, in the house style: named colours (`colours.toml`, hue/tone/token, OKLCH over the theme's anchors, `resolve` pure in core, `Colour::Named`, unknown-name warning with path), the overlay's `[view.columns.<col>]` tables with both spellings read and the differing-keys writer, `ListItem.presentation`, `Stage::Column` as a projection (fold on `revalidate`, `field_by_key`'s parent fallback — the trap: a new consumer that reads `draft.fields` for the view's list while a column is open sees seven presentation fields, not the view), `Number.step/wrap`, the Colours dialog with swatches, definitions travelling through the bridge and resolving at paint behind `ColourCache`, the theme check. Update the harness count on the command line.
- [ ] **Step 3: Verification** — the five CI checks, `--anchors-only`, `git status --porcelain` empty, then `nohup zsh scripts/mutation-check.sh --changed=main > /tmp/mut.log 2>&1 &` and wait for `0 SURVIVED` (a survivor is a test that cannot see its behaviour — fix the test).
- [ ] **Step 4: Commit** — `git commit -m "docs: Phase 4c Part 2c as built, CLAUDE.md, harness count"`. Then `superpowers:finishing-a-development-branch`.
