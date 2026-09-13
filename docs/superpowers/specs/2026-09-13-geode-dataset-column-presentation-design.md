# Geode — dataset-level column presentation

**Date:** 2026-09-13
**Status:** approved in conversation, spec for review
**Builds on:** `2026-09-13-geode-phase-4c-part-2c-columns-and-colours-design.md`
(the column stage, §5; the overlay reshape, §4) and
`2026-09-08-geode-phase-4c-config-dialogs-design.md` (§19, the Schema
inspector and the read-only gate).

## 1. Scope

### 1.1 The problem

Part 2c personalises a column's presentation — label, width, scale,
precision, thousands, negative, colour — **per view**, in
`view_presentation.toml`. A trader who wants `delta01` shown in
thousands and painted `delta` wherever it appears has to say so once
per view that shows it, and again in every view the desk adds later.
The natural home of "how this column looks" is the column itself: the
dataset's column, used by whichever views carry it, with a view able to
diverge where it needs to.

### 1.2 What this delivers

- A second personal overlay, `dataset_presentation.toml`, one table per
  personalised column of a dataset, carrying the seven presentation keys
  and nothing else (§2).
- A resolution order every view of that dataset paints by:
  **kind default → desk view column → trader's dataset-level → trader's
  view-level** (§3). The dataset level beats the desk view's own keys;
  only the trader's own view-level override sits above it.
- The Schema inspector gains the column stage as its one writable door:
  `enter` (or a click) on a column row opens the same seven-field stage
  the Views dialog has, writing the dataset overlay (§4).
- The Views column stage keeps working per view, but its baseline is
  now desk + dataset, its fold notice names the layer a cleared key
  falls to, and every field shows which layer's value is in force (§5).
- A dataset-level edit reaches every open blotter tile on the next
  delivery, like a view overlay edit (§6).

### 1.3 Done state

1. `[risk.columns.delta01]\nscale = "k"\ncolour = "delta"` in the user
   layer's `dataset_presentation.toml` paints `delta01` in thousands and
   `delta` in every view of `risk`, including one the desk wrote with
   `format = { scale = "units" }` for that column, and including a view
   that reaches `delta01` through a join.
2. `[tree.columns.delta01]\nscale = "m"` in `view_presentation.toml`
   wins in `tree` alone; every other view of `risk` keeps `k`.
3. In the Schema dialog, `enter` on `delta01` opens a stage crumbed
   `risk › delta01`; `i` on Width, `160`, `enter` lands
   `[risk.columns.delta01]\nwidth = 160` on disk and every open tile of
   any view of `risk` paints the column 160 px wide on its next delivery.
4. In the Views dialog's column stage for `tree › delta01`, Scale shows
   `k` with a `dataset` chip; stepping it to `m` writes `scale = "m"`
   under `[tree.columns.delta01]`; stepping it back to `k` removes that
   key rather than writing an equal one, and the chip reads `dataset`
   again.
5. `d` and `r` are refused in both column stages with the existing
   notice; `d`, `r`, `n`, ticks and drops on the Schema dialog's own rows
   still answer the read-only notice.

### 1.4 Explicitly not in this

- A **desk-level** dataset default (`datasets.toml` gaining presentation
  keys). The desk's baseline stays the view's own `[[columns]]` keys.
- A one-shot "clear this column's personalisation" verb; clearing is
  field by field, as in the view stage.
- A scope toggle inside the Views column stage.
- Any change to hiding, ordering, membership, drag-and-drop or the write
  door.

## 2. The doc

### 2.1 Shape

`dataset_presentation.toml`, **user layer only** (like
`view_presentation.toml`), `atomic_depth` 3, so a write replaces one
`[<dataset>.columns.<col>]` table:

```toml
[risk.columns.delta01]
label = "Δ"
width = 90
scale = "k"
precision = 0
thousands = true
negative = "parens"
colour = "delta"
```

The seven keys are the view overlay's per-column keys (2c §4.1) with the
same spellings and the same parsers. `hidden` and `order` are **refused
with a warning naming `view_presentation.toml`** — membership and
sequence belong to a view. There is no legacy flat spelling: a new doc
gets one shape from day one.

### 2.2 The reader

`DatasetPresentationSpec::from_doc(&MergedDoc) -> (Self, Vec<Diagnostic>)`
in `geode-core::view`, beside `ViewPresentationSpec`:

```rust
pub struct DatasetPresentationSpec {
    /// dataset → column → presentation
    pub datasets: BTreeMap<String, BTreeMap<String, ColumnPresentation>>,
}
```

