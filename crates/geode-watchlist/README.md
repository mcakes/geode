# geode-watchlist

The Watchlist tile: one named list of underlyings (`watchlists.toml`) per
tile. The list model lives in `geode_core::watchlist`; the resolved
members arrive through `geode_shell::watchlist::WatchlistGlobal`, which the
app's bridge replaces on every change; this crate hosts the editor and
writes only through the frame's config door. The tile issues no data
request and never depends on `geode-data`.

## Module map

- `content.rs`: `WatchlistFactory` (kind `watchlist`; accepts no launch
  dimension; no data handle) and `WatchlistConfig`, the snapshot the app
  pushes on startup and on every reload (the schema, derived dimensions,
  saved scopes and named expressions a rule is validated against).
  `set_config` stores it and calls `config_changed` on every live tile;
  `set_refresh` installs the bridge's cache refresh, by list name, as the
  tile's `shift+r` route (the factory's hook is the tile's only route to
  the bridge). Also the tile's `TileContent` door (keys, commands, the
  link-group capability and emission: `emits`, `follows`, `emission`,
  `watch_emission`), `ACTIONS` (category `Watchlist`) and the
  `DEFAULT_KEYMAP` fragment (contexts `normal`, `visual`, `insert`, `menu`
  and `rules`). New, Clone, Rename, Delete and Revert have no default
  chord: the palette and the `⋯` menu reach them. Rules… is `r`.
- `core/session.rs`: the session table (`version`, `name`,
  `sort = [column, "asc" | "desc"]` over `name`, `origin` or `reference`,
  `cursor` holding a member name). An unreadable key is dropped with a
  notice and the rest kept.
- `core/history.rs`: `History`, the shown list's undo and redo stacks over
  `geode_core::watchlist::edit` entries and its optimistic pending object:
  every verb works over `current` (the pending object while one awaits
  its reload, else the snapshot's definition), so two quick edits compose;
  `reloaded` applies the in-flight chain rule (a reload equal to the
  pending object or carrying another surface's change drops it; one equal
  to the base or to an earlier in-flight object keeps the later edits);
  `refused` drops it and remembers the names a refused write would have
  changed, so an undo that skips them says `not saved` rather than
  `changed elsewhere`.
- `core/prompt.rs`: `Prompt` (what the field asks: `AddName`; a rule's
  `RuleDataset`, `RuleScope`, `RuleExpression`; a list's name for
  `NewName`, `CloneName { from }` and `Rename { from }`) and the typed
  answer's step: `submit` for a name (trimmed to add, or `type a name` for
  a blank); `submit_object` for a list's name over every list defined
  (`watchlist::validate_name`: an identifier, no reserved word, no clash
  ignoring case naming the other list; a rename's own name is left out of
  the clash check, so a case change is a rename, but the unchanged name is
  refused as `<name> is already its name`); `submit_rule` for a rule step
  over a `RuleContext` (the
  factory's schema, dimensions, saved scopes and named expressions, and
  the shown list as it is now): the dataset must be one
  `fold::eligible_datasets` names, the scope is `whole dataset`, a saved
  scope or `expression…`, and a scope or expression is folded over a
  one-rule list (`fold_rules`) before it is written, the fold's reason
  being the refusal. `scope_choices` lists the saved scopes that fold
  clean over a dataset.
- `core/rules.rs`: `RuleRow` (one per rule: its index, dataset, scope text
  `whole dataset` | `scope <name>` | the expression as written, and the
  snapshot's `rule_errors` reason by index) and `RulesPopup`, the popup's
  cursor, clamped to the rows.
- `core/rows.rs`: `WatchRow`, `members(state, pending)` and
  `rows(state, pending, reference)`, the members and the grid's rows: the
  snapshot's members, or, with a pending definition (an edit awaiting its
  reload), `resolve_members` re-run over the snapshot's rule names with
  the pending `include` and `exclude`, so a manual change shows at once
  and survives a snapshot that moved on under the same definition; the
  verbs read the same origins (`x` on a name added a moment ago sees it as
  manual). Each row carries the reference table's `name` cell
  (`underlyings`, `None` when absent or NULL), whether the table holds the
  key at all (`in_reference`), and `pending` when its manual state differs
  between the two definitions. `origin_text` spells the origin column:
  `manual`, `rule 1`, `rules 1, 3`, `manual + rule 2`, `excluded (rule 1)`,
  `excluded (manual)`, `excluded` for an exclusion nothing supplies, with
  ` · pending` while the row awaits its reload. Rules are numbered from 1.
