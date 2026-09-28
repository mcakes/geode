# Workspace-Pinned Frame Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let one workspace pin the frame so its scope, grouping, and as-of
changes stay local to it, while unpinned workspaces keep sharing one frame.

**Architecture:** `Frame` stays one GPUI entity. Its per-selection state
(scope + history, active slot, as-of) moves into a `Lane`; the frame holds a
shared lane plus one lane per pinned workspace. Scope/grouping/as-of
generations come from one frame-wide counter so a number names one value in
any lane. Tiles receive a `FrameRef { entity, workspace }` and read through a
`FrameView` that resolves their lane; the shell reaches lanes through one door,
`ShellView::target_frame()`.

**Tech Stack:** Rust, GPUI (`gpui-pre =0.3.5`), gpui-component 0.6.2, TOML
session persistence.

**Spec:** `docs/superpowers/specs/2026-09-27-workspace-pinned-frame-design.md`

## Global Constraints

- Work in a git worktree (`superpowers:using-git-worktrees`), never on main.
- The pin covers scope, grouping (active slot), and as-of together; no per-part pins.
- Unpin discards the local lane without a confirm; it never promotes local state to shared.
- Definitions stay shared: named expressions, saved scopes, grouping slot contents, publications.
- `frame::pin_workspace`, category "Frame", title "Toggle the frame pin for this workspace", no default binding.
- Pinned glyph uses `chip::Tone::Neutral`; unpinned glyph is bare and muted. No literal colors.
- Invariant: a scope/grouping/as-of generation number names exactly one value, in any lane.
- `session.toml`: `workspaces.N.frame` present iff workspace N is pinned; same `FrameRecord` format as `[frame]`.
- The UI thread performs no session or config I/O.
- CLAUDE.md commands must pass at the end of every task: `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`,
  `cargo check -p geode-shell --features test-support --all-targets`,
  `zsh scripts/mutation-check.sh --anchors-only`.
- Every mutation-harness entry names its detecting test (6th argument). Where this plan writes `\n` inside an anchor or replacement, write a literal newline inside the quoted argument, as the existing multi-line entries in `scripts/mutation-check.sh` do.
- Say "color", not "colour", in any new user-facing text.

## Review Focus

1. Typing in the scope field, then switching workspace with a chord from the field (`alt-2` in the fixture): the remaining keystrokes must land in the new workspace's lane, and the old lane must keep exactly what was typed there. Test in Task 4.
2. A `groupings.toml` reload that removes a slot active only in a hidden pinned workspace: that lane's slot clears and its grouping generation moves, so its tiles regroup when shown. Test in Task 1.
3. Unpinning while the scope field holds an open text session: the session must not leak into the shared lane's undo history, and one `mod+z` afterwards must undo a shared-lane edit, not resurrect pinned text. Test in Task 4.
4. Restarting with a pinned workspace whose record names an undefined named expression: restore keeps the name, and the pinned lane refuses per query like the shared lane does. Test in Task 5.
5. Returning to a workspace whose lane changed while hidden: its tiles requery and promote without waiting `FLIP_DEADLINE` for a barrier that was never opened for them. Test in Task 4.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/geode-shell/src/tiling/workspaces.rs` | `WorkspaceIx` newtype; `Workspaces::active_ix`, `Workspaces::workspace_of` |
| `crates/geode-shell/src/tiling/mod.rs` | re-export `WorkspaceIx` |
| `crates/geode-shell/src/frame.rs` | `Lane`, generation counter, `FrameView`, `FrameViewMut`, pin/unpin, cross-lane slot bumps |
| `crates/geode-shell/src/frame_ref.rs` (new) | `FrameRef`: the gpui handle bound to one workspace |
| `crates/geode-shell/src/scopebar.rs` | `build_model` reads a `FrameView` |
| `crates/geode-shell/src/module.rs` | `ModuleFactory::create` takes `FrameRef`; recording module logs it |
| `crates/geode-shell/src/shell/occupants.rs` | each occupant gets its own workspace's `FrameRef` |
| `crates/geode-shell/src/shell/mod.rs` | `target_frame`, `frame_at`, `active_ix`; flip compare on the active lane; restore pinned lanes |
| `crates/geode-shell/src/shell/dialog.rs` | `ShellModal::workspace` recorded at push |
| `crates/geode-shell/src/shell/input.rs` | switch detection, `frame::pin_workspace`, lane-routed frame actions |
| `crates/geode-shell/src/shell/pin.rs` (new) | `toggle_workspace_pin`, `on_workspace_switched`, `rebind_scope_field` |
| `crates/geode-shell/src/shell/toolbar.rs` | pin glyph |
| `crates/geode-shell/src/session.rs`, `shell/session_io.rs` | pinned lane records |
| `crates/geode-shell/src/defaults.rs` | action registration |
| five module crates + `geode-app` | `Entity<Frame>` → `FrameRef` at the boundary |

---

### Task 1: Lanes inside the frame

**Files:**
- Modify: `crates/geode-shell/src/tiling/workspaces.rs`, `crates/geode-shell/src/tiling/mod.rs:28`
- Modify: `crates/geode-shell/src/frame.rs` (whole struct and impl; tests module at the end)
- Modify: `crates/geode-shell/src/scopebar.rs:161` (`build_model`) and its tests
- Modify: `scripts/mutation-check.sh` (entries anchored in `frame.rs`)

**Interfaces:**
- Produces:
  - `crate::tiling::WorkspaceIx` — `Copy + Ord + Hash + Debug`; `WorkspaceIx::new(n: u8) -> Option<WorkspaceIx>` (1..=9), `WorkspaceIx::FIRST`, `WorkspaceIx::get(self) -> u8`.
  - `Workspaces::active_ix(&self) -> WorkspaceIx`, `Workspaces::workspace_of(&self, id: TileId) -> Option<WorkspaceIx>`.
  - `Frame::view(&self, ws) -> FrameView<'_>`, `Frame::view_mut(&mut self, ws) -> FrameViewMut<'_>`, `Frame::shared(&self) -> FrameView<'_>`, `Frame::shared_mut(&mut self) -> FrameViewMut<'_>`.
  - `Frame::pin(&mut self, ws) -> bool`, `Frame::unpin(&mut self, ws) -> bool`, `Frame::is_pinned(&self, ws) -> bool`, `Frame::pinned_workspaces(&self) -> impl Iterator<Item = WorkspaceIx> + '_`, `Frame::generation(&self) -> u64`, `Frame::config_version(&self) -> u64`, `Frame::data_version(&self) -> u64`.
  - `FrameView` (derefs to `Frame`): `versions`, `versions_for`, `scope`, `active_slot`, `active_grouping`, `as_of`, `expression_term_is`, `effective_scope`, `bar_model`.
  - `FrameViewMut` (derefs mutably to `Frame`): `view()`, the same reads, and every lane mutation listed in Step 3.
  - Migration shims on `Frame` forwarding to the shared lane (deleted in Task 3).

- [ ] **Step 1: `WorkspaceIx` and the two `Workspaces` queries**

In `tiling/workspaces.rs`, above `pub struct Workspaces`:

```rust
/// A workspace's index, 1–9. Frame lanes and tile frame handles key on it;
/// the tiling layer keeps its own `u8` internally and hands this out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkspaceIx(u8);

impl WorkspaceIx {
    pub const FIRST: WorkspaceIx = WorkspaceIx(1);

    /// `None` outside 1–9.
    pub fn new(n: u8) -> Option<WorkspaceIx> {
        (1..=9).contains(&n).then_some(WorkspaceIx(n))
    }

    pub fn get(self) -> u8 {
        self.0
    }
}
```

In `impl Workspaces`:

```rust
    pub fn active_ix(&self) -> WorkspaceIx {
        WorkspaceIx(self.active)
    }

    /// The workspace whose main or dock trees hold `id`. A tile never
    /// moves between workspaces, so the answer is fixed for its life.
    pub fn workspace_of(&self, id: TileId) -> Option<WorkspaceIx> {
        self.spaces()
            .find(|(_, w)| w.region_of(id).is_some())
            .map(|(ix, _)| WorkspaceIx(ix))
    }
```

Add `WorkspaceIx` to the `pub use workspaces::{…}` line in `tiling/mod.rs`.
Add a unit test in `workspaces.rs`'s tests: `workspace_of_names_the_workspace_holding_the_tile`
(split in workspace 1, `switch(2)`, split there; assert each tile maps to its
workspace and an unallocated `TileId(999)` maps to `None`).

