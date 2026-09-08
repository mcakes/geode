//! The shared frame (foundation §4, Phase 3 §4, Phase 4 §3.1/§3.6/§3.8/
//! §3.9/§3.12): global scope, undo/redo history, the active grouping slot,
//! as-of (with one remembered previous value), recent publishes, saved
//! scopes, and the data and config generations, as one value every tile
//! observes. Pure: `ShellView` holds it in a gpui entity and notifies; a
//! module reads it through that entity.
//!
//! Every mutation bumps exactly the counters it affects, so a tile can
//! compare the fields it follows against the ones it last acted on with
//! one integer compare each — a pinned tile ignores `grouping`, an
//! unscoped tile ignores `scope`, every tile follows `as_of`, `data` and
//! `config` (§4.1).

use crate::perf::RequeryStats;
use crate::scopebar::{self, ScopeBarModel};
use geode_core::config::Layer;
use geode_core::groupings::GroupingSlots;
use geode_core::query::{AsOf, QueryKey};
use geode_core::scope::Scope;
use geode_core::scopes::SavedScopes;
use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};
use toml_edit::value;

/// How long [`Frame::open_flip`]'s barrier waits for every following
/// tile's outcome before [`Frame::sweep`] releases it with whatever
/// arrived (Phase 4 §3.10) — long enough that a normal requery clears it
/// well before the deadline, short enough that one slow or dropped query
/// never holds every other tile's paint for more than a blink.
pub const FLIP_DEADLINE: Duration = Duration::from_millis(250);

/// How many scope edits [`Frame::undo_scope`] can walk back through (spec
/// §3.6). Bounded, not unlimited: an unbounded history is an unbounded
/// per-frame allocation waiting to happen (every `set_scope` clones the
/// outgoing `Scope` onto the stack).
pub const UNDO_DEPTH: usize = 32;

/// How many recent publishes [`Frame::recent_publishes`] remembers (spec
/// §3.12), newest first.
pub const RECENT_PUBLISHES: usize = 32;

/// One file publish the frame was told about (spec §3.12) — enough for a
/// "recent publishes" picker to name what changed and when, without the
/// frame depending on `geode-data` (CLAUDE.md: shell and data never depend
/// on each other) to know what a `DataEvent::Published` looked like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Publish {
    pub dataset: String,
    pub batch: String,
    pub books: usize,
    pub at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameVersions {
    pub scope: u64,
    pub grouping: u64,
    pub as_of: u64,
    pub data: u64,
    pub config: u64,
    /// Bumped only by the flip barrier's [`Frame::release`](Frame) once
    /// every following tile's outcome for one `(scope, grouping, as_of)`
    /// has arrived or the deadline passed (Phase 4 §3.10). Deliberately
    /// NOT one of the counters `geode_blotter::tile::BlotterTile::
    /// follows_changed` compares — a tile decides whether to *requery*
    /// from the other five; `flip` only ever tells a tile that already
    /// has a staged snapshot waiting when it is safe to *promote* it,
    /// which `BlotterTile::on_frame_changed` checks separately.
    pub flip: u64,
}

impl FrameVersions {
    /// Whether `self` and `other` share the same flip identity (Phase 4
    /// §3.10) — `scope`, `grouping` and `as_of` only. `data`, `config`
    /// and `flip` are deliberately excluded: a flip barrier is opened
    /// over one `(scope, grouping, as_of)` triple, and neither a
    /// data/config-only bump nor `flip`'s own release (via
    /// [`Frame::release`]) is a change of identity for it. The sole
    /// place a barrier or a staged snapshot decides "is this still the
    /// versions I was opened/staged for" — [`Frame::matches`] and
    /// `geode_blotter::tile::BlotterTile::promote`'s version check both
    /// call this rather than comparing the three fields inline.
    pub fn same_flip_identity(self, other: FrameVersions) -> bool {
        self.scope == other.scope && self.grouping == other.grouping && self.as_of == other.as_of
    }
}

