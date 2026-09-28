//! Cursor movement between grid cells and the header attribute strip.
//! The tile applies these pure transitions to its retained cursor.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Rows(isize),
    Cols(isize),
    Top,
    Bottom,
    FirstCol,
    LastCol,
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
pub fn step(cursor: Cursor, last_grid_col: &mut usize, motion: Motion, grid: Grid) -> Cursor {
    use geode_shell::vimnav::{self, NavCommand};
    if grid.rows == 0 || grid.cols == 0 {
        return Cursor::Cell { row: 0, col: 0 };
    }
    let max_row = grid.rows - 1;
    let max_col = grid.cols - 1;
    match (cursor, motion) {
        (Cursor::Cell { row: 0, col }, Motion::Rows(n)) if n < 0 && grid.attrs > 0 => {
            *last_grid_col = col;
            Cursor::Attr(col.min(grid.attrs - 1))
        }
        // Single row steps wrap; larger steps clamp through `vimnav::apply`.
        // The strip is outside that cycle: upward motion from row 0 enters
        // it when attributes exist, while downward motion from the last row
        // wraps to row 0.
        (Cursor::Cell { row, col }, Motion::Rows(n)) => Cursor::Cell {
            row: vimnav::apply(row, grid.rows, NavCommand::Move(n as i64)),
            col,
        },
        (Cursor::Cell { row, col }, Motion::Cols(n)) => Cursor::Cell {
            row,
            col: add(col, n, max_col),
        },
        (Cursor::Cell { col, .. }, Motion::Top) => Cursor::Cell { row: 0, col },
        (Cursor::Cell { col, .. }, Motion::Bottom) => Cursor::Cell { row: max_row, col },
        (Cursor::Cell { row, .. }, Motion::FirstCol) => Cursor::Cell { row, col: 0 },
        (Cursor::Cell { row, .. }, Motion::LastCol) => Cursor::Cell { row, col: max_col },
        (Cursor::Attr(_), Motion::Rows(n)) if n > 0 => Cursor::Cell {
            row: 0,
            col: (*last_grid_col).min(max_col),
        },
        (Cursor::Attr(i), Motion::Rows(_)) => Cursor::Attr(i),
        (Cursor::Attr(i), Motion::Cols(n)) => Cursor::Attr(add(i, n, grid.attrs.saturating_sub(1))),
        (Cursor::Attr(_), Motion::Top) => Cursor::Cell {
            row: 0,
            col: (*last_grid_col).min(max_col),
        },
        (Cursor::Attr(_), Motion::Bottom) => Cursor::Cell {
            row: max_row,
            col: (*last_grid_col).min(max_col),
        },
        (Cursor::Attr(_), Motion::FirstCol) => Cursor::Attr(0),
        (Cursor::Attr(_), Motion::LastCol) => Cursor::Attr(grid.attrs.saturating_sub(1)),
    }
}

/// One motion while a selection is live:
/// every move clamps at the grid's edges instead of wrapping, and none
/// leaves the grid for the attribute strip, because wrapping past the
/// anchor would silently invert the selection.
pub fn step_clamped(cursor: Cursor, motion: Motion, grid: Grid) -> Cursor {
    use geode_shell::vimnav::{NavCommand, apply_clamped};
    let Cursor::Cell { row, col } = cursor else {
        return cursor;
    };
    if grid.rows == 0 || grid.cols == 0 {
        return Cursor::Cell { row: 0, col: 0 };
    }
    let max_col = grid.cols - 1;
    match motion {
        Motion::Rows(n) => Cursor::Cell {
            row: apply_clamped(row, grid.rows, NavCommand::Move(n as i64)),
            col,
        },
        Motion::Cols(n) => Cursor::Cell {
            row,
            col: add(col, n, max_col),
        },
        Motion::Top => Cursor::Cell { row: 0, col },
        Motion::Bottom => Cursor::Cell {
            row: grid.rows - 1,
            col,
        },
        Motion::FirstCol => Cursor::Cell { row, col: 0 },
        Motion::LastCol => Cursor::Cell { row, col: max_col },
    }
}

