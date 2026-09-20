# Geode — Dividend Schedule Panel, Row Editing, and Choice with Typeahead

**Date:** 2026-09-19
**Status:** Approved in brainstorm; implementation plan to follow
**Governs:** two phases of one session. Phase 1 builds a shared
"choose one value from a list, with typeahead" mechanism and ports it
to the settings and object dialogs. Phase 2 builds the dividend
schedule panel — the second market-data document kind — and, on the
way, widens the document family to per-row dates and text, makes the
flat panel layout per-column typed, and gives the market-data draft
row insert and delete on both layouts (CVI included).
**Conforms to:** `docs/PHILOSOPHY.md`, the foundation design, the
modules roadmap (`2026-09-12-geode-modules-roadmap.md`, rulings 1, 6,
8, 9), the market-data documents design
(`2026-09-12-geode-market-data-documents-design.md`, amended in §7
below), the panel header design (`2026-09-14-…-panel-header-design.md`)
and the dialog interaction model (`2026-09-08-…-dialog-interaction-
model-design.md`, amended in §7).

## 1. Why

`docs/modules.md` sketches the dividend schedule as "tabular display
(T × 5) of dividend schedule and metadata (ex date, announced date,
pay date, amount, status)", fed by XML over Solace and uploaded back
to Sophis. The roadmap files it under "config and a generator each",
and `PanelSpec` has carried a flat `Columns::Values` layout since Part
3 for exactly this shape. Three things the sketch needs are not there:

- **Per-row dates and text.** A document `role = "value"` column must
  be `f64`/`i64` today; three of the five columns are dates and one is
  text.
- **Per-column types in the flat layout.** `PanelSpec` has one
  `value_type` and one `format` for every cell; a schedule's columns
  differ in type, format and editor.
- **Row insert and delete.** A schedule gains a newly announced
  dividend and loses a cancelled one. The user ruled (2026-09-19) that
  inserting and deleting rows is needed on the dividend panel **and on
  CVI** (a term slice), so it is a draft capability, not a
  dividend-only one.

And one thing surfaced while settling `status`: a closed set is
stepped through with `space`/`shift+space` everywhere in Geode, and
the user noted that stepping alone is too slow when the set is long —
the settings dialog's theme row (38 themes) being the standing example.
So a typeahead choice is built first, once, and used by the panel's
`status` cell, the object dialog's `Choice` rows and the settings
dialog's rows.

## 2. Rulings (2026-09-19 brainstorm)

1. **Rows can be inserted and deleted, on every panel.** A draft
   carries row edits beside cell and attribute edits; CVI's row is a
   term slice.
2. **A dividend row is identified by the feed's own id**, not by ex
   date: an index schedule can carry several dividends on one ex date,
   and pay date and status do not disambiguate either. The id is the
   row axis; ex date is a value. An inserted row's id is minted by the
   panel.
3. **`status` is a closed set**, stepped in place and also chosen from
   a typeahead list.
