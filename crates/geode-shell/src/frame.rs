//! The shared frame: global scope, undo/redo history, the active grouping slot,
//! as-of (with one remembered previous value), recent publishes, saved
//! scopes, and the data and config generations, as one value every tile
//! observes. Pure: `ShellView` holds it in a gpui entity and notifies; a
//! module reads it through that entity.
//!
//! Every mutation bumps exactly the counters it affects, so a tile can
//! compare the fields it follows against the ones it last acted on with
//! one integer compare each — a pinned tile ignores `grouping`, an
//! unscoped tile ignores `scope`. Publication watches narrow `data` to the
//! datasets/documents a consumer reads; other counters retain their contracts.

use crate::perf::RequeryStats;
use crate::scopebar::{self, ScopeBarModel};
use geode_core::config::Layer;
use geode_core::groupings::GroupingSlots;
use geode_core::named::NamedExpressions;
use geode_core::query::{AsOf, QueryKey};
use geode_core::scope::{Expr, Scope};
use geode_core::scopes::SavedScopes;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};
use toml_edit::value;

/// Minimum barrier age before [`Frame::sweep`] releases incomplete work.
/// The caller must schedule sweeps; the duration alone does not release it.
pub const FLIP_DEADLINE: Duration = Duration::from_millis(250);

/// Maximum stored scope undo depth, bounding retained scope allocations.
pub const UNDO_DEPTH: usize = 32;

/// Maximum recent publishes retained, most recently received first.
pub const RECENT_PUBLISHES: usize = 32;

/// [`Frame::replace_expression_term`]'s refusal: the scope no longer has
/// the expected term at that index (it changed since the caller read it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermGone;

/// Publication metadata for the history and as-of picker, independent of
/// the data crate's event types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Publish {
    pub dataset: String,
    pub batch: String,
    pub books: usize,
    pub at: chrono::DateTime<chrono::Utc>,
}

/// A tile's retained interest in a dataset, or one document within it.
/// Hidden tiles keep their watches, so no event history or catch-up scan is
/// needed. The frame retains only weak references; closed tiles release them.
/// Read through `Frame::versions_for` wherever a request or staged result is
/// compared, so requery and promotion use exactly the same dependency boundary.
#[derive(Debug, Clone)]
pub struct PublicationWatch {
    dataset: String,
    batch: Option<String>,
    revision: Rc<Cell<u64>>,
}

impl PublicationWatch {
    pub fn matches(&self, dataset: &str, batch: Option<&str>) -> bool {
        self.dataset == dataset && self.batch.as_deref() == batch
    }
}

#[derive(Debug, Default)]
struct DatasetWatches {
    dataset: Weak<Cell<u64>>,
    documents: HashMap<String, Weak<Cell<u64>>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameVersions {
    pub scope: u64,
    pub grouping: u64,
    pub as_of: u64,
    /// Global on `Frame::versions`, dependency-specific on `Frame::versions_for`.
    pub data: u64,
    pub config: u64,
    /// Bumped by [`Frame::save_scope`] without changing `config`: saving a
    /// named snapshot leaves the active query inputs unchanged. Consumers that
    /// only follow scope/grouping/as-of/data/config need not requery.
    pub saved_scopes: u64,
    /// Barrier release generation. Used to promote staged results after all
    /// arrivals or a timeout; it is not itself a requery input.
    pub flip: u64,
}

impl FrameVersions {
    /// Compare only the scope, grouping, and as-of generations used by the
    /// flip barrier. Data, config, saved scopes, and release generation do not
    /// change this identity.
    pub fn same_flip_identity(self, other: FrameVersions) -> bool {
        self.scope == other.scope && self.grouping == other.grouping && self.as_of == other.as_of
    }
}

/// Coordinates arrivals for one `(scope, grouping, as_of)` generation.
/// Tiles hold staged results until all requested keys arrive or a sweep
/// releases the expired barrier. A timeout permits ready tiles to advance
/// while slow tiles still show older data. Opening another barrier replaces
/// this one, and arrivals for its old identity cannot satisfy the new one.
#[derive(Debug)]
struct FlipBarrier {
    /// Captured counters; only scope/grouping/as-of participate in identity.
    versions: FrameVersions,
    awaiting: HashSet<QueryKey>,
    opened: Instant,
}

/// One coalesced scope-editing session. Its first actual edit pushes the
/// base; later session edits reuse that undo entry. An explicit `pushed`
/// flag remains valid when an unrelated scope edit moves the undo stack.
/// Ending back at the base removes the session entry and restores redo only
/// if the base is still the top undo entry.
#[derive(Debug, Clone)]
struct ScopeSession {
    /// The scope this session began on.
    base: Scope,
    /// Whether this session pushed its base, independent of later stack changes.
    pushed: bool,
    /// Redo history to restore if ending the session removes a no-op undo entry.
    redo_snapshot: Vec<Scope>,
}

/// Cache key and shared scope-bar model.
type BarCache = RefCell<
    Option<(
        FrameVersions,
        geode_core::clock::Clock,
        chrono::NaiveDate,
        Rc<ScopeBarModel>,
    )>,
>;

#[derive(Debug)]
pub struct Frame {
    scope: Scope,
    /// Bounded stack of outgoing scopes, oldest first — `undo_scope` pops
    /// the back, `redo_scope` pushes it back on. Capped at [`UNDO_DEPTH`]
    /// by `push_undo`, which drops the oldest entry once full.
    scope_undo: Vec<Scope>,
    scope_redo: Vec<Scope>,
    /// An explicitly opened scope-editing session, usually owned by the text field.
    scope_session: Option<ScopeSession>,
    slots: GroupingSlots,
    active_slot: Option<u8>,
    as_of: AsOf,
    /// The remembered as-of value. Repeated undo swaps between two values.
    previous_as_of: Option<AsOf>,
    /// Publishes in arrival order, newest first, capped at [`RECENT_PUBLISHES`].
    recent_publishes: VecDeque<Publish>,
    publication_watches: HashMap<String, DatasetWatches>,
    saved_scopes: SavedScopes,
    /// Named scope expressions from `expressions.toml`, which the scope and
    /// saved scopes reference by name; `effective_scope` folds them in.
    named: NamedExpressions,
    /// Latest saved scope awaiting persistence to user `scopes.toml`.
    /// A later save replaces this pending value until the observer drains it.
    pending_scope_persist: Option<(String, Scope)>,
    versions: FrameVersions,
    /// Module requery timings, exposed through the shared frame handle.
    pub requery: RequeryStats,
    user_dir: Option<PathBuf>,
    /// Latest saved slot awaiting persistence to user `groupings.toml`.
    /// The shell drains it on frame notification and writes off the UI thread.
    pending_persist: Option<(u8, Vec<String>)>,
    /// Lazy model cache keyed by versions excluding flip, clock, and local date.
    /// `Rc` makes a hit cheap; interior mutability permits caching through `&self`.
    bar_cache: BarCache,
    /// Current scope/grouping/as-of barrier, if one is waiting for arrivals.
    barrier: Option<FlipBarrier>,
}

impl Frame {
    pub fn new(slots: GroupingSlots, saved: SavedScopes, user_dir: Option<PathBuf>) -> Frame {
        Frame {
            scope: Scope::default(),
            scope_undo: Vec::new(),
            scope_redo: Vec::new(),
            scope_session: None,
            slots,
            active_slot: None,
            as_of: AsOf::Live,
            previous_as_of: None,
            recent_publishes: VecDeque::new(),
            publication_watches: HashMap::new(),
            saved_scopes: saved,
            named: NamedExpressions::default(),
            pending_scope_persist: None,
            versions: FrameVersions::default(),
            requery: RequeryStats::new(),
            user_dir,
            pending_persist: None,
            bar_cache: RefCell::new(None),
            barrier: None,
        }
    }

