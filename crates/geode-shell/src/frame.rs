//! The shared frame: global scope, undo/redo history, the active grouping slot,
//! as-of (with one remembered previous value), recent publishes, saved
//! scopes, and the data and config generations, as one value every tile
//! observes. Pure: `ShellView` holds it in a gpui entity and notifies; a
//! module reads it through that entity.
//!
//! The selection (scope with its history, active slot, as-of) lives in a
//! lane: one shared lane, plus one per pinned workspace. A workspace reads
//! and writes its lane through `Frame::view`/`Frame::view_mut`; definitions
//! and publications stay frame-wide. Lane generations all come from one
//! counter, so a number names exactly one value in any lane.
//!
//! A tile may follow one of four link groups. Its reading
//! (`Frame::view_for`) takes the scope and scope generation from the group
//! and everything else from its workspace's lane; group scope generations
//! come from the same counter as the lanes'. A tile may also emit into a
//! group: what it posts sets the group's scope and its board of draft
//! documents, which a `BoardWatch` follows apart from `data`.
//!
//! Every mutation bumps exactly the counters it affects, so a tile can
//! compare the fields it follows against the ones it last acted on with
//! one integer compare each — a pinned tile ignores `grouping`, an
//! unscoped tile ignores `scope`. Publication watches narrow `data` to the
//! datasets/documents a consumer reads; other counters retain their contracts.

pub use crate::frame_ref::FrameRef;
pub use crate::link::BoardWatch;
use crate::link::{GroupLane, Links};
use crate::perf::RequeryStats;
use crate::scopebar::{self, ScopeBarModel};
use crate::tiling::{TileId, WorkspaceIx};
use geode_core::config::Layer;
use geode_core::document::{DocumentRows, KEY_SEPARATOR, is_key_prefix};
use geode_core::groupings::GroupingSlots;
use geode_core::link::{Emission, Group, Membership};
use geode_core::named::NamedExpressions;
use geode_core::query::{AsOf, QueryKey};
use geode_core::scope::{Expr, Scope};
use geode_core::scopes::SavedScopes;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};
use toml_edit::value;

/// Minimum barrier age before [`Frame::sweep`] releases incomplete work.
/// The caller must schedule sweeps; the duration alone does not release it.
pub const FLIP_DEADLINE: Duration = Duration::from_millis(250);

/// Maximum stored scope undo depth, bounding retained scope allocations.
pub const UNDO_DEPTH: usize = 32;

/// Maximum recent publishes retained, most recently received first.
pub const RECENT_PUBLISHES: usize = 32;

/// [`FrameViewMut::replace_expression_term`]'s refusal: the scope no longer has
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
/// Read through `FrameView::versions_for` wherever a request or staged result is
/// compared, so requery and promotion use exactly the same dependency boundary.
#[derive(Debug, Clone)]
pub struct PublicationWatch {
    dataset: String,
    batch: Option<String>,
    revision: Rc<Cell<u64>>,
}

impl PublicationWatch {
    /// Whether this watch was registered for exactly this dataset and key —
    /// an identity test, unlike [`matches`](Self::matches), which asks
    /// whether a publish concerns the watch. Use it to decide whether to
    /// keep a watch or register a new one.
    pub fn is_for(&self, dataset: &str, batch: Option<&str>) -> bool {
        self.dataset == dataset && self.batch.as_deref() == batch
    }

    /// Whether a publish of `batch` in `dataset` concerns this watch: a
    /// dataset-wide watch hears a dataset-wide notice; a document watch
    /// hears a batch that is its key or lies under it (`is_key_prefix`),
    /// so a watch on an underlying hears every expiry of a two-part-key
    /// dataset.
    pub fn matches(&self, dataset: &str, batch: Option<&str>) -> bool {
        self.dataset == dataset
            && match (self.batch.as_deref(), batch) {
                (None, None) => true,
                (Some(watched), Some(published)) => is_key_prefix(watched, published),
                _ => false,
            }
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
    /// Global on `FrameView::versions`, dependency-specific on `FrameView::versions_for`.
    pub data: u64,
    pub config: u64,
    /// Bumped by [`FrameViewMut::save_scope`] without changing `config`: saving a
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

/// Coordinates arrivals for one flip. Each awaited key carries the
/// `(scope, grouping, as_of)` identity its tile will answer under: tiles in
/// one flip can read different scopes (a tile following a link group reads
/// the group's), and one shared identity would leave such a tile's arrival
/// unmatched and hold every flip to its deadline. Tiles hold staged results
/// until all keys arrive or a sweep releases the expired barrier. A timeout
/// permits ready tiles to advance while slow tiles still show older data.
/// Opening another barrier replaces this one, and an arrival under a key's
/// old identity cannot satisfy the new one.
#[derive(Debug)]
struct FlipBarrier {
    /// Captured counters per key; only scope/grouping/as-of participate in
    /// identity.
    awaiting: HashMap<QueryKey, FrameVersions>,
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

/// Advance the frame-wide generation counter and return the new value.
/// Every lane draws its scope/grouping/as-of numbers from this one counter,
/// so a number names exactly one value in any lane: a tile that switches
/// lanes can never mistake different content for "unchanged".
fn fresh(counter: &mut u64) -> u64 {
    *counter += 1;
    *counter
}

/// The selection one workspace sees: the shared lane, or a pinned
/// workspace's own. Definitions (slots, saved scopes, named expressions)
/// and publications stay on `Frame`.
#[derive(Debug, Default)]
struct Lane {
    scope: Scope,
    /// Bounded stack of outgoing scopes, oldest first — `undo_scope` pops
    /// the back, `redo_scope` pushes it back on. Capped at [`UNDO_DEPTH`]
    /// by `push_undo`, which drops the oldest entry once full.
    scope_undo: Vec<Scope>,
    scope_redo: Vec<Scope>,
    /// An explicitly opened scope-editing session, usually owned by the text field.
    scope_session: Option<ScopeSession>,
    active_slot: Option<u8>,
    as_of: AsOf,
    /// The remembered as-of value. Repeated undo swaps between two values.
    previous_as_of: Option<AsOf>,
    scope_gen: u64,
    grouping_gen: u64,
    as_of_gen: u64,
}

impl Lane {
    /// A pinned workspace's starting lane: the same values and generations
    /// (equal content, so equal numbers are truthful and nothing
    /// requeries), with no history of its own.
    fn pinned_copy(&self) -> Lane {
        Lane {
            scope: self.scope.clone(),
            active_slot: self.active_slot,
            as_of: self.as_of.clone(),
            scope_gen: self.scope_gen,
            grouping_gen: self.grouping_gen,
            as_of_gen: self.as_of_gen,
            ..Lane::default()
        }
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
    /// The lane every unpinned workspace resolves to.
    shared: Lane,
    /// One lane per pinned workspace. Ordered so session writes are stable.
    pinned: BTreeMap<WorkspaceIx, Lane>,
    /// Link groups: each group's scope and board, and which tile follows
    /// or emits into which.
    links: Links,
    /// Source of every lane's scope/grouping/as-of generation and every
    /// link group's scope generation; also advanced by pin, unpin and a
    /// membership change, which makes it the session writer's dirty signal.
    generation: u64,
    slots: GroupingSlots,
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
    /// Frame-wide counters. Only `data`, `config`, `saved_scopes`, and `flip`
    /// are read from here; its scope/grouping/as-of fields stay zero, because
    /// [`FrameView::versions`] composes those from the resolved lane.
    versions: FrameVersions,
    /// Module requery timings, exposed through the shared frame handle.
    pub requery: RequeryStats,
    user_dir: Option<PathBuf>,
    /// Latest saved slot awaiting persistence to user `groupings.toml`.
    /// The shell drains it on frame notification and writes off the UI thread.
    pending_persist: Option<(u8, Vec<String>)>,
    /// Lazy model cache keyed by versions excluding flip, clock, and local date.
    /// `Rc` makes a hit cheap; interior mutability permits caching through `&self`.
    /// Lane generations are unique across lanes, so one cache serves them all.
    bar_cache: BarCache,
    /// Current scope/grouping/as-of barrier, if one is waiting for arrivals.
    barrier: Option<FlipBarrier>,
}

impl Frame {
    pub fn new(slots: GroupingSlots, saved: SavedScopes, user_dir: Option<PathBuf>) -> Frame {
        Frame {
            shared: Lane::default(),
            pinned: BTreeMap::new(),
            links: Links::default(),
            generation: 0,
            slots,
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

    fn lane(&self, ws: Option<WorkspaceIx>) -> &Lane {
        ws.and_then(|w| self.pinned.get(&w)).unwrap_or(&self.shared)
    }

    fn lane_mut(&mut self, ws: Option<WorkspaceIx>) -> &mut Lane {
        let Frame { shared, pinned, .. } = self;
        match ws.and_then(|w| pinned.get_mut(&w)) {
            Some(lane) => lane,
            None => shared,
        }
    }

    /// The group `tile` follows and that group's lane; `None` for a view
    /// bound to no tile and for a tile that follows its workspace.
    fn followed(&self, tile: Option<TileId>) -> Option<(Group, &GroupLane)> {
        let group = self.links.following(tile?)?;
        Some((group, self.links.group(group)))
    }

    pub fn view(&self, ws: WorkspaceIx) -> FrameView<'_> {
        FrameView {
            frame: self,
            lane: self.lane(Some(ws)),
            group: None,
        }
    }

    pub fn view_mut(&mut self, ws: WorkspaceIx) -> FrameViewMut<'_> {
        FrameViewMut {
            frame: self,
            ws: Some(ws),
            tile: None,
        }
    }

    /// One tile's reading: its workspace's lane, with the scope and scope
    /// generation of the link group it follows, when it follows one.
    pub fn view_for(&self, ws: WorkspaceIx, tile: TileId) -> FrameView<'_> {
        FrameView {
            frame: self,
            lane: self.lane(Some(ws)),
            group: self.followed(Some(tile)),
        }
    }

    /// One tile's writable view: `set_scope` and `clear_scope` write the
    /// link group it follows, when it follows one; every other write is
    /// its workspace lane's. Crate-private: a tile reaches it through its
    /// own `FrameRef::update`, and no module can build one for another
    /// tile.
    pub(crate) fn view_mut_for(&mut self, ws: WorkspaceIx, tile: TileId) -> FrameViewMut<'_> {
        FrameViewMut {
            frame: self,
            ws: Some(ws),
            tile: Some(tile),
        }
    }

    /// The shared lane explicitly, whatever any workspace is pinned to.
    /// Session restore and `[frame]` writes use it.
    pub fn shared(&self) -> FrameView<'_> {
        FrameView {
            frame: self,
            lane: &self.shared,
            group: None,
        }
    }

    pub fn shared_mut(&mut self) -> FrameViewMut<'_> {
        FrameViewMut {
            frame: self,
            ws: None,
            tile: None,
        }
    }

    pub fn membership(&self, tile: TileId) -> Membership {
        self.links.membership(tile)
    }

    /// Follow a link group, or the workspace again with `None`. Advances
    /// the generation (the session writer's dirty signal) when it changes
    /// something; the tile's next `versions()` differs in `scope`, which is
    /// what makes it requery. The one exception is a lane and a group that
    /// both hold the empty scope at generation zero (neither was ever
    /// written): equal content under an equal number, so nothing requeries
    /// and nothing needs to.
    ///
    /// Crate-private, like every write of a tile's membership or emission:
    /// the shell's doors (`ShellView::set_follow`, `set_emit` and the
    /// emission pull) are the only callers, so a module, which reaches
    /// `Frame` through its handle's `DerefMut`, has no door to a group.
    pub(crate) fn follow(&mut self, tile: TileId, group: Option<Group>) -> bool {
        let changed = self.links.follow(tile, group);
        if changed {
            fresh(&mut self.generation);
        }
        changed
    }

    /// Emit into a link group, or into none. Advances the generation when
    /// it changes something. A tile that leaves or switches group takes
    /// what it posted off the old group's board at once; that group's scope
    /// stays as last written.
    pub(crate) fn emit(&mut self, tile: TileId, group: Option<Group>) -> bool {
        let changed = self.links.emit(tile, group);
        if changed {
            fresh(&mut self.generation);
        }
        changed
    }

    pub fn group_scope(&self, group: Group) -> &Scope {
        &self.links.group(group).scope
    }

    /// Each group's scope generation, in `Group::ALL` order: what the shell
    /// compares to see that a group's scope moved.
    pub fn group_scope_gens(&self) -> [u64; 4] {
        self.links.scope_gens()
    }

