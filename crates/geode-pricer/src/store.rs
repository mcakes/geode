//! Sheet loading and whole-document saves shared by pricer tiles.
//!
//! Loads return rows or absence immediately, `Pending` for an admitted
//! asynchronous request, or `Refused` when no request was submitted.
//! [`DuckSheetStore`] delivers admitted loads through the tile's
//! `Delivery::Query` route. Its saves publish whole documents; removals delete
//! both live and archived generations. Write methods report queue admission,
//! with completion delivered separately by sheet name. The tile decides how
//! to report or retry failures and skips empty sheets during conversion.
//!
//! [`DuckSheetStore`] is the store the app wires. [`MemorySheetStore`] is
//! the tests' fake: a sheet in it lives for the process, not across a
//! restart.

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
    /// No document under that name: open an empty sheet without blocking saves.
    Missing,
    /// On its way; the tile paints `loading` until `PricerTile::loaded`.
    Pending,
    /// The load was never submitted — a closed or full request channel.
    /// No answer will arrive. The caller must report a failed load and block
    /// saves so an empty fallback cannot overwrite the stored document.
    Refused,
}

pub trait SheetStore {
    /// Ask for the document named `name`, addressed by `key`/`tag` so a
    /// DuckDB-backed store's answer can be routed back and matched
    /// against the caller's latest request. `key`/`tag` are
    /// unused by a store that answers at once.
    fn load(&self, name: &str, key: QueryKey, tag: u64) -> Loaded;
    /// Submit the whole sheet. `false` means refusal; `true` means admission,
    /// with completion reported separately for asynchronous stores.
    fn save(&self, name: &str, rows: DocumentRows) -> bool;
    /// Submit deletion of the document's whole history. The return value
    /// reports admission, not asynchronous completion.
    fn forget(&self, name: &str) -> bool;
    /// Every name this store currently knows, for the `:e`/`:name`/`:rm`
    /// commands. Order is not significant to callers.
    fn names(&self) -> Vec<String>;
    /// Whether this store knows `name`. A cache-backed implementation can
    /// return false for a document absent from its latest catalog.
    fn contains(&self, name: &str) -> bool;
    /// A save of `name` is CONFIRMED (the data tier's local-publish
    /// outcome, never `save` answering `true`): a store keeping a cache of
    /// known names adds it. A store that answers `names` from its own
    /// contents has nothing to do.
    fn note_saved(&self, name: &str) {
        let _ = name;
    }
    /// A forget of `name` is CONFIRMED: a store keeping a cache of known
    /// names drops it.
    fn note_forgotten(&self, name: &str) {
        let _ = name;
    }
    /// Names a catalog of the store's documents holds. They are ADDED: a
    /// catalog never drops a name this store learned from its own
    /// confirmed saves, which a catalog read before that save landed would
    /// not carry yet. A name confirmed forgotten (and not saved since) is
    /// skipped: a catalog read before the forget would otherwise revive
    /// it. A store that answers `names` from its own contents has nothing
    /// to do.
    fn set_known(&self, names: Vec<String>) {
        let _ = names;
    }
}

/// The process-lifetime store. Clones share one map, so the factory and
/// every tile it builds see the same sheets. The knobs exist for the
/// tile's tests (a refused save, a pending or refused load, a save count,
/// the loads asked for, names known only once confirmed); the app never
/// turns them.
///
/// `names`/`contains` answer the map's names plus any `set_known` ones.
/// Under `set_confirming(true)` they answer only what was confirmed —
/// `set_known`, `note_saved`, less `note_forgotten` — the way
/// [`DuckSheetStore`] does, so a test can hold a save queued and
/// unconfirmed.
#[derive(Clone, Default)]
pub struct MemorySheetStore {
    sheets: Rc<RefCell<BTreeMap<String, DocumentRows>>>,
    known: Rc<RefCell<BTreeSet<String>>>,
    /// Confirmed forgotten and not saved since: `set_known` skips them, as
    /// [`DuckSheetStore`] does.
    forgotten: Rc<RefCell<BTreeSet<String>>>,
    confirming: Rc<Cell<bool>>,
    forgets: Rc<RefCell<Vec<String>>>,
    saves: Rc<Cell<usize>>,
    refusing: Rc<Cell<bool>>,
    pending: Rc<Cell<bool>>,
    load_refused: Rc<Cell<bool>>,
    loads: Rc<RefCell<Vec<(String, QueryKey, u64)>>>,
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

    /// While enabled, every load returns `Refused` without looking up rows.
    pub fn set_load_refused(&self, refused: bool) {
        self.load_refused.set(refused);
    }

    /// Every `load` asked so far, as `(name, key, tag)`.
    pub fn loads(&self) -> Vec<(String, QueryKey, u64)> {
        self.loads.borrow().clone()
    }

    /// Names are known only once confirmed (see the type's doc).
    pub fn set_confirming(&self, confirming: bool) {
        self.confirming.set(confirming);
    }

    /// Every `forget` asked so far, in order.
    pub fn forgets(&self) -> Vec<String> {
        self.forgets.borrow().clone()
    }
}

impl SheetStore for MemorySheetStore {
    fn load(&self, name: &str, key: QueryKey, tag: u64) -> Loaded {
        self.loads.borrow_mut().push((name.to_string(), key, tag));
        if self.load_refused.get() {
            return Loaded::Refused;
        }
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
        self.forgets.borrow_mut().push(name.to_string());
        self.sheets.borrow_mut().remove(name);
        if !self.confirming.get() {
            self.known.borrow_mut().remove(name);
        }
        true
    }

