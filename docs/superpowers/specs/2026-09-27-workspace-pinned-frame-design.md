# Workspace-pinned frame — design

Date: 2026-09-27. Status: approved in conversation, awaiting spec review.

## 1. Intent

The frame's scope, grouping, and as-of are app-wide today: every workspace
shows the same selection. A user wants one workspace to opt out — to look at
a different book, grouping, or point in time — without disturbing the others.

A toolbar pin toggles this per workspace. While a workspace is pinned, scope,
grouping, and as-of changes made in it stay in it, and changes made elsewhere
do not reach it. An unpinned workspace behaves exactly as today.

Success: a user pins workspace 2, narrows its scope and switches it to
yesterday's close, returns to workspace 1 and finds it unchanged and live;
restarting the app restores both states; unpinning workspace 2 returns it to
the shared state.

## 2. Rulings

1. The pin covers scope, grouping (active slot), and as-of together. They are
   the flip barrier's identity and move as one. No per-part pins.
2. Unpinning discards the workspace's local state without a confirm and
   rejoins the shared state. It does not promote local state to shared.
3. Definitions stay shared: named expressions, saved scopes, grouping slot
   contents, and publications. Only the *selection* is per lane.
4. `frame::pin_workspace` ships without a default binding; the palette and the
   toolbar glyph are its routes.
5. Approach: lanes inside the one `Frame` entity (§4). Rejected: one `Frame`
   entity per pinned workspace (pinning would have to rebind every tile in
   the workspace through a new `TileContent` method), and one frame per
   workspace with mirrored shared state (two sources of truth).

## 3. Interaction

### 3.1 The control

A pin glyph at the toolbar's leading edge, before the scope segment, built on
the `shell::control` door (hover and pressed fills, arrow cursor).

- Off: muted. Tooltip "Pin the frame to workspace N".
- On: the Neutral chip tone (`chip::Tone::Neutral`, the shell's tone for a
  user-selected state with no hazard). Tooltip "Frame pinned to workspace N — scope, grouping,
  and as-of changes stay here. Click to rejoin the shared frame."

The glyph is the pinned indicator and follows the active workspace. User-
facing text always says "pinned to workspace N" so it does not read as the
blotter's tile-level "pinned grouping".

### 3.2 Keyboard route

`frame::pin_workspace`, Frame palette category, titled "Toggle the frame pin
for this workspace" (palette titles are static; the glyph shows the state).
Unbound by default.

### 3.3 Pin and unpin

- Pin: the workspace gets its own lane, a copy of the shared lane's scope,
  active slot, and as-of (and their generations, §4.2), with empty scope undo,
  redo, and previous as-of. Tiles see identical values and identical
  generations, so nothing requeries.
- Unpin: the lane is dropped; the workspace resolves to the shared lane. Tiles
  requery only where the content differs (§4.2).
- Pin and unpin are not scope-undo entries.

### 3.4 What targets the lane

Every surface that acts on "the frame" acts on the active workspace's lane:
the scope bar (chips, `×`, `+`, save glyph), `frame::pick`,
`frame::add_expression`, `frame::scope_expression`, `frame::as_of`,
`frame::live`, `frame::as_of_undo`, `frame::grouping`, `frame::focus_text`,
`frame::slot_1`–`9`, `frame::slot_clear`, `frame::scope_undo`,
`frame::scope_redo`, `frame::scope_clear`, and the Scopes dialog's save-current
and load actions. Switching workspace repaints the toolbar from the new
workspace's lane.

Saving a scope or a grouping slot from a pinned workspace writes the shared
`scopes.toml` / `groupings.toml` as today (ruling 3). `:` commands are
tile-local and unchanged.

### 3.5 Layering

Tile overrides compose on the workspace's lane exactly as they compose on the
shared frame today: blotter pinned grouping, unscoped mode, local as-of, and
`TileAsOf`. Order: shared lane or pinned lane → tile.

## 4. Architecture

### 4.1 `Frame` and `Lane`

`Frame` remains one GPUI entity. It keeps, once:

- slot definitions (`GroupingSlots`) and the pending slot persist;
- saved scopes and the pending scope persist;
- named expressions;
- publications: watches, recent publishes;
- the data, config, saved-scopes, and flip versions;
- requery stats, `user_dir`, the flip barrier, and the scope-bar cache;
- the generation counter (§4.2).

A new `Lane` holds the per-selection state:

- scope, scope undo/redo, scope-editing session;
- active slot;
- as-of and previous as-of;
- its scope, grouping, and as-of generations.

`Frame` holds `shared: Lane` and `pinned: HashMap<WorkspaceIx, Lane>`, where
`WorkspaceIx` is a newtype over the workspace index (1–9). `lane(ws)` returns
the pinned lane if present, else the shared lane. `pin(ws)` and `unpin(ws)`
are frame methods; `is_pinned(ws)` answers the toolbar and palette label.

Every existing lane-state method (`set_scope`, `undo_scope`,
`set_active_slot`, `set_as_of`, `drop_named`, `effective_scope`, `bar_model`,
…) takes or resolves a `WorkspaceIx`. Methods over shared state keep their
signatures.

### 4.2 Generations name values

Scope, grouping, and as-of generations are allocated from one frame-wide
counter, not per lane. Invariant: **a generation number names exactly one
value, in any lane.**

- Pin copies the shared lane's values *and* generations: the content is
  equal, so equal numbers are truthful.
