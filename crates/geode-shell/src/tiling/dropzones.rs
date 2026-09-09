//! Drop-zone geometry for mouse-driven tile movement (tile-drag task).
//! Pure functions only — no gpui (spec §10.3), same as the rest of
//! `tiling/`. While a mod+drag is in flight, the render pass and the
//! mouse-up handler both need one question answered: *what would dropping
//! here mean?* This module answers it from geometry the caller already has
//! (or re-derives once, at drop time, through the same pure layout
//! authorities the render pass uses — `dock_layout` + `Tree::layout`).
//!
//! **Zone model** (recorded choices from the approved design):
//! - Each tile rect divides into five zones: an edge band on each side —
//!   the outer 25% of the rect's extent along that axis
//!   ([`DROP_EDGE_BAND`]) — and the remaining center (the middle 50%×50%).
//! - A point falling in two bands at once (a corner) resolves to the
//!   *nearest edge by absolute distance* — the edge the cursor has
//!   penetrated deepest toward, measured in the caller's units (pixels in
//!   the shell), not normalized per-axis; exact ties break in the fixed
//!   order Left, Right, Top, Bottom. Absolute distance was chosen over
//!   per-axis normalization so the visual meaning is stable ("the edge my
//!   cursor is closest to"), even in very elongated tiles.
//! - A cursor inside a visible dock's frame but not over any of its tiles
//!   is the *dock background* target. Because a dock tree's layout tiles
//!   the whole frame, this is in practice the empty-dock case (the "move a
//!   tile here" hint area) — but the classification is geometric, not
//!   state-aware, so it stays correct if dock chrome ever leaves gaps.
//!
//! **Center-drop semantics note (recorded)**: today a center drop *swaps*
//! the two tiles in place — deliberate keyboard parity with
//! `workspace::move_*`'s swap semantics. When spec §3.1's tabbed/stacked
//! container modes are built (segmented-title-row direction chosen),
//! center-drop is planned to become "add to the target's stack"; that
//! meaning change is accepted in advance rather than designing a reserved
//! zone for it now.

use super::docks::layout as dock_layout;
use super::tree::{Direction, Rect, TileId};
use super::workspaces::Workspace;

/// Fraction of a tile rect's extent that each edge band occupies (outer
/// 25% per side, leaving the middle 50%×50% as the center zone).
pub const DROP_EDGE_BAND: f32 = 0.25;

/// One visible dock's drop geometry, owned: its side, frame rect, and
/// laid-out tiles — what [`locate_drop_target`] (and the tests) build
/// eagerly before borrowing slices of it into [`resolve_drop_target`].
/// (The core itself takes borrows, not this owned shape, so the render
/// pass can feed it per-frame without allocating.)
type OwnedDockCells = Vec<(super::docks::DockSide, Rect, Vec<(TileId, Rect)>)>;

/// What part of a tile the cursor is over: the center, or one of the four
/// edge bands. Edge directions use the same [`Direction`] vocabulary as
/// navigation (`Up` = the top band, `Down` = the bottom band).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropZone {
    Center,
    Edge(Direction),
}

/// A resolved drop target: a specific tile (with the zone within it), or
/// a visible dock's background. Deliberately carries no region — the drop
/// verbs on [`Workspace`] locate the target tile by id themselves, so a
/// target can never be applied against the wrong tree via a stale region.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DropTarget {
    Tile { id: TileId, zone: DropZone },
    DockBackground { side: super::docks::DockSide },
}

/// Point-in-rect with the half-open convention every hit test here uses
/// (inclusive left/top edge, exclusive right/bottom — adjacent rects from
/// one layout never both claim a boundary point). Public so the render
/// pass can test a cursor against dock frame rects it already has without
/// restating the convention.
pub fn rect_contains(r: &Rect, x: f32, y: f32) -> bool {
    x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
}

/// The first tile rect containing the point (tile rects from one layout
/// are disjoint, so "first" is "the one"). `None` when the point is in
/// none of them.
pub fn hit_tile(rects: &[(TileId, Rect)], x: f32, y: f32) -> Option<(TileId, Rect)> {
    rects
        .iter()
        .find(|(_, r)| rect_contains(r, x, y))
        .map(|&(id, r)| (id, r))
}

