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
| Edge drop | Insert beside the target's slot, within or across regions |
| Centre drop | Insert after the target in its stack; a plain target becomes a stack |
| Dock-background drop | Insert into that dock; dropping into the source dock does nothing |

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
main and dock trees. A switch generation advances only when the active index
changes, allowing a gesture to detect switching away and back between renders.

Restoration validates main-tree structure and repairs local dock conflicts as
described in [session recovery](shell.md#recovery-and-restoration). Raw tree
validation removes repeated/already-claimed stack members but does not reject
plain duplicate leaf IDs; workspace recovery prunes dock conflicts, not
duplicates between main trees. Live operations assume unique IDs. Restored
IDs seed allocation at the maximum observed value; the allocator has no
exhaustion check. Session serialization additionally requires IDs to fit a
signed TOML integer to round-trip.