- `core/grid.rs`: `GridModel` over `WatchRow`s: the sort order, the `/`
  filter (fuzzy over the name and the reference name, keeping the order),
  a cursor and a row selection held by name so a rebuild keeps them on
  their rows; `after_verb(rows)` keeps the cursor's shown index instead
  (the next row lands under it) and ends the selection; `counts()` is
  `(live, excluded)` over every row whatever the filter shows. The default
  order is by name with excluded rows last; `Origin` sorts by its text,
  `Reference` by the reference name with a missing one last either way,
  ties keep the default order. A restored cursor is a seed that waits for
  the snapshot holding its row.
- `tile/`: the hosted entity. It observes `WatchlistGlobal` (the lists),
  `ReferenceGlobal` (reference names), `AppClock` (the `as of` time), the
  frame (the shell's word on its config writes) and `Chords` (menu hints).
  `tile/header.rs` paints the header: the switch control
  (`Watchlist: <name> ▾`, also the switch chord, `g w` by default),
  `<n> names` (the grid's live count), `<k> rules` (warning tone while any
  rule is bad), the resolution state
  (`resolving…`; `as of <time>` on the display clock; `failed` in the
  warning tone after a failed resolution, with the last good time), the
  winning layer's badge, then the shared cluster with `⋯` and ×. The
  switcher hangs under the name and lists every list alphabetically, the
  shown one ticked. `tile/field.rs` is the prompt field: a bar under
  the header with the typeahead hung from it, its commit rule
  (`answer_value`: the highlighted name when the highlight was moved or
  typed out in full ignoring case, else the text as typed), its paint, and
  the tile's side of it (open, commit through `prompt::submit` and the
  add verb, close with a blur first, release where no window is at hand,
  the highlight keys and the row press). Its rows are one of two shapes:
  a `ChoiceList` (open for the add field, closed for a rule's dataset and
  scope, where the highlight is the answer whatever is typed; a list's
  name has no options and paints no list) or an `ExprCompletion` over the
  rule's dataset, whose rows are written into the field. A rename's field
  is seeded with the name, selected, so typing replaces it.
  `tile/rules.rs` is the rules popup: its paint (hung from the
  header's `<k> rules` item, or inline under the prompt bar while a rule
  step is open), its keys, the rule verbs and the rules write.
  `tile/verbs.rs` is the member
  verbs: what each acts on, the write gate (`queue_write`, one
  whole-object `ConfigEdit`), `commit` (queue, then hold pending, then
  say), `replay` (peek, gate, step) and `refresh`; `verbs_allowed` is the
  gate every member and rules verb passes (refused while the shown list's
  revert is on its way). `tile/objects.rs` is the watchlist's own verbs:
  New, Clone, Rename, Delete and Revert, their gates (`own`,
  `revertible`), `Pending` (what `y` does), `Awaiting` (a write shown
  ahead of its reload) and the tile's `ConfirmHost`. `tile/table.rs` is the
  grid's `TableDelegate` over rows
  the tile prepares (`Prepared::build`) whenever the snapshot, the
  reference tables, the shown list, the sort or the filter changes; render
  reads it only. It reports a row press, a right press and a header sort
  click as events the tile answers.

## Showing a list

A tile showing nothing (new, or its list removed by a snapshot change)
opens the switcher at once and says why in its empty state (`no watchlist
shown — <chord> switches`, or `<name> no longer exists — <chord> switches`,
naming the switch chord as the keymap binds it, `g w` by default, or the
palette title `Watchlist: Switch` when it binds none); with no list defined
the empty state names `Watchlist: New…` instead and the switcher refuses
with `no watchlists to switch to`. The switcher opens by
itself only on the first snapshot that holds lists (a restored tile is
built before the bridge publishes them) and when the shown list goes away;
a snapshot change that leaves a nothing-shown tile as it was keeps a
closed switcher closed.

## The grid

Columns: `name` (with a muted `not in reference` mark beside a name the
reference table does not hold), `reference` (the table's `name` cell,
blank when absent or NULL) and `origin` (muted). Excluded rows paint in
the muted text tone throughout. A list with no members paints the grid's
empty state (`No names`); a filter that keeps nothing says
`No matching names`.

Order: by name, excluded rows last. `:sort <name|origin|reference>
[asc|desc]` (a bare column is `asc`); `origin` sorts by its text as shown
(`rule 10` before `rule 2`; the ` · pending` suffix participates); a bare
`:sort` restores the default;
the header's sort control cycles desc → asc → default as every grid
tile's does (`SortOrder::click_cycle`), another column starting at desc.
The sort is the tile's and survives a switch; the session saves it.

