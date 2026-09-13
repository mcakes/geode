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
   `format = { scale = "none" }` for that column, and including a view
   that reaches `delta01` through a join. (**Amended (as built)**: the
   original read `scale = "units"`, which is not one of the three
   spellings the reader accepts — see item 2's note.)
2. `[tree.columns.delta01]\nscale = "M"` in `view_presentation.toml`
   wins in `tree` alone; every other view of `risk` keeps `k`.
   (**Amended (as built)**: millions is spelled `"M"`, never `"m"` —
   `views::scale_key`/`scale_keys` write `none`/`k`/`M` and
   `ColumnPresentation::parse_format_keys` reads back exactly those
   three. A lowercase `"m"` parses to no scale at all, silently; it was
   caught in a Task 5 fixture that would otherwise have passed for no
   reason.)
3. In the Schema dialog, `enter` on `delta01` opens a stage crumbed
   `risk › delta01`; `i` on Width, `160`, `enter` lands
   `[risk.columns.delta01]\nwidth = 160` on disk and every open tile of
   any view of `risk` paints the column 160 px wide on its next delivery.
4. In the Views dialog's column stage for `tree › delta01`, Scale shows
   `k` with a `dataset` chip; stepping it to `M` writes `scale = "M"`
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

> **Amended (as built).** `atomic_depth("dataset_presentation")` is
> **1**, not 3 — one entry per *dataset* name, exactly as
> `view_presentation` is one per view. The writer
> (`dataset_columns::table`, §4.5) renders the whole `[<dataset>]`
> object — every personalised column of that dataset, the open one
> folded and the rest copied verbatim — so a later layer's table for a
> dataset must replace the earlier one whole rather than half-merging
> one trader's columns into another's. **Cost if wrong:** a user-layer
> dataset table could not half-merge over a desk one, which no layer
> but user writes anyway (this doc is user-layer only).

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

> **Corrected (as built): ownership is resolved by NAME, so a derived
> column whose name a dataset ALSO declares does take that dataset's
> entry.** `owner_of` asks each dataset in turn whether it has a column
> of that name; it never asks what kind the view's own column is. So
> `[[v.columns]] name = "npv" kind = "derived"` over a dataset that also
> declares `npv` inherits `[<ds>.columns.npv]`'s presentation — which is
> presentation only, and arguably what a trader who set a label for
> `npv` would want. Only a derived column NO dataset of the view names
> takes nothing (the final whole-branch review's Minor 7).

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

> **Amended (as built): one door, two dialogs, and the click is
> `enter`.** `render::column_stage_target(shell)` is the single answer
> to "does the selected row open a column stage, and for which
> column" — Views names an `EditRow::Item` by its label, Schema strips
> `columns.` off an `EditRow::Field` key (so a `derived.<name>` row
> falls through to the read-only notice) — and both
> `commit_selected_row` (`enter`, in either mode) and
> `on_edit_row_clicked` (the mouse) read it. A **member-row click in
> the Views edit stage therefore opens the column stage too**, closing
> the 2c spec §9.9 deferred minor "a member-row click still only
> selects". A click that opens leaves filter mode, exactly as `enter`
> already did — there is one rule about which rows are doors, not two.

> **Amended (as built): a `key` or `attribute` column opens the stage,
> and its kind comes from its TYPE.** §4.1 says "a column row" and that
> is read literally: every column the dataset declares is personalisable
> (label and width are exactly what a trader wants on a key column).
> `views::schema_role_kind` answers `None` for `Key` and `Attribute`,
> and `views::kind_default` treats anything that is not `dimension` as a
> measure — so without a fallback a `utf8` key opened with Precision 2,
> thousands on, a scale, a negative form and a `sign` colour, and one
> `space` wrote a real key into the overlay. `dataset_columns::
> kind_for_type(ColumnType)` is the fallback, reached only when the role
> names no kind: `Utf8 | Date | Timestamp | Bool` → `dimension` →
> `ColumnFormat::TEXT`; `F64 | I64` → `None` → MEASURE, which is right
> (a numeric attribute is formatted like a measure even though it does
> not sum). Keyed on the TYPE rather than the role deliberately — "how
> is this column formatted" is a question about its values, so a future
> role does not have to be remembered in two places.

### 4.2 The read-only gate grows a stage

`Domain::writable(self) -> bool` becomes
`Domain::writable(self, stage: &Stage) -> bool`. Every domain but
Schema ignores the stage. Schema answers `true` only for
`Stage::Column { .. }`. All ten sites that gate on it today
(`handle_edit_key`, `actions()`, both footers, browse `n`, the dest
badge, `edit_commit_notice`, `press_verb`, `on_tick_clicked`,
`on_row_dropped`) pass the current stage; the compiler finds any site
that does not. The read-only notice keeps its text.

> **As built, unchanged**, and recorded in the 4c dialogs spec §19.4
> where `writable()` was introduced: the gate is stage-aware, and
> Schema's column stage is its one writable surface. All ten sites pass
> `&state.stage`; `Domain::writable` is
> `!matches!(self, Domain::Schema) || matches!(stage, Stage::Column
> { .. })`.

### 4.3 What a field shows

Seed order per field: the dataset overlay's value if the table sets the
key, else the kind default (`views::kind_default`), with the column's
own name for label and `auto` for width. A set key carries
`Field.layer = Some(Layer::User)` — the badge slot Schema already fills
— and an unset one `None`. There is no desk value to show: the desk's
setting varies per view and sits **below** this layer.

> **Amended (as built): the per-field chip reads `dataset`, and `Field`
> has no `provenance` field.** A set key is *not* marked with
> `Field.layer = Some(Layer::User)`. Both column doors paint the same
> `Provenance` chip in the same slot (§5.3), computed **at paint** from
> `Draft::column_ctx` through `dataset_columns::provenance_of` — so a
> field stepped in the Schema stage reads `dataset` on the same frame
> rather than 250 ms later when the write lands, and the two doors share
> one vocabulary instead of one painting a layer badge and the other a
> provenance chip. A plan-level ruling, taken at the preflight scan: a
> `Field.provenance` **stored** on the field would go stale one
> keystroke after a step, and the painter has everything it needs.
> `Field.layer` survives untouched, filled by the Schema adapter's own
> browse/column *inspector* rows from `Config::explain`; a column stage
> leaves it `None`, so the two never appear together.

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

> **Amended (as built): it is `dataset_columns::table(draft) ->
> toml_edit::Table`**, in the new `objectdialog/dataset_columns.rs`
> beside the Views writer rather than inside `views.rs` — this doc is
> not a view's. It renders the **whole `[<dataset>]` object** (the
> atomic-depth-1 amendment to §2.1): the dataset's other personalised
> columns copied verbatim from `ColumnContext.overlay_object`, then the
> open column folded and emitted key-by-key. The five `ColumnFormat`
> keys compare RESOLVED through the kind default, never as raw
> `Option`s, for `presentation_table`'s own reason (the fold writes a
> `Some` for all five, so a raw compare would copy the whole kind
> default into the trader's file on the first keystroke); `label` and
> `width` compare as bare `Option`s, having no kind default to resolve
> against.
>
> **`table()` normalises the object to `columns` alone.** Sibling keys
> of `columns` under `[<dataset>]` are not carried through. `[<dataset>]`
> holds one key in this vocabulary, and a hand-written `order` or any
> unknown key is already refused by `DatasetPresentationSpec::from_doc`
> with a warning naming `view_presentation.toml` (§2.1) — so it affects
> nothing the app reads and nothing here is preserving state by keeping
> it. A write through this dialog does what that warning asks the trader
> to do by hand. The *columns* themselves ARE state and are copied
> verbatim.

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

> **Amended (as built): the signature is `views::baseline_below(draft)`,
> and the dataset layer travels on the draft.** `Draft.dataset_layer:
> BTreeMap<String, ColumnPresentation>` is filled by `Domain::draft`
> (for `Domain::Views` alone) and refreshed by
> `render::enter_column_stage` from the pending-aware config, because
> `Domain::to_table` — the writer's own entry point, and exactly where
> getting this wrong is silent — has no `Config` to read it from.
> `views::dataset_layer_for(config, view_name)` is what both call;
> **an unknown view name answers an empty map, not a panic** — a draft
> can outlive the object it describes for as long as one keystroke.

> **Amended (as built): `views::dataset_layer` resolves over every
> column the view's datasets DECLARE, not only `view.columns`.** The
> candidate set is the view's own dataset plus each join's, deduped,
> each name then resolved through the unchanged
> `DatasetPresentationSpec::owner_of` (so ownership precedence is
> exactly §3.2's — a dataset that declares a column but says nothing
> about it still owns it, and a later join's table for the same name
> must not stand in). Only the candidate SET widened. The reason is
> promotion: a column promoted out of the available catalogue in the
> same draft lifetime **is not in `view.columns`** on the keystroke that
> writes it, so a `view.columns`-shaped layer left the baseline empty
> for it and the writer emitted the trader's own dataset-level value
> back as a spurious per-view override.

### 5.2 Clearing names the layer

The fold notice becomes `<key> follows the dataset again` when the
dataset overlay sets that key, `<key> follows the desk again` otherwise.

> **Amended (as built): a clear is measured against the ITEM's pre-fold
> value, not against the baseline.** `views::fold_into` names a key as
> cleared only when *this keystroke* emptied it — the label text is now
> empty where the item had a label, the width is now `auto` where the
> item had a width — never "whenever the rendering is empty regardless
> of the baseline". The literal reading flagged `width` on every fold of
> a width-less column (`column_fields` seeds `auto` whenever the item
> has no width) and, since the `width` arm runs after the `label` arm,
> **overwrote a genuinely cleared label's notice with a `width` nobody
> touched** — the trader's notice going missing on the keystroke it
> exists for. The item-relative rule also restores the doc's claimed
> at-most-one-key-per-fold invariant. **Cost if wrong:** a notice missed
> on an exotic double-clear; nothing is written wrong either way.
> `Fold { key, to: Option<FellTo> }` is the shape, and
> `FellTo::{Desk, Dataset, EachView}` is the third spelling `to: None`
> says nothing at all.

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

> **Amended (as built): nothing is carried on `Field`, and both doors
> paint the chip.** There is no `Field.provenance` (§4.3's amendment):
> `dataset_columns::provenance_of(&ProvenanceInputs, &Field)` answers
> the variant at paint from `Draft::column_ctx`, and
> `render::provenance_chip` paints it through `dialog::badge` with the
> element id `objectdialog-field-provenance-{key}`. `ProvenanceInputs`
> is built **once before the row loop**, not per field, because both its
> halves allocate and seven clones a frame is per-frame heap churn on
> the render thread; its `below` is `Some` for `ColumnDoor::View` and
> `None` for `ColumnDoor::Dataset`, which compares against
> `ColumnPresentation::default()` and never reads it. The Schema stage's
> chip reads `dataset`, not `user`.

> **Amended (as built): `ColumnLayers` holds TWO layers, and the chip's
> `view` means "differs from desk + dataset".** The first cut captured
> the view overlay as a third `ColumnLayers` field and read it as
> `differs_from(below) || set_in(&ctx.layers.view)`, which made a field
> stepped **back** to the value the layer below already gives keep
> reading `view` until the stage was closed and reopened — the captured
> layer still held the key the write had yet to remove (the final
> whole-branch review's named risk 4). The clause is gone and, with its
> one reader, so is the field: `ColumnLayers` is `desk` + `dataset`, and
> `views::column_layers` no longer reads `view_presentation.toml` at
> all.
>
> **Ruling:** the chip reads `View` when, and only when, the field
> differs from the desk + dataset baseline. A view key EQUAL to the
> layer below is unobservable — the resolved value is identical with or
> without it, and `views::presentation_table` omits every such key, so
> the writer cannot produce one; the only sources are a hand-edited
> `view_presentation.toml` or a desk/dataset change that happened to
> coincide with an existing override, and in both cases the next
> keystroke in this stage removes it. Naming `dataset`/`desk` there is
> therefore not merely defensible, it is what the file will say 250 ms
> later. **Cost if wrong:** a field showing exactly the value below
> would be badged `dataset`/`desk` while a stale equal key sat in the
> overlay until the next keystroke swept it — advisory only; nothing is
> written differently. This satisfies §1.3's done-state item 4 on the
> same frame rather than on reopen, and supersedes this section's
> literal "`View` when the view overlay sets the key".

### 5.4 Everything else

`ListItem.presentation` — the member row summary, the `included`
truth — is derived from the merged map the loaded `ViewSpec` carries,
which now includes the dataset level, so the summary shows effective
values. Hiding, ordering, membership, drag-and-drop, the write door and
the drift table are untouched.

> **Amended (as built): the AVAILABLE catalogue's items carry the
> dataset-level presentation too** (`views::dataset_catalogue` takes the
> dataset's own overlay entries, supplied by `views::dataset_overlay`,
> at both call sites — `views::fields` and `views::refresh_available`).
> The catalogue is the one place a promoted item is BORN:
> `Draft::step_selected`'s `Available` arm moves that `ListItem` onto
> the view's own list verbatim and nothing re-derives it, so a
> `ColumnPresentation::default()` seed meant the stage opened seven
> fields at the kind default over a baseline holding the trader's own
> dataset-level values, and the first keystroke folded all seven into
> the view overlay. With the seed, the stage, the writer and the member
> summary all agree. **Cost if wrong:** none — a merge at the door alone
> would leave the member summary blank. A lookup by the catalogue's own
> dataset name IS `owner_of`'s answer for those columns (a catalogue is
> by construction the columns of exactly one dataset), and `hidden` can
> never be among the keys, the overlay's reader refusing it (§2.1).
> Consequence worth a display check: **available rows now show
> dataset-level values in their summary**.

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

> **Recorded (as built): the error-diagnostic gate on a Schema
> column-stage write reads the DATASETS doc's own diagnostics.**
> `apply::blocking_diagnostic` reads the open draft's `diagnostics`, and
> the draft in a Schema column stage is still the Schema draft, whose
> `Domain::validate` is `schema::validate` — `SchemaSpec::from_doc` over
> that one dataset. So an error-severity defect in `datasets.toml` (a
> grain in use whose key column is undeclared, say) refuses **every
> dataset-level presentation edit on that dataset**, with a notice about
> something the read-only inspector cannot fix. Known and as specified:
> the gate is 4c §19 Part 2a's one safety property — `reload::decide`
> rejects a config holding an error diagnostic, so an edit rated
> `Severity::Error` must never reach the batch or memory and disk
> disagree — and narrowing it per destination would reopen it. A
> `Severity::Warning` never blocks.

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

## 9. As built

Shipped as six tasks on `worktree-dataset-column-presentation`
(`38e4db8`..`35e5627` on top of `691cd92`), 14 commits, 14 files,
+3,413 / −302. **21 harness entries added (747 → 768)** and nine
pre-existing entries re-anchored where these tasks moved their source
lines; `--anchors-only` reports 0 stale, 0 ambiguous. §1.3, §2.1, §4.1,
§4.2, §4.3, §4.5, §5.1, §5.2, §5.3, §5.4 and §7 are amended in place
above where the build or a ruling contradicted them, each keeping its
original wording under an **Amended (as built)** note. What follows is
what a maintainer has to know that the design did not say, task by task,
then the rulings with their reasons and costs, the deferred minors, the
harness arithmetic and the display-pending list.

### 9.1 Task 1 — `DatasetPresentationSpec`, the merge point, the cross-checks

`crates/geode-core/src/view.rs` gained `DATASET_PRESENTATION_DOC` and
`DatasetPresentationSpec { datasets: BTreeMap<String, BTreeMap<String,
ColumnPresentation>> }` with `from_doc`, `owner_of` and `apply`, beside
`ViewPresentationSpec`; `config/load.rs`'s `load_views` merges the
overlay between `ViewSpec::from_doc` and `ViewPresentationSpec::apply`;
`config/merge.rs` puts `dataset_presentation` in the `atomic_depth`
`Some(1)` arm (§2.1's amendment).

- **The colour cross-check is unconditional on the dataset overlay
  alone** — a review Critical, and the one defect of this task. First
  written *inside* `if let Some(doc) = config.doc("view_presentation")`,
  so with no `view_presentation.toml` in any layer — the default state —
  an unknown `colour` in `dataset_presentation.toml` was never warned
  about, §2.3's requirement silently unmet. The test that should have
  caught it shipped BOTH docs in its fixture. It is now hoisted to run
  right after the `colours` binding, gated only on `if let Some(overlay)
  = &dataset_overlay`, and `a_dataset_overlay_colour_is_cross_checked_without_a_view_overlay`
  is the test with deliberately no view overlay. `load_views`' own doc
  comment now describes all three cross-check sites and why this one is
  not nested.
- **The harness entry for it mutates `&overlay.datasets` to
  `overlay.datasets.iter().take(0)`, not the `if let` to `if false`.**
  The latter leaves `overlay` unbound, so the entry would have been a
  disguised compile error reported as a `caught` — the harness's own
  "caught for the wrong reason" failure mode.
- **The diagnostics' messages follow this crate's own convention, not
  §2.3's sketch**: `dataset presentation '<ds>': names dataset '<ds>',
  which no schema declares — ignored` at path
  `dataset_presentation.<ds>`, and `dataset presentation '<ds>': names
  column '<col>', which dataset '<ds>' does not have — ignored` at
  `dataset_presentation.<ds>.columns.<col>`. The path carries the
  location; the message names the object the way every other reader in
  `view.rs` does.
- `owner_of` takes `&'a ViewSpec` and returns `Option<&'a str>`, so
  `apply`'s per-view loop resolves ownership per (column, view) pair
  rather than per dataset: a dataset that declares a column but says
  nothing about it still OWNS it there, and a later join's table for the
  same name must not stand in.
- **`apply` runs BELOW the desk views' own `format.colour` cross-check**
  (the final whole-branch review's Important 1, fixed before merge). The
  first cut ran it directly after the overlay was read, above that
  loop — and the loop reads `view.presentation_of(column.name())`, so
  with the dataset level merged over the desk's keys it reported the
  DATASET's colour once per view carrying the column, each at a
  `views.<v>.columns.<i>.format.colour` path into a file holding no
  colour key at all (one mistake, N+1 diagnostics, and `Draft::row_for_path`
  paints the glyph on an innocent view's column row), while a desk
  view's own broken `format.colour` under a VALID dataset-level colour
  went unreported entirely — a regression to an existing check. The
  `apply` call now sits immediately above the `view_presentation` block.
  Resolution order is untouched (the loop mutates nothing), so the merge
  is still kind default → desk → dataset → view.
  `load_views_merges_the_dataset_overlay_under_the_view_overlay_and_reports_its_colours`
  asserts the dataset path is the ONLY colour diagnostic, and
  `the_desk_views_own_colour_check_reads_the_desk_value` is case 2. The
  fixture that let both halves through used `find`, which cannot see a
  spurious extra.
- **A key inside `[<ds>.columns.<col>]` that the reader does not
  recognise now warns** — `column '<col>': unknown key '<key>' —
  ignored` at `dataset_presentation.<ds>.columns.<col>.<key>`, §2.2's
  requirement (the same review's Minor 2). `parse_format_keys`/
  `parse_column_keys` report only keys they recognise and mis-read, so a
  typo (`precison`) was accepted in silence. `color` is
  `parse_format_keys`' own American alias and does NOT warn; `hidden`
  keeps its own view_presentation-naming message.
  `ViewPresentationSpec::from_doc` still has the gap — deliberately out
  of this branch's scope.
- One fixture in the task brief's own test spelled a scale `"units"`,
  which the reader does not accept, so the view-level override it was
  meant to establish never existed and the assertion passed for no
  reason. Corrected to `"none"` — the same class of defect as the
  `"m"`/`"M"` spelling §1.3's amendment records.

### 9.2 Task 2 — the reload predicate

`hot_reload::views_changed` gained `|| changed("dataset_presentation")`,
with the comment saying why it rides there (`load_views` merges the
overlay into every `ViewSpec` of that dataset). **No bridge code
changed**: `data_setup` already calls `load_views` and extends
`setup.diagnostics` with its diagnostics, so the new doc's reader and
cross-check diagnostics reach the startup report and the reload report
with nothing added — §6's "`data_setup` extends its startup diagnostics
with the new doc's" was already true. Two tests pin it:
`a_dataset_presentation_change_fires_config_reloaded` (shell) and
`data_setup_hands_out_views_with_the_dataset_level_merged_under_the_view_level`
(bridge, which pins Task 1's merge through the real hand-out path rather
than the reader alone).

### 9.3 Task 3 — the scaffold: destination, stage-aware gate, provenance, the context

`Destination::DatasetPresentation`; `Domain::writable(self, stage:
&Stage)`; `Provenance`, `ColumnDoor`, `ColumnLayers` (with
`below_view()`), `ColumnContext` and `Draft.column_ctx`; `FellTo`,
`Fold` and a `fold_column` that serves either door;
`views::column_fields(item, colours, dest)`; the new
`objectdialog/dataset_columns.rs`, holding `provenance_of` alone at this
point.

- **`apply::object_value` answers `ObjectWrite::Remove` for
  `Presentation | DatasetPresentation`** — one arm, because the two
  destinations mean the same thing by an emptied rendering: an
  overlay's ABSENCE is "I have no personalisation of this object". Part
  2a's Major (removing a key from a domain's OWN doc means "inherit the
  layer beneath") is untouched, `Destination::Doc` keeping its own arm.
  Two exhaustive matches the compiler named with the new variant: the
  dest badge's `dest_label` in `render.rs` (`"dataset"`) and
  `views::to_table` (an `unreachable!` — no Views field carries it).
- **`Field` gained NO `provenance` field** (the preflight ruling, §9.6).
- **`fold_into` measures a clear against the ITEM's pre-fold value**
  (the ruling in §9.6, and §5.2's amendment). The brief's literal rule —
  name the key cleared whenever the rendering is empty, regardless of
  the baseline — was caught by the existing suite within the task:
  `column_fields` seeds `width` to `auto` whenever the item has no
  width, so every fold of such a column reported `width` cleared, and
  because the `width` arm runs after the `label` arm it overwrote a
  genuinely cleared label's notice.
- **`provenance_of`'s unknown-key fallthrough is `_ => false`, not the
  brief's `_ => return None`** — inside a `|p| match key { .. }` closure
  a `return` exits the *closure*, so its type infers as `Option<_>` and
  every `bool` use site fails to compile. Behaviourally identical:
  `differs_from` falls through to `false` too, so both doors answer
  `None` for a key this stage does not paint.
- The three `writable()` call-site shapes are worth knowing: `state` in
  scope → `state.domain.writable(&state.stage)` (six sites);
  `edit_commit_notice` clones the stage out first; and the dest-badge
  site computes one `dest_badges` bool **beside** `let domain =
  state.domain`, outside the row loop, because the per-row `.then(..)`
  closure would otherwise recompute a constant per row per frame.
- `views.rs` widened ten helpers to `pub(super)` (`negative_key`,
  `scale_key`, `colour_key`, `width_value`, `scale_from_key`,
  `negative_from_key`, `colour_from_key`, `schema_role_kind`, `AUTO`,
  plus `text_row`/`choice_row`) so the second door can build the same
  seven rows from the same spellings rather than a second copy of them.
- Views' pure column-stage tests install a real context through a shared
  `views::tests::open_column` helper, because under the new contract a
  draft with no `column_ctx` folds nothing — a test calling
  `enter_column` directly would have opened a stage whose fold is a
  no-op, which is not the stage the dialog opens.

### 9.4 Task 4 — the Schema door

`dataset_columns.rs` grew `DOC`, `overlay_object`, `item_for`, `table`,
`row_summary` and `ProvenanceInputs`; `schema::fields` appends the row
summary and `schema::to_table` branches on the destination;
`render::enter_column_stage` gained its Schema arm,
`render::column_stage_target` was extracted, and `leave_column_stage`
re-derives the Schema rows through a new `Draft::reseed_fields`.

- **`toml_edit::Table::to_string()` prints LEAF values only.** It
  iterates `get_values()`, which recurses into *dotted* tables alone, so
  a writer whose whole output is sub-tables renders the empty string
  through `Display`. Both this task's and Task 5's pure writer tests
  therefore assert through `super::super::object_text(name, item)` —
  this crate's one spelling of "what a write produces", and what
  `apply::object_value` parses back — rather than through a `Display`
  impl that could never show the headers being asserted on.
- **`Draft::select_item_named` gained a `columns.<col>` field
  fallback**, beyond the brief: it only searched ordered lists, so
  `escape` out of column 25 of a 30-column dataset dumped the trader at
  the top of the list. The fallback is `enter_column`'s own membership
  rule read backwards, and no other domain can reach it (a view's list
  field is keyed `columns`, never `columns.<something>`).
- **`the_schema_inspector_lists_datasets_and_refuses_every_verb` lost
  `enter` from its refusal loop**, deliberately: `enter` on a column row
  is now this dialog's one door. §1.3 item 5 names exactly which verbs
  still answer the read-only notice (`d`, `r`, `n`, ticks, drops), and
  that test presses `d` again after `escape`, so the dialog's own rows
  are still proved read-only after a round trip through the stage.
- **`clicking_an_edit_row_while_filtering_keeps_the_filter_focused` was
  retargeted from a member row to a field row** rather than weakening
  the click parity (the ruling in §9.6). A member row is now a door and
  a door leaves filter mode, so that row can no longer carry a test
  about the mode surviving a click; a field row opens nothing, which is
  the property under test, and the cursor is stepped onto `npv` first so
  the click is still a real move.
- `item_for` drops the reader's diagnostics through a no-op warn sink,
  on purpose: the file's own diagnostics are already reported with their
  paths by `DatasetPresentationSpec::from_doc` at load, and re-reporting
  them from inside a keystroke would put a file-level warning on a
  one-column stage. Its doc says so.
- Fix round 1 folded in Task 3's review minors: `apply.rs`'s three doc
  comments naming `DatasetPresentation` as an overlay destination;
  `ProvenanceInputs` hoisted out of the row loop and its `below` made
  lazy per door; `fold_column` copying `door` and cloning `layers` only;
  the `debug_assert!` that a column stage always carries its door's
  context; `provenance_of` trimming both sides of a label comparison;
  and `leave_column_stage` building `config_with_pending` for Schema
  alone.

### 9.5 Task 5 — the Views stage with a layer under it

`views::dataset_layer`, `dataset_layer_for`, `baseline_below`,
`column_layers` and `column_context`; `Draft.dataset_layer`;
`presentation_table` reading `baseline_below`;
`render::enter_column_stage`'s Views arm refreshing the layer from the
pending-aware config.

- **`views::column_context` is the one builder of the Views door's
  context**, shared by the dialog and by the two test openers that
  mirror it — a context is exactly the kind of value where a test that
  drifts from the door stops testing the door: a missing layer there is
  a fold that names the wrong one with every assertion still green. One
  literal deliberately survives, in
  `a_cleared_view_key_falls_to_the_dataset_level_before_the_desk`, whose
  whole subject is the fold's answer over three *synthetic* layer
  combinations supplied as the parameter under test.
- **A column promoted in the same draft lifetime was the review's
  Important**, and fixing the catalogue's seed alone does not fix it —
  both halves landed (the ruling in §9.6 and §5.1's and §5.4's
  amendments): `dataset_catalogue` seeds from the dataset overlay, AND
  `dataset_layer`'s candidates widened to every column the view's
  datasets declare, because a just-promoted column is not in
  `view.columns` on the keystroke that writes it.
- **`fold_into`'s parameter is `below`, not `desk`** (fix round 1), and
  its doc names `ColumnLayers::below_view` as the stage path's spelling
  of the same merge, so the fold and the writer are visibly measuring a
  clear against the same thing.
- **A joined-view fixture needs a grain.** A `ref` dataset declaring
  only `book` and `sector` as bare dimensions loses both to
  `validate_dataset` (Phase 4a's rule: a bare `dimension` naming no
  grain and not a built-in key column is an error and is dropped), so
  `owner_of` answered `None` and the column took nothing from the layer
  for the WRONG reason. The fixture gives `ref` a measure and `sector` a
  grain, which is the state a real joined dataset is in.
- **A window test cannot read a chip's text**: `cx.debug_bounds` carries
  an element id, not its content. The window test therefore asserts the
  chip pair that IS decidable by existence (`scale`, set at the dataset
  level, paints one; `width`, set nowhere, paints none) and asserts the
  three layers directly off the live `Draft::column_ctx`. Which layer
  each chip NAMES over those layers is `provenance_of`'s own pure test.
- The window test's file assertions were made non-vacuous in fix round
  1: the old form read the overlay with `unwrap_or_default()`, which
  cannot distinguish "the key was removed" from "the file was never
  written".

### 9.6 Rulings

- **`Field` gains NO `provenance` field** (preflight ruling). Provenance
  is computed by the painter from `Draft::column_ctx`
  (`dataset_columns::provenance_of`). **Reason:** a stored value goes
  stale one keystroke after a step, and the painter has everything it
  needs. **Cost if wrong:** none. §4.3 and §5.3 amended.
- **`atomic_depth("dataset_presentation") = Some(1)`**, over §2.1's
  "atomic_depth 3". **Reason:** the writer renders the whole
  `[<dataset>]` object — every personalised column of that dataset —
  exactly as `view_presentation` renders a whole view's object at depth
  1, so a later layer's dataset table must replace the earlier one
  whole. **Cost if wrong:** a user-layer dataset table could not
  half-merge over a desk one, which no layer but user writes anyway.
  §2.1 amended.
- **The dataset overlay's colour cross-check is hoisted to run
  unconditionally** after `colours` is bound, with a test that has a
  dataset overlay and NO view overlay. **Reason:** §2.3 is the authority
  over the brief's placement. **Cost if wrong:** none.
- **`fold_into` names a key as CLEARED by comparing the field against
  the ITEM's pre-fold value**, not "regardless of the baseline" as the
  brief said. **Reason:** the brief's version flagged `width` on every
  fold of a width-less column and overwrote a real label clear; the
  item-relative rule is what "this keystroke emptied it" means and keeps
  the at-most-one-key-per-fold invariant. **Cost if wrong:** a notice
  missed on an exotic double-clear, nothing written wrong. §5.2 amended.
- **A member-row click opens its column stage and leaves filter mode,
  exactly as `enter` does** (§4.1 parity); the filter-focus click test
  was retargeted to a field row rather than weakening the parity.
  **Reason:** one rule about which rows are doors, through one
  `column_stage_target`. **Cost if wrong:** a trader clicking a member
  row mid-filter loses the filter, which `enter` already did. §4.1
  amended; the 2c spec's §9.9 deferred minor is marked closed.
- **The column stage's door stays open for `key` and `attribute`
  columns, and the kind is fixed by TYPE** (a `utf8` key seeds TEXT).
  **Reason:** label and width are what a trader wants on a key column,
  and "how is this column formatted" is a question about its values.
  **Cost if wrong:** a numeric attribute keeps measure defaults, which
  is what it would show anyway. §4.3 amended.
- **The AVAILABLE block's items seed their presentation from the dataset
  overlay** (`dataset_catalogue` / `refresh_available`), so a promotion
  carries the dataset level with it and the stage, the writer and the
  member summary agree. **Reason:** the catalogue is the one place a
  promoted item is born. **Cost if wrong:** none; a merge at the door
  alone would leave the summary blank. §5.4 amended.

### 9.7 Deferred — review minors taken and recorded rather than fixed

**Task 1.** The "applied before the view overlay" harness mutation
removes the `apply` rather than reordering it — a truer order-swap
mutation if revisited. The final review's fix wave added the companion
entry that DOES reorder it (`load: the dataset overlay is applied after
the desk view's own colour cross-check`, duplicating the `apply` back
above the loop), so the position is now guarded from both sides.

**Final whole-branch review, fixed rather than deferred.** Important 1
(the merge point relative to the desk colour loop — §9.1), named risk 4
(the provenance chip's liveness — §5.3), Minor 2 (unknown keys inside a
column table — §9.1), Minor 3 (the `(Presentation, Schema)` arm's stated
reason, which was §9.7's own "a gate's doc and its domain's header"
pattern recurring one file over), Minor 4 (`render::maybe_refresh_available`
now reads `apply::config_with_pending`, like every other read of this
layer on this branch — covered by
`a_dataset_switch_inside_the_debounce_seeds_the_catalogue_from_the_pending_write`),
Minor 6 (`render::actions` returns nothing while `Draft::column()` is
`Some`, so neither door advertises a verb a column stage can only
refuse — `a_column_stage_offers_no_destructive_action`) and Minor 7
(§3.2's correction). **Still deferred: Minor 5**, the ~20 small
allocations per painted frame while a column stage is open
(`provenance_of`'s per-field `kind.clone().with(p)`, `colour_key`,
`width_text`, and `provenance_chip`'s `format!`ed id) — modal-bounded
and consistent with `build_edit`'s own per-row `format!`s; if the chip
ever moves onto a non-modal surface, `differs_from` should take the
resolved `ColumnFormat` once per call instead of once per key. And
`ViewPresentationSpec::from_doc` still accepts an unknown key inside a
column table in silence — the same gap Minor 2 closed for the dataset
overlay, left alone as out of this branch's scope.

**Task 3, all since closed** — recorded because each names a shape a
later task had to honour. `fold_into`'s new pre-fold-value guard had no
harness entry when the rule landed; Task 5 added it (`views: a clear is
what this keystroke emptied`) rather than anchoring one Task 5 would
immediately move. Five review minors were folded into Task 4 (the three
`apply.rs` doc comments, `ProvenanceInputs` hoisted out of the row loop,
`fold_column` copying `door` and cloning `layers` alone, the
`debug_assert!` on the context, and `provenance_of` trimming both sides
of a label compare), and `schema.rs`'s module doc and `to_table` comment
— which still claimed Schema never writes — were rewritten in the same
task that opened the door. **The pattern is worth knowing**: a gate's
doc and its domain's module header are two places the same claim lives,
and changing the gate alone leaves the header lying.

**Task 4, closed in its own fix round.** A `key`/`attribute` column
opening the stage with MEASURE defaults (ruled and fixed: `kind_for_type`);
`table()`'s silence about dropped sibling keys (documented);
`leave_column_stage` building `config_with_pending` for every domain
(now Schema alone); `ProvenanceInputs` computing `below_view` for the
Dataset door, which never reads it (now lazy). Still open by design:
`item_for` drops the reader's diagnostics (above), and `desk_baseline`
still walks the source's columns on every column-stage keystroke
(inherited from 2c; modal, bounded by typing speed).

**Task 5.** `fold_into`'s parameter was still named `desk` while
`fold_column` handed it `layers.below_view()` — **closed** in fix round
1 (renamed `below`, doc corrected, two anchors moved with it).
`Domain::draft` runs `load_views` twice for `Domain::Views`
— once inside `views::fields`, once inside `dataset_layer_for` — per
*stage open*, not per frame or per keystroke; the alternative is a
`fields` that returns two things, which every other adapter would have
to answer for. `views::column_layers` builds the whole `desk_baseline`
map to remove one key (`desk_baseline` is `pub(super)` with one file's
callers). `dataset_layer` now does work proportional to the view's
datasets' whole column set rather than the view's own columns — a sort
and a dedup over names, per draft build and per column-stage open; the
alternative (widening only on promotion) would put the rule in two
places and leave the narrow one to be found again. The joined-view
fixture asserts own-dataset-first *precedence* only through `npv` and
`sector`, `book` being declared by both datasets and personalised in
neither; `owner_of`'s own ordering is tested in `geode-core`.

### 9.8 Harness

**772 entries** (747 at the branch point, 21 added over the six tasks
and 4 more in the final review's fix wave, none removed);
`--anchors-only` reports 0 stale, 0 ambiguous. The fix wave's four:
`load: the dataset overlay is applied after the desk view's own colour
cross-check` (the companion to the position entry below — it duplicates
the `apply` back above the loop rather than removing it), `view: an
unknown key inside a dataset-presentation column table warns`,
`objectdialog: the refreshed catalogue reads the pending config` and
`objectdialog: a column stage offers no destructive action`. One more
was re-anchored there: `objectdialog: a diverged field's provenance is
the view level`, whose line lost its `|| set_in(&ctx.layers.view)`
clause. The 21:
three for the core merge, ownership and order plus one for the hoisted
colour cross-check (Task 1); one for the reload predicate (Task 2);
three for the stage-aware gate, provenance and the fold's layer (Task
3); seven for the Schema door plus one for the key column's type
fallback (Task 4); and three for the view writer's baseline, the owning
dataset and the clear's measure plus two for the catalogue seed and the
widened layer (Task 5).

Nine pre-existing entries were re-anchored where these tasks moved their
source lines: `load_views: an unknown colour in the overlay warns with
its path` (extended upward to its enclosing `for` line — the new
dataset-overlay check made the short anchor a substring match of two
sites), `objectdialog: Schema is not writable`, `objectdialog: a drop on
the schema inspector is refused`, `objectdialog: an edit-stage click
takes the keyboard off the filter`, `objectdialog: revalidate folds the
column stage first` (also **ambiguous** after `column_stage_target`
introduced a second `if draft.column().is_some() {` in the same file, so
its anchor now carries the following line), `views: a presentation save
copies the desk's widths into the user's file`, `views: the overlay
writer omits keys equal to the desk`, `views: the writer's baseline
carries the desk's own format keys`, and `views: clearing a desk-set key
restores the desk's value`. Three entries added earlier on this branch
were re-anchored later on it, by Task 4's and Task 5's own edits.

### 9.9 Display checks pending

No sandbox on this branch painted a window, as on every Phase 4c branch.
Unverified pixel-for-pixel:

- the **provenance chip** on a column-stage field — its text
  (`desk` / `dataset` / `view`), and that it sits in the slot the Schema
  rows' layer badge uses without the two ever colliding;
- the **Schema browse-stage row summary** (`f64 · measure · k · 0 dp ·
  delta` appended after the type/role text of a personalised column);
- the **crumb in the Schema column stage** (`risk › delta01`);
- the **available rows' summaries in the Views edit stage**, which now
  show dataset-level values as a consequence of the catalogue seed
  (§5.4's amendment).

Everything else in this section is verified against window-test
assertions, unit tests, the harness and the code directly.