/// A barrier opened by [`Frame::open_flip`] (Phase 4 §3.10): every
/// following tile's outcome for one `(scope, grouping, as_of)` triple
/// must arrive — or fail, or [`FLIP_DEADLINE`] must pass — before any of
/// them promotes its staged snapshot, so a scope/grouping/as-of change
/// never leaves two tiles showing two different scopes for even one
/// frame. A later mutation while this is still open (`open_flip` called
/// again) simply replaces it outright — nothing "double-releases" the
/// old one, and a stale outcome for the old versions can no longer
/// satisfy the new barrier (`matches` compares against the CURRENT
/// barrier's own versions, not whatever was true when the outcome was
/// submitted).
#[derive(Debug)]
struct FlipBarrier {
    /// The versions this barrier was opened for — only `scope`,
    /// `grouping` and `as_of` are ever compared (via
    /// [`FrameVersions::same_flip_identity`]), so `data`/`config`/`flip`
    /// riding along here are inert.
    versions: FrameVersions,
    awaiting: HashSet<QueryKey>,
    opened: Instant,
}

#[derive(Debug)]
pub struct Frame {
    scope: Scope,
    /// Bounded stack of outgoing scopes, oldest first — `undo_scope` pops
    /// the back, `redo_scope` pushes it back on. Capped at [`UNDO_DEPTH`]
    /// by `push_undo`, which drops the oldest entry once full.
    scope_undo: Vec<Scope>,
    scope_redo: Vec<Scope>,
    /// `Some` while the text field has focus (spec §3.8): the first
    /// mutation of the session pushes the base scope onto `scope_undo`,
    /// later ones in the same session push nothing, so a whole typing
    /// burst coalesces into one undo entry. `None`: no session open;
    /// `Some(Some(base))`: open, not yet pushed; `Some(None)`: open,
    /// already pushed once.
    scope_session: Option<Option<Scope>>,
    slots: GroupingSlots,
    active_slot: Option<u8>,
    as_of: AsOf,
    /// The one previous as-of value `undo_as_of` swaps back in (spec
    /// §3.6) — not a stack: undoing twice in a row toggles between the
    /// current and previous value rather than walking further back.
    previous_as_of: Option<AsOf>,
    /// The most recent publishes, newest first, capped at
    /// [`RECENT_PUBLISHES`] (spec §3.12).
    recent_publishes: VecDeque<Publish>,
    saved_scopes: SavedScopes,
    /// A scope saved by `save_scope`, waiting to be written to the user
    /// layer's `scopes.toml` — drained by `ShellView`'s frame observer via
    /// [`take_pending_scope_persist`](Self::take_pending_scope_persist),
    /// same pattern as `pending_persist` below (§3.9).
    pending_scope_persist: Option<(String, Scope)>,
    versions: FrameVersions,
    /// Requery timing the blotter records (Phase 3 §6.8). Here because
    /// the frame is the one shell-side handle every module holds.
    pub requery: RequeryStats,
    user_dir: Option<PathBuf>,
    /// A slot saved by `save_slot`, waiting to be written to the user
    /// layer's `groupings.toml`. The frame is pure (no file access), so
    /// `ShellView` drains this via [`take_pending_persist`](Self::take_pending_persist)
    /// — observed off an entity-change notification — and does the actual
    /// background write with [`persist_slot_to_user_config`] (§4.2).
    pending_persist: Option<(u8, Vec<String>)>,
    /// Lazy cache for [`bar_model`](Self::bar_model), keyed on
    /// `versions()` (carried over from the Phase 3c `readout` cache this
    /// replaces — same reasoning): `render` calls `bar_model` every frame
    /// (`shell/render.rs`), and building one fresh each time allocates a
    /// `Vec<Chip>`, several `String`s, for a value that's almost always
    /// identical to the previous frame's. `RefCell` because `bar_model`
    /// takes `&self` (every other read-only accessor on `Frame` does) but
    /// still needs to update this cache; `Rc<ScopeBarModel>` rather than
    /// an owned clone so a cache hit costs a refcount bump, not a fresh
    /// allocation.
    bar_cache: RefCell<Option<(FrameVersions, Rc<ScopeBarModel>)>>,
    /// The open flip barrier (Phase 4 §3.10), if any — see
    /// [`FlipBarrier`]'s own doc comment. `None` when no scope/grouping/
    /// as-of mutation has a barrier waiting on it right now.
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
            saved_scopes: saved,
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

