# Geode — Scopes Dialog: Editing, Creating and Duplicating Scopes

**Date:** 2026-09-19
**Status:** Approved in brainstorm; implementation plan to follow
**Amends:** `2026-09-08-geode-phase-4c-config-dialogs-design.md` §8.4
(recorded there as §23) — the ruling that "a scope's values are not
edited here" is reversed by user ruling 2026-09-19.
**Conforms to:** `docs/PHILOSOPHY.md`, the dialog interaction model
(`2026-09-08-geode-dialog-interaction-model-design.md`), the 4c
scaffold (`Draft`, `Stage`, `Destination`, instant apply, the fork
rule of 2026-09-14) and spec §3.4 of the Phase 4 frame features design
(the distinct-values request the picker uses).

## 1. Why

`Domain::Scopes` today is two read-only `Text` rows (`Selects`, `Text
filter`) and two verbs that both snapshot the frame: `n` saves the
frame's current scope under a name, `o` overwrites an existing scope
with it. A trader who wants a scope that is *not* what the frame
currently shows has to scope the frame first, then save — and cannot
open a saved scope and change one book without loading it, re-picking
and overwriting. The user's own words: "the edit scopes dialog should
let you actually edit (and create new) saved scopes by changing the
scoping criteria and fields. currently it just applies whatever the
app's scope is set as."

## 2. Rulings (2026-09-19 brainstorm)

1. **Dimension values come from the data**, through the same
   `ShellEvent::DistinctRequested` path the `mod+p` picker uses — never
   typed free text. Same vocabulary as the picker, never a typo.
2. **`n` creates an empty scope** (selects everything) and opens its
   edit stage; **`c` duplicates** the selected scope under a new name
   and opens it. `o` (overwrite from the frame) stays.
3. **Editing is a Values sub-stage inside the dialog** (approach A),
   not a re-targeted picker (B: the picker is frame-shaped end to end
   and the shell has one modal slot) and not a flat inline list (C:
   does not scale past a handful of dimensions).
4. **Reordering selections is refused.** Selections AND together;
   order means nothing and the dialog must not pretend otherwise.
5. **A saved value the data does not currently hold is shown, ticked,
   and marked** — never silently dropped by the next write.

## 3. The edit stage

`Domain::Scopes.fields` becomes three fields:

| key | kind | dest | notes |
|---|---|---|---|
| `dimensions` | `OrderedList { items, available: Some(..) }` | Doc | one item per selected dimension; `available` = every `pickable_columns` entry not already selected |
| `text` | `Text`, editable (`i`) | Doc | empty = none |
| `expression` | `Text`, editable (`i`) | Doc | empty = none; split out of today's `Selects` summary |

- An **item** row is labelled `<column>` with `views::column_summary`'s
  slot painting the values: the values joined `, ` up to three, then
  `<n> values`. `ListItem` gains nothing: the values live on
  `draft.source` (`dimensions.<column>`), and the summary is read from
  there through a Scopes-specific `row_summary`.
- The **available** block is `pickable_columns(config)` minus the
  selected columns, in the picker's own order, so the two surfaces
  agree on what is scopeable. `pickable_columns` is a pure function of
  the `Config` `Domain::fields` already receives, so the domain calls
  it directly — no new plumbing.
- `space`, `enter` or a click on an **available** row opens the Values
  stage for that column (§4). It does **not** add an empty selection:
  an empty `values` is "no constraint" and `scope_to_table` skips it,
  so there is nothing to add until the first tick.
- `enter` on an **item** row opens the Values stage for that column.
  `space` on an item names the door (`enter opens this dimension's
  values`) rather than opening it. **As built:** the tick IS painted on
  a Scopes dimension row — an item's `included` is `true`, an
  available row's `false`, the same flag every other domain's tick
  reads — and a click on it names the same door `space` does rather
  than toggling membership on the spot: `step_selected_row`'s Scopes
  arm sends an available row's tick into the Values stage and a
  selected row's tick to the same notice `space` gives, so the mouse
  and the key cannot disagree about which rows are doors
  (`render::values_stage_target`, read by `commit_selected_row`,
  `on_edit_row_clicked` and the tick/chip click alike).
