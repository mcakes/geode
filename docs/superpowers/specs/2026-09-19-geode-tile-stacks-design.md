# Geode — Tile Stacks Design

Amends `docs/superpowers/specs/2026-08-28-geode-foundation-design.md`
§3.1 (the "tabbed/stacked container modes" line) and
`docs/superpowers/specs/2026-09-08-geode-add-tile-design.md` §3.2 (the
per-kind add rows) and §4.2 (placement). It also retires the
"segmented-title-row direction" the drop-zone module's header note
records as chosen for this feature; the centre-drop meaning change that
note accepted in advance is taken here. Everything else in those
documents stands.

## 1. Why this changes

A slot in the tiling tree holds exactly one tile. A trader who wants a
second blotter grouped by strike beside the book view, or the CVI panel
for three underlyings, has to spend screen area on every one of them at
once, or park the extras in a dock and hop. The foundation spec promised
i3's tabbed containers for exactly this; they are unbuilt.

The obvious build, a tab bar on every pane, is refused on cost: a
permanent row of chrome on every tile is the one thing an index-exotics
desk's screen cannot spare, and most tiles would carry an empty bar.
This design pays for the feature only on the tiles that use it, and
there only in the header strip the module already paints.

Brainstormed with the user 2026-09-19 through four choices, each
recorded where it lands below: tabbing serves density (a junk-drawer
slot) and grouping (variants of one thing) equally (§2); the rest state
is a **tiny marker**, never a strip (§5); switching is a chord pair plus
a click on the marker, with directional focus treating a stack as one
tile (§4); tiles get in by kind and by a centre drop, never by a
"convert this split" verb or a "stack with neighbour" chord (§6); and
the marker lives in the **module's own header**, placement D of the
four mocked up, chosen over a shell-painted border notch or corner pill
(§5).

## 2. Vocabulary

- **Stack**: a slot holding two or more tiles, exactly one of which,
  the **active** member, is painted. The others are **hidden**: their
  occupants stay alive (as tiles in a switched-away workspace do) and
  are told `set_visible(false)`, so they hold no live subscription and
  requery nothing.
- **Member**: a tile inside a stack. A member is a leaf; a stack never
  contains a split. i3 permits nested splits inside a tabbed container,
  and nothing on this desk needs one.
- **Marker**: the `2/4` chip a module paints first in its header strip
  while its tile is a member. Index is one-based, in the stack's order.
- **The list**: the shell-owned transient overlay naming every member of
  the focused tile's stack, opened from the marker or from the palette.
- **Slot verbs**: every existing workspace verb that reasons about a
  tile's rectangle (focus direction, resize, divider drag, orientation
  toggle, fullscreen, split beside, move to dock). To all of them a
  stack **is** its active member, with no code of their own (§3).

## 3. The model

`Node` gains a third variant:

```rust
pub enum Node {
    Leaf(TileId),
    Split { orientation, children, ratios },
    Stack { children: Vec<TileId>, active: usize },
}
```

Invariants, held by every tree verb and re-established on restore (§7):
`children.len() >= 2`, `active < children.len()`, members are leaves by
construction (the variant holds `TileId`s, not `Node`s), and a tile id
appears once in the whole tree.

**Layout.** `Tree::layout` emits one `(TileId, Rect)` for a stack, the
active member's, over the whole node rect. Because layout is the only
geometry authority (rendering, `neighbor`, divider drag and the drop
zones all consume it), every slot verb sees a stack as one tile with
nothing further to teach it. `Tree::tiles()` still returns every tile,
members included, so occupant retention, session dirt and the sidebar
counts keep seeing them.

**Focus.** `Tree::focus(id)` on a hidden member makes it active first.
This is what session restore and `ShellView::open_module`'s
focus-an-existing-tile arm use, so both land on a visible tile without
knowing about stacks. `focused()` is unchanged: the focused tile of a
workspace is a member only ever when that member is active.

**Fullscreen.** `fullscreen` is a `TileId`; when a cycle changes the
active member of the stack that holds the fullscreen tile, the new
active member becomes the fullscreen one. Exiting is unchanged. Every
other stack mutation that changes the layout — `stack_after` (an add or
a centre drop onto a fullscreen tile) and `pop_out` (move-out, unstack)
— exits fullscreen first, the rule `split` already follows: an explicit
layout operation trumps a stale fullscreen (rulings 4 and the
whole-branch review, 2026-09-19).