    pub fn versions(&self) -> FrameVersions {
        self.versions
    }

    pub fn user_dir(&self) -> Option<&Path> {
        self.user_dir.as_deref()
    }

    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    /// Push an outgoing scope, cap history, and clear redo. Used by ordinary
    /// scope edits and the first mutation of an open editing session.
    fn push_undo(&mut self, outgoing: Scope) {
        self.scope_undo.push(outgoing);
        if self.scope_undo.len() > UNDO_DEPTH {
            self.scope_undo.remove(0);
        }
        self.scope_redo.clear();
    }

    /// Replace the scope, pushing its outgoing value and clearing redo.
    /// An equal value returns false without changing history or versions.
    pub fn set_scope(&mut self, scope: Scope) -> bool {
        if self.scope == scope {
            return false;
        }
        let outgoing = std::mem::replace(&mut self.scope, scope);
        self.push_undo(outgoing);
        self.versions.scope += 1;
        true
    }

    pub fn clear_scope(&mut self) -> bool {
        self.set_scope(Scope::default())
    }

    /// Start or replace a session, capturing the current scope and redo stack.
    /// Nothing is pushed until the first actual session edit.
    pub fn begin_scope_session(&mut self) {
        self.scope_session = Some(ScopeSession {
            base: self.scope.clone(),
            pushed: false,
            redo_snapshot: self.scope_redo.clone(),
        });
    }

    /// Replace the scope and bump its version on an actual change. An open
    /// session pushes its base only once, even if other edits move the stack.
    /// Without a session, each change pushes the outgoing scope and clears redo.
    pub fn set_scope_in_session(&mut self, scope: Scope) -> bool {
        if self.scope == scope {
            return false;
        }
        match self.scope_session.as_ref() {
            Some(session) if !session.pushed => {
                let base = session.base.clone();
                self.push_undo(base);
                if let Some(session) = self.scope_session.as_mut() {
                    session.pushed = true;
                }
            }
            Some(_) => {}
            None => {
                let outgoing = self.scope.clone();
                self.push_undo(outgoing);
            }
        }
        self.scope = scope;
        self.versions.scope += 1;
        true
    }

    /// End coalescing. If the session pushed its base, the current scope equals
    /// that base, and the base remains the top undo entry, pop it and restore
    /// the captured redo stack. Otherwise leave history as it stands.
    /// Subsequent edits use ordinary history until another session is opened.
    pub fn end_scope_session(&mut self) {
        if let Some(session) = self.scope_session.take()
            && session.pushed
            && self.scope_undo.last() == Some(&session.base)
            && self.scope == session.base
        {
            self.scope_undo.pop();
            self.scope_redo = session.redo_snapshot;
        }
    }

    /// Undo one scope edit. An empty stack returns false without a version bump.
    pub fn undo_scope(&mut self) -> bool {
        let Some(previous) = self.scope_undo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.scope, previous);
        self.scope_redo.push(current);
        self.versions.scope += 1;
        true
    }

