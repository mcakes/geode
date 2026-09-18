# Market-data panel: header, attributes and actions

**Date:** 2026-09-14
**Status:** Approved in brainstorm; amends §8.2–§8.5 of
`2026-09-12-geode-market-data-documents-design.md` (the "documents
spec" below). Part 4 (egress, documents spec §9) builds on this.

## 1. Why

Three user requests on the CVI panel, 2026-09-14: the header "could use
a facelift"; `anchor_date` and `spot_ref` should be editable; and the
panel needs buttons or a menu for its verbs — loading a different
document, uploading, and, later, per-kind requests such as *reanchor*
and *recalc forward*. The user also does not love the name "key" for
the concept of which document the panel shows.

Decisions taken in the brainstorm (each binding here):

1. The concept is **underlying**, in every place a trader reads it.
   "Underlying works in every case" — a CVI's underlying, a repo curve's,
   an index composition's. The data tier keeps `key` (it is the document
   family's own declared vocabulary, `key = [...]` in `datasets.toml`);
   the panel translates at its edge.
2. An attribute edit is **part of the same draft** as the cell edits:
   painted as edited, uploaded in the same document, dropped by the same
   revert, carried through `Behind` and `rebase` the same way.
3. The action surface is **one `⋯` menu button plus a key**, opening a
   Geode-owned, instant, anchored popup — not gpui-component's
   `DropdownMenu`, which dispatches gpui `Action`s (not Geode's action
   registry) and animates in (the same reasons the shell paints its own
   modals rather than the component's `Dialog`).
4. The keyboard reaches the attributes by **moving the cursor into the
   header strip** from the grid's top row — one vocabulary for cells and
   attributes, no new keys to learn.
5. Layout **B — one dense row** — over a two-row title/strip split and a
   stacked-block form; status made concise: a **dot** after the
   underlying marks a dirty draft (the editor convention for an unsaved
   buffer), and `Behind` reads `update HH:MM`, never "different document
   received HH:MM".

The mockups the user approved are in
`.superpowers/brainstorm/12813-1789440847/content/header-layout.html`
(layout B) and `header-states.html` (every state, dot marker).

## 2. Charter note on per-kind verbs

*Reanchor* and *recalc forward* are computations. Geode is a lens, not
a brain (PHILOSOPHY): when they are built they are **requests sent to
the upstream system through egress**, exactly as an upload is, and the
panel paints what comes back. This design reserves the slot (§6.3) and
designs nothing else about them; the user has said only that the idea
exists.

## 3. Vocabulary

| Where | Before | After |
|---|---|---|
| Header | `CVI SPX.Z` | badge `CVI` · bold `SPX.Z` |
| No document chosen | `CVI — no key — :key <value>` | `CVI` · muted `no underlying — load…` |
| Command line | `:key SPX.Z` | `:underlying SPX.Z` (`:key` a silent alias, one release) |
| Menu row | — | `Load underlying…` |
| Picker title | — | `underlying` |
| Session | `key = [..]` | `underlying = [..]` (`key` still read on restore, one release) |
| `PanelSpec`, `DocumentParams`, storage | `key` | unchanged — the data tier's word |

One doc comment on `MarketDataTile.key` says the two words are the same
thing at different layers, so nobody "fixes" one to match the other.

## 4. The header row

One row, `h(22px)` as today, in this order (mockup B):

1. **Kind badge** — `spec.title` in a small pill (`dialog::badge`'s
   shape, this crate's own paint: the marketdata crate does not depend
   on the blotter and the shell's badge is a dialog primitive).
2. **Underlying** — `display_key(key)` in `foreground`, bold; followed
   immediately by the **dirty dot** (an 8px filled circle in the floored
   `warning` from `FlooredTones`) whenever `draft` has any edit, cell or
   attribute. No dot on a clean draft. Nothing else says "edited" in the
   header: the tinted cells and attribute values say which.
3. **Attribute strip** — one `label value` pair per `spec.header` entry,
   in spec order, `label` in `muted_foreground`, `value` in the data
   face. The value is painted from the model's prepared text; an edited
   attribute takes the edited cell's own tint (`cell_paint`'s edited
   fill), and the cursor on an attribute is the same bordered box a
   cursor cell gets. The strip is inline, not a second row.
4. Flex spacer.
5. **State** — at most one short run in the floored `warning`:
   - `Behind { newer }` → `update HH:MM` (local clock, Phase 4a's rule);
   - key set, empty model → `no document yet`;
   - restored edits unresolved → `edits await a document`;
   - `Sent { at }` (Part 4) → `sent HH:MM` in the info tone, painted in
     this slot;
   - an error notice → the full sentence in the floored `danger`; it is
     the one state that keeps a sentence, because it must be readable.
   A notice and a state can coexist (an error while Behind); the notice
   is painted after the state.
6. **Time** — the generation's source time `HH:MM:SS`, `muted_foreground`;
   with ` stale` appended and the whole chip in the floored `warning`
   while `is_stale`.
7. **`⋯`** — a small bordered button; click opens the action list
   (§6). Painted pressed while the popup is open.

`Draft::summary() -> String` is replaced by `Draft::badge() ->
DraftBadge::{Clean, Dirty, Behind { newer }, Sent { at }}`; the header
paints from the badge, and the count ("3 edited cells, 1 attribute")
survives only where a number matters: the upload confirm (documents spec
§9.3) and `:revert`'s notice.

`PanelSpec.header` becomes `&'static [HeaderAttr { column: &'static
str, label: &'static str }]` — `("anchor_date", "anchor")`,
`("spot_ref", "spot")` for CVI. `names()` reads `.column`.

## 5. The attribute strip

### 5.1 Cursor

```rust
pub enum Cursor { Cell { row: usize, col: usize }, Attr(usize) }
```

Motion, all in `core::cursor` (pure, tested without a window):

- `k` on grid row 0 → `Attr(i)` where `i = min(col, attrs − 1)`; with
  no attributes (`spec.header` empty) `k` stays put.
- `j` from `Attr(_)` → `Cell { row: 0, col: last_grid_col }` where
  `last_grid_col` is the grid column the cursor left from (remembered
  on entering the strip; 0 if the cursor was restored into the strip).
- `h`/`l` in the strip step attributes, clamped; counts apply.
- `^`/`$` in the strip → first/last attribute. `gg`/`G`/`ctrl+d`/`ctrl+u`
  are grid verbs: from the strip they land on the grid (row 0 / last
  row / page) at `last_grid_col`.
- `y` in the strip yanks `value` alone; `yy` yanks `label\tvalue`;
  `yc` is inert with a notice.
- `/` (find) matches row and column labels as today and never the
  strip; `n`/`N` move within the grid.
- A **click** on an attribute value moves the cursor to `Attr(i)` and
  opens nothing (editing is keyboard-only, the 2026-09-14 ruling); a
  click on a cell from the strip moves into the grid as today.
- An empty model (no rows) keeps `Attr` reachable only if the model
  still carries the attribute values; with no document at all the
  cursor is `Cell { 0, 0 }` and every strip motion is inert.

`sync_cursor` clears the table's selection while the cursor is in the
strip (`set_selected_row(None)`-equivalent — the pinned component's
clear door is confirmed at implementation time) so the grid paints no
highlighted row behind an attribute edit.

### 5.2 Editing

`i`/`enter` on `Attr(i)` opens the tile-owned `InputState` in the
strip, in the value's place, seeded with the painted value — the cell
editor's own machinery, the same insert-mode contract (documents spec
§8.6), the same `close_editor` blur-then-drop. `enter` parses through
`core::draft::parse_attr(text, ty: ColumnType) -> Result<Value,
String>`: `Date` as `YYYY-MM-DD` (refused otherwise, naming the format),
`F64`/`I64` through the same rule `parse_cell` applies, `Utf8` trimmed
and non-empty. `Ok` → `Draft::set_attr(column, value, base)` and a
rebuild; `Err` → the notice, and the editor stays open with the typed
text (the cell rule). Refused with a notice while `Behind` (the cell
rule) and while the model is empty.

`:set <attr> <value>` is the typed door: the same parse, the same
refusals, attribute names as completions. `:set` with no value shows
the current value as the notice.

### 5.3 One draft

```rust
pub struct Draft {
    pub base: Option<String>,
    pub edits: BTreeMap<(usize, usize), f64>,
    pub attrs: BTreeMap<String, Value>,   // by column name
    pub state: DraftState,
    labels: BTreeMap<(usize, usize), (String, String)>,
}
```

- `len()` counts cells + attrs; `is_empty()` is both empty; `base` is
  `None` exactly when both are empty (the existing invariant, widened).
- `revert()` and `discard()` clear both.
- `rebase(model_of_newer)` keeps every attribute edit whose column the
  newer model declares (for a fixed kind, always; the generic rule costs
  one `contains` and never silently drops); dropped cell pairs are
  reported as today, dropped attributes by name in the same notice.
- `on_delivered` is unchanged: `Behind` is a property of `base`, not of
  which map holds the edits.
- `bump` touches cells only.
- `to_toml`/`from_toml` add an `attrs` table keyed by column name, each
  value as the TOML type it is (`Date` as a string `YYYY-MM-DD`, since
  the document vocabulary has no TOML date); a restored attribute edit
  is applied on the first non-empty successfully built model exactly as
  a parked cell edit is.

`MatrixModel.header` becomes `Vec<HeaderCell { column: SharedString,
label: SharedString, text: SharedString, edited: bool }>`, prepared per
build: `text` is the draft's value formatted when edited (dates as
`YYYY-MM-DD`, numbers through `spec.format`), else the document's own.

Part 4's upload applies `attrs` to the `DocumentRows` attribute columns
beside the cell edits, so one document goes out under one confirm —
the confirm's count names both ("3 cells, spot").

## 6. The action list

### 6.1 Approach

A **tile-owned anchored popup** (chosen over the palette scoped to the
tile: the palette is a centred overlay, not the anchored menu the user
approved, and greying would need a palette concept it lacks).

```rust
enum Popup { Menu(MenuState), Picker(PickerState) }   // on MarketDataTile
```

- `.` in normal mode, or a click on `⋯`, opens `Popup::Menu`;
  `key_context()` reports `mode == menu` while the MENU is open (the
  picker, whose filter field holds focus, reports `mode == insert` —
  §7), and the fragment binds `j`/`k`/`enter`/`escape`/`.` there (`.`
  toggles closed). Counts apply to `j`/`k`.
- **Any other dispatched action closes the popup first, then runs** —
  an unbound bare key does what it always does (`/` opens find) and
  the popup is gone by the time it does. This is the one rule that
  keeps a tile popup from needing the shell's modal machinery.
- Painted with gpui's `deferred(anchored(...))`, anchored at the
  header's right edge, so it escapes the tile's clip and paints above
  neighbouring tiles. Instant; no animation. `on_mouse_down_out`
  closes it. Theme: `popover`/`popover_foreground` background, `border`
  border, `list_active` for the highlighted row, `muted_foreground` for
  a disabled row and its reason.
- Insert mode and the popup are exclusive: `.` while an editor is open
  is a character; opening the popup from a dispatched action while an
  editor is open closes the editor first (cancel, never commit).

### 6.2 Rows

`core::menu::rows(&MenuInputs) -> Vec<MenuRow>` is pure:

```rust
pub struct MenuInputs<'a> { pub badge: DraftBadge, pub has_key: bool, pub has_model: bool, pub built_upload: bool, pub kind_actions: &'a [KindAction] }
pub enum MenuRow { Action { id: ActionId, title: SharedString, hint: SharedString, enabled: Result<(), &'static str> }, Separator, Section(SharedString) }
```

In order:

| Row | Hint | Disabled when (reason) |
|---|---|---|
| Load underlying… | `u` | draft has edits (`revert or upload first`) |
| Upload | `:upload` | not built (`not built yet`, until Part 4); clean (`nothing to upload`); Behind (`rebase or discard first`) |
| Rebase onto HH:MM | `:rebase` | shown only while Behind |
| Discard edits | `:discard` | shown only while Behind |
| Revert edits | `:revert` | clean (`nothing to revert`) |
| — separator, then `Section(spec.title)` — | | only if `kind_actions` is non-empty |
| each `KindAction.title` | | `!built` (`not built yet`) |

`enter` on a disabled row sets the reason as the notice and leaves the
popup open. The highlighted row starts at the first enabled one.

### 6.3 Per-kind verbs

```rust
pub struct KindAction { pub id: &'static str, pub title: &'static str, pub built: bool }
// PanelSpec.actions: &'static [KindAction]
// CVI: [("marketdata::cvi_reanchor", "Reanchor", false), ("marketdata::cvi_recalc_forward", "Recalc forward", false)]
```

Each is registered through `register_actions` (category `Market data`)
so the palette lists it and a keymap can bind it; `dispatch` on an
unbuilt one sets the notice `not built yet`. When built, each becomes
an egress request (§2) designed in its own slice; nothing here assumes
their shape.

## 7. Load underlying

`u` (normal mode) and the menu row open `Popup::Picker`: the same
anchored popup with a filter `Input` on top (the tile's own
`InputState`, focused — so this is insert mode for the shell's purposes:
`key_context()` reports `mode == insert` while the picker's field is
focused, and the fragment's insert-mode `enter`/`escape` commit and
cancel; `up`/`down` (and `ctrl+j`/`ctrl+k` chords) move the highlight)
and the dataset's catalog keys below, ranked by `listfilter` over
`display_key`. `enter` loads the highlighted key through the same door
`:underlying` uses; a row click loads; `escape` cancels. Opening
re-requests the catalog (`request_catalog()` + `cx.notify()` in the
same update, the existing rule) so the list is fresh. Refused with a
notice while the draft has edits, naming `:revert`. With no bridge or
no catalog yet the list shows `no underlyings known` and `enter` is
inert.

## 8. Session and commands

- `serialize`: `underlying = [..]` (was `key`); `draft.attrs`. Restore
  reads `underlying`, then `key`.
- Commands: `:underlying <value>` (completions from the catalog),
  `:key` → alias; `:set <attr> [value]`; `:menu` (opens the popup, so
  the palette has a row); the rest unchanged. `:rebase`/`:discard`
  completions still only while Behind.
- Fragment additions, `marketdata && mode == normal`: `"." =
  "marketdata::menu"`, `"u" = "marketdata::load_underlying"`;
  `marketdata && mode == menu`: `j`/`k`/`enter`/`escape`/`.`. Insert
  mode's two bindings are unchanged.
- Actions added: `marketdata::menu`, `marketdata::load_underlying`,
  `marketdata::set_attr` (the `:set` door, palette-only), and the
  kind's own.

## 9. Testing

Pure cores, no window:
- `core::cursor`: `k` at row 0 enters the strip at `min(col, n−1)`;
  `j` returns to row 0 at the remembered column; `h`/`l` clamp; `^`/`$`
  in the strip; grid verbs from the strip land in the grid; no
  attributes → `k` inert.
- `parse_attr` per `ColumnType`, including a refused date naming the
  format.
- `Draft` attrs: set/revert/discard/len/is_empty/base invariant;
  rebase keeps a declared attribute and names an undeclared one;
  `to_toml`/`from_toml` round trip (proptest beside the cell one);
  restored attrs park and resolve.
- `MatrixModel::build` prepares `HeaderCell` text and `edited` from the
  draft.
- `menu::rows` per `DraftBadge` × built flags: Upload's three reasons,
  Rebase/Discard only while Behind, the kind section present, greyed,
  and absent for a spec with no actions.
- Header preparation per state: the exact strings (`update 14:09`,
  `no document yet`, `edits await a document`, `stale`), dot present
  iff dirty.

Window tests (existing harness):
- `.` opens the popup, `escape` closes it, `j`/`k` move, `enter` on a
  disabled row notices and stays; an unrelated action (`mod+…` or a
  bare grid key) closes it first; mouse-down outside closes.
- `u` opens the picker, typing filters, `enter` loads and the panel
  requests that document; refused while dirty.
- `k` from row 0 puts the cursor on an attribute; `i` opens the editor
  in the strip; `2026-13-45` stays in insert mode with the notice; a
  good date writes `attrs`, the dot appears, the value paints tinted;
  `:revert` clears it and the dot.
- A click on an attribute value moves the cursor and opens nothing.
- `:underlying` and `:key` both load; a session written with `key`
  restores.
- Bundled-theme sweep: the dot and the strip's tinted value clear 3:1,
  reusing the delegate's ground helper.

Harness entries for each rule above with named tests. Display checks
pending on a real window: the anchored popup escaping the tile clip,
the strip's tint and cursor border, the badge and dot at 22px.

## 10. Not in this design

- Renaming `key` in the data tier or `datasets.toml`.
- Any behaviour of reanchor / recalc forward beyond the greyed row.
- Editing axes (`term`, `node`).
- A primary Upload button outside the menu (offered, declined).
- A shell-level popup facility for other modules: the popup is this
  crate's own until a second module needs one.

## 11. As built (2026-09-15)

Built as designed, with the following decisions the design left open or
got wrong, each verified against `crates/geode-marketdata` before being
recorded here.

1. **§5.1's own open note is resolved:** the pinned gpui-component's
   clear door is `TableState::clear_selection`, called from
   `MarketDataTile::sync_cursor`'s `Cursor::Attr` arm — the delegate's
   `cursor` field is also set to `None` there, so the grid mirrors no
   selection at all while the cursor is in the strip.
2. **`:bump` in the strip is refused, not silently inert or grid-wide.**
   `bump` touches cells only (§5.3 says so for the draft; the tile's
   own `bump` refuses with `"bump needs a grid cell — the cursor is in
   the header"` when the cursor is `Cursor::Attr` rather than treating
   the strip as an empty row or column). `yc` in the strip is inert
   with a notice for the same reason — a column yank names a grid
   column that does not exist in the strip.
3. **The popup closes only at the module's own doors, never the
   shell's.** `find` closes it on every `FindEvent` and `command`
   closes it for every `Command` but `Menu` (a `:` command typed while
   the menu is open should not be swallowed as "just close it" — the
   trader is mid-command). The one residual this leaves, recorded
   rather than fixed: a `:` line painted over an open menu stays
   painted until a command is actually typed, since `command` only
   fires on `enter`. Cosmetic, left as is.
4. **The `⋯` button does not stop propagation, and must not.** It is a
   `capture_any_mouse_down` listener (Capture phase, ahead of the
   popup's own `on_mouse_down_out`, which is also Capture and would
   otherwise close a just-reopened menu one beat behind this button's
   own toggle). `cx.stop_propagation()` was in the first cut and was
   wrong: it suppressed the shell's entire Bubble phase for that click,
   so clicking `⋯` on an unfocused tile opened the menu without ever
   focusing that tile — `mode == menu` then drove whichever tile the
   shell still had focused, not the one just clicked. Capture-before-
   Bubble is the whole fix; nothing needs to be suppressed at all.
5. **The picker has no chords — spec §7 is wrong and is corrected
   here.** §7 says "`up`/`down` (and `ctrl+j`/`ctrl+k` chords) move the
   highlight"; only bare `up`/`down` were built, and the two chords
   were deliberately dropped (commit `cdfae5d`). A shipped chord (here,
   `ctrl+k`, the palette) must still fire from inside any module's
   insert-mode field exactly as it fires from inside a cell editor —
   CLAUDE.md's own standing rule — because a chord resolves against the
   *whole* context stack, and narrowing that shadow to "only while the
   picker is open" still takes the palette away exactly when a trader
   is typing into the picker. `ctrl_k_still_opens_the_palette_from_the_
   open_picker` is the test that pins this. §3's grammar table
   (`up`/`down` — chords never named there) already agreed with the
   build; only §7's own sentence needed correcting.
6. **Picker identity is the catalog key string, never a row index —
   found and fixed across two review rounds, not designed in from the
   start.** The first build re-ranked (and reset the highlight to row
   0) on every `commit`, including the defensive re-rank `commit`
   always makes to cover `InputState::set_value` firing no `Change`
   event at all — so `u`, `down`, `down`, `enter` always loaded the TOP
   match, silently discarding whichever row the trader had highlighted
   (round 1, CRITICAL, fixed in `cf166b7`). The fix's own `rerank`
   still captured the highlighted position as an index into `all` and
   looked that number up again after the catalog re-sorted — since
   `catalog_keys()` returns a freshly sorted list, a newly arrived key
   that sorts earlier shifts every later index, silently re-highlighting
   a *different* row with no signal anything had moved (round 2,
   CRITICAL, fixed in `59111d3`). `PickerState::place(key: Option<&str>)`
   is now the one door every re-rank goes through — `refilter` (a real
   query change) and the diagnostics observer's own catalog refresh
   both capture `highlighted_key()` before rebuilding and hand it back
   in — falling back to row 0 only when the key is genuinely gone.
   `commit_picker` re-ranks from the field's live text (`p.input.read
   (cx).value()`) before reading `highlighted`, never trusting whatever
   `ranked` last held, for the `set_value`-fires-no-`Change` reason
   above.
7. **`DraftBadge::Sent` carries no timestamp.** §4 designed `Sent { at }`
   painting "sent HH:MM" in the info tone; Part 3 built the unit variant
   `Sent` (no field) and `HeaderModel::prepare` paints a bare `"sent"`
   in `Tone::Time` (the muted tone, not a distinct info tone — none was
   added). This is deliberately provisional: nothing in Part 3 ever
   constructs `Sent` (`:upload` still answers "upload is not built
   yet"), so there is no `at` to carry yet and no reader depending on
   one. Part 4 (egress) is expected to add the field and the real
   paint when upload actually produces a sent state.
8. **`marketdata::set_attr` was not registered as an action.** §8 lists
   it among "actions added" as "the `:set` door, palette-only"; the
   built `ACTIONS` table (`content.rs`) has no such entry and `:set`
   remains reachable only by typing the command line — there is no
   `ActionDef`, so it does not appear in the palette and cannot be
   bound in a keymap. `:set`'s own behaviour (parse, refuse, report the
   current value with no argument) is built exactly as §5.2 describes;
   only the palette-visibility half of §8's sentence was not.
9. **Insert-mode routing while the picker is open**, beyond `up`/
   `down`: `commit` and `cancel` are excluded from `dispatch`'s
   close-first gate (alongside the five menu verbs) precisely because
   `key_context()` reports `mode == "insert"` while a `Picker`'s field
   holds focus, which is what puts the shell's `enter`/`escape` in a
   trader's hand as this crate's own `commit_picker`/`close_popup_with_
   window` rather than having the generic "any other action closes the
   popup" rule swallow them first.

Every rule above has a harness entry and a named test in
`crates/geode-marketdata/src/tile.rs`, `popup.rs`, `commands.rs` or
`core/menu.rs`'s own test modules; `scripts/mutation-check.sh` carries
one `run_mutation` entry per behaviour changed on this branch (Task 8
brought the harness to 928 entries; the final review's wave below, to
942 on the branch, 968 after merging main's verb-consistency entries — counted as `grep -c '^run_mutation "'`, the bare `^run_mutation`
count including the function definition). Display checks — the
anchored popup escaping the tile clip, the strip's tint and cursor
border, the badge and dot at 22px — remain pending on a real window, as
recorded in §9.

### Final review (2026-09-17)

Two reviewers over the whole branch found no Criticals; every item
below is fixed, each with a named test and a harness entry.

- **`close_popup(cx)` is deleted**, `debug_assert!` and all:
  `close_popup_with_window` is the one door, and every close blurs
  through the `Window` its site already had. The sites believed unable
  to see a Picker could — `u` then `ctrl+k` then the palette's Find
  reaches `find`; `u` then `mod+l` (the shell moves focus to its root,
  the picker stays `Some`) then `/` or `:` reaches `find` or `command`;
  the menu's `on_mouse_down_out` closure had a window all along.
  `TileContent::find` now forwards its window. Harness: `mdmenu: a find
  keystroke closes the popup` (re-anchored), `mdmenu: a command line
  closes the popup` (new).
- **`set_key` cancels an open cell editor** before swapping the
  document — a key change is navigation, never a commit, and an editor
  left open passed its label-identity check on a same-ladder underlying
  and filed the typed number into the NEW document's draft.
  `a_key_change_cancels_an_open_editor`; `mdattr: a key change cancels
  an open editor`.
- **An attribute value's click cancels an open editor** as a grid cell's
  click does — it was the one mouse door that left one open and deaf
  behind the shell's focus re-arm.
  `an_attribute_click_cancels_the_editor_then_moves`; `mdattr: an
  attribute click cancels an open editor`.
- **The menu row's `stop_propagation` is load-bearing** for the row →
  picker path ("Load underlying…" focuses the picker's field inside the
  row's own handler; a bubble past it would re-arm
  `pending_focus_restore` and take the keyboard back next render) — the
  opposite of `⋯`'s, which must NOT stop because no field is focused
  after it. Found while testing it: the pinned
  `TableState::set_selected_row` (run by `sync_cursor` at the end of
  every `dispatch`) stops propagation of its own, so the row's stop is
  hidden while the cursor is in the grid and is the only one with the
  cursor in the strip (`clear_selection`) — the test starts there.
  `the_menu_rows_stop_propagation_keeps_the_pickers_focus`; `mdmenu:
  the menu row's stop_propagation keeps the picker's focus`.
- **`is_stale` is reachable**: `header_texts_at(now)` is the test door;
  not stale at `BASE + 1s` nor at exactly `stale_after`, stale one
  second past it. `the_time_chip_says_stale_past_stale_after`;
  `mdheader: the time chip says stale past stale_after`.
- **`PickerRows`** is the picker's pure half (`PickerState { input,
  rows }`), with its own test module: an unchanged query keeps the
  highlight, a changed one re-places by key, a filtered-out key falls to
  row 0, an empty catalog has no highlighted key, `step` clamps both
  ways. **The picker paints at most `PICKER_ROWS` (12)** ranked keys —
  a cap, not a scroll container, since the query narrows the rest — and
  `place`/`step_highlighted` clamp the highlight to the painted range.
  `mdpicker: place falls back to row 0 when the key is gone`, `the
  picker paints at most PICKER_ROWS rows`, `step stops at the last
  painted row`.
- **`parse_attr` parses an `I64` directly**, exact above 2^53 (the
  `parse_cell` → `f64` → `as i64` path silently rounded), same error
  text. `mdattr: an I64 attribute parses exactly above 2^53`.
- **`:set <attr> <value...>`** joins its tail with single spaces (a
  `Utf8` attribute may carry them); the usage error for no attribute
  stands. `mdattr: a multi-word set value is joined with spaces`.
- **`down`/`up` move the menu highlight** beside `j`/`k` (the picker
  already had them). `mdmenu: the arrow keys move the menu highlight`.
- Triage: `:set <attr>` with no document answers `no document to edit`
  (`mdattr: set with no document says no document`); `FindState.origin`
  is the whole `Cursor`, so a find cancelled from the strip returns to
  the strip (`mdattr: a find cancelled from the strip returns to the
  strip`); window tests for `toggle_menu` over an open editor and a
  click outside the picker (the latter clicks an attribute value, not a
  grid cell — the pinned `DataTable` `track_focus`es its own handle, so
  a cell click takes focus on the same mouse-down and `focused ==
  None` would say nothing about the blur).
- Minors: `NO_DEFAULT_KEY` (was `MENU_ONLY`) is checked as an exact set
  in both directions, kind actions included; the dead
  `MenuInputs.has_key` is gone; the time chip's colour goes through
  `tone_colour(Tone::Time, stale, ..)`; `commands.rs`'s docs say every
  verb is built (`upload` alone answers not-built until Part 4);
  `mdheader: a dirty draft paints the dot` is renamed `sets the dot
  flag`. The harness stands at 941 entries.
- **Re-review (same day):** the one-door close blurred whenever the popup
  was a Picker, whether or not its field held focus. A picker orphaned
  with the keyboard elsewhere — `u`, `ctrl+k` (the palette), the
  palette's "Find" (the shell's command line, picker still `Some`) — had
  the first find keystroke blur the FIND FIELD, and the shell's focus
  backstop cancelled the command line: the find died after one
  character; the same shape on a `:` line returning `Err`.
  `close_popup_with_window` now blurs only when the picker's own field
  `is_focused`, and `close_editor` carries the same guard (an editor
  orphaned by `mod+l` and closed from a `:` line must not blur the
  command line). `a_find_keystroke_with_an_orphaned_picker_keeps_the_
  foreign_focus`; `mdmenu: closing an orphaned picker never blurs a
  foreign field`. Harness at 942.

**Deferred to Part 4:** `Sent` dirty-semantics unification, sent-attribute
paint, rebase with a NULL attribute, attribute number formatting via
`spec.format`, `from_toml` date coercion by `HeaderAttr.ty`, the
geode-app type cross-check, a `registry()` test helper, the
dropped-attribute notice wording, and `:upload`'s wording.

### 11.x Merge with main (2026-09-17)

Main's verb-consistency merge (spec §20.5) landed while this branch was
in review: every list and tile now WRAPS the row axis on a bare `j`/`k`
(`vimnav::apply`) and clamps larger deltas and the column axis; the panel
also gained `page_down_full`/`page_up_full` (`FULL_PAGE` = 10). The two
rules are composed rather than chosen between: `core::cursor::step`'s
grid-row arm goes through `vimnav::apply`, so `j` on the last row wraps
to row 0 and `k` on row 0 wraps to the last row **only when the panel has
no attribute strip**; with attributes, `k` on row 0 — bare or counted —
enters the strip (§5.1), and `k` in the strip stays. The strip therefore
sits above the wrap cycle rather than inside it: repeated `j` cycles the
grid forever, repeated `k` stops at the strip. Pinned by
`a_bare_step_wraps_the_grid_and_the_strip_stays_outside_the_cycle` (pure)
and main's `a_bare_row_step_wraps_and_the_full_page_keys_move_ten`
(window; its top-row `k` step now asserts the strip). Main's harness entry
for the wrap was re-anchored from `move_cursor` (deleted here) to that
grid-row arm. Harness count after the merge: 968.

### 11.y Mouse editing and arrow nudging (2026-09-17)

User ruling 2026-09-17 reverses §5.1's and §8.8.6's "the mouse opens
nothing": **a double-click on an attribute value opens its editor**, in
the strip, seeded with the painted value — `MarketDataTile::attr_clicked
(i, click_count, ..)` is the door `header::render` now attaches, every
press being `cursor_to_attr` and the second press of a pair also running
`begin_edit` (the same refusals `i` has) — and a double-click on a grid
cell opens the cell's (`TableEvent::DoubleClickedCell`; the documents
spec's §8.8.6 has the whole account). A single click still only moves
the cursor, and a click elsewhere while an editor is open still cancels
it. Neither listener stops propagation: the shell's tile-level
mouse-down runs as before, and what keeps the opened editor focused is a
shell rule — `ShellView::render` withholds `pending_focus_restore`'s
focus move only while the focused tile's occupant itself HOLDS the
focused handle in insert mode (`occupant_holds_insert_focus`:
`TileContent::holds_focus` answered by that occupant for the focused
handle — this panel answers off its editor's and picker's own focus
handles, which are never shell surfaces — and `mode ==
insert`; the insert branch's own predicate, shared) — never a swallowed
event.

The insert-mode `up`/`down` pair is no longer the picker's `menu_up`/
`menu_down`: `up`/`down`/`shift+up`/`shift+down` bind
`marketdata::insert_up`/`insert_down`/`insert_up_big`/`insert_down_big`,
whose meaning follows which input is open. With the picker open (§7)
they step its highlight exactly as before (`_big` is one step too — a
list has no "big"). With the cell or attribute editor open they NUDGE
its text: one unit (ten with `shift`) of the target's painted precision
— a cell at its column's format, a slice column's own `precision`
first; an `F64`/`I64` attribute at the places its text paints, so
`spot`'s `5000` steps by one; a `Date` attribute by whole days, so
`anchor` steps `2026-09-12` to `2026-09-13`. `core::nudge::nudge_text`
is the arithmetic (scaled-integer, so no float rounding leaks into the
text); nothing is committed by a nudge — `enter` commits and `escape`
cancels exactly as before, and unparseable text is left alone with the
usual inline notice. `menu_up`/`menu_down` stay the menu block's verbs
(`j`/`k`/`up`/`down` in `mode == menu`). Harness: `mdedit: a
double-click on an attribute opens its editor`, four `mdnudge:` entries.
