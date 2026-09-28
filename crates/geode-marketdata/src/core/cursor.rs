//! Cursor movement between grid cells and the header attribute strip.
//! The tile applies these pure transitions to its retained cursor.

use geode_tile::motion::{self, Motion};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    Cell { row: usize, col: usize },
    Attr(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub rows: usize,
    pub cols: usize,
    pub attrs: usize,
}

/// Pin a cursor produced or restored against a `grid` it may not fit —
/// a shorter delivery, a restored session, or a strip that has since lost
/// an attribute.
pub fn clamp(cursor: Cursor, grid: Grid) -> Cursor {
    if grid.rows == 0 || grid.cols == 0 {
        return Cursor::Cell { row: 0, col: 0 };
    }
    match cursor {
        Cursor::Attr(i) if grid.attrs > 0 => Cursor::Attr(i.min(grid.attrs - 1)),
        Cursor::Attr(_) => Cursor::Cell { row: 0, col: 0 },
        Cursor::Cell { row, col } => Cursor::Cell {
            row: row.min(grid.rows - 1),
            col: col.min(grid.cols - 1),
        },
    }
}

/// One motion. `last_grid_col` is the column the cursor left the grid
/// from — written on entering the strip, read on leaving it.
pub fn step(cursor: Cursor, last_grid_col: &mut usize, m: Motion, grid: Grid) -> Cursor {
    if grid.rows == 0 || grid.cols == 0 {
        return Cursor::Cell { row: 0, col: 0 };
    }
    let max_col = grid.cols - 1;
    match (cursor, m) {
        // Upward motion from row 0 enters the strip when attributes exist,
        // bare or counted: the strip sits outside the grid's wrap cycle.
        (Cursor::Cell { row: 0, col }, Motion::Rows { by, .. }) if by < 0 && grid.attrs > 0 => {
            *last_grid_col = col;
            Cursor::Attr(col.min(grid.attrs - 1))
        }
        // The grid itself moves under the shared rules: a bare single step
        // wraps (downward from the last row wraps to row 0), the rest clamp.
        (Cursor::Cell { row, col }, _) => Cursor::Cell {
            row: motion::row(row, grid.rows, m, false),
            col: motion::col(col, grid.cols, m),
        },
        (Cursor::Attr(_), Motion::Rows { by, .. }) if by > 0 => Cursor::Cell {
            row: 0,
            col: (*last_grid_col).min(max_col),
        },
        (Cursor::Attr(i), Motion::Rows { .. }) => Cursor::Attr(i),
        (Cursor::Attr(i), Motion::Cols(_) | Motion::LineStart | Motion::LineEnd) => {
            Cursor::Attr(motion::col(i, grid.attrs, m))
        }
        (Cursor::Attr(_), Motion::Top(_) | Motion::Bottom(_)) => Cursor::Cell {
            row: motion::row(0, grid.rows, m, false),
            col: (*last_grid_col).min(max_col),
        },
    }
}