    /// Redo one scope edit. An ordinary edit or the first mutation of a new
    /// session clears this stack; later mutations in an open session do not.
    pub fn redo_scope(&mut self) -> bool {
        let Some(next) = self.scope_redo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.scope, next);
        self.scope_undo.push(current);
        self.versions.scope += 1;
        true
    }

    /// Remove every selection for a column through the undoable `set_scope`
    /// path. Preserve other fields, including `impossible`. Return false when
    /// there is no matching selection.
    pub fn drop_dimension(&mut self, column: &str) -> bool {
        let mut s = self.scope.clone();
        let before = s.dimensions.len();
        s.dimensions.retain(|d| d.column != column);
        if s.dimensions.len() == before {
            return false;
        }
        self.set_scope(s)
    }

    /// Remove named expression `name` from the scope through the undoable
    /// `set_scope` path. Return false when the scope does not name it.
    pub fn drop_named(&mut self, name: &str) -> bool {
        let mut s = self.scope.clone();
        let before = s.named.len();
        s.named.retain(|n| n != name);
        if s.named.len() == before {
            return false;
        }
        self.set_scope(s)
    }

    /// Remove top-level expression term `i` (`Expr::conjuncts` order)
    /// through the undoable `set_scope` path; the remaining terms are
    /// rebuilt as a left-folded `and` chain, and removing the last one
    /// leaves no expression. Out of range (including no expression)
    /// returns false and changes nothing.
    pub fn drop_expression_term(&mut self, i: usize) -> bool {
        self.edit_expression_term(i, None, None, None)
            .unwrap_or(false)
    }

    /// Replace top-level expression term `i` with `term`, or remove it
    /// with `None`, through the undoable `set_scope` path. The other terms
    /// keep their order. `expected` is the term the caller read at `i`
    /// (the dialog's seed); `Err(TermGone)` unless term `i` still equals
    /// it — the scope changed since the caller read it, and an index alone
    /// would silently edit whichever term now sits there. `Ok(false)` when
    /// the result equals the current scope.
    pub fn replace_expression_term(
        &mut self,
        i: usize,
        expected: &Expr,
        term: Option<Expr>,
    ) -> Result<bool, TermGone> {
        self.edit_expression_term(i, Some(expected), term, None)
    }

    /// Whether top-level expression term `i` still equals `expected`: the
    /// check [`Self::replace_expression_term`] makes, for a caller that must
    /// refuse before doing anything else (writing a definition).
    pub fn expression_term_is(&self, i: usize, expected: &Expr) -> bool {
        self.scope
            .expression
            .as_ref()
            .and_then(|e| e.conjuncts().get(i).copied())
            .is_some_and(|t| t == expected)
    }

    /// Replace top-level expression term `i` with the named expression
    /// `name`: the term leaves the expression and the name joins the named
    /// list, in ONE `set_scope` so a single undo puts the term back.
    /// `expected` guards the index as in [`Self::replace_expression_term`].
    pub fn name_expression_term(
        &mut self,
        i: usize,
        expected: &Expr,
        name: &str,
    ) -> Result<bool, TermGone> {
        self.edit_expression_term(i, Some(expected), None, Some(name))
    }

    fn edit_expression_term(
        &mut self,
        i: usize,
        expected: Option<&Expr>,
        term: Option<Expr>,
        name: Option<&str>,
    ) -> Result<bool, TermGone> {
        let mut terms: Vec<Expr> = self
            .scope
            .expression
            .as_ref()
            .map(|e| e.conjuncts().into_iter().cloned().collect())
            .unwrap_or_default();
        let Some(current) = terms.get(i) else {
            return Err(TermGone);
        };
        if expected.is_some_and(|e| e != current) {
            return Err(TermGone);
        }
        match term {
            Some(t) => terms[i] = t,
            None => {
                terms.remove(i);
            }
        }
        let mut s = self.scope.clone();
        s.expression = Expr::from_conjuncts(terms);
        if let Some(name) = name
            && !s.named.iter().any(|n| n == name)
        {
            s.named.push(name.to_string());
        }
        Ok(self.set_scope(s))
    }

    /// Remove the whole expression layer through the undoable `set_scope`
    /// path; false (and no history entry) when there is none.
    pub fn clear_expression(&mut self) -> bool {
        if self.scope.expression.is_none() {
            return false;
        }
        let mut s = self.scope.clone();
        s.expression = None;
        self.set_scope(s)
    }

    /// Set (or clear, with `None`/whitespace-only) the scope's text
    /// filter — an undoable edit like `drop_dimension`, going through the
    /// ordinary (non-session) `set_scope` path. `begin_scope_session`/
    /// `set_scope_in_session` is the coalescing alternative a live text
    /// field drives per keystroke.
    pub fn set_text(&mut self, text: Option<String>) -> bool {
        let mut s = self.scope.clone();
        s.text = text.filter(|t| !t.trim().is_empty());
        self.set_scope(s)
    }

    pub fn slots(&self) -> &GroupingSlots {
        &self.slots
    }

    pub fn active_slot(&self) -> Option<u8> {
        self.active_slot
    }

    pub fn active_grouping(&self) -> Option<&[String]> {
        self.slots.get(self.active_slot?)
    }

    /// `Some(n)` activates a filled slot; `None` returns following tiles
    /// to their views' own grouping. `false` when nothing changed or the
    /// slot is empty.
    pub fn set_active_slot(&mut self, slot: Option<u8>) -> bool {
        if let Some(n) = slot
            && self.slots.get(n).is_none()
        {
            return false;
        }
        if self.active_slot == slot {
            return false;
        }
        self.active_slot = slot;
        self.versions.grouping += 1;
        true
    }

    /// Replace grouping slots after reload. A change bumps config and grouping,
    /// even if the active slot is unchanged. Clear an active slot that disappeared.
    pub fn replace_slots(&mut self, slots: GroupingSlots) -> bool {
        if self.slots == slots {
            return false;
        }
        self.slots = slots;
        if self
            .active_slot
            .is_some_and(|n| self.slots.get(n).is_none())
        {
            self.active_slot = None;
        }
        self.versions.config += 1;
        self.versions.grouping += 1;
        true
    }

    /// Save a nonempty grouping in slot 1–9 and replace the pending write.
    /// Bump grouping only if that slot is active. Production grouping edits use
    /// the Groupings dialog's config writer; this model API remains independently
    /// usable and covered by tests.
    pub fn save_slot(&mut self, slot: u8, grouping: Vec<String>) -> Result<(), String> {
        let persisted = grouping.clone();
        if !self.slots.set(slot, grouping) {
            return Err(format!(
                "slot must be 1–9 and the grouping non-empty (got {slot})"
            ));
        }
        if self.active_slot == Some(slot) {
            self.versions.grouping += 1;
        }
        self.pending_persist = Some((slot, persisted));
        Ok(())
    }

    /// Drain the latest pending slot write for the shell's background writer.
    pub fn take_pending_persist(&mut self) -> Option<(u8, Vec<String>)> {
        self.pending_persist.take()
    }

    pub fn as_of(&self) -> &AsOf {
        &self.as_of
    }

    /// Replace as-of and remember its outgoing value for `undo_as_of`.
    /// An equal value returns false without changing versions or history.
    pub fn set_as_of(&mut self, as_of: AsOf) -> bool {
        if self.as_of == as_of {
            return false;
        }
        self.previous_as_of = Some(std::mem::replace(&mut self.as_of, as_of));
        self.versions.as_of += 1;
        true
    }

    /// Swap current and remembered as-of. Repeated calls toggle the pair.
    pub fn undo_as_of(&mut self) -> bool {
        let Some(previous) = self.previous_as_of.take() else {
            return false;
        };
        let current = std::mem::replace(&mut self.as_of, previous);
        self.previous_as_of = Some(current);
        self.versions.as_of += 1;
        true
    }

    /// Watch a whole dataset (`None`) or one document's encoded batch key.
    /// Registration does not notify observers. A new watch starts at the current
    /// revision; the consumer must issue its initial query when adopting it.
    pub fn watch_publications(&mut self, dataset: &str, batch: Option<&str>) -> PublicationWatch {
        // Reap on registration, not every feed update. Registry size follows
        // live interests (including hidden tiles), not publication cardinality.
        self.publication_watches.retain(|_, watches| {
            watches
                .documents
                .retain(|_, watch| watch.strong_count() != 0);
            watches.dataset.strong_count() != 0 || !watches.documents.is_empty()
        });
        let watches = self
            .publication_watches
            .entry(dataset.to_owned())
            .or_default();
        let slot = match batch {
            Some(batch) => watches.documents.entry(batch.to_owned()).or_default(),
            None => &mut watches.dataset,
        };
        let revision = slot.upgrade().unwrap_or_else(|| {
            let revision = Rc::new(Cell::new(self.versions.data));
            *slot = Rc::downgrade(&revision);
            revision
        });
        PublicationWatch {
            dataset: dataset.to_owned(),
            batch: batch.map(str::to_owned),
            revision,
        }
    }

    /// Frame counters for one consumer. Only `data` is narrowed to its watched
    /// publications; scope/grouping/as-of and flip identity remain unchanged.
    /// Watches must come from this frame and remain alive with the consumer.
    pub fn versions_for<'a>(
        &self,
        watches: impl IntoIterator<Item = &'a PublicationWatch>,
    ) -> FrameVersions {
        FrameVersions {
            data: watches
                .into_iter()
                .map(|watch| watch.revision.get())
                .max()
                .unwrap_or(0),
            ..self.versions
        }
    }

    /// Record a publish for the global history and advance only matching tile
    /// watches. Global `data` remains available to the as-of picker and chrome.
    pub fn note_published(&mut self, publish: Publish) {
        self.versions.data += 1;
        if let Some(watches) = self.publication_watches.get(&publish.dataset) {
            if let Some(revision) = watches.dataset.upgrade() {
                revision.set(self.versions.data);
            }
            if let Some(revision) = watches
                .documents
                .get(&publish.batch)
                .and_then(Weak::upgrade)
            {
                revision.set(self.versions.data);
            }
        }
        self.recent_publishes.push_front(publish);
        self.recent_publishes.truncate(RECENT_PUBLISHES);
    }

    pub fn recent_publishes(&self) -> &VecDeque<Publish> {
        &self.recent_publishes
    }

    pub fn saved_scopes(&self) -> &SavedScopes {
        &self.saved_scopes
    }

    /// Replace saved scopes after reload. A change bumps config; callers that
    /// follow config can requery even though the active scope is unchanged.
    pub fn replace_saved_scopes(&mut self, saved: SavedScopes) -> bool {
        if self.saved_scopes == saved {
            return false;
        }
        self.saved_scopes = saved;
        self.versions.config += 1;
        true
    }

    pub fn named_expressions(&self) -> &NamedExpressions {
        &self.named
    }

    /// Replace the named expressions after reload. A change bumps config so a
    /// tile whose scope references a redefined name requeries, even though
    /// the active scope itself is unchanged.
    pub fn replace_named_expressions(&mut self, named: NamedExpressions) -> bool {
        if self.named == named {
            return false;
        }
        self.named = named;
        self.versions.config += 1;
        true
    }

    /// Save the current scope in memory, replace the pending scope write, and
    /// bump saved_scopes. Validate the object name and reject reserved action
    /// names to prevent collisions when registering `scope::<name>` actions.
    /// An existing name is overwritten; an empty scope is accepted.
    pub fn save_scope(&mut self, name: &str) -> Result<(), String> {
        let name = geode_core::config::check_object_name(name)
            .map_err(|_| format!("'{}' is not a usable scope name", name.trim()))?;
        if geode_core::scopes::RESERVED_NAMES.contains(&name) {
            return Err(format!("'{name}' is reserved"));
        }
        self.saved_scopes
            .insert(name.to_string(), self.scope.clone());
        self.pending_scope_persist = Some((name.to_string(), self.scope.clone()));
        self.versions.saved_scopes += 1;
        Ok(())
    }

    /// Load a saved scope by name, going through `set_scope` so it's
    /// undoable like any other scope change. `Err` when no scope by that
    /// name exists; `Ok(false)` when it exists but is already the current
    /// scope.
    pub fn load_scope(&mut self, name: &str) -> Result<bool, String> {
        let scope = self
            .saved_scopes
            .get(name)
            .cloned()
            .ok_or_else(|| format!("no saved scope '{name}'"))?;
        Ok(self.set_scope(scope))
    }

    /// Drain the latest pending scope write for the shell's background writer.
    pub fn take_pending_scope_persist(&mut self) -> Option<(String, Scope)> {
        self.pending_scope_persist.take()
    }

    /// Clear undo and redo without changing scope or ending an open session.
    /// Session restoration uses this after applying its initial scope.
    pub fn clear_history(&mut self) {
        self.scope_undo.clear();
        self.scope_redo.clear();
    }

    pub fn note_config_reloaded(&mut self) {
        self.versions.config += 1;
    }

    /// Compose the frame scope with the tile layer through `Scope::and_then`,
    /// then fold in every named expression it references. A missing or
    /// invalid name is an error the caller shows instead of querying:
    /// skipping it would widen the scope and produce plausible wrong totals.
    pub fn effective_scope(&self, tile: &Scope) -> Result<Scope, String> {
        self.scope.and_then(tile).resolve(&self.named)
    }

    /// Return cached scope-bar labels for versions excluding flip, the configured
    /// clock, and today's date on that clock. Clock/date changes invalidate labels
    /// even without a frame mutation, covering zone reloads and midnight.
    /// The caller supplies cached time inputs; this method reads no global clock.
    pub fn bar_model(
        &self,
        clock: geode_core::clock::Clock,
        today: chrono::NaiveDate,
    ) -> Rc<ScopeBarModel> {
        // `flip` alone never changes what the bar shows — keyed out here
        // (rather than relying on it happening to already match) so a
        // flip costs a refcount bump like any other unrelated notify,
        // not a rebuild.
        let mut versions = self.versions();
        versions.flip = 0;
        if let Some((cached_versions, cached_clock, cached_today, cached)) =
            self.bar_cache.borrow().as_ref()
            && *cached_versions == versions
            && *cached_clock == clock
            && *cached_today == today
        {
            return Rc::clone(cached);
        }
        let built = Rc::new(scopebar::build_model(self, clock, today));
        *self.bar_cache.borrow_mut() = Some((versions, clock, today, Rc::clone(&built)));
        built
    }

    /// Replace the barrier with the current scope/grouping/as-of identity and
    /// these tile keys. An empty key set clears it without bumping flip.
    /// No version changes or notifications are emitted here.
    ///
    /// The shell must open the barrier before occupant frame observers run:
    /// tiles unaffected by the changed inputs can then self-arrive immediately.
    /// Shell observer registration precedes occupant registration to enforce this.
    pub fn open_flip(&mut self, keys: impl IntoIterator<Item = QueryKey>, now: Instant) {
        let awaiting: HashSet<QueryKey> = keys.into_iter().collect();
        if awaiting.is_empty() {
            self.barrier = None;
            return;
        }
        self.barrier = Some(FlipBarrier {
            versions: self.versions,
            awaiting,
            opened: now,
        });
    }

    fn matches(b: &FlipBarrier, v: FrameVersions) -> bool {
        b.versions.same_flip_identity(v)
    }

    /// Whether an open barrier is waiting for `key` at `versions` —
    /// `false` once nothing is open, once `key` already arrived, or once
    /// a later mutation replaced the barrier with one over different
    /// versions (a stale outcome from before the replacement must not
    /// satisfy it).
    pub fn barrier_wants(&self, key: QueryKey, versions: FrameVersions) -> bool {
        self.barrier
            .as_ref()
            .is_some_and(|b| Self::matches(b, versions) && b.awaiting.contains(&key))
    }

    /// `key`'s outcome for `versions` arrived — a failed outcome counts
    /// too (`geode-blotter`'s `deliver`: one broken tile must never hold
    /// the rest open). `true` exactly when this arrival emptied the
    /// barrier, which also bumps `flip` via `release`;
    /// the caller uses the return value to promote its own staged
    /// snapshot right away rather than waiting for its own
    /// `on_frame_changed` to see the bump.
    pub fn arrived(&mut self, key: QueryKey, versions: FrameVersions) -> bool {
        let Some(b) = self.barrier.as_mut() else {
            return false;
        };
        if !Self::matches(b, versions) {
            return false;
        }
        b.awaiting.remove(&key);
        if b.awaiting.is_empty() {
            self.release();
            true
        } else {
            false
        }
    }

    /// Past [`FLIP_DEADLINE`], release whatever arrived so far rather
    /// than waiting forever on a tile that never answers (a query the
    /// pool dropped, a tile torn down mid-flight). `true` when this call
    /// released it; `false` before the deadline or once nothing is open
    /// — a spurious extra call changes nothing.
    pub fn sweep(&mut self, now: Instant) -> bool {
        match &self.barrier {
            Some(b) if now.duration_since(b.opened) >= FLIP_DEADLINE => {
                self.release();
                true
            }
            _ => false,
        }
    }

    pub fn barrier_open(&self) -> bool {
        self.barrier.is_some()
    }

    fn release(&mut self) {
        self.barrier = None;
        self.versions.flip += 1;
    }
}

