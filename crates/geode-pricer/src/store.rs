//! Where a sheet lives between tiles (line-pricer spec §7.1): the tile
//! never holds a document request of its own, only this seam.
//!
//! **The shape is Part 3's** (planning decision 7): `load` answers at
//! once when it can and `Pending` when the answer is on its way — Part
//! 4's DuckDB store answers `Pending` and delivers the rows through the
//! tile's `loaded`, reached from its `Delivery::Query` arm. `save` is one
//! whole-sheet publish; `false` is a refusal the tile shows and retries on
//! the next edit burst (spec §7.3). A zero-row sheet is never saved
//! (`to_rows` answers `None`; spec §7.2).
//!
//! [`MemorySheetStore`] is the only implementation until Part 4 — the
//! app's for this part and the tests' fake. A sheet in it lives for the
//! process, not across a restart.

use geode_core::document::DocumentRows;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

/// What a `load` answered.
#[derive(Debug)]
pub enum Loaded {
    Rows(DocumentRows),
    /// No document under that name: an empty sheet, not an error (§7.2).
    Missing,
    /// On its way; the tile paints `loading` until `PricerTile::loaded`.
    Pending,
}

pub trait SheetStore {
    fn load(&self, name: &str) -> Loaded;
    /// Publish the whole sheet. `false`: refused, nothing written.
    fn save(&self, name: &str, rows: DocumentRows) -> bool;
    /// Whether a document exists under `name` (the `untitled-N` rule).
    fn contains(&self, name: &str) -> bool;
}

/// The process-lifetime store. Clones share one map, so the factory and
/// every tile it builds see the same sheets. The three knobs exist for
/// the tile's tests (a refused save, a pending load, a save count); the
/// app never turns them.
#[derive(Clone, Default)]
pub struct MemorySheetStore {
    sheets: Rc<RefCell<BTreeMap<String, DocumentRows>>>,
    saves: Rc<Cell<usize>>,
    refusing: Rc<Cell<bool>>,
    pending: Rc<Cell<bool>>,
}

impl MemorySheetStore {
    pub fn get(&self, name: &str) -> Option<DocumentRows> {
        self.sheets.borrow().get(name).cloned()
    }

    /// Accepted saves so far.
    pub fn save_count(&self) -> usize {
        self.saves.get()
    }

    pub fn set_refusing(&self, refusing: bool) {
        self.refusing.set(refusing);
    }

    pub fn set_pending(&self, pending: bool) {
        self.pending.set(pending);
    }
}

impl SheetStore for MemorySheetStore {
    fn load(&self, name: &str) -> Loaded {
        if self.pending.get() {
            return Loaded::Pending;
        }
        match self.get(name) {
            Some(rows) => Loaded::Rows(rows),
            None => Loaded::Missing,
        }
    }

    fn save(&self, name: &str, rows: DocumentRows) -> bool {
        if self.refusing.get() {
            return false;
        }
        self.sheets.borrow_mut().insert(name.to_string(), rows);
        self.saves.set(self.saves.get() + 1);
        true
    }

    fn contains(&self, name: &str) -> bool {
        self.sheets.borrow().contains_key(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sheet::tests::{callspread, line, push, spx};
    use crate::core::{Sheet, from_rows, to_rows};
    use geode_core::pricing::OptionKind;

    fn rows() -> DocumentRows {
        let mut s = Sheet::new("book");
        push(
            &mut s,
            vec![line(spx(5000.0, OptionKind::Call), 1), callspread(-2)],
        );
        to_rows(&s).expect("a non-empty sheet has rows")
    }

    #[test]
    fn a_saved_sheet_loads_back_and_an_unknown_name_is_missing() {
        let store = MemorySheetStore::default();
        assert!(matches!(store.load("book"), Loaded::Missing));
        assert!(!store.contains("book"));
        assert!(store.save("book", rows()));
        assert!(store.contains("book"));
        let Loaded::Rows(back) = store.load("book") else {
            panic!("saved rows load")
        };
        assert_eq!(from_rows("book", &back).unwrap().len(), 4);
        assert_eq!(store.save_count(), 1);
    }

    #[test]
    fn a_refusing_store_keeps_nothing_and_a_pending_one_answers_pending() {
        let store = MemorySheetStore::default();
        store.set_refusing(true);
        assert!(!store.save("book", rows()));
        assert!(store.get("book").is_none());
        store.set_pending(true);
        assert!(matches!(store.load("book"), Loaded::Pending));
    }

    #[test]
    fn clones_share_one_map() {
        let a = MemorySheetStore::default();
        let b = a.clone();
        a.save("book", rows());
        assert!(
            b.contains("book"),
            "the factory and every tile see one store"
        );
    }
}
