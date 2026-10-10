//! The resolved watchlists behind `WatchlistGlobal`: the fold of every list
//! in the config against the schema the service serves, and the cache that
//! resolves each list through the data service and keeps the snapshot.
//!
//! The global is replaced on every accepted answer, including one whose
//! members did not change: `resolved_at` moves, and the tile shows it as
//! "as of". An observer that only wants member changes compares the
//! members it holds.

use chrono::Utc;
use geode_core::config::{Config, Diagnostic, EXPRESSIONS_DOC, Layer, Severity, WATCHLISTS_DOC};
use geode_core::dimensions::DerivedDimensions;
use geode_core::named::NamedExpressions;
use geode_core::query::{QueryKey, WatchlistOutcome, WatchlistParams};
use geode_core::schema::SchemaSpec;
use geode_core::scope::complete::ExprVocab;
use geode_core::watchlist::fold::{RuleError, fold_rules};
use geode_core::watchlist::state::{
    Folded, Status, WatchlistState, diff_definitions, lists_naming,
};
use geode_data::{DataHandle, Refusal};
use geode_shell::shell::objectdialog::shadow_of;
use geode_shell::shell::{WATCHLIST_KEY_BASE, WATCHLIST_KEY_COUNT};
use geode_shell::watchlist::WatchlistGlobal;
use gpui::{App, AsyncApp, WindowHandle};
use gpui_component::Root;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// Each object's winning layer, and the objects whose user copy shadows a
/// definition in a lower layer, with that layer: the ones a revert would
/// restore, and what it restores. Read from the layered documents, the
/// same provenance the object dialog badges.
pub fn object_provenance<'a>(
    config: &Config,
    doc: &str,
    names: impl Iterator<Item = &'a str>,
) -> (BTreeMap<String, Layer>, BTreeMap<String, Layer>) {
    let layers: BTreeMap<String, Layer> = names
        .filter_map(|n| config.explain(doc, n).map(|l| (n.to_string(), l)))
        .collect();
    let shadowed = layers
        .iter()
        .filter(|(_, layer)| **layer == Layer::User)
        .filter_map(|(name, _)| {
            shadow_of(config, doc, name).map(|(lower, _)| (name.clone(), lower))
        })
        .collect();
    (layers, shadowed)
}

/// Every list in the config, folded against the schema the service serves.
/// Saved scopes and named expressions are read from the same config, as
/// the shell's reload reads them, so the fold does not depend on the frame
/// having applied the reload first. A rule that does not fold is a warning
/// here and a `RuleError` on its list; the list itself is kept.
pub fn fold_watchlists(
    config: &Config,
    schema: &SchemaSpec,
    dims: &DerivedDimensions,
) -> (BTreeMap<String, Folded>, Vec<Diagnostic>) {
    let Some(doc) = config.doc(WATCHLISTS_DOC) else {
        return (BTreeMap::new(), Vec::new());
    };
    let (lists, mut diags) = geode_core::watchlist::from_doc(doc);
    // `geode_core::config` exports no constant for the scopes doc.
    let saved = config
        .doc("scopes")
        .map(|d| geode_core::scopes::saved_scopes_from_doc(d, schema, dims).0)
        .unwrap_or_default();
    let vocab = ExprVocab::new(schema, dims);
    let named = config
        .doc(EXPRESSIONS_DOC)
        .map(|d| NamedExpressions::from_doc(d, &vocab).0)
        .unwrap_or_default();
    let (layers, shadowed) = object_provenance(config, WATCHLISTS_DOC, lists.names());
    let mut out = BTreeMap::new();
    for (name, list) in lists.iter() {
        let (rules, errors) = fold_rules(list, schema, dims, &saved, &named);
        for e in &errors {
            diags.push(Diagnostic {
                severity: Severity::Warning,
                layer: None,
                file: None,
                message: format!("watchlist '{name}' rule {}: {}", e.index + 1, e.reason),
                path: Some(format!("watchlists.{name}.rules[{}]", e.index)),
            });
        }
        out.insert(
            name.clone(),
            Folded {
                list: list.clone(),
                rules,
                errors,
                layer: layers.get(name).copied(),
                shadowed: shadowed.get(name).copied(),
            },
        );
    }
    (out, diags)
}