/// `term` joined to `existing` with `and` (`existing and term`), or `term`
/// alone when there is no expression. Appending never replaces: the
/// existing expression keeps narrowing.
pub fn and_join(existing: Option<Expr>, term: Expr) -> Expr {
    match existing {
        Some(existing) => Expr::And(Box::new(existing), Box::new(term)),
        None => term,
    }
}

/// Write a validated slot into user `groupings.toml`, preserving other keys.
/// `config_write` owns the read-modify-write and atomic rename.
pub fn persist_slot_to_user_config(
    user_dir: &Path,
    slot: u8,
    grouping: &[String],
) -> Result<(), String> {
    if !(1..=9).contains(&slot) || grouping.is_empty() {
        return Err(format!("slot {slot} out of range or empty grouping"));
    }
    crate::config_write::edit(user_dir, Layer::User, "groupings", |doc| {
        let mut array = toml_edit::Array::new();
        for g in grouping {
            array.push(g.as_str());
        }
        doc[slot.to_string().as_str()] = value(array);
    })
}

/// Write a named scope into user `scopes.toml`, preserving other keys through
/// `config_write`. `scope_to_table` defines the persisted shape; this helper
/// does not validate the name or serialize runtime-only contradiction state.
pub fn persist_scope_to_user_config(
    user_dir: &Path,
    name: &str,
    scope: &Scope,
) -> Result<(), String> {
    crate::config_write::edit(user_dir, Layer::User, "scopes", |doc| {
        doc[name] = toml_edit::Item::Table(geode_core::scopes::scope_to_table(scope));
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::clock::Clock;
    use geode_core::scope::{DimensionSelection, Scope};

    fn hm(h: u32, m: u32) -> chrono::NaiveTime {
        chrono::NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn slots() -> GroupingSlots {
        let mut s = GroupingSlots::default();
        s.set(1, vec!["book".into(), "lhu".into()]);
        s.set(2, vec!["underlying_ref".into()]);
        s
    }

    fn book_scope(book: &str) -> Scope {
        Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec![book.into()],
            }],
            ..Scope::default()
        }
    }

    #[test]
    fn each_mutation_bumps_exactly_its_own_counter() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let v0 = f.versions();

        assert!(f.set_scope(book_scope("BK000")));
        let v1 = f.versions();
        assert_eq!(v1.scope, v0.scope + 1);
        assert_eq!(
            (v1.grouping, v1.as_of, v1.data, v1.config),
            (v0.grouping, v0.as_of, v0.data, v0.config)
        );

        assert!(f.set_active_slot(Some(2)));
        let v2 = f.versions();
        assert_eq!(v2.grouping, v1.grouping + 1);
        assert_eq!(v2.scope, v1.scope);

        assert!(
            f.set_as_of(AsOf::At(
                chrono::DateTime::parse_from_rfc3339("2026-09-03T14:05:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            ))
        );
        assert_eq!(f.versions().as_of, v2.as_of + 1);

        f.note_published(Publish {
            dataset: "risk".into(),
            batch: "EOD".into(),
            books: 1,
            at: chrono::Utc::now(),
        });
        assert_eq!(f.versions().data, v2.data + 1);
        f.note_config_reloaded();
        assert_eq!(f.versions().config, v2.config + 1);
    }

    #[test]
    fn an_unchanged_value_bumps_nothing() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let v0 = f.versions();
        assert!(!f.set_scope(Scope::default()));
        assert!(!f.set_active_slot(None));
        assert!(!f.set_as_of(AsOf::Live));
        assert_eq!(f.versions(), v0);
    }

    #[test]
    fn an_empty_slot_cannot_be_activated() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        assert!(!f.set_active_slot(Some(5)));
        assert_eq!(f.active_slot(), None);
        assert!(f.set_active_slot(Some(1)));
        assert_eq!(
            f.active_grouping(),
            Some(&["book".to_string(), "lhu".into()][..])
        );
        assert!(f.set_active_slot(None));
        assert_eq!(f.active_grouping(), None);
    }

    #[test]
    fn effective_scope_composes_global_and_tile() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.set_scope(book_scope("BK000"));
        let tile = Scope {
            text: Some("spx".into()),
            ..Scope::default()
        };
        let eff = f.effective_scope(&tile).unwrap();
        assert_eq!(eff.dimensions, book_scope("BK000").dimensions);
        assert_eq!(eff.text.as_deref(), Some("spx"));
        assert_eq!(
            f.effective_scope(&Scope::default()),
            Ok(book_scope("BK000"))
        );
    }

    #[test]
    fn replacing_slots_bumps_config_and_grouping_and_drops_a_vanished_active_slot() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.set_active_slot(Some(2));
        let v = f.versions();
        let mut fewer = GroupingSlots::default();
        fewer.set(1, vec!["book".into()]);
        assert!(f.replace_slots(fewer));
        assert_eq!(f.active_slot(), None, "slot 2 no longer exists");
        assert_eq!(f.versions().config, v.config + 1);
        assert_eq!(f.versions().grouping, v.grouping + 1);
    }

    #[test]
    fn saving_a_slot_updates_memory_and_bumps_grouping_only_when_active() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let v = f.versions();
        assert!(f.save_slot(3, vec!["lhu".into()]).is_ok());
        assert_eq!(f.slots().label(3).as_deref(), Some("lhu"));
        assert_eq!(f.versions().grouping, v.grouping, "not the active slot");
        f.set_active_slot(Some(3));
        let v = f.versions();
        assert!(f.save_slot(3, vec!["book".into()]).is_ok());
        assert_eq!(f.take_pending_persist(), Some((3, vec!["book".into()])));
        assert_eq!(
            f.versions().grouping,
            v.grouping + 1,
            "the active slot changed"
        );
        assert!(f.save_slot(0, vec!["book".into()]).is_err());
        assert!(f.save_slot(3, Vec::new()).is_err());
    }

    #[test]
    fn a_slot_is_persisted_as_a_bare_numeric_key() {
        let dir = tempfile::tempdir().unwrap();
        persist_slot_to_user_config(dir.path(), 3, &["lhu".into(), "position_ref".into()]).unwrap();
        let text = std::fs::read_to_string(dir.path().join("groupings.toml")).unwrap();
        let table: toml::Table = text.parse().unwrap();
        assert_eq!(table["config_version"].as_integer(), Some(1));
        assert_eq!(
            table["3"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["lhu", "position_ref"]
        );
        // A second save keeps the first slot.
        persist_slot_to_user_config(dir.path(), 5, &["book".into()]).unwrap();
        let text = std::fs::read_to_string(dir.path().join("groupings.toml")).unwrap();
        let table: toml::Table = text.parse().unwrap();
        assert!(table.contains_key("3") && table.contains_key("5"));
    }

    // Scope history, as-of, publications, saved scopes, and bar-model tests.

    #[test]
    fn undo_and_redo_walk_a_bounded_stack() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        for i in 0..40 {
            assert!(f.set_scope(book_scope(&format!("BK{i:03}"))));
        }
        // 32 undos land on BK007 (40 sets, depth 32); a 33rd does nothing.
        for _ in 0..32 {
            assert!(f.undo_scope());
        }
        assert_eq!(f.scope().dimensions[0].values, vec!["BK007".to_string()]);
        assert!(!f.undo_scope());
        assert!(f.redo_scope());
        assert_eq!(f.scope().dimensions[0].values, vec!["BK008".to_string()]);
        // A new set clears redo.
        assert!(f.set_scope(book_scope("X")));
        assert!(!f.redo_scope());
    }

    #[test]
    fn a_no_op_set_pushes_nothing() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        assert!(f.set_scope(book_scope("A")));
        assert!(!f.set_scope(book_scope("A")));
        assert!(f.undo_scope());
        assert!(f.scope().is_empty());
        assert!(!f.undo_scope());
    }

    #[test]
    fn a_text_session_that_ends_where_it_began_leaves_no_undo_entry() {
        // A session returning to its base removes its own undo entry and restores
        // the redo history captured before typing began.
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.set_scope(book_scope("A"));
        assert!(f.undo_scope());
        let redo_before = f.scope_redo.clone();
        assert!(!redo_before.is_empty(), "fixture must seed a redo entry");

        let depth_before = f.scope_undo.len();
        f.begin_scope_session();
        let mut s = f.scope().clone();
        s.text = Some("a".into());
        assert!(f.set_scope_in_session(s));
        let mut s = f.scope().clone();
        s.text = None;
        assert!(f.set_scope_in_session(s));
        f.end_scope_session();
        assert_eq!(
            f.scope_undo.len(),
            depth_before,
            "the session's own push must be popped once it ends where it began"
        );
        assert!(
            !f.undo_scope(),
            "nothing to undo: the session never actually changed anything"
        );
        assert_eq!(
            f.scope_redo, redo_before,
            "a no-op session must not destroy redo history from before it opened"
        );
    }

    #[test]
    fn a_mid_session_external_push_does_not_cause_a_second_session_push() {
        // An unrelated scope edit can move the undo stack during a text session.
        // The session still pushes its base only once.
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.set_scope(book_scope("A"));
        let base = f.scope().clone();
        f.begin_scope_session();

        // The session's own first mutation: pushes `base` once.
        let mut s = f.scope().clone();
        s.text = Some("a".into());
        assert!(f.set_scope_in_session(s));
        assert_eq!(f.scope_undo.last(), Some(&base));
        let len_after_first_push = f.scope_undo.len();

        // Mid-session external mutation (the mouse route) — pushes its
        // own outgoing scope; `base` is no longer the stack top.
        assert!(f.drop_dimension("book"));
        assert_ne!(f.scope_undo.last(), Some(&base));

        // A second session mutation must not push `base` again just
        // because the stack top moved.
        let mut s = f.scope().clone();
        s.text = Some("ab".into());
        assert!(f.set_scope_in_session(s));
        assert_eq!(
            f.scope_undo.len(),
            len_after_first_push + 1,
            "the session must not re-push its own base after an external \
             mutation moved the stack top"
        );
        assert_eq!(
            f.scope_undo.iter().filter(|s| *s == &base).count(),
            1,
            "the base scope must appear on the undo stack exactly once"
        );
    }

    #[test]
    fn a_text_session_coalesces_into_one_undo_entry() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.set_scope(book_scope("A"));
        f.begin_scope_session();
        for t in ["s", "sp", "spx"] {
            let mut s = f.scope().clone();
            s.text = Some(t.into());
            assert!(f.set_scope_in_session(s));
        }
        f.end_scope_session();
        assert!(f.undo_scope());
        assert_eq!(
            f.scope().text,
            None,
            "one undo returns to before the session"
        );
        assert_eq!(f.scope().dimensions[0].values, vec!["A".to_string()]);
        assert!(f.redo_scope());
        assert_eq!(f.scope().text.as_deref(), Some("spx"));
    }

    #[test]
    fn as_of_remembers_one_previous_value_in_both_directions() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let t = chrono::Utc::now();
        assert!(f.set_as_of(AsOf::At(t)));
        assert!(f.set_as_of(AsOf::Live));
        assert!(f.undo_as_of());
        assert_eq!(f.as_of(), &AsOf::At(t));
        assert!(f.undo_as_of(), "undo swaps, so it can go back again");
        assert_eq!(f.as_of(), &AsOf::Live);
    }

    #[test]
    fn recent_publishes_keep_the_last_thirty_two_newest_first() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let v0 = f.versions().data;
        for i in 0..40u32 {
            f.note_published(Publish {
                dataset: "risk".into(),
                batch: "EOD".into(),
                books: 3,
                at: chrono::Utc::now() + chrono::Duration::seconds(i as i64),
            });
        }
        assert_eq!(f.versions().data, v0 + 40);
        assert_eq!(f.recent_publishes().len(), RECENT_PUBLISHES);
        assert!(f.recent_publishes()[0].at > f.recent_publishes()[1].at);
    }

    #[test]
    fn saved_scopes_load_save_and_persist_pending() {
        let mut saved = SavedScopes::new();
        saved.insert("eu".into(), book_scope("BK001"));
        let mut f = Frame::new(slots(), saved, None);
        assert!(f.load_scope("eu").unwrap());
        assert_eq!(f.scope(), &book_scope("BK001"));
        assert!(f.load_scope("nope").is_err());
        f.set_scope(book_scope("BK002"));
        f.save_scope("mine").unwrap();
        assert_eq!(f.saved_scopes()["mine"], book_scope("BK002"));
        assert_eq!(
            f.take_pending_scope_persist(),
            Some(("mine".into(), book_scope("BK002")))
        );
        assert_eq!(f.take_pending_scope_persist(), None);
        assert!(f.save_scope("").is_err());
    }

    /// Reserved scope names must fail before mutating memory or pending writes,
    /// preventing collisions with built-in scope action identifiers.
    #[test]
    fn save_scope_refuses_the_reserved_save_current_name() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.set_scope(book_scope("BK001"));
        assert_eq!(
            f.save_scope("save_current"),
            Err("'save_current' is reserved".to_string())
        );
        assert!(!f.saved_scopes().contains_key("save_current"));
        assert_eq!(f.take_pending_scope_persist(), None);
    }

    #[test]
    fn save_scope_bumps_saved_scopes_not_config() {
        // Saving a named snapshot leaves the active query inputs unchanged.
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.set_scope(book_scope("A"));
        let before = f.versions();
        f.save_scope("mine").unwrap();
        let after = f.versions();
        assert_eq!(
            after.config, before.config,
            "save_scope must not bump config"
        );
        assert_eq!(after.saved_scopes, before.saved_scopes + 1);
    }

    #[test]
    fn drop_dimension_and_set_text_are_undoable_edits() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.set_scope(book_scope("A"));
        assert!(f.drop_dimension("book"));
        assert!(f.scope().is_empty());
        assert!(!f.drop_dimension("book"));
        assert!(f.set_text(Some("spx".into())));
        assert!(!f.set_text(Some("spx".into())));
        assert!(f.undo_scope());
        assert!(f.scope().is_empty());
        assert!(f.undo_scope());
        assert_eq!(f.scope(), &book_scope("A"));
    }

    fn expr_scope(text: &str) -> Scope {
        Scope {
            expression: Some(geode_core::scope::parse_expr(text).unwrap()),
            ..Scope::default()
        }
    }

    fn term_texts(f: &Frame) -> Vec<String> {
        f.scope()
            .expression
            .as_ref()
            .map(|e| e.conjuncts().iter().map(|t| t.to_string()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn drop_expression_term_removes_only_that_term_and_is_undoable() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.set_scope(expr_scope("a = 1 and b = 2 and c = 3"));
        assert!(f.drop_expression_term(1));
        assert_eq!(term_texts(&f), vec!["a = 1", "c = 3"]);
        assert!(!f.drop_expression_term(2), "out of range changes nothing");
        assert_eq!(term_texts(&f), vec!["a = 1", "c = 3"]);
        assert!(f.drop_expression_term(0));
        assert!(f.drop_expression_term(0), "the last term");
        assert_eq!(f.scope().expression, None, "the last term leaves none");
        assert!(!f.drop_expression_term(0), "no expression, no term");
        assert!(f.undo_scope());
        assert_eq!(term_texts(&f), vec!["c = 3"]);
        assert!(f.undo_scope());
        assert!(f.undo_scope());
        assert_eq!(term_texts(&f), vec!["a = 1", "b = 2", "c = 3"]);
    }

    #[test]
    fn replace_expression_term_keeps_the_others_and_refuses_out_of_range() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.set_scope(expr_scope("a = 1 and b = 2 and c = 3"));
        let p = |t: &str| geode_core::scope::parse_expr(t).unwrap();
        let x = p("x = 9");
        assert_eq!(
            f.replace_expression_term(1, &p("b = 2"), Some(x.clone())),
            Ok(true)
        );
        assert_eq!(term_texts(&f), vec!["a = 1", "x = 9", "c = 3"]);
        assert_eq!(
            f.replace_expression_term(1, &x, Some(x.clone())),
            Ok(false),
            "the same term again is no edit"
        );
        assert_eq!(
            f.replace_expression_term(1, &p("b = 2"), Some(p("y = 1"))),
            Err(TermGone),
            "index 1 now holds a different term: refuse rather than edit it"
        );
        assert_eq!(
            f.replace_expression_term(1, &p("b = 2"), None),
            Err(TermGone),
            "nor remove it"
        );
        assert_eq!(term_texts(&f), vec!["a = 1", "x = 9", "c = 3"]);
        assert_eq!(
            f.replace_expression_term(3, &x, Some(x.clone())),
            Err(TermGone)
        );
        assert_eq!(f.replace_expression_term(2, &p("c = 3"), None), Ok(true));
        assert_eq!(term_texts(&f), vec!["a = 1", "x = 9"]);
        assert!(f.undo_scope());
        assert!(f.undo_scope());
        assert_eq!(term_texts(&f), vec!["a = 1", "b = 2", "c = 3"]);
    }

    #[test]
    fn name_expression_term_swaps_the_term_for_the_name_in_one_undo_step() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.set_scope(expr_scope("a = 1 and b = 2 and c = 3"));
        let p = |t: &str| geode_core::scope::parse_expr(t).unwrap();
        assert!(f.expression_term_is(1, &p("b = 2")));
        assert!(!f.expression_term_is(1, &p("a = 1")));
        assert!(!f.expression_term_is(3, &p("b = 2")), "out of range");
        assert_eq!(
            f.name_expression_term(1, &p("a = 1"), "bee"),
            Err(TermGone),
            "index 1 holds a different term: refuse rather than name it"
        );
        assert_eq!(f.scope().named, Vec::<String>::new());
        assert_eq!(f.name_expression_term(1, &p("b = 2"), "bee"), Ok(true));
        assert_eq!(term_texts(&f), vec!["a = 1", "c = 3"]);
        assert_eq!(f.scope().named, vec!["bee".to_string()]);
        assert!(f.undo_scope(), "one step");
        assert_eq!(term_texts(&f), vec!["a = 1", "b = 2", "c = 3"]);
        assert_eq!(f.scope().named, Vec::<String>::new());

        // A scope may already list the name (a reference whose definition
        // is missing); naming a term after it lists it once.
        let mut s = f.scope().clone();
        s.named = vec!["gone".to_string()];
        f.set_scope(s);
        assert_eq!(f.name_expression_term(0, &p("a = 1"), "gone"), Ok(true));
        assert_eq!(f.scope().named, vec!["gone".to_string()]);
    }

    #[test]
    fn and_join_joins_with_and_or_sets_it() {
        let a = geode_core::scope::parse_expr("a = 1").unwrap();
        let b = geode_core::scope::parse_expr("b = 2 or c = 3").unwrap();
        let joined = and_join(None, a);
        assert_eq!(joined.to_string(), "a = 1", "none: it is set");
        assert_eq!(
            and_join(Some(joined), b).to_string(),
            "(a = 1) and ((b = 2) or (c = 3))",
            "existing and (new)"
        );
    }

    #[test]
    fn clear_expression_drops_the_layer_and_is_a_no_op_without_one() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        assert!(!f.clear_expression());
        let mut s = expr_scope("a = 1 and b = 2");
        s.text = Some("spx".into());
        f.set_scope(s);
        assert!(f.clear_expression());
        assert_eq!(f.scope().expression, None);
        assert_eq!(f.scope().text.as_deref(), Some("spx"), "other layers stay");
        assert!(f.undo_scope());
        assert_eq!(term_texts(&f), vec!["a = 1", "b = 2"]);
    }

    #[test]
    fn the_bar_model_is_cached_on_versions_and_describes_the_scope() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.set_active_slot(Some(1));
        let mut s = book_scope("BK001");
        s.dimensions[0].values.push("BK002".into());
        s.dimensions.push(DimensionSelection {
            column: "lhu".into(),
            values: (0..7).map(|i| i.to_string()).collect(),
        });
        s.text = Some("spx".into());
        s.expression = Some(geode_core::scope::parse_expr("npv > 0").unwrap());
        f.set_scope(s);
        let clock = Clock::utc();
        let today = clock.today(chrono::Utc::now());
        let m1 = f.bar_model(clock, today);
        let m2 = f.bar_model(clock, today);
        assert!(Rc::ptr_eq(&m1, &m2));
        assert_eq!(m1.slot, Some((1, "book / lhu".into())));
        assert_eq!(m1.chips[0].summary, "book ∈ BK001, BK002");
        assert_eq!(m1.chips[1].summary, "lhu ∈ {7}");
        assert_eq!(m1.text.as_deref(), Some("spx"));
        assert_eq!(m1.terms.len(), 1);
        assert_eq!(m1.terms[0].label, "npv > 0");
        assert_eq!(m1.as_of, None);
        f.set_text(None);
        assert!(!Rc::ptr_eq(&m1, &f.bar_model(clock, today)));

        // Changing the clock invalidates the cache with unchanged versions and date.
        let m3 = f.bar_model(clock, today);
        let other_clock = Clock::utc().with_times(hm(7, 0), hm(17, 0));
        let m4 = f.bar_model(other_clock, today);
        assert!(
            !Rc::ptr_eq(&m3, &m4),
            "a different clock must rebuild even with versions and today unchanged"
        );
    }

    #[test]
    fn the_bar_model_cache_rebuilds_when_today_changes_with_versions_unchanged() {
        // Midnight invalidates a cached today-only label without a frame mutation.
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.set_scope(book_scope("A"));
        let clock = Clock::utc();
        let day1 = clock.today(chrono::Utc::now());
        let m1 = f.bar_model(clock, day1);
        let day2 = day1 + chrono::Duration::days(1);
        let m2 = f.bar_model(clock, day2);
        assert!(
            !Rc::ptr_eq(&m1, &m2),
            "versions unchanged but the date moved on: must rebuild"
        );
    }

    #[test]
    fn a_contradiction_is_named_not_hidden() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let s = book_scope("A").and_then(&book_scope("B"));
        assert!(s.impossible);
        f.set_scope(s);
        let clock = Clock::utc();
        assert_eq!(
            f.bar_model(clock, clock.today(chrono::Utc::now()))
                .impossible
                .as_deref(),
            Some("∅ book")
        );
    }

    #[test]
    fn a_saved_scope_is_persisted_alongside_a_pre_existing_sibling_and_comment() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("scopes.toml"),
            "config_version = 1\n# a hand-written comment\n[keep]\n[keep.dimensions]\nbook = [\"BK000\"]\n",
        )
        .unwrap();

        persist_scope_to_user_config(dir.path(), "mine", &book_scope("BK002")).unwrap();

        let text = std::fs::read_to_string(dir.path().join("scopes.toml")).unwrap();
        assert!(text.contains("# a hand-written comment"), "{text}");

        let doc: toml_edit::DocumentMut = text.parse().unwrap();
        let table: toml::Table = doc.to_string().parse().unwrap();
        let merged = geode_core::config::merge_docs(
            "scopes",
            &[geode_core::config::LayerDoc {
                layer: geode_core::config::Layer::Builtin,
                name: "scopes".to_string(),
                file: "<test>".into(),
                table,
            }],
        );
        let (saved, diags) = geode_core::scopes::saved_scopes_from_doc(
            &merged,
            &geode_core::schema::SchemaSpec::default(),
            &geode_core::dimensions::DerivedDimensions::default(),
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(saved["keep"], book_scope("BK000"));
        assert_eq!(saved["mine"], book_scope("BK002"));
    }

    // Flip-barrier tests.

    #[test]
    fn a_barrier_releases_when_every_key_arrives_and_bumps_flip_once() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.set_scope(book_scope("A"));
        let v = f.versions();
        let t0 = Instant::now();
        f.open_flip([QueryKey(1), QueryKey(2)], t0);
        assert!(f.barrier_open());
        assert!(f.barrier_wants(QueryKey(1), v));
        assert!(!f.barrier_wants(QueryKey(3), v));
        let mut stale = v;
        stale.scope -= 1;
        assert!(!f.barrier_wants(QueryKey(1), stale));
        assert!(!f.arrived(QueryKey(1), v));
        assert_eq!(f.versions().flip, v.flip);
        assert!(f.arrived(QueryKey(2), v));
        assert_eq!(f.versions().flip, v.flip + 1);
        assert!(!f.barrier_open());
        assert!(!f.arrived(QueryKey(2), v), "nothing open");
    }

    #[test]
    fn the_deadline_releases_with_whatever_arrived() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let v = f.versions();
        let t0 = Instant::now();
        f.open_flip([QueryKey(1), QueryKey(2)], t0);
        assert!(!f.sweep(t0 + Duration::from_millis(100)));
        assert!(f.sweep(t0 + FLIP_DEADLINE + Duration::from_millis(1)));
        assert_eq!(f.versions().flip, v.flip + 1);
        assert!(!f.barrier_open());
    }

    #[test]
    fn a_new_mutation_while_open_replaces_the_barrier() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.open_flip([QueryKey(1)], Instant::now());
        let v_old = f.versions();
        f.set_scope(book_scope("B"));
        let v_new = f.versions();
        f.open_flip([QueryKey(1), QueryKey(2)], Instant::now());
        assert!(!f.barrier_wants(QueryKey(1), v_old));
        assert!(f.barrier_wants(QueryKey(2), v_new));
    }

    #[test]
    fn data_and_config_bumps_do_not_open_a_barrier_and_do_not_match_one() {
        // Data and config do not change flip identity, so either may differ on
        // an arrival for this barrier. Opening barriers on frame notifications is
        // a separate shell responsibility.
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.set_scope(book_scope("A"));
        let v = f.versions();
        f.open_flip([QueryKey(1)], Instant::now());
        let mut only_data_changed = v;
        only_data_changed.data += 1;
        assert!(
            f.barrier_wants(QueryKey(1), only_data_changed),
            "scope/grouping/as_of still match — data is not part of the barrier's key"
        );
        let mut only_config_changed = v;
        only_config_changed.config += 1;
        assert!(f.barrier_wants(QueryKey(1), only_config_changed));
        // But nothing opens a barrier by itself just because `data`/
        // `config` moved — that's the shell's decision, not the frame's.
        let mut f2 = Frame::new(slots(), SavedScopes::new(), None);
        f2.note_published(Publish {
            dataset: "risk".into(),
            batch: "EOD".into(),
            books: 1,
            at: chrono::Utc::now(),
        });
        assert!(!f2.barrier_open());
    }
    #[test]
    fn publication_watches_are_exact_retained_and_reclaimed() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let risk = f.watch_publications("risk", None);
        let spx = f.watch_publications("cvi", Some("SPX"));
        let spx_twin = f.watch_publications("cvi", Some("SPX"));
        let ndx = f.watch_publications("cvi", Some("NDX"));
        let publish = |dataset: &str, batch: &str| Publish {
            dataset: dataset.into(),
            batch: batch.into(),
            books: 0,
            at: chrono::Utc::now(),
        };
        f.note_published(publish("cvi", "SPX"));
        assert_eq!(f.versions_for([&spx]).data, 1);
        assert_eq!(f.versions_for([&spx_twin]).data, 1);
        assert_eq!(f.versions_for([&ndx, &risk]).data, 0);
        // Updates outlive the recent-publish history, without storing interests
        // for thousands of unrelated datasets or document keys.
        for n in 0..1000 {
            f.note_published(publish(&format!("other-{n}"), "SPX"));
            f.note_published(publish("cvi", &format!("other-{n}")));
        }
        assert_eq!(f.versions_for([&spx]).data, 1);
        assert_eq!(f.versions_for([&ndx, &risk]).data, 0);
        assert_eq!(f.publication_watches.len(), 2);
        assert_eq!(f.publication_watches["cvi"].documents.len(), 2);
        assert_eq!(f.recent_publishes().len(), RECENT_PUBLISHES);
        f.note_published(publish("risk", "EOD"));
        assert_eq!(f.versions_for([&spx, &risk]).data, f.versions().data);
        f.set_scope(book_scope("BK000"));
        f.set_as_of(AsOf::At(chrono::Utc::now()));
        assert!(f.versions_for([&spx]).same_flip_identity(f.versions()));
        drop((risk, spx, ndx));
        let _new = f.watch_publications("new", None);
        assert!(!f.publication_watches.contains_key("risk"));
        assert_eq!(f.publication_watches["cvi"].documents.len(), 1);
        f.note_published(publish("cvi", "SPX"));
        assert_eq!(f.versions_for([&spx_twin]).data, f.versions().data);
    }

    fn named(text: &str) -> geode_core::named::NamedExpressions {
        use geode_core::config::{LayerDoc, merge_docs};
        let doc = LayerDoc::builtin(geode_core::config::EXPRESSIONS_DOC, text).unwrap();
        let merged = merge_docs(geode_core::config::EXPRESSIONS_DOC, &[doc]);
        let (n, diags) = geode_core::named::NamedExpressions::from_doc(
            &merged,
            &geode_core::scope::complete::ExprVocab::default(),
        );
        assert!(diags.is_empty(), "{diags:?}");
        n
    }

    #[test]
    fn effective_scope_resolves_named_expressions_and_refuses_a_missing_one() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let mut scope = book_scope("BK000");
        scope.named = vec!["gone".into()];
        f.set_scope(scope);
        assert_eq!(
            f.effective_scope(&Scope::default()),
            Err("named expression 'gone' is missing".to_string())
        );

        assert!(f.replace_named_expressions(named("[gone]\nexpression = \"npv > 0\"\n")));
        let resolved = f.effective_scope(&Scope::default()).unwrap();
        assert!(resolved.named.is_empty(), "{resolved:?}");
        assert_eq!(
            resolved.expression.map(|e| e.to_string()).as_deref(),
            Some("npv > 0")
        );
        assert_eq!(resolved.dimensions, book_scope("BK000").dimensions);
    }

    #[test]
    fn replace_named_expressions_bumps_config_exactly_when_content_changes() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let v0 = f.versions();
        let liq = named("[liq]\nexpression = \"npv > 0\"\n");
        assert!(f.replace_named_expressions(liq.clone()));
        let v1 = f.versions();
        assert_eq!(v1.config, v0.config + 1);
        assert_eq!(f.named_expressions(), &liq);

        assert!(!f.replace_named_expressions(liq), "same content");
        assert_eq!(f.versions(), v1);

        assert!(f.replace_named_expressions(named("[liq]\nexpression = \"npv > 5\"\n")));
        assert_eq!(f.versions().config, v1.config + 1);
    }
}