- `x` on an item removes the selection (the dimension returns to the
  available block). A `Doc` write.
- `shift+j`/`shift+k` on an item are refused with the notice
  `selections have no order`, gated per domain the way `x`'s refusal
  already is (`Domain::reorderable()` → `false` for Scopes; every other
  domain `true`). The footer omits the reorder group on this domain.
- **Every mutation folds into `draft.source`**: a tick writes
  `source.dimensions.<column>`, a removal deletes it, `i` on `text` or
  `expression` writes the key. `to_table` keeps rendering `source`
  verbatim, `Draft::is_dirty`'s `source` vs `baseline_source`
  comparison is untouched, and the summary-collision test
  (`overwrite_with_is_seen_even_when_the_painted_summary_collides`)
  keeps its meaning. `overwrite_with` (`o`), `n` and `c` all rebuild
  `source` then `fields` from it, one function (`fields_from_table`,
  as today).
- Help (`Domain::help`): `dimensions` → "The selected dimensions —
  enter opens a column's values, x drops it"; `text` → "The saved text
  filter, matched against every textual column"; `expression` → "A
  filter expression over the scope's columns, checked when you apply
  it". The sweep `every_field_on_every_domain_has_help` covers them.

## 4. The Values stage

```rust
Stage::Values { object: String, column: String }
```

A projection over the same `Draft`, exactly as `Stage::Column` is:
`enter_values_stage` stashes the scope's fields as `parent_fields` and
installs ONE field, `values: FieldKind::OrderedList { items, available:
None }` — Groupings' "ticking IS membership" shape, each distinct value
one `ListItem` whose `included` is the tick and whose new `note`
carries the count (or `not in data`). **Amendment (plan, 2026-09-19):**
the brainstorm named `MultiChoice` plus a fourth `EditRow::Option`
variant here; the plan uses the list shape instead, because every
consumer (`rows`, `row_label`, `visible_rows`, the tick click, the
filter, the `ctrl+a`/`ctrl+x` walk) already exists for it and
`MultiChoice` has never had a row model. `Draft.values: Option<String>`
sits beside `Draft.column` so `revalidate` can fold the stage and
`step_selected`'s "keep at least one entry" guard can stand down there.
Crumb `<object> › <column>`, pill as the edit stage's.

**Entering** emits `ShellEvent::DistinctRequested(DistinctParams {
key: SCOPES_KEY, tag, column, scope, as_of })`:

- `SCOPES_KEY = QueryKey(u64::MAX - 3)`, reserved beside `PICKER_KEY`
  and `DIAGNOSTICS_KEY`, with the same never-collides-with-a-tile
  reasoning.