/// The resolved lists behind `WatchlistGlobal`, one lane per list, each
/// under ITS OWN query key from the `WATCHLIST_KEY_BASE` range (the pool
/// and the mailbox coalesce per key, so a shared key would let one list's
/// refresh cancel another's). Keys are handed out on first sight of a name
/// and never reused within the run. Only a list's latest tag is applied,
/// so a slow answer cannot replace a newer one. A refused read keeps its
/// demand on a timer; a failed resolution keeps the last members and marks
/// the list `Failed`.
pub struct WatchlistCache {
    handle: DataHandle,
    folded: RefCell<BTreeMap<String, Folded>>,
    /// Each list's latest submitted tag. Kept when a list is removed, so a
    /// list defined again under the same name continues its tags and the
    /// answer to its old definition, if still on its way, is dropped.
    tags: RefCell<HashMap<String, u64>>,
    /// Per list, the last tag submitted for a definition since replaced or
    /// removed: an answer at or below it belongs to that old definition and
    /// is dropped, even while it is still the latest tag because the new
    /// definition's refresh was refused and waits on its retry.
    floor: RefCell<HashMap<String, u64>>,
    /// Each list's own key, allocated monotonically from the range.
    keys: RefCell<HashMap<String, QueryKey>>,
    next_key: Cell<u64>,
    /// Lists refused a key, so the exhausted range is logged once each.
    unkeyed: RefCell<HashSet<String>>,
    /// Lists with a retry timer armed: at most one each. The timer owns its
    /// entry: it clears it when it fires, whether or not the list still
    /// exists, so a list removed and re-added keeps the one timer.
    retry: RefCell<HashSet<String>>,
    /// Lists whose last answer was an error, so a run of failures warns once.
    failing: RefCell<HashSet<String>>,
}

pub(crate) const WATCHLIST_RETRY_DELAY: Duration = Duration::from_secs(1);

impl WatchlistCache {
    pub fn new(handle: DataHandle) -> WatchlistCache {
        WatchlistCache {
            handle,
            folded: RefCell::default(),
            tags: RefCell::default(),
            floor: RefCell::default(),
            keys: RefCell::default(),
            next_key: Cell::new(0),
            unkeyed: RefCell::default(),
            retry: RefCell::default(),
            failing: RefCell::default(),
        }
    }

    /// Replace every definition: lists no longer defined leave the
    /// snapshot, new and changed ones are resolved again, and the rest keep
    /// their members and resolution under their new provenance.
    pub fn set_definitions(
        self: &Rc<Self>,
        folded: BTreeMap<String, Folded>,
        window: WindowHandle<Root>,
        cx: &mut App,
    ) {
        let diff = diff_definitions(&self.folded.borrow(), &folded);
        *self.folded.borrow_mut() = folded;
        {
            // Whatever was submitted for a replaced or removed definition
            // is now beneath the floor, whether or not its refresh is
            // accepted before that answer arrives.
            let tags = self.tags.borrow();
            let mut floor = self.floor.borrow_mut();
            for name in diff.resolve.iter().chain(&diff.removed) {
                floor.insert(name.clone(), tags.get(name).copied().unwrap_or(0));
            }
        }
        for name in &diff.removed {
            // A list defined again fails afresh: its first failure warns.
            self.failing.borrow_mut().remove(name);
        }
        let current = cx.global::<WatchlistGlobal>().0.clone();
        let mut next = (*current).clone();
        for name in &diff.removed {
            next.lists.remove(name);
        }
        {
            let folded = self.folded.borrow();
            for (name, f) in folded.iter() {
                let resolving = diff.resolve.contains(name);
                // A new list is always in `diff.resolve`, which fills its
                // `rule_errors` below.
                let state = next
                    .lists
                    .entry(name.clone())
                    .or_insert_with(|| WatchlistState {
                        definition: f.list.clone(),
                        layer: f.layer,
                        shadowed: f.shadowed,
                        rule_errors: Vec::new(),
                        members: Vec::new(),
                        resolved_at: None,
                        status: Status::Resolving,
                    });
                state.definition = f.list.clone();
                state.layer = f.layer;
                state.shadowed = f.shadowed;
                if resolving {
                    // The rules the data layer could not run belonged to the
                    // old definition; the next answer brings the new ones.
                    state.rule_errors = f.errors.clone();
                    state.status = Status::Resolving;
                }
            }
        }
        if next != *current {
            cx.set_global(WatchlistGlobal(Arc::new(next)));
        }
        for name in &diff.resolve {
            self.refresh(name, window, cx);
        }
    }

