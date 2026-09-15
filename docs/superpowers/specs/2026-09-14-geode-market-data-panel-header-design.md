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
