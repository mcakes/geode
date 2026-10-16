//! The grid's pure model: one watchlist's rows (`rows::rows`) in the shared
//! named-row model (`geode_tile::rows::RowModel`): the order a sort puts
//! them in, the rows the `/` filter keeps, a cursor and a row selection,
//! held by name so a rebuild keeps them on their rows. This module names
//! the watchlist's filter columns and turns its session's sort into the
//! model's [`Order`].

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

use geode_core::watchlist::members::Origin;

pub use geode_tile::rows::{GridRow, Order, RowModel};

use crate::core::rows::{WatchRow, origin_text};
pub use crate::core::session::SortCol;

pub type GridModel = RowModel<WatchRow>;

/// The filter's columns: what `/` searches and what [`RowModel::marks`]
/// indexes (`WatchRow::narrow_fields`). The origin is not searched.
pub const NAME_COL: usize = 0;
pub const REFERENCE_COL: usize = 1;

/// Present keys in `desc`'s direction, then absent ones: a row a sort
/// cannot rank (no reference name) never leads.
fn absent_last<T: Ord>(a: Option<T>, b: Option<T>, desc: bool) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) if desc => y.cmp(&x),
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// The origin column's text per distinct origin, formatted once rather
/// than twice per comparison: a sort compares O(n log n) times, and a list
/// holds few distinct origins. Bucketed by a cheap structural hash, the
/// origin compared on a hit; cleared past `ORIGIN_TEXTS_CAP` entries, so an
/// order kept across many rebuilds never grows without bound.
#[derive(Default)]
struct OriginTexts {
    by_hash: HashMap<u64, Vec<(Origin, bool, Rc<str>)>>,
    len: usize,
}

const ORIGIN_TEXTS_CAP: usize = 4096;

impl OriginTexts {
    fn get(&mut self, origin: &Origin, pending: bool) -> Rc<str> {
        let hash = origin_hash(origin, pending);
        if let Some(hit) = self
            .by_hash
            .get(&hash)
            .and_then(|b| b.iter().find(|(o, p, _)| *p == pending && o == origin))
        {
            return Rc::clone(&hit.2);
        }
        if self.len >= ORIGIN_TEXTS_CAP {
            self.by_hash.clear();
            self.len = 0;
        }
        let text: Rc<str> = origin_text(origin, pending).into();
        self.by_hash
            .entry(hash)
            .or_default()
            .push((origin.clone(), pending, Rc::clone(&text)));
        self.len += 1;
        text
    }
}

/// FNV-1a over the origin's variant, rule numbers and flags: the bucket
/// key only, equality decides.
fn origin_hash(origin: &Origin, pending: bool) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |v: u64| h = (h ^ v).wrapping_mul(PRIME);
    let rules = match origin {
        Origin::Manual => {
            mix(0);
            &[][..]
        }
        Origin::Rules(r) => {
            mix(1);
            r
        }
        Origin::Both(r) => {
            mix(2);
            r
        }
        Origin::Excluded { rules, manual } => {
            mix(3 + u64::from(*manual));
            rules
        }
    };
    for &r in rules {
        mix(r as u64);
    }
    mix(u64::from(pending));
    h
}