- `tag` from `ShellView::next_picker_tag` (the one monotonic counter;
  Phase 4b M5's reasoning applies unchanged).
- `scope` is the **draft's** scope with this column's own selection
  removed — read through `saved_scopes_from_doc` over the rendered
  draft (the same round trip `validate` makes), so a count answers
  "how many rows would this value leave within the scope I am
  authoring", not within the frame's.
- `as_of` is the frame's, read at the call site as the picker does.

**Delivery.** `ShellView::deliver_distinct` routes by `outcome.key`:
`PICKER_KEY` to the picker (today's body, unchanged), `SCOPES_KEY` to
the object dialog, which applies the same three guards (a dialog is
open, its stage is `Values` on `outcome.column`, `outcome.tag` is the
latest one it handed out) and drops anything else. The bridge's
synthetic `Err` outcome for a refused submit already echoes key, tag
and column, so the stage never sits on "loading…" forever; an `Err`
paints the failure text as the stage's one row, with `escape` the way
out.

**Rows**, once values arrive:

- One row per `(value, count)` in the outcome's own sorted order,
  ticked iff `value` is in `ticked`; the count painted muted at the
  right as the picker's values stage paints it.
- Then one row per value in the saved selection that the outcome does
  NOT contain, ticked, with the marker `not in data` in the count's
  slot (ruling 5). The trader can untick it; nothing else ever removes
  it.
- Before values arrive: one inert row, `loading…`.
- `/` filters through `Draft::query` (the edit stage's own one-way
  mirror; a Values-stage query does not leak back into the edit
  stage's, and vice versa). Filtered rows paint in row order, as the
  edit stage's do.

**Keys**: `space` (and a tick click) toggles the row; `ctrl+a` ticks
every row the filter currently shows; `ctrl+x` clears the selection —
both already reclaimed inside `GeodeModal` for the picker, now consumed
here through `normal_command`-adjacent handling in `handle_edit_key`'s
Values arm; `escape` returns to the edit stage with the cursor on the
dimension's item row (or, if the selection emptied, on the column's
available row). `enter` is inert on a value row (a MultiChoice has no
"commit" — every tick already applied).

**Every toggle is a `Step::Changed`** that folds into the parent's
`dimensions` item and `draft.source` (`fold_values`, the sibling of
`fold_column`): a first tick on a previously unselected column inserts
the item; an untick that empties the selection removes the item and
`source.dimensions.<column>` (an empty array is never written —
`scope_to_table`'s own rule). It then rides `revalidate` +
`commit_change`: the 250 ms debounced `Doc` write, the fork notice on
a desk- or builtin-owned scope, and the diagnostics glyph.

**Known gap (final review, 2026-09-19).** The Values stage's row list —
like the rest of the edit-stage scaffold — is not virtualised: every
visible row is a real element built and laid out on every frame,
whatever the list's length. For a low-cardinality dimension (a book,
a handful of underlyings) this is unmeasured noise; for a
high-cardinality one (an instrument ref, a position ref) it pays
per-frame churn proportional to the dimension's cardinality, on a
surface whose whole reason to exist is to browse exactly that kind of
list. The `mod+p` picker's own Values stage already solved this with
`uniform_list`, precisely because it faces the same cardinality. A
follow-up should either cap the painted rows here (as the picker's own
`PICKER_ROWS` cap does for its ranked keys) or move the object dialog's
edit-stage list onto `uniform_list` outright; either way, `docs/perf.md`
should carry a measurement once one lands, since none exists today.

## 5. Text and expression

`i` on `text` or `expression` opens the shared `Input` through the
existing `TextEntry` path (`Completions::None`).
`Domain::Scopes.parse_text`:

- `text` → trimmed as today.
- `expression` → `parse_expr(text)`; `Err(e)` refuses with the parser's
  own message (`expression: <e>`) and keeps the field open, exactly as
  a refused `Number` does. A broken expression never reaches disk —
  today `saved_scopes_from_doc` only warns and drops the whole scope at
  load, which is the worst of both: the file is written and the scope
  vanishes.
- An empty commit on either clears the key (`text = ""` /
  `expression = ""`, the spelling `scope_to_table` already writes).

A column name the expression uses that no dataset declares stays the
reader's *warning* on the row's glyph (`scopes.<n>.expression`), never
a refusal — the dataset may arrive later, and warnings do not block.

## 6. Create and duplicate

- **`n`** enters `Stage::Naming` as today; `enter` writes
  `{ dimensions = {}, text = "", expression = "" }` under the typed
  name through `apply::commit_create` (one `Doc` write at zero
  debounce) and opens the edit stage with the `new` badge. It no longer
  snapshots the frame.
- **`c`** on a browse row (normal mode, or the bar's button in either)
  enters `Naming` seeded: `ObjectDialogState.naming_seed:
  NameSeed::{Empty, CopyOf(String)}`, recorded by name when armed (the
  browse cursor is an index; the same reason `confirm_target` records
  a name). `enter` writes the source object's table verbatim (read
  through `apply::config_with_pending`, never `services.config` alone)
  under the new name and opens it. `check_object_name` refuses a
  collision exactly as `n` does. `c` is Scopes-only for now
  (`Domain::duplicable()`); the mechanism is generic and a later
  ruling can turn it on for Views.
- **`o`** is unchanged: it replaces the open scope with the frame's
  and rebuilds `source` + `fields` through `overwrite_with`.
- The naming label reads `New scope · name` for `n` and
  `Copy of <name> · name` for `c`; the browse list keeps ranking by the
  typed name while naming, so a near-collision stays visible.

**Amendment (2026-09-19, scope-save): two more doors onto the same
naming prompt, seeded from the frame rather than empty or copied.**
`NameSeed` gains a third variant, `FromFrame`, for what pre-2026-09-19
`n` used to do before this spec's own §6 changed `n` to create an empty
object — saving the frame's *current* scope under a new name is still a
thing a trader wants, just no longer through the browse list's `n`. Two
doors reach it instead: the palette action `scope::save_current`
(`objectdialog::render::open_save_scope`, dispatched from `input.rs`
*before* its `strip_prefix("scope::")` arm — that arm would otherwise
read `save_current` as a saved scope's name to load, which is also why
`Domain::Scopes.reserved_names()` now refuses a scope by that name) and
the scope bar's own `save` chip (`shell::toolbar`, painted only while
`ScopeBarModel::savable` — `!scope.is_empty()` — is true; the bar's `+`
chip beside it is unconditional and opens the dimension picker, the
mouse form of `mod+p`). Both open the Scopes dialog and, when the frame
has something to save, enter `Stage::Naming` with `naming_seed =
FromFrame` and the label `Save scope · name`; an empty frame scope opens
the dialog in browse instead, with the notice "the frame's scope is
empty — nothing to save" (`open_save_scope`'s own gate; the same
condition is re-checked in `create_from_name`'s `FromFrame` arm on
`enter`, since the scope can still empty out in between). `enter` there
runs `scopes::overwrite_with` against the pending-aware config exactly
as `o`'s confirmed overwrite does, then the same `enter_edit_stage` +
`commit_create` path `n`/`c` already use — so the new object is `is_new`
and carries the frame's dimensions, text and expression verbatim.
`escape` cancels through the existing `cancel_naming`, which already
resets any `naming_seed` to `Empty` without needing to know a third
variant exists.