- [ ] **Step 2: Write the failing pure tests** in `frame.rs`'s `mod tests`
(`use crate::tiling::WorkspaceIx;` at the top of the module):

```rust
    fn ws(n: u8) -> WorkspaceIx {
        WorkspaceIx::new(n).unwrap()
    }

    #[test]
    fn pinning_copies_values_and_generations_with_empty_history() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("BK000"));
        f.shared_mut().set_active_slot(Some(1));
        assert!(f.pin(ws(2)));
        assert_eq!(f.view(ws(2)).scope(), f.shared().scope());
        assert_eq!(f.view(ws(2)).active_slot(), Some(1));
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
        assert!(!f.unpin(ws(2)), "unpinning an unpinned workspace is refused");
        assert_ne!(f.view(ws(2)).versions().scope, seen.scope,
            "after unpin a tile must see a change when the content differs");
    }

    #[test]
    fn unpinning_an_untouched_lane_keeps_the_generations() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("BK000"));
        f.pin(ws(3));
        let seen = f.view(ws(3)).versions();
        f.unpin(ws(3));
        assert!(f.view(ws(3)).versions().same_flip_identity(seen),
            "equal content keeps equal numbers, so nothing requeries");
    }

    #[test]
    fn undo_is_per_lane() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.shared_mut().set_scope(book_scope("BK000"));
        f.pin(ws(2));
        f.view_mut(ws(2)).set_scope(book_scope("BK001"));
        assert!(f.view_mut(ws(2)).undo_scope());
        assert_eq!(f.view(ws(2)).scope(), &book_scope("BK000"));
        assert!(!f.view_mut(ws(2)).undo_scope(), "pinned history starts at the pin");
        assert!(f.shared_mut().undo_scope());
        assert_eq!(f.shared().scope(), &Scope::default());
    }

    #[test]
    fn a_slot_reload_regroups_every_lane_and_clears_a_vanished_slot_in_a_hidden_lane() {
        let mut f = Frame::new(slots(), SavedScopes::new(), None);
        f.pin(ws(2));
        f.view_mut(ws(2)).set_active_slot(Some(2));
        let (shared_g, pinned_g) = (f.shared().versions().grouping, f.view(ws(2)).versions().grouping);
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
        let (shared_g, pinned_g) = (f.shared().versions().grouping, f.view(ws(2)).versions().grouping);
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
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test -p geode-shell --lib frame::tests`
Expected: compile errors (`view`, `pin`, `WorkspaceIx` not found).

- [ ] **Step 4: Implement lanes**

In `frame.rs`:

1. `use crate::tiling::WorkspaceIx; use std::collections::BTreeMap; use std::ops::{Deref, DerefMut};`
2. Add:

```rust
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
    /// Bounded stack of outgoing scopes, oldest first; see [`UNDO_DEPTH`].
    scope_undo: Vec<Scope>,
    scope_redo: Vec<Scope>,
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

    fn push_undo(&mut self, outgoing: Scope) {
        self.scope_undo.push(outgoing);
        if self.scope_undo.len() > UNDO_DEPTH {
            self.scope_undo.remove(0);
        }
        self.scope_redo.clear();
    }
}
```

3. In `struct Frame`, delete `scope`, `scope_undo`, `scope_redo`, `scope_session`,
   `active_slot`, `as_of`, `previous_as_of`; add:

```rust
    /// The lane every unpinned workspace resolves to.
    shared: Lane,
    /// One lane per pinned workspace. Ordered so session writes are stable.
    pinned: BTreeMap<WorkspaceIx, Lane>,
    /// Source of every lane's scope/grouping/as-of generation; also advanced
    /// by pin and unpin, which makes it the session writer's dirty signal.
    generation: u64,
```

   Keep `versions: FrameVersions`, documenting that only its `data`, `config`,
   `saved_scopes`, and `flip` fields are read; lane fields are composed by
   `FrameView::versions`. Update `Frame::new` accordingly
   (`shared: Lane::default()`, `pinned: BTreeMap::new()`, `generation: 0`).
   `AsOf::default()` is `AsOf::Live`.

4. Lane resolution and the pin API on `impl Frame`:

```rust
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

    pub fn view(&self, ws: WorkspaceIx) -> FrameView<'_> {
        FrameView { frame: self, lane: self.lane(Some(ws)) }
    }
    pub fn view_mut(&mut self, ws: WorkspaceIx) -> FrameViewMut<'_> {
        FrameViewMut { frame: self, ws: Some(ws) }
    }
    /// The shared lane explicitly, whatever any workspace is pinned to.
    /// Session restore and `[frame]` writes use it.
    pub fn shared(&self) -> FrameView<'_> {
        FrameView { frame: self, lane: &self.shared }
    }
    pub fn shared_mut(&mut self) -> FrameViewMut<'_> {
        FrameViewMut { frame: self, ws: None }
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
```

5. Views:

```rust
/// One workspace's reading of the frame: shared state through `Deref`,
/// selection state from the lane the workspace resolves to.
#[derive(Clone, Copy)]
pub struct FrameView<'a> {
    frame: &'a Frame,
    lane: &'a Lane,
}

impl Deref for FrameView<'_> {
    type Target = Frame;
    fn deref(&self) -> &Frame {
        self.frame
    }
}

/// One workspace's writable frame. The lane is resolved per call, so a
/// pin or unpin through `DerefMut` redirects later calls at once.
pub struct FrameViewMut<'a> {
    frame: &'a mut Frame,
    /// `None` addresses the shared lane explicitly.
    ws: Option<WorkspaceIx>,
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
```

   `impl<'a> FrameView<'a>`: move `scope`, `active_slot`, `active_grouping`
   (`self.frame.slots.get(self.lane.active_slot?)`), `as_of`,
   `expression_term_is`, `effective_scope`
   (`self.lane.scope.and_then(tile).resolve(&self.frame.named)`), `bar_model`
   (body unchanged except `let mut versions = self.versions();` and
   `scopebar::build_model(self, clock, today)`), and add:

```rust
    pub fn versions(&self) -> FrameVersions {
        FrameVersions {
            scope: self.lane.scope_gen,
            grouping: self.lane.grouping_gen,
            as_of: self.lane.as_of_gen,
            ..self.frame.versions
        }
    }

    /// `versions` with `data` narrowed to `watches` (see `PublicationWatch`).
    pub fn versions_for<'w>(
        &self,
        watches: impl IntoIterator<Item = &'w PublicationWatch>,
    ) -> FrameVersions {
        FrameVersions {
            data: watches.into_iter().map(|w| w.revision.get()).max().unwrap_or(0),
            ..self.versions()
        }
    }
```

   `impl<'a> FrameViewMut<'a>`:

```rust
    pub fn view(&self) -> FrameView<'_> {
        FrameView { frame: &*self.frame, lane: self.frame.lane(self.ws) }
    }
    pub fn versions(&self) -> FrameVersions { self.view().versions() }
    pub fn scope(&self) -> &Scope { &self.frame.lane(self.ws).scope }
    pub fn active_slot(&self) -> Option<u8> { self.frame.lane(self.ws).active_slot }
    pub fn as_of(&self) -> &AsOf { &self.frame.lane(self.ws).as_of }

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

    pub fn set_scope(&mut self, scope: Scope) -> bool {
        let lane = self.lane();
        if lane.scope == scope {
            return false;
        }
        let outgoing = std::mem::replace(&mut lane.scope, scope);
        lane.push_undo(outgoing);
        self.bump_scope();
        true
    }

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

    pub fn set_as_of(&mut self, as_of: AsOf) -> bool {
        let lane = self.lane();
        if lane.as_of == as_of {
            return false;
        }
        lane.previous_as_of = Some(std::mem::replace(&mut lane.as_of, as_of));
        self.bump_as_of();
        true
    }

    pub fn save_scope(&mut self, name: &str) -> Result<(), String> {
        let name = geode_core::config::check_object_name(name)
            .map_err(|_| format!("'{}' is not a usable scope name", name.trim()))?;
        if geode_core::scopes::RESERVED_NAMES.contains(&name) {
            return Err(format!("'{name}' is reserved"));
        }
        let scope = self.scope().clone();
        self.frame.saved_scopes.insert(name.to_string(), scope.clone());
        self.frame.pending_scope_persist = Some((name.to_string(), scope));
        self.frame.versions.saved_scopes += 1;
        Ok(())
    }

    pub fn open_flip(&mut self, keys: impl IntoIterator<Item = QueryKey>, now: Instant) {
        let awaiting: HashSet<QueryKey> = keys.into_iter().collect();
        if awaiting.is_empty() {
            self.frame.barrier = None;
            return;
        }
        let versions = self.versions();
        self.frame.barrier = Some(FlipBarrier { versions, awaiting, opened: now });
    }
```

   Move the remaining lane methods from `impl Frame` into `impl FrameViewMut`
   verbatim, applying exactly two rewrites: a lane field `self.X` becomes
   `self.lane().X` (bind `let lane = self.lane();` once where a body touches
   several fields), and `self.versions.scope += 1` / `.grouping` / `.as_of`
   becomes `self.bump_scope()` / `bump_grouping()` / `bump_as_of()` after the
   lane borrow ends. The complete list: `clear_scope`, `begin_scope_session`,
   `set_scope_in_session`, `end_scope_session`, `undo_scope`, `redo_scope`,
   `drop_dimension`, `drop_named`, `drop_expression_term`,
   `replace_expression_term`, `name_expression_term`, `edit_expression_term`
   (private), `clear_expression`, `set_text`, `undo_as_of`, `load_scope`
   (reads `self.frame.saved_scopes`), `clear_history`, plus a read-only
   `expression_term_is` forwarding to `self.view()`. Their doc comments move
   with them unchanged.

