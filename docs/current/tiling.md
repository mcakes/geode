# Tiling and workspaces

The pure layout model in [`tiling`](../../crates/geode-shell/src/tiling/mod.rs)
owns tree structure and structural focus. The shell owns occupants, window
focus, and gesture lifetimes. The model has no GPUI dependency.

## Trees and regions

Each workspace has a main tree, three dock trees, and a focused region. A
tree consists of leaves, splits, and stacks. Splits divide a rectangle by
orientation and ratios; stacks occupy one rectangle while retaining all
members and showing only the active one. Focusing a stack member activates
it. If the outgoing active member was fullscreen, fullscreen transfers to the
newly active member.

`Tree::layout` supplies visible tile rectangles. Rendering and directional
navigation use its subdivision rules. `tiles()` includes inactive stack
members for retention and persistence; `visible_tiles()` selects active
members but does not itself apply the fullscreen filter used by `layout`.
`slot_count()` counts the slots `layout` would paint unmaximised, without
allocating. `Workspace::fullscreen_hidden` uses it to report how many
tiles a fullscreen main-tree tile hides: the tree's other slots plus
every visible dock's slots, since fullscreen paints no dock. It returns
`None` when nothing is fullscreen, and `Some(0)` for a lone maximised tile.

Visible docks reserve space even when empty. Left and right consume fractions
of the full content width; bottom consumes a fraction of the full height in
the remaining central width. Sizes default to 0.25 and clamp to 0.10–0.50.
Hidden docks retain layout and focus memory. Dock trees cannot be fullscreen.

## Focus and movement

Directional focus looks for an overlapping neighbor on the requested side,
preferring the closest edge, then greatest overlap, then tree order. Navigation
runs within the selected region first. At a main-tree edge, it may enter a
visible occupied dock on that side. Main fullscreen blocks this crossing.
At a dock edge, only the inward direction crosses back to main; if main is
empty, left and right may cross to the opposite occupied visible dock.

Explicitly showing a dock focuses it even when empty and exits main fullscreen.
Clicking an empty visible dock also permits adding a tile there. Hiding the
focused dock chooses main when occupied, otherwise the first visible occupied
dock in left/right/bottom order, otherwise main. Closing an already-empty
focused dock does nothing; removing its last tile hides it.

| Operation | Result |
|---|---|
| Split/add | Insert beside focus, or create the first leaf; focus the new tile |
| Directional move of a plain tile | Swap with a geometric neighbor within the current region |
| Directional move of a stack member | Pop it out beside its stack in the requested direction |
| Move to a named dock | Insert at that dock's focus and show it; if already focused there, send one tile back to main |
| Close | Remove focus, retaining focus in a surviving stack or selecting a tree-order neighbor |
| Close by id (a tile's ×) | The focused tile closes as Close does; any other tile is removed and focus stays where it was; an emptied dock hides, and focus falls back only if that dock held it |
| Edge drop | Insert beside the target's slot, within or across regions |
| Centre drop | Insert after the target in its stack; a plain target becomes a stack |
| Dock-background drop | Insert into that dock; dropping into the source dock does nothing |
| Pull (`mod+shift+h/j/k/l`) | Move the visible neighbor in that direction into the focused tile's slot; a plain tile becomes a stack |
| Split a stack (`mod+s`) | The focused member's stack becomes a split in its own slot |

Pull names a tile on screen rather than a node in the tree, so its result
never depends on how the layout was nested. The neighbor is the nearest
visible tile that way within the focused region, chosen by the same geometry
as directional focus; a stacked neighbor
gives up only its visible member. Focus stays on the focused tile, which stays
active. Stack order follows the screen: a tile pulled from the left or above
goes first and one from the right or below goes last, so repeated pulls sweep
a row or column in reading order from either end. With no neighbor that way —
an edge, a lone tile, or a fullscreen tile, which has no visible neighbor — it
refuses with a notice.

Splitting a stack keeps member order, gives equal ratios, and takes its
orientation from the configured add direction resolved against the stack's
slot, as `stack::unstack` does. The stack's previous split ratios and nesting
are not remembered, so pulling tiles in and splitting them out again yields an
equal flat split. It clears fullscreen, and a plain tile refuses with a
notice.

Matching-orientation insertion adds a sibling and equalizes sibling ratios.
Otherwise it wraps the anchor slot in a half-and-half split. A member used
as a split anchor represents its whole stack. Removal collapses one-child
containers and renormalizes remaining split ratios. Transfers retain the
tile ID, hide an emptied source dock, and focus the destination.

## Resizing and drop geometry

Keyboard resizing moves the adjacent ratio pair at the nearest ancestor with
the requested orientation, preserving its total. A step below the minimum
ratio is refused. In a dock, any refusal to move an internal divider falls
back to resizing the dock frame on the same press, including refusal at the
internal divider's size limit.

Divider dragging uses absolute coordinates, preserves the adjacent pair's
sum, and clamps at the minimum ratio. Invalid paths, nonpositive extents, and
non-finite positions on the relevant axis are refused. Repeated positions
within 1e-6 ratio units report no change. Dock divider/edge drags refuse
hidden docks; applied resize steps remain in place when a gesture stops.
Callers supply finite layout bounds in the cursor's coordinate space; the
divider mutation does not independently reject non-finite bounds.

A `DividerAddress` contains child indices and a boundary index, not a node
identity. A structural edit can leave a valid address pointing at a different
boundary. The shell's gesture checks must therefore consider workspace and
layout context as well as whether the address resolves.

Drop targets use the outer quarter of each tile dimension as edge bands;
the central half in each dimension is the stack zone. Corners choose the
nearest edge by absolute distance, with ties left, right, up, then down.
Dock frames take priority over main tiles; a point in a dock outside its
tile rectangles targets the background. Highlighting and release share
`resolve_drop_target`; release recomputes geometry from the current model.
The release helper refuses fullscreen layouts and non-finite coordinates.

Drop methods check endpoint membership before removing the source and keep
a split fallback if destination insertion refuses. A duplicate ID already
in a different destination tree triggers a debug assertion or an unchanged
refusal in release builds. Their successful return means the operation was
accepted; reinserting a member at its existing stack position can still
return true.

## Workspace identity and restoration

`Workspaces` stores indices 1–9. Switching creates an absent workspace and
retains empty ones. Its tile allocator is shared across that collection's
main and dock trees.

### Windows, shown and hosted

One `Workspaces` serves every window of the app. Each window, named by its
`WindowKey`, *shows* one index and *hosts* the indexes whose occupants it
builds and keeps: the index it shows, every index it showed and switched
away from (hidden in that window until another takes it), and, for the
first window of the process, every index the restored session materialised,
since no other window exists to build them. A later window hosts only the
index it opens on. An index is shown by at most one window; a tile's window
(`window_of`) is its workspace's host, which need not be showing it. A
window's own builders (`split_in`, `stack_in`, `workspace`,
`workspace_mut`, the keyed `apply_workspace_action`) act on the workspace it
shows; its occupant set is `hosted_by`.