4. **Choice with typeahead is rolled in-house on the pattern the
   dialogs already use**, not gpui-component's `Select`/`Combobox`
   (each owns a key context, a query `Input`, focus transfer and a
   deferred popover layer — the set of things this codebase has had to
   reclaim or replace every time; see the `DataTable` `NoAction`
   binding, `init_reclaimed_keybindings`, the blur-then-drop rule and
   Geode's own modal in place of `Dialog`). The panel's underlying
   picker (`PickerRows`) is already that mechanism by hand.
5. **The dialog ports ship with Phase 1.** A shared primitive is only
   proven shared once a second surface uses it, and the settings theme
   row is the case that motivated it.
6. **A deleted document row stays painted, struck through**, until
   `:revert` or upload. A hidden deletion is unsent work with nothing
   on screen to explain it. (Offered as a decision point; accepted.)
7. **`role = "value"` widens to `date` and `utf8`** rather than a new
   per-row role: a value is "a per-row fact that is not identity", its
   `type` says what it is, and storage, selection and attribution treat
   every value alike. (`timestamp`/`bool` stay refused — `Column`/
   `Value` do not carry them.)

## 3. Phase 1 — choice with typeahead

### 3.1 The core: `geode_shell::choice::ChoiceList`

Pure, no gpui, in `listfilter`'s and `vimnav`'s mould.

```rust
pub struct ChoiceList {
    options: Vec<String>,   // declared order — the index `pick` answers
    query: String,
    highlight: usize,       // into the RANKED, painted list
    cap: usize,             // painted rows; 12, the picker's
}
```

- `rank(&self) -> Vec<Ranked>` through `listfilter::rank` over
  `options`; an empty query lists every option in declared order,
  truncated to `cap`.
- `set_query(text)` re-ranks and re-places the highlight by identity
  (`place`) — the picker's rule, so typing never silently moves the
  highlight onto a different option.
- `nav(NavCommand)` moves the highlight through `vimnav::apply`,
  clamped to the painted range so `enter` never picks a row the trader
  cannot see.
- `complete()` copies the highlighted option's text into `query`
  (`tab`).
- `pick(&self) -> Option<usize>` answers the highlighted option's index
  in the **declared** list, never the ranked index.
- `place(&mut self, value: &str)` re-ranks against the current query
  and puts the highlight on `value`'s row, row 0 if it is gone.

`geode_marketdata::popup::PickerRows` is re-implemented over
`ChoiceList` (behaviour unchanged; its tests move to the core). There
is then one ranking, one cap and one highlight rule for every
"choose one" surface.

### 3.2 Object dialog

`TextEntry.completions: bool` becomes
`TextEntry.completions: Completions::{None, Chain, Choice}`; the chain
field is `Chain`, a plain field `None`, and `Draft::chain_entry()` keeps
its meaning ("the open field is the chain field").

`i` on a `Choice` row — today `Step::Inert` — opens the shared `Input`
in the filter row's place with `Completions::Choice`:

- seeded **empty** (the current value is already painted on the
  highlighted row; a seed would have to be deleted before typing);
  pill `choose`; label `"{object} · {field label}"`, as text entry's.
- `Draft::visible_rows` answers the field's `ChoiceList::rank` for the
  `Choice` arm (the options as rows, as the chain field's candidates
  are), the highlight placed on the current value at open.
- `up`/`down` and the rest of `listfilter::nav_command`'s chords
  (`ctrl+p/n/u/d/b/f`, the page keys) move the highlight — `j`/`k` are
  letters typed into the field, not bindings; `tab` completes; a
  completion-row click is `tab` (§18.9's rule for the chain field).
- **`enter` picks the highlighted option**, not the typed text — a
  dropdown commits what is lit, exactly as the underlying picker does.
  It sets `Choice.current` and rides the same `revalidate` +
  `commit_change` path a `space` step takes, so a desk object forks and
  says so, an error diagnostic blocks, and the notice is the fork
  notice or nothing.
- `escape` cancels; the cursor returns to the row (`follow`, as a plain
  field does).
- Read-only domains refuse through the existing `writable()` gate at
  the `i` site (already one of the ten). A one-option `Choice` stays
  `Inert` for `i`, mirroring `step_selected`'s own guard.
- The footer's `i` chip appears on a `Choice` row
  (`Draft::selected_vocabulary` gains the arm); `Domain::help` needs no
  change — the help line describes the field, not the keys.

### 3.3 Settings dialog

Every settings row is a value list (theme, font size, find style, add
direction, line numbers). In normal mode, `i` or `enter` on the
selected row opens the shared `Input` as a choice field over that row's
`values`, the rows below becoming the ranked options; `settings_view::
route` gains the entry as a pure table row; `enter` applies the picked
value through the row's existing `set_*_on` core (a theme applies live,
as stepping does) and persists as stepping persists; `escape` cancels.
The mouse form: a row click while the field is open is `tab`.

`enter` was inert in the settings dialog by the interaction-model
spec's §18 ruling ("enter inert in both modes"). It now opens the
choice field on every row; §7 records the amendment.

### 3.4 Not built here

No floating dropdown chrome in the dialogs: the row list *is* the
dropdown on these surfaces, and a floating layer inside `GeodeModal`
would need its own occlusion and click-out rules. The panel's `status`
cell (Phase 2) opens a `ChoiceList` in the existing `Popup` slot
anchored at the cell.

**As built (2026-09-19):** Tasks 1–5 of
`docs/superpowers/plans/2026-09-19-choice-with-typeahead.md`; the
object dialog paints the options through `dialog::choice_rows` in
place of the row list rather than through `EditRow` rows (no new
`EditRow` variant), and the settings dialog's `enter` opens the field
in normal mode only. The cap is a WINDOW that follows the highlight
(`ChoiceList::follow`), not a truncation of the ranked list — a
controller ruling landing after the tasks above were written, fixing a
trap the first build shipped: `i` then `enter` on the settings Theme
row, whose active theme usually ranks past row 12, opened lit on the
first theme and silently switched to it. The first sweep after Task 4
found one pre-existing harness entry (`objectdialog: the i button is
offered per row, not per domain`) no longer discriminating once a
`Choice` row legitimately offers `i`; it now discriminates on a
read-only `Text` row (794698b).

**As-built correction (final whole-branch review):** §3.2's bullet
above named `j`/`k` beside `up`/`down` as choice-field motions; they
are not — `crate::choice::route` dispatches nav keys through
`listfilter::nav_command`, whose table has no `j`/`k` entry at all
(they are letters the field types), only `up`/`down` and the chord set.
The bullet is corrected in place rather than left to stand beside this
note.

**Amendment — the dialogs' choice list scrolls (user report 2026-09-19,
"I'd expect to be able to scroll them with the mouse wheel — the
command palette already behaves this way"):** in the object dialog and
the settings dialog, `dialog::choice_rows` no longer paints the
twelve-row window; it paints EVERY ranked option inside a viewport
`min(ranked, 12)` rows tall with `overflow_y_scroll` on the dialog's own
scroll handle (`object_dialog_scroll` / `settings_scroll`, idle while
the row list is withdrawn), so the wheel scrolls it as the palette and
every dialog row list scroll. The highlight is compared in ranked space
(`ChoiceList::ranked_highlighted`; a row click hands back a ranked
index through `set_ranked_highlighted`), and every key path that can
move it — nav, `tab`, a keystroke's re-rank in the `Change`
subscription — calls `scroll_to_item` on it, the palette's
`sync_palette_scroll` rule. Rows are `flex_shrink_0`: a fixed-height
row inside a fixed-height column otherwise shrinks to its text, which
is how the first cut fitted forty rows into the viewport. Row colours
come through `listrow::row_paint`. `ChoiceList`'s window and §3.1's
cap semantics are unchanged and still what the market-data picker
popup paints, so `painted()`/`highlighted()` keep their window-relative
meaning there. §3.4's "no floating dropdown chrome" stands: the list is
still in the row list's place.

## 4. Phase 2 — the document family and the typed flat panel

### 4.1 `role = "value"` accepts `date` and `utf8`

`validate_document`'s rule becomes "a value must be f64, i64, date or
utf8" (error at `datasets.{ds}.columns.{col}.type` otherwise, column
dropped, as today). Nothing below changes: `document::Column`/`Value`
already carry `Utf8`/`Date`; `publish_document` stages by the `Cell`
plan from `document_columns()`; `compile_document` selects every column
and marks a value `DeterminedNonAdditive` whatever its type; the picker
offers dimensions only.