/// One motion while a selection is live: every move clamps at the grid's
/// edges and none leaves the grid for the strip, because wrapping past the
/// anchor would silently invert the selection.
pub fn step_clamped(cursor: Cursor, m: Motion, grid: Grid) -> Cursor {
    let Cursor::Cell { row, col } = cursor else {
        return cursor;
    };
    if grid.rows == 0 || grid.cols == 0 {
        return Cursor::Cell { row: 0, col: 0 };
    }
    Cursor::Cell {
        row: motion::row(row, grid.rows, m, true),
        col: motion::col(col, grid.cols, m),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_shell::actions::ActionId;
    use geode_tile::motion::parse;

    fn m(id: &str, count: Option<u32>) -> Motion {
        parse(&ActionId(format!("motion::{id}")), count).unwrap()
    }

    const G: Grid = Grid {
        rows: 5,
        cols: 4,
        attrs: 2,
    };
    fn cell(row: usize, col: usize) -> Cursor {
        Cursor::Cell { row, col }
    }

    #[test]
    fn k_on_the_top_row_enters_the_strip_at_the_nearest_attribute() {
        let mut last = 0;
        assert_eq!(
            step(cell(0, 1), &mut last, m("up", None), G),
            Cursor::Attr(1)
        );
        assert_eq!(last, 1);
        assert_eq!(
            step(cell(0, 3), &mut last, m("up", None), G),
            Cursor::Attr(1),
            "clamped to the last attribute"
        );
        assert_eq!(last, 3);
        assert_eq!(
            step(cell(2, 1), &mut last, m("up", None), G),
            cell(1, 1),
            "not from a lower row"
        );
    }

    /// Without attributes, a single upward step from row 0 wraps. A single
    /// downward step from the last row wraps regardless of the strip.
    #[test]
    fn a_bare_step_wraps_the_grid_and_the_strip_stays_outside_the_cycle() {
        let mut last = 0;
        let g = Grid { attrs: 0, ..G };
        assert_eq!(step(cell(0, 2), &mut last, m("up", None), g), cell(4, 2));
        assert_eq!(step(cell(4, 2), &mut last, m("down", None), g), cell(0, 2));
        assert_eq!(
            step(cell(4, 2), &mut last, m("down", None), G),
            cell(0, 2),
            "j wraps even with a strip"
        );
        assert_eq!(
            step(cell(0, 2), &mut last, m("up", None), G),
            Cursor::Attr(1),
            "k enters the strip instead"
        );
        assert_eq!(
            step(cell(0, 2), &mut last, m("up", Some(3)), g),
            cell(0, 2),
            "a counted step clamps, never wraps"
        );
        assert_eq!(
            step(cell(0, 2), &mut last, m("up", Some(3)), G),
            Cursor::Attr(1),
            "a counted k on row 0 enters the strip like a bare one"
        );
        assert_eq!(
            step(cell(3, 2), &mut last, m("down", Some(5)), G),
            cell(4, 2)
        );
    }

    /// A counted gg/G from the strip lands on that row at the remembered
    /// column; from the grid it is that row, 1-based.
    #[test]
    fn counted_top_and_bottom_from_the_strip_land_on_that_row() {
        let mut last = 2;
        assert_eq!(
            step(Cursor::Attr(1), &mut last, m("bottom", Some(3)), G),
            cell(2, 2)
        );
        assert_eq!(
            step(Cursor::Attr(0), &mut last, m("top", Some(4)), G),
            cell(3, 2)
        );
        assert_eq!(
            step(cell(0, 1), &mut last, m("bottom", Some(2)), G),
            cell(1, 1)
        );
        assert_eq!(
            step(cell(4, 1), &mut last, m("down", Some(1)), G),
            cell(4, 1),
            "1j clamps"
        );
        assert_eq!(
            step_clamped(cell(1, 1), m("bottom", Some(9)), G),
            cell(4, 1)
        );
    }

    #[test]
    fn j_returns_to_the_top_row_at_the_remembered_column() {
        let mut last = 3;
        assert_eq!(
            step(Cursor::Attr(0), &mut last, m("down", None), G),
            cell(0, 3)
        );
        assert_eq!(
            step(Cursor::Attr(0), &mut last, m("down", Some(7)), G),
            cell(0, 3),
            "any downward count lands on row 0"
        );
    }

    #[test]
    fn h_l_caret_dollar_in_the_strip() {
        let mut last = 0;
        assert_eq!(
            step(Cursor::Attr(0), &mut last, m("right", None), G),
            Cursor::Attr(1)
        );
        assert_eq!(
            step(Cursor::Attr(1), &mut last, m("right", Some(5)), G),
            Cursor::Attr(1)
        );
        assert_eq!(
            step(Cursor::Attr(1), &mut last, m("left", None), G),
            Cursor::Attr(0)
        );
        assert_eq!(
            step(Cursor::Attr(1), &mut last, m("line_start", None), G),
            Cursor::Attr(0)
        );
        assert_eq!(
            step(Cursor::Attr(0), &mut last, m("line_end", None), G),
            Cursor::Attr(1)
        );
    }

    #[test]
    fn grid_verbs_from_the_strip_land_in_the_grid() {
        let mut last = 2;
        assert_eq!(
            step(Cursor::Attr(1), &mut last, m("top", None), G),
            cell(0, 2)
        );
        assert_eq!(
            step(Cursor::Attr(1), &mut last, m("bottom", None), G),
            cell(4, 2)
        );
    }

    #[test]
    fn a_clamped_step_never_wraps_and_never_enters_the_strip() {
        assert_eq!(step_clamped(cell(0, 1), m("up", None), G), cell(0, 1));
        assert_eq!(step_clamped(cell(4, 1), m("down", None), G), cell(4, 1));
        assert_eq!(step_clamped(cell(1, 1), m("down", Some(10)), G), cell(4, 1));
        assert_eq!(step_clamped(cell(1, 3), m("right", None), G), cell(1, 3));
        assert_eq!(step_clamped(cell(1, 2), m("top", None), G), cell(0, 2));
        assert_eq!(step_clamped(cell(1, 2), m("line_end", None), G), cell(1, 3));
        // An attribute cursor has no selection to extend; it stays put.
        assert_eq!(
            step_clamped(Cursor::Attr(1), m("down", None), G),
            Cursor::Attr(1)
        );
    }

    #[test]
    fn an_empty_grid_pins_the_cursor_to_the_origin() {
        let mut last = 0;
        let g = Grid {
            rows: 0,
            cols: 0,
            attrs: 2,
        };
        assert_eq!(step(cell(0, 0), &mut last, m("up", None), g), cell(0, 0));
        assert_eq!(clamp(Cursor::Attr(1), g), cell(0, 0));
        assert_eq!(clamp(Cursor::Attr(5), G), Cursor::Attr(1));
        assert_eq!(clamp(cell(9, 9), G), cell(4, 3));
    }
}