    /// Record what an emitting tile answered. `true` when its group's scope
    /// or board changed and observers should be notified. A tile that emits
    /// into no group, or repeats its last answer, writes nothing; a board
    /// change never moves `data`.
    pub(crate) fn post_emission(&mut self, tile: TileId, emission: Emission) -> bool {
        let Frame {
            links, generation, ..
        } = self;
        links.post(tile, emission, generation)
    }

    /// Drop a closed tile's membership and what it posted. `true`, and the
    /// generation advances, when it was in a group.
    pub(crate) fn forget_tile(&mut self, tile: TileId) -> bool {
        let changed = self.links.forget(tile);
        if changed {
            fresh(&mut self.generation);
        }
        changed
    }

    /// Test-only: put `tile` in these groups, as the shell's doors would.
    /// A test outside this crate has no shell to go through; production
    /// code links a tile through `ShellView::set_follow` and `set_emit`
    /// only.
    #[cfg(any(test, feature = "test-support"))]
    pub fn link_for_test(&mut self, tile: TileId, membership: Membership) {
        self.follow(tile, membership.follow);
        self.emit(tile, membership.emit);
    }

    /// Test-only: record `emission` as `tile`'s, as the shell's pull would.
    /// `true` when its group's scope or board changed. Production code
    /// posts only what the shell pulled from `TileContent::emission`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn post_for_test(&mut self, tile: TileId, emission: Emission) -> bool {
        self.post_emission(tile, emission)
    }

    /// Watch one group's board for a dataset, or one document key (or key
    /// prefix) in it. `key` is the joined key, as
    /// `geode_core::document::join_key` builds it from the key's parts, or
    /// a prefix of it ending at a part boundary; [`Self::board_entry`]
    /// takes the parts themselves. Registration does not notify. Board
    /// reads ignore as-of: a draft is now.
    pub fn watch_board(&mut self, group: Group, dataset: &str, key: Option<&str>) -> BoardWatch {
        self.links.watch(group, dataset, key)
    }

    /// The draft on `group`'s board for this dataset and document key.
    /// `key` is the key's parts, every one of them: not the joined string
    /// [`Self::watch_board`] takes (`geode_core::document::join_key`), and
    /// never a prefix.
    pub fn board_entry(
        &self,
        group: Group,
        dataset: &str,
        key: &[String],
    ) -> Option<Arc<DocumentRows>> {
        self.links.entry(group, dataset, key)
    }

    /// How many times `group`'s board has changed.
    pub fn board_gen(&self, group: Group) -> u64 {
        self.links.board_gen(group)
    }

    /// Give `ws` its own lane, copied from the shared one. `false` when it
    /// is already pinned. Advances the generation without touching the
    /// lane's numbers.
    pub fn pin(&mut self, ws: WorkspaceIx) -> bool {
        if self.pinned.contains_key(&ws) {
            return false;
        }
        let lane = self.shared.pinned_copy();
        self.pinned.insert(ws, lane);
        fresh(&mut self.generation);
        true
    }

    /// Drop `ws`'s lane, discarding its selection and history; it resolves
    /// to the shared lane again. `false` when it was not pinned.
    pub fn unpin(&mut self, ws: WorkspaceIx) -> bool {
        if self.pinned.remove(&ws).is_none() {
            return false;
        }
        fresh(&mut self.generation);
        true
    }

    pub fn is_pinned(&self, ws: WorkspaceIx) -> bool {
        self.pinned.contains_key(&ws)
    }

    pub fn pinned_workspaces(&self) -> impl Iterator<Item = WorkspaceIx> + '_ {
        self.pinned.keys().copied()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn config_version(&self) -> u64 {
        self.versions.config
    }

    pub fn data_version(&self) -> u64 {
        self.versions.data
    }

    pub fn user_dir(&self) -> Option<&Path> {
        self.user_dir.as_deref()
    }

    pub fn slots(&self) -> &GroupingSlots {
        &self.slots
    }

    /// Replace grouping slots after reload. A change bumps config and every
    /// lane's grouping, even where the active slot is unchanged. Clear an
    /// active slot that disappeared.
    pub fn replace_slots(&mut self, slots: GroupingSlots) -> bool {
        if self.slots == slots {
            return false;
        }
        self.slots = slots;
        self.versions.config += 1;
        // Every lane regroups, hidden pinned ones included: the numbers a
        // lane's active slot names may now hold other columns.
        let Frame {
            shared,
            pinned,
            slots,
            generation,
            ..
        } = self;
        for lane in std::iter::once(shared).chain(pinned.values_mut()) {
            if lane.active_slot.is_some_and(|n| slots.get(n).is_none()) {
                lane.active_slot = None;
            }
            lane.grouping_gen = fresh(generation);
        }
        true
    }

    /// Save a nonempty grouping in slot 1–9 and replace the pending write.
    /// Bump grouping only in the lanes where that slot is active. Production
    /// grouping edits use the Groupings dialog's config writer; this model API
    /// remains independently usable and covered by tests.
    pub fn save_slot(&mut self, slot: u8, grouping: Vec<String>) -> Result<(), String> {
        let persisted = grouping.clone();
        if !self.slots.set(slot, grouping) {
            return Err(format!(
                "slot must be 1–9 and the grouping non-empty (got {slot})"
            ));
        }
        let Frame {
            shared,
            pinned,
            generation,
            ..
        } = self;
        for lane in std::iter::once(shared).chain(pinned.values_mut()) {
            if lane.active_slot == Some(slot) {
                lane.grouping_gen = fresh(generation);
            }
        }
        self.pending_persist = Some((slot, persisted));
        Ok(())
    }

    /// Drain the latest pending slot write for the shell's background writer.
    pub fn take_pending_persist(&mut self) -> Option<(u8, Vec<String>)> {
        self.pending_persist.take()
    }

    /// Watch a whole dataset (`None`) or a document's encoded batch key:
    /// `Some(key)` watches that document, or every document under it when
    /// `key` is a shorter prefix of a multi-part key.
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

    /// Record a publish for the global history and advance only matching tile
    /// watches. Global `data` remains available to the as-of picker and chrome.
    pub fn note_published(&mut self, publish: Publish) {
        self.versions.data += 1;
        if let Some(watches) = self.publication_watches.get(&publish.dataset) {
            if let Some(revision) = watches.dataset.upgrade() {
                revision.set(self.versions.data);
            }
            // Every key-part prefix of the batch, then the batch itself:
            // a watch on `SPX` hears `SPX␟2026-10-16`, never `SPXW␟…`.
            let batch = publish.batch.as_str();
            let ends = batch
                .match_indices(KEY_SEPARATOR)
                .map(|(i, _)| i)
                .chain(std::iter::once(batch.len()));
            for end in ends {
                if let Some(revision) = watches.documents.get(&batch[..end]).and_then(Weak::upgrade)
                {
                    revision.set(self.versions.data);
                }
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

    /// Drain the latest pending scope write for the shell's background writer.
    pub fn take_pending_scope_persist(&mut self) -> Option<(String, Scope)> {
        self.pending_scope_persist.take()
    }

    pub fn note_config_reloaded(&mut self) {
        self.versions.config += 1;
    }

    /// Whether `b` still waits for `key` under the identity `versions`
    /// carries. A key is matched against its own captured identity only.
    fn awaits(b: &FlipBarrier, key: QueryKey, versions: FrameVersions) -> bool {
        b.awaiting
            .get(&key)
            .is_some_and(|opened| opened.same_flip_identity(versions))
    }

    /// Whether an open barrier is waiting for `key` at `versions` —
    /// `false` once nothing is open, once `key` already arrived, or once
    /// a later mutation replaced the barrier with one that awaits `key`
    /// under different versions (a stale outcome from before the
    /// replacement must not satisfy it).
    pub fn barrier_wants(&self, key: QueryKey, versions: FrameVersions) -> bool {
        self.barrier
            .as_ref()
            .is_some_and(|b| Self::awaits(b, key, versions))
    }

    /// `key`'s outcome for `versions` arrived — a failed outcome counts
    /// too (`geode-blotter`'s `deliver`: one broken tile must never hold
    /// the rest open). An arrival under an identity other than the one
    /// `key` was opened with changes nothing. `true` exactly when this
    /// arrival emptied the barrier, which also bumps `flip` via `release`;
    /// the caller uses the return value to promote its own staged
    /// snapshot right away rather than waiting for its own
    /// `on_frame_changed` to see the bump.
    pub fn arrived(&mut self, key: QueryKey, versions: FrameVersions) -> bool {
        let Some(b) = self.barrier.as_mut() else {
            return false;
        };
        if !Self::awaits(b, key, versions) {
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

    /// Replace the barrier with these keys, each under its own identity. A
    /// key listed twice keeps the last identity given for it. An empty set
    /// clears the barrier without bumping flip. No version changes or
    /// notifications are emitted here.
    pub(crate) fn open_flip_each(
        &mut self,
        keys: impl IntoIterator<Item = (QueryKey, FrameVersions)>,
        now: Instant,
    ) {
        let awaiting: HashMap<QueryKey, FrameVersions> = keys.into_iter().collect();
        self.barrier = (!awaiting.is_empty()).then_some(FlipBarrier {
            awaiting,
            opened: now,
        });
    }

    /// Add these keys to the flip in progress, each under its own identity.
    /// The keys already awaited stay, a key listed again takes the identity
    /// given here, and the barrier keeps the instant it opened, so its
    /// deadline does not move. With nothing open this opens a barrier at
    /// `now`. An empty set changes nothing: it never clears an open barrier.
    /// No version changes or notifications are emitted here.
    ///
    /// For a change that concerns only some visible tiles (a link group's
    /// scope). Replacing the barrier there would drop the other tiles while
    /// their queries are in flight; they would apply on arrival, beside
    /// tiles still holding what they staged.
    pub(crate) fn extend_flip(
        &mut self,
        keys: impl IntoIterator<Item = (QueryKey, FrameVersions)>,
        now: Instant,
    ) {
        match self.barrier.as_mut() {
            Some(barrier) => barrier.awaiting.extend(keys),
            None => self.open_flip_each(keys, now),
        }
    }

    /// `key`'s tile now answers under `versions`: it started or stopped
    /// following a link group, which changes its scope generation. When an
    /// open barrier awaits `key`, hold it to that identity from here on;
    /// left under the old one the tile's next arrival would not match and
    /// the flip would wait out its deadline. A key nothing awaits is left
    /// out: this never opens, releases or notifies.
    pub(crate) fn reidentify(&mut self, key: QueryKey, versions: FrameVersions) {
        if let Some(awaited) = self.barrier.as_mut().and_then(|b| b.awaiting.get_mut(&key)) {
            *awaited = versions;
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

/// One workspace's reading of the frame: shared state through `Deref`,
/// selection state from the lane the workspace resolves to. A view built
/// for a tile that follows a link group (`Frame::view_for`) reads the scope
/// and its generation from that group instead.
#[derive(Clone, Copy)]
pub struct FrameView<'a> {
    frame: &'a Frame,
    lane: &'a Lane,
    /// The followed link group and its lane; `None` reads the lane's scope.
    group: Option<(Group, &'a GroupLane)>,
}

impl Deref for FrameView<'_> {
    type Target = Frame;
    fn deref(&self) -> &Frame {
        self.frame
    }
}

impl<'a> FrameView<'a> {
    /// The link group this view's tile follows, if any.
    pub fn following(&self) -> Option<Group> {
        self.group.map(|(g, _)| g)
    }

    /// This view with no group: the workspace lane's own scope and scope
    /// generation, which is what the scope bar shows.
    fn lane_view(&self) -> FrameView<'a> {
        FrameView {
            group: None,
            ..*self
        }
    }

    /// A follower's `scope` is its group's generation, so a lane scope
    /// edit it does not read is not a change to it, and a group change is.
    pub fn versions(&self) -> FrameVersions {
        FrameVersions {
            scope: self.group.map_or(self.lane.scope_gen, |(_, g)| g.scope_gen),
            grouping: self.lane.grouping_gen,
            as_of: self.lane.as_of_gen,
            ..self.frame.versions
        }
    }

    /// `versions` with `data` narrowed to `watches` (see `PublicationWatch`).
    /// Scope/grouping/as-of and flip identity are this view's, unchanged.
    /// Watches must come from this frame and remain alive with the consumer.
    pub fn versions_for<'w>(
        &self,
        watches: impl IntoIterator<Item = &'w PublicationWatch>,
    ) -> FrameVersions {
        FrameVersions {
            data: watches
                .into_iter()
                .map(|w| w.revision.get())
                .max()
                .unwrap_or(0),
            ..self.versions()
        }
    }

    /// The scope this view queries under: the followed group's, else the
    /// lane's.
    pub fn scope(&self) -> &'a Scope {
        self.group.map_or(&self.lane.scope, |(_, g)| &g.scope)
    }

    pub fn active_slot(&self) -> Option<u8> {
        self.lane.active_slot
    }

    pub fn active_grouping(&self) -> Option<&'a [String]> {
        self.frame.slots.get(self.lane.active_slot?)
    }

    pub fn as_of(&self) -> &'a AsOf {
        &self.lane.as_of
    }

    /// Whether top-level expression term `i` still equals `expected`: the
    /// check [`FrameViewMut::replace_expression_term`] makes, for a caller
    /// that must refuse before doing anything else (writing a definition).
    /// Reads the workspace lane even through a follower's view: it serves
    /// the scope bar, which shows the workspace.
    pub fn expression_term_is(&self, i: usize, expected: &Expr) -> bool {
        self.lane
            .scope
            .expression
            .as_ref()
            .and_then(|e| e.conjuncts().get(i).copied())
            .is_some_and(|t| t == expected)
    }

    /// Compose this view's scope (the followed group's, else the lane's)
    /// with the tile layer through `Scope::and_then`, then fold in every
    /// named expression it references. A follower's lane scope is not
    /// composed in: the group replaces it. A missing or invalid name is an
    /// error the caller shows instead of querying: skipping it would widen
    /// the scope and produce plausible wrong totals.
    pub fn effective_scope(&self, tile: &Scope) -> Result<Scope, String> {
        self.scope().and_then(tile).resolve(&self.frame.named)
    }

    /// Return cached scope-bar labels for versions excluding flip, the configured
    /// clock, and today's date on that clock. Clock/date changes invalidate labels
    /// even without a frame mutation, covering zone reloads and midnight.
    /// The caller supplies cached time inputs; this method reads no global clock.
    ///
    /// The bar shows the workspace, so the model is built from the lane
    /// whatever the view follows: a model built from a follower's view
    /// would describe its group's scope under the workspace's controls. A
    /// module asking through its own handle gets the workspace's model.
    pub fn bar_model(
        &self,
        clock: geode_core::clock::Clock,
        today: chrono::NaiveDate,
    ) -> Rc<ScopeBarModel> {
        let lane = self.lane_view();
        // `flip` alone never changes what the bar shows — keyed out here
        // (rather than relying on it happening to already match) so a
        // flip costs a refcount bump like any other unrelated notify,
        // not a rebuild.
        let mut versions = lane.versions();
        versions.flip = 0;
        if let Some((cached_versions, cached_clock, cached_today, cached)) =
            self.frame.bar_cache.borrow().as_ref()
            && *cached_versions == versions
            && *cached_clock == clock
            && *cached_today == today
        {
            return Rc::clone(cached);
        }
        let built = Rc::new(scopebar::build_model(&lane, clock, today));
        *self.frame.bar_cache.borrow_mut() = Some((versions, clock, today, Rc::clone(&built)));
        built
    }
}

/// One workspace's writable frame. The lane is resolved per call, so a
/// pin or unpin through `DerefMut` redirects later calls at once; so is the
/// link group a bound tile follows.
pub struct FrameViewMut<'a> {
    frame: &'a mut Frame,
    /// `None` addresses the shared lane explicitly.
    ws: Option<WorkspaceIx>,
    /// The tile this view answers for; `None` is the workspace itself.
    tile: Option<TileId>,
}