    /// A publish of `dataset`: resolve again every list with a good rule
    /// over it.
    pub fn on_published(self: &Rc<Self>, dataset: &str, window: WindowHandle<Root>, cx: &mut App) {
        let names: Vec<String> = lists_naming(&self.folded.borrow(), dataset)
            .into_iter()
            .map(str::to_string)
            .collect();
        for name in &names {
            self.refresh(name, window, cx);
        }
    }

    /// The list's key, allocated on first sight. `None` once the range is
    /// exhausted, which no run reaches: that list is not resolved.
    pub fn key_of(&self, name: &str) -> Option<QueryKey> {
        if let Some(key) = self.keys.borrow().get(name) {
            return Some(*key);
        }
        let n = self.next_key.get();
        if n >= WATCHLIST_KEY_COUNT {
            if self.unkeyed.borrow_mut().insert(name.to_string()) {
                tracing::error!(
                    target: "geode::watchlist",
                    "watchlist '{name}' is not resolved: all {WATCHLIST_KEY_COUNT} query keys \
                     of the run are allocated"
                );
            }
            return None;
        }
        self.next_key.set(n + 1);
        let key = QueryKey(WATCHLIST_KEY_BASE.0 + n);
        self.keys.borrow_mut().insert(name.to_string(), key);
        Some(key)
    }

    /// Resolve `name` under a new tag, superseding any resolution in
    /// flight. `Busy` arms a retry; `Stopped` drops the demand, since
    /// nothing would ever serve it. A name no longer defined (a late timer
    /// for a removed list) is ignored. `window` is the window the cache
    /// serves; its closure ends a retry.
    pub fn refresh(self: &Rc<Self>, name: &str, window: WindowHandle<Root>, cx: &mut App) {
        let params = {
            let folded = self.folded.borrow();
            let Some(f) = folded.get(name) else { return };
            let Some(key) = self.key_of(name) else { return };
            // The tag becomes the latest only once submitted: a refused
            // read sends nothing, and advancing the tag anyway would drop
            // the answer to the resolution still in flight.
            let tag = self.tags.borrow().get(name).copied().unwrap_or(0) + 1;
            WatchlistParams {
                key,
                tag,
                name: name.to_string(),
                rules: f.rules.clone(),
                include: f.list.include.clone(),
                exclude: f.list.exclude.clone(),
            }
        };
        let tag = params.tag;
        match self.handle.watchlist(params) {
            Ok(()) => {
                self.tags.borrow_mut().insert(name.to_string(), tag);
            }
            Err(Refusal::Busy) => self.retry(name, window, cx),
            Err(Refusal::Stopped) => {}
        }
    }

    /// Resolve again after the delay. A list already waiting keeps its one
    /// timer, so a burst of refused publishes cannot pile up resolutions.
    /// The timer holds the cache weakly and checks the window: the drain
    /// keeps the cache alive until its next event, so only the window's
    /// closure reliably ends the lane.
    fn retry(self: &Rc<Self>, name: &str, window: WindowHandle<Root>, cx: &mut App) {
        if !self.retry.borrow_mut().insert(name.to_string()) {
            return;
        }
        let cache = Rc::downgrade(self);
        let name = name.to_string();
        cx.spawn(async move |cx: &mut AsyncApp| {
            cx.background_executor().timer(WATCHLIST_RETRY_DELAY).await;
            cx.update(|cx| {
                if let Some(cache) = cache.upgrade() {
                    cache.retry.borrow_mut().remove(&name);
                    if window.read(cx).is_ok() {
                        cache.refresh(&name, window, cx);
                    }
                }
            });
        })
        .detach();
    }