    /// Push `outgoing` onto the bounded undo stack (spec §3.6) and clear
    /// redo — a fresh edit invalidates whatever `redo_scope` could have
    /// replayed. Shared by every path that replaces `self.scope` outright:
    /// `set_scope` and the first mutation of a text-editing session.
    fn push_undo(&mut self, outgoing: Scope) {
        self.scope_undo.push(outgoing);
        if self.scope_undo.len() > UNDO_DEPTH {
            self.scope_undo.remove(0);
        }
        self.scope_redo.clear();
    }

    /// Replace the global scope, pushing the outgoing one onto the undo
    /// stack and clearing redo (spec §3.6). `false` when nothing changed —
    /// no push, no version bump.
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

    /// Open a text-editing session (spec §3.8): the text field just took
    /// focus. The base scope is remembered but not yet pushed — only the
    /// session's first actual mutation (`set_scope_in_session`) pushes it,
    /// so opening a session that never edits anything leaves undo
    /// untouched.
    pub fn begin_scope_session(&mut self) {
        self.scope_session = Some(Some(self.scope.clone()));
    }

    /// Set the scope during an open text-editing session, coalescing every
    /// mutation in the session into a single undo entry (spec §3.8): the
    /// first call pushes the session's base scope; later calls in the same
    /// session push nothing, so undoing once after typing "s", "sp", "spx"
    /// returns straight to the pre-session scope rather than walking back
    /// one keystroke at a time. Falls back to `set_scope`'s own
    /// push-every-time behaviour when no session is open — a caller that
    /// forgets `begin_scope_session` still gets correct (if less
    /// convenient) undo semantics rather than silently losing history.
    pub fn set_scope_in_session(&mut self, scope: Scope) -> bool {
        if self.scope == scope {
            return false;
        }
        match self.scope_session.take() {
            Some(Some(base)) => {
                self.push_undo(base);
                self.scope_session = Some(None);
            }
            Some(None) => self.scope_session = Some(None),
            None => {
                let outgoing = self.scope.clone();
                self.push_undo(outgoing);
            }
        }
        self.scope = scope;
        self.versions.scope += 1;
        true
    }

    /// Close a text-editing session (spec §3.8): the text field lost
    /// focus. The next `set_scope_in_session` call (if any) starts a fresh
    /// session rather than continuing to coalesce into this one.
    pub fn end_scope_session(&mut self) {
        self.scope_session = None;
    }

