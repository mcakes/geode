//! Which package rows are open (line-pricer spec §8.2, §8.5's tree keys)
//! and the flat rows that are therefore visible. Keyed by `LineId`, so
//! an edit that moves rows never opens or closes the wrong package.

use crate::core::sheet::{LineId, Sheet};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Expansion {
    open: BTreeSet<LineId>,
}

impl Expansion {
    pub fn from_ids(ids: impl IntoIterator<Item = LineId>) -> Expansion {
        Expansion {
            open: ids.into_iter().collect(),
        }
    }

    pub fn is_open(&self, id: LineId) -> bool {
        self.open.contains(&id)
    }

    pub fn set(&mut self, id: LineId, open: bool) {
        if open {
            self.open.insert(id);
        } else {
            self.open.remove(&id);
        }
    }

    /// Flip one package; answers the new state.
    pub fn toggle(&mut self, id: LineId) -> bool {
        let open = !self.is_open(id);
        self.set(id, open);
        open
    }

    pub fn open_all(&mut self, sheet: &Sheet) {
        self.open = (0..sheet.len())
            .filter(|r| sheet.is_package(*r))
            .map(|r| sheet.id(r))
            .collect();
    }

    pub fn close_all(&mut self) {
        self.open.clear();
    }

    /// Forget ids that no longer name a package (removed, or ungrouped),
    /// so the session record never carries dead ids.
    pub fn retain_packages(&mut self, sheet: &Sheet) {
        self.open
            .retain(|id| sheet.index_of(*id).is_some_and(|r| sheet.is_package(r)));
    }

    pub fn ids(&self) -> impl Iterator<Item = LineId> + '_ {
        self.open.iter().copied()
    }
}

/// Flat rows in sheet order, a leg shown only under an open package.
pub fn visible_rows(sheet: &Sheet, expansion: &Expansion) -> Vec<usize> {
    (0..sheet.len())
        .filter(|r| match sheet.parent(*r) {
            None => true,
            Some(p) => expansion.is_open(sheet.id(p)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sheet::tests::{callspread, line, push, spx};
    use geode_core::pricing::OptionKind;

    /// [A, P(L1, L2), B] — flat rows 0..5.
    fn sheet() -> Sheet {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![callspread(1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s
    }

    #[test]
    fn a_closed_package_hides_its_legs_and_an_open_one_shows_them() {
        let s = sheet();
        let mut e = Expansion::default();
        assert_eq!(visible_rows(&s, &e), vec![0, 1, 4]);
        assert!(e.toggle(s.id(1)), "toggle answers the new state");
        assert_eq!(visible_rows(&s, &e), vec![0, 1, 2, 3, 4]);
        e.close_all();
        assert_eq!(visible_rows(&s, &e), vec![0, 1, 4]);
        e.open_all(&s);
        assert!(e.is_open(s.id(1)));
        assert!(!e.is_open(s.id(0)), "open_all opens packages only");
    }

    #[test]
    fn retain_drops_ids_that_are_no_longer_packages() {
        let s = sheet();
        let mut e = Expansion::from_ids([s.id(0), s.id(1), crate::core::LineId(99)]);
        e.retain_packages(&s);
        assert_eq!(e.ids().collect::<Vec<_>>(), vec![s.id(1)]);
    }
}