6. Cross-lane slot changes on `impl Frame`:

```rust
    pub fn replace_slots(&mut self, slots: GroupingSlots) -> bool {
        if self.slots == slots {
            return false;
        }
        self.slots = slots;
        self.versions.config += 1;
        // Every lane regroups, hidden pinned ones included: the numbers a
        // lane's active slot names may now hold other columns.
        let Frame { shared, pinned, slots, generation, .. } = self;
        for lane in std::iter::once(shared).chain(pinned.values_mut()) {
            if lane.active_slot.is_some_and(|n| slots.get(n).is_none()) {
                lane.active_slot = None;
            }
            lane.grouping_gen = fresh(generation);
        }
        true
    }

    pub fn save_slot(&mut self, slot: u8, grouping: Vec<String>) -> Result<(), String> {
        let persisted = grouping.clone();
        if !self.slots.set(slot, grouping) {
            return Err(format!(
                "slot must be 1–9 and the grouping non-empty (got {slot})"
            ));
        }
        let Frame { shared, pinned, generation, .. } = self;
        for lane in std::iter::once(shared).chain(pinned.values_mut()) {
            if lane.active_slot == Some(slot) {
                lane.grouping_gen = fresh(generation);
            }
        }
        self.pending_persist = Some((slot, persisted));
        Ok(())
    }
```

7. Migration shims (temporary; Task 3 deletes them). Directly after the
   `impl Frame` block:

```rust
/// Migration shims: the pre-lane API, answering for the shared lane so
/// callers compile while they move to `FrameView`/`FrameViewMut`.
/// Deleted once every caller names its workspace.
impl Frame {
    pub fn versions(&self) -> FrameVersions { self.shared().versions() }
    pub fn scope(&self) -> &Scope { &self.shared.scope }
    // …one forwarding method per moved method, same signature, body
    // `self.shared().<m>(..)` for reads and `self.shared_mut().<m>(..)`
    // for mutations: versions_for, active_slot, active_grouping, as_of,
    // expression_term_is, effective_scope, bar_model, set_scope,
    // clear_scope, begin_scope_session, set_scope_in_session,
    // end_scope_session, undo_scope, redo_scope, drop_dimension,
    // drop_named, drop_expression_term, replace_expression_term,
    // name_expression_term, clear_expression, set_text, set_active_slot,
    // set_as_of, undo_as_of, save_scope, load_scope, clear_history,
    // open_flip.
}
```

   Write every one of the listed forwarders out in full; none may be omitted
   or the workspace will not compile.

8. `scopebar.rs`: `pub fn build_model(frame: &FrameView<'_>, clock: Clock, today: NaiveDate)`;
   body unchanged (it calls `scope()`, `active_slot()`, `slots()`,
   `named_expressions()`, `as_of()` — all available on `FrameView`). In its
   tests, replace `build_model(&f, …)` with `build_model(&f.shared(), …)`.

- [ ] **Step 5: Fix existing tests whose arithmetic assumed per-field counters**