impl Deref for FrameViewMut<'_> {
    type Target = Frame;
    fn deref(&self) -> &Frame {
        self.frame
    }
}

impl DerefMut for FrameViewMut<'_> {
    fn deref_mut(&mut self) -> &mut Frame {
        self.frame
    }
}

impl<'a> FrameViewMut<'a> {
    pub fn view(&self) -> FrameView<'_> {
        FrameView {
            frame: &*self.frame,
            lane: self.frame.lane(self.ws),
            group: self.frame.followed(self.tile),
        }
    }

    // Reads answer as `view()` does, so a read through a mutable view
    // never needs a separate `view()` call.

    pub fn versions(&self) -> FrameVersions {
        self.view().versions()
    }

    pub fn versions_for<'w>(
        &self,
        watches: impl IntoIterator<Item = &'w PublicationWatch>,
    ) -> FrameVersions {
        self.view().versions_for(watches)
    }

    pub fn scope(&self) -> &Scope {
        self.view().scope()
    }

    pub fn active_slot(&self) -> Option<u8> {
        self.frame.lane(self.ws).active_slot
    }

    pub fn active_grouping(&self) -> Option<&[String]> {
        self.view().active_grouping()
    }

    pub fn as_of(&self) -> &AsOf {
        &self.frame.lane(self.ws).as_of
    }

    pub fn expression_term_is(&self, i: usize, expected: &Expr) -> bool {
        self.view().expression_term_is(i, expected)
    }

    pub fn effective_scope(&self, tile: &Scope) -> Result<Scope, String> {
        self.view().effective_scope(tile)
    }

    pub fn bar_model(
        &self,
        clock: geode_core::clock::Clock,
        today: chrono::NaiveDate,
    ) -> Rc<ScopeBarModel> {
        self.view().bar_model(clock, today)
    }

    fn lane(&mut self) -> &mut Lane {
        self.frame.lane_mut(self.ws)
    }

    fn bump_scope(&mut self) {
        let g = fresh(&mut self.frame.generation);
        self.lane().scope_gen = g;
    }

    fn bump_grouping(&mut self) {
        let g = fresh(&mut self.frame.generation);
        self.lane().grouping_gen = g;
    }

    fn bump_as_of(&mut self) {
        let g = fresh(&mut self.frame.generation);
        self.lane().as_of_gen = g;
    }

    /// Replace the scope, pushing its outgoing value and clearing redo.
    /// An equal value returns false without changing history or versions.
    /// Through the view of a tile that follows a link group, this replaces
    /// the group's scope instead; a group keeps no history.
    pub fn set_scope(&mut self, scope: Scope) -> bool {
        // A follower's scope is its group's: writing the lane would change
        // what the scope bar shows and leave the tile reading the old value.
        if let Some(g) = self.tile.and_then(|t| self.frame.links.following(t)) {
            let Frame {
                links, generation, ..
            } = &mut *self.frame;
            return links.set_scope(g, scope, generation);
        }
        self.set_lane_scope(scope)
    }

    /// `set_scope` for the workspace lane, whatever the view's tile
    /// follows. The scope bar's edits derive the new scope from the lane's
    /// and go through here: sent to a followed group they would overwrite
    /// its scope with an edited copy of the lane's.
    fn set_lane_scope(&mut self, scope: Scope) -> bool {
        let lane = self.lane();
        if lane.scope == scope {
            return false;
        }
        let outgoing = std::mem::replace(&mut lane.scope, scope);
        lane.push_undo(outgoing);
        self.bump_scope();
        true
    }

    /// Empty the scope `set_scope` writes: the followed group's, else the
    /// lane's.
    pub fn clear_scope(&mut self) -> bool {
        self.set_scope(Scope::default())
    }

    /// Start or replace a session, capturing the current scope and redo stack.
    /// Nothing is pushed until the first actual session edit. Sessions edit
    /// the workspace lane even through a follower's view.
    pub fn begin_scope_session(&mut self) {
        let lane = self.lane();
        lane.scope_session = Some(ScopeSession {
            base: lane.scope.clone(),
            pushed: false,
            redo_snapshot: lane.scope_redo.clone(),
        });
    }

    /// Replace the scope and bump its version on an actual change. An open
    /// session pushes its base only once, even if other edits move the stack.
    /// Without a session, each change pushes the outgoing scope and clears redo.
    /// Edits the workspace lane even through a follower's view.
    pub fn set_scope_in_session(&mut self, scope: Scope) -> bool {
        let lane = self.lane();
        if lane.scope == scope {
            return false;
        }
        match lane.scope_session.as_ref() {
            Some(session) if !session.pushed => {
                let base = session.base.clone();
                lane.push_undo(base);
                if let Some(session) = lane.scope_session.as_mut() {
                    session.pushed = true;
                }
            }
            Some(_) => {}
            None => {
                let outgoing = lane.scope.clone();
                lane.push_undo(outgoing);
            }
        }
        lane.scope = scope;
        self.bump_scope();
        true
    }

    /// End coalescing. If the session pushed its base, the current scope equals
    /// that base, and the base remains the top undo entry, pop it and restore
    /// the captured redo stack. Otherwise leave history as it stands.
    /// Subsequent edits use ordinary history until another session is opened.
    /// Edits the workspace lane even through a follower's view.
    pub fn end_scope_session(&mut self) {
        let lane = self.lane();
        if let Some(session) = lane.scope_session.take()
            && session.pushed
            && lane.scope_undo.last() == Some(&session.base)
            && lane.scope == session.base
        {
            lane.scope_undo.pop();
            lane.scope_redo = session.redo_snapshot;
        }
    }

    /// Undo one scope edit. An empty stack returns false without a version bump.
    /// Edits the workspace lane even through a follower's view.
    pub fn undo_scope(&mut self) -> bool {
        let lane = self.lane();
        let Some(previous) = lane.scope_undo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut lane.scope, previous);
        lane.scope_redo.push(current);
        self.bump_scope();
        true
    }

    /// Redo one scope edit. An ordinary edit or the first mutation of a new
    /// session clears this stack; later mutations in an open session do not.
    /// Edits the workspace lane even through a follower's view.
    pub fn redo_scope(&mut self) -> bool {
        let lane = self.lane();
        let Some(next) = lane.scope_redo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut lane.scope, next);
        lane.scope_undo.push(current);
        self.bump_scope();
        true
    }

    /// Remove every selection for a column through the undoable `set_scope`
    /// path. Preserve other fields, including `impossible`. Return false when
    /// there is no matching selection. Edits the workspace lane even through
    /// a follower's view.
    pub fn drop_dimension(&mut self, column: &str) -> bool {
        let mut s = self.lane().scope.clone();
        let before = s.dimensions.len();
        s.dimensions.retain(|d| d.column != column);
        if s.dimensions.len() == before {
            return false;
        }
        self.set_lane_scope(s)
    }

    /// Remove named expression `name` from the scope through the undoable
    /// `set_scope` path. Return false when the scope does not name it.
    /// Edits the workspace lane even through a follower's view.
    pub fn drop_named(&mut self, name: &str) -> bool {
        let mut s = self.lane().scope.clone();
        let before = s.named.len();
        s.named.retain(|n| n != name);
        if s.named.len() == before {
            return false;
        }
        self.set_lane_scope(s)
    }

    /// Remove top-level expression term `i` (`Expr::conjuncts` order)
    /// through the undoable `set_scope` path; the remaining terms are
    /// rebuilt as a left-folded `and` chain, and removing the last one
    /// leaves no expression. Out of range (including no expression)
    /// returns false and changes nothing. Like every term edit, it edits
    /// the workspace lane even through a follower's view.
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

    /// Replace top-level expression term `i` with the named expression
    /// `name`: the term leaves the expression and the name joins the named
    /// list, in one `set_scope` so a single undo puts the term back.
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
        let lane = self.lane();
        let mut terms: Vec<Expr> = lane
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
        let mut s = lane.scope.clone();
        s.expression = Expr::from_conjuncts(terms);
        if let Some(name) = name
            && !s.named.iter().any(|n| n == name)
        {
            s.named.push(name.to_string());
        }
        Ok(self.set_lane_scope(s))
    }

    /// Remove the whole expression layer through the undoable `set_scope`
    /// path; false (and no history entry) when there is none. Edits the
    /// workspace lane even through a follower's view.
    pub fn clear_expression(&mut self) -> bool {
        let lane = self.lane();
        if lane.scope.expression.is_none() {
            return false;
        }
        let mut s = lane.scope.clone();
        s.expression = None;
        self.set_lane_scope(s)
    }

    /// Set (or clear, with `None`/whitespace-only) the scope's text
    /// filter — an undoable edit like `drop_dimension`, going through the
    /// ordinary (non-session) `set_scope` path. `begin_scope_session`/
    /// `set_scope_in_session` is the coalescing alternative a live text
    /// field drives per keystroke. Edits the workspace lane even through a
    /// follower's view.
    pub fn set_text(&mut self, text: Option<String>) -> bool {
        let mut s = self.lane().scope.clone();
        s.text = text.filter(|t| !t.trim().is_empty());
        self.set_lane_scope(s)
    }

    /// `Some(n)` activates a filled slot; `None` returns following tiles
    /// to their views' own grouping. `false` when nothing changed or the
    /// slot is empty.
    pub fn set_active_slot(&mut self, slot: Option<u8>) -> bool {
        if let Some(n) = slot
            && self.frame.slots.get(n).is_none()
        {
            return false;
        }
        if self.lane().active_slot == slot {
            return false;
        }
        self.lane().active_slot = slot;
        self.bump_grouping();
        true
    }

    /// Replace as-of and remember its outgoing value for `undo_as_of`.
    /// An equal value returns false without changing versions or history.
    pub fn set_as_of(&mut self, as_of: AsOf) -> bool {
        let lane = self.lane();
        if lane.as_of == as_of {
            return false;
        }
        lane.previous_as_of = Some(std::mem::replace(&mut lane.as_of, as_of));
        self.bump_as_of();
        true
    }

    /// Swap current and remembered as-of. Repeated calls toggle the pair.
    pub fn undo_as_of(&mut self) -> bool {
        let lane = self.lane();
        let Some(previous) = lane.previous_as_of.take() else {
            return false;
        };
        let current = std::mem::replace(&mut lane.as_of, previous);
        lane.previous_as_of = Some(current);
        self.bump_as_of();
        true
    }

    /// Save this lane's scope in memory, replace the pending scope write, and
    /// bump saved_scopes. Validate the object name and reject reserved action
    /// names to prevent collisions when registering `scope::<name>` actions.
    /// An existing name is overwritten; an empty scope is accepted. The
    /// scope saved is the workspace lane's even through a follower's view.
    pub fn save_scope(&mut self, name: &str) -> Result<(), String> {
        let name = geode_core::config::check_object_name(name)
            .map_err(|_| format!("'{}' is not a usable scope name", name.trim()))?;
        if geode_core::scopes::RESERVED_NAMES.contains(&name) {
            return Err(format!("'{name}' is reserved"));
        }
        let scope = self.frame.lane(self.ws).scope.clone();
        self.frame
            .saved_scopes
            .insert(name.to_string(), scope.clone());
        self.frame.pending_scope_persist = Some((name.to_string(), scope));
        self.frame.versions.saved_scopes += 1;
        Ok(())
    }

    /// Load a saved scope by name, going through `set_scope` so it's
    /// undoable like any other scope change. `Err` when no scope by that
    /// name exists; `Ok(false)` when it exists but is already the current
    /// scope. Loads into the workspace lane even through a follower's view.
    pub fn load_scope(&mut self, name: &str) -> Result<bool, String> {
        let scope = self
            .frame
            .saved_scopes
            .get(name)
            .cloned()
            .ok_or_else(|| format!("no saved scope '{name}'"))?;
        Ok(self.set_lane_scope(scope))
    }

    /// Clear undo and redo without changing scope or ending an open session.
    /// Session restoration uses this after applying its initial scope.
    pub fn clear_history(&mut self) {
        let lane = self.lane();
        lane.scope_undo.clear();
        lane.scope_redo.clear();
    }

    /// Replace the barrier with this view's scope/grouping/as-of identity and
    /// these tile keys. An empty key set clears it without bumping flip.
    /// No version changes or notifications are emitted here.
    ///
    /// The shell must open the barrier before occupant frame observers run:
    /// tiles unaffected by the changed inputs can then self-arrive immediately.
    /// Shell observer registration precedes occupant registration to enforce this.
    pub fn open_flip(&mut self, keys: impl IntoIterator<Item = QueryKey>, now: Instant) {
        let versions = self.versions();
        self.frame
            .open_flip_each(keys.into_iter().map(|k| (k, versions)), now);
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
    use crate::tiling::{TileId, WorkspaceIx};
    use geode_core::clock::Clock;
    use geode_core::document::DocumentRows;
    use geode_core::link::{BoardEntry, Emission, Group};
    use geode_core::scope::{DimensionSelection, Scope};
    use std::sync::Arc;

    fn ws(n: u8) -> WorkspaceIx {
        WorkspaceIx::new(n).unwrap()
    }

    #[test]
    fn pinning_copies_values_and_generations_with_empty_history() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("BK000"));
        f.shared_mut().set_active_slot(Some(1));
        f.shared_mut().set_as_of(AsOf::At(chrono::Utc::now()));
        // Every generation has moved off its initial value, so a copy that
        // dropped any of them would differ from the shared lane's.
        let initial = Frame::new(slots(), SavedScopes::new(), None)
            .shared()
            .versions();
        let shared = f.shared().versions();
        assert_ne!(shared.scope, initial.scope);
        assert_ne!(shared.grouping, initial.grouping);
        assert_ne!(shared.as_of, initial.as_of);
        assert!(f.pin(ws(2)));
        assert_eq!(f.view(ws(2)).scope(), f.shared().scope());
        assert_eq!(f.view(ws(2)).active_slot(), Some(1));
        assert_eq!(f.view(ws(2)).as_of(), f.shared().as_of());
        assert_eq!(f.view(ws(2)).versions(), f.shared().versions());
        assert!(!f.view_mut(ws(2)).undo_scope(), "a new lane has no history");
        assert!(!f.pin(ws(2)), "pinning twice is refused");
    }

    #[test]
    fn an_edit_in_one_lane_leaves_the_other_alone() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.pin(ws(2));
        let shared_before = f.shared().versions();
        assert!(f.view_mut(ws(2)).set_scope(book_scope("BK001")));
        assert_eq!(f.shared().versions(), shared_before);
        assert_eq!(f.shared().scope(), &Scope::default());
        let pinned_before = f.view(ws(2)).versions();
        assert!(f.view_mut(ws(1)).set_as_of(AsOf::At(chrono::Utc::now())));
        assert_eq!(f.view(ws(2)).versions(), pinned_before);
        assert_eq!(f.view(ws(2)).as_of(), &AsOf::Live);
    }

    #[test]
    fn generations_are_unique_across_lanes() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.pin(ws(2));
        f.shared_mut().set_scope(book_scope("BK000"));
        f.view_mut(ws(2)).set_scope(book_scope("BK001"));
        assert_ne!(f.shared().versions().scope, f.view(ws(2)).versions().scope);
        let seen = f.view(ws(2)).versions();
        assert!(f.unpin(ws(2)));
        assert!(
            !f.unpin(ws(2)),
            "unpinning an unpinned workspace is refused"
        );
        assert_ne!(
            f.view(ws(2)).versions().scope,
            seen.scope,
            "after unpin a tile must see a change when the content differs"
        );
    }

    #[test]
    fn unpinning_an_untouched_lane_keeps_the_generations() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("BK000"));
        f.pin(ws(3));
        let seen = f.view(ws(3)).versions();
        f.unpin(ws(3));
        assert!(
            f.view(ws(3)).versions().same_flip_identity(seen),
            "equal content keeps equal numbers, so nothing requeries"
        );
    }

    #[test]
    fn undo_is_per_lane() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("BK000"));
        f.pin(ws(2));
        f.view_mut(ws(2)).set_scope(book_scope("BK001"));
        assert!(f.view_mut(ws(2)).undo_scope());
        assert_eq!(f.view(ws(2)).scope(), &book_scope("BK000"));
        assert!(
            !f.view_mut(ws(2)).undo_scope(),
            "pinned history starts at the pin"
        );
        assert!(f.shared_mut().undo_scope());
        assert_eq!(f.shared().scope(), &Scope::default());
    }

    #[test]
    fn a_slot_reload_regroups_every_lane_and_clears_a_vanished_slot_in_a_hidden_lane() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.pin(ws(2));
        f.view_mut(ws(2)).set_active_slot(Some(2));
        let (shared_g, pinned_g) = (
            f.shared().versions().grouping,
            f.view(ws(2)).versions().grouping,
        );
        let mut only_one = GroupingSlots::default();
        only_one.set(1, vec!["book".into()]);
        assert!(f.replace_slots(only_one));
        assert_eq!(f.view(ws(2)).active_slot(), None, "slot 2 vanished");
        assert_ne!(f.view(ws(2)).versions().grouping, pinned_g);
        assert_ne!(f.shared().versions().grouping, shared_g);
    }

    #[test]
    fn saving_a_slot_regroups_only_lanes_on_that_slot() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.pin(ws(2));
        f.view_mut(ws(2)).set_active_slot(Some(1));
        f.shared_mut().set_active_slot(Some(2));
        let (shared_g, pinned_g) = (
            f.shared().versions().grouping,
            f.view(ws(2)).versions().grouping,
        );
        f.save_slot(1, vec!["lhu".into()]).unwrap();
        assert_ne!(f.view(ws(2)).versions().grouping, pinned_g);
        assert_eq!(f.shared().versions().grouping, shared_g);
    }

    #[test]
    fn the_generation_advances_on_pin_unpin_and_every_lane_edit() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let g0 = f.generation();
        f.pin(ws(2));
        let g1 = f.generation();
        assert!(g1 > g0);
        f.view_mut(ws(2)).set_text(Some("spx".into()));
        let g2 = f.generation();
        assert!(g2 > g1);
        f.unpin(ws(2));
        assert!(f.generation() > g2);
    }

    #[test]
    fn a_barrier_opened_on_one_lane_wants_nothing_from_another() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.pin(ws(2));
        f.view_mut(ws(2)).set_scope(book_scope("BK001"));
        f.view_mut(ws(2)).open_flip([QueryKey(1)], Instant::now());
        assert!(f.barrier_wants(QueryKey(1), f.view(ws(2)).versions()));
        assert!(!f.barrier_wants(QueryKey(1), f.shared().versions()));
    }

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
        let v0 = f.shared().versions();

        assert!(f.shared_mut().set_scope(book_scope("BK000")));
        let v1 = f.shared().versions();
        assert_ne!(v1.scope, v0.scope);
        assert_eq!(
            (v1.grouping, v1.as_of, v1.data, v1.config),
            (v0.grouping, v0.as_of, v0.data, v0.config)
        );

        assert!(f.shared_mut().set_active_slot(Some(2)));
        let v2 = f.shared().versions();
        assert_ne!(v2.grouping, v1.grouping);
        assert_eq!(v2.scope, v1.scope);

        assert!(
            f.shared_mut().set_as_of(AsOf::At(
                chrono::DateTime::parse_from_rfc3339("2026-09-03T14:05:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            ))
        );
        assert_ne!(f.shared().versions().as_of, v2.as_of);

        f.note_published(Publish {
            dataset: "risk".into(),
            batch: "EOD".into(),
            books: 1,
            at: chrono::Utc::now(),
        });
        assert_eq!(f.shared().versions().data, v2.data + 1);
        f.note_config_reloaded();
        assert_eq!(f.shared().versions().config, v2.config + 1);
    }

    #[test]
    fn an_unchanged_value_bumps_nothing() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let v0 = f.shared().versions();
        assert!(!f.shared_mut().set_scope(Scope::default()));
        assert!(!f.shared_mut().set_active_slot(None));
        assert!(!f.shared_mut().set_as_of(AsOf::Live));
        assert_eq!(f.shared().versions(), v0);
    }

    #[test]
    fn an_empty_slot_cannot_be_activated() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        assert!(!f.shared_mut().set_active_slot(Some(5)));
        assert_eq!(f.shared().active_slot(), None);
        assert!(f.shared_mut().set_active_slot(Some(1)));
        assert_eq!(
            f.shared().active_grouping(),
            Some(&["book".to_string(), "lhu".into()][..])
        );
        assert!(f.shared_mut().set_active_slot(None));
        assert_eq!(f.shared().active_grouping(), None);
    }

    #[test]
    fn effective_scope_composes_global_and_tile() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("BK000"));
        let tile = Scope {
            text: Some("spx".into()),
            ..Scope::default()
        };
        let eff = f.shared().effective_scope(&tile).unwrap();
        assert_eq!(eff.dimensions, book_scope("BK000").dimensions);
        assert_eq!(eff.text.as_deref(), Some("spx"));
        assert_eq!(
            f.shared().effective_scope(&Scope::default()),
            Ok(book_scope("BK000"))
        );
    }

    #[test]
    fn replacing_slots_bumps_config_and_grouping_and_drops_a_vanished_active_slot() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_active_slot(Some(2));
        let v = f.shared().versions();
        let mut fewer = GroupingSlots::default();
        fewer.set(1, vec!["book".into()]);
        assert!(f.replace_slots(fewer));
        assert_eq!(f.shared().active_slot(), None, "slot 2 no longer exists");
        assert_eq!(f.shared().versions().config, v.config + 1);
        assert_ne!(f.shared().versions().grouping, v.grouping);
    }

    #[test]
    fn saving_a_slot_updates_memory_and_bumps_grouping_only_when_active() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let v = f.shared().versions();
        assert!(f.save_slot(3, vec!["lhu".into()]).is_ok());
        assert_eq!(f.slots().label(3).as_deref(), Some("lhu"));
        assert_eq!(
            f.shared().versions().grouping,
            v.grouping,
            "not the active slot"
        );
        f.shared_mut().set_active_slot(Some(3));
        let v = f.shared().versions();
        assert!(f.save_slot(3, vec!["book".into()]).is_ok());
        assert_eq!(f.take_pending_persist(), Some((3, vec!["book".into()])));
        assert_ne!(
            f.shared().versions().grouping,
            v.grouping,
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
            assert!(f.shared_mut().set_scope(book_scope(&format!("BK{i:03}"))));
        }
        // 32 undos land on BK007 (40 sets, depth 32); a 33rd does nothing.
        for _ in 0..32 {
            assert!(f.shared_mut().undo_scope());
        }
        assert_eq!(
            f.shared().scope().dimensions[0].values,
            vec!["BK007".to_string()]
        );
        assert!(!f.shared_mut().undo_scope());
        assert!(f.shared_mut().redo_scope());
        assert_eq!(
            f.shared().scope().dimensions[0].values,
            vec!["BK008".to_string()]
        );
        // A new set clears redo.
        assert!(f.shared_mut().set_scope(book_scope("X")));
        assert!(!f.shared_mut().redo_scope());
    }

    #[test]
    fn a_no_op_set_pushes_nothing() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        assert!(f.shared_mut().set_scope(book_scope("A")));
        assert!(!f.shared_mut().set_scope(book_scope("A")));
        assert!(f.shared_mut().undo_scope());
        assert!(f.shared().scope().is_empty());
        assert!(!f.shared_mut().undo_scope());
    }

    #[test]
    fn a_text_session_that_ends_where_it_began_leaves_no_undo_entry() {
        // A session returning to its base removes its own undo entry and restores
        // the redo history captured before typing began.
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("A"));
        assert!(f.shared_mut().undo_scope());
        let redo_before = f.shared.scope_redo.clone();
        assert!(!redo_before.is_empty(), "fixture must seed a redo entry");

        let depth_before = f.shared.scope_undo.len();
        f.shared_mut().begin_scope_session();
        let mut s = f.shared().scope().clone();
        s.text = Some("a".into());
        assert!(f.shared_mut().set_scope_in_session(s));
        let mut s = f.shared().scope().clone();
        s.text = None;
        assert!(f.shared_mut().set_scope_in_session(s));
        f.shared_mut().end_scope_session();
        assert_eq!(
            f.shared.scope_undo.len(),
            depth_before,
            "the session's own push must be popped once it ends where it began"
        );
        assert!(
            !f.shared_mut().undo_scope(),
            "nothing to undo: the session never actually changed anything"
        );
        assert_eq!(
            f.shared.scope_redo, redo_before,
            "a no-op session must not destroy redo history from before it opened"
        );
    }

    #[test]
    fn a_mid_session_external_push_does_not_cause_a_second_session_push() {
        // An unrelated scope edit can move the undo stack during a text session.
        // The session still pushes its base only once.
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("A"));
        let base = f.shared().scope().clone();
        f.shared_mut().begin_scope_session();

        // The session's own first mutation: pushes `base` once.
        let mut s = f.shared().scope().clone();
        s.text = Some("a".into());
        assert!(f.shared_mut().set_scope_in_session(s));
        assert_eq!(f.shared.scope_undo.last(), Some(&base));
        let len_after_first_push = f.shared.scope_undo.len();

        // Mid-session external mutation (the mouse route) — pushes its
        // own outgoing scope; `base` is no longer the stack top.
        assert!(f.shared_mut().drop_dimension("book"));
        assert_ne!(f.shared.scope_undo.last(), Some(&base));

        // A second session mutation must not push `base` again just
        // because the stack top moved.
        let mut s = f.shared().scope().clone();
        s.text = Some("ab".into());
        assert!(f.shared_mut().set_scope_in_session(s));
        assert_eq!(
            f.shared.scope_undo.len(),
            len_after_first_push + 1,
            "the session must not re-push its own base after an external \
             mutation moved the stack top"
        );
        assert_eq!(
            f.shared.scope_undo.iter().filter(|s| *s == &base).count(),
            1,
            "the base scope must appear on the undo stack exactly once"
        );
    }

    #[test]
    fn a_text_session_coalesces_into_one_undo_entry() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("A"));
        f.shared_mut().begin_scope_session();
        for t in ["s", "sp", "spx"] {
            let mut s = f.shared().scope().clone();
            s.text = Some(t.into());
            assert!(f.shared_mut().set_scope_in_session(s));
        }
        f.shared_mut().end_scope_session();
        assert!(f.shared_mut().undo_scope());
        assert_eq!(
            f.shared().scope().text,
            None,
            "one undo returns to before the session"
        );
        assert_eq!(
            f.shared().scope().dimensions[0].values,
            vec!["A".to_string()]
        );
        assert!(f.shared_mut().redo_scope());
        assert_eq!(f.shared().scope().text.as_deref(), Some("spx"));
    }

    #[test]
    fn as_of_remembers_one_previous_value_in_both_directions() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let t = chrono::Utc::now();
        assert!(f.shared_mut().set_as_of(AsOf::At(t)));
        assert!(f.shared_mut().set_as_of(AsOf::Live));
        assert!(f.shared_mut().undo_as_of());
        assert_eq!(f.shared().as_of(), &AsOf::At(t));
        assert!(
            f.shared_mut().undo_as_of(),
            "undo swaps, so it can go back again"
        );
        assert_eq!(f.shared().as_of(), &AsOf::Live);
    }

    #[test]
    fn recent_publishes_keep_the_last_thirty_two_newest_first() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let v0 = f.shared().versions().data;
        for i in 0..40u32 {
            f.note_published(Publish {
                dataset: "risk".into(),
                batch: "EOD".into(),
                books: 3,
                at: chrono::Utc::now() + chrono::Duration::seconds(i as i64),
            });
        }
        assert_eq!(f.shared().versions().data, v0 + 40);
        assert_eq!(f.recent_publishes().len(), RECENT_PUBLISHES);
        assert!(f.recent_publishes()[0].at > f.recent_publishes()[1].at);
    }

    #[test]
    fn saved_scopes_load_save_and_persist_pending() {
        let mut saved = SavedScopes::new();
        saved.insert("eu".into(), book_scope("BK001"));
        let mut f = Frame::new(slots(), saved, None);
        assert!(f.shared_mut().load_scope("eu").unwrap());
        assert_eq!(f.shared().scope(), &book_scope("BK001"));
        assert!(f.shared_mut().load_scope("nope").is_err());
        f.shared_mut().set_scope(book_scope("BK002"));
        f.shared_mut().save_scope("mine").unwrap();
        assert_eq!(f.saved_scopes()["mine"], book_scope("BK002"));
        assert_eq!(
            f.take_pending_scope_persist(),
            Some(("mine".into(), book_scope("BK002")))
        );
        assert_eq!(f.take_pending_scope_persist(), None);
        assert!(f.shared_mut().save_scope("").is_err());
    }

    /// Reserved scope names must fail before mutating memory or pending writes,
    /// preventing collisions with built-in scope action identifiers.
    #[test]
    fn save_scope_refuses_the_reserved_save_current_name() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("BK001"));
        assert_eq!(
            f.shared_mut().save_scope("save_current"),
            Err("'save_current' is reserved".to_string())
        );
        assert!(!f.saved_scopes().contains_key("save_current"));
        assert_eq!(f.take_pending_scope_persist(), None);
    }

    #[test]
    fn save_scope_bumps_saved_scopes_not_config() {
        // Saving a named snapshot leaves the active query inputs unchanged.
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("A"));
        let before = f.shared().versions();
        f.shared_mut().save_scope("mine").unwrap();
        let after = f.shared().versions();
        assert_eq!(
            after.config, before.config,
            "save_scope must not bump config"
        );
        assert_eq!(after.saved_scopes, before.saved_scopes + 1);
    }

    #[test]
    fn drop_dimension_and_set_text_are_undoable_edits() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("A"));
        assert!(f.shared_mut().drop_dimension("book"));
        assert!(f.shared().scope().is_empty());
        assert!(!f.shared_mut().drop_dimension("book"));
        assert!(f.shared_mut().set_text(Some("spx".into())));
        assert!(!f.shared_mut().set_text(Some("spx".into())));
        assert!(f.shared_mut().undo_scope());
        assert!(f.shared().scope().is_empty());
        assert!(f.shared_mut().undo_scope());
        assert_eq!(f.shared().scope(), &book_scope("A"));
    }

    fn expr_scope(text: &str) -> Scope {
        Scope {
            expression: Some(geode_core::scope::parse_expr(text).unwrap()),
            ..Scope::default()
        }
    }

    fn term_texts(f: &Frame) -> Vec<String> {
        f.shared()
            .scope()
            .expression
            .as_ref()
            .map(|e| e.conjuncts().iter().map(|t| t.to_string()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn drop_expression_term_removes_only_that_term_and_is_undoable() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut()
            .set_scope(expr_scope("a = 1 and b = 2 and c = 3"));
        assert!(f.shared_mut().drop_expression_term(1));
        assert_eq!(term_texts(&f), vec!["a = 1", "c = 3"]);
        assert!(
            !f.shared_mut().drop_expression_term(2),
            "out of range changes nothing"
        );
        assert_eq!(term_texts(&f), vec!["a = 1", "c = 3"]);
        assert!(f.shared_mut().drop_expression_term(0));
        assert!(f.shared_mut().drop_expression_term(0), "the last term");
        assert_eq!(
            f.shared().scope().expression,
            None,
            "the last term leaves none"
        );
        assert!(
            !f.shared_mut().drop_expression_term(0),
            "no expression, no term"
        );
        assert!(f.shared_mut().undo_scope());
        assert_eq!(term_texts(&f), vec!["c = 3"]);
        assert!(f.shared_mut().undo_scope());
        assert!(f.shared_mut().undo_scope());
        assert_eq!(term_texts(&f), vec!["a = 1", "b = 2", "c = 3"]);
    }

    #[test]
    fn replace_expression_term_keeps_the_others_and_refuses_out_of_range() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut()
            .set_scope(expr_scope("a = 1 and b = 2 and c = 3"));
        let p = |t: &str| geode_core::scope::parse_expr(t).unwrap();
        let x = p("x = 9");
        assert_eq!(
            f.shared_mut()
                .replace_expression_term(1, &p("b = 2"), Some(x.clone())),
            Ok(true)
        );
        assert_eq!(term_texts(&f), vec!["a = 1", "x = 9", "c = 3"]);
        assert_eq!(
            f.shared_mut()
                .replace_expression_term(1, &x, Some(x.clone())),
            Ok(false),
            "the same term again is no edit"
        );
        assert_eq!(
            f.shared_mut()
                .replace_expression_term(1, &p("b = 2"), Some(p("y = 1"))),
            Err(TermGone),
            "index 1 now holds a different term: refuse rather than edit it"
        );
        assert_eq!(
            f.shared_mut().replace_expression_term(1, &p("b = 2"), None),
            Err(TermGone),
            "nor remove it"
        );
        assert_eq!(term_texts(&f), vec!["a = 1", "x = 9", "c = 3"]);
        assert_eq!(
            f.shared_mut()
                .replace_expression_term(3, &x, Some(x.clone())),
            Err(TermGone)
        );
        assert_eq!(
            f.shared_mut().replace_expression_term(2, &p("c = 3"), None),
            Ok(true)
        );
        assert_eq!(term_texts(&f), vec!["a = 1", "x = 9"]);
        assert!(f.shared_mut().undo_scope());
        assert!(f.shared_mut().undo_scope());
        assert_eq!(term_texts(&f), vec!["a = 1", "b = 2", "c = 3"]);
    }

    #[test]
    fn name_expression_term_swaps_the_term_for_the_name_in_one_undo_step() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut()
            .set_scope(expr_scope("a = 1 and b = 2 and c = 3"));
        let p = |t: &str| geode_core::scope::parse_expr(t).unwrap();
        assert!(f.shared().expression_term_is(1, &p("b = 2")));
        assert!(!f.shared().expression_term_is(1, &p("a = 1")));
        assert!(
            !f.shared().expression_term_is(3, &p("b = 2")),
            "out of range"
        );
        assert_eq!(
            f.shared_mut().name_expression_term(1, &p("a = 1"), "bee"),
            Err(TermGone),
            "index 1 holds a different term: refuse rather than name it"
        );
        assert_eq!(f.shared().scope().named, Vec::<String>::new());
        assert_eq!(
            f.shared_mut().name_expression_term(1, &p("b = 2"), "bee"),
            Ok(true)
        );
        assert_eq!(term_texts(&f), vec!["a = 1", "c = 3"]);
        assert_eq!(f.shared().scope().named, vec!["bee".to_string()]);
        assert!(f.shared_mut().undo_scope(), "one step");
        assert_eq!(term_texts(&f), vec!["a = 1", "b = 2", "c = 3"]);
        assert_eq!(f.shared().scope().named, Vec::<String>::new());

        // A scope may already list the name (a reference whose definition
        // is missing); naming a term after it lists it once.
        let mut s = f.shared().scope().clone();
        s.named = vec!["gone".to_string()];
        f.shared_mut().set_scope(s);
        assert_eq!(
            f.shared_mut().name_expression_term(0, &p("a = 1"), "gone"),
            Ok(true)
        );
        assert_eq!(f.shared().scope().named, vec!["gone".to_string()]);
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
        assert!(!f.shared_mut().clear_expression());
        let mut s = expr_scope("a = 1 and b = 2");
        s.text = Some("spx".into());
        f.shared_mut().set_scope(s);
        assert!(f.shared_mut().clear_expression());
        assert_eq!(f.shared().scope().expression, None);
        assert_eq!(
            f.shared().scope().text.as_deref(),
            Some("spx"),
            "other layers stay"
        );
        assert!(f.shared_mut().undo_scope());
        assert_eq!(term_texts(&f), vec!["a = 1", "b = 2"]);
    }

    #[test]
    fn the_bar_model_is_cached_on_versions_and_describes_the_scope() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_active_slot(Some(1));
        let mut s = book_scope("BK001");
        s.dimensions[0].values.push("BK002".into());
        s.dimensions.push(DimensionSelection {
            column: "lhu".into(),
            values: (0..7).map(|i| i.to_string()).collect(),
        });
        s.text = Some("spx".into());
        s.expression = Some(geode_core::scope::parse_expr("npv > 0").unwrap());
        f.shared_mut().set_scope(s);
        let clock = Clock::utc();
        let today = clock.today(chrono::Utc::now());
        let m1 = f.shared().bar_model(clock, today);
        let m2 = f.shared().bar_model(clock, today);
        assert!(Rc::ptr_eq(&m1, &m2));
        assert_eq!(m1.slot, Some((1, "book / lhu".into())));
        assert_eq!(m1.chips[0].summary, "book ∈ BK001, BK002");
        assert_eq!(m1.chips[1].summary, "lhu ∈ {7}");
        assert_eq!(m1.text.as_deref(), Some("spx"));
        assert_eq!(m1.terms.len(), 1);
        assert_eq!(m1.terms[0].label, "npv > 0");
        assert_eq!(m1.as_of, None);
        f.shared_mut().set_text(None);
        assert!(!Rc::ptr_eq(&m1, &f.shared().bar_model(clock, today)));

        // Changing the clock invalidates the cache with unchanged versions and date.
        let m3 = f.shared().bar_model(clock, today);
        let other_clock = Clock::utc().with_times(hm(7, 0), hm(17, 0));
        let m4 = f.shared().bar_model(other_clock, today);
        assert!(
            !Rc::ptr_eq(&m3, &m4),
            "a different clock must rebuild even with versions and today unchanged"
        );
    }

    #[test]
    fn the_bar_model_cache_rebuilds_when_today_changes_with_versions_unchanged() {
        // Midnight invalidates a cached today-only label without a frame mutation.
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("A"));
        let clock = Clock::utc();
        let day1 = clock.today(chrono::Utc::now());
        let m1 = f.shared().bar_model(clock, day1);
        let day2 = day1 + chrono::Duration::days(1);
        let m2 = f.shared().bar_model(clock, day2);
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
        f.shared_mut().set_scope(s);
        let clock = Clock::utc();
        assert_eq!(
            f.shared()
                .bar_model(clock, clock.today(chrono::Utc::now()))
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
        f.shared_mut().set_scope(book_scope("A"));
        let v = f.shared().versions();
        let t0 = Instant::now();
        f.shared_mut().open_flip([QueryKey(1), QueryKey(2)], t0);
        assert!(f.barrier_open());
        assert!(f.barrier_wants(QueryKey(1), v));
        assert!(!f.barrier_wants(QueryKey(3), v));
        let mut stale = v;
        stale.scope -= 1;
        assert!(!f.barrier_wants(QueryKey(1), stale));
        assert!(!f.arrived(QueryKey(1), v));
        assert_eq!(f.shared().versions().flip, v.flip);
        assert!(f.arrived(QueryKey(2), v));
        assert_eq!(f.shared().versions().flip, v.flip + 1);
        assert!(!f.barrier_open());
        assert!(!f.arrived(QueryKey(2), v), "nothing open");
    }

    #[test]
    fn the_deadline_releases_with_whatever_arrived() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let v = f.shared().versions();
        let t0 = Instant::now();
        f.shared_mut().open_flip([QueryKey(1), QueryKey(2)], t0);
        assert!(!f.sweep(t0 + Duration::from_millis(100)));
        assert!(f.sweep(t0 + FLIP_DEADLINE + Duration::from_millis(1)));
        assert_eq!(f.shared().versions().flip, v.flip + 1);
        assert!(!f.barrier_open());
    }

    #[test]
    fn a_new_mutation_while_open_replaces_the_barrier() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().open_flip([QueryKey(1)], Instant::now());
        let v_old = f.shared().versions();
        f.shared_mut().set_scope(book_scope("B"));
        let v_new = f.shared().versions();
        f.shared_mut()
            .open_flip([QueryKey(1), QueryKey(2)], Instant::now());
        assert!(!f.barrier_wants(QueryKey(1), v_old));
        assert!(f.barrier_wants(QueryKey(2), v_new));
    }

    #[test]
    fn data_and_config_bumps_do_not_open_a_barrier_and_do_not_match_one() {
        // Data and config do not change flip identity, so either may differ on
        // an arrival for this barrier. Opening barriers on frame notifications is
        // a separate shell responsibility.
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("A"));
        let v = f.shared().versions();
        f.shared_mut().open_flip([QueryKey(1)], Instant::now());
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
    fn a_barrier_holds_each_key_to_its_own_identity() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("b1"));
        let lane = f.shared().versions();
        // A second identity: same grouping and as-of, another scope number.
        let other = FrameVersions {
            scope: lane.scope + 100,
            ..lane
        };
        let (k1, k2) = (QueryKey(1), QueryKey(2));
        f.open_flip_each([(k1, lane), (k2, other)], Instant::now());

        assert!(f.barrier_wants(k1, lane));
        assert!(
            !f.barrier_wants(k1, other),
            "k1 was opened under the lane's identity"
        );
        assert!(f.barrier_wants(k2, other));
        assert!(!f.barrier_wants(k2, lane));

        assert!(
            !f.arrived(k2, lane),
            "an arrival under the wrong identity is not an arrival"
        );
        assert!(f.barrier_wants(k2, other), "and leaves the key awaited");
        assert!(!f.arrived(k1, lane), "one of two has arrived");
        assert!(f.arrived(k2, other), "the last arrival releases");
        assert!(!f.barrier_open());
    }

    /// A tile that starts following a group while a barrier awaits it
    /// answers under the group's identity from then on. Left enrolled under
    /// the lane's, its arrival does not count and the flip waits out the
    /// deadline.
    #[test]
    fn a_follow_under_an_open_barrier_arrives_once_reidentified() {
        let (tile, key) = (TileId(1), QueryKey(1));
        for reidentified in [false, true] {
            let mut f = Frame::new(slots(), SavedScopes::new(), None);
            f.shared_mut().set_scope(book_scope("b1"));
            let lane = f.view_for(ws(1), tile).versions();
            f.open_flip_each([(key, lane)], Instant::now());
            assert!(f.follow(tile, Some(Group::A)));
            let follower = f.view_for(ws(1), tile).versions();
            assert_ne!(follower.scope, lane.scope, "the tile's identity moved");
            if reidentified {
                f.reidentify(key, follower);
                assert!(!f.barrier_wants(key, lane));
            }
            assert_eq!(f.barrier_wants(key, follower), reidentified);
            assert_eq!(f.arrived(key, follower), reidentified);
            assert_eq!(f.barrier_open(), !reidentified);
        }
    }

    /// Re-identifying is not enrolling: a key no barrier awaits stays out,
    /// and with nothing open nothing opens.
    #[test]
    fn reidentify_touches_only_a_key_an_open_barrier_awaits() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let v = f.shared().versions();
        let other = FrameVersions {
            scope: v.scope + 100,
            ..v
        };
        f.reidentify(QueryKey(1), other);
        assert!(!f.barrier_open());

        f.open_flip_each([(QueryKey(1), v)], Instant::now());
        f.reidentify(QueryKey(2), other);
        assert!(!f.barrier_wants(QueryKey(2), other));
        assert!(f.barrier_wants(QueryKey(1), v));
        assert_eq!(f.shared().versions().flip, v.flip, "and releases nothing");
    }

    /// Extending joins the flip in progress. Replacing it instead would
    /// drop the tiles still in flight, which then apply on arrival while
    /// the staged ones wait: the tear the barrier exists to prevent.
    #[test]
    fn extending_a_barrier_keeps_its_keys_and_its_deadline() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let lane = f.shared().versions();
        let other = FrameVersions {
            scope: lane.scope + 100,
            ..lane
        };
        let (k1, k2, k3) = (QueryKey(1), QueryKey(2), QueryKey(3));
        let opened = Instant::now();
        f.open_flip_each([(k1, lane), (k2, lane)], opened);

        let later = opened + std::time::Duration::from_millis(100);
        f.extend_flip([(k2, other), (k3, other)], later);
        assert!(
            f.barrier_wants(k1, lane),
            "an earlier key stays, under its identity"
        );
        assert!(
            f.barrier_wants(k2, other),
            "a key listed again is re-identified"
        );
        assert!(!f.barrier_wants(k2, lane));
        assert!(f.barrier_wants(k3, other), "a new key joins");

        assert!(
            !f.sweep(later),
            "sanity: the deadline has not passed at the extension"
        );
        assert!(
            f.sweep(opened + FLIP_DEADLINE),
            "the deadline is still the one the barrier opened with"
        );
    }

    #[test]
    fn extending_with_nothing_open_opens_and_an_empty_extension_changes_nothing() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let v = f.shared().versions();
        f.extend_flip(std::iter::empty(), Instant::now());
        assert!(!f.barrier_open(), "nothing to await opens nothing");

        f.extend_flip([(QueryKey(1), v)], Instant::now());
        assert!(f.barrier_wants(QueryKey(1), v), "with none open it opens");

        f.extend_flip(std::iter::empty(), Instant::now());
        assert!(
            f.barrier_wants(QueryKey(1), v),
            "an empty extension never clears an open barrier"
        );
        assert_eq!(f.shared().versions().flip, v.flip);
    }

    #[test]
    fn an_empty_key_set_clears_the_barrier_without_a_flip() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let v = f.shared().versions();
        f.open_flip_each([(QueryKey(1), v)], Instant::now());
        let flip = f.shared().versions().flip;
        f.open_flip_each(std::iter::empty(), Instant::now());
        assert!(!f.barrier_open());
        assert_eq!(f.shared().versions().flip, flip);
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
        assert_eq!(f.shared().versions_for([&spx]).data, 1);
        assert_eq!(f.shared().versions_for([&spx_twin]).data, 1);
        assert_eq!(f.shared().versions_for([&ndx, &risk]).data, 0);
        // Updates outlive the recent-publish history, without storing interests
        // for thousands of unrelated datasets or document keys.
        for n in 0..1000 {
            f.note_published(publish(&format!("other-{n}"), "SPX"));
            f.note_published(publish("cvi", &format!("other-{n}")));
        }
        assert_eq!(f.shared().versions_for([&spx]).data, 1);
        assert_eq!(f.shared().versions_for([&ndx, &risk]).data, 0);
        assert_eq!(f.publication_watches.len(), 2);
        assert_eq!(f.publication_watches["cvi"].documents.len(), 2);
        assert_eq!(f.recent_publishes().len(), RECENT_PUBLISHES);
        f.note_published(publish("risk", "EOD"));
        assert_eq!(
            f.shared().versions_for([&spx, &risk]).data,
            f.shared().versions().data
        );
        f.shared_mut().set_scope(book_scope("BK000"));
        f.shared_mut().set_as_of(AsOf::At(chrono::Utc::now()));
        assert!(
            f.shared()
                .versions_for([&spx])
                .same_flip_identity(f.shared().versions())
        );
        drop((risk, spx, ndx));
        let _new = f.watch_publications("new", None);
        assert!(!f.publication_watches.contains_key("risk"));
        assert_eq!(f.publication_watches["cvi"].documents.len(), 1);
        f.note_published(publish("cvi", "SPX"));
        assert_eq!(
            f.shared().versions_for([&spx_twin]).data,
            f.shared().versions().data
        );
    }

    #[test]
    fn a_prefix_watch_fires_for_its_own_documents_only() {
        use geode_core::document::join_key;
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let key =
            |parts: &[&str]| join_key(&parts.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let publish = |dataset: &str, batch: &str| Publish {
            dataset: dataset.into(),
            batch: batch.into(),
            books: 0,
            at: chrono::Utc::now(),
        };
        let spx = f.watch_publications("option_chain", Some(&key(&["SPX"])));
        let spx_oct = f.watch_publications("option_chain", Some(&key(&["SPX", "2026-10-16"])));

        let before = f.shared().versions_for([&spx]).data;
        f.note_published(publish("option_chain", &key(&["SPX", "2026-11-20"])));
        assert!(
            f.shared().versions_for([&spx]).data > before,
            "a November publish is under SPX"
        );
        assert_eq!(
            f.shared().versions_for([&spx_oct]).data,
            before,
            "and not under SPX October"
        );
        assert!(spx.matches("option_chain", Some(&key(&["SPX", "2026-11-20"]))));
        assert!(!spx_oct.matches("option_chain", Some(&key(&["SPX", "2026-11-20"]))));

        let before = f.shared().versions_for([&spx]).data;
        f.note_published(publish("option_chain", &key(&["SPXW", "2026-10-16"])));
        assert_eq!(
            f.shared().versions_for([&spx]).data,
            before,
            "SPXW is a string extension, not a key under SPX"
        );
        assert!(!spx.matches("option_chain", Some(&key(&["SPXW", "2026-10-16"]))));

        f.note_published(publish("option_chain", &key(&["SPX", "2026-10-16"])));
        assert!(
            f.shared().versions_for([&spx_oct]).data > before
                && f.shared().versions_for([&spx]).data > before,
            "both fire"
        );
        assert!(spx.matches("option_chain", Some(&key(&["SPX", "2026-10-16"]))));
        assert!(spx_oct.matches("option_chain", Some(&key(&["SPX", "2026-10-16"]))));
    }

    #[test]
    fn is_for_is_exact_where_matches_is_a_prefix() {
        use geode_core::document::join_key;
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let spx = f.watch_publications("option_chain", Some("SPX"));
        let oct = join_key(&["SPX".to_string(), "2026-10-16".to_string()]);
        assert!(spx.is_for("option_chain", Some("SPX")));
        assert!(!spx.is_for("option_chain", Some(&oct)));
        assert!(spx.matches("option_chain", Some(&oct)));
        assert!(!spx.is_for("other", Some("SPX")));
        assert!(!spx.is_for("option_chain", None));
        let whole = f.watch_publications("option_chain", None);
        assert!(whole.is_for("option_chain", None));
        assert!(!whole.is_for("option_chain", Some("SPX")));
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
        f.shared_mut().set_scope(scope);
        assert_eq!(
            f.shared().effective_scope(&Scope::default()),
            Err("named expression 'gone' is missing".to_string())
        );

        assert!(f.replace_named_expressions(named("[gone]\nexpression = \"npv > 0\"\n")));
        let resolved = f.shared().effective_scope(&Scope::default()).unwrap();
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
        let v0 = f.shared().versions();
        let liq = named("[liq]\nexpression = \"npv > 0\"\n");
        assert!(f.replace_named_expressions(liq.clone()));
        let v1 = f.shared().versions();
        assert_eq!(v1.config, v0.config + 1);
        assert_eq!(f.named_expressions(), &liq);

        assert!(!f.replace_named_expressions(liq), "same content");
        assert_eq!(f.shared().versions(), v1);

        assert!(f.replace_named_expressions(named("[liq]\nexpression = \"npv > 5\"\n")));
        assert_eq!(f.shared().versions().config, v1.config + 1);
    }

    #[test]
    fn a_followers_scope_is_the_groups_and_everything_else_its_workspaces() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("b1"));
        f.shared_mut().set_active_slot(Some(1));
        let tile = TileId(7);
        assert!(f.follow(tile, Some(Group::A)));
        f.view_mut_for(ws(1), tile)
            .set_scope(Scope::one("underlying_ref", "SPX.Z"));

        let follower = f.view_for(ws(1), tile);
        let lane = f.view(ws(1));
        assert_eq!(follower.following(), Some(Group::A));
        assert_eq!(follower.scope().sole("underlying_ref"), Some("SPX.Z"));
        assert_eq!(
            lane.scope(),
            &book_scope("b1"),
            "the lane kept its own scope"
        );
        assert_eq!(follower.active_slot(), lane.active_slot());
        assert_eq!(follower.as_of(), lane.as_of());
        let (fv, lv) = (follower.versions(), lane.versions());
        assert_ne!(fv.scope, lv.scope);
        assert_eq!((fv.grouping, fv.as_of), (lv.grouping, lv.as_of));
        // Another tile in the same workspace follows nothing.
        assert_eq!(f.view_for(ws(1), TileId(8)).scope(), &book_scope("b1"));
        assert_eq!(f.view_for(ws(1), TileId(8)).following(), None);
    }

    #[test]
    fn a_followers_effective_scope_composes_the_group_with_the_tiles_own() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("b1"));
        let tile = TileId(7);
        f.follow(tile, Some(Group::B));
        f.view_mut_for(ws(1), tile)
            .set_scope(Scope::one("underlying_ref", "NDX"));
        let eff = f
            .view_for(ws(1), tile)
            .effective_scope(&book_scope("b2"))
            .unwrap();
        assert_eq!(eff.sole("underlying_ref"), Some("NDX"));
        assert_eq!(
            eff.sole("book"),
            Some("b2"),
            "the lane's b1 is not composed in"
        );
    }

    #[test]
    fn group_scope_generations_are_unique_across_groups_and_lanes() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.pin(ws(2));
        let (a, b) = (TileId(1), TileId(2));
        f.follow(a, Some(Group::A));
        f.follow(b, Some(Group::B));
        f.shared_mut().set_scope(book_scope("b1"));
        f.view_mut(ws(2)).set_scope(book_scope("b2"));
        f.view_mut_for(ws(1), a)
            .set_scope(Scope::one("underlying_ref", "SPX.Z"));
        f.view_mut_for(ws(1), b)
            .set_scope(Scope::one("underlying_ref", "NDX"));
        let mut gens = vec![
            f.shared().versions().scope,
            f.view(ws(2)).versions().scope,
            f.view_for(ws(1), a).versions().scope,
            f.view_for(ws(1), b).versions().scope,
        ];
        gens.sort();
        gens.dedup();
        assert_eq!(gens.len(), 4, "one number names one scope anywhere");
        assert_eq!(
            f.group_scope_gens()[Group::A.index()],
            f.view_for(ws(1), a).versions().scope
        );
    }

    #[test]
    fn follow_and_emit_advance_the_generation_only_when_they_change_something() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let tile = TileId(7);
        let g0 = f.generation();
        assert!(f.follow(tile, Some(Group::A)));
        assert!(f.generation() > g0);
        let g1 = f.generation();
        assert!(!f.follow(tile, Some(Group::A)), "already following A");
        assert_eq!(f.generation(), g1);
        assert!(f.emit(tile, Some(Group::B)));
        assert!(f.generation() > g1, "an emit that changed something");
        let g2 = f.generation();
        assert!(!f.emit(tile, Some(Group::B)), "already emitting into B");
        assert_eq!(f.generation(), g2);
        assert_eq!(
            f.membership(tile),
            geode_core::link::Membership {
                follow: Some(Group::A),
                emit: Some(Group::B)
            }
        );
        assert!(f.follow(tile, None));
        assert!(f.emit(tile, None));
        assert!(f.membership(tile).is_empty());
    }

    #[test]
    fn a_follow_changes_the_tiles_scope_generation_and_nothing_else() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("b1"));
        let tile = TileId(7);
        let before = f.view_for(ws(1), tile).versions();
        f.follow(tile, Some(Group::C));
        let after = f.view_for(ws(1), tile).versions();
        assert_ne!(
            after.scope, before.scope,
            "the tile now reads the group's (empty) scope"
        );
        assert_eq!(
            (after.grouping, after.as_of, after.data, after.config),
            (before.grouping, before.as_of, before.data, before.config)
        );
    }

    #[test]
    fn an_equal_group_scope_is_not_a_write_and_clear_empties_it() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let tile = TileId(7);
        f.follow(tile, Some(Group::A));
        assert!(
            f.view_mut_for(ws(1), tile)
                .set_scope(Scope::one("underlying_ref", "SPX.Z"))
        );
        assert_eq!(
            f.group_scope(Group::A).sole("underlying_ref"),
            Some("SPX.Z"),
            "the write landed in the group"
        );
        assert!(f.shared().scope().is_empty(), "and not in the lane");
        let generation = f.view_for(ws(1), tile).versions().scope;
        assert!(
            !f.view_mut_for(ws(1), tile)
                .set_scope(Scope::one("underlying_ref", "SPX.Z"))
        );
        assert_eq!(f.view_for(ws(1), tile).versions().scope, generation);
        assert!(f.view_mut_for(ws(1), tile).clear_scope());
        assert!(f.group_scope(Group::A).is_empty());
        assert_eq!(
            f.shared().scope(),
            &Scope::default(),
            "the lane was never written"
        );
    }

    /// Set and clear are the only scope verbs a follower's view sends to
    /// its group. Each of the scope bar's other verbs names the workspace
    /// lane: landing in the group instead would requery every follower for
    /// a chip the bar removed, and leave the bar showing the old lane.
    #[test]
    fn the_scope_bars_other_verbs_edit_the_lane_through_a_followers_view() {
        let mut saved = SavedScopes::new();
        saved.insert("desk".to_string(), book_scope("b9"));
        let mut f = Frame::new(slots(), saved, None);
        let mut lane = expr_scope("a = 1 and b = 2");
        lane.dimensions = book_scope("b1").dimensions;
        lane.named = vec!["liq".into()];
        f.shared_mut().set_scope(lane);
        let tile = TileId(7);
        f.follow(tile, Some(Group::A));
        let group = Scope::one("underlying_ref", "SPX.Z");
        f.view_mut_for(ws(1), tile).set_scope(group.clone());
        let group_gen = f.group_scope_gens()[Group::A.index()];

        let mut v = f.view_mut_for(ws(1), tile);
        assert!(v.set_text(Some("x".into())));
        assert!(v.drop_dimension("book"));
        assert!(v.drop_named("liq"));
        assert!(v.drop_expression_term(0));
        assert!(v.clear_expression());
        v.save_scope("mine").unwrap();
        assert_eq!(v.load_scope("desk"), Ok(true));

        assert_eq!(f.group_scope(Group::A), &group, "no verb reached the group");
        assert_eq!(f.group_scope_gens()[Group::A.index()], group_gen);
        assert_eq!(f.shared().scope(), &book_scope("b9"), "the lane loaded");
        let text_only = Scope {
            text: Some("x".into()),
            ..Scope::default()
        };
        assert_eq!(
            f.saved_scopes().get("mine"),
            Some(&text_only),
            "the lane's scope was saved, each earlier verb having edited it"
        );
        assert!(f.view_mut_for(ws(1), tile).undo_scope());
        assert_eq!(f.shared().scope(), &text_only, "undo walks the lane");
        assert_eq!(f.group_scope(Group::A), &group);
    }

    fn doc(key: &str) -> Arc<DocumentRows> {
        Arc::new(DocumentRows {
            key: vec![key.to_string()],
            attributes: Vec::new(),
            axes: Vec::new(),
            values: Vec::new(),
        })
    }

    fn draft(u: &str, rows: &Arc<DocumentRows>) -> Emission {
        Emission {
            scope: Some(Scope::one("underlying_ref", u)),
            board: vec![BoardEntry {
                dataset: "cvi_params".into(),
                key: vec![u.to_string()],
                rows: Arc::clone(rows),
            }],
        }
    }

    fn on_board(f: &Frame, g: Group, u: &str) -> Option<Arc<DocumentRows>> {
        f.board_entry(g, "cvi_params", &[u.to_string()])
    }

    #[test]
    fn an_emission_sets_the_groups_scope_and_posts_its_board() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let panel = TileId(1);
        let rows = doc("SPX.Z");
        assert!(
            !f.post_emission(panel, draft("SPX.Z", &rows)),
            "not emitting: nothing is written"
        );
        f.emit(panel, Some(Group::A));
        assert!(f.post_emission(panel, draft("SPX.Z", &rows)));
        assert_eq!(
            f.group_scope(Group::A).sole("underlying_ref"),
            Some("SPX.Z")
        );
        assert!(Arc::ptr_eq(
            &on_board(&f, Group::A, "SPX.Z").unwrap(),
            &rows
        ));
        assert!(
            on_board(&f, Group::B, "SPX.Z").is_none(),
            "another group's board is its own"
        );
    }

    #[test]
    fn an_unchanged_emission_writes_nothing() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let panel = TileId(1);
        f.emit(panel, Some(Group::A));
        let rows = doc("SPX.Z");
        f.post_emission(panel, draft("SPX.Z", &rows));
        let (generation, board, data) = (f.generation(), f.board_gen(Group::A), f.data_version());
        assert!(
            !f.post_emission(panel, draft("SPX.Z", &rows)),
            "the same draft allocation again"
        );
        assert_eq!(
            (f.generation(), f.board_gen(Group::A), f.data_version()),
            (generation, board, data)
        );
        // A new allocation is a new draft: the board moves, the scope and the
        // global data counter do not.
        assert!(f.post_emission(panel, draft("SPX.Z", &doc("SPX.Z"))));
        assert_eq!(
            f.generation(),
            generation,
            "the scope was equal, so no generation was drawn"
        );
        assert_eq!(f.board_gen(Group::A), board + 1);
        assert_eq!(
            f.data_version(),
            data,
            "a draft never bumps the publish counter"
        );
    }

    #[test]
    fn a_scope_none_emission_leaves_the_groups_scope() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let blotter = TileId(1);
        f.emit(blotter, Some(Group::A));
        f.post_emission(
            blotter,
            Emission {
                scope: Some(Scope::one("underlying_ref", "SPX.Z")),
                board: vec![],
            },
        );
        assert!(
            !f.post_emission(blotter, Emission::default()),
            "the cursor names no single value"
        );
        assert_eq!(
            f.group_scope(Group::A).sole("underlying_ref"),
            Some("SPX.Z")
        );
    }

    #[test]
    fn a_board_entry_leaves_when_its_emitter_stops_listing_it_leaves_or_closes() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let panel = TileId(1);
        let rows = doc("SPX.Z");
        let scope_only = || Emission {
            scope: Some(Scope::one("underlying_ref", "SPX.Z")),
            board: vec![],
        };

        f.emit(panel, Some(Group::A));
        f.post_emission(panel, draft("SPX.Z", &rows));
        assert!(
            f.post_emission(panel, scope_only()),
            "the draft was discarded or published"
        );
        assert!(on_board(&f, Group::A, "SPX.Z").is_none());

        f.post_emission(panel, draft("SPX.Z", &rows));
        assert!(f.emit(panel, None), "leaving the group");
        assert!(on_board(&f, Group::A, "SPX.Z").is_none());
        assert_eq!(
            f.group_scope(Group::A).sole("underlying_ref"),
            Some("SPX.Z"),
            "the scope stays as last written"
        );

        f.emit(panel, Some(Group::A));
        assert!(
            f.post_emission(panel, draft("SPX.Z", &rows)),
            "what it posted before it left is new to the group again"
        );
        assert!(Arc::ptr_eq(
            &on_board(&f, Group::A, "SPX.Z").unwrap(),
            &rows
        ));
        assert!(f.forget_tile(panel), "closing");
        assert!(on_board(&f, Group::A, "SPX.Z").is_none());
        assert!(f.membership(panel).is_empty());
    }

    #[test]
    fn switching_group_moves_nothing_until_the_next_post() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let panel = TileId(1);
        let rows = doc("SPX.Z");
        f.emit(panel, Some(Group::A));
        f.post_emission(panel, draft("SPX.Z", &rows));
        f.emit(panel, Some(Group::B));
        assert!(on_board(&f, Group::A, "SPX.Z").is_none(), "it left A");
        assert!(
            on_board(&f, Group::B, "SPX.Z").is_none(),
            "and has not posted into B yet"
        );
        assert!(
            f.post_emission(panel, draft("SPX.Z", &rows)),
            "the same emission is new to B"
        );
        assert!(on_board(&f, Group::B, "SPX.Z").is_some());
    }

    #[test]
    fn the_latest_post_wins_a_key_and_a_leaving_emitter_uncovers_the_other() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let (p1, p2) = (TileId(1), TileId(2));
        let (r1, r2) = (doc("SPX.Z"), doc("SPX.Z"));
        f.emit(p1, Some(Group::A));
        f.emit(p2, Some(Group::A));
        f.post_emission(p1, draft("SPX.Z", &r1));
        f.post_emission(p2, draft("SPX.Z", &r2));
        assert!(
            Arc::ptr_eq(&on_board(&f, Group::A, "SPX.Z").unwrap(), &r2),
            "the later post"
        );
        f.emit(p2, None);
        assert!(
            Arc::ptr_eq(&on_board(&f, Group::A, "SPX.Z").unwrap(), &r1),
            "the first emitter still lists it"
        );
    }

    #[test]
    fn a_board_watch_bumps_for_its_key_only_and_on_removal_too() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let panel = TileId(1);
        f.emit(panel, Some(Group::A));
        let spx = f.watch_board(Group::A, "cvi_params", Some("SPX.Z"));
        let ndx = f.watch_board(Group::A, "cvi_params", Some("NDX"));
        let any = f.watch_board(Group::A, "cvi_params", None);
        let other_group = f.watch_board(Group::B, "cvi_params", Some("SPX.Z"));
        let (s0, n0, a0, o0) = (
            spx.revision(),
            ndx.revision(),
            any.revision(),
            other_group.revision(),
        );

        f.post_emission(panel, draft("SPX.Z", &doc("SPX.Z")));
        assert!(spx.revision() > s0);
        assert!(any.revision() > a0, "a dataset-wide watch hears every key");
        assert_eq!(ndx.revision(), n0);
        assert_eq!(other_group.revision(), o0);

        let s1 = spx.revision();
        f.emit(panel, None);
        assert!(spx.revision() > s1, "a removal is a change too");
        assert!(spx.is_for(Group::A, "cvi_params", Some("SPX.Z")));
        assert!(!spx.is_for(Group::A, "cvi_params", None));
    }

    /// Forgetting a tile advances the generation exactly when the tile was
    /// in a group: it is the session writer's dirty signal, so a membership
    /// that left without it would stay in the session file, and a tile in
    /// no group closing must not make the session dirty.
    #[test]
    fn forgetting_a_tile_advances_the_generation_only_when_it_was_linked() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let (emitter, follower, unlinked) = (TileId(1), TileId(2), TileId(3));
        f.emit(emitter, Some(Group::A));
        f.follow(follower, Some(Group::B));
        let before = f.generation();
        assert!(f.forget_tile(follower), "a tile that only followed");
        assert!(f.membership(follower).is_empty());
        assert_eq!(f.membership(emitter).emit, Some(Group::A));
        assert!(f.generation() > before);
        let settled = f.generation();
        assert!(!f.forget_tile(follower), "already gone");
        assert!(!f.forget_tile(unlinked));
        assert_eq!(f.generation(), settled, "nothing dropped, nothing to save");
        assert!(f.forget_tile(emitter));
        assert!(f.generation() > settled);
    }

    /// The shell pulls an emitter again on every notify. A repeat of its
    /// last answer must not count as a newer post: it would retake a key
    /// another emitter posted since, and two panels drafting the same
    /// document would swap the board on every repaint.
    #[test]
    fn a_repeated_emission_does_not_retake_a_key_from_a_later_post() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let (p1, p2) = (TileId(1), TileId(2));
        let (r1, r2) = (doc("SPX.Z"), doc("SPX.Z"));
        f.emit(p1, Some(Group::A));
        f.emit(p2, Some(Group::A));
        f.post_emission(p1, draft("SPX.Z", &r1));
        f.post_emission(p2, draft("SPX.Z", &r2));
        let board = f.board_gen(Group::A);
        assert!(!f.post_emission(p1, draft("SPX.Z", &r1)));
        assert!(Arc::ptr_eq(&on_board(&f, Group::A, "SPX.Z").unwrap(), &r2));
        assert_eq!(f.board_gen(Group::A), board);
    }

    /// A tile that follows and emits into the same group answers with the
    /// scope it was just handed. That must not be a write, or the shell's
    /// notify would pull the tile again and the pair would never settle.
    #[test]
    fn a_tile_emitting_the_scope_it_follows_writes_nothing() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let tile = TileId(1);
        let scoped = |u: &str| Emission {
            scope: Some(Scope::one("underlying_ref", u)),
            board: vec![],
        };
        f.follow(tile, Some(Group::A));
        f.emit(tile, Some(Group::A));
        f.view_mut_for(ws(1), tile)
            .set_scope(Scope::one("underlying_ref", "SPX.Z"));
        let generation = f.generation();
        assert!(
            !f.post_emission(tile, scoped("SPX.Z")),
            "the group's own scope coming back"
        );
        assert_eq!(f.generation(), generation);
        assert!(f.post_emission(tile, scoped("NDX")));
        assert_eq!(f.generation(), generation + 1, "the group moved once");
        assert_eq!(
            f.view_for(ws(1), tile).scope().sole("underlying_ref"),
            Some("NDX")
        );
        assert!(!f.post_emission(tile, scoped("NDX")));
        assert_eq!(f.generation(), generation + 1);
    }

    /// A repeat of an emitter's last answer is skipped whole, scope
    /// included. Applied again it would pull the group back to a scope
    /// another emitter has since replaced, on every notify of the first.
    #[test]
    fn a_repeated_emission_does_not_restore_a_scope_another_writer_moved() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let (p1, p2) = (TileId(1), TileId(2));
        let scoped = |u: &str| Emission {
            scope: Some(Scope::one("underlying_ref", u)),
            board: vec![],
        };
        f.emit(p1, Some(Group::A));
        f.emit(p2, Some(Group::A));
        assert!(f.post_emission(p1, scoped("SPX.Z")));
        assert!(f.post_emission(p2, scoped("NDX")));
        let generation = f.generation();
        assert!(!f.post_emission(p1, scoped("SPX.Z")));
        assert_eq!(f.group_scope(Group::A).sole("underlying_ref"), Some("NDX"));
        assert_eq!(f.generation(), generation);
    }

    /// A follower reads the same scope and versions through its writable
    /// view as through its reading one: a tile that answers the barrier
    /// inside an update must not answer under its lane's identity.
    #[test]
    fn a_followers_writable_view_reads_what_its_reading_view_does() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("b1"));
        let tile = TileId(7);
        f.follow(tile, Some(Group::A));
        f.view_mut_for(ws(1), tile)
            .set_scope(Scope::one("underlying_ref", "SPX.Z"));
        let reading = f.view_for(ws(1), tile);
        let (scope, versions) = (reading.scope().clone(), reading.versions());
        assert_eq!(scope.sole("underlying_ref"), Some("SPX.Z"));

        let writable = f.view_mut_for(ws(1), tile);
        assert_eq!(writable.scope(), &scope);
        assert_eq!(writable.versions(), versions);
        assert_eq!(writable.view().following(), Some(Group::A));
    }

    /// The scope bar's model is built from the lane even when the view it
    /// is asked of follows a group: the bar shows the workspace.
    #[test]
    fn a_followers_lane_view_reads_the_lane() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("b1"));
        let tile = TileId(7);
        f.follow(tile, Some(Group::A));
        f.view_mut_for(ws(1), tile)
            .set_scope(Scope::one("underlying_ref", "SPX.Z"));
        let follower = f.view_for(ws(1), tile);
        assert_eq!(follower.following(), Some(Group::A));
        let lane = follower.lane_view();
        assert_eq!(lane.following(), None);
        assert_eq!(lane.scope(), &book_scope("b1"));
        assert_eq!(lane.versions(), f.view(ws(1)).versions());
    }

    /// A module can ask for the bar's model through its own handle, whose
    /// view follows a group. It gets the workspace's model, the one the
    /// shell's bar paints: built from the group it would describe a scope
    /// the bar's controls do not edit.
    #[test]
    fn the_bar_model_asked_of_a_followers_view_describes_the_lane() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("b1"));
        let tile = TileId(7);
        f.follow(tile, Some(Group::A));
        f.view_mut_for(ws(1), tile)
            .set_scope(Scope::one("underlying_ref", "SPX.Z"));
        let clock = Clock::utc();
        let today = clock.today(chrono::Utc::now());

        let model = f.view_for(ws(1), tile).bar_model(clock, today);
        assert_eq!(model.chips.len(), 1);
        assert_eq!(model.chips[0].summary, "book \u{2208} b1");
        assert!(
            Rc::ptr_eq(&model, &f.view(ws(1)).bar_model(clock, today)),
            "one cached model serves the workspace's view and a follower's"
        );
        assert!(Rc::ptr_eq(
            &model,
            &f.view_mut_for(ws(1), tile).bar_model(clock, today)
        ));
    }
}