    /// Apply an answer: members and `Current`, or `Failed` over the last
    /// members. Answers under another key than the list's, a superseded
    /// tag, a tag of a definition since replaced, or for a list since
    /// removed are dropped.
    pub fn answer(&self, outcome: WatchlistOutcome, cx: &mut App) {
        if self.keys.borrow().get(&outcome.name) != Some(&outcome.key)
            || self.tags.borrow().get(&outcome.name) != Some(&outcome.tag)
            || outcome.tag <= self.floor.borrow().get(&outcome.name).copied().unwrap_or(0)
        {
            return;
        }
        let Some(fold_errors) = self
            .folded
            .borrow()
            .get(&outcome.name)
            .map(|f| f.errors.clone())
        else {
            return;
        };
        let current = cx.global::<WatchlistGlobal>().0.clone();
        let mut next = (*current).clone();
        let Some(state) = next.lists.get_mut(&outcome.name) else {
            return;
        };
        match outcome.result {
            Ok(res) => {
                self.failing.borrow_mut().remove(&outcome.name);
                state.members = res.members;
                state.resolved_at = Some(Utc::now());
                state.status = Status::Current;
                // Rules the data layer could not run join the fold's errors
                // for this answer.
                state.rule_errors = fold_errors
                    .into_iter()
                    .chain(
                        res.rules_failed
                            .into_iter()
                            .map(|(index, reason)| RuleError { index, reason }),
                    )
                    .collect();
            }
            Err(e) => {
                if self.failing.borrow_mut().insert(outcome.name.clone()) {
                    tracing::warn!(
                        target: "geode::watchlist",
                        "watchlist '{}' did not resolve: {e}",
                        outcome.name
                    );
                }
                state.status = Status::Failed(e);
            }
        }
        if next != *current {
            cx.set_global(WatchlistGlobal(Arc::new(next)));
        }
    }

