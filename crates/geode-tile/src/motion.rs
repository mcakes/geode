//! The shared motion vocabulary every grid tile answers. The shell registers
//! the `motion::*` ids and ships their keys once, under the `grid` and
//! `tilelist` key contexts; the focused tile receives them through its own
//! `dispatch`, because the shell handles none of them. This module turns an
//! id and its count into a [`Motion`] and applies it under one rule set, so
//! the same key moves the same way in every tile.
//!
//! A bare single row step wraps at the ends, except while a selection is
//! live, where wrapping past the anchor would silently invert it. Every
//! counted move and every page move clamps. A counted `top`/`bottom` jumps to
//! that 1-based row. Columns clamp. An empty axis leaves the position alone.
//! What a tile does around the result (entering a header strip, closing a
//! field, following a log) stays the tile's.

use geode_shell::actions::ActionId;

pub const DOWN: &str = "motion::down";
pub const UP: &str = "motion::up";
pub const LEFT: &str = "motion::left";
pub const RIGHT: &str = "motion::right";
pub const TOP: &str = "motion::top";
pub const BOTTOM: &str = "motion::bottom";
pub const HALF_PAGE_DOWN: &str = "motion::half_page_down";
pub const HALF_PAGE_UP: &str = "motion::half_page_up";
pub const PAGE_DOWN: &str = "motion::page_down";
pub const PAGE_UP: &str = "motion::page_up";
pub const LINE_START: &str = "motion::line_start";
pub const LINE_END: &str = "motion::line_end";
/// Step an open tile menu or popup list. What a step means there (skipping
/// disabled rows, wrapping a series list) stays the list's own rule.
pub const MENU_DOWN: &str = "motion::menu_down";
pub const MENU_UP: &str = "motion::menu_up";

/// Rows `ctrl+d`/`ctrl+u` move, times the count. A fixed offset rather than
/// a viewport fraction, so a count scales it predictably.
pub const HALF_PAGE: i64 = 5;
/// Rows `ctrl+f`/`ctrl+b`/`pagedown`/`pageup` move, times the count.
pub const FULL_PAGE: i64 = 10;

/// One resolved grid motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// A signed row step, down positive, already multiplied by the count.
    /// `counted` records whether a count was typed: only an uncounted
    /// single step may wrap, so `1j` clamps like any counted move.
    Rows {
        by: i64,
        counted: bool,
    },
    /// A signed column step, right positive, multiplied by the count.
    Cols(i64),
    /// `g g`: the first row, or row N (1-based) under a count.
    Top(Option<u32>),
    /// `shift+g`: the last row, or row N (1-based) under a count.
    Bottom(Option<u32>),
    LineStart,
    LineEnd,
}

impl Motion {
    /// Whether this motion targets the row axis (a column motion leaves the
    /// row alone, and a tile must not re-seat its row cursor for one).
    pub fn moves_rows(self) -> bool {
        matches!(
            self,
            Motion::Rows { .. } | Motion::Top(_) | Motion::Bottom(_)
        )
    }
}

/// The grid motion `action` names, with `count` applied. `None` for any
/// other id, the menu steps included.
pub fn parse(action: &ActionId, count: Option<u32>) -> Option<Motion> {
    let counted = count.is_some();
    let n = i64::from(count.unwrap_or(1).max(1));
    let rows = |per: i64| Motion::Rows {
        by: per * n,
        counted,
    };
    Some(match action.0.as_str() {
        DOWN => rows(1),
        UP => rows(-1),
        HALF_PAGE_DOWN => rows(HALF_PAGE),
        HALF_PAGE_UP => rows(-HALF_PAGE),
        PAGE_DOWN => rows(FULL_PAGE),
        PAGE_UP => rows(-FULL_PAGE),
        LEFT => Motion::Cols(-n),
        RIGHT => Motion::Cols(n),
        TOP => Motion::Top(count.map(|c| c.max(1))),
        BOTTOM => Motion::Bottom(count.map(|c| c.max(1))),
        LINE_START => Motion::LineStart,
        LINE_END => Motion::LineEnd,
        _ => return None,
    })
}

