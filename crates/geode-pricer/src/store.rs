//! Where a sheet lives between tiles (line-pricer spec §7.1): the tile
//! never holds a document request of its own, only this seam.
//!
//! **The shape is Part 3's** (planning decision 7): `load` answers at
//! once when it can and `Pending` when the answer is on its way — Part
//! 4's [`DuckSheetStore`] answers `Pending` and delivers the rows through
//! the tile's `loaded`, reached from its `Delivery::Query` arm. `save` is
//! one whole-sheet publish; `false` is a refusal the tile shows and
//! retries on the next edit burst (spec §7.3). A zero-row sheet is never
//! saved (`to_rows` answers `None`; spec §7.2). `forget` deletes the
//! whole document, live and archived (spec §7.4's `:rm`).
//!
//! [`MemorySheetStore`] is the tests' fake and the app's store until Part
//! 4 wires [`DuckSheetStore`] in. A sheet in it lives for the process,
//! not across a restart.

use crate::core::storage::PRICER_SHEETS_DATASET;
use geode_core::document::DocumentRows;
use geode_core::pricing::LocalPublish;
use geode_core::query::{AsOf, DocumentParams, QueryKey};
use geode_data::{DataHandle, LocalForget};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::time::Instant;

/// What a `load` answered.
#[derive(Debug)]
pub enum Loaded {
    Rows(DocumentRows),
    /// No document under that name: an empty sheet, not an error (§7.2).
    Missing,
    /// On its way; the tile paints `loading` until `PricerTile::loaded`.
    Pending,
    /// The load was never submitted — a closed or full request channel.
    /// Nothing is coming: the caller must not wait on it the way it
    /// waits on `Pending` (spec §7.1's failed-load path applies at
    /// once, not a `loading` state that never resolves).
    Refused,
}

pub trait SheetStore {
    /// Ask for the document named `name`, addressed by `key`/`tag` so a
    /// DuckDB-backed store's answer can be routed back and matched
    /// against the caller's latest request (spec §7.1). `key`/`tag` are
    /// unused by a store that answers at once.
    fn load(&self, name: &str, key: QueryKey, tag: u64) -> Loaded;
    /// Publish the whole sheet. `false`: refused, nothing written.
    fn save(&self, name: &str, rows: DocumentRows) -> bool;
    /// Delete the document's whole history. `false`: refused, nothing
    /// changed.
    fn forget(&self, name: &str) -> bool;
    /// Every name this store currently knows, for the `:e`/`:name`/`:rm`
    /// vocabulary (spec §7.4) — order is not significant to callers.
    fn names(&self) -> Vec<String>;
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
    fn load(&self, name: &str, _key: QueryKey, _tag: u64) -> Loaded {
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

    fn forget(&self, name: &str) -> bool {
        self.sheets.borrow_mut().remove(name);
        true
    }

    fn names(&self) -> Vec<String> {
        self.sheets.borrow().keys().cloned().collect()
    }

    fn contains(&self, name: &str) -> bool {
        self.sheets.borrow().contains_key(name)
    }
}

/// The production store: `pricer_sheets` document reads and writes over
/// `DataHandle`. `load` submits a document request and answers `Pending`
/// (the answer arrives through the tile's own `Delivery::Query`, decoded
/// by `core::storage::rows_from_snapshot`) or `Refused` when the
/// request channel itself refuses admission — a load that will never
/// answer. `save` and `forget` likewise only queue the write; whether it
/// lands reaches the tile separately, by sheet name, through the bridge
/// (spec §7.3, planning decision 6). Loads are always `AsOf::Live`: a
/// sheet does not follow the frame's as-of (planning decision 4).
///
/// `known` answers `names`/`contains` without asking the data tier: it
/// is seeded and refreshed from the diagnostics catalog's `pricer_sheets`
/// partitions (`set_known`, planning decision 12) and kept current by
/// `note_saved`/`note_forgotten`, which the caller invokes once a write's
/// outcome is confirmed — never on submission, since a refused or still
/// in-flight write must not appear known.
pub struct DuckSheetStore {
    data: DataHandle,
    known: Rc<RefCell<BTreeSet<String>>>,
}

impl DuckSheetStore {
    pub fn new(data: DataHandle) -> Self {
        DuckSheetStore {
            data,
            known: Rc::new(RefCell::new(BTreeSet::new())),
        }
    }

    /// Add names from a fresh catalog snapshot; nothing already known is
    /// removed by this (a name this store learned from its own writes
    /// stays even if a catalog it is passed does not carry it yet).
    pub fn set_known(&self, names: impl IntoIterator<Item = String>) {
        self.known.borrow_mut().extend(names);
    }

    /// A confirmed save: `name` is now a known document.
    pub fn note_saved(&self, name: &str) {
        self.known.borrow_mut().insert(name.to_string());
    }

    /// A confirmed forget: `name` is no longer a known document.
    pub fn note_forgotten(&self, name: &str) {
        self.known.borrow_mut().remove(name);
    }
}

impl SheetStore for DuckSheetStore {
    fn load(&self, name: &str, key: QueryKey, tag: u64) -> Loaded {
        let queued = self.data.document(DocumentParams {
            key,
            tag,
            submitted: Instant::now(),
            dataset: PRICER_SHEETS_DATASET.to_string(),
            document_key: vec![name.to_string()],
            as_of: AsOf::Live,
        });
        if queued {
            Loaded::Pending
        } else {
            Loaded::Refused
        }
    }