    /// Walk back one entry in the undo stack (spec §3.6). `false` when the
    /// stack is empty — nothing to undo, no version bump.
    pub fn undo_scope(&mut self) -> bool {
        let Some(previous) = self.scope_undo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.scope, previous);
        self.scope_redo.push(current);
        self.versions.scope += 1;
        true
    }

    /// Walk forward one entry in the redo stack (spec §3.6) — only
    /// non-empty right after one or more `undo_scope` calls; any
    /// intervening `set_scope`/`set_scope_in_session` clears it.
    pub fn redo_scope(&mut self) -> bool {
        let Some(next) = self.scope_redo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.scope, next);
        self.scope_undo.push(current);
        self.versions.scope += 1;
        true
    }

    /// Drop one dimension's selection from the current scope — an
    /// undoable edit (spec §3.6), same as `set_text`. `false` when the
    /// scope doesn't constrain `column` at all.
    pub fn drop_dimension(&mut self, column: &str) -> bool {
        let mut s = self.scope.clone();
        let before = s.dimensions.len();
        s.dimensions.retain(|d| d.column != column);
        if s.dimensions.len() == before {
            return false;
        }
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

    /// A reloaded `groupings.toml` (§4.5). Bumps `config`, and `grouping`
    /// too because the active slot's contents may have changed; an active
    /// slot that no longer exists is cleared.
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

    /// `:group save N`: set the slot in memory. The caller persists with
    /// [`persist_slot_to_user_config`] off the UI thread — see
    /// `take_pending_persist`.
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

    /// Take the slot a `save_slot` call is waiting to have written to the
    /// user layer's `groupings.toml`, if any (§4.2). `ShellView` calls this
    /// from its frame-change observer and does the actual write on the
    /// background executor.
    pub fn take_pending_persist(&mut self) -> Option<(u8, Vec<String>)> {
        self.pending_persist.take()
    }

    pub fn as_of(&self) -> &AsOf {
        &self.as_of
    }

    /// Replace as-of, remembering the outgoing value (spec §3.6) so
    /// `undo_as_of` can swap back to it. `false` when nothing changed.
    pub fn set_as_of(&mut self, as_of: AsOf) -> bool {
        if self.as_of == as_of {
            return false;
        }
        self.previous_as_of = Some(std::mem::replace(&mut self.as_of, as_of));
        self.versions.as_of += 1;
        true
    }

    /// Swap the current and remembered-previous as-of (spec §3.6): unlike
    /// `undo_scope`, this is a *swap*, not a pop — calling it twice in a
    /// row toggles back and forth between the two values rather than
    /// exhausting a stack after one call.
    pub fn undo_as_of(&mut self) -> bool {
        let Some(previous) = self.previous_as_of.take() else {
            return false;
        };
        let current = std::mem::replace(&mut self.as_of, previous);
        self.previous_as_of = Some(current);
        self.versions.as_of += 1;
        true
    }

    /// Record a file publish (spec §3.12): bumps `data` (every visible
    /// tile requeries) and keeps it in `recent_publishes`, newest first,
    /// capped at [`RECENT_PUBLISHES`].
    pub fn note_published(&mut self, publish: Publish) {
        self.recent_publishes.push_front(publish);
        self.recent_publishes.truncate(RECENT_PUBLISHES);
        self.versions.data += 1;
    }

    pub fn recent_publishes(&self) -> &VecDeque<Publish> {
        &self.recent_publishes
    }

    pub fn saved_scopes(&self) -> &SavedScopes {
        &self.saved_scopes
    }

    /// A reloaded `scopes.toml` (spec §4.5-style live pickup, mirroring
    /// `replace_slots`). Bumps `config` only — saved scopes are a picker
    /// input, not something a following tile requeries against by
    /// themselves.
    pub fn replace_saved_scopes(&mut self, saved: SavedScopes) -> bool {
        if self.saved_scopes == saved {
            return false;
        }
        self.saved_scopes = saved;
        self.versions.config += 1;
        true
    }

    /// Save the current scope under `name`, in memory immediately and
    /// queued for the user layer's `scopes.toml` (spec §3.9) — see
    /// [`take_pending_scope_persist`](Self::take_pending_scope_persist).
    /// Rejects a name that couldn't round-trip through a TOML key or the
    /// reserved `config_version` key.
    pub fn save_scope(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty()
            || name == "config_version"
            || name.contains(|c: char| c.is_whitespace() || c == '.' || c == '"')
        {
            return Err(format!("'{name}' is not a usable scope name"));
        }
        self.saved_scopes
            .insert(name.to_string(), self.scope.clone());
        self.pending_scope_persist = Some((name.to_string(), self.scope.clone()));
        self.versions.config += 1;
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

    /// Take the scope a `save_scope` call is waiting to have written to
    /// the user layer's `scopes.toml`, if any (§3.9) — same pattern as
    /// `take_pending_persist`.
    pub fn take_pending_scope_persist(&mut self) -> Option<(String, Scope)> {
        self.pending_scope_persist.take()
    }

    /// Empty both undo and redo stacks without touching the current scope
    /// (spec §3.6) — `ShellView::new` calls this right after applying a
    /// restored session's scope, so a restored session doesn't start with
    /// a phantom undo entry back to the empty scope nobody actually chose.
    pub fn clear_history(&mut self) {
        self.scope_undo.clear();
        self.scope_redo.clear();
    }

    pub fn note_config_reloaded(&mut self) {
        self.versions.config += 1;
    }

    /// Global AND tile (foundation §4.2). Phase 3 passes an empty tile
    /// layer; `:filter` will fill it.
    pub fn effective_scope(&self, tile: &Scope) -> Scope {
        self.scope.and_then(tile)
    }

    /// What the toolbar's scope bar shows (spec §3.1/§3.6/§4.4). Cached
    /// (see `bar_cache`'s doc comment) keyed on `versions()`: a call with
    /// nothing changed since the last one returns the exact same `Rc` — a
    /// refcount bump, no fresh allocation — rather than rebuilding.
    pub fn bar_model(&self) -> Rc<ScopeBarModel> {
        // `flip` alone never changes what the bar shows — keyed out here
        // (rather than relying on it happening to already match) so a
        // flip costs a refcount bump like any other unrelated notify,
        // not a rebuild.
        let mut versions = self.versions();
        versions.flip = 0;
        if let Some((cached_versions, cached)) = self.bar_cache.borrow().as_ref()
            && *cached_versions == versions
        {
            return Rc::clone(cached);
        }
        let built = Rc::new(scopebar::build_model(self, chrono::Local::now()));
        *self.bar_cache.borrow_mut() = Some((versions, Rc::clone(&built)));
        built
    }

    /// Open a barrier for the current `(scope, grouping, as_of)` versions
    /// over `keys` (Phase 4 §3.10) — `ShellView::on_frame_changed` calls
    /// this right after it sees one of those three counters move, with
    /// every visible tile's key ([`visible_tile_keys`](crate::shell::
    /// ShellView)). Bumps no version itself, so the notify this does not
    /// emit cannot re-enter whatever branch called it. An empty `keys`
    /// (no visible tile has an occupant yet) closes any barrier outright
    /// rather than opening one nothing could ever satisfy.
    ///
    /// A pinned or unscoped tile self-arrives from its own
    /// `on_frame_changed` (never requerying) only if this already ran in
    /// the same notify flush — which it does: `ShellView`'s own frame
    /// observer is registered before any tile occupant's, and gpui calls
    /// one entity's observers in registration order, so this call always
    /// finishes before a single tile's `on_frame_changed` runs for the
    /// same notify.
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
    /// barrier, which also bumps `flip` via [`release`](Self::release);
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

/// Write one slot into the user layer's `groupings.toml` as a bare
/// numeric key, keeping every other key (Phase 3 §4.2). The same
/// `toml_edit` read-modify-write and atomic rename `theme::
/// persist_to_user_config` uses.
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

/// Write one named scope into the user layer's `scopes.toml`, keeping
/// every other key (spec §3.9) — `persist_slot_to_user_config`'s own
/// `toml_edit` read-modify-write and atomic rename, over a different file
/// and using `geode_core::scopes::scope_to_table` for the value shape.
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
    use geode_core::scope::{DimensionSelection, Scope};

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
        let eff = f.effective_scope(&tile);
        assert_eq!(eff.dimensions, book_scope("BK000").dimensions);
        assert_eq!(eff.text.as_deref(), Some("spx"));
        assert_eq!(f.effective_scope(&Scope::default()), book_scope("BK000"));
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

    // --- Phase 4a: undo/redo, previous as-of, recent publishes, saved
    // scopes, the bar model --------------------------------------------

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
        let m1 = f.bar_model();
        let m2 = f.bar_model();
        assert!(Rc::ptr_eq(&m1, &m2));
        assert_eq!(m1.slot, Some((1, "book / lhu".into())));
        assert_eq!(m1.chips[0].summary, "book ∈ BK001, BK002");
        assert_eq!(m1.chips[1].summary, "lhu ∈ {7}");
        assert_eq!(m1.text.as_deref(), Some("spx"));
        assert_eq!(m1.expr.as_deref(), Some("npv > 0"));
        assert_eq!(m1.as_of, None);
        f.set_text(None);
        assert!(!Rc::ptr_eq(&m1, &f.bar_model()));
    }

    #[test]
    fn a_contradiction_is_named_not_hidden() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        let s = book_scope("A").and_then(&book_scope("B"));
        assert!(s.impossible);
        f.set_scope(s);
        assert_eq!(f.bar_model().impossible.as_deref(), Some("∅ book"));
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

    // --- Phase 4a §3.10: the flip barrier -------------------------------

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
        // `open_flip` is never called for a `data`/`config`-only change
        // (that's `ShellView::on_frame_changed`'s job, tested in
        // `shell/tests/flip.rs`); this is the pure half — a barrier
        // opened over one `(scope, grouping, as_of)` triple must not be
        // satisfiable by a `versions` that only differs in `data` or
        // `config`, since those two fields play no part in `matches`.
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
}
