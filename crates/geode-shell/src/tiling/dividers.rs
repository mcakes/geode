//! Pure divider geometry shared by rendering and drag handling.
//!
//! [`divider_strips`] walks split ratios using the same rectangle subdivision
//! as [`Tree::layout`], placing a hit strip at each adjacent-child boundary.
//! [`dock_edge_strips`] uses already-computed dock frames. Fullscreen trees
//! have no divider strips.
//!
//! [`dock_size_from_position`] projects an absolute cursor coordinate to a
//! fraction of the content area; the dock setter owns size clamping. Tree
//! ratio mutation belongs to [`Tree::drag_divider`], which checks the supplied
//! [`DividerAddress`] on each application. Addresses name structure, not node
//! identity: an invalidated path is refused, but a still-valid path can name a
//! different boundary after a structural edit.

use super::docks::DockSide;
use super::tree::{DividerAddress, Node, Orientation, Rect, Tree};

/// Width of a divider's hit strip in the caller's units (pixels in the
/// shell), centered on the boundary. The grab area extends beyond the
/// visible gutter so dragging does not require hitting a narrow line.
pub const DIVIDER_HIT_WIDTH: f32 = 8.0;

/// One grabbable divider inside a tree: the hit rect (a strip
/// `hit_width` wide centered on the boundary between two adjacent split
/// children), the owning split's orientation (a `Horizontal` split's
/// divider is a vertical line dragged left/right — col-resize), and the
/// stable [`DividerAddress`] a drag applies through.
#[derive(Debug, Clone, PartialEq)]
pub struct DividerStrip {
    pub rect: Rect,
    pub orientation: Orientation,
    pub address: DividerAddress,
}

/// Enumerate every divider strip of `tree` laid out in `bounds`.
/// Mirrors [`Tree::layout`]'s fullscreen rule exactly: while a tile is
/// fullscreen the layout is a single full-bounds tile with no visible
/// boundaries, so there are no strips (the same suppression rendering
/// applies to docks and tile chrome). An empty tree or a lone leaf has no
/// splits and therefore no strips.
pub fn divider_strips(tree: &Tree, bounds: Rect, hit_width: f32) -> Vec<DividerStrip> {
    let mut out = Vec::new();
    let Some(root) = tree.root() else {
        return out;
    };
    if let Some(fs) = tree.fullscreen()
        && tree.contains(fs)
    {
        return out;
    }
    let mut path = Vec::new();
    collect_strips(root, bounds, hit_width, &mut path, &mut out);
    out
}

/// The recursive walk behind [`divider_strips`]: re-derives each child's
/// rect from the split's ratios exactly like `layout_node`, and emits one
/// strip per interior boundary (after every child but the last), addressed
/// by the path to the split plus the boundary's left/top child index —
/// the same `(i, i + 1)` adjacent-pair convention `move_divider` uses.
fn collect_strips(
    node: &Node,
    rect: Rect,
    hit_width: f32,
    path: &mut Vec<usize>,
    out: &mut Vec<DividerStrip>,
) {
    let Node::Split {
        orientation,
        children,
        ratios,
    } = node
    else {
        return;
    };
    let mut offset = 0.0;
    for (ix, (child, ratio)) in children.iter().zip(ratios).enumerate() {
        let child_rect = match orientation {
            Orientation::Horizontal => Rect {
                x: rect.x + rect.w * offset,
                y: rect.y,
                w: rect.w * ratio,
                h: rect.h,
            },
            Orientation::Vertical => Rect {
                x: rect.x,
                y: rect.y + rect.h * offset,
                w: rect.w,
                h: rect.h * ratio,
            },
        };
        offset += ratio;
        if ix + 1 < children.len() {
            let strip_rect = match orientation {
                Orientation::Horizontal => Rect {
                    x: rect.x + rect.w * offset - hit_width / 2.0,
                    y: rect.y,
                    w: hit_width,
                    h: rect.h,
                },
                Orientation::Vertical => Rect {
                    x: rect.x,
                    y: rect.y + rect.h * offset - hit_width / 2.0,
                    w: rect.w,
                    h: hit_width,
                },
            };
            out.push(DividerStrip {
                rect: strip_rect,
                orientation: *orientation,
                address: DividerAddress {
                    path: path.clone(),
                    index: ix,
                },
            });
        }
        path.push(ix);
        collect_strips(child, child_rect, hit_width, path, out);
        path.pop();
    }
}