`add_window(key, n)` opens a window on `n`, refusing an index outside 1–9 or
shown by another window, and reports whether `n` was hidden in another
window (`TakenFrom`) so the caller can move its occupants. The first window
opens on the session's initial index, later ones on `lowest_free`, the
lowest of 1–9 no window shows, whether or not it has been created. Nine
windows exhaust it. `remove_window` returns the indexes the window hosted,
now unhosted; the workspaces themselves stay stored.

`switch(key, n)` has four outcomes. `Unchanged`: the window already shows
`n`. `Raise(other)`: another window shows `n`; nothing changes and the
caller brings that window forward instead. `Moved { from }`: another window
hosted `n` hidden; this window now shows and hosts it and the caller moves
the occupants. `Shown`: `n` was free, hidden in this same window, or not yet
created; it is created if needed and shown and hosted here. The index left
behind stays hosted by the window that showed it. `Invalid` names an index
outside 1–9 or a window that is not open. `workspace::switch_*` is not the
action router's (`apply_workspace_action` leaves it unhandled): the shell
dispatches it through `ShellApp::switch` and acts on the outcome. `Raise`
asks the other window forward; `Moved` tells the window it came from to
park its occupants for the index, which the new host rebuilds from their
records (see [workspaces moving between
windows](shell.md#workspaces-moving-between-windows)); `Shown` and `Moved`
re-seed the window's flip baseline and pull any records already parked for
what it now hosts; `Unchanged` reconciles focus as any recognised workspace
action does; `Invalid` does nothing.

Each window has its own switch generation (`switch_epoch`), advanced only by
`Shown` and `Moved`, allowing a gesture to detect switching away and back
between renders. The session file's `active` is `initial_index`: what the
oldest open window shows, so a restart reopens there, or, before any window
opens, what the file said.

Restoration validates main-tree structure and repairs local dock conflicts as
described in [session recovery](shell.md#recovery-and-restoration). Raw tree
validation removes repeated/already-claimed stack members but does not reject
plain duplicate leaf IDs; workspace recovery prunes dock conflicts, not
duplicates between main trees. Live operations assume unique IDs. Restored
IDs seed allocation at the maximum observed value; the allocator has no
exhaustion check. Session serialization additionally requires IDs to fit a
signed TOML integer to round-trip.