It reuses `ColumnPresentation::parse_format_keys` and
`parse_column_keys(table, hidden_allowed = false, warn)`. Every
diagnostic carries a path in the existing convention:
`dataset_presentation.risk.columns.delta01.scale`. A top-level key that
is not a table, a `columns` value that is not a table, a column table
holding an unknown key: each warns with its path and is skipped; the
rest of the file loads.

### 2.3 Cross-checks at load

`load_views` has the schema doc in hand. A dataset no schema declares
warns `dataset_presentation.<ds>: 'columns' names dataset '<ds>', which
no schema declares — ignored`; a column its dataset lacks warns
`dataset_presentation.<ds>.columns.<col>: names column '<col>', which
dataset '<ds>' does not have — ignored`. Both skip that entry only. A
`colour` naming no colour in `colours.toml` warns through the same
cross-check `load_views` already runs for view columns (2c §3), with the
new doc's path.

## 3. Resolution

### 3.1 Where the layer merges

`ViewSpec::from_doc` already folds the desk view's own `[[columns]]`
`label`, `width` and `format` into `ViewSpec.presentation` (2c §4.4), and
`ViewPresentationSpec::apply` merges the trader's view overlay over that
map. The dataset overlay merges into the same map **between the two**:

```text
load_views:
  ViewSpec::from_doc            desk keys        → view.presentation
  DatasetPresentationSpec::apply dataset overlay  → merge_over, per column
  ViewPresentationSpec::apply   view overlay     → merge_over, per column
```

`merge_over` is the existing `Option`-wise merge: a set key wins, an
unset one leaves the layer below in place. The resolved order is
therefore kind default → desk view column → dataset-level →
view-level, per key. `ColumnPlan::build` reads the merged map through
`presentation_of` unchanged.

### 3.2 Which dataset owns a column

A view can join. `DatasetPresentationSpec::apply(&self, views: &mut
[ViewSpec], schema: &SchemaSpec) -> Vec<Diagnostic>` looks a column up
in the view's **own dataset first, then each join's dataset in file
order, first match wins** — the order the compiler resolves names in.
A column no dataset of the view declares (a derived column) takes
nothing from this layer.

### 3.3 What a cleared key falls to

Removing a dataset-level key exposes the desk view's own key, per view.
Removing a view-level key exposes the dataset-level key if set, else the
desk's. The dialogs say which (§4.4, §5.2).

## 4. The Schema dialog's column stage

### 4.1 The door

`Domain::Schema`'s browse list and column rows are unchanged. `enter`
on a column row — and a row click, by the mouse-parity rule (4c §18.9)
— opens `Stage::Column { object: <dataset>, column: <col> }` crumbed
`risk › delta01`, with the seven fields, `i`, `space`, `shift+space`,
`enter`, `escape` and the footer chips of the Views column stage. The
fields' destination is a new variant:

```rust
pub enum Destination {
    Doc,
    Presentation,
    /// `dataset_presentation.toml`, user layer.
    DatasetPresentation,
}
```