/// One grabbable dock frame edge: the boundary between a visible dock and
/// the main area (the left dock's right edge, the right dock's left edge,
/// the bottom dock's top edge). `orientation` follows the same convention
/// as [`DividerStrip`]: `Horizontal` means a vertical line dragged
/// left/right (col-resize).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DockEdgeStrip {
    pub side: DockSide,
    pub rect: Rect,
    pub orientation: Orientation,
}

/// Edge strips for the visible docks, straight from the `(side, rect)`
/// pairs `dock_layout` already computed this frame — taking those instead
/// of `(&Docks, area)` keeps the render pass at one dock layout call
/// (render-discipline constraint: divider geometry comes from rects the
/// pass already has). Each strip is `hit_width` wide, centered on the
/// dock's inner edge, spanning that edge's full length — for the bottom
/// dock that is the center column between the side docks, exactly the
/// boundary the user sees.
pub fn dock_edge_strips(dock_rects: &[(DockSide, Rect)], hit_width: f32) -> Vec<DockEdgeStrip> {
    dock_rects
        .iter()
        .map(|&(side, r)| match side {
            DockSide::Left => DockEdgeStrip {
                side,
                orientation: Orientation::Horizontal,
                rect: Rect {
                    x: r.right() - hit_width / 2.0,
                    y: r.y,
                    w: hit_width,
                    h: r.h,
                },
            },
            DockSide::Right => DockEdgeStrip {
                side,
                orientation: Orientation::Horizontal,
                rect: Rect {
                    x: r.x - hit_width / 2.0,
                    y: r.y,
                    w: hit_width,
                    h: r.h,
                },
            },
            DockSide::Bottom => DockEdgeStrip {
                side,
                orientation: Orientation::Vertical,
                rect: Rect {
                    x: r.x,
                    y: r.y - hit_width / 2.0,
                    w: r.w,
                    h: hit_width,
                },
            },
        })
        .collect()
}

/// Project a cursor position onto a dock-size fraction of `area` (the
/// same content area `dock_layout` carves): the fraction that would put
/// the dock's inner edge exactly under the cursor. Left dock reads the
/// cursor's x from the area's left edge, right dock from its right edge,
/// bottom dock reads y from the bottom edge. Returns `None` for a
/// degenerate area or a non-finite result — the caller must treat that as
/// a no-op rather than feeding it to `Dock::set_size` (whose NaN-healing
/// would *reset* the size to the default, turning a junk event into a
/// visible jump). Range clamping is deliberately NOT done here:
/// `Dock::set_size` already owns the 0.10..=0.50 clamp, and duplicating
/// it would create a second place the range is defined.
pub fn dock_size_from_position(side: DockSide, x: f32, y: f32, area: Rect) -> Option<f32> {
    let (extent, distance) = match side {
        DockSide::Left => (area.w, x - area.x),
        DockSide::Right => (area.w, area.x + area.w - x),
        DockSide::Bottom => (area.h, area.y + area.h - y),
    };
    if extent <= 0.0 {
        return None;
    }
    let frac = distance / extent;
    frac.is_finite().then_some(frac)
}

#[cfg(test)]
mod tests {
    use super::super::docks::{DOCK_MAX_SIZE, DOCK_MIN_SIZE, Docks, layout as dock_layout};
    use super::super::tree::TileId;
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    fn rect_approx(r: Rect, x: f32, y: f32, w: f32, h: f32) -> bool {
        approx(r.x, x) && approx(r.y, y) && approx(r.w, w) && approx(r.h, h)
    }