`/` narrows over the name and the reference name as the query is typed,
keeping the order, with each word's match marked in its column; enter
keeps the filter, escape restores the one in force when the search began.
The counts in the header ignore the filter. A filter hiding the cursor's
row rests the cursor on the nearest shown row and returns it when the row
shows again; the session saves the trader's row meanwhile.

Cursor and selection: the shared grid motions (`j`/`k`, counts, `g g`,
`shift+g`, the page keys) move a cursor held by name, so a snapshot or
reference change keeps it on its row; a cursor nobody has moved keeps its
index. `v`/`shift+v` start a row selection at the cursor (a bare step then
clamps rather than wrap past the anchor); a sort, a filter, or a rebuild
that removes the cursor's row or the anchor's ends it. A row press moves
the cursor, shift-press extends a selection to it; a double-click is two
presses and nothing more. A right press moves the cursor there (a row of
a live selection keeps the selection) and opens the `⋯` menu hung from
the pointer. The session saves the cursor's name; a restored cursor waits
for the snapshot that holds its row.

The `⋯` menu (`.`) lists Add name, Remove name (disabled with `no row`
while no row is under the cursor), Rules…, Resolve now (each disabled with
`no watchlist shown` while none is), Undo and Redo (disabled with `nothing
to undo` / `nothing to redo` while the history has nothing that way), then
Switch…, then New…, Clone… (disabled with `no watchlist shown`), Rename…
and Delete… (disabled with why the list is not the user's to remove, see
Objects) and, only over a user copy with a lower copy beneath it, Revert…,
each with its live chord. A disabled row says why in its lane, and in
full when picked. While a revert is on its way the member verbs' rows and
Undo and Redo say `reverting <name>…`.

## Editing members

Every edit is the whole object written through the frame's config door
(`ConfigEdit { doc: watchlists, object: <name>, value: to_toml(next) }`;
the frame stamps the tile as its origin) and shown at once: the history
holds the next object as pending, `rows()` re-derives the grid over it (a
pending manual add paints `manual · pending`, a pending exclusion muted),
and the snapshot carrying the write drops the copy. A reload carrying the
tile's own earlier write keeps the later ones (the in-flight chain); a
reload carrying any other change is the truth and drops them; the shell's
`Refused` notice drops the pending object and replaces the verb's word.

`o`/`enter` (`watchlist::add`) opens the add field, a free typeahead over
the reference table's keys and every list's names, sorted; a name already
a member is not hidden but refused at commit, naming where it comes from
(`DAX is already here from rule 1`), with the field left open under the
reason. `up`/`down` move the highlight; `enter` commits (the highlighted
name when the highlight was moved or equals the typed text ignoring case,
else the text as typed); `escape`, a press on the grid or any other verb
closes it unwritten; the field is the keyboard owner (`insert` mode)
while open and is blurred before it is dropped. The verb says `added
<name>`, or `restored <name>` when the name was excluded.

`x` (`watchlist::remove`) acts on the selection, else the cursor's row,
by origin: a manual name leaves `include` (`removed <name>`), a
rule-supplied one is excluded (`excluded <name> — rule <i> still supplies
it; x again restores`), one that is both does both in one write (`removed
and excluded <name>`), an excluded one is restored (`restored <name>`); a
selection is counted (`removed 2 names, excluded 1 name`); with nothing to
change it says `nothing to remove`. The cursor keeps its shown index and a
selection ends.

`u`/`ctrl+r` replay the history one step over the current object (`undid
1 change`, `redid 2 changes`). A change another surface made since is
skipped (`— 1 changed elsewhere`); one left by the tile's own refused
write is `— 1 not saved`; a replay that skipped everything writes nothing.
Undo puts a name back by hand, so it lands at the end of `include`: the
object is restored, not the file's order.

## Rules

`r`, `Watchlist: Rules…`, the `⋯` menu's Rules… row and a press on the
header's `<k> rules` item (a control, like the switch beside it; the press
is `watchlist::rules`, so it passes the same gate and closes an open
popup as `r` does) open the rules popup, hung from that item: one row per
rule,
`rule <i> · <dataset> · <scope>` (`whole dataset`, `scope <name>`, or
the expression as written), with the fold's reason in the warning tone
beneath a rule the startup schema refuses (one over a dataset it lacks,
a saved scope it cannot honour); `no rules — o adds one` with none. The
tile is in `rules` mode while it is open and no field is: `j`/`k` (the
shared list steps) move the cursor, clamped; `o` (`watchlist::rule_add`)
adds a rule; `enter` (`watchlist::rule_edit`) edits the cursor rule's
scope; `x` (`watchlist::rule_remove`) removes it (`removed rule 2`; `no
rule under the cursor` with none); `escape` and `r` close it. A press on
a row moves the cursor there; a press outside the popup, a press on the
grid, or any verb that is not the popup's (a member verb, the refresh,
showing another list) closes it. The list going away closes it too.