## 7. Gates, fork, diagnostics

- `apply::blocking_diagnostic` is unaffected: `saved_scopes_from_doc`
  only warns, so no scope edit is blocked by it; the expression is
  gated at `parse_text` (§5) instead.
- The 2026-09-14 fork rule applies unchanged: a tick on a desk-owned
  scope copies it to the user layer at once and says so.
- `d`/`r` from browse and edit, and `Confirm::Overwrite` on a
  user-owned `o`, are untouched.

## 8. Testing

- **Pure (`Draft`, no window):** `fields` builds the three rows and
  the available block from `pickable_columns`; a tick inserts the item
  and `source.dimensions.<col>`; an untick to empty removes both;
  `x` removes; reorder is `Step::Refused` with the notice; `to_table`
  round-trips through `saved_scopes_from_doc`; `parse_text` refuses a
  bad expression and accepts a good one; `n`'s object is the empty
  table; `c` copies verbatim; `overwrite_with` still rebuilds both.
- **Window tests:** enter the Values stage and assert the emitted
  `DistinctRequested` (key, column, draft-minus-column scope);
  deliver a synthetic `DistinctOutcome` through `deliver_distinct` and
  assert the rows, the `not in data` row, a stale tag dropped, a wrong
  column dropped, a `PICKER_KEY` outcome leaving the dialog untouched
  and a `SCOPES_KEY` outcome leaving an open picker untouched; a tick
  reaches disk through the write door; `escape` lands the cursor on
  the item; `c` end to end.
- **Mutation harness:** one entry per behaviour above (routing by key,
  the stale-tag guard, the not-in-data row, the empty-array rule, the
  expression refusal, the reorder refusal, `c`'s verbatim copy).
- Display checks pending on a real window, as every dialog's are.

## 9. Docs

- 4c spec §8.4 amended by reference to this spec (a new §23 there).
- CLAUDE.md's Scopes sentences in the Part 2a paragraph rewritten.
- `docs/superpowers/plans/2026-09-19-scopes-dialog-editing.md` — the
  implementation plan, next.