/// Classify a point *inside* `rect` into its five-zone position. Corner
/// resolution per the module doc: nearest edge by absolute distance, ties
/// in Left/Right/Top/Bottom order (implemented by strict `<` comparison
/// over candidates considered in that order). A point outside every band —
/// or inside a degenerate zero-extent rect — is `Center`.
pub fn classify_drop_zone(rect: Rect, x: f32, y: f32) -> DropZone {
    let band_x = rect.w * DROP_EDGE_BAND;
    let band_y = rect.h * DROP_EDGE_BAND;
    let candidates = [
        (x - rect.x, band_x, Direction::Left),
        (rect.x + rect.w - x, band_x, Direction::Right),
        (y - rect.y, band_y, Direction::Up),
        (rect.y + rect.h - y, band_y, Direction::Down),
    ];
    let mut best: Option<(f32, Direction)> = None;
    for (distance, band, dir) in candidates {
        if distance < band && best.is_none_or(|(d, _)| distance < d) {
            best = Some((distance, dir));
        }
    }
    match best {
        Some((_, dir)) => DropZone::Edge(dir),
        None => DropZone::Center,
    }
}

/// The sub-rect a zone highlight should paint: an edge zone highlights
/// the half of the target tile the insert would occupy; center highlights
/// the whole tile. (The dock-background highlight is the dock's own frame
/// rect — the caller already has it, no helper needed.)
pub fn drop_highlight_rect(rect: Rect, zone: DropZone) -> Rect {
    match zone {
        DropZone::Center => rect,
        DropZone::Edge(Direction::Left) => Rect {
            w: rect.w / 2.0,
            ..rect
        },
        DropZone::Edge(Direction::Right) => Rect {
            x: rect.x + rect.w / 2.0,
            w: rect.w / 2.0,
            ..rect
        },
        DropZone::Edge(Direction::Up) => Rect {
            h: rect.h / 2.0,
            ..rect
        },
        DropZone::Edge(Direction::Down) => Rect {
            y: rect.y + rect.h / 2.0,
            h: rect.h / 2.0,
            ..rect
        },
    }
}

/// The one shared answer to "what would dropping here mean", over
/// caller-supplied rects (post-merge review cleanup 8 — before this
/// extraction the render pass's highlight closure and
/// [`locate_drop_target`] each restated the dock-frame → dock-tile →
/// dock-background → tree-tile resolution order, and two hand-kept
/// copies of a targeting rule is exactly how a highlight ends up
/// promising a drop the release won't perform). Both call sites now
/// feed this core:
///
/// - [`locate_drop_target`] wraps it with geometry re-derived from the
///   live layout authorities, once per drop;
/// - the render pass (`ShellView::render`'s drop-highlight closure)
///   feeds it the dock cells and tree rects its single layout pass
///   already computed — the core takes borrowed slices through a plain
///   iterator precisely so that per-frame call allocates nothing
///   (PHILOSOPHY.md: per-frame heap churn is a defect).
///
/// Returns the resolved target plus the rect it resolved in — the hit
/// tile's rect, or the dock's frame rect for a background hit — because
/// the render caller needs that rect to paint the zone highlight
/// ([`drop_highlight_rect`] over it) and re-deriving it would mean a
/// second hit test. Resolution order is part of the contract: dock
/// frames are checked before the main tree's tiles (dock rects are
/// carved OUT of the tree area so they never overlap it, but a dock
/// frame containing the point must claim it even when no dock tile
/// does), and within a dock a tile hit beats the background. A
/// non-finite cursor falls out naturally — `rect_contains` is false for
/// NaN/inf against every rect.
pub fn resolve_drop_target<'a>(
    docks: impl IntoIterator<Item = (super::docks::DockSide, Rect, &'a [(TileId, Rect)])>,
    tree_tiles: &[(TileId, Rect)],
    x: f32,
    y: f32,
) -> Option<(DropTarget, Rect)> {
    for (side, frame, tiles) in docks {
        if rect_contains(&frame, x, y) {
            return Some(match hit_tile(tiles, x, y) {
                Some((id, tr)) => (
                    DropTarget::Tile {
                        id,
                        zone: classify_drop_zone(tr, x, y),
                    },
                    tr,
                ),
                None => (DropTarget::DockBackground { side }, frame),
            });
        }
    }
    hit_tile(tree_tiles, x, y).map(|(id, tr)| {
        (
            DropTarget::Tile {
                id,
                zone: classify_drop_zone(tr, x, y),
            },
            tr,
        )
    })
}

