//! The one screen's cursor. It rests on a current-scope row or on a saved
//! stop, never both: `Zone` says which group, and each group keeps its own
//! index and identity (`ScopeDialogState::cursor`, `SavedScreen::cursor`),
//! so a re-derive follows a row within its group. `j`/`k` walk the two
//! groups as one list, current rows first, wrapping at both ends.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Zone {
    Current,
    Saved,
}

/// The cursor after a one-row move of `delta` (±1) over `current_len`
/// current rows followed by `saved_len` saved stops, wrapping at both ends.
/// With nothing to rest on it stays where it is.
///
/// Callers pass a normalized position (`ScopeDialogState::normalize_zone`):
/// a `Current` zone with no current rows would be read as saved stop `at`.
pub(crate) fn step(
    zone: Zone,
    at: usize,
    current_len: usize,
    saved_len: usize,
    delta: i64,
) -> (Zone, usize) {
    let total = current_len + saved_len;
    if total == 0 {
        return (zone, at);
    }
    let flat = match zone {
        Zone::Current => at,
        Zone::Saved => current_len + at,
    };
    let next = crate::vimnav::apply(
        flat.min(total - 1),
        total,
        crate::vimnav::NavCommand::Move(delta),
    );
    if next < current_len {
        (Zone::Current, next)
    } else {
        (Zone::Saved, next - current_len)
    }
}

/// Where `o` sends the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Jump {
    Scopes,
    Expressions,
    Current,
}

/// `o` cycles: from a current row to the saved scopes, from a saved scope
/// (or its empty row; `on_scopes`) to the saved expressions, from a saved
/// expression back to the current rows — or to the saved scopes when the
/// scope has no rows to land on.
pub(crate) fn next_jump(zone: Zone, on_scopes: bool, current_len: usize) -> Jump {
    match zone {
        Zone::Current => Jump::Scopes,
        Zone::Saved if on_scopes => Jump::Expressions,
        Zone::Saved if current_len == 0 => Jump::Scopes,
        Zone::Saved => Jump::Current,
    }
}

/// The separator and the filter row, painted between the two groups.
pub(crate) const SEPARATOR_AND_FILTER: usize = 2;

/// What `view::build` paints into the scroll list, as child counts. Each
/// section paints its header, then its body: its rows, or one empty row.
/// A saved section the filter emptied paints its header alone (0).
pub(crate) struct Layout {
    pub contradiction: bool,
    pub current: [usize; 3],
    pub saved: [usize; 2],
}

/// The cursor's place: its group, its section's index in paint order, and
/// its index among that section's body children.
pub(crate) struct At {
    pub zone: Zone,
    pub section: usize,
    pub within: usize,
}

/// The cursor's child index in the scroll list, for `scroll_to_item`. Must
/// agree with `view::build`'s paint order: the contradiction line, each
/// current section (header, body), the separator, the filter row, each
/// saved section (header, body).
pub(crate) fn child_index(layout: &Layout, at: &At) -> usize {
    let block = |bodies: &[usize]| bodies.iter().map(|n| 1 + n).sum::<usize>();
    let mut ix = usize::from(layout.contradiction);
    match at.zone {
        Zone::Current => ix += block(&layout.current[..at.section]),
        Zone::Saved => {
            ix += block(&layout.current) + SEPARATOR_AND_FILTER;
            ix += block(&layout.saved[..at.section]);
        }
    }
    ix + 1 + at.within
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn j_walks_current_rows_then_saved_stops_and_wraps() {
        assert_eq!(step(Zone::Current, 0, 2, 3, 1), (Zone::Current, 1));
        assert_eq!(step(Zone::Current, 1, 2, 3, 1), (Zone::Saved, 0));
        assert_eq!(step(Zone::Saved, 2, 2, 3, 1), (Zone::Current, 0), "wraps");
        assert_eq!(
            step(Zone::Current, 0, 2, 3, -1),
            (Zone::Saved, 2),
            "wraps back"
        );
        assert_eq!(step(Zone::Saved, 0, 2, 3, -1), (Zone::Current, 1));
    }

    #[test]
    fn an_empty_group_is_skipped_and_nothing_to_rest_on_stays_put() {
        assert_eq!(step(Zone::Saved, 0, 0, 3, 1), (Zone::Saved, 1));
        assert_eq!(step(Zone::Saved, 2, 0, 3, 1), (Zone::Saved, 0));
        assert_eq!(step(Zone::Current, 1, 2, 0, 1), (Zone::Current, 0));
        assert_eq!(step(Zone::Saved, 0, 0, 0, 1), (Zone::Saved, 0));
    }

    #[test]
    fn o_cycles_scopes_expressions_current() {
        assert_eq!(next_jump(Zone::Current, false, 2), Jump::Scopes);
        assert_eq!(next_jump(Zone::Saved, true, 2), Jump::Expressions);
        assert_eq!(next_jump(Zone::Saved, false, 2), Jump::Current);
        assert_eq!(
            next_jump(Zone::Saved, false, 0),
            Jump::Scopes,
            "no current rows: back to the scopes"
        );
    }

    #[test]
    fn child_index_counts_headers_bodies_separator_and_filter() {
        let layout = Layout {
            contradiction: false,
            // two dimensions, an empty Expressions (its row), one text
            current: [2, 1, 1],
            // three scopes, two expressions
            saved: [3, 2],
        };
        let current = |section, within| At {
            zone: Zone::Current,
            section,
            within,
        };
        let saved = |section, within| At {
            zone: Zone::Saved,
            section,
            within,
        };
        assert_eq!(child_index(&layout, &current(0, 0)), 1);
        assert_eq!(child_index(&layout, &current(0, 1)), 2);
        assert_eq!(child_index(&layout, &current(2, 0)), 6);
        // 3 + 2 + 2 current children, separator, filter, scopes header
        assert_eq!(child_index(&layout, &saved(0, 0)), 10);
        assert_eq!(child_index(&layout, &saved(1, 1)), 15);
        let with_line = Layout {
            contradiction: true,
            ..layout
        };
        assert_eq!(child_index(&with_line, &current(0, 0)), 2);
        assert_eq!(child_index(&with_line, &saved(1, 1)), 16);
    }
}