fn add(at: usize, n: isize, max: usize) -> usize {
    (at as isize).saturating_add(n).clamp(0, max as isize) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

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
            step(cell(0, 1), &mut last, Motion::Rows(-1), G),
            Cursor::Attr(1)
        );
        assert_eq!(last, 1);
        assert_eq!(
            step(cell(0, 3), &mut last, Motion::Rows(-1), G),
            Cursor::Attr(1),
            "clamped to the last attribute"
        );
        assert_eq!(last, 3);
        assert_eq!(
            step(cell(2, 1), &mut last, Motion::Rows(-1), G),
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
        assert_eq!(step(cell(0, 2), &mut last, Motion::Rows(-1), g), cell(4, 2));
        assert_eq!(step(cell(4, 2), &mut last, Motion::Rows(1), g), cell(0, 2));
        assert_eq!(
            step(cell(4, 2), &mut last, Motion::Rows(1), G),
            cell(0, 2),
            "j wraps even with a strip"
        );
        assert_eq!(
            step(cell(0, 2), &mut last, Motion::Rows(-1), G),
            Cursor::Attr(1),
            "k enters the strip instead"
        );
        assert_eq!(
            step(cell(0, 2), &mut last, Motion::Rows(-3), g),
            cell(0, 2),
            "a counted step clamps, never wraps"
        );
        assert_eq!(
            step(cell(0, 2), &mut last, Motion::Rows(-3), G),
            Cursor::Attr(1),
            "a counted k on row 0 enters the strip like a bare one"
        );
        assert_eq!(step(cell(3, 2), &mut last, Motion::Rows(5), G), cell(4, 2));
    }

    #[test]
    fn j_returns_to_the_top_row_at_the_remembered_column() {
        let mut last = 3;
        assert_eq!(
            step(Cursor::Attr(0), &mut last, Motion::Rows(1), G),
            cell(0, 3)
        );
        assert_eq!(
            step(Cursor::Attr(0), &mut last, Motion::Rows(7), G),
            cell(0, 3),
            "any downward count lands on row 0"
        );
    }

    #[test]
    fn h_l_caret_dollar_in_the_strip() {
        let mut last = 0;
        assert_eq!(
            step(Cursor::Attr(0), &mut last, Motion::Cols(1), G),
            Cursor::Attr(1)
        );
        assert_eq!(
            step(Cursor::Attr(1), &mut last, Motion::Cols(5), G),
            Cursor::Attr(1)
        );
        assert_eq!(
            step(Cursor::Attr(1), &mut last, Motion::Cols(-1), G),
            Cursor::Attr(0)
        );
        assert_eq!(
            step(Cursor::Attr(1), &mut last, Motion::FirstCol, G),
            Cursor::Attr(0)
        );
        assert_eq!(
            step(Cursor::Attr(0), &mut last, Motion::LastCol, G),
            Cursor::Attr(1)
        );
    }

    #[test]
    fn grid_verbs_from_the_strip_land_in_the_grid() {
        let mut last = 2;
        assert_eq!(step(Cursor::Attr(1), &mut last, Motion::Top, G), cell(0, 2));
        assert_eq!(
            step(Cursor::Attr(1), &mut last, Motion::Bottom, G),
            cell(4, 2)
        );
    }

    #[test]
    fn a_clamped_step_never_wraps_and_never_enters_the_strip() {
        assert_eq!(step_clamped(cell(0, 1), Motion::Rows(-1), G), cell(0, 1));
        assert_eq!(step_clamped(cell(4, 1), Motion::Rows(1), G), cell(4, 1));
        assert_eq!(step_clamped(cell(1, 1), Motion::Rows(10), G), cell(4, 1));
        assert_eq!(step_clamped(cell(1, 3), Motion::Cols(1), G), cell(1, 3));
        assert_eq!(step_clamped(cell(1, 2), Motion::Top, G), cell(0, 2));
        assert_eq!(step_clamped(cell(1, 2), Motion::LastCol, G), cell(1, 3));
        // An attribute cursor has no selection to extend; it stays put.
        assert_eq!(
            step_clamped(Cursor::Attr(1), Motion::Rows(1), G),
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
        assert_eq!(step(cell(0, 0), &mut last, Motion::Rows(-1), g), cell(0, 0));
        assert_eq!(clamp(Cursor::Attr(1), g), cell(0, 0));
        assert_eq!(clamp(Cursor::Attr(5), G), Cursor::Attr(1));
        assert_eq!(clamp(cell(9, 9), G), cell(4, 3));
    }
}