/// Resolve the drop target under an absolute cursor position, re-deriving
/// the frame's geometry from the same pure authorities the render pass
/// uses (`dock_layout` carves the dock frames, each tree's own
/// `Tree::layout` places its tiles) and handing it to the shared
/// [`resolve_drop_target`] core. Called once per *drop* (mouse-up) — not
/// per frame: the render pass calls the core directly with the rects its
/// own single layout pass already computed, so this composition never
/// runs per-frame (and the eager per-dock layouts built here are a
/// per-drop cost, not a per-frame one). Deriving from live state at drop
/// time (rather than a snapshot captured at mouse-down) is deliberate:
/// the keyboard stays hot during a drag, so a tile added mid-drag
/// changes the layout — the drop must land on the layout the user *sees*
/// at release.
///
/// `None` — no target, drop is a no-op — for a cursor outside every tile
/// and dock, a non-finite cursor, or a fullscreen layout (no tile drag
/// can be in flight during fullscreen anyway; the shell gates arming and
/// cancels on fullscreen mid-drag, but a pure function shouldn't rely on
/// that).
pub fn locate_drop_target(workspace: &Workspace, area: Rect, x: f32, y: f32) -> Option<DropTarget> {
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let tree = workspace.tree();
    if tree.fullscreen().is_some() {
        return None;
    }
    let (tree_area, dock_rects) = dock_layout(workspace.docks(), area);
    let dock_cells: OwnedDockCells = dock_rects
        .into_iter()
        .map(|(side, r)| (side, r, workspace.docks().get(side).tree().layout(r)))
        .collect();
    resolve_drop_target(
        dock_cells
            .iter()
            .map(|(side, r, tiles)| (*side, *r, tiles.as_slice())),
        &tree.layout(tree_area),
        x,
        y,
    )
    .map(|(target, _)| target)
}

#[cfg(test)]
mod tests {
    use super::super::docks::DockSide;
    use super::super::tree::Orientation;
    use super::super::workspaces::Workspaces;
    use super::*;

    const RECT: Rect = Rect {
        x: 100.0,
        y: 200.0,
        w: 400.0,
        h: 200.0,
    };

    #[test]
    fn center_of_a_tile_classifies_center() {
        assert_eq!(classify_drop_zone(RECT, 300.0, 300.0), DropZone::Center);
        // Just inside the band boundary on every side is still center:
        // bands are the outer 25% (x: 100..200 and 400..500; y: 200..250
        // and 350..400).
        assert_eq!(classify_drop_zone(RECT, 201.0, 251.0), DropZone::Center);
        assert_eq!(classify_drop_zone(RECT, 399.0, 349.0), DropZone::Center);
    }

    #[test]
    fn each_edge_band_classifies_its_direction() {
        // Points deep in a single band, well clear of the perpendicular
        // bands (y centered for left/right, x centered for top/bottom).
        assert_eq!(
            classify_drop_zone(RECT, 110.0, 300.0),
            DropZone::Edge(Direction::Left)
        );
        assert_eq!(
            classify_drop_zone(RECT, 490.0, 300.0),
            DropZone::Edge(Direction::Right)
        );
        assert_eq!(
            classify_drop_zone(RECT, 300.0, 210.0),
            DropZone::Edge(Direction::Up)
        );
        assert_eq!(
            classify_drop_zone(RECT, 300.0, 390.0),
            DropZone::Edge(Direction::Down)
        );
    }

    #[test]
    fn corners_resolve_to_the_nearest_edge_by_absolute_distance() {
        // Top-left corner region: 2px from the left edge, 10px from the
        // top edge — left is nearer.
        assert_eq!(
            classify_drop_zone(RECT, 102.0, 210.0),
            DropZone::Edge(Direction::Left)
        );
        // Same corner, 30px from the left, 5px from the top — top wins.
        assert_eq!(
            classify_drop_zone(RECT, 130.0, 205.0),
            DropZone::Edge(Direction::Up)
        );
        // Bottom-right: 3px from the right, 20px from the bottom.
        assert_eq!(
            classify_drop_zone(RECT, 497.0, 380.0),
            DropZone::Edge(Direction::Right)
        );
    }

    #[test]
    fn exact_corner_ties_break_in_declaration_order() {
        // Equidistant from left and top (10px each): Left is considered
        // first and strict `<` means the tie stands — the recorded fixed
        // priority Left, Right, Top, Bottom.
        assert_eq!(
            classify_drop_zone(RECT, 110.0, 210.0),
            DropZone::Edge(Direction::Left)
        );
        // Equidistant from right and bottom: Right precedes Down.
        assert_eq!(
            classify_drop_zone(RECT, 490.0, 390.0),
            DropZone::Edge(Direction::Right)
        );
    }