`apply::object_value` answers `ObjectWrite::Remove` for an emptied
rendering under `DatasetPresentation` exactly as it does under
`Presentation` (Part 2a's Major): the overlay's absence IS "no
personalisation". `config_write::edit` targets doc
`dataset_presentation`, key path `[<ds>.columns.<col>]`, ordinary
debounce, the error-diagnostic gate.

### 4.2 The read-only gate grows a stage

`Domain::writable(self) -> bool` becomes
`Domain::writable(self, stage: &Stage) -> bool`. Every domain but
Schema ignores the stage. Schema answers `true` only for
`Stage::Column { .. }`. All ten sites that gate on it today
(`handle_edit_key`, `actions()`, both footers, browse `n`, the dest
badge, `edit_commit_notice`, `press_verb`, `on_tick_clicked`,
`on_row_dropped`) pass the current stage; the compiler finds any site
that does not. The read-only notice keeps its text.

### 4.3 What a field shows

Seed order per field: the dataset overlay's value if the table sets the
key, else the kind default (`views::kind_default`), with the column's
own name for label and `auto` for width. A set key carries
`Field.layer = Some(Layer::User)` — the badge slot Schema already fills
— and an unset one `None`. There is no desk value to show: the desk's
setting varies per view and sits **below** this layer.

### 4.4 Clearing

An emptied label or an `auto` width removes the key; the field re-seeds
to the default and the notice reads `label follows each view again` /
`width follows each view again`. Any other key steps back through its
choices; a `Number` cannot be cleared, only set.

### 4.5 Writing

`views::dataset_presentation_table(item) -> toml_edit::Table` emits only
keys that differ from the kind default (label from the column name,
width from `auto`), the same differing-keys logic as
`presentation_table` with the kind default as its baseline. An empty
table is `Remove`.

### 4.6 `d` and `r`

Refused in this stage with the existing `<key> is not a verb in a
column's stage` notice, keeping the two column doors identical.

### 4.7 The browse row

A Schema column row whose dataset table sets any key appends the same
summary the Views member row shows — `views::column_summary` over the
dataset-level presentation alone — after its type/role text:
`f64 · measure · k · 0 dp · delta`.

## 5. The Views column stage with a layer under it

### 5.1 The baseline

`views::desk_baseline(draft)` becomes `views::baseline_below(draft,
dataset_overlay)`: the desk keys parsed from the draft's source, with
the column's dataset-level entry merged over, read through
`apply::config_with_pending` (the 2c final review's M-6 lesson: never
`services.config` alone). Both the fold and the writer read it. A field
set to the value the baseline already gives writes nothing, so a view
override exists only where the trader diverges.

### 5.2 Clearing names the layer

The fold notice becomes `<key> follows the dataset again` when the
dataset overlay sets that key, `<key> follows the desk again` otherwise.

### 5.3 Provenance on every field

```rust
pub enum Provenance { Desk, Dataset, View }
```

Every field in the Views column stage carries `Field.provenance:
Option<Provenance>` — `View` when the view overlay sets the key,
`Dataset` when the dataset overlay does, `Desk` when the desk view's own
keys do, `None` when the kind default is in force. Painted through
`dialog::badge` as a lowercase chip (`desk` / `dataset` / `view`) in the
same slot the layer badge uses; the two never appear together (Schema
fills `layer`, Views fills `provenance`). In the Schema stage the field
reads `user` or nothing (§4.3) — it has no desk value to name.

### 5.4 Everything else

`ListItem.presentation` — the member row summary, the `included`
truth — is derived from the merged map the loaded `ViewSpec` carries,
which now includes the dataset level, so the summary shows effective
values. Hiding, ordering, membership, drag-and-drop, the write door and
the drift table are untouched.

## 6. Reload and the blotter

`hot_reload::apply_reload`'s `views_changed` gains
`|| changed("dataset_presentation")`. The bridge's `ConfigReloaded`
handler already re-runs `load_views` (which now applies the new layer)
and hands the views to the factory; the blotter's plan is rebuilt on the
next delivery whenever it differs (2c §9.14). Nothing in `geode-blotter`
changes. `data_setup` extends its startup diagnostics with the new
doc's, as it does for `view_presentation` and `colours`.

## 7. Checks and the harness

**Core.** `from_doc`: every diagnostic's path; unknown dataset and
unknown column warn and skip; `hidden`/`order` refused naming the view
overlay; unknown colour cross-checked. `apply`: the same key at all
three layers resolves desk < dataset < view per key; dataset alone beats
a desk view's explicit key; a joined column takes the join's entry, the
own dataset first when both declare it; a derived column takes nothing.

**Dialog, pure.** `Domain::Schema.writable(stage)` iterated over every
`Stage` variant, true only for `Column`. Dataset stage: seeding from
overlay / kind default, fold with the `follows each view again` notice,
writer emits only keys off the kind default, `Remove` on empty. Views
stage: a field equal to the dataset value writes nothing; fold notice
names `dataset` vs `desk`; `Provenance` resolves to the right variant
for each layer.

**Window.** `enter` and a click on a Schema column row open the stage
crumbed `risk › delta01`; `i`, `160`, `enter` lands
`[risk.columns.delta01]\nwidth = 160` on disk; `d`/`r` refused; the
Schema row shows the summary afterwards; the read-only notice still
answers the schema rows' own verbs. Bridge: a `dataset_presentation`
change emits `ConfigReloaded` and the tile's plan carries the new
label on its next delivery.

**Harness.** One entry per load-bearing line, each naming its test: the
merge order in `apply`, the own-dataset-first join rule, the Schema arm
of `writable(stage)`, the Views writer's baseline, the fold notice's
layer choice, the reload predicate.

**Display checks pending** on a real window: the provenance chip, the
Schema row summary, the crumb in the Schema stage.

## 8. Sequencing

1. Core: `DatasetPresentationSpec` (`from_doc`, `apply`), the merge
   point in `load_views`, cross-checks, tests, harness.
2. Reload: the predicate, `data_setup` diagnostics, bridge test.
3. Dialog scaffold: `Destination::DatasetPresentation`, `writable(stage)`
   at all ten sites, `Provenance`, `Field.provenance`, the badge.
4. Schema door: `enter`/click into the column stage, seeding, fold,
   writer, refusals, row summary, window tests, harness.
5. Views stage: `baseline_below`, fold notice, provenance, the
   nothing-when-equal writer, tests, harness.
6. Docs: `## 9. As built` here, CLAUDE.md paragraph, harness count.
