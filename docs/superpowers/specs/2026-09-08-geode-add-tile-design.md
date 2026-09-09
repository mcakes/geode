# Geode — Adding Tiles Design

Amends `docs/superpowers/specs/2026-08-28-geode-foundation-design.md`
§3.1 (the split verbs) and
`docs/superpowers/specs/2026-09-03-geode-phase-3-blotter-design.md`
§3.2 (a new tile "is filled by the module named in `[app]
modules.default`"). Everything else in both documents stands.

## 1. Why this changes

Today a tile is created by a *split* (`ctrl+v` side by side, `ctrl+h`
stacked) and the shell then decides what goes in it: the module named in
`[app] modules.default`, `"blotter"` when unset. Opening anything else —
diagnostics — is a separate door (`diagnostics::open`) that splits
sideways, unconditionally, and stashes the kind for the next render.

That makes "split" the verb and "what fills it" an afterthought, which
is backwards for a desk with more than one module. It also leaves three
things half-said: an empty workspace paints a hint naming the split
keys; an empty dock cannot hold focus, so nothing can be opened *into*
it without first opening it elsewhere and moving it; and showing a dock
does not focus it, so the natural sequence "open the left dock, put a
watchlist in it" is four keystrokes with a focus hop in the middle.

This spec makes **adding a tile of a chosen kind** the only way a tile
is created. A split is how an add is *placed*, not a verb of its own.

## 2. Vocabulary

- **Add**: create a tile hosting a named module kind, placed relative
  to the focused tile (§4). The palette rows follow the crate's
  established `Category: Verb` pattern ("Pick: <column>", "Scope:
  <name>"): "Blotter: Split", "Blotter: Split Horizontal", "Blotter:
  Split Vertical", and the same three for every other kind in the
  roster (retitled from the original "Add Blotter"/etc. wording by
  user ruling 2026-09-09 — see §13(e)).
- **Horizontal**: tiles arranged left → right; the new tile lands to
  the *right* of the focused one (`Orientation::Horizontal`, what
  `ctrl+v` did). **Vertical**: stacked; the new tile lands *below*
  (`Orientation::Vertical`, what `ctrl+h` did). The tree already names
  its orientations this way; the palette titles now say the same word.
- **Auto**: the setting's default. Split along the focused tile's
  longer side — wider than tall lands to the right, taller than wide
  lands below. Stateless; it produces grids rather than ever-thinner
  strips.
- **Duplicate**: add a tile of the focused tile's kind carrying the
  focused tile's serialized state (§6).
- **Empty pane**: a focused region whose tree has no tiles (a fresh
  workspace, a just-shown dock), *or* a focused tile hosting the
  placeholder occupant. An add on an empty pane *fills* it (§4).

## 3. Actions and keys

### 3.1 Removed

`workspace::split_right` and `workspace::split_down` are unregistered
and their `ctrl+v`/`ctrl+h` bindings removed from `BUILTIN_KEYMAP`.
Both keys are free. `workspace::toggle_split_orientation` (`mod+e`),
`workspace::close_tile` (`ctrl+w`) and every focus/move/resize binding
are unchanged.

### 3.2 Per-kind add actions

`register_add_actions(reg, roster)` runs at startup after
`register_builtin_actions` and **before `build_keymap`** (which drops
bindings to unregistered actions), exactly where `register_pick_actions`
and `register_scope_actions` already run. For each `kind` the roster
holds it registers, in category `"Tiles"`:

| id                              | title                          |
|---------------------------------|---------------------------------|
| `tile::add_<kind>`              | `<Kind>: Split`                 |
| `tile::add_<kind>_horizontal`   | `<Kind>: Split Horizontal`      |
| `tile::add_<kind>_vertical`     | `<Kind>: Split Vertical`        |

`<Kind>` is the kind string with its first letter upper-cased
(`blotter` → `Blotter`, `diagnostics` → `Diagnostics`). The palette
needs no change: every registered action is already a row.

Dispatch strips the `tile::add_` prefix (the same pattern
`frame::pick_`/`scope::` use in `ShellView::dispatch`), then peels a
trailing `_horizontal`/`_vertical` into an explicit direction, and
takes the remainder as the kind. This ordering means a kind can never
be misparsed by its own suffix, since the suffix is peeled first.

*Amended 2026-09-08 (as built):* dispatch does **not** consult the
roster. Only *registered* action ids ever reach `ShellView::dispatch`
(the palette and the keymap both resolve through `ActionRegistry`), and
registration is `register_add_actions` over the roster's own kinds — so
"the remainder names a real kind" holds by construction rather than by
a lookup. `defaults::parse_add_action` is therefore pure string work
(`Option<(&str, Option<Orientation>)>`, `None` for an empty kind or a
non-`tile::add_` id). The kind is checked once, later and in one place:
`ensure_occupants` warns (`target: "geode::shell"`) and paints the
placeholder if a pending request's kind has no factory — which is also
what the tests rely on to exercise the placeholder path.

### 3.3 Duplicate

Two built-in actions in category `"Workspace"`:

| id                                | title                       | key            |
|-----------------------------------|-----------------------------|----------------|
| `workspace::duplicate_horizontal` | Duplicate tile horizontal   | `shift+d`      |
| `workspace::duplicate_vertical`   | Duplicate tile vertical     | `ctrl+shift+d` |

Both live in the always-active `workspace` context. Neither collides:
no context binds `d` or `shift+d`; the blotter's and diagnostics'
`ctrl+d` (page down) is a different keystroke; and a focused text
`Input` (the scope-bar filter, a tile's command line, any dialog's
filter) already swallows every key but `escape` before the shell's
matcher sees it, so `D` typed into a field never duplicates a tile. A
shifted letter keeps its `shift` modifier on every platform
(`defaults.rs`'s platform note), so both keystrokes parse as written.

### 3.4 `diagnostics::open` is retired (user ruling 2026-09-09)

The status bar's summary click is the one focus-or-add door, via
`open_module`; the palette's `Diagnostics: Split` rows always add (or
fill).

## 4. One door: `add_tile`

```rust
impl ShellView {
    /// Create (or fill an empty pane with) a tile of `kind`, carrying
    /// `state` as its restored record when given.
    pub fn add_tile(
        &mut self,
        kind: &str,
        direction: Option<Orientation>,   // None = the setting decides
        state: Option<toml::Table>,
        window: &mut Window,
        cx: &mut Context<Self>,
    );
}
```

`add_tile` is the only place a tile id is allocated outside session
restore. `open_module` becomes a thin wrapper (its existing-occupant
search, then `add_tile(kind, None, None)`).

### 4.1 Direction

1. An explicit `direction` wins.
2. Else the setting (§5): `Horizontal`, `Vertical`, or `Auto`.
3. `Auto` reads the focused tile's rect from the pure layout —
   `dock_layout(docks, content_area)` then `Tree::layout` for the
   focused region — over the same content area the drag code derives
   from `window.viewport_size()`. `w >= h` → `Horizontal`, else
   `Vertical`. No rect (nothing focused, or an area of zero size) →
   `Horizontal`. This is the pure function
   `AddDirection::resolve(setting, explicit, rect: Option<Rect>) ->
   Orientation`, tested without a window.

The content-area calculation is factored into one `content_area(window)`
helper that `drag.rs` and `add_tile` share; `render` keeps its own
because it also subtracts the as-of stripe it is about to paint. A few
pixels of stripe cannot change which side of a tile is longer in any
case that matters.

### 4.2 Placement

Exactly one of, checked in this order:

1. **Fill a placeholder.** The focused tile hosts `PLACEHOLDER_KIND`:
   record a pending request *for that tile id*. No split; the tile
   keeps its id and its focus.
2. **Fill an empty region.** The focused region's tree is empty:
   `Tree::split` on an empty tree makes the new tile its root, so this
   is `split_active(direction)` like case 3 — the direction is simply
   irrelevant. Named separately because it is the case a fresh
   workspace and a just-shown dock hit.
3. **Split.** `split_active(direction)` beside the focused tile; the
   tree focuses the new tile (`Tree::split` always does).

In cases 2 and 3 `Workspaces::split_active` now *returns* the new
`TileId`, and the pending request is recorded under it. Every case
sets `session_dirty` and `cx.notify()`s, so `ensure_occupants` consumes
the request on the very next render.

### 4.3 Pending requests are addressed

`pending_kind_for_new_tile: Option<String>` becomes

```rust
pending_tiles: BTreeMap<TileId, PendingTile>,
pub struct PendingTile { pub kind: String, pub state: Option<toml::Table> }
```

`ensure_occupants` looks a tile up by id rather than spending a single
request on "the first occupant-less, non-restored tile in id order".
Consequences, all deliberate:

- The MIN-7 same-kind guard (two `open_module` calls in one render
  splitting twice) is retired: a repeated request for the *same* tile
  simply overwrites, and `open_module`'s existing-occupant search
  already treats a pending tile of the requested kind as "open" (it
  consults `pending_tiles` too, so a second `open_module` call for the
  same kind before the render focuses nothing new and adds nothing).
- The "lands on the lower tile id when two tiles go occupant-less in
  one pass" rule is moot; that test is replaced by one asserting exact
  addressing.
- A request for a tile that no longer exists (closed before the render)
  is dropped by `ensure_occupants`'s existing retain-to-live-tiles
  sweep and logged at `debug`.
- Filling a placeholder (case 1): the placeholder occupant is removed
  and the pending factory creates in its place, `state` passed as
  `restored`. If that tile had an unknown-kind session record (§7.2)
  the user has chosen to replace it, and the record goes.

Kind selection in `ensure_occupants` is now: **restored record whose
kind the roster knows → pending request → placeholder.** There is no
default factory (§7).

## 5. The setting

`[tiles] add = "horizontal" | "vertical" | "auto"` in the `app` doc,
default `auto`. Modelled on `FindStyle` end to end, in a new pure
module `crates/geode-shell/src/tileadd.rs`:

- `pub enum AddDirection { Horizontal, Vertical, Auto }`, `Default` =
  `Auto`, `ALL` in display order, `label()` (`Horizontal` / `Vertical`
  / `Auto`), `config_value()`, `from_value`, lenient `from_config`
  (missing or unknown → default, no diagnostic — `FindStyle`'s
  precedent).
- `resolve(self, explicit: Option<Orientation>, rect: Option<Rect>) ->
  Orientation` (§4.1).
- `persist_to_user_config(user_dir, value)`: `toml_edit` read-modify-
  write of `<user_dir>/app.toml`, the seventh sibling of
  `vimfind::persist_to_user_config`. **It is a Phase 4c migration
  target** (`config_write`, that spec's first task) and is not to be
  consolidated by hand here — CLAUDE.md's standing rule.
- `ShellView.add_direction: AddDirection`, set at construction and
  re-derived on hot reload beside `find_style`.
- A fifth settings-dialog row: `SettingId::AddDirection`, title "Add
  tile", category "Tiling", values from `ALL`, stepping wraps like the
  others, `apply_setting` sets the field and spawns the persist off the
  UI thread with failures logged under `geode::config`.

## 6. Duplicate

`workspace::duplicate_*` reads the focused tile's occupant, calls
`TileContent::serialize` on it, and calls
`add_tile(kind, Some(direction), Some(table))`. The table is *exactly*
the session record, so what survives a restart survives a duplicate and
nothing else does: for the blotter that is the view, a pinned grouping
or slot, `unscoped`, and the tile filter; cursor, expansion and scroll
do not (Phase 3 §3.5's honesty rule — the duplicate requeries and
starts collapsed at the top). A placeholder or an empty region has
nothing to duplicate: no-op, no notify.

## 7. No default kind

### 7.1 Removal

`[app] modules.default` is no longer read. `ModuleRoster::new()` takes
no default; `default_factory()` is deleted; `ModuleRoster::kinds()`
gains its first production caller (§3.2). A user layer that still sets
`modules.default` gets a `warn`-level config diagnostic at load and
reload — "`modules.default` is no longer read; tiles are added by kind
(ctrl+k → Add …)" — and is otherwise ignored.

### 7.2 Placeholders

A tile with no restored record and no pending request paints the
placeholder, as does a restored tile whose kind the roster does not
know — what Phase 3 §3.2 specified and `ensure_occupants` never did (it
fell back to the default kind and discarded the state). The unknown
record now **rides through**: `ShellView` keeps the restored records it
could not place, keyed by tile id, and `current_tiles` writes them back
verbatim alongside the live occupants, so a session saved by a build
with more modules than this one is not silently thinned by the next
flush. A record is dropped only when its tile is closed or filled.

### 7.3 Hints

Every empty-state hint names the palette rather than a key that no
longer exists:

| where                        | text                                              |
|------------------------------|---------------------------------------------------|
| empty main tree              | `ctrl+k → Add a tile`                             |
| placeholder tile             | `ctrl+k → Add a tile`                             |
| empty dock (left/right/bottom) | `ctrl+k → Add a tile here · ctrl+shift+[ moves one` (bracket per side) |
| empty main tree, dock focused | `ctrl+k → Add a tile · focus is in the left dock` (`right`/`bottom` per side) |

The dock-focused row keeps the single `empty-hint` selector and the same
verb; it only appends where the add would actually land, because the
hint paints over the *tree's* area and would otherwise read as an offer
to fill the space the reader is looking at (final review, Ruling J). The
old state-aware "return" variants are still gone.

`PlaceholderContent::command`'s error text is unchanged.

## 8. Docks take focus

`Workspace::toggle_dock` showing a hidden dock now calls
`enter_region(Dock(side))`: the dock is visible and focused in one
step, whether or not its tree has tiles, so `ctrl+[` then "Add Blotter"
fills the left dock. Hiding a dock is unchanged (focus falls back per
`fallback_region`).

The invariant relaxes from "the region may name a dock only while that
dock is focusable (visible and non-empty)" to "**the region may name a
dock only while that dock is visible**". `Dock::focusable()` keeps its
meaning for `fallback_region` — focus that must *move somewhere* still
prefers a dock with tiles over an empty one, and `Main` over both when
the tree has tiles. `focused_tile()` returns `None` on an empty focused
dock, which every caller already tolerates (it is the empty-workspace
case today). `move_to_dock`, `focus_dock`, `focus_dock_tile`, the
directional verbs and the drop verbs are unchanged; the test
`toggling_an_empty_dock_shows_it_without_taking_focus` flips to assert
the new rule.

## 9. Session

The `session.toml` format does not change. `tiles` records are written
for every live non-placeholder occupant *plus* every unplaced record
(§7.2). Restore is unchanged except for the kind-selection order in
§4.3.

## 10. Tests

Weight follows spec §10.3: pure cores first.

**Pure (no window):**
- `AddDirection`: `from_value`/`from_config`/`config_value` round-trip;
  `resolve` — explicit beats setting; `Auto` on a wide rect →
  `Horizontal`, tall → `Vertical`, square → `Horizontal`, `None` →
  `Horizontal`.
- `register_add_actions`: three ids per roster kind with the specified
  titles and category; an empty roster registers nothing.
- Action id parsing: `tile::add_blotter_vertical` → (`blotter`,
  `Some(Vertical)`); `tile::add_blotter` → (`blotter`, `None`);
  `tile::add_nope` → not an add.
- `Workspaces::split_active` returns the id that is now focused, in the
  main tree and inside a focused dock.
- `toggle_dock` on a hidden empty dock shows it *and* sets the region;
  toggling it back hides it and falls back to `Main`.
- `persist_to_user_config` (mirroring `vimfind`'s tests): creates the
  file, preserves other tables, refuses an unparseable file.

**Shell (`#[gpui::test]`, real keystrokes and palette rows):**
- On an empty workspace, palette "Add Blotter" makes one blotter tile
  that is the tree's root and focused.
- On a placeholder tile, "Add Blotter" fills it in place: same
  `TileId`, kind now `blotter`, tile count unchanged.
- On a blotter tile with the setting `horizontal`, "Add Diagnostics"
  lands a diagnostics tile to the right; with `vertical`, below;
  "Add Diagnostics Vertical" lands below regardless of the setting.
- With the setting `auto`, a wide tile splits to the right and a tall
  one below (two-step: split right first, then add again in the
  now-tall half).
- `shift+d` on a blotter showing view `wide` yields a second tile whose
  factory received `view = "wide"`; `ctrl+shift+d` stacks it.
- `ctrl+v` and `ctrl+h` change nothing (tile count and layout
  identical before and after).
- `ctrl+[` focuses the (empty) left dock; a following "Add Blotter"
  lands inside it; `ctrl+[` again hides it and focus returns to `Main`.
- Two `open_module("diagnostics", ..)` calls (the status bar's summary
  click, the door's one production caller since `diagnostics::open`
  was retired — user ruling 2026-09-09) yield one diagnostics tile,
  focused (the retired MIN-7 guard's behaviour, now by addressing).
- A restored record of an unknown kind paints the placeholder and is
  written back verbatim by the next `current_tiles`.
- `modules.default` in the user layer produces the §7.1 diagnostic.
- The settings dialog shows the "Add tile" row, stepping cycles the
  three values and persists.

**Integration:** `tests/tiling_integration.rs`'s two tests are rewritten
to drive `shift+d` through the real matcher against `BUILTIN_KEYMAP`.
The palette rows themselves are not reachable there — see §13(d): a
matcher-only test has no palette, so the tiles those tests need are
created by calling `Workspaces::split_active` directly.

**Mutation harness** (`scripts/mutation-check.sh`, one entry per
behaviour, 6th argument naming the test): auto's `w >= h` comparison
flipped; fill-in-place replaced by a split; the pending map keyed by
the wrong id; duplicate dropping its `state`; `toggle_dock` not
entering the region; `split_active` returning a stale id; the unknown
record not written back; the `_vertical` suffix parsed as `Horizontal`;
`register_add_actions` skipping the suffixed pair. These are the tiling
layer's first entries.

## 11. Docs

- `CLAUDE.md`: the Phase 1c line naming `ctrl+h`/`ctrl+v` splits, and
  the Phase 4b paragraph's `open_module` description, updated to the
  add door; a gotcha for the region invariant (§8) and for the pending
  map (§4.3).
- Foundation spec §3.1's "split vertical/horizontal" verb line gains a
  pointer to this document; Phase 3 §3.2's `modules.default` sentence
  likewise.
- `docs/modules.md` (untracked, the author's notes) is not touched.

## 12. Sequencing

1. Pure cores: `tileadd.rs`, `split_active` returning the id,
   `toggle_dock` focus, action registration and parsing.
2. `pending_tiles` map + `ensure_occupants` order + placeholder record
   pass-through + roster default removal + `modules.default`
   diagnostic.
3. `add_tile`, `open_module` over it, dispatch of `tile::add_*` and
   `workspace::duplicate_*`, keymap changes, hints.
4. Setting: field, hot reload, dialog row, persist.
5. Integration tests, mutation entries, docs.

## 13. Implementation notes (2026-09-08)

Four things the build settled that the design above did not say:

(a) `Workspace::toggle_dock`'s show branch exits main-tree fullscreen
first. A fullscreen tile plus a focused dock is a state `render` cannot
paint (no docks while fullscreen, no focus ring outside `Main`) and
`toggle_fullscreen` refuses to undo while a dock is focused, so `mod+f`
was left dead — the same precedent `move_to_dock`'s `Main` arm already
set.

(b) The shell's test fixtures carry a `rec` recording factory in their
roster (`shell/tests/mod.rs`). With §4.2's "an add on a focused
placeholder fills it in place", an empty roster makes every add after
the first collapse into the first tile, so no two-tile fixture could
exist without one. The fixtures also bind `ctrl+v`/`ctrl+h` to
`tile::add_rec_horizontal`/`_vertical` in a test keymap layer
(`TEST_ADD_KEYMAP`) — the shipped keymap has no create-a-tile chord
(§3.1), and these are exactly the shape a desk keymap would ship.

(c) `[modules] default` was removed from `examples/demo-config/app.toml`
(§7.1); the key now only produces the "no default kind" diagnostic if a
user config still sets it.

(d) `tests/tiling_integration.rs` drives only `shift+d` through the real
matcher and creates its tiles by calling `Workspaces::split_active`
directly. That file tests the keymap-to-`Workspaces` seam with no
`ShellView` and therefore no palette, so §10's "the palette rows … through
the real matcher" is not something it can do; the palette rows are covered
by the `#[gpui::test]`s in `shell/tests/` instead.

(e) **User ruling 2026-09-09, follow-up:** the per-kind add rows are
retitled to the crate's established `Category: Verb` pattern (§2,
§3.2) — "Blotter: Split", "Blotter: Split Horizontal", "Blotter: Split
Vertical", and the same for every other kind — superseding this
document's original "Add <Kind>" wording everywhere it appears. Action
ids are unchanged. In the same ruling, `diagnostics::open` (§3.4's
original "keeps its meaning" text) is retired along with its
`mod+shift+d` binding: the status bar's diagnostics-summary click
(`render.rs`'s `on_diagnostics_click`, calling `open_module` directly)
is now the one focus-or-add door, and the palette's `Diagnostics: Split`
rows always add (or fill) rather than ever focusing an existing tile —
`register_add_actions`'s rows never did the existing-occupant search
`open_module` does, so this only removes a second, now-redundant way to
reach the same tile.