/// The watchlist's sort (a column, `true` descending) as the shared
/// model's order. `Origin` sorts by its text, `Reference` by the reference
/// name with a missing one last in either direction; the model breaks
/// ties by its default order (by name, excluded rows last). The key is
/// `<column> <asc|desc>`; the session keeps the `(SortCol, bool)` itself.
pub fn order((col, desc): (SortCol, bool)) -> Order<WatchRow> {
    let key = format!("{} {}", col.name(), if desc { "desc" } else { "asc" });
    let cmp: Rc<dyn Fn(&WatchRow, &WatchRow) -> Ordering> = match col {
        SortCol::Name => Rc::new(move |a: &WatchRow, b: &WatchRow| {
            absent_last(Some(&a.name), Some(&b.name), desc)
        }),
        SortCol::Origin => {
            let texts = RefCell::new(OriginTexts::default());
            Rc::new(move |a: &WatchRow, b: &WatchRow| {
                let mut texts = texts.borrow_mut();
                let a = texts.get(&a.origin, a.pending);
                let b = texts.get(&b.origin, b.pending);
                absent_last(Some(a), Some(b), desc)
            })
        }
        SortCol::Reference => Rc::new(move |a: &WatchRow, b: &WatchRow| {
            absent_last(a.reference.as_deref(), b.reference.as_deref(), desc)
        }),
    };
    Order { key, cmp }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_tile::motion::Motion;

    fn row(name: &str, origin: Origin, reference: Option<&str>) -> WatchRow {
        WatchRow {
            name: name.into(),
            reference: reference.map(Into::into),
            in_reference: reference.is_some(),
            origin,
            pending: false,
        }
    }

    fn manual(name: &str) -> WatchRow {
        row(name, Origin::Manual, None)
    }

    fn rule(name: &str, i: usize) -> WatchRow {
        row(name, Origin::Rules(vec![i]), None)
    }

    fn excluded(name: &str) -> WatchRow {
        row(
            name,
            Origin::Excluded {
                rules: vec![0],
                manual: false,
            },
            None,
        )
    }

    fn down(by: i64) -> Motion {
        Motion::Rows {
            by,
            counted: by != 1,
        }
    }

    fn names(g: &GridModel) -> Vec<&str> {
        g.visible()
            .iter()
            .map(|&i| g.row(i).name.as_str())
            .collect()
    }

    #[test]
    fn the_default_order_is_by_name_with_excluded_last() {
        let mut g = GridModel::new();
        g.set_rows(vec![
            excluded("AAA"),
            manual("SPX"),
            rule("DAX", 0),
            excluded("UKX"),
            manual("NDX"),
        ]);
        assert_eq!(names(&g), ["DAX", "NDX", "SPX", "AAA", "UKX"]);
        assert_eq!(g.counts(), (3, 2));
    }

    #[test]
    fn counts_ignore_the_filter() {
        let mut g = GridModel::new();
        g.set_rows(vec![manual("A"), excluded("B"), manual("C")]);
        g.set_filter("C");
        assert_eq!(g.counts(), (2, 1));
    }

    #[test]
    fn sort_by_name_interleaves_excluded_rows_and_bare_sort_restores() {
        let mut g = GridModel::new();
        g.set_rows(vec![manual("A"), excluded("B"), manual("C")]);
        g.set_sort(Some(order((SortCol::Name, true))));
        assert_eq!(names(&g), ["C", "B", "A"]);
        g.set_sort(Some(order((SortCol::Name, false))));
        assert_eq!(names(&g), ["A", "B", "C"]);
        g.set_sort(None);
        assert_eq!(names(&g), ["A", "C", "B"]);
    }

    #[test]
    fn sort_by_origin_orders_by_its_text() {
        let mut g = GridModel::new();
        g.set_rows(vec![
            manual("NDX"),
            rule("DAX", 0),
            row("SPX", Origin::Both(vec![0, 1]), None),
            excluded("UKX"),
        ]);
        g.set_sort(Some(order((SortCol::Origin, false))));
        assert_eq!(names(&g), ["UKX", "NDX", "SPX", "DAX"]);
        g.set_sort(Some(order((SortCol::Origin, true))));
        assert_eq!(names(&g), ["DAX", "SPX", "NDX", "UKX"]);
    }

    /// The origin texts are kept per distinct origin and pending flag: a
    /// pending row's text sorts apart from the same origin settled, and a
    /// rebuild under the same order reads the same texts.
    #[test]
    fn sort_by_origin_tells_a_pending_row_from_a_settled_one() {
        let mut g = GridModel::new();
        let pending = |name: &str| WatchRow {
            pending: true,
            ..manual(name)
        };
        g.set_rows(vec![pending("A"), manual("B"), rule("C", 0)]);
        g.set_sort(Some(order((SortCol::Origin, false))));
        assert_eq!(names(&g), ["B", "A", "C"]);
        g.set_rows(vec![manual("A"), pending("B"), rule("C", 0)]);
        assert_eq!(names(&g), ["A", "B", "C"]);
    }

    #[test]
    fn sort_by_reference_puts_a_missing_name_last_either_way() {
        let mut g = GridModel::new();
        g.set_rows(vec![
            row("SPX", Origin::Manual, Some("S&P 500")),
            row("NDX", Origin::Manual, None),
            row("DAX", Origin::Manual, Some("DAX 40")),
            excluded("UKX"),
        ]);
        g.set_sort(Some(order((SortCol::Reference, false))));
        assert_eq!(names(&g), ["DAX", "SPX", "NDX", "UKX"]);
        g.set_sort(Some(order((SortCol::Reference, true))));
        assert_eq!(names(&g), ["SPX", "DAX", "NDX", "UKX"]);
    }

    /// After a member verb the cursor keeps its shown index, whatever put
    /// it there: excluding the top row moves it to the foot and leaves the
    /// cursor on the next one.
    #[test]
    fn a_verb_rebuild_keeps_the_cursor_index_even_when_put() {
        let mut g = GridModel::new();
        g.set_rows(vec![rule("A", 0), rule("B", 0), manual("C")]);
        g.move_cursor(down(1), false);
        g.move_cursor(
            Motion::Rows {
                by: -1,
                counted: false,
            },
            false,
        );
        assert_eq!(g.cursor_name(), Some("A"));
        // A is excluded: the default order moves it to the foot.
        g.after_verb(vec![excluded("A"), rule("B", 0), manual("C")]);
        assert_eq!(g.cursor(), Some(0));
        assert_eq!(g.cursor_name(), Some("B"));
        // And the cursor is put there: a later rebuild follows B.
        g.set_rows(vec![excluded("A"), manual("AA"), rule("B", 0)]);
        assert_eq!(g.cursor_name(), Some("B"));
    }

    /// A verb over a selection ends it: the rows it removed move, and a
    /// span re-resolved across the reorder would cover others.
    #[test]
    fn a_verb_rebuild_ends_a_selection_and_drops_a_waiting_seed() {
        let mut g = GridModel::new();
        g.seed_cursor("Q".into());
        g.set_rows(vec![manual("A"), manual("B")]);
        g.start_selection();
        g.move_cursor(down(1), true);
        g.after_verb(vec![excluded("A"), excluded("B")]);
        assert_eq!(g.selected(), None);
        assert_eq!(g.cursor(), Some(1));
        assert_eq!(g.saved_cursor(), Some("B"), "the seed is gone");
    }
}