The pivot's requirement that its one cell column be numeric moves from
the schema to `MatrixModel::build`'s `Columns::Axis` arm, which already
refuses "more than one value column" and now also refuses a non-numeric
one, naming the column. The CVI slice-value disagreement check (through
`f64_at`) is untouched.

### 4.2 `PanelSpec` declares flat columns explicitly

```rust
pub struct ValueColumn {
    pub column: &'static str,
    pub label: &'static str,       // must not collide with another column's
    pub ty: ColumnType,            // F64 | I64 | Date | Utf8
    pub format: ColumnFormat,      // numbers only; ignored otherwise
    pub choices: Option<&'static [&'static str]>,  // Utf8 closed set
    pub required: bool,            // an inserted row must fill it (§5.2)
}
pub enum Columns {
    Axis(&'static str),
    Values(&'static [ValueColumn]),   // paint order
}
pub enum RowIdentity { Typed(ColumnType), Minted }
pub struct RowAxis { pub column: &'static str, pub identity: RowIdentity }
```

`PanelSpec.rows: RowAxis`; `value_type`/`format` stay the pivot's.
A flat panel names its columns as `header` names its attributes: a
document value the spec does not list is **refused** by the build ("the
document carries a value column '{c}' this panel does not declare") —
a schema drifting under a spec is reported, never painted as an
unlabelled column. `names()` covers `Values` and `RowAxis`. A
`HeaderAttr.ty` may now be `Utf8` (a `currency` chip): `parse_attr`
gains the arm and the header editor treats it as text.

### 4.3 The model's cells are typed

`MatrixModel`'s cell carries `Option<Value>` beside its prepared text
(today `Option<f64>`). `flatten` fills each column through the
`ValueColumn.ty`'s reader (`f64_at`/`display_at`) and prepares each
column's text once per build with that column's own kind: a date paints
ISO, a choice paints its word, a number through `ColumnFormat`.

`MatrixModel.column_kinds: Vec<CellKind>` runs parallel to `columns`:

```rust
pub enum CellKind {
    Number(ColumnFormat),
    Date,
    Text,
    Choice(&'static [&'static str]),
}
```

The pivot fills it with `Number(spec.format)` and the slice values with
theirs, so the delegate, the editor door and the nudge read one answer
per column on both layouts.

`Draft::edits` becomes `BTreeMap<(usize, usize), Value>` (CVI writes
`Value::F64`). `to_toml`/`from_toml` spell a `Value` as a TOML float,
integer, or a string with a `type` tag (`{ type = "date", value =
"2026-12-18" }`, `{ type = "text", value = "declared" }`) so a restored
draft cannot mistake a date for text. `Draft::bump` and the arrow nudge
act on `Number` cells only and say so on any other ("not a numeric
cell").

### 4.4 Editing by cell kind

`begin_edit` on a grid cell dispatches on `column_kinds[col]`:

- `Number` → the text `Input` + `parse_cell(ty)`, as today.
- `Date` → the segmented date field (`EditorState::Date`, today opened
  only in the attribute strip), painted in the cell.
- `Text` → the text `Input`, committed verbatim (trimmed; an empty
  commit is refused for a `required` column).
- `Choice` → `Popup::Choice(ChoiceList)` anchored at the cell, the
  field focused (insert mode, the picker's contract); `enter` writes
  the picked option; `escape` closes; a click elsewhere closes as it
  closes the picker. In normal mode `space`/`shift+space` step a
  `Choice` cell in place — `marketdata::step`/`step_back`, new fragment
  verbs, refused on any other kind ("not a choice cell") and while
  `Behind`.

All four commit through the one `commit_edit` door into `Draft::set`
with a `Value`. The double-click rule (header spec §8.8.6) applies to
every kind. `close_editor`/`close_popup_with_window` blur only their
own field, as today.

### 4.5 Patching, not rebuilding

A cell commit calls `MatrixModel::patch_cell(row, col, &draft)` — re-
prepares that one cell's text and state, and the header's dirty count
— instead of `rebuild_model`. Row insert/delete and a delivery still
rebuild. This closes the note the perf doc parks against slice 2
(8.18 ms per keystroke at 10,000 × 5). CVI takes the same door: one
rule, benched at both shapes, with a test asserting `patch_cell` ≡
`build` on the same draft.

## 5. Row insert and delete (both layouts)

### 5.1 Draft

```rust
pub enum RowEdit {
    Inserted { after: Option<String>, cells: BTreeMap<String, Value> },
    Deleted,
}
// Draft.rows: BTreeMap<String /* row label */, RowEdit>
```

One `base`, one `state`, one `len()`, one `revert`. `count_phrase()`
reads "2 cells, 1 row added, 1 row removed" (each part only when non-
zero). `after` is the label of the document row the inserted row sits
under, `None` = top. Cell edits on an inserted row live in
`RowEdit.cells` keyed by column label, not in `edits` — an inserted
row's grid index is the model's business.

`rebase` carries rows by label: a `Deleted` whose label the newer
document no longer has is dropped and named; an `Inserted` whose label
the newer document now **carries** is dropped and named as a conflict
(upstream got there first); an `after` anchor that vanished re-anchors
to the top and is named. Dropped pairs and rows share the one
`dropped_notice`.

### 5.2 Model

`MatrixModel::build` lays document rows out in document order, splices
each `Inserted` row after its anchor (top for `None`, in label order
among several under one anchor), and marks `Deleted` rows
`RowState::Deleted` rather than removing them (ruling 6).

`MatrixRow.state: RowState::{Document, Inserted, Deleted}`. The
delegate tints an inserted row's cells with the `success` family and
paints a deleted row struck through in `muted`, both floored through
`FlooredTones` as the header tones are, with a bundled-theme sweep.
A deleted row's cells refuse edits ("row is deleted — `:revert`
restores it"); an inserted row's empty cells paint `·`; the header's
dirty dot counts rows.

A row with a `required` cell still empty — every ladder cell and slice
value on CVI, `ex_date`/`amount`/`status` on the schedule — is counted
in the header (`1 row incomplete`) and, in Part 4, blocks upload.

### 5.3 Verbs

| key | action | on |
|---|---|---|
| `o` | `marketdata::insert_below` | cursor row |
| `shift+o` | `marketdata::insert_above` | cursor row |
| `d d` | `marketdata::delete_row` | cursor row |

All in the `marketdata && mode == normal` fragment, all refused while
`Behind` (`:bump`'s rule), while the model is empty, and from the
attribute strip. `delete_row` drops an `Inserted` row outright, turns
a document row `Deleted`, and says so on a `Deleted` one.

Insert on a **`Typed`** row axis (CVI's `term`, a `Date`) opens the
row-label editor first — the date field for a `Date` axis, the text
`Input` for `Utf8`, the numeric `Input` for a number — in a new blank
row at the insert position; `enter` with a label already present is
refused, naming the model's uniqueness invariant to the trader;
`escape` drops the blank row. Insert on a **`Minted`** axis mints
`new-<n>` (`n` the smallest unused, never reused within the draft) and
lands the cursor on the first cell in insert mode. `DividendKind::parse`
refuses an upstream id starting `new-`, so a minted id can never
collide with a real one. `yank_row` on an inserted row yanks its cells
as painted.

### 5.4 Session and policy

`to_toml` writes `[drafts.<key>.rows.<label>]` carrying `after` and
`cells`, or `deleted = true`; a restored row edit is parked as a cell
edit is until the first non-empty model resolves it. The update policy
applies unchanged: `rebase` carries rows through §5.1, `replace`
counts them in the disclosure phrase, `hold` goes `Behind`. Parking
per underlying carries rows in the same table.

## 6. The dividend kind, dataset, generator and bus

### 6.1 Dataset (`examples/demo-config/datasets.toml`)

```toml
[dividend_schedule]
family = "document"
key = ["underlying_ref"]
axes = ["dividend_id"]

[dividend_schedule.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true
[dividend_schedule.columns.dividend_id]
type = "utf8"
role = "axis"
[dividend_schedule.columns.ex_date]
type = "date"
role = "value"
[dividend_schedule.columns.announced_date]
type = "date"
role = "value"
[dividend_schedule.columns.pay_date]
type = "date"
role = "value"
[dividend_schedule.columns.amount]
type = "f64"
role = "value"
[dividend_schedule.columns.status]
type = "utf8"
role = "value"
[dividend_schedule.columns.currency]
type = "utf8"
role = "attribute"
[dividend_schedule.columns.schedule_date]
type = "date"
role = "attribute"
```

`currency` and `schedule_date` (the date the schedule was struck) are
assumptions until the XSD, as CVI's tag names are. `status`'s closed
set is **`estimated` · `declared` · `paid` · `cancelled`** — declared
once as `DividendKind::STATUSES` and referenced by the spec's
`choices`, so the kind's refusal and the panel's list cannot drift;
also an assumption, one table to change.

### 6.2 `DividendKind` (`crates/geode-documents/src/dividend.rs`)

`NAME = "dividend_schedule"`, the CVI mould: a `quick_xml` event walk
over `marketData/underlying`, `dividends/currency`,
`dividends/scheduleDate`, `dividends/dividend*` each carrying `id`,
`exDate`, `announcedDate`, `payDate`, `amount`, `status`. Parsed into
`DocumentRows` in `COLUMNS` order; unknown elements skipped and logged
once per path; a missing or repeated `id`, an unknown `status`, an id
beginning `new-`, or a malformed date refused with the element path.
`write` is the inverse and refuses rows whose status is not in the set.
Round-trip test. Tag names live in one table and are an assumption.
`builtin_kinds()` returns both kinds.

### 6.3 Generator (`geode_demo_data::documents::dividend::DividendGenerator`)

Seeded. Per underlying, a schedule over the next two years: `SPX`,
`NDX`, `RUT` get index-style schedules (30–40 rows, with two or three
same-ex-date pairs each, so the id axis is exercised); other names
quarterly regulars (8–12 rows) with an occasional special. Ids
`D<hash>-<n>`, stable per (underlying, ordinal) across republishes so
rebase-by-label is exercised for real. `status` weighted by ex date
(past → `paid`, near → `declared`, far → `estimated`, one in twenty
`cancelled`). Each republish walks one or two amounts, promotes one
`estimated` to `declared`, and occasionally appends a row (upstream
inserting under a draft — §5.1's conflict case). Rows emitted sorted by
ex date, then id. A `same_seed` determinism test as CVI's.

### 6.4 Demo bus

`demo_bus::spawn` takes producers rather than one `CviGenerator`:

```rust
pub struct Producer {
    pub kind: Arc<dyn DocumentKind>,
    pub topic_prefix: &'static str,          // "marketdata/cvi/"
    pub keys: Vec<String>,
    pub next: Box<dyn FnMut(&str) -> DocumentRows + Send>,
}
```

The loop publishes one key of one producer per cadence tick, round-
robin across producers, so the overall generation rate stays one per
~5 s and adding the second source does not double the archive growth
recorded in `docs/perf.md`. Every producer's every key is still
published once at start. The demo layer declares `[dividend]` beside
`[cvi]`: `adapter = "demo_bus"`, `dataset = "dividend_schedule"`,
`document = "dividend_schedule"`, `topics = ["marketdata/dividend/>"]`,
`coalesce = "500ms"`. The demo database must be deleted for the new
dataset (the standing rule in CLAUDE.md).

### 6.5 Spec and roster

```rust
pub const DIVIDEND: PanelSpec = PanelSpec {
    kind: "dividend", title: "Dividend",
    dataset: "dividend_schedule", document: "dividend_schedule",
    rows: RowAxis { column: "dividend_id", identity: RowIdentity::Minted },
    columns: Columns::Values(&[
        ValueColumn { column: "ex_date",        label: "ex",        ty: Date, required: true,  .. },
        ValueColumn { column: "announced_date", label: "announced", ty: Date, required: false, .. },
        ValueColumn { column: "pay_date",       label: "pay",       ty: Date, required: false, .. },
        ValueColumn { column: "amount",         label: "amount",    ty: F64,  required: true,
                      format: precision 4, no thousands, Colour::None, .. },
        ValueColumn { column: "status",         label: "status",    ty: Utf8, required: true,
                      choices: Some(DividendKind::STATUSES), .. },
    ]),
    header: &[ currency Utf8 "ccy", schedule_date Date "struck" ],
    slice_values: &[], actions: &[], ..
};
```

`main.rs` registers a second `MarketDataFactoryHandle` over `DIVIDEND`
beside `CVI`'s. The factory is already one-per-spec with
`contexts() = ["marketdata"]`, so "Dividend: Split" lands in the
palette and the fragment binds it with no keymap change. The
underlying picker, key, drafts-per-underlying, update policy, session
and header carry over untouched. The row-label column paints the id —
the honest identity for an index schedule; a minted row reads `new-1`.

### 6.6 As built (2026-09-19)

Tasks 1–12 of `docs/superpowers/plans/2026-09-19-dividend-schedule.md`.
§4–§6 above shipped as drafted, with three refinements the plan's own
review rounds settled and this note records as rulings:

- **§5.3's anchor rule is sharper than drafted.** The draft text said
  `o` "anchors on the cursor row" and `shift+o` "anchors on the row
  painted above it" without saying what happens to a follower already
  hanging off either row. The controller ruling (Task 7, refined by
  Task 8's review): an anchor may itself be an inserted row, and
  `Draft::rehang_followers`/`reanchor_row` are the one mechanism for
  moving a follower, used in both directions — `o` on a row re-hangs
  that row's EXISTING follower onto the new row before anchoring the
  new row on the cursor, so the new row lands IMMEDIATELY below the
  cursor rather than beside its earlier sibling in label order (where a
  later rename would re-sort the pair); `shift+o` on an inserted row
  takes that row's own anchor and re-anchors the row onto the new one,
  so a chain paints new-above-old; and `delete_row` on a dropped
  `Inserted` row hands its followers to ITS OWN anchor rather than to
  the top. Without this, `o` then `shift+o` under one anchor could not
  land `[D1, new-2, new-1, D2]` in that painted order (Task 7's own
  note to Task 8).
- **§6.3's status rule is applied at creation, not deferred to
  republish.** The drafted generator description left status
  ambiguous about when the past/near/far rule first applies; the
  review ruling (Task 10) is that `new_row` applies the full three-way
  rule (past → `paid`, within 30 days → `declared`, else `estimated`,
  one in twenty `cancelled`) at CREATION, so a fresh panel's first
  document is never dishonestly `estimated` on a near-dated row.
  `republish`'s own promotion step is narrowed to what creation's
  static rule cannot do — every THIRD republish promotes the single
  nearest-dated `estimated` row to `declared` — rather than "each
  republish promotes one estimated row" as §6.3 first said.
- **A restored or update-policy rebase runs against a clean model**,
  never the tile's own painted `self.model` — which already carries
  the draft's own spliced-in rows and would read each inserted row as
  a document row the newer generation "now carries" (a phantom
  conflict) and key a cell edit by its post-splice position rather
  than the document position `Draft::edits` holds. One extra
  `MatrixModel::build(snapshot, spec, &Draft::default())`, once per
  restore or rebase, never per delivery — not specified in §5.4, since
  the trap only surfaces once row edits exist to be misread this way.

`Draft::rebase`'s row handling from §5.1 shipped exactly as drafted
(a vanished anchor re-anchors to the top and is named; an `Inserted`
label the newer document now carries is dropped as a conflict), with
one added case the review found: a row anchored on ANOTHER surviving
inserted row must keep that anchor rather than being re-anchored to the
top on a spurious "dropped" report (Task 7's review).

The final whole-branch review (2026-09-20) added one behaviour and
three records of where the text above and the build differ:

- **`Draft::rename_row` carries the row's followers with it**
  (`rehang_followers(from → to)` after the move — the review's
  Critical). The tile anchors a chain on the MINTED label before the
  row-label editor's commit renames it (`o` re-hangs the cursor row's
  follower onto `new-2`, then `commit_row_label` renames `new-2` to the
  typed term), so without the rehang the follower kept a label no row
  held, `splice_rows`' `known` set could not resolve it, and a second
  `o` on the same CVI term painted the first typed row at the TOP of
  the grid with a dangling `after` in `session.toml` and a spurious
  `anchor 'new-2'` drop on the next rebase. Pinned at three levels:
  `rename_row_rehangs_its_followers`, the harness entry `draft:
  rename_row re-hangs followers`, and the window test
  `a_second_o_on_the_same_row_keeps_the_first_typed_row_below_it`.
- **`required` and the kind disagree, and the disagreement is a Part 4
  decision, not fixed here** (controller ruling). `DIVIDEND` marks
  `announced_date` and `pay_date` `required: false` — §6.5's own
  reasoning, that an `estimated` row does not have them yet — but
  `DividendKind` refuses a `<dividend>` missing either (`is missing
  announcedDate`/`payDate`) and `geode_core::document::Column::Date` is
  a `Vec<NaiveDate>` with no NULL, so a row the header counts complete
  is one the kind cannot write. The flags stay as they are; egress
  (Part 4) decides between the kind writing an empty element with the
  columns made nullable, or the flags becoming `required: true` — and
  until then `incomplete_rows` UNDER-COUNTS what egress will refuse.
  The sibling Part 4 item from the ledger sits beside it: `Draft::bump`
  lands `Value::F64` on an `I64` column (`current + delta`, untyped)
  where a commit lands `Value::I64`, so egress must coerce by the
  column's declared type rather than trust the value's tag.
- **§5.2's row tones are not both floored.** As built (`cell_paint`),
  an inserted row's text is bare `foreground` over the `success` tint
  at 18% — the bundled-theme sweep clears 3:1 on every theme with no
  floor applied — and a deleted row's text is `muted_foreground` on the
  bare ground, UNFLOORED, by the `Tone::Plain` rule (the theme author's
  own secondary-text pairing; nine bundled themes ship it under 3:1, a
  theme-authoring matter). `FlooredTones` is the header's, not the
  cells'.
- **§6.1's "declared once as `DividendKind::STATUSES`" is three
  declarations.** The crate-layering rule (only `geode-app` sees a
  document kind; `geode-demo-data` depends on `geode-core` alone, never
  `geode-documents`) forbids one shared table, so the vocabulary is spelled in
  `geode_documents::dividend::STATUSES`, `geode_marketdata::core::STATUSES`
  and `geode_demo_data::documents::dividend::STATUSES`, and two
  `geode-app` tests (`demo_bus.rs`) assert all three equal — one to
  change means three, and the tests say which.
- **The dividend row label is not displayed (user ruling 2026-09-20,
  "dividend_id shouldn't be displayed — keep it hidden").** `RowAxis`
  gained `label: RowLabel::{Shown, Hidden}` — `CVI` Shown, `DIVIDEND`
  Hidden. The label stays the row's IDENTITY everywhere §5 uses it
  (draft edits by label, anchors, `rebase`, the session, the minted
  `new-<n>`); only its COLUMN is withheld: `MatrixDelegate` reads one
  `label_column` flag and `model_col`/`table_col`/`columns_count`/
  `column` go through its offset (1 or 0) rather than a fixed
  `LABEL_COL + 1`, so under Hidden table column 0 is the first value
  column and takes the left pin the label column had; `/` searches each
  row's painted cell texts joined (a trader can only look for what they
  can see) and `yy` copies the cells alone. Hidden implies Minted — a
  `Typed` axis needs the column to type into — pinned by
  `a_hidden_row_label_is_minted_on_every_shipped_spec` over the shipped
  specs.

## 7. Amendments to earlier specs

- **Market-data documents design §3 / §6:** a document `value` may be
  `date` or `utf8` (ruling 7 above). §8.1: `Columns::Values` carries an
  explicit column list; `PanelSpec.rows` is a `RowAxis`.
- **Market-data documents design §8.7 / perf note:** the flat build is
  no longer a per-commit cost; `patch_cell` is the commit path on both
  layouts.
- **Dialog interaction model §18 (settings):** `enter` is no longer
  inert; with `i` it opens the choice field on the selected row.
- **Dialog interaction model §19 (footers):** the edit row's `i` chip
  is taught on a `Choice` row.
- **Panel header design §6/§7:** `PickerRows` is `ChoiceList`; the
  picker's keys and cap are unchanged.

## 8. Testing, performance, and what a maintainer must know

**Tests, in the foundation spec's weighting (data ≫ shell ≫ modules):**

- `geode-core`: `validate_document` accepts `date`/`utf8` values and
  still refuses `timestamp`/`bool`; `DocumentRows::validate` on a
  dividend document.
- `geode-documents`: parse, write, round-trip; every refusal (missing
  id, repeated id, unknown status, `new-` id, malformed date, unknown
  element logged once per path).
- `geode-demo-data`: `same_seed` determinism; same-day pairs present
  for the index names; ids stable across republishes; an appended row
  appears.
- `geode-marketdata` core: typed cells per kind; `patch_cell` ≡
  `build`; `Draft` row insert/delete/revert/rebase (all three named
  drops) and `to_toml`/`from_toml` round trips including the `type`
  tag; `Minted` never reuses; `Typed` refuses a duplicate label;
  `step`/`step_back` on a choice cell; `bump`/nudge refused off
  `Number`; the pivot's numeric refusal; an undeclared flat value
  refused. Window tests: each editor kind in a grid cell, `o`/
  `shift+o`/`d d`, the choice popup's focus round trip (blur-then-
  drop), a click elsewhere closes it, `Behind` refusals.
- `geode-shell`: `ChoiceList` pure tests (migrated from `PickerRows`
  plus `pick` answering the declared index under a query); the object
  dialog's `i` on a `Choice` row on every writable domain, refused on
  Schema, `enter` picks the lit row and forks a desk object with the
  fork notice, `escape` returns the cursor; settings `i`/`enter`
  applying a theme live and persisting.
- Theme sweeps: inserted/deleted row tones readable on every bundled
  theme.

**Mutation harness:** an entry per new behaviour — `patch_cell` made a
no-op; the pivot's numeric refusal; the undeclared-column refusal; the
`new-` guard; the `Inserted`-conflict drop on rebase; `Deleted` rows
painted rather than removed; the bus's round-robin; `pick` answering
the ranked index. `--anchors-only` before every merge.

**Benches** (`cargo bench -p geode-marketdata`): `patch_cell` and
`build` at 20 × 30 and 10,000 × 5; `Draft::rebase` with 1,000 cells +
100 rows. Recorded in `docs/perf.md`'s market-data section. Target: a
commit at 10,000 rows well inside the 8 ms pure-UI budget.

**Docs:** CLAUDE.md paragraphs for Phase 1 and the dividend slice; the
roadmap's §6 status line; the amendments in §7.

**Sequencing:** Phase 1 is one branch (core → object dialog → settings
→ picker migration), reviewed and merged first. Phase 2 is a second
branch: family widening → spec/model/typed editing → row edits → kind
+ generator + bus → spec/registration → perf/docs.

**Display checks pending on a real window** (the implementation
sandbox cannot paint one): the choice field in both dialogs; the date
field and choice popup painted in a grid cell; inserted/deleted row
tones; the dividend panel at an index-sized schedule.

## 9. Not decided here

- The wire tags, `currency`/`schedule_date` and the `status` vocabulary
  wait for the desk's XSD (roadmap ruling 5).
- How an upload names a minted row to Sophis (Part 4, egress).
- Whether `announced_date`/`pay_date` become nullable on the wire or
  `required: true` on the panel, and the `F64`-on-`I64` bump coercion
  (Part 4, egress — §6.6 has both).
- Sorting the flat panel by a column (the feed's order is the order).