- Every subsequent edit, in any lane, takes a fresh number from the counter.
- After unpin, a tile's last-seen generation equals the shared lane's only if
  the content is equal. Tiles keep their one-integer compare per field, and a
  lane switch can never read as "no change" when the content differs.
- `replace_slots` (a `groupings.toml` reload) gives a fresh grouping
  generation to every lane, pinned ones in hidden workspaces included, as it
  bumps the one grouping counter today, and clears an active slot that
  vanished in any lane. `save_slot` gives a fresh grouping generation to every
  lane whose active slot is the saved one.

`FrameVersions` keeps its shape; `versions(ws)` fills scope/grouping/as-of
from `lane(ws)` and the rest from the shared fields. `versions_for` likewise
takes the workspace.

### 4.3 The flip barrier

Only the active workspace's tiles are visible, so at most one lane can need a
visible flip. The barrier stays single. `ShellView::on_frame_changed`
compares the **active** lane's versions against `last_flip_versions` and
opens the barrier with the visible keys, as today. A workspace switch
re-seeds `last_flip_versions` from the new active lane without opening a
barrier. Because generations are unique across lanes, a barrier's identity
cannot match a tile evaluating another lane.

A change to a hidden lane (another workspace's pinned lane, or the shared lane
while the active workspace is pinned) opens no barrier; its tiles are hidden
and catch up when shown.

### 4.4 `FrameRef` at the module boundary

`ModuleFactory::create` takes `frame: FrameRef` instead of `Entity<Frame>`:

```rust
#[derive(Clone)]
pub struct FrameRef { entity: Entity<Frame>, ws: WorkspaceIx }
```

- `FrameRef::read(cx) -> FrameView<'_>` pairs `&Frame` with the resolved
  `&Lane` and exposes the method names modules already use: `versions`,
  `versions_for`, `scope`, `as_of`, `active_slot`, `active_grouping`, `slots`,
  `effective_scope`, `barrier_wants`, `named_expressions`, `saved_scopes`,
  `recent_publishes`, `expression_term_is`, `bar_model`, `requery`. Most
  module call sites stay textually `self.frame.read(cx).scope()`.
- `FrameRef::update(cx, |f, cx| …)` gives a `FrameViewMut` for the mutations
  modules perform (`arrived`, `watch_publications`, requery stats).
- `FrameRef::entity()` is what a tile observes. A change in any lane wakes
  every tile; the version compare filters it.

The shell creates each occupant with its own workspace's `FrameRef`. A tile
never leaves its workspace (stacks, docks, fullscreen, and pop-out all stay
within one), so the reference is fixed for the tile's life. Shell surfaces
build a `FrameRef` for the active workspace.

## 5. Persistence

`session.toml`:

| Record | Contents |
|---|---|
| `frame` | Shared lane, unchanged format |
| `workspaces.N.frame` | Pinned lane for workspace N, same `FrameRecord` format; present iff pinned |

Restore follows the shared frame's recovery rules: unusable fields warn and
are dropped, the rest is kept. A non-table `workspaces.N.frame` warns and the
workspace restores unpinned. A rejected session drops everything, as today.
Every lane's undo history starts empty. Pin, unpin, and lane edits mark the
session dirty through the existing frame-version path; no session I/O on the
UI thread.

## 6. Edge cases

- Workspace switch during a scope text session: the session ends as it does
  on focus loss; the text field resyncs to the new active lane.
- Frame dialogs (pick, as-of, expression, grouping, Scopes save/load) target
  the workspace recorded on the modal stack's base entry
  (`ShellModal::workspace`, set at push). Every shell frame read or write goes
  through one door, `ShellView::target_frame()`: the base modal's workspace
  while a modal is open, else the active workspace. Modality already prevents
  a switch in between; the recorded target makes it explicit and testable.
- A pinned workspace with no tiles keeps its lane.
- Named-expression edits reach every lane (definitions are shared). A missing
  or invalid name is still a per-query refusal in whichever lane references it.
- The as-of dialog's publication rows are shared; its current/previous values
  come from its target lane.

## 7. Out of scope

Moving tiles between workspaces; promote-to-shared on unpin; per-part pins; a
default binding.

## 8. Testing

Lowest layer first; production routes for interaction.

- Pure `Frame`: pin copies values and generations with empty history; an edit
  in either lane takes a fresh unique generation; unpin resolves to shared;
  undo/redo and previous as-of isolated per lane; `replace_slots` bumps every
  affected lane including hidden ones and leaves unaffected lanes alone.
- GPUI shell tests: glyph click and palette dispatch both toggle; a scope edit
  in pinned workspace 1 does not change the versions an occupant in workspace
  2 observes, and vice versa; after unpin the occupant requeries iff content
  differs; a workspace switch re-seeds the barrier without opening a flip; the
  scope text field resyncs on switch; slot keys and `mod+z` target the active
  lane.
- Session: round-trip of a pinned lane; non-table recovery; partial record.
- Mutation-harness entries: lane resolution, generation freshness on edit,
  `replace_slots` cross-lane bump, barrier re-seed on switch, pinned restore.
- Display check: pin glyph in both states, light and dark.

## 9. Documentation

Same change: `docs/current/shell.md` (shared-frame section, session table),
`docs/current/architecture.md` (`FrameRef` at the module boundary),
`docs/current/input-and-dialogs.md` (frame dialog targets), the geode-shell
README, and the TODO.md scoping item.