    #[cfg(test)]
    pub(crate) fn retry_count(&self) -> usize {
        self.retry.borrow().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{ConfigSources, LayerDoc, merge_docs};

    /// `risk` carries `underlying_ref`, `book` and `npv`.
    const DATASETS: &str = "[risk]\n\
        [risk.columns.book]\nrole = \"dimension\"\ntype = \"utf8\"\ngrain = \"position\"\n\
        [risk.columns.underlying_ref]\nrole = \"dimension\"\ntype = \"utf8\"\ngrain = \"position\"\n\
        [risk.columns.npv]\nrole = \"measure\"\ntype = \"f64\"\ngrain = \"position\"\n";

    fn config(docs: &[(&str, &str)]) -> Config {
        Config::load(&ConfigSources {
            builtin: docs
                .iter()
                .map(|(name, text)| LayerDoc::builtin(name, text).unwrap())
                .collect(),
            desk: None,
            user: None,
        })
    }

    fn startup_schema(config: &Config) -> SchemaSpec {
        SchemaSpec::from_doc(config.doc("datasets").unwrap()).0
    }

    #[test]
    fn fold_reads_lists_scopes_and_expressions_from_one_config() {
        let config = config(&[
            ("datasets", DATASETS),
            (
                "scopes",
                "[eu]\nnamed = [\"big\"]\n[eu.dimensions]\nbook = [\"BK000\"]\n",
            ),
            (EXPRESSIONS_DOC, "[big]\nexpression = \"npv > 1\"\n"),
            (
                WATCHLISTS_DOC,
                "[a]\n[[a.rules]]\ndataset = \"risk\"\nscope = \"eu\"\n\
                 [b]\n[[b.rules]]\ndataset = \"risk\"\nexpression = \"npv > \"\n",
            ),
        ]);
        let schema = startup_schema(&config);
        let (folded, diags) = fold_watchlists(&config, &schema, &DerivedDimensions::default());
        let a = &folded["a"];
        assert_eq!(a.rules.len(), 1);
        assert_eq!(a.rules[0].scope.dimensions[0].values, vec!["BK000"]);
        assert!(
            a.rules[0].scope.expression.is_some() && a.rules[0].scope.named.is_empty(),
            "the scope's named expression is folded in: {:?}",
            a.rules[0].scope
        );
        assert!(a.errors.is_empty());
        assert_eq!(a.layer, Some(Layer::Builtin));
        assert_eq!(a.shadowed, None);
        let b = &folded["b"];
        assert!(b.rules.is_empty());
        assert_eq!(b.errors.len(), 1);
        assert_eq!(b.errors[0].index, 0);
        let diag = diags
            .iter()
            .find(|d| d.message.starts_with("watchlist 'b' rule 1:"))
            .unwrap_or_else(|| panic!("{diags:?}"));
        assert_eq!(diag.severity, Severity::Warning);
        assert_eq!(diag.path.as_deref(), Some("watchlists.b.rules[0]"));
    }

    #[test]
    fn a_rule_over_a_dataset_the_service_does_not_serve_is_a_rule_error() {
        // The config declares `later` too, but the service started on a
        // schema without it: a restart is needed before a rule can read it.
        let config = config(&[
            (
                "datasets",
                &format!(
                    "{DATASETS}[later]\n[later.columns.underlying_ref]\n\
                     role = \"dimension\"\ntype = \"utf8\"\ngrain = \"position\"\n"
                ),
            ),
            (
                WATCHLISTS_DOC,
                "[a]\ninclude = [\"SPX\"]\n[[a.rules]]\ndataset = \"later\"\n",
            ),
        ]);
        let startup = SchemaSpec::from_doc(&merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", DATASETS).unwrap()],
        ))
        .0;
        let (folded, _) = fold_watchlists(&config, &startup, &DerivedDimensions::default());
        let a = &folded["a"];
        assert!(a.rules.is_empty());
        assert_eq!(a.errors.len(), 1);
        assert!(
            a.errors[0].reason.contains("restart"),
            "{}",
            a.errors[0].reason
        );
        assert_eq!(a.list.include, vec!["SPX"], "the list itself is kept");
    }

    #[test]
    fn no_watchlists_document_folds_to_nothing() {
        let config = config(&[("datasets", DATASETS)]);
        let schema = startup_schema(&config);
        let (folded, diags) = fold_watchlists(&config, &schema, &DerivedDimensions::default());
        assert!(folded.is_empty());
        assert!(diags.is_empty());
    }

    #[test]
    fn a_user_copy_over_a_builtin_list_is_shadowed_by_builtin() {
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("watchlists.toml"),
            "[a]\ninclude = [\"SPX\"]\n[c]\ninclude = [\"NDX\"]\n",
        )
        .unwrap();
        let config = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("datasets", DATASETS).unwrap(),
                LayerDoc::builtin(WATCHLISTS_DOC, "[a]\ninclude = [\"SX5E\"]\n[b]\n").unwrap(),
            ],
            desk: None,
            user: Some(user.path().to_path_buf()),
        });
        let (layers, shadowed) = object_provenance(
            &config,
            WATCHLISTS_DOC,
            ["a", "b", "c", "nonesuch"].into_iter(),
        );
        assert_eq!(layers.get("a"), Some(&Layer::User));
        assert_eq!(layers.get("b"), Some(&Layer::Builtin));
        assert_eq!(layers.get("c"), Some(&Layer::User));
        assert_eq!(layers.get("nonesuch"), None);
        assert_eq!(shadowed.get("a"), Some(&Layer::Builtin));
        assert_eq!(shadowed.get("b"), None, "no user copy");
        assert_eq!(shadowed.get("c"), None, "nothing beneath the user copy");
        let schema = startup_schema(&config);
        let (folded, _) = fold_watchlists(&config, &schema, &DerivedDimensions::default());
        assert_eq!(folded["a"].list.include, vec!["SPX"], "the user copy wins");
        assert_eq!(folded["a"].layer, Some(Layer::User));
        assert_eq!(folded["a"].shadowed, Some(Layer::Builtin));
    }
}