    fn save(&self, name: &str, rows: DocumentRows) -> bool {
        // `rows.key` already carries the sheet name (`to_rows` sets it);
        // `name` names the caller's intent for readers of this call site.
        let _ = name;
        self.data.publish(LocalPublish {
            dataset: PRICER_SHEETS_DATASET.to_string(),
            rows,
        })
    }

    fn forget(&self, name: &str) -> bool {
        self.data.forget(LocalForget {
            dataset: PRICER_SHEETS_DATASET.to_string(),
            key: vec![name.to_string()],
        })
    }

    fn names(&self) -> Vec<String> {
        self.known.borrow().iter().cloned().collect()
    }

    fn contains(&self, name: &str) -> bool {
        self.known.borrow().contains(name)
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
        assert!(matches!(
            store.load("book", QueryKey(1), 0),
            Loaded::Missing
        ));
        assert!(!store.contains("book"));
        assert!(store.save("book", rows()));
        assert!(store.contains("book"));
        assert_eq!(store.names(), vec!["book".to_string()]);
        let Loaded::Rows(back) = store.load("book", QueryKey(1), 0) else {
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
        assert!(matches!(
            store.load("book", QueryKey(1), 0),
            Loaded::Pending
        ));
    }

    #[test]
    fn forget_removes_the_entry_and_drops_it_from_names() {
        let store = MemorySheetStore::default();
        assert!(store.save("book", rows()));
        assert!(store.forget("book"));
        assert!(!store.contains("book"));
        assert!(store.names().is_empty());
        // Forgetting an absent name is still accepted: nothing to undo.
        assert!(store.forget("book"));
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

    mod duck_store {
        use super::*;
        use geode_core::document::Column;
        use geode_data::Request;
        use std::time::Duration;

        fn sheet_rows(name: &str) -> DocumentRows {
            DocumentRows {
                key: vec![name.to_string()],
                attributes: Vec::new(),
                axes: vec![("line".into(), Column::I64(vec![0]))],
                values: vec![("order".into(), Column::I64(vec![0]))],
            }
        }

        #[test]
        fn load_submits_a_document_request_addressed_by_key_and_tag() {
            let (handle, rx) = DataHandle::for_tests();
            let store = DuckSheetStore::new(handle);
            assert!(matches!(
                store.load("book", QueryKey(7), 3),
                Loaded::Pending
            ));
            match rx.recv_timeout(Duration::from_secs(1)).unwrap() {
                Request::Document(p) => {
                    assert_eq!(p.key, QueryKey(7));
                    assert_eq!(p.tag, 3);
                    assert_eq!(p.dataset, PRICER_SHEETS_DATASET);
                    assert_eq!(p.document_key, vec!["book".to_string()]);
                    assert_eq!(p.as_of, AsOf::Live);
                }
                other => panic!("expected a Document request, got {other:?}"),
            }
        }

        #[test]
        fn a_closed_channel_answers_refused_not_pending() {
            let (handle, rx) = DataHandle::for_tests();
            drop(rx);
            let store = DuckSheetStore::new(handle);
            assert!(matches!(
                store.load("book", QueryKey(1), 0),
                Loaded::Refused
            ));
        }

        #[test]
        fn save_submits_a_publish_request_with_the_dataset_and_rows() {
            let (handle, rx) = DataHandle::for_tests();
            let store = DuckSheetStore::new(handle);
            let rows = sheet_rows("book");
            assert!(store.save("book", rows.clone()));
            match rx.recv_timeout(Duration::from_secs(1)).unwrap() {
                Request::Publish(p) => {
                    assert_eq!(p.dataset, PRICER_SHEETS_DATASET);
                    assert_eq!(p.rows, rows);
                }
                other => panic!("expected a Publish request, got {other:?}"),
            }
        }

        #[test]
        fn forget_submits_a_forget_request_naming_the_dataset_and_key() {
            let (handle, rx) = DataHandle::for_tests();
            let store = DuckSheetStore::new(handle);
            assert!(store.forget("book"));
            match rx.recv_timeout(Duration::from_secs(1)).unwrap() {
                Request::Forget(f) => {
                    assert_eq!(f.dataset, PRICER_SHEETS_DATASET);
                    assert_eq!(f.key, vec!["book".to_string()]);
                }
                other => panic!("expected a Forget request, got {other:?}"),
            }
        }

        #[test]
        fn names_and_contains_are_known_union_saved_minus_forgotten() {
            let (handle, _rx) = DataHandle::for_tests();
            let store = DuckSheetStore::new(handle);
            store.set_known(["alpha".to_string(), "beta".to_string()]);
            store.note_saved("gamma");
            assert!(store.contains("alpha"));
            assert!(store.contains("beta"));
            assert!(store.contains("gamma"));
            assert_eq!(
                store.names(),
                vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]
            );
            store.note_forgotten("beta");
            assert!(!store.contains("beta"));
            assert_eq!(
                store.names(),
                vec!["alpha".to_string(), "gamma".to_string()]
            );
        }
    }
}