    const BOUNDS: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 1000.0,
        h: 800.0,
    };

    #[test]
    fn empty_tree_and_lone_leaf_have_no_strips() {
        let tree = Tree::default();
        assert!(divider_strips(&tree, BOUNDS, 8.0).is_empty());
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        assert!(divider_strips(&tree, BOUNDS, 8.0).is_empty());
    }

    #[test]
    fn two_tile_row_has_one_centered_vertical_strip() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        let strips = divider_strips(&tree, BOUNDS, 8.0);
        assert_eq!(strips.len(), 1);
        let s = &strips[0];
        // Boundary at x=500; strip 8 wide centered on it, full height.
        assert!(rect_approx(s.rect, 496.0, 0.0, 8.0, 800.0), "{:?}", s.rect);
        assert_eq!(s.orientation, Orientation::Horizontal);
        assert_eq!(
            s.address,
            DividerAddress {
                path: vec![],
                index: 0
            }
        );
    }

    #[test]
    fn two_tile_stack_has_one_centered_horizontal_strip() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Vertical);
        tree.split(TileId(2), Orientation::Vertical);
        let strips = divider_strips(&tree, BOUNDS, 8.0);
        assert_eq!(strips.len(), 1);
        let s = &strips[0];
        assert!(rect_approx(s.rect, 0.0, 396.0, 1000.0, 8.0), "{:?}", s.rect);
        assert_eq!(s.orientation, Orientation::Vertical);
    }

    #[test]
    fn three_way_row_has_two_strips_with_pair_indices() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.split(TileId(3), Orientation::Horizontal);
        let strips = divider_strips(&tree, BOUNDS, 8.0);
        assert_eq!(strips.len(), 2);
        assert!(approx(strips[0].rect.x + 4.0, 1000.0 / 3.0));
        assert_eq!(strips[0].address.index, 0);
        assert!(approx(strips[1].rect.x + 4.0, 2000.0 / 3.0));
        assert_eq!(strips[1].address.index, 1);
    }

    /// 2x2 grid (same construction as tree.rs's `grid` fixture):
    /// [(1 / 4) | (2 / 3)] — one outer vertical divider plus each column's
    /// inner horizontal divider, each strip confined to its own column and
    /// addressed by the path down to its split.
    #[test]
    fn nested_grid_strips_are_column_local_with_path_addresses() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.split(TileId(3), Orientation::Vertical);
        tree.focus(TileId(1));
        tree.split(TileId(4), Orientation::Vertical);
        let strips = divider_strips(&tree, BOUNDS, 8.0);
        assert_eq!(strips.len(), 3);

        // Root divider: full height at x=500, empty path.
        let root = strips
            .iter()
            .find(|s| s.address.path.is_empty())
            .expect("root strip");
        assert!(rect_approx(root.rect, 496.0, 0.0, 8.0, 800.0));
        assert_eq!(root.orientation, Orientation::Horizontal);

        // Left column's inner divider: spans only x 0..500, at y=400,
        // addressed through child 0 of the root.
        let left = strips
            .iter()
            .find(|s| s.address.path == vec![0])
            .expect("left column strip");
        assert!(
            rect_approx(left.rect, 0.0, 396.0, 500.0, 8.0),
            "{:?}",
            left.rect
        );
        assert_eq!(left.orientation, Orientation::Vertical);
        assert_eq!(left.address.index, 0);

        // Right column's inner divider mirrors it through child 1.
        let right = strips
            .iter()
            .find(|s| s.address.path == vec![1])
            .expect("right column strip");
        assert!(rect_approx(right.rect, 500.0, 396.0, 500.0, 8.0));
    }

    #[test]
    fn strips_respect_unequal_ratios_and_offset_bounds() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        tree.focus(TileId(1));
        assert!(tree.move_divider(super::super::tree::Direction::Right, 0.2)); // 0.7 / 0.3
        let bounds = Rect {
            x: 100.0,
            y: 50.0,
            w: 500.0,
            h: 200.0,
        };
        let strips = divider_strips(&tree, bounds, 6.0);
        assert_eq!(strips.len(), 1);
        // Boundary at x = 100 + 0.7 * 500 = 450, strip 6 wide.
        assert!(
            rect_approx(strips[0].rect, 447.0, 50.0, 6.0, 200.0),
            "{:?}",
            strips[0].rect
        );
    }

    #[test]
    fn fullscreen_suppresses_all_strips() {
        let mut tree = Tree::default();
        tree.split(TileId(1), Orientation::Horizontal);
        tree.split(TileId(2), Orientation::Horizontal);
        assert_eq!(divider_strips(&tree, BOUNDS, 8.0).len(), 1);
        tree.toggle_fullscreen();
        assert!(
            divider_strips(&tree, BOUNDS, 8.0).is_empty(),
            "a fullscreen layout has no visible boundaries, so no strips"
        );
        tree.toggle_fullscreen();
        assert_eq!(divider_strips(&tree, BOUNDS, 8.0).len(), 1);
    }

    // --- dock edges -------------------------------------------------------

    /// All three docks visible at defaults over a 1000x800 area (same
    /// geometry as docks.rs's own layout tests): left column 0..250,
    /// right column 750..1000, bottom band y 600..800 spanning x 250..750.
    fn all_docks_rects() -> Vec<(DockSide, Rect)> {
        let mut docks = Docks::default();
        for side in DockSide::ALL {
            docks.get_mut(side).set_visible(true);
        }
        dock_layout(&docks, BOUNDS).1
    }

    #[test]
    fn dock_edge_strips_sit_on_each_visible_docks_inner_edge() {
        let strips = dock_edge_strips(&all_docks_rects(), 8.0);
        assert_eq!(strips.len(), 3);

        let left = strips.iter().find(|s| s.side == DockSide::Left).unwrap();
        assert!(
            rect_approx(left.rect, 246.0, 0.0, 8.0, 800.0),
            "{:?}",
            left.rect
        );
        assert_eq!(left.orientation, Orientation::Horizontal);

        let right = strips.iter().find(|s| s.side == DockSide::Right).unwrap();
        assert!(
            rect_approx(right.rect, 746.0, 0.0, 8.0, 800.0),
            "{:?}",
            right.rect
        );
        assert_eq!(right.orientation, Orientation::Horizontal);

        // The bottom strip spans only the center column, like the band it
        // tops.
        let bottom = strips.iter().find(|s| s.side == DockSide::Bottom).unwrap();
        assert!(
            rect_approx(bottom.rect, 250.0, 596.0, 500.0, 8.0),
            "{:?}",
            bottom.rect
        );
        assert_eq!(bottom.orientation, Orientation::Vertical);
    }

    #[test]
    fn no_visible_docks_means_no_edge_strips() {
        assert!(dock_edge_strips(&[], 8.0).is_empty());
    }

    // --- position → fraction ----------------------------------------------

    #[test]
    fn dock_size_from_position_projects_each_side_from_its_own_edge() {
        // Left dock: cursor at x=300 over a 1000-wide area → 0.30.
        let f = dock_size_from_position(DockSide::Left, 300.0, 0.0, BOUNDS).unwrap();
        assert!(approx(f, 0.30), "{f}");
        // Right dock: cursor at x=800 → 200 from the right edge → 0.20.
        let f = dock_size_from_position(DockSide::Right, 800.0, 0.0, BOUNDS).unwrap();
        assert!(approx(f, 0.20), "{f}");
        // Bottom dock: cursor at y=560 over an 800-tall area → 240 from
        // the bottom → 0.30.
        let f = dock_size_from_position(DockSide::Bottom, 0.0, 560.0, BOUNDS).unwrap();
        assert!(approx(f, 0.30), "{f}");
    }

    #[test]
    fn dock_size_from_position_respects_area_offsets() {
        let area = Rect {
            x: 100.0,
            y: 40.0,
            w: 500.0,
            h: 400.0,
        };
        let f = dock_size_from_position(DockSide::Left, 200.0, 0.0, area).unwrap();
        assert!(approx(f, 0.20), "{f}");
        let f = dock_size_from_position(DockSide::Bottom, 0.0, 340.0, area).unwrap();
        assert!(approx(f, 0.25), "{f}");
    }

    #[test]
    fn dock_size_from_position_refuses_degenerate_areas_and_junk_input() {
        let flat = Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        };
        assert_eq!(
            dock_size_from_position(DockSide::Left, 10.0, 0.0, flat),
            None
        );
        assert_eq!(
            dock_size_from_position(DockSide::Bottom, 0.0, 10.0, flat),
            None
        );
        assert_eq!(
            dock_size_from_position(DockSide::Left, f32::NAN, 0.0, BOUNDS),
            None
        );
    }

    #[test]
    fn out_of_range_fractions_are_returned_raw_for_set_size_to_clamp() {
        // Beyond the max: the projection itself doesn't clamp (that's
        // Dock::set_size's one job) — it just reports where the cursor is.
        let f = dock_size_from_position(DockSide::Left, 900.0, 0.0, BOUNDS).unwrap();
        assert!(f > DOCK_MAX_SIZE);
        let f = dock_size_from_position(DockSide::Left, 10.0, 0.0, BOUNDS).unwrap();
        assert!(f < DOCK_MIN_SIZE);
    }
}