/// The row `motion` lands on from `at` over `len` rows. `selecting` clamps
/// a bare single step that would otherwise wrap. A column motion, or an
/// empty axis, returns `at` unchanged.
pub fn row(at: usize, len: usize, motion: Motion, selecting: bool) -> usize {
    if len == 0 {
        return at;
    }
    let last = len - 1;
    match motion {
        Motion::Rows { by, counted } => {
            // A cursor left past the end after the rows shrank steps from
            // the last row: otherwise a bare wrapping step would land modulo
            // `len` somewhere unrelated to where the cursor is drawn.
            let from = at.min(last);
            if !counted && !selecting && by.abs() == 1 {
                (from as i64 + by).rem_euclid(len as i64) as usize
            } else {
                (from as i64).saturating_add(by).clamp(0, last as i64) as usize
            }
        }
        Motion::Top(None) => 0,
        Motion::Bottom(None) => last,
        Motion::Top(Some(n)) | Motion::Bottom(Some(n)) => (n.max(1) as usize - 1).min(last),
        Motion::Cols(_) | Motion::LineStart | Motion::LineEnd => at,
    }
}

/// The column `motion` lands on from `at` over `cols` columns. Columns
/// never wrap. A row motion, or an empty axis, returns `at` unchanged.
pub fn col(at: usize, cols: usize, motion: Motion) -> usize {
    if cols == 0 {
        return at;
    }
    let last = cols - 1;
    match motion {
        // A column left past the end after the columns shrank steps from
        // the last one, where the cursor is drawn.
        Motion::Cols(by) => (at.min(last) as i64)
            .saturating_add(by)
            .clamp(0, last as i64) as usize,
        Motion::LineStart => 0,
        Motion::LineEnd => last,
        Motion::Rows { .. } | Motion::Top(_) | Motion::Bottom(_) => at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: &str, count: Option<u32>) -> Motion {
        parse(&ActionId(format!("motion::{id}")), count).expect("a grid motion id")
    }

    #[test]
    fn a_bare_single_step_wraps_at_both_ends() {
        assert_eq!(row(4, 5, m("down", None), false), 0);
        assert_eq!(row(0, 5, m("up", None), false), 4);
        assert_eq!(row(2, 5, m("down", None), false), 3);
        assert_eq!(
            row(0, 1, m("down", None), false),
            0,
            "one row wraps to itself"
        );
    }

    #[test]
    fn a_live_selection_clamps_a_bare_step() {
        assert_eq!(row(4, 5, m("down", None), true), 4);
        assert_eq!(row(0, 5, m("up", None), true), 0);
        assert_eq!(row(2, 5, m("down", None), true), 3);
    }

    #[test]
    fn a_counted_step_clamps_even_a_count_of_one() {
        assert_eq!(
            row(4, 5, m("down", Some(1)), false),
            4,
            "1j is counted: it clamps"
        );
        assert_eq!(row(0, 5, m("up", Some(1)), false), 0);
        assert_eq!(row(1, 5, m("down", Some(2)), false), 3);
        assert_eq!(row(3, 5, m("down", Some(50)), false), 4);
        assert_eq!(row(3, 5, m("up", Some(50)), false), 0);
    }

    #[test]
    fn a_counted_top_or_bottom_jumps_to_that_row() {
        assert_eq!(row(3, 10, m("top", None), false), 0);
        assert_eq!(row(3, 10, m("bottom", None), false), 9);
        assert_eq!(
            row(0, 10, m("top", Some(5)), false),
            4,
            "5gg is row 5, 1-based"
        );
        assert_eq!(row(0, 10, m("bottom", Some(5)), false), 4, "5G is row 5");
        assert_eq!(
            row(0, 10, m("bottom", Some(99)), false),
            9,
            "past the end clamps"
        );
        assert_eq!(row(0, 10, m("bottom", Some(1)), false), 0);
    }

    #[test]
    fn the_page_motions_move_five_and_ten_times_the_count() {
        assert_eq!(row(0, 100, m("half_page_down", None), false), 5);
        assert_eq!(row(50, 100, m("half_page_up", None), false), 45);
        assert_eq!(row(0, 100, m("page_down", None), false), 10);
        assert_eq!(row(50, 100, m("page_up", None), false), 40);
        assert_eq!(row(0, 100, m("half_page_down", Some(3)), false), 15);
        assert_eq!(row(0, 100, m("page_down", Some(2)), false), 20);
        assert_eq!(
            row(98, 100, m("half_page_down", None), false),
            99,
            "a page clamps"
        );
        assert_eq!(row(2, 100, m("page_up", None), false), 0);
    }

    #[test]
    fn columns_clamp_and_line_start_and_end_reach_the_extremes() {
        assert_eq!(col(2, 6, m("line_start", None)), 0);
        assert_eq!(col(2, 6, m("line_end", None)), 5);
        assert_eq!(col(5, 6, m("right", None)), 5, "l at the last column stays");
        assert_eq!(col(0, 6, m("left", None)), 0, "h at the first column stays");
        assert_eq!(col(1, 6, m("right", Some(3))), 4);
        assert_eq!(col(4, 6, m("left", Some(9))), 0);
        assert_eq!(
            col(3, 6, m("down", None)),
            3,
            "a row motion leaves the column"
        );
        assert_eq!(
            row(3, 6, m("right", None), false),
            3,
            "a column motion leaves the row"
        );
    }

    #[test]
    fn an_empty_axis_is_a_no_op() {
        for id in ["down", "up", "top", "bottom", "half_page_down", "page_up"] {
            assert_eq!(row(3, 0, m(id, None), false), 3, "{id}");
            assert_eq!(row(3, 0, m(id, Some(5)), false), 3, "{id} counted");
        }
        for id in ["left", "right", "line_start", "line_end"] {
            assert_eq!(col(2, 0, m(id, None)), 2, "{id}");
        }
    }

    /// A cursor left past the end after the rows shrank moves from the last
    /// row, not from its stale index: a bare `j` wraps to the top and a
    /// bare `k` steps to the row above the last, rather than landing at
    /// `stale ± 1` modulo the length.
    #[test]
    fn a_stale_position_past_the_end_moves_from_the_last_row() {
        assert_eq!(row(7, 5, m("down", None), false), 0);
        assert_eq!(row(7, 5, m("up", None), false), 3);
        assert_eq!(row(9, 5, m("down", None), true), 4);
        assert_eq!(col(9, 4, m("left", None)), 2);
        assert_eq!(
            row(9, 5, m("right", None), false),
            9,
            "a column motion leaves the row"
        );
    }

    #[test]
    fn only_grid_motion_ids_parse() {
        assert_eq!(parse(&ActionId(MENU_DOWN.into()), None), None);
        assert_eq!(parse(&ActionId(MENU_UP.into()), None), None);
        assert_eq!(parse(&ActionId("blotter::down".into()), None), None);
        assert_eq!(parse(&ActionId("motion::nope".into()), None), None);
        assert!(!m("left", None).moves_rows());
        assert!(m("bottom", Some(3)).moves_rows());
    }

    /// The shell registers the ids the tiles interpret: an id missing from
    /// either side is a key that dispatches to nothing, or a motion no key
    /// can reach.
    #[test]
    fn every_shared_id_is_registered_by_the_shell_and_parsed_here() {
        let registered: Vec<&str> = geode_shell::defaults::MOTION_ACTIONS
            .iter()
            .map(|(id, _)| *id)
            .collect();
        let ours = [
            DOWN,
            UP,
            LEFT,
            RIGHT,
            TOP,
            BOTTOM,
            HALF_PAGE_DOWN,
            HALF_PAGE_UP,
            PAGE_DOWN,
            PAGE_UP,
            LINE_START,
            LINE_END,
            MENU_DOWN,
            MENU_UP,
        ];
        assert_eq!(registered.len(), ours.len());
        for id in ours {
            assert!(
                registered.contains(&id),
                "{id} is not registered by the shell"
            );
            let is_menu = id == MENU_DOWN || id == MENU_UP;
            assert_eq!(
                parse(&ActionId(id.into()), None).is_some(),
                !is_menu,
                "{id}"
            );
        }
    }
}