**Split beside.** `Tree::split` on a focused member wraps the *stack*
in the new split, not the member: adding a tile horizontally beside a
stack puts it beside the whole stack. `toggle_split_orientation` acts on
the split above the stack as it would above a leaf.

**Close.** `remove_focused` on a member removes it from `children`. If
it was active, the next member becomes active, or the previous one when
it was last. A stack left with one member collapses to `Leaf`.

**Move.** `move_direction` on a member does not swap; it pops the member
out of the stack and places it beside the stack in that direction (a
new split of that orientation, member on the far side), collapsing the
stack if one member remains. i3 does the same for moving out of a
container, and a swap between a member and a tile outside the stack has
no meaning a trader would expect. On a plain leaf the swap stands.

**Docks.** A dock's tree is the same `Tree`, so a stack can live in a
dock with nothing added. `Dock::focusable()` and the region invariant
are untouched.

## 4. Actions and keys

All in the `workspace` context, all registered in the shell.

| Action | Title | Default | Behaviour |
|---|---|---|---|
| `stack::next` | Stack: Next | `mod+]` | activate the next member, wrapping; a count prefix steps N |
| `stack::prev` | Stack: Previous | `mod+[` | the same, backwards |
| `stack::pick` | Stack: Pick… | none (palette) | open the list (§5.2) on the focused tile |
| `stack::unstack` | Stack: Unstack | none (palette) | pop the focused member out beside its stack in the `[tiles] add` direction (`AddDirection::resolve`, the add path's own rule), collapsing the stack if one member remains |

Every one of these is a no-op with a status notice (`not in a stack`)
on a tile that is not a member. A cycle moves tile focus to the new active member
(the ring follows) and re-arms `pending_focus_restore` through
`note_keyboard_focus_move`, since it moves which tile has focus while an
abandoned editor may still hold the keyboard, exactly as `mod+l` does.

`mod+[`/`mod+]` are free in the shipped map and in every module fragment
(user choice 2026-09-19, over `mod+n`/`mod+shift+n`). They pair with the
existing `ctrl+[`/`ctrl+]` dock toggles: ctrl brackets move between
docks, mod brackets move within the slot. Bare `[`/`]` were rejected
because the diagnostics tile's fragment already binds them to its
section cycle, and a tile context is deeper on the stack than
`workspace`, so the same key would mean two things depending on the
focused tile. `mod+tab` was rejected because `mod` is Alt on Windows.

Existing verbs whose behaviour on a member is defined in §3: `ctrl+w`
(close), `ctrl+alt+arrows` (move), `mod+f` (fullscreen), `shift+d` and
`ctrl+shift+d` (duplicate beside the stack, as a split beside does).

## 5. What the module sees

### 5.1 The marker

Two additions to `TileContent`, both **required** methods with no
default, so the compiler makes every occupant answer (blotter,
diagnostics, market-data panel, placeholder, the recording fixture and
the shell-test `WatchingContent`):

```rust
fn set_stack(&self, stack: Option<StackHandle>, cx: &mut App);
fn title(&self, cx: &App) -> SharedString;
```

`StackHandle` is a shell-built value the module stores and paints from:

```rust
pub struct StackHandle {
    pub index: usize,   // one-based, for painting
    pub len: usize,     // always >= 2 when Some
    open: Rc<dyn Fn(&mut Window, &mut App)>,
}
impl StackHandle {
    pub fn open_list(&self, window: &mut Window, cx: &mut App)
}
```

The closure captures the shell's own `WeakEntity<ShellView>` and the
`TileId`, so a module opens the list without a path to the shell, the
same layering `Frame` and the two globals keep. `set_stack` is called
from `ensure_occupants` after every layout change with the tile's
current `(index, len)`, or `None` when it is not a member, **skipping a
tile whose value is unchanged** (the shell keeps the last value it sent
per tile) so a scope keystroke's render does not re-notify every member.
A fresh occupant is told its stack position on the same first render
`set_visible` is delivered on, under the same contract.

The marker is one chip, painted first in the header strip at the
position the module's own layout gives it: text `{index}/{len}` in the
mono face, `Tone::Neutral` through `chip_paint` (it is a state the
trader chose, like `pinned`), theme radius, painted only while
`len > 1`. Its mouse-down calls `open_list` and stops propagation, so
the click that opens the list is not also a tile click-to-focus with a
drag arm behind it (the tile is already focused by the time the list
matters; `open_list` focuses it first regardless, §5.2). The placeholder
paints the marker too, centred beside its label, so a stack of two
placeholders is still navigable by mouse.

### 5.2 The list

Shell-owned, painted in `ShellView::render` in the palette's mould: an
instant overlay, backdrop-less, anchored at the focused tile's top-left
corner offset by the tile's header height on the design scale, so it
hangs just under the header on any module. Rows through
`listrow::row_paint`; the theme's `radius_lg` panel and
`popover_style`; width the widest title plus the index gutter, clamped
to the tile.

One row per member, in stack order: `{n}` gutter, `title()`, and the
kind dimmed on the right (`blotter`, `cvi`, `diagnostics`). The active
member is the highlighted row when the list opens. Keys, all owned by
the list while it is open (a modal-shaped surface, but no `Input`, so no
focus dance and no reclaimed bindings): `j`/`k`/`up`/`down` move (the
`vimnav::apply` rule, wrap on a bare ±1), `1`–`9` activate that member
at once, `enter` or a row click activates the highlighted one, `escape`
or a mouse-down outside closes without a change. Activating does what
`stack::next` does: the member becomes active and focused, the flag
re-arms.

Opening from `stack::pick` when the focused tile is not a member gives
the `not in a stack` notice and opens nothing. Opening from the marker
first focuses that tile (a click on an unfocused tile's marker must
open *that* tile's list). The list closes on any workspace mutation
underneath it (a reload restoring a session, a delivery is fine) and on
`ctrl+k`, which the palette's own open arm already does for the modal.

Titles: blotter `{view} · {grouping chain}` (`risk · book, lhu`, the
header's own title run); market-data `{kind} · {underlying}`
(`CVI · SPX.Z`, `CVI` alone with no underlying); diagnostics
`diagnostics · {section}`; placeholder `empty`. The unsent-edits dot the
mockup showed is **not built**: it needs a third question of every
occupant, and nothing else asks it yet.

## 6. Entry doors

### 6.1 Add by kind

`register_add_actions` registers a fourth row per roster kind,
`tile::add_{kind}_stacked`, titled `{Kind}: Stack`, category Tiles. It
routes through `ShellView::add_tile` with a new placement,
`Placement::Stacked`, resolved ahead of the direction:

- focused tile is a leaf or a member: the new tile is inserted
  **after** the focused one in its stack (a leaf becomes a two-member
  stack holding the two), becomes active, and takes focus, as a split
  focuses what it adds;
- focused tile is a placeholder, or the region is empty: the row does
  exactly what `{Kind}: Split` does, filling the placeholder in place or
  becoming the region's root. A stack of one is meaningless, and a
  trader who asked to stack onto nothing wanted the tile.

`AddDirection` and `[tiles] add` are untouched; a stacked add never
reads them.

### 6.2 Drop onto the centre

`DropZone::Center` changes meaning from swap to **add to the target's
stack**: the dragged tile leaves its slot with `remove`'s collapse rules
(its own stack collapses if one member remains; a split does as today),
is inserted after the target in the target's stack (a leaf target
becomes a two-member stack), and becomes active and focused. `drop_swap`
is replaced by `drop_stack`; the drop verbs already locate the target
by id, so a cross-region drop (dock to main, main to dock) works with
nothing added. Dropping a member onto another member of the same stack
**reorders** it to sit after the target, which is the only reorder the
feature has and the mouse gets it for free. Dropping a tile onto itself
is refused as today. Keyboard move keeps its swap on a plain leaf.

The centre highlight is the whole rect, unchanged.

## 7. Session

The `layout` table gains a third node shape beside `leaf` and `split`:

```toml
[[workspaces.1.node.children]]
kind = "stack"
members = [4, 7, 9]
active = 1
```

Written from `Node::Stack` verbatim through the shared `node_to_toml`
seam, so a dock's tree carries it too. On read: a `members` list shorter than two
after unknown ids are dropped collapses to a `leaf` of the survivor or
to nothing; an `active` that is missing, negative or out of range clamps
to `0`; a stack member duplicating an id already claimed earlier in
document order — by an earlier leaf, or by an earlier stack's own
member — is dropped from the stack. That rule is one-directional, not
"first occurrence wins" both ways: a later LEAF duplicating an earlier
stack member is *kept*, because `validate_node`'s leaf arm never checks
against what it has already seen — the pre-existing rule for leaves,
unchanged here, and only a stack's own arm dedupes on the way in. A
hostile file therefore never produces a stack the tree's own verbs
could not have built. `Tree`'s derived `PartialEq` stays, so
`session.rs`'s dock-table skip is unaffected, and
`SESSION_CONFIG_VERSION` is unchanged — but the two trees an older
reader can meet `kind = "stack"` in diverge sharply. In a workspace's
MAIN tree, `node_from_toml`'s "unknown node kind" error propagates
through `parse_workspace`'s `?` into `from_toml`'s `errors`, so
`from_toml` returns `Err` and `load` answers a wholly fresh session:
every workspace, every tile record and the palette usage history are
discarded, and the next periodic flush overwrites the file with that
empty state. In a DOCK tree the same error is caught inside
`parse_docks`'s own `match node_from_toml { .. }` arm, which only warns
and drops that one dock's tree — the main tree, the workspace's other
docks and every other workspace survive intact.

Hidden members are serialised through `serialize` like any tile; there
is nothing stack-specific in a tile record.

## 8. Rendering and the visible set

`ensure_occupants` derives the visible set from `Tree::layout` as it
does now, so hidden members fall out of it with no new code, receive
`set_visible(false)`, and stop requerying; a member made active is told
`set_visible(true)` and requeries if stale, the existing contract. A
hidden member's `AnyView` is never rendered, so no per-frame cost
accrues to a stack beyond its active member. The render loop paints the
active member in the stack's rect through the same `tile_cell`; there is
no stack chrome in the shell at all, by design.

The which-key hints and the status bar are untouched.

## 9. Tests

Test weight follows the code: pure tree first.

**`tiling/tree.rs`** (pure, no window): layout returns only the active
member's rect over the whole node; `next`/`prev` wrap, and a count
steps N with the same wrap; `focus(id)` on a hidden member activates it;
close of the active member activates the next, of the last the
previous, of one of two collapses to a leaf, of a hidden member leaves
the active one alone; `move_direction` on a member pops it out beside
the stack in that direction and collapses the stack when one remains;
`split` beside a member wraps the stack; fullscreen follows a cycle;
`toggle_split_orientation` above a stack acts on the split.

**`session.rs`**: round trip of a stack in the main tree and in a dock;
the three hostile-restore cases of §7.

**`shell/tests/`** (window tests, `test-support`): `set_stack` is
delivered to every member with the right `(index, len)`, once per change
and not on an unrelated render; hidden members get `set_visible(false)`
and the active one `true`; a `{Kind}: Stack` row on a leaf, on a
member, on a placeholder and in an empty region each produce the §6.1
tree; a centre drop on a leaf, on a member, on a member of the dragged
tile's own stack, and across regions each produce the §6.2 tree; the
list opens from `stack::pick` and from the handle, switches on a digit,
on `enter` and on a row click, closes on `escape`, an outside click and
`ctrl+k`, and refuses to open on a non-member with the notice; a cycle
re-arms the focus restore when an abandoned editor holds the keyboard.

**Modules**: each occupant's `title()` for the cases in §5.2, and the
marker's presence keyed on `len > 1` (the blotter's and the panel's
header tests, the diagnostics tile's row test).

**Mutation harness**: one entry per behaviour above naming its test;
`--anchors-only` before merge.

**Display checks**, pending on the user's screen: the marker in each of
the three module headers and on the placeholder; the list's anchor under
the header; the ring following a cycle.

## 10. Docs

`CLAUDE.md` gains a stacks paragraph (the model, the two trait methods,
the `set_stack` skip rule, the centre-drop meaning change). The
drop-zone module's header note is rewritten to record the change as
made. Foundation §3.1's "tabbed/stacked container modes" line points
here.

## 11. Sequencing

1. Pure tree: the variant, layout, focus, close, move, split, fullscreen,
   cycle verbs, tests.
2. Session read and write, tests.
3. Shell: the four actions and bindings, `set_stack` delivery from
   `ensure_occupants`, `StackHandle`, the `title` method, the list
   overlay, tests.
4. Entry doors: the stacked add row and placement, `drop_stack`
   replacing `drop_swap`, tests.
5. Modules: marker and `title` on blotter, market-data, diagnostics,
   placeholder and the fixtures.
6. Harness entries, docs, display check.