A rule is asked in steps, each a closed choice in the prompt field with
the popup kept painted beneath it (the field owns the keys: `insert`
mode; `escape` closes the field first, then the popup): the dataset
(`fold::eligible_datasets`, in schema order; `o` is refused with `no
dataset carries underlying_ref` when there is none; `'<x>' is not a
dataset a rule may read` for any other answer), then the scope: `whole
dataset`, each saved scope that folds clean over that dataset, then
`expression…`, which opens the expression step. The expression field
completes over the rule's dataset alone (its columns and the derived
dimensions, then operators, values for bool and derived columns, and
connectives), the completion's hint or warning on a line under the
field; `up`/`down` move the highlight and `enter` then writes it over
the word at the caret (a row press does the same) with the field kept
open; `enter` with an unmoved highlight is the answer. Every answer is
folded over a one-rule list against the factory's schema, dimensions,
saved scopes and named expressions, and refused under the field with
the fold's reason (`scope references unknown column 'pair'`, `expression:
<what is wrong> at column <n>`), so a rule the popup lists is one the
data layer will run. Editing replaces the rule in place (`changed rule
2`); adding appends (`added rule 3`); the same rule again says `rules
unchanged` and writes nothing. A rules write is the whole object
through the config door like any member edit, shown at once (the popup
lists the pending rules; the header counts them; the snapshot's rule
errors apply only while the rules are the snapshot's) and undone whole
(`Change::Rules`).

`shift+r` (`watchlist::refresh`) calls the factory's refresh hook with
the shown name; the header reads `resolving…` with the next snapshot. A
tile hosted without the hook says `resolve now is not wired`. Showing
another list forgets the history and closes an open field; a snapshot
that removes the shown list does the same, and writes nothing.

## Objects

New…, Clone…, Rename…, Delete… and Revert… are registered actions with
no default chord (the palette and the `⋯` menu reach them), never `:`
commands. Each name is asked in the prompt field (`New watchlist`,
`clone <from> as`, `rename <from> to`), checked by
`prompt::submit_object` before anything is written, and refused under
the field with the field left open (`'Europe' already exists
('europe')`, `'and' is a reserved word in scope expressions`, `<name> is
already its name`); escape closes it unwritten. Rename, Delete and Revert
then ask y/n on the in-tile confirm bar (`geode_tile::confirm`) under the
header: `y` or the Yes button confirms; `n`, any other key, the No
button, a press anywhere else in the tile or focus leaving cancels
(`<name> not renamed` / `not deleted` / `not reverted`), and the key that
answered does nothing else. The bar holds the keyboard while armed (the
tile is in `insert` mode), so it sits above every other layer in the
escape order: `escape` on the bar answers no and stops there; a verb
reaching the tile from the palette answers no first and then acts, and
`Watchlist: Cancel` from the palette answers no and nothing more (the
field, popup, selection and notices beneath wait for the next escape).
Every write is a whole-object `ConfigEdit` through the frame's config
door, and the tile shows the outcome ahead of the reload that carries it
(`Awaiting`): the title and `saving <name>…` in the body for a list on its
way; a `Refused` notice from the shell puts back what was shown before.

- **New…** asks a name (with or without a list shown; the field survives
  a snapshot change) and writes an empty list under it
  (`to_toml(&Watchlist::default())`), then shows it.
- **Clone…** (refused with `no watchlist shown`) asks the name a copy of
  the shown list is written under and writes the list as shown, pending
  edits included (`history.current`), under the new name. The list cloned
  is never touched: no edit is queued for it, so a desk-layer list is not
  forked by its clone.
- **Rename…** asks the new name (seeded with the current one, selected),
  then `rename <from> → <to> — y renames`; `y` validates again and queues
  one batch of two edits, set `to` then remove `from`, so the shell writes
  both or neither. The object written is the shown one as the tile has it:
  an edit still on its way to the old name goes with the rename. No
  reference count is asked about: nothing names a watchlist yet, so there
  is nothing a rename would leave pointing at the old name. The switcher
  leaves the old name out until the reload drops it.
- **Delete…** asks `delete <name> — y deletes` and removes the user
  definition; the tile shows the empty state and opens the switcher
  without the deleted name.
- **Revert…** is offered only over a user copy with a lower copy beneath
  it (`WatchlistState.shadowed`; otherwise `<name> has no copy beneath
  yours to revert to`): `revert <name> to the <layer> copy — y reverts`
  removes the user definition and the name stays defined by the lower
  copy. The revert is awaited (`reverting`): until the snapshot no longer
  shows a user copy of the name, the member and rules verbs on that list
  (`o`, `x`, `u`, `ctrl+r`, the rules popup's verbs, their `⋯` rows) are
  refused with `reverting <name>…`, since an edit built on the user copy
  would replace the removal in the shell's batch; `shift+r` and `g w`
  are not gated. A `Refused` notice ends the wait.

Rename and Delete act only on a list the user layer owns outright
(`own`): a desk or builtin one is refused with `<name> is defined in
<layer> config; Geode cannot rename it` (`… delete it`), a user copy over
a lower layer's with `<name> shadows the <layer> copy — Revert… removes
it` (removing it would leave that copy standing under the old name), and
a list with no recorded layer with `can't tell where <name> is defined`.
The `⋯` rows carry the same refusals in their lane.