With one counter, a lane field advances by "some fresh number", not by one.
In `frame.rs` tests, rewrite each `assert_eq!(vN.scope|grouping|as_of, vM.X + 1)`
as `assert_ne!(vN.X, vM.X)` (and keep the neighbouring "unchanged" equalities).
Known sites: `each_mutation_bumps_exactly_its_own_counter`, the assertions at
the former lines 908 and 972. `data`, `config`, `saved_scopes`, `flip`
assertions keep `+ 1`. Run `cargo test --workspace` and apply the same rewrite
to any other `+ 1` assertion on those three fields that fails (for example in
`shell/tests/scopebar.rs`), never to other fields.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p geode-shell --lib frame:: && cargo test --workspace`
Expected: PASS, including the nine new tests.

- [ ] **Step 7: Re-anchor the mutation entries and add the new ones**

Run: `zsh scripts/mutation-check.sh --anchors-only`. For each ANCHOR/AMBIG
entry in `frame.rs`, re-anchor it on the moved code preserving the mutation's
intent (e.g. "set_scope bumps only the scope counter" now anchors on
`self.bump_scope();` inside `FrameViewMut::set_scope`). Add entries in the same
style as the neighbouring `frame:` entries, each naming its test:

- `frame: a pinned lane is resolved for its workspace` — anchor `ws.and_then(|w| self.pinned.get(&w)).unwrap_or(&self.shared)` → `&self.shared`; test `an_edit_in_one_lane_leaves_the_other_alone`.
- `frame: lane generations come from the shared counter` — anchor the body of `fresh` (`*counter += 1;\n    *counter`) → `1`; test `generations_are_unique_across_lanes`.
- `frame: a slot reload regroups hidden pinned lanes` — anchor `std::iter::once(shared).chain(pinned.values_mut())` in `replace_slots` → `std::iter::once(shared)`; test `a_slot_reload_regroups_every_lane_and_clears_a_vanished_slot_in_a_hidden_lane`.
- `frame: pinning copies the shared generations` — anchor `scope_gen: self.scope_gen,` → `scope_gen: 0,`; test `unpinning_an_untouched_lane_keeps_the_generations`.

Run: `zsh scripts/mutation-check.sh "frame:"` — expected: every entry caught
by its named test, no SURVIVED/BUILD. Then `--anchors-only` passes.

- [ ] **Step 8: Full gate and commit**

Run the Global Constraints commands. Then:

```bash
git add -A crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(shell): frame lanes — shared lane plus pinned lanes per workspace"
```

---

### Task 2: `FrameRef` at the module boundary

**Files:**
- Create: `crates/geode-shell/src/frame_ref.rs`
- Modify: `crates/geode-shell/src/lib.rs` (declare `mod frame_ref`), `crates/geode-shell/src/frame.rs` (`pub use crate::frame_ref::FrameRef;`)
- Modify: `crates/geode-shell/src/module.rs` (`ModuleFactory::create`, `Rc<F>` forwarder, placeholder, recording module)
- Modify: `crates/geode-shell/src/shell/occupants.rs:247-262`
- Modify: `crates/geode-shell/src/shell/tests/occupants.rs` (`WatchingFactory::create`)
- Modify: every `Entity<Frame>` in `crates/geode-{blotter,pricer,marketdata,timeseries,diagnostics}/src/**` (list: `grep -rn "Entity<Frame>" crates --include='*.rs'`)
- Modify: `docs/current/architecture.md`, `crates/geode-shell/README.md`

**Interfaces:**
- Consumes: `WorkspaceIx`, `Frame::view`/`view_mut`, `FrameView`, `FrameViewMut`, `Workspaces::workspace_of`.
- Produces:
  - `FrameRef::new(entity: Entity<Frame>, ws: WorkspaceIx) -> FrameRef`, `.entity() -> &Entity<Frame>`, `.workspace() -> WorkspaceIx`, `.read<'a>(&self, cx: &'a App) -> FrameView<'a>`, `.update<R, C: AppContext>(&self, cx: &mut C, f: impl FnOnce(&mut FrameViewMut<'_>, &mut Context<Frame>) -> R) -> R`.
  - `ModuleFactory::create(&self, tile, restored, frame: FrameRef, diagnostics, window, cx)`.
  - `recording::Recorded::Framed(TileId, WorkspaceIx)`, pushed by the recording factory's `create` right after `Created`.

- [ ] **Step 1: Write the failing shell test** in `shell/tests/occupants.rs`:

```rust
/// Each occupant is handed its own workspace's frame: a tile added in
/// workspace 2 reads workspace 2's lane, never the active one at some
/// later moment.
#[gpui::test]
fn an_occupant_is_created_with_its_own_workspaces_frame(cx: &mut gpui::TestAppContext) {
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    dispatch_and_draw(&shell, &mut cx, "workspace::switch_2");
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let framed: Vec<u8> = log
        .borrow()
        .iter()
        .filter_map(|r| match r {
            crate::module::recording::Recorded::Framed(_, ws) => Some(ws.get()),
            _ => None,
        })
        .collect();
    assert_eq!(framed, vec![1, 2]);
}
```

(Check `services_with_recorder`'s return shape and the add action's id in
`shell/tests/mod.rs` before running; use what the neighbouring recorder tests
use.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-shell --features test-support an_occupant_is_created_with_its_own_workspaces_frame`
Expected: compile error (`Recorded::Framed` missing).

- [ ] **Step 3: Implement `FrameRef`**

`crates/geode-shell/src/frame_ref.rs`:

```rust
//! A tile's handle on the frame: the entity plus the workspace the tile
//! lives in. Tiles never move between workspaces, so the binding is fixed
//! for the tile's life; pinning or unpinning that workspace changes which
//! lane `read` resolves to without the tile re-subscribing.

use gpui::{App, AppContext, Context, Entity};

use crate::frame::{Frame, FrameView, FrameViewMut};
use crate::tiling::WorkspaceIx;

#[derive(Clone)]
pub struct FrameRef {
    entity: Entity<Frame>,
    ws: WorkspaceIx,
}

impl FrameRef {
    pub fn new(entity: Entity<Frame>, ws: WorkspaceIx) -> FrameRef {
        FrameRef { entity, ws }
    }

    /// What a tile observes. A change in any lane notifies every observer;
    /// comparing `read(cx).versions()` filters the ones that matter.
    pub fn entity(&self) -> &Entity<Frame> {
        &self.entity
    }

    pub fn workspace(&self) -> WorkspaceIx {
        self.ws
    }

    pub fn read<'a>(&self, cx: &'a App) -> FrameView<'a> {
        self.entity.read(cx).view(self.ws)
    }

    pub fn update<R, C: AppContext>(
        &self,
        cx: &mut C,
        f: impl FnOnce(&mut FrameViewMut<'_>, &mut Context<Frame>) -> R,
    ) -> R {
        let ws = self.ws;
        self.entity.update(cx, |frame, cx| f(&mut frame.view_mut(ws), cx))
    }
}
```

If `AppContext::update_entity` returns a wrapped result in this gpui version,
the compiler will say so; `Entity::update` at
`gpui-pre-0.3.5/src/app/entity_map.rs:476` returns `R` directly.

- [ ] **Step 4: Change the factory boundary and the shell's call**

In `module.rs`, `ModuleFactory::create` and the `Rc<F>` forwarder take
`frame: FrameRef`; the placeholder and recording factories take `_: FrameRef`.
Add `Framed(TileId, WorkspaceIx)` to `recording::Recorded` (doc: "the workspace
of the frame handle `create` received") and push it after `Created`.

In `shell/occupants.rs` `ensure_occupants`, before `let occupant = match factory`:

```rust
            // The tile's own workspace, not the active one: an occupant
            // restored into a hidden workspace reads that workspace's lane.
            let ws = self
                .services
                .workspaces
                .workspace_of(*id)
                .unwrap_or_else(|| self.services.workspaces.active_ix());
            let frame = FrameRef::new(self.frame.clone(), ws);
```

and pass `frame.clone()` to both `create` calls.

- [ ] **Step 5: Migrate the five modules**

In each module crate, mechanically:
- field/param type `Entity<Frame>` → `FrameRef` (import `geode_shell::frame::FrameRef`);
- `cx.observe(&frame, …)` / `cx.observe(&self.frame, …)` → `cx.observe(frame.entity(), …)`;
- `self.frame.read(cx).<m>()` stays as written (it now returns a `FrameView`);
- `self.frame.update(cx, |f, cx| …)` stays as written (`f` is now `&mut FrameViewMut`);
- test fixtures that build a frame with `cx.new(|_| Frame::new(..))` wrap it as
  `FrameRef::new(frame.clone(), WorkspaceIx::FIRST)` where a tile or content
  is constructed, keeping the raw entity for test-side mutation through the
  Task 1 shims.

Update `WatchingFactory::create` in `shell/tests/occupants.rs` the same way.
Let `cargo check --workspace --all-targets` drive the list.

- [ ] **Step 6: Run the tests**

Run: `cargo test --workspace`
Expected: PASS, including `an_occupant_is_created_with_its_own_workspaces_frame`.

- [ ] **Step 7: Harness entry, docs, gate, commit**

Add `occupants: a tile is framed by its own workspace` — anchor
`.workspace_of(*id)` block's `let frame = FrameRef::new(self.frame.clone(), ws);`
→ `let frame = FrameRef::new(self.frame.clone(), self.services.workspaces.active_ix());`,
package `geode-shell`, test `an_occupant_is_created_with_its_own_workspaces_frame`.
Run it; expected caught.

`docs/current/architecture.md`: where the module boundary lists what a module
receives, replace `Entity<Frame>` with `FrameRef` and add two sentences: a
tile's `FrameRef` is bound to its workspace for life; reads resolve to that
workspace's lane (see shell.md, "The shared frame"). Same note in
`crates/geode-shell/README.md`'s module-boundary paragraph.

Run the Global Constraints commands (`--anchors-only` will flag anchors on
changed module lines; re-anchor them). Commit:

```bash
git commit -am "feat(shell): modules receive a workspace-bound FrameRef"
```

---

### Task 3: The shell reaches lanes through one door

**Files:**
- Modify: `crates/geode-shell/src/shell/mod.rs` (helpers, `on_frame_changed`, restore, filter subscription, poll-loop sweep)
- Modify: `crates/geode-shell/src/shell/dialog.rs` (`ShellModal::workspace`)
- Modify: every frame read/write in `crates/geode-shell/src/shell/{input,render,picker,asof_view,choicedialog,scope_expr_view,expr_suggest,palette_ctl,hot_reload,session_io}.rs` and `shell/objectdialog/{render,scopes}.rs`
- Modify: `crates/geode-app/src/{bridge,main}.rs`
- Modify: `crates/geode-shell/src/frame.rs` (delete the shims)
- Modify: shell, module, and app tests that mutated the frame through shims
- Test: `crates/geode-shell/src/shell/tests/pin.rs` (new; register in `shell/tests/mod.rs`)

**Interfaces:**
- Consumes: `FrameRef`, `Frame::view_mut`, `Workspaces::active_ix`.
- Produces:
  - `ShellView::active_ix(&self) -> WorkspaceIx`
  - `ShellView::frame_at(&self, ws: WorkspaceIx) -> FrameRef`
  - `ShellView::target_frame(&self) -> FrameRef` — the base modal's recorded workspace while a modal is open, else the active workspace
  - `ShellView::active_frame(&self) -> FrameRef` (`pub`, for `geode-app`)
  - `ShellModal::workspace: WorkspaceIx`
  - `ShellView::frame(&self) -> &Entity<Frame>` keeps its signature (shared state only)

- [ ] **Step 1: Write the failing dialog-target test** in the new
`shell/tests/pin.rs` (`use super::*;`):

```rust
/// A frame dialog commits into the workspace it was opened from, even if
/// the active workspace changed underneath it.
#[gpui::test]
fn a_frame_dialog_commits_into_the_workspace_it_opened_from(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    let ws1 = WorkspaceIx::FIRST;
    let ws2 = WorkspaceIx::new(2).unwrap();
    frame.update(&mut vcx, |f, _| assert!(f.pin(ws1)));
    dispatch_and_draw(&shell, &mut vcx, "frame::scope_expression");
    assert!(shell.read_with(&vcx, |s, _| s.modal_open()));
    // Test-only: move the active workspace underneath the open modal.
    shell.update(&mut vcx, |s, _| assert!(s.services.workspaces.switch(2)));
    vcx.simulate_input("book = 'BK000'");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    let (pinned, shared) = frame.read_with(&vcx, |f, _| {
        (f.view(ws1).scope().expression.is_some(), f.view(ws2).scope().expression.is_some())
    });
    assert!(pinned, "the expression lands in workspace 1's pinned lane");
    assert!(!shared, "the shared lane is untouched");
}
```

(If `book` is not a column in the test fixture's schema, use a column the
existing `shell/tests/scope_expr.rs` tests type.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-shell --features test-support a_frame_dialog_commits_into_the_workspace_it_opened_from`
Expected: FAIL on the `pinned` assertion (the commit writes the shared lane
through the shims).

- [ ] **Step 3: Add the door**

`dialog.rs`: add to `ShellModal`

```rust
    /// The workspace active when this entry was pushed. The stack's base
    /// entry decides which lane every frame dialog reads and commits to.
    pub workspace: WorkspaceIx,
```

and set `workspace: view.services.workspaces.active_ix(),` in the push.

`shell/mod.rs`, in `impl ShellView`:

```rust
    pub(crate) fn active_ix(&self) -> WorkspaceIx {
        self.services.workspaces.active_ix()
    }

    pub(crate) fn frame_at(&self, ws: WorkspaceIx) -> FrameRef {
        FrameRef::new(self.frame.clone(), ws)
    }

    /// The lane every shell surface reads and writes: the workspace the
    /// open modal stack was opened from, else the active one. A dialog
    /// therefore commits where it was opened, whatever is active later.
    pub(crate) fn target_frame(&self) -> FrameRef {
        let ws = self.modals.first().map(|m| m.workspace).unwrap_or_else(|| self.active_ix());
        self.frame_at(ws)
    }

    /// The active workspace's frame, for the app's catalog as-of.
    pub fn active_frame(&self) -> FrameRef {
        self.frame_at(self.active_ix())
    }
```

- [ ] **Step 4: Route every shell and app call site**

Rewrite rules (apply to the files listed above):
- `self.frame.read(cx).<lane read>` / `view.frame…` / `shell.frame…` → `self.target_frame().read(cx).<lane read>`.
- `self.frame.update(cx, |f, cx| { <lane mutation> })` → `self.target_frame().update(cx, |f, cx| { … })`.
- Shared-only operations (`replace_slots`, `replace_saved_scopes`, `replace_named_expressions`, `note_config_reloaded`, `note_published`, `take_pending_*`, `sweep`, `requery`, `saved_scopes`, `named_expressions`, `recent_publishes`, `slots`) may stay on `self.frame` directly.
- `on_frame_changed`: the flip compare and `open_flip` use `self.active_frame()` (visible tiles are the active workspace's), not `target_frame()`:

```rust
        let active = self.active_frame();
        let now_v = active.read(cx).versions();
        …
            active.update(cx, |f, _| f.open_flip(keys.iter().copied(), Instant::now()));
```

  and its text reflection reads `self.active_frame().read(cx).scope().text`.
- Startup restore (`mod.rs:1163`) writes the shared lane explicitly:
  `frame.update(cx, |f, _| { let mut s = f.shared_mut(); s.set_scope(record.scope); s.set_active_slot(record.active_slot); s.set_as_of(record.as_of); s.clear_history(); })`;
  `last_flip_versions` seeds from `frame.read(cx).view(services.workspaces.active_ix()).versions()`.
- The filter-input subscription (`mod.rs:743-772`) uses `view.active_frame()`.
- `session_io.rs`: `frame_record` reads `self.frame.read(cx).shared()`; the dirty
  check compares `self.frame.read(cx).generation()` against a
  `last_frame_generation_written: u64` field that replaces
  `last_frame_versions_written`.
- `geode-app/src/bridge.rs:698,1052`: `shell.read(cx).active_frame().read(cx).as_of().clone()`.
  `bridge.rs:848,853` and `main.rs:279,285`: `frame.read(cx).config_version()`.

- [ ] **Step 5: Delete the shims**

Remove the "Migration shims" `impl Frame` block from `frame.rs`. Run
`cargo check --workspace --all-targets`; every remaining error is a caller
that has not named its lane. Fix production callers with the Step 4 rules.
Fix test callers by addressing a lane explicitly: shell tests use
`frame.update(&mut vcx, |f, _| f.shared_mut().<m>(..))` or read
`f.shared().<m>()` when the test never pins; module tests use the tile's
`FrameRef`; `bridge.rs` tests use `f.shared_mut().set_as_of(..)` and
`f.data_version()`.

- [ ] **Step 6: Run the tests**

Run: `cargo test --workspace`
Expected: PASS, including `a_frame_dialog_commits_into_the_workspace_it_opened_from`.

- [ ] **Step 7: Harness entry, gate, commit**

Add `shell: a dialog targets the workspace it opened from` — anchor
`let ws = self.modals.first().map(|m| m.workspace).unwrap_or_else(|| self.active_ix());`
→ `let ws = self.active_ix();`, package `geode-shell`, test
`a_frame_dialog_commits_into_the_workspace_it_opened_from`. Run it; expected
caught. Run the Global Constraints commands, re-anchoring entries flagged on
edited lines. Commit:

```bash
git commit -am "refactor(shell): every frame access names its lane; drop the shared-lane shims"
```

---

### Task 4: Pin action, toolbar glyph, and workspace switching

**Files:**
- Create: `crates/geode-shell/src/shell/pin.rs` (declare `mod pin;` in `shell/mod.rs`)
- Modify: `crates/geode-shell/src/shell/input.rs:217` (switch detection) and its action chain (`frame::pin_workspace`)
- Modify: `crates/geode-shell/src/defaults.rs` (registration, in the Frame block near `frame::scope_clear`)
- Modify: `crates/geode-shell/src/shell/toolbar.rs`, `crates/geode-shell/src/shell/render.rs:951`
- Test: `crates/geode-shell/src/shell/tests/pin.rs`

**Interfaces:**
- Consumes: `Frame::pin/unpin/is_pinned`, `ShellView::{active_ix, frame_at, active_frame}`, `FrameViewMut::{begin_scope_session, end_scope_session}`.
- Produces:
  - `ShellView::toggle_workspace_pin(&mut self, window, cx)`
  - `ShellView::on_workspace_switched(&mut self, prev: WorkspaceIx, window, cx)`
  - `toolbar::PinState { pub ws: WorkspaceIx, pub pinned: bool }` and a `toolbar(…, pin: PinState, on_pin: impl Fn(&mut Window, &mut App) + Clone + 'static, …)` parameter pair placed before `cx`.

- [ ] **Step 1: Write the failing tests** in `shell/tests/pin.rs`. The fixture's
default modifier is Alt, so `mod+2` is `alt-2`; `ctrl+1` activates slot 1.
Reuse `grouping.rs`'s slot setup by calling `replace_slots` on the frame.

```rust
fn slots() -> geode_core::groupings::GroupingSlots {
    let mut s = geode_core::groupings::GroupingSlots::default();
    s.set(1, vec!["book".into()]);
    s.set(2, vec!["lhu".into()]);
    s
}

fn open_pinnable(cx: &mut gpui::TestAppContext)
    -> (gpui::WindowHandle<Root>, gpui::VisualTestContext, Entity<ShellView>, Entity<Frame>)
{
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| { f.replace_slots(slots()); cx.notify(); });
    vcx.run_until_parked();
    (window, vcx, shell, frame)
}

fn ws(n: u8) -> WorkspaceIx { WorkspaceIx::new(n).unwrap() }

#[gpui::test]
fn the_pin_action_toggles_the_active_workspace(cx: &mut gpui::TestAppContext) {
    let (_w, mut vcx, shell, frame) = open_pinnable(cx);
    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_2");
    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace");
    assert!(frame.read_with(&vcx, |f, _| f.is_pinned(ws(2))));
    assert!(!frame.read_with(&vcx, |f, _| f.is_pinned(ws(1))));
    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace");
    assert!(!frame.read_with(&vcx, |f, _| f.is_pinned(ws(2))));
}

#[gpui::test]
fn clicking_the_pin_glyph_pins_the_active_workspace(cx: &mut gpui::TestAppContext) {
    let (_w, mut vcx, _shell, frame) = open_pinnable(cx);
    let bounds = vcx.debug_bounds("scope-pin").expect("the pin glyph paints");
    vcx.simulate_click(bounds.center(), gpui::Modifiers::none());
    vcx.run_until_parked();
    assert!(frame.read_with(&vcx, |f, _| f.is_pinned(WorkspaceIx::FIRST)));
}

#[gpui::test]
fn keys_in_a_pinned_workspace_stay_there(cx: &mut gpui::TestAppContext) {
    let (_w, mut vcx, shell, frame) = open_pinnable(cx);
    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_2");
    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace");
    vcx.simulate_keystrokes("ctrl-1");
    vcx.run_until_parked();
    assert_eq!(frame.read_with(&vcx, |f, _| f.view(ws(2)).active_slot()), Some(1));
    assert_eq!(frame.read_with(&vcx, |f, _| f.view(ws(1)).active_slot()), None);
    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_1");
    vcx.simulate_keystrokes("ctrl-2");
    vcx.run_until_parked();
    assert_eq!(frame.read_with(&vcx, |f, _| f.view(ws(2)).active_slot()), Some(1),
        "a shared-lane edit does not reach the pinned workspace");
}

#[gpui::test]
fn switching_workspace_reseeds_the_barrier_without_opening_one(cx: &mut gpui::TestAppContext) {
    let (_w, mut vcx, shell, frame) = open_pinnable(cx);
    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_2");
    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace");
    vcx.simulate_keystrokes("ctrl-1");
    vcx.run_until_parked();
    frame.update(&mut vcx, |f, _| { f.sweep(Instant::now() + FLIP_DEADLINE * 2); });
    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_1");
    vcx.run_until_parked();
    assert!(!frame.read_with(&vcx, |f, _| f.barrier_open()),
        "a switch shows another lane; it is not a frame change to flip");
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.last_flip_versions),
        frame.read_with(&vcx, |f, _| f.view(ws(1)).versions()),
    );
}

#[gpui::test]
fn typing_across_a_workspace_switch_lands_in_each_lane(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx, shell, frame) = open_pinnable(cx);
    vcx.update(|window, _| window.activate_window());
    let _ = window;
    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_2");
    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace");
    vcx.simulate_keystrokes("alt-/");
    vcx.update(|window, cx| { let _ = window.draw(cx); });
    vcx.simulate_input("sp");
    vcx.simulate_keystrokes("alt-1"); // switch from inside the field
    vcx.update(|window, cx| { let _ = window.draw(cx); });
    vcx.simulate_keystrokes("alt-/");
    vcx.update(|window, cx| { let _ = window.draw(cx); });
    vcx.simulate_input("nd");
    let (two, one) = frame.read_with(&vcx, |f, _| {
        (f.view(ws(2)).scope().text.clone(), f.view(ws(1)).scope().text.clone())
    });
    assert_eq!(two.as_deref(), Some("sp"));
    assert_eq!(one.as_deref(), Some("nd"), "the field shows and edits the new lane");
}

#[gpui::test]
fn unpinning_mid_session_leaves_the_shared_history_clean(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx, shell, frame) = open_pinnable(cx);
    vcx.update(|window, _| window.activate_window());
    let _ = window;
    frame.update(&mut vcx, |f, _| { f.shared_mut().set_active_slot(Some(2)); });
    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace");
    vcx.simulate_keystrokes("alt-/");
    vcx.update(|window, cx| { let _ = window.draw(cx); });
    vcx.simulate_input("pinned");
    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace"); // unpin
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert_eq!(frame.read_with(&vcx, |f, _| f.shared().scope().text.clone()), None,
        "pinned text never reached the shared lane");
    assert!(!frame.update(&mut vcx, |f, _| f.shared_mut().undo_scope()),
        "and left no undo entry there");
}

#[gpui::test]
fn a_lane_changed_while_hidden_promotes_without_waiting(cx: &mut gpui::TestAppContext) {
    let (_w, mut vcx, shell, frame) = open_pinnable(cx);
    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_2");
    dispatch_and_draw(&shell, &mut vcx, "frame::pin_workspace");
    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_1");
    // Workspace 2's lane changes while hidden (a hot reload bumps it).
    frame.update(&mut vcx, |f, cx| {
        f.view_mut(ws(2)).set_text(Some("hidden".into()));
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(!frame.read_with(&vcx, |f, _| f.barrier_open()),
        "no barrier for a lane nobody can see");
    dispatch_and_draw(&shell, &mut vcx, "workspace::switch_2");
    assert!(!frame.read_with(&vcx, |f, _| f.barrier_open()),
        "showing it opens none either; its tiles requery and promote directly");
}
```

`last_flip_versions` must be `pub(super)`-visible to tests; if it is private,
add a `#[cfg(test)] pub(crate) fn last_flip_versions(&self) -> FrameVersions`
accessor and use it. Imports at the top of `pin.rs`:
`use crate::frame::{FLIP_DEADLINE, Frame}; use crate::tiling::WorkspaceIx; use std::time::Instant;`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell --features test-support shell::tests::pin`
Expected: FAIL — unknown action, no `scope-pin` selector, text lands in the wrong lane.

- [ ] **Step 3: Register the action**

In `defaults.rs`, after `frame::scope_clear`:

```rust
    // Give the active workspace its own scope, grouping, and as-of, or
    // return it to the shared frame (discarding its own). Toolbar glyph
    // and palette; no default chord.
    action(
        reg,
        "frame::pin_workspace",
        "Toggle the frame pin for this workspace",
        "Frame",
    );
```

- [ ] **Step 4: `shell/pin.rs`**

```rust
//! Workspace pinning at the shell: the toggle, the switch hook, and the
//! scope field's rebinding. A text-editing session belongs to one lane;
//! whenever the lane the field edits changes (a switch, a pin, an unpin)
//! the old session ends there and the field re-reads the new lane.

use gpui::{Context, Window};

use super::ShellView;
use crate::tiling::WorkspaceIx;

impl ShellView {
    pub(super) fn toggle_workspace_pin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ws = self.active_ix();
        self.frame.update(cx, |f, cx| {
            f.view_mut(ws).end_scope_session();
            if !f.unpin(ws) {
                f.pin(ws);
            }
            cx.notify();
        });
        self.session_dirty = true;
        self.rebind_scope_field(window, cx);
    }

    /// The active workspace changed from `prev`. Showing another lane is not
    /// a frame change: re-seed the flip baseline instead of opening a barrier.
    pub(super) fn on_workspace_switched(
        &mut self,
        prev: WorkspaceIx,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.frame.update(cx, |f, _| f.view_mut(prev).end_scope_session());
        self.last_flip_versions = self.active_frame().read(cx).versions();
        self.rebind_scope_field(window, cx);
        cx.notify();
    }

    /// Show the active lane's text in the scope field. A focused field keeps
    /// focus and starts a fresh session on the new lane, so the next
    /// keystroke coalesces there and Escape restores the new lane's text.
    fn rebind_scope_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let here = self.active_frame();
        let text = here.read(cx).scope().text.clone().unwrap_or_default();
        let focused = self.filter_input.read(cx).focus_handle(cx).is_focused(window);
        // `set_value` emits no Change event, so this cannot feed the session.
        self.filter_input.update(cx, |i, cx| i.set_value(text.clone(), window, cx));
        if focused {
            self.filter_session_base = Some(text);
            here.update(cx, |f, _| f.begin_scope_session());
        } else {
            self.filter_session_base = None;
        }
    }
}
```

- [ ] **Step 5: Wire the switch and the action in `input.rs`**

Replace the `apply_workspace_action` block:

```rust
        let before = self.active_ix();
        let handled = apply_workspace_action(&mut self.services.workspaces, action);
        if handled {
            self.session_dirty = true;
            if self.active_ix() != before {
                self.on_workspace_switched(before, window, cx);
            }
            self.note_keyboard_focus_move(window, cx);
        } else if action.0 == "frame::pin_workspace" {
            self.toggle_workspace_pin(window, cx);
        } else if action.0 == "palette::toggle" {
```

- [ ] **Step 6: The toolbar glyph**

In `toolbar.rs`:

```rust
/// Whether the active workspace holds its own frame lane.
#[derive(Clone, Copy)]
pub struct PinState {
    pub ws: WorkspaceIx,
    pub pinned: bool,
}

/// Tooltip titles by workspace, static so hovering allocates nothing.
const PIN_TITLES: [&str; 9] = [
    "Pin the frame to workspace 1", "Pin the frame to workspace 2",
    "Pin the frame to workspace 3", "Pin the frame to workspace 4",
    "Pin the frame to workspace 5", "Pin the frame to workspace 6",
    "Pin the frame to workspace 7", "Pin the frame to workspace 8",
    "Pin the frame to workspace 9",
];
const PINNED_TITLES: [&str; 9] = [
    "Frame pinned to workspace 1", "Frame pinned to workspace 2",
    "Frame pinned to workspace 3", "Frame pinned to workspace 4",
    "Frame pinned to workspace 5", "Frame pinned to workspace 6",
    "Frame pinned to workspace 7", "Frame pinned to workspace 8",
    "Frame pinned to workspace 9",
];
const PIN_HINT: &str = "scope, grouping, and as-of changes here stay here";
const PINNED_HINT: &str =
    "scope, grouping, and as-of changes stay here · click to rejoin the shared frame";
```

Add `pin: PinState, on_pin: impl Fn(&mut Window, &mut App) + Clone + 'static,`
before `cx: &App`. In the readout, make the glyph its first child (before the
as-of chip):

```rust
    let i = usize::from(pin.ws.get() - 1);
    let (title, hint) = if pin.pinned {
        (PINNED_TITLES[i], PINNED_HINT)
    } else {
        (PIN_TITLES[i], PIN_HINT)
    };
    // Pinned paints as a selected, hazard-free state (the Neutral chip);
    // unpinned is a bare verb like `+` and save.
    let pinned_paint = chip::chip_paint(theme, chip::Tone::Neutral);
    let (pin_fg, pin_bg, pin_states) = if pin.pinned {
        (
            pinned_paint.text,
            pinned_paint.fill,
            control::for_chip(theme, &pinned_paint, theme.title_bar),
        )
    } else {
        (chip_fg, None, glyph_states)
    };
    let pin_glyph = div()
        .id("scope-pin")
        .flex()
        .items_center()
        .justify_center()
        .size(scale::design(GLYPH_BOX))
        .rounded(glyph_radius)
        .text_color(pin_fg)
        .when_some(pin_bg, |el, bg| el.bg(bg))
        .child(Icon::new(CatalogIcon::Pin).small())
        .debug_selector(|| "scope-pin".to_string())
        .occlude()
        .pointer_states(pin_states)
        .tooltip(tips::tip_with(
            SharedString::new_static("tip-scope-pin"),
            SharedString::new_static(title),
            Some("frame::pin_workspace"),
            Some(SharedString::new_static(hint)),
        ))
        .on_mouse_down(MouseButton::Left, move |_event, window, cx| on_pin(window, cx));
```

and `readout … .child(pin_glyph).child(divider("scope-divider-pin", theme.title_bar_border))`
ahead of the as-of `.when_some`. Use `WorkspaceIx` via `crate::tiling::WorkspaceIx`.
If `CatalogIcon::Pin` does not resolve, the variant generated from
`assets/icons/pin.svg` has another spelling; find it with
`grep -rn "pin.svg" $(ls -d ~/.cargo/registry/src/*/gpui-kit-assets-0.6.2)` and
the build output, and use that.

Check that `tips::tip_with`'s parameter types accept these `SharedString`s
(its signature is in `crates/geode-shell/src/tips.rs`); match them.

In `render.rs` at the `toolbar::toolbar(` call, compute and pass:

```rust
        let ws = self.active_ix();
        let pin = toolbar::PinState { ws, pinned: self.frame.read(cx).is_pinned(ws) };
        let on_pin = {
            let entity = cx.entity();
            move |window: &mut Window, cx: &mut App| {
                entity.update(cx, |view, cx| view.toggle_workspace_pin(window, cx));
            }
        };
```

Update the module doc in `toolbar.rs` (first paragraph) to name the pin glyph
as the readout's first control.

- [ ] **Step 7: Run the tests**

Run: `cargo test -p geode-shell --features test-support shell::tests::pin && cargo test --workspace`
Expected: PASS.

- [ ] **Step 8: Harness entries, gate, commit**

Add, each naming its test, package `geode-shell`:
- `pin: a switch re-seeds the flip baseline` — anchor `self.last_flip_versions = self.active_frame().read(cx).versions();` in `on_workspace_switched` → `{}`; test `switching_workspace_reseeds_the_barrier_without_opening_one`.
- `pin: a switch rebinds the scope field` — anchor `self.rebind_scope_field(window, cx);\n        cx.notify();` in `on_workspace_switched` → `cx.notify();`; test `typing_across_a_workspace_switch_lands_in_each_lane`.
- `pin: toggling ends the lane's text session` — anchor `f.view_mut(ws).end_scope_session();\n            if !f.unpin(ws)` → `if !f.unpin(ws)`; test `unpinning_mid_session_leaves_the_shared_history_clean`.
- `pin: the action reaches the toggle` — anchor `self.toggle_workspace_pin(window, cx);\n        } else if action.0 == "palette::toggle"` → `} else if action.0 == "palette::toggle"`; test `the_pin_action_toggles_the_active_workspace`.

Run `zsh scripts/mutation-check.sh "pin:"`; expected all caught. Run the
Global Constraints commands. Commit:

```bash
git commit -am "feat(shell): pin the frame to a workspace from the toolbar or palette"
```

---

### Task 5: Persist pinned lanes

**Files:**
- Modify: `crates/geode-shell/src/session.rs` (`Restored`, `to_toml`, `to_string_pretty`, `save`, `from_toml`, `load`'s `fresh`)
- Modify: `crates/geode-shell/src/shell/session_io.rs`, `crates/geode-shell/src/shell/mod.rs` (`ShellServices::restored_pinned`, restore)
- Modify: `crates/geode-app/src/main.rs:207`, and every `ShellServices { … }` literal (`grep -rn "restored_frame: None" crates`)
- Test: `crates/geode-shell/src/session.rs` tests; `crates/geode-shell/src/shell/tests/pin.rs`

**Interfaces:**
- Consumes: `Frame::pinned_workspaces`, `Frame::view`, `Frame::pin`, `FrameViewMut::{set_scope, set_active_slot, set_as_of, clear_history}`.
- Produces:
  - `pub type PinnedRecords = BTreeMap<WorkspaceIx, FrameRecord>;` in `session.rs`
  - `Restored::pinned: PinnedRecords`
  - `to_toml(workspaces, tiles, frame: Option<&FrameRecord>, pinned: &PinnedRecords, palette_usage)` (same new parameter on `to_string_pretty` and `save`)
  - `ShellServices::restored_pinned: PinnedRecords`

- [ ] **Step 1: Write the failing session tests** in `session.rs` tests:

```rust
    #[test]
    fn a_pinned_lane_round_trips_under_its_workspace() {
        let mut spaces = Workspaces::new();
        spaces.switch(2);
        let mut pinned = PinnedRecords::new();
        pinned.insert(WorkspaceIx::new(2).unwrap(), sample_frame_record());
        let table = to_toml(&spaces, &TileRecords::new(), None, &pinned, &PaletteUsage::new());
        let ws2 = table["workspaces"]["2"].as_table().unwrap();
        assert!(ws2.contains_key("frame"));
        assert!(!table["workspaces"]["1"].as_table().unwrap().contains_key("frame"),
            "an unpinned workspace writes no frame");
        let restored = from_toml(&table).unwrap();
        assert_eq!(restored.pinned, pinned);
    }

    #[test]
    fn a_non_table_workspace_frame_restores_unpinned_with_a_warning() {
        let mut table = to_toml(&Workspaces::new(), &TileRecords::new(), None,
            &PinnedRecords::new(), &PaletteUsage::new());
        table["workspaces"]["1"].as_table_mut().unwrap()
            .insert("frame".into(), toml::Value::Integer(3));
        let restored = from_toml(&table).unwrap();
        assert!(restored.pinned.is_empty());
        assert!(restored.warnings.iter().any(|w| w.contains("workspaces.1.frame")));
    }

    #[test]
    fn a_partial_pinned_record_keeps_its_usable_fields() {
        let mut table = to_toml(&Workspaces::new(), &TileRecords::new(), None,
            &PinnedRecords::new(), &PaletteUsage::new());
        let frame: toml::Table = r#"
            slot = 2
            as_of = "not a date"
            named = ["undefined_name"]
        "#.parse().unwrap();
        table["workspaces"]["1"].as_table_mut().unwrap()
            .insert("frame".into(), toml::Value::Table(frame));
        let restored = from_toml(&table).unwrap();
        let record = &restored.pinned[&WorkspaceIx::FIRST];
        assert_eq!(record.active_slot, Some(2));
        assert_eq!(record.scope.named, vec!["undefined_name".to_string()],
            "an undefined name is kept; the lane refuses per query");
        assert!(!restored.warnings.is_empty(), "the bad as-of warns");
    }
```

(Match the `FrameRecord::to_toml` key names — read `FrameRecord::to_toml`/
`from_toml` at `session.rs:112-250` and use its actual keys for slot, as-of,
and named in the partial-record fixture.)

And in `shell/tests/pin.rs`:

```rust
#[gpui::test]
fn a_restored_pinned_workspace_is_pinned_with_its_record(cx: &mut gpui::TestAppContext) {
    let mut services = test_services();
    let mut record = crate::session::FrameRecord {
        scope: geode_core::scope::Scope::default(),
        active_slot: None,
        as_of: geode_core::query::AsOf::Live,
    };
    record.scope.text = Some("spx".into());
    services.restored_pinned.insert(ws(1), record);
    let (window, mut vcx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    assert!(frame.read_with(&vcx, |f, _| f.is_pinned(ws(1))));
    assert_eq!(frame.read_with(&vcx, |f, _| f.view(ws(1)).scope().text.clone()).as_deref(), Some("spx"));
    assert_eq!(frame.read_with(&vcx, |f, _| f.shared().scope().text.clone()), None);
    assert!(!frame.update(&mut vcx, |f, _| f.view_mut(ws(1)).undo_scope()),
        "restore leaves no undo entry");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell --features test-support pinned`
Expected: compile errors (`PinnedRecords`, `restored_pinned`).

- [ ] **Step 3: Implement**

`session.rs`:
- `pub type PinnedRecords = BTreeMap<WorkspaceIx, FrameRecord>;` (import `crate::tiling::WorkspaceIx`).
- `Restored` gains `pub pinned: PinnedRecords,`; `load`'s `fresh` sets `PinnedRecords::new()`.
- `to_toml` gains `pinned: &PinnedRecords`; inside the per-workspace loop, before `spaces_table.insert(…)`:

```rust
        // A pinned workspace carries its own lane; presence means pinned.
        if let Some(record) = WorkspaceIx::new(ix).and_then(|w| pinned.get(&w)) {
            ws_table.insert("frame".to_string(), toml::Value::Table(record.to_toml()));
        }
```

- `from_toml`: in the `for (key, value) in workspaces_table` loop, after a
  successful `parse_workspace` returns `ix`:

```rust
                            match value.get("frame") {
                                None => {}
                                Some(toml::Value::Table(t)) => {
                                    if let Some(w) = WorkspaceIx::new(ix) {
                                        pinned.insert(w, FrameRecord::from_toml(t, &mut warnings));
                                    }
                                }
                                Some(_) => warnings.push(format!(
                                    "workspaces.{ix}.frame is not a table; workspace {ix} restores unpinned"
                                )),
                            }
```

  declaring `let mut pinned = PinnedRecords::new();` beside `tiles`, and
  returning it in `Restored`. Update the `to_toml`/`from_toml` doc comments
  to name `workspaces.N.frame`.
- `to_string_pretty` and `save` take and pass `pinned: &PinnedRecords`.

`shell/mod.rs`:
- `ShellServices` gains `pub restored_pinned: crate::session::PinnedRecords,`
  (doc: "pinned workspace lanes from `session.toml`; each is pinned and filled
  at startup"). Add `restored_pinned: Default::default(),` to every literal
  found by `grep -rn "restored_frame: None" crates`.
- After the shared-lane restore block:

```rust
        for (ws, record) in services.restored_pinned.clone() {
            frame.update(cx, |f, _| {
                f.pin(ws);
                let mut lane = f.view_mut(ws);
                lane.set_scope(record.scope);
                lane.set_active_slot(record.active_slot);
                lane.set_as_of(record.as_of);
                lane.clear_history();
            });
        }
```

  keeping `last_flip_versions`' seed after this loop.

`shell/session_io.rs`: add

```rust
    fn pinned_records(&self, cx: &App) -> session::PinnedRecords {
        let frame = self.frame.read(cx);
        frame
            .pinned_workspaces()
            .map(|ws| {
                let lane = frame.view(ws);
                (ws, FrameRecord {
                    scope: lane.scope().clone(),
                    active_slot: lane.active_slot(),
                    as_of: lane.as_of().clone(),
                })
            })
            .collect()
    }
```

and pass `&self.pinned_records(cx)` to both `to_string_pretty` and `save`.
`geode-app/src/main.rs:207`: `services.restored_pinned = restored.pinned;`.
Update every other `to_toml(…)` / `to_string_pretty(…)` call (tests included)
with `&PinnedRecords::new()`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Harness entries, gate, commit**

Add, package `geode-shell`:
- `session: a pinned lane is written under its workspace` — anchor `ws_table.insert("frame".to_string(), toml::Value::Table(record.to_toml()));` → `{}`; test `a_pinned_lane_round_trips_under_its_workspace`.
- `shell: restored pinned lanes are pinned` — anchor `                f.pin(ws);\n                let mut lane = f.view_mut(ws);` → `                let mut lane = f.view_mut(ws);`; test `a_restored_pinned_workspace_is_pinned_with_its_record`.

Run them; expected caught. Run the Global Constraints commands. Commit:

```bash
git commit -am "feat(shell): persist pinned workspace lanes in session.toml"
```

---

### Task 6: Current documentation

**Files:**
- Modify: `docs/current/shell.md` ("The shared frame", the session table at ~line 369, recovery table at ~line 398, frame restoration paragraph at ~line 409)
- Modify: `docs/current/input-and-dialogs.md` (frame dialog section)
- Modify: `crates/geode-shell/README.md` (frame module map entry)
- Modify: `TODO.md` if it is tracked in the worktree; otherwise leave it (it is the user's untracked file on main)

- [ ] **Step 1: `shell.md`**

In "The shared frame", after the first paragraph, add a subsection
"Workspace lanes" stating: the frame holds a shared lane and one lane per
pinned workspace; a lane is scope + undo/redo + text session, active slot,
as-of + previous; definitions and publications stay shared; generations come
from one counter so a number names one value in any lane (and why: a tile
switching lanes cannot mistake different content for unchanged); pin copies
values and generations with empty history; unpin discards; `replace_slots`
regroups every lane and `save_slot` the lanes on that slot; only the active
workspace's lane can open a flip barrier, and a switch re-seeds the baseline
instead of opening one; the toolbar's pin glyph and `frame::pin_workspace`
toggle it; `ShellView::target_frame` is the shell's one door (base modal's
workspace, else active). In the session table add a row
`| workspaces.N.frame | Pinned lane for workspace N (same fields as frame); present iff pinned |`.
In the recovery table add `| workspaces.N.frame is not a table | Warn; the workspace restores unpinned |`.
In the restoration paragraph say each pinned lane is pinned, filled, and has
its history cleared like the shared one.

- [ ] **Step 2: `input-and-dialogs.md`**

In the frame dialogs section add: frame dialogs read and commit the lane of
the workspace their modal stack was opened from (`ShellModal::workspace`).

- [ ] **Step 3: README and commit**

`crates/geode-shell/README.md`: the `frame.rs` line mentions lanes and views;
add `frame_ref.rs` — the workspace-bound handle modules receive; add
`shell/pin.rs` — pin toggle, switch hook, scope-field rebinding.

```bash
git commit -am "docs: workspace-pinned frame lanes"
```

- [ ] **Step 4: Final gate**

Run every Global Constraints command plus `zsh scripts/mutation-check.sh --changed`
(run detached and poll; it takes several minutes). Expected: no SURVIVED, no
BUILD, `--anchors-only` clean. Record the display check still owed: the pin
glyph at rest, hovered, and pinned, on one light and one dark theme.