    #[test]
    fn degenerate_rect_classifies_center() {
        let flat = Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        };
        assert_eq!(classify_drop_zone(flat, 0.0, 0.0), DropZone::Center);
    }

    #[test]
    fn highlight_rects_cover_the_insert_half_or_the_whole_tile() {
        assert_eq!(drop_highlight_rect(RECT, DropZone::Center), RECT);
        let left = drop_highlight_rect(RECT, DropZone::Edge(Direction::Left));
        assert_eq!(
            left,
            Rect {
                x: 100.0,
                y: 200.0,
                w: 200.0,
                h: 200.0
            }
        );
        let right = drop_highlight_rect(RECT, DropZone::Edge(Direction::Right));
        assert_eq!(
            right,
            Rect {
                x: 300.0,
                y: 200.0,
                w: 200.0,
                h: 200.0
            }
        );
        let top = drop_highlight_rect(RECT, DropZone::Edge(Direction::Up));
        assert_eq!(
            top,
            Rect {
                x: 100.0,
                y: 200.0,
                w: 400.0,
                h: 100.0
            }
        );
        let bottom = drop_highlight_rect(RECT, DropZone::Edge(Direction::Down));
        assert_eq!(
            bottom,
            Rect {
                x: 100.0,
                y: 300.0,
                w: 400.0,
                h: 100.0
            }
        );
    }

    #[test]
    fn hit_tile_finds_the_containing_rect_or_none() {
        let a = Rect {
            x: 0.0,
            y: 0.0,
            w: 50.0,
            h: 100.0,
        };
        let b = Rect {
            x: 50.0,
            y: 0.0,
            w: 50.0,
            h: 100.0,
        };
        let rects = vec![(TileId(1), a), (TileId(2), b)];
        assert_eq!(hit_tile(&rects, 10.0, 10.0), Some((TileId(1), a)));
        assert_eq!(hit_tile(&rects, 75.0, 10.0), Some((TileId(2), b)));
        assert_eq!(hit_tile(&rects, 10.0, 500.0), None);
    }

    // --- locate_drop_target ---------------------------------------------

    const AREA: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 1000.0,
        h: 800.0,
    };

    /// Two tiles side by side in the main tree, plus an occupied visible
    /// left dock — built through the real `Workspaces` verbs so every
    /// invariant holds.
    fn workspace_with_dock() -> Workspaces {
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Horizontal); // tile 1
        ws.split_active(Orientation::Horizontal); // tile 2
        ws.split_active(Orientation::Horizontal); // tile 3 → dock it
        ws.active_mut().move_to_dock(DockSide::Left);
        ws
    }

    #[test]
    fn locate_finds_main_tiles_with_their_zone() {
        let ws = workspace_with_dock();
        // Left dock (default size 0.25) takes x 0..250; the two main tiles
        // split x 250..1000 at 625.
        let target = locate_drop_target(ws.active(), AREA, 400.0, 400.0);
        assert_eq!(
            target,
            Some(DropTarget::Tile {
                id: TileId(1),
                zone: DropZone::Center
            })
        );
        // Deep in tile 2's right band, vertically centered.
        let target = locate_drop_target(ws.active(), AREA, 990.0, 400.0);
        assert_eq!(
            target,
            Some(DropTarget::Tile {
                id: TileId(2),
                zone: DropZone::Edge(Direction::Right)
            })
        );
    }

    #[test]
    fn locate_finds_dock_tiles_ahead_of_the_dock_background() {
        let ws = workspace_with_dock();
        // The docked tile fills the dock frame, so any point in the dock
        // hits the tile, not the background.
        let target = locate_drop_target(ws.active(), AREA, 125.0, 400.0);
        assert_eq!(
            target,
            Some(DropTarget::Tile {
                id: TileId(3),
                zone: DropZone::Center
            })
        );
    }

    #[test]
    fn locate_reports_an_empty_visible_docks_background() {
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Horizontal);
        ws.active_mut().toggle_dock(DockSide::Right); // visible, empty
        let target = locate_drop_target(ws.active(), AREA, 900.0, 400.0);
        assert_eq!(
            target,
            Some(DropTarget::DockBackground {
                side: DockSide::Right
            })
        );
    }

    #[test]
    fn locate_returns_none_outside_everything_and_for_junk_input() {
        let ws = workspace_with_dock();
        assert_eq!(locate_drop_target(ws.active(), AREA, -5.0, 400.0), None);
        assert_eq!(locate_drop_target(ws.active(), AREA, 500.0, 5000.0), None);
        assert_eq!(locate_drop_target(ws.active(), AREA, f32::NAN, 400.0), None);
        // Empty main tree, no docks: nothing anywhere.
        let empty = Workspaces::new();
        assert_eq!(locate_drop_target(empty.active(), AREA, 500.0, 400.0), None);
    }

    /// Post-merge review cleanup 8 parity pin: `locate_drop_target` (the
    /// drop path) and the shared `resolve_drop_target` core fed with
    /// render-style rects (dock frames + per-dock tile layouts + main
    /// tree layout, exactly what `ShellView::render`'s single layout
    /// pass computes) must agree at every cursor position — the two call
    /// sites share one answer to "what would dropping here mean" by
    /// construction now, and this grid sweep keeps any future divergence
    /// loud. Swept at a 10px step over the whole area plus a margin
    /// outside it, crossing every dock frame, tile boundary, and zone
    /// band in the fixture.
    #[test]
    fn render_path_resolution_agrees_with_locate_drop_target_everywhere() {
        let ws = workspace_with_dock();
        let workspace = ws.active();
        // The render pass's own geometry: dock frames carved out of the
        // area, each visible dock's tiles laid into its frame, the main
        // tree laid into what's left.
        let (tree_area, dock_rects) = dock_layout(workspace.docks(), AREA);
        let dock_cells: super::OwnedDockCells = dock_rects
            .into_iter()
            .map(|(side, r)| (side, r, workspace.docks().get(side).tree().layout(r)))
            .collect();
        let tree_tiles = workspace.tree().layout(tree_area);

        let mut checked = 0u32;
        let mut y = -20.0f32;
        while y <= AREA.h + 20.0 {
            let mut x = -20.0f32;
            while x <= AREA.w + 20.0 {
                let from_core = resolve_drop_target(
                    dock_cells
                        .iter()
                        .map(|(side, r, tiles)| (*side, *r, tiles.as_slice())),
                    &tree_tiles,
                    x,
                    y,
                )
                .map(|(target, _)| target);
                let from_locate = locate_drop_target(workspace, AREA, x, y);
                assert_eq!(
                    from_core, from_locate,
                    "core and locate_drop_target disagree at ({x}, {y})"
                );
                checked += 1;
                x += 10.0;
            }
            y += 10.0;
        }
        assert!(
            checked > 8000,
            "sanity: the sweep actually covered the grid"
        );
    }

    /// The rect the core hands back is the one the highlight paints over:
    /// the hit tile's rect for a tile target, the dock's frame for a
    /// background target.
    #[test]
    fn resolve_drop_target_returns_the_rect_it_resolved_in() {
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Horizontal);
        ws.active_mut().toggle_dock(DockSide::Right); // visible, empty
        let workspace = ws.active();
        let (tree_area, dock_rects) = dock_layout(workspace.docks(), AREA);
        let dock_cells: super::OwnedDockCells = dock_rects
            .into_iter()
            .map(|(side, r)| (side, r, workspace.docks().get(side).tree().layout(r)))
            .collect();
        let tree_tiles = workspace.tree().layout(tree_area);
        let docks = || {
            dock_cells
                .iter()
                .map(|(side, r, tiles)| (*side, *r, tiles.as_slice()))
        };

        // Center of the lone main tile: the tile target carries its rect.
        let (id, tile_rect) = tree_tiles[0];
        let (target, rect) = resolve_drop_target(
            docks(),
            &tree_tiles,
            tile_rect.x + tile_rect.w / 2.0,
            tile_rect.y + tile_rect.h / 2.0,
        )
        .unwrap();
        assert_eq!(
            target,
            DropTarget::Tile {
                id,
                zone: DropZone::Center
            }
        );
        assert_eq!(rect, tile_rect);

        // Inside the empty dock: the background target carries the frame.
        let frame = dock_cells[0].1;
        let (target, rect) = resolve_drop_target(
            docks(),
            &tree_tiles,
            frame.x + frame.w / 2.0,
            frame.y + frame.h / 2.0,
        )
        .unwrap();
        assert_eq!(
            target,
            DropTarget::DockBackground {
                side: DockSide::Right
            }
        );
        assert_eq!(rect, frame);
    }

    #[test]
    fn locate_returns_none_while_fullscreen() {
        let mut ws = Workspaces::new();
        ws.split_active(Orientation::Horizontal);
        ws.split_active(Orientation::Horizontal);
        ws.active_mut().toggle_fullscreen();
        assert_eq!(locate_drop_target(ws.active(), AREA, 500.0, 400.0), None);
    }
}