Notices, in order of precedence: the verb's own word (or the shell's
`TileNotice` about its write, taken on every frame notification: `Forked`
as status, `Refused` as danger), then the standing resolution notices from
the snapshot (`not resolved: <error> — shift+r retries` as danger after a
failed resolution; `rule <i> failed: <reason> — shift+r retries` as a
warning per bad rule), then the session-restore notices. A verb's notices
last until the next verb or another list is shown; the standing ones
until the next resolution changes them, and dismissed they hide until
their text changes; session-restore notices until the trader's first key
or press in the tile. `escape` (`watchlist::cancel`) peels one layer at a
time: an armed confirm bar first (it answers no and stops there), then an
open field (it owns the keys), then a menu (the surface on top, which may
be acting on the selection), then the rules popup, then a live selection,
then the warning and danger notices showing, each as a click on one would;
with nothing to dismiss it is unhandled.

## Link groups

The tile emits and never follows (`emits` is `true`, `follows` is
`false`: a list reads no scope). `emission` posts the name under the
cursor as a one-value `underlying_ref` path (`link::underlying_scope`), so
a market-data panel or a vol slice following the group shows that name;
only the cursor row is read, never the selection, and an excluded row is
still the name the cursor rests on. With no row under the cursor (nothing
shown, an empty list, a filter keeping nothing) it posts
`CursorScope::Nothing` and the group keeps its scope. The tile has no
`:filter` layer, no `:unscoped` flag and no board. `watch_emission`
observes the tile entity: every cursor move and row rebuild notifies it,
so the shell pulls once per change, and a pull that finds the emission
unchanged costs no write. The header paints the link chips from the frame
handle (`geode_tile::header::link_chips`) as every tile does.

## Performance

`benches/grid.rs` measures the rebuild one member edit costs on the UI
thread before table preparation and paint, over 5,000 names from three
rules with a hundred manual includes and fifty exclusions, an origin sort
and a `/` filter active: `rows::rows` with a pending definition, then
`GridModel::after_verb`. Reference (2026-10-10, loaded machine, load
average 35 to 48):

| Bench | Result |
|---|---|
| `watchlist_rebuild_after_edit_5k` | 4.0 ms |
| `watchlist_rows_and_grid_5k` (no pending definition) | 1.7 ms |

## Known limitations

- The add field's typeahead offers names only: the reference table's keys
  and the names any list already holds.
- The field's rows do not close on a press outside them (a press on the
  field itself is outside the list); a press on the grid, or any verb,
  closes the field.
- The rule expression's completion offers columns, operators and keywords
  only: a categorical column's values are not suggested, since the tile
  issues no distinct query (`mark_loading`/`deliver` are not wired); its
  hint reads `value for <column> · values not suggested here` at a value
  position for such a column.
- A rules write leaves the header at `as of <time>` until its reload
  lands: the resolution state comes from the snapshot, and the bridge marks
  the list `resolving…` only when the reload re-resolves it.
- In `rules` mode only the popup's own keys are bound: `u`, `ctrl+r`,
  `shift+r` and the member verbs act once the popup is closed (or from
  the palette, which closes it first).

The behavior as the trader sees it is in
[features](../../docs/current/features.md#watchlists).

## Commands

```sh
cargo test -p geode-watchlist
cargo bench -p geode-watchlist
```