    fn names(&self) -> Vec<String> {
        let mut names = self.known.borrow().clone();
        if !self.confirming.get() {
            names.extend(self.sheets.borrow().keys().cloned());
        }
        names.into_iter().collect()
    }

    fn contains(&self, name: &str) -> bool {
        self.known.borrow().contains(name)
            || (!self.confirming.get() && self.sheets.borrow().contains_key(name))
    }

    fn note_saved(&self, name: &str) {
        self.forgotten.borrow_mut().remove(name);
        if self.confirming.get() {
            self.known.borrow_mut().insert(name.to_string());
        }
    }

    fn note_forgotten(&self, name: &str) {
        self.known.borrow_mut().remove(name);
        self.forgotten.borrow_mut().insert(name.to_string());
    }

    fn set_known(&self, names: Vec<String>) {
        let forgotten = self.forgotten.borrow();
        self.known
            .borrow_mut()
            .extend(names.into_iter().filter(|n| !forgotten.contains(n)));
    }
}

/// The production store: `pricer_sheets` document reads and writes over
/// `DataHandle`. `load` submits a document request and answers `Pending`
/// (the answer arrives through the tile's own `Delivery::Query`, decoded
/// by `core::storage::rows_from_snapshot`) or `Refused` when the
/// request channel itself refuses admission — a load that will never
/// answer. `save` and `forget` likewise only queue the write; whether it
/// lands reaches the tile separately, by sheet name, through the bridge.
/// Loads are always `AsOf::Live`: a sheet does not follow the frame's as-of.
///
/// `known` answers `names`/`contains` without asking the data tier: it
/// is seeded and refreshed from the diagnostics catalog's `pricer_sheets`
/// partitions (`set_known`) and kept current by
/// `note_saved`/`note_forgotten`, which the caller invokes once a write's
/// outcome is confirmed — never on submission, since a refused or still
/// in-flight write must not appear known.
pub struct DuckSheetStore {
    data: DataHandle,
    known: Rc<RefCell<BTreeSet<String>>>,
    /// Names confirmed forgotten and not saved since. The catalog the
    /// diagnostics entity holds is refreshed only while a diagnostics tile
    /// watches it, so a catalog read before a forget can be re-read long
    /// after it: without this, it would make the forgotten name known
    /// again.
    forgotten: Rc<RefCell<BTreeSet<String>>>,
}

impl DuckSheetStore {
    pub fn new(data: DataHandle) -> Self {
        DuckSheetStore {
            data,
            known: Rc::new(RefCell::new(BTreeSet::new())),
            forgotten: Rc::new(RefCell::new(BTreeSet::new())),
        }
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

    /// A confirmed save: `name` is now a known document, and a catalog
    /// may name it again.
    fn note_saved(&self, name: &str) {
        self.forgotten.borrow_mut().remove(name);
        self.known.borrow_mut().insert(name.to_string());
    }

    /// A confirmed forget: `name` is no longer a known document, and no
    /// catalog brings it back until a save of it is confirmed.
    fn note_forgotten(&self, name: &str) {
        self.known.borrow_mut().remove(name);
        self.forgotten.borrow_mut().insert(name.to_string());
    }

    /// Add names from a catalog snapshot, skipping every name this store
    /// saw forgotten; nothing already known is removed (a name this store
    /// learned from its own writes stays even if a catalog it is passed
    /// does not carry it yet).
    fn set_known(&self, names: Vec<String>) {
        let forgotten = self.forgotten.borrow();
        self.known
            .borrow_mut()
            .extend(names.into_iter().filter(|n| !forgotten.contains(n)));
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
    fn a_confirming_store_knows_a_name_only_once_confirmed() {
        let store = MemorySheetStore::default();
        store.set_confirming(true);
        assert!(store.save("book", rows()));
        assert!(!store.contains("book"), "queued, not confirmed");
        assert!(store.names().is_empty());
        store.note_saved("book");
        assert!(store.contains("book"));
        store.set_known(vec!["alpha".into()]);
        assert_eq!(store.names(), vec!["alpha".to_string(), "book".to_string()]);
        assert!(store.forget("book"));
        assert!(store.contains("book"), "a forget is known once confirmed");
        store.note_forgotten("book");
        assert_eq!(store.names(), vec!["alpha".to_string()]);
        assert_eq!(store.forgets(), vec!["book".to_string()]);
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

        /// A catalog read before a forget never revives the name; a
        /// confirmed save of it does.
        #[test]
        fn a_catalog_never_revives_a_forgotten_name_until_it_is_saved() {
            let (handle, _rx) = DataHandle::for_tests();
            let store = DuckSheetStore::new(handle);
            store.set_known(vec!["gone".to_string()]);
            store.note_forgotten("gone");
            store.set_known(vec!["gone".to_string(), "other".to_string()]);
            assert!(!store.contains("gone"));
            assert!(store.contains("other"));
            store.note_saved("gone");
            assert!(store.contains("gone"));
        }

        #[test]
        fn names_and_contains_are_known_union_saved_minus_forgotten() {
            let (handle, _rx) = DataHandle::for_tests();
            let store = DuckSheetStore::new(handle);
            store.set_known(vec!["alpha".to_string(), "beta".to_string()]);
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
