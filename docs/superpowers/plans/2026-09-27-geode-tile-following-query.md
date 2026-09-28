# geode-tile Following Query Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the three hand-written copies of the flip-barrier state machine (market-data, timeseries, blotter) and the two self-arrive observers (pricer, diagnostics) with one `geode_tile::following` module, then make hide and close behave as ruled: hiding lets a query finish, closing cancels it and answers the barrier.

**Architecture:** `FollowingQuery<T>` holds `acted`, the in-flight instant, the stage, the last flip seen and the tag. It never paints and never submits: every method takes the barrier as `&mut impl Barrier` and returns a decision (`Promotion<T>`, `Delivered<T, E>`, `bool`) that the tile acts on. `Frame` implements `Barrier` directly (pure tests); `FrameDoor` implements it over the shared `Entity<Frame>` and notifies the frame when an arrival releases the barrier. The shell gains a `TileContent::closed` hook (default no-op) called once when an occupant is removed; the shell never depends on `geode-tile`.

**Tech Stack:** Rust 2024, GPUI (`gpui-pre =0.3.5`, aliased `gpui`), `geode-shell::frame` barrier primitives, `geode-core::query::QueryKey`.

**Spec:** `docs/superpowers/specs/2026-09-27-geode-tile-following-query-design.md`

## Global Constraints

- `geode-tile` depends on `geode-shell`, `geode-core`, `gpui`, `gpui-component`; never on `geode-data` or a feature module. The helper takes `submitted: bool`; each tile formats its own refusal notice. `geode-shell` never depends on `geode-tile`.
- Tasks 2–5 are behaviour-preserving: every existing module test keeps its assertions. The only edits allowed to an existing test in those tasks are mechanical field-to-accessor rewrites (`t.tag` → `t.following.tag()`, `t.acted` → `t.following.acted()`, `t.staged.is_some()` → `t.following.is_staged()`), named in the commit message. Only Task 7 changes behaviour; the commit that rewrites a test pinning old hide behaviour names that test.
- No state mutation, I/O or unbounded allocation in render. The close hook runs inside the shell's existing occupant reconciliation, beside the existing hide-before-drop call; it sends one bounded `try_send` cancel and updates the frame, whose notification is queued as an effect (the frame is not a rendered view).
- The UI thread never waits on data work: `DataHandle::cancel` is a non-blocking `try_send`; a refused submission is reported through `submitted(false, ..)`, never retried in a loop.
- Comments state the local invariant and the failure it prevents. The helper carries the existing rulings' reasoning as comments: a failed outcome still arrives; a stale tag is not an arrival; a refusal arrives before `acted` is cleared; promotion compares followed counters, not flip identity; a new submission always clears the stage; promotion on a new flip runs before the visibility check; self-arrive never answers for a same-flip query still in flight. No task numbers, review ids or dates in code or docs.
- `docs/current/*`, `CLAUDE.md` where it lists crate contents, and every touched crate README are updated in the same task as the behaviour or structure they describe.
- Tests assert no elapsed wall-clock time. The barrier deadline is driven with explicit instants (`open_flip(keys, t0)` then `sweep(t0 + FLIP_DEADLINE)`), never sleeps.
- Mutation harness: mode flags first (`--build-check`, `--anchors-only`); never `--changed`; never an unfiltered mutation run; select one entry at a time by its full name (or a narrow unique substring); before any run, `pgrep -f 'mutation-che[c]k'` must print nothing; commit before any mutation run (the harness edits tracked files). Verify every new or re-aimed entry three ways: `zsh scripts/mutation-check.sh --build-check "<name>"` reports no BUILD; `zsh scripts/mutation-check.sh "<name>"` reports `caught` (not `caught*`, not SURVIVED); a hand application of the replacement makes the named test fail on an assertion (then `git checkout -- <file>`). Tests that could hang on a mutant must fail by assertion (use `try_recv`/`try_iter`, or the existing `next_query`, whose `recv_timeout` expectation is an assertion).
- Anchors are copied from the file **after** `cargo fmt`. If rustfmt formats a line differently from this plan, keep the formatted code and copy the formatted text into the entry; `--anchors-only` must report neither ANCHOR nor AMBIG.
- Per-task gates: the task's focused tests; `cargo test -p <crate>` for each touched crate (`cargo test -p geode-app <filter>` for app tests; geode-app is bin-only); `cargo clippy -p <crate> --all-targets -- -D warnings`; `cargo fmt --check`; `zsh scripts/mutation-check.sh --anchors-only`; `cargo check -p geode-shell --features test-support --all-targets` whenever `geode-shell` changes.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **A tab or workspace switch in the middle of a flip.** A market-data or timeseries tile enrolled in an open barrier is hidden before its reply lands. Today it cancels and forgets, so every other tile waits out 250 ms. After Task 7 the reply lands, answers the barrier, and promotes while hidden. Pinned by `a_panel_hidden_mid_flip_still_answers_the_barrier` (Task 7) and `a_stage_held_when_the_panel_hides_promotes_on_the_flip` (Task 2).
2. **A reply that lands while hidden, then a followed change before reshow.** The applied reply is stale; reshow must ask again. Pinned by `a_followed_change_while_hidden_requeries_on_reshow` (market-data and blotter, Task 7) and `an_as_of_change_while_hidden_requeries_on_reshow` (timeseries, Task 7).
3. **A late outcome for a closed tile.** The entity may still be alive when the pool's answer is delivered; it must not paint. Pinned by the close tests' final assertions (Task 7) and `close_supersedes_the_question_and_answers_the_barrier` (Task 1).
4. **Closing the tile the barrier was last waiting on.** The other tiles must promote in the same pass, which needs the frame notification `FrameDoor` sends on release. Pinned by `closing_a_tile_mid_flip_cancels_its_query_and_releases_the_barrier` (blotter, Task 7) and `a_releasing_arrival_through_the_door_notifies_the_frame` (Task 1).
5. **A workspace switch is not a close.** The shell must call `closed` only on removal. Pinned by `closing_a_tile_tells_its_occupant_it_closed_and_a_workspace_switch_does_not` (Task 6).

## Spec deviations (code wins; evidence)

1. **The helper returns decisions; it takes no apply callback, and the post-step hook is the tile's code after each call.** A callback would need `&mut` to the tile while the helper, a field of the tile, is mutably borrowed. So `on_flip`, `promote` and `deliver` return `Promotion<T>` / `Delivered<T, E>`, and timeseries calls `release_view` after every non-empty promotion and every non-stale delivery, exactly where its `promote` and `deliver` call it today (`crates/geode-timeseries/src/tile/data.rs:53`, `:285`). The helper's share of the hook is `in_flight()` and `is_staged()`, which `release_view` reads.
2. **Timeseries reshow still refetches, and the refetch's completion requeries.** `set_visible(true)` marks every source slot fetching and submits fetches (`tile/mod.rs:413-423`), and `on_fetched` requeries on success (`tile/data.rs:79-92`). The shell broadcasts `SeriesFetched` to visible tiles only (`crates/geode-shell/src/shell/occupants.rs:62-86`), so a hidden tile cannot know what it missed. After Task 7, `set_visible(true)` itself issues no series query when nothing followed moved, but a query follows the fetch completion. The new test asserts the first half, and the fetch-driven requery is unchanged data freshness.
3. **The close seam is a new `TileContent::closed` hook.** Today occupant removal (`ShellView::ensure_occupants`, `occupants.rs:171-182`; the placeholder fill in `add_tile.rs:39-41`) calls `set_visible(false)` and drops the occupant. There is no drop hook, and the shell cannot name `geode-tile`. Closing a market-data or timeseries tile cancels today only because removal goes through hide, and nothing arrives. The blotter neither cancels nor arrives. The hook defaults to a no-op; the three following tiles implement it once each over `FollowingQuery::close`.
4. **A hidden tile enrolled in an open barrier now answers it.** Task 7 improves more than the spec's "what a trader notices" lists. A market-data or timeseries tile hidden mid-flip currently cancels and holds the flip to the deadline.
5. **Diagnostics gains a `geode-tile` dependency** for `arrive_immediately`. `crates/geode-tile/README.md` and `docs/current/architecture.md:31-34` currently say it has none; both are updated in Task 5.
6. **The not-wanted `Ok` path does not arrive.** Market-data and timeseries call `arrive` after applying a result the barrier did not want (`marketdata/src/tile.rs:1141-1142`), and the blotter does not. That arrival is provably a no-op: `barrier_wants(key, acted)` false means no barrier, a different identity, or `key` already removed with others still awaited, and in each case `Frame::arrived` changes nothing. The helper follows the blotter.
7. **Market-data and timeseries hide keeps cancelling through Tasks 2–6** via a temporary `FollowingQuery::abandon`, so those tasks stay behaviour-preserving. Task 7 deletes `abandon` and its test.
8. **Blotter follow policy becomes a `Copy` value.** `differs_on_followed(&self, ..)` cannot be passed to a helper method while `self.following` is mutably borrowed. `Followed { scope, grouping, as_of }` is copied out first, and `differs_on_followed` stays as a one-line delegate (used by `set_visible` and an existing test). The harness entry "asof-pin: a pinned tile does not follow the frame's as-of" is re-aimed to the new `as_of:` field line.

## Controller rulings (2026-09-27) — override task text where they differ

1. **A blotter whose view is no longer configured answers the barrier** (arrives) instead of holding the flip to the deadline — the existing ruling "one broken tile never holds the rest open" (a failed outcome still arrives) covers it. Implemented in Task 7 with the other behaviour change, with a blotter test and a mutation entry; the docs say so.
2. **Task 1's CLAUDE.md edit (adding `following` to the geode-tile module list) is wanted.**

## File Structure

| File | Change |
|---|---|
| `crates/geode-tile/src/following.rs` | **New.** `Barrier`, `FrameDoor`, `Promotion`, `Delivered`, `Unanswered`, `FollowingQuery<T>`, `arrive_immediately`, pure tests and one GPUI test for the door. |
| `crates/geode-tile/src/lib.rs`, `README.md` | `pub mod following;`, module row, the `submitted: bool` note. |
| `crates/geode-marketdata/src/tile.rs`, `content.rs`, `README.md` | Machine moves onto `FollowingQuery<Arc<Snapshot>>`; `closed`. |
| `crates/geode-timeseries/src/tile/mod.rs`, `tile/data.rs`, `tile/tests.rs`, `content.rs`, `README.md` | Machine moves onto `FollowingQuery<SeriesResult>`; `closed`. |
| `crates/geode-blotter/src/tile.rs`, `content.rs`, `README.md` | Inline machine moves onto `FollowingQuery<(Arc<Snapshot>, Vec<String>)>`; `Followed`; `closed`. |
| `crates/geode-pricer/src/tile.rs`, `README.md` | Observer becomes `arrive_immediately`. |
| `crates/geode-diagnostics/Cargo.toml`, `src/tile.rs`, `README.md` | `geode-tile` dependency; observer tail becomes `arrive_immediately`. |
| `crates/geode-shell/src/module.rs`, `shell/occupants.rs`, `shell/add_tile.rs`, `shell/tests/occupants.rs`, `README.md` | `TileContent::closed`, `Recorded::Closed`, the two removal call sites, the shell test. |
| `docs/current/shell.md`, `docs/current/features.md`, `docs/current/architecture.md`, `CLAUDE.md` | Barrier hide/close rules, lifecycle, crate map. |
| `scripts/mutation-check.sh` | Re-aimed and new entries per task. |

**Where new harness entries go:** immediately above the last `if [[ -n "$changed_ref" ]]; then` line in `scripts/mutation-check.sh` (`grep -n 'if \[\[ -n "\$changed_ref" \]\]; then' scripts/mutation-check.sh | tail -1`). **Re-aiming an entry:** find it with `grep -n 'run_mutation "<name>"' scripts/mutation-check.sh` and replace the whole `run_mutation` block (name line through its filter line) with the block given here. Keep the comment above it, and append one comment line saying what the anchor now is, for example `# The rule now lives in geode_tile::following; this module's route stays the named test.` Never read `scripts/mutation-check.sh` whole (24k lines); use `grep -n` and `sed -n 'A,Bp'`.

---

### Task 1: The `following` helper

**Files:**
- Create: `crates/geode-tile/src/following.rs`
- Modify: `crates/geode-tile/src/lib.rs`, `crates/geode-tile/README.md`, `docs/current/architecture.md` (the `geode-tile` paragraph), `CLAUDE.md` (the `geode-tile` module list), `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `geode_shell::frame::{Frame, FrameVersions, FLIP_DEADLINE}` (`Frame::versions`, `barrier_wants`, `arrived`, `open_flip`, `sweep`, `barrier_open`, `set_text`, `note_config_reloaded`), `geode_core::query::QueryKey`.
- Produces (later tasks rely on these exact names):
  - `pub trait Barrier { fn current(&self) -> FrameVersions; fn wants(&self, key: QueryKey, versions: FrameVersions) -> bool; fn arrive(&mut self, key: QueryKey, versions: FrameVersions) -> bool; }`, implemented for `Frame`
  - `pub struct FrameDoor<'a>`; `FrameDoor::new(frame: &'a Entity<Frame>, cx: &'a mut App) -> FrameDoor<'a>`, implementing `Barrier`
  - `pub enum Promotion<T> { Empty, Superseded, Apply(T) }`
  - `pub enum Delivered<T, E> { Stale, Apply(T), Held, Failed(E) }`
  - `pub enum Unanswered { Retry, KeepActed }`
  - `pub struct FollowingQuery<T>` with `new`, `tag() -> u64`, `acted() -> Option<FrameVersions>`, `in_flight() -> bool`, `in_flight_since() -> Option<Instant>`, `is_staged() -> bool`, `on_flip(now, differs) -> Promotion<T>`, `promote(now, differs) -> Promotion<T>`, `follows_changed(now, differs) -> bool`, `self_arrive(barrier, key, now) -> bool`, `begin(versions, submitted: Instant) -> u64`, `drop_stage()`, `submitted(submitted: bool, unanswered: Unanswered, barrier, key)`, `deliver<E>(tag, result: Result<T, E>, now, differs, barrier, key) -> Delivered<T, E>`, `reset()`, `abandon()` (temporary, removed in Task 7), `close(barrier, key) -> bool`
  - `pub fn arrive_immediately(barrier: &mut impl Barrier, key: QueryKey) -> bool`
  - `differs` everywhere is `impl Fn(FrameVersions, FrameVersions) -> bool`, called as `differs(asked_or_staged, now)`.

- [ ] **Step 1: Write the module with its tests (the tests are the failing half until the module compiles)**

Create `crates/geode-tile/src/following.rs`:

```rust
//! The flip-barrier state machine every following tile runs for its own
//! query.
//!
//! A scope, grouping or as-of change opens a flip barrier
//! (`geode_shell::frame`): visible tiles stage their results until every
//! participant answers or the deadline passes, then promote together, so no
//! frame shows tiles evaluated under different global states. The frame
//! supplies the primitives; [`FollowingQuery`] is the one copy of the rules a
//! tile applies around them. The tile keeps what differs: its `QueryKey`,
//! which counters it follows (a `differs` function), how it submits, and how
//! it applies a result. Every method returns what the tile must do rather
//! than calling back into it.
//!
//! This crate never depends on `geode-data`: a tile reports whether its
//! submission went out as a `bool` and formats its own refusal notice.

use std::time::Instant;

use geode_core::query::QueryKey;
use geode_shell::frame::{Frame, FrameVersions};
use gpui::{App, Entity};

/// The flip barrier as a following query sees it. [`Frame`] implements it
/// directly, for pure tests; [`FrameDoor`] implements it over the shared
/// frame entity.
pub trait Barrier {
    /// The frame's counters now. An open barrier always carries the current
    /// flip identity (`Frame::open_flip` captures it and a later change
    /// replaces the barrier), so these are what a closing tile answers with.
    fn current(&self) -> FrameVersions;
    /// Whether an open barrier waits for `key` at `versions`.
    fn wants(&self, key: QueryKey, versions: FrameVersions) -> bool;
    /// Record `key`'s arrival for `versions`; `true` exactly when this
    /// arrival released the barrier.
    fn arrive(&mut self, key: QueryKey, versions: FrameVersions) -> bool;
}

impl Barrier for Frame {
    fn current(&self) -> FrameVersions {
        self.versions()
    }
    fn wants(&self, key: QueryKey, versions: FrameVersions) -> bool {
        self.barrier_wants(key, versions)
    }
    fn arrive(&mut self, key: QueryKey, versions: FrameVersions) -> bool {
        self.arrived(key, versions)
    }
}

/// The shared frame entity as a [`Barrier`]. An arrival that releases the
/// barrier notifies the frame, so every other tile's observer sees the
/// `flip` bump and promotes what it staged in the same pass; without the
/// notification they would sit on their stages until an unrelated frame
/// change.
pub struct FrameDoor<'a> {
    frame: &'a Entity<Frame>,
    cx: &'a mut App,
}

impl<'a> FrameDoor<'a> {
    pub fn new(frame: &'a Entity<Frame>, cx: &'a mut App) -> FrameDoor<'a> {
        FrameDoor { frame, cx }
    }
}

impl Barrier for FrameDoor<'_> {
    fn current(&self) -> FrameVersions {
        self.frame.read(self.cx).versions()
    }
    fn wants(&self, key: QueryKey, versions: FrameVersions) -> bool {
        self.frame.read(self.cx).barrier_wants(key, versions)
    }
    fn arrive(&mut self, key: QueryKey, versions: FrameVersions) -> bool {
        self.frame.update(self.cx, |frame, cx| {
            let released = frame.arrived(key, versions);
            if released {
                cx.notify();
            }
            released
        })
    }
}

/// What a flip, or an arrival that released the barrier, did with the
/// result held for it.
#[derive(Debug, PartialEq)]
pub enum Promotion<T> {
    /// Nothing was held.
    Empty,
    /// A held result was dropped: a counter the tile follows moved since it
    /// was staged, so it answers a question nobody is asking.
    Superseded,
    /// Put this on screen.
    Apply(T),
}

/// What a delivery asks of the tile.
#[derive(Debug, PartialEq)]
pub enum Delivered<T, E> {
    /// An older request's outcome: dropped, and not an arrival.
    Stale,
    /// Put this on screen now.
    Apply(T),
    /// Held behind the barrier; nothing to paint yet.
    Held,
    /// The query failed, and has already arrived. The tile keeps its last
    /// good result and shows this error.
    Failed(E),
}

/// What a submission that did not go out leaves behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unanswered {
    /// Forget the versions it was asked under, so the next frame change is
    /// a real retry (a refused or empty submission).
    Retry,
    /// Keep them: the tile's own retry is a change it follows, such as the
    /// configuration change that defines a missing named expression.
    /// Forgetting would re-run the failing request on every unrelated frame
    /// notification.
    KeepActed,
}

/// One tile's query under the flip barrier.
#[derive(Debug)]
pub struct FollowingQuery<T> {
    /// The frame versions the last submission was made under; `None` before
    /// the first and after a refusal. The whole `FrameVersions`, though a
    /// tile follows only some counters: the barrier is keyed by flip
    /// identity, so answering it needs the versions the request was made
    /// under.
    acted: Option<FrameVersions>,
    /// When the unanswered submission went out; `None` once answered.
    in_flight: Option<Instant>,
    /// A result held for the barrier, with the versions it answers.
    staged: Option<(T, FrameVersions)>,
    /// `flip` as of the last promotion attempt. Starts at the frame's own
    /// seed: a first pass over an already-flipped frame promotes, which is a
    /// no-op with nothing staged.
    last_flip: u64,
    tag: u64,
}

impl<T> Default for FollowingQuery<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> FollowingQuery<T> {
    pub fn new() -> FollowingQuery<T> {
        FollowingQuery {
            acted: None,
            in_flight: None,
            staged: None,
            last_flip: 0,
            tag: 0,
        }
    }

    /// The latest request's tag; an outcome under any other is stale.
    pub fn tag(&self) -> u64 {
        self.tag
    }

    pub fn acted(&self) -> Option<FrameVersions> {
        self.acted
    }

    pub fn in_flight(&self) -> bool {
        self.in_flight.is_some()
    }

    pub fn in_flight_since(&self) -> Option<Instant> {
        self.in_flight
    }

    pub fn is_staged(&self) -> bool {
        self.staged.is_some()
    }

    /// The tile's frame observer calls this first, before its visibility
    /// check: a tile hidden after staging must still land its answer when
    /// the flip releases it. Promotes once per flip.
    pub fn on_flip(
        &mut self,
        now: FrameVersions,
        differs: impl Fn(FrameVersions, FrameVersions) -> bool,
    ) -> Promotion<T> {
        if now.flip == self.last_flip {
            return Promotion::Empty;
        }
        self.last_flip = now.flip;
        self.promote(now, differs)
    }

    /// Take the held result and apply it only if nothing the tile follows
    /// moved since it was staged. Followed counters, not flip identity: a
    /// barrier replaced by a change the tile does not follow (a scope
    /// keystroke under a document panel) must not discard the only answer
    /// to the tile's current question.
    pub fn promote(
        &mut self,
        now: FrameVersions,
        differs: impl Fn(FrameVersions, FrameVersions) -> bool,
    ) -> Promotion<T> {
        let Some((result, staged_under)) = self.staged.take() else {
            return Promotion::Empty;
        };
        if differs(staged_under, now) {
            return Promotion::Superseded;
        }
        Promotion::Apply(result)
    }

    /// Whether a counter the tile follows moved since it last asked;
    /// nothing asked yet is always a change.
    pub fn follows_changed(
        &self,
        now: FrameVersions,
        differs: impl Fn(FrameVersions, FrameVersions) -> bool,
    ) -> bool {
        match self.acted {
            None => true,
            Some(asked) => differs(asked, now),
        }
    }

    /// Answer a barrier this tile needs no query for. The shell enrolls every
    /// visible occupant, so silence would hold the other tiles to the
    /// deadline. A query still out under this same flip identity is the
    /// answer, so an unrelated notification must not arrive in its place.
    pub fn self_arrive(
        &self,
        barrier: &mut impl Barrier,
        key: QueryKey,
        now: FrameVersions,
    ) -> bool {
        let answering_now = self.in_flight.is_some()
            && self.acted.is_some_and(|asked| asked.same_flip_identity(now));
        if answering_now || !barrier.wants(key, now) {
            return false;
        }
        barrier.arrive(key, now)
    }

    /// Record a submission made under `versions` and return its tag. The
    /// stage is dropped first: a new question supersedes whatever was held
    /// for the old one even when no frame counter moved (a tile-local
    /// filter, a key change), which promotion's own check cannot see.
    pub fn begin(&mut self, versions: FrameVersions, submitted: Instant) -> u64 {
        self.staged = None;
        self.tag += 1;
        self.acted = Some(versions);
        self.in_flight = Some(submitted);
        self.tag
    }

    /// Drop the stage without submitting (a tile that cannot build its
    /// request this time).
    pub fn drop_stage(&mut self) {
        self.staged = None;
    }

    /// Report whether the submission `begin` recorded went out.
    pub fn submitted(
        &mut self,
        submitted: bool,
        unanswered: Unanswered,
        barrier: &mut impl Barrier,
        key: QueryKey,
    ) {
        if submitted {
            return;
        }
        // No outcome will come for a request that never went out. Arrive
        // under the versions it was made under before forgetting them —
        // arrival reads them — or every other tile waits out the deadline.
        if let Some(asked) = self.acted {
            barrier.arrive(key, asked);
        }
        self.in_flight = None;
        if unanswered == Unanswered::Retry {
            self.acted = None;
        }
    }

    /// Route one outcome. `now` is the tile's current view of the frame
    /// (its own publication watches included), used if this arrival
    /// releases the barrier and the result promotes at once.
    pub fn deliver<E>(
        &mut self,
        tag: u64,
        result: Result<T, E>,
        now: FrameVersions,
        differs: impl Fn(FrameVersions, FrameVersions) -> bool,
        barrier: &mut impl Barrier,
        key: QueryKey,
    ) -> Delivered<T, E> {
        if tag != self.tag {
            // Stale, and deliberately not an arrival: the barrier waits for
            // the versions the newer request was made under, and that
            // request's own outcome answers it.
            return Delivered::Stale;
        }
        self.in_flight = None;
        let asked = self.acted;
        match result {
            Ok(value) => {
                // Held while the barrier waits for this key at the request's
                // versions, so this result and every other tile's promote
                // together.
                let Some(held_under) = asked.filter(|&under| barrier.wants(key, under)) else {
                    return Delivered::Apply(value);
                };
                self.staged = Some((value, held_under));
                // This arrival may be what empties the barrier; promote at
                // once rather than waiting for the flip to reach the tile's
                // observer on a later pass.
                if !barrier.arrive(key, held_under) {
                    return Delivered::Held;
                }
                match self.promote(now, differs) {
                    Promotion::Apply(value) => Delivered::Apply(value),
                    Promotion::Empty | Promotion::Superseded => Delivered::Held,
                }
            }
            Err(error) => {
                // A failure arrives too: one broken tile must never hold
                // every other tile open until the deadline.
                if let Some(under) = asked {
                    barrier.arrive(key, under);
                }
                Delivered::Failed(error)
            }
        }
    }

    /// The tile now asks a different question (market-data's key change):
    /// drop the stage, forget what was asked, and advance the tag even
    /// while hidden, so the old question's late answer is stale.
    pub fn reset(&mut self) {
        self.staged = None;
        self.acted = None;
        self.in_flight = None;
        self.tag += 1;
    }

    /// The tile cancelled its outstanding request on hide: nothing will
    /// answer it, so the next show must ask again. The stage and the tag
    /// are kept, so a result already held still promotes on its flip.
    pub fn abandon(&mut self) {
        self.acted = None;
        self.in_flight = None;
    }

    /// The tile is being removed (the caller has already cancelled its
    /// request by key). Supersede everything, so a late outcome is stale,
    /// then answer any open barrier still waiting on this key: a tile that
    /// no longer exists must not hold a flip to its deadline. `true` when
    /// this released the barrier.
    pub fn close(&mut self, barrier: &mut impl Barrier, key: QueryKey) -> bool {
        self.tag += 1;
        self.in_flight = None;
        self.staged = None;
        self.acted = None;
        let closing_under = barrier.current();
        if !barrier.wants(key, closing_under) {
            return false;
        }
        barrier.arrive(key, closing_under)
    }
}

/// A tile that submits no frame-dependent query answers every barrier that
/// enrolls it at once; otherwise the following tiles wait out the deadline
/// for an answer that never comes.
pub fn arrive_immediately(barrier: &mut impl Barrier, key: QueryKey) -> bool {
    let now = barrier.current();
    barrier.wants(key, now) && barrier.arrive(key, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::groupings::GroupingSlots;
    use geode_core::scopes::SavedScopes;
    use geode_shell::frame::FLIP_DEADLINE;
    use gpui::AppContext as _;
    use std::cell::Cell;
    use std::rc::Rc;

    const K: QueryKey = QueryKey(7);
    const OTHER: QueryKey = QueryKey(8);

    fn fresh_frame() -> Frame {
        Frame::new(GroupingSlots::default(), SavedScopes::new(), None)
    }

    /// A scope change and the barrier the shell opens for it over `keys`,
    /// opened at `at`; the versions it carries.
    fn flip_scope(f: &mut Frame, text: &str, keys: &[QueryKey], at: Instant) -> FrameVersions {
        assert!(f.set_text(Some(text.into())), "a real scope change");
        f.open_flip(keys.iter().copied(), at);
        f.versions()
    }

    /// Follows configuration only, so a scope change is one it ignores.
    fn follows_config(a: FrameVersions, b: FrameVersions) -> bool {
        a.config != b.config
    }

    #[test]
    fn a_stale_tag_is_not_an_arrival() {
        let mut f = fresh_frame();
        let t0 = Instant::now();
        let v = flip_scope(&mut f, "a", &[K, OTHER], t0);
        let mut q = FollowingQuery::<u32>::new();
        let old = q.begin(v, t0);
        q.begin(v, t0);
        assert_eq!(
            q.deliver::<&str>(old, Ok(1), v, follows_config, &mut f, K),
            Delivered::Stale
        );
        assert!(
            f.barrier_wants(K, v),
            "a superseded answer must not stand in for the newer one"
        );
        assert!(q.in_flight(), "and the newer question is still out");
    }

    #[test]
    fn a_failed_outcome_still_arrives() {
        let mut f = fresh_frame();
        let t0 = Instant::now();
        let v = flip_scope(&mut f, "a", &[K], t0);
        let mut q = FollowingQuery::<u32>::new();
        let tag = q.begin(v, t0);
        assert_eq!(
            q.deliver(tag, Err("boom"), v, follows_config, &mut f, K),
            Delivered::Failed("boom")
        );
        assert!(!f.barrier_open(), "one broken tile never holds the rest open");
        assert!(!q.in_flight());
    }

    #[test]
    fn a_refusal_arrives_under_what_it_asked_then_forgets_it() {
        let mut f = fresh_frame();
        let t0 = Instant::now();
        let v = flip_scope(&mut f, "a", &[K, OTHER], t0);
        let mut q = FollowingQuery::<u32>::new();
        q.begin(v, t0);
        q.submitted(false, Unanswered::Retry, &mut f, K);
        assert!(!f.barrier_wants(K, v), "nothing is coming, so it answered at once");
        assert!(f.barrier_open(), "for itself only");
        assert_eq!(q.acted(), None, "forgotten, so the next change retries");
        assert!(!q.in_flight());
    }

    #[test]
    fn keep_acted_arrives_and_remembers_what_it_answered() {
        let mut f = fresh_frame();
        let t0 = Instant::now();
        let v = flip_scope(&mut f, "a", &[K, OTHER], t0);
        let mut q = FollowingQuery::<u32>::new();
        q.begin(v, t0);
        q.submitted(false, Unanswered::KeepActed, &mut f, K);
        assert!(!f.barrier_wants(K, v), "it answered the barrier");
        assert_eq!(q.acted(), Some(v), "and kept what it acted on");
        assert!(!q.in_flight());
        assert!(
            !q.follows_changed(v, follows_config),
            "so an unrelated notification is not a retry"
        );
    }

    #[test]
    fn a_submission_that_went_out_changes_nothing() {
        let mut f = fresh_frame();
        let t0 = Instant::now();
        let v = flip_scope(&mut f, "a", &[K], t0);
        let mut q = FollowingQuery::<u32>::new();
        q.begin(v, t0);
        q.submitted(true, Unanswered::Retry, &mut f, K);
        assert!(f.barrier_wants(K, v), "its outcome is the answer");
        assert_eq!(q.acted(), Some(v));
        assert_eq!(q.in_flight_since(), Some(t0));
    }

    /// Also the helper's half of timeseries' post-step hook: a held result
    /// reads as staged, the query as answered, until the promotion.
    #[test]
    fn a_held_result_promotes_on_the_flip_and_only_once() {
        let mut f = fresh_frame();
        let t0 = Instant::now();
        let v = flip_scope(&mut f, "a", &[K, OTHER], t0);
        let mut q = FollowingQuery::<u32>::new();
        let tag = q.begin(v, t0);
        assert_eq!(
            q.deliver::<&str>(tag, Ok(5), v, follows_config, &mut f, K),
            Delivered::Held
        );
        assert!(q.is_staged(), "held for the barrier");
        assert!(!q.in_flight(), "answered");
        assert!(f.arrived(OTHER, v), "the other tile's answer releases it");
        let now = f.versions();
        assert_eq!(q.on_flip(now, follows_config), Promotion::Apply(5));
        assert!(!q.is_staged());
        assert_eq!(
            q.on_flip(now, follows_config),
            Promotion::Empty,
            "one flip, one promotion"
        );
    }

    #[test]
    fn a_delivery_that_releases_the_barrier_promotes_at_once() {
        let mut f = fresh_frame();
        let t0 = Instant::now();
        let v = flip_scope(&mut f, "a", &[K], t0);
        let mut q = FollowingQuery::<u32>::new();
        let tag = q.begin(v, t0);
        assert_eq!(
            q.deliver::<&str>(tag, Ok(3), v, follows_config, &mut f, K),
            Delivered::Apply(3)
        );
        assert!(!f.barrier_open());
        assert!(!q.is_staged(), "nothing left held");
    }

    #[test]
    fn a_delivery_with_no_barrier_applies() {
        let mut f = fresh_frame();
        let v = f.versions();
        let mut q = FollowingQuery::<u32>::new();
        let tag = q.begin(v, Instant::now());
        assert_eq!(
            q.deliver::<&str>(tag, Ok(2), v, follows_config, &mut f, K),
            Delivered::Apply(2)
        );
    }

    #[test]
    fn a_stage_survives_a_barrier_replaced_by_a_change_it_does_not_follow() {
        let mut f = fresh_frame();
        let t0 = Instant::now();
        let v1 = flip_scope(&mut f, "a", &[K, OTHER], t0);
        let mut q = FollowingQuery::<u32>::new();
        let tag = q.begin(v1, t0);
        assert_eq!(
            q.deliver::<&str>(tag, Ok(1), v1, follows_config, &mut f, K),
            Delivered::Held
        );
        // A second scope change replaces the barrier; this query follows
        // configuration, not scope.
        flip_scope(&mut f, "b", &[K, OTHER], t0);
        assert!(f.sweep(t0 + FLIP_DEADLINE), "the deadline releases the replacement");
        assert_eq!(q.on_flip(f.versions(), follows_config), Promotion::Apply(1));
    }

    #[test]
    fn a_stage_is_dropped_once_a_counter_it_follows_moved() {
        let mut f = fresh_frame();
        let t0 = Instant::now();
        let v = flip_scope(&mut f, "a", &[K, OTHER], t0);
        let mut q = FollowingQuery::<u32>::new();
        let tag = q.begin(v, t0);
        q.deliver::<&str>(tag, Ok(1), v, follows_config, &mut f, K);
        f.note_config_reloaded();
        assert!(f.arrived(OTHER, v), "configuration is not part of the flip identity");
        assert_eq!(
            q.on_flip(f.versions(), follows_config),
            Promotion::Superseded
        );
    }

    #[test]
    fn self_arrive_waits_for_a_same_identity_query_in_flight() {
        let mut f = fresh_frame();
        let t0 = Instant::now();
        let mut older = FollowingQuery::<u32>::new();
        older.begin(f.versions(), t0);
        let v = flip_scope(&mut f, "a", &[K, OTHER, QueryKey(9)], t0);
        let mut q = FollowingQuery::<u32>::new();
        q.begin(v, t0);
        assert!(!q.self_arrive(&mut f, K, v));
        assert!(
            f.barrier_wants(K, v),
            "its own outcome answers this barrier, not an unrelated notification"
        );
        assert!(!older.self_arrive(&mut f, OTHER, v));
        assert!(
            !f.barrier_wants(OTHER, v),
            "a query out under an older identity is no answer to this one"
        );
        let idle = FollowingQuery::<u32>::new();
        assert!(!idle.self_arrive(&mut f, QueryKey(9), v));
        assert!(!f.barrier_wants(QueryKey(9), v), "an idle tile answers at once");
    }

    #[test]
    fn follows_changed_is_true_before_the_first_question() {
        let mut f = fresh_frame();
        let mut q = FollowingQuery::<u32>::new();
        assert!(q.follows_changed(f.versions(), follows_config));
        q.begin(f.versions(), Instant::now());
        assert!(!q.follows_changed(f.versions(), follows_config));
        f.note_config_reloaded();
        assert!(q.follows_changed(f.versions(), follows_config));
    }

    #[test]
    fn reset_forgets_the_question_and_its_stage() {
        let mut f = fresh_frame();
        let t0 = Instant::now();
        let v = flip_scope(&mut f, "a", &[K, OTHER], t0);
        let mut q = FollowingQuery::<u32>::new();
        let tag = q.begin(v, t0);
        q.deliver::<&str>(tag, Ok(1), v, follows_config, &mut f, K);
        let tag = q.begin(v, t0);
        q.reset();
        assert!(!q.is_staged());
        assert_eq!(q.acted(), None);
        assert!(!q.in_flight());
        assert_eq!(
            q.deliver::<&str>(tag, Ok(9), v, follows_config, &mut f, K),
            Delivered::Stale,
            "the old question's answer cannot land under the new one"
        );
    }

    #[test]
    fn abandon_forgets_the_question_and_keeps_the_stage() {
        let mut f = fresh_frame();
        let t0 = Instant::now();
        let v = flip_scope(&mut f, "a", &[K, OTHER], t0);
        let mut q = FollowingQuery::<u32>::new();
        let tag = q.begin(v, t0);
        q.deliver::<&str>(tag, Ok(1), v, follows_config, &mut f, K);
        q.abandon();
        assert_eq!(q.acted(), None);
        assert!(!q.in_flight());
        assert!(q.is_staged());
        assert_eq!(q.tag(), tag);
    }

    #[test]
    fn close_supersedes_the_question_and_answers_the_barrier() {
        let mut f = fresh_frame();
        let t0 = Instant::now();
        let v = flip_scope(&mut f, "a", &[K, OTHER], t0);
        let mut q = FollowingQuery::<u32>::new();
        let tag = q.begin(v, t0);
        assert!(!q.close(&mut f, K), "the other tile still holds the barrier");
        assert!(
            !f.barrier_wants(K, v),
            "a closed tile never holds a flip to its deadline"
        );
        assert!(f.barrier_open());
        assert!(!q.in_flight());
        assert_eq!(
            q.deliver::<&str>(tag, Ok(1), v, follows_config, &mut f, K),
            Delivered::Stale,
            "a late outcome answers a tile that is gone"
        );
    }

    #[test]
    fn close_releases_a_barrier_it_was_the_last_wait_of() {
        let mut f = fresh_frame();
        let t0 = Instant::now();
        let v = flip_scope(&mut f, "a", &[K], t0);
        let mut q = FollowingQuery::<u32>::new();
        q.begin(v, t0);
        assert!(q.close(&mut f, K));
        assert!(!f.barrier_open());
    }

    #[test]
    fn close_with_nothing_open_changes_nothing() {
        let mut f = fresh_frame();
        let before = f.versions();
        let mut q = FollowingQuery::<u32>::new();
        assert!(!q.close(&mut f, K));
        assert_eq!(f.versions(), before);
    }

    #[test]
    fn arrive_immediately_answers_only_a_barrier_that_wants_the_key() {
        let mut f = fresh_frame();
        assert!(!arrive_immediately(&mut f, K), "nothing open");
        let v = flip_scope(&mut f, "a", &[K, OTHER], Instant::now());
        assert!(!arrive_immediately(&mut f, K));
        assert!(!f.barrier_wants(K, v));
        assert!(arrive_immediately(&mut f, OTHER), "the last wait releases");
        assert!(!f.barrier_open());
    }

    #[gpui::test]
    fn a_releasing_arrival_through_the_door_notifies_the_frame(cx: &mut gpui::TestAppContext) {
        let frame = cx.update(|cx| cx.new(|_| fresh_frame()));
        let heard = Rc::new(Cell::new(0u32));
        let seen = heard.clone();
        let _watch = cx.update(|cx| cx.observe(&frame, move |_, _| seen.set(seen.get() + 1)));
        let t0 = Instant::now();
        let v = frame.update(cx, |f, _| flip_scope(f, "a", &[K, OTHER], t0));
        cx.update(|cx| {
            let mut door = FrameDoor::new(&frame, cx);
            assert!(!door.arrive(K, v), "the other tile still holds it");
        });
        cx.run_until_parked();
        assert_eq!(heard.get(), 0, "an arrival that releases nothing is not news");
        cx.update(|cx| {
            let mut door = FrameDoor::new(&frame, cx);
            assert!(door.wants(OTHER, v));
            assert!(door.arrive(OTHER, v));
        });
        cx.run_until_parked();
        assert_eq!(
            heard.get(),
            1,
            "a release notifies, so every staged tile promotes in the same pass"
        );
    }
}
```

In `crates/geode-tile/src/lib.rs`, add `pub mod following;` between `pub mod confirm;` and `pub mod menu;`.

If the compiler rejects `self.frame.read(self.cx)` (a `&mut App` behind `&self`), write `self.frame.read(&*self.cx)` in both reads.

- [ ] **Step 2: Run the helper tests; they compile and pass**

Run: `cargo test -p geode-tile following`
Expected: 17 tests pass (`a_stale_tag_is_not_an_arrival` … `a_releasing_arrival_through_the_door_notifies_the_frame`).

To see a test fail first, hand-apply one mutation from Step 4 (for example delete `self.tag += 1;` from `close`) and run `cargo test -p geode-tile close_supersedes`. Expected: FAIL on the `Delivered::Stale` assertion. Then `git checkout -- crates/geode-tile/src/following.rs` and re-apply Step 1, or undo the edit by hand.

- [ ] **Step 3: Documentation**

`crates/geode-tile/README.md`: add this row to the module table, first in alphabetical order after `confirm`:

```markdown
| `following` | The flip-barrier state machine a following tile runs for its own query: `FollowingQuery<T>` (the versions last asked under, the in-flight instant, the result held for the barrier, the last flip seen, the tag) with `on_flip`/`promote` (followed counters decide, not flip identity; promotion runs before the tile's visibility check), `follows_changed`, `self_arrive` (never for a same-identity query still out), `begin` (drops the stage), `submitted` (`Unanswered::Retry` arrives then forgets; `KeepActed` arrives and remembers), `deliver` (a stale tag is not an arrival; a failure arrives; a releasing arrival promotes at once), `reset`, `close` (supersede, then answer any barrier still waiting); `arrive_immediately` for tiles that submit no frame query; the `Barrier` trait over `Frame` and `FrameDoor` (the entity, notifying on release). Methods return decisions; the tile applies results and formats notices. |
```

Replace the paragraph "Used by the pricer (all four doors), …" with:

```markdown
Used by the pricer (all four doors, and `following::arrive_immediately`),
market-data (all four, and `following`), timeseries (popover, menu, notice,
`following`), the blotter (notice, `following`) and diagnostics
(`following::arrive_immediately`).

The crate takes a submission's outcome as `submitted: bool` rather than
depending on `geode-data`: every refusal path uses the data service's
`Refusal` only to word a notice, which the tile writes itself.
```

`docs/current/architecture.md`, the paragraph beginning "`geode-tile` is the kit tiles are built from": after "the in-tile y/n confirm and the notice line, as models with one painter each" insert ", and the `following` flip-barrier state machine every following tile runs for its own query". Diagnostics' dependency sentence is changed in Task 5.

`CLAUDE.md`, the bullet "A tile mechanism two modules would otherwise each write lives in `geode-tile`": change the tail to "popups (`popover`), `.` action menus (`menu`), the in-tile y/n confirm (`confirm`), notices (`notice`) and the flip-barrier state machine (`following`)." (Owner question 2.)

- [ ] **Step 4: Harness entries**

Insert above the last `if [[ -n "$changed_ref" ]]; then`:

```zsh
# geode_tile::following carries the barrier rules for every following tile.
# These entries name its own pure tests; each module's re-aimed entries name
# the module route.
run_mutation "following: KeepActed remembers what a local refusal answered" \
  crates/geode-tile/src/following.rs \
  '        if unanswered == Unanswered::Retry {' \
  '        if true {' \
  geode-tile keep_acted_arrives_and_remembers_what_it_answered

run_mutation "following: a closing tile answers the barrier" \
  crates/geode-tile/src/following.rs \
  '        barrier.arrive(key, closing_under)' \
  '        false' \
  geode-tile close_releases_a_barrier_it_was_the_last_wait_of

run_mutation "following: a closing tile supersedes its tag" \
  crates/geode-tile/src/following.rs \
  '    pub fn close(&mut self, barrier: &mut impl Barrier, key: QueryKey) -> bool {
        self.tag += 1;' \
  '    pub fn close(&mut self, barrier: &mut impl Barrier, key: QueryKey) -> bool {' \
  geode-tile close_supersedes_the_question_and_answers_the_barrier

run_mutation "following: arrive_immediately answers" \
  crates/geode-tile/src/following.rs \
  '    barrier.wants(key, now) && barrier.arrive(key, now)' \
  '    barrier.wants(key, now) && false' \
  geode-tile arrive_immediately_answers_only_a_barrier_that_wants_the_key

run_mutation "following: a releasing arrival notifies the frame" \
  crates/geode-tile/src/following.rs \
  '            if released {
                cx.notify();
            }' \
  '            let _ = &cx;' \
  geode-tile a_releasing_arrival_through_the_door_notifies_the_frame
```

- [ ] **Step 5: Gates**

```sh
cargo test -p geode-tile
cargo clippy -p geode-tile --all-targets -- -D warnings
cargo fmt --check
zsh scripts/mutation-check.sh --anchors-only
```
Expected: all pass; the anchor check prints no ANCHOR/AMBIG line and exits 0.

- [ ] **Step 6: Commit**

```bash
git add crates/geode-tile docs/current/architecture.md CLAUDE.md scripts/mutation-check.sh
git commit -m "feat(tile): following — the shared flip-barrier state machine

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 7: Verify the five entries three ways**

```sh
pgrep -f 'mutation-che[c]k'   # must print nothing
for n in "following: KeepActed" "following: a closing tile answers" "following: a closing tile supersedes" "following: arrive_immediately answers" "following: a releasing arrival notifies"; do
  zsh scripts/mutation-check.sh --build-check "$n"
  zsh scripts/mutation-check.sh "$n"
done
```
Expected: no BUILD; each reports `caught`. Then hand-apply each replacement in turn, run its named test (`cargo test -p geode-tile <test>`), confirm it fails on an assertion (not a panic elsewhere or a compile error), and `git checkout -- crates/geode-tile/src/following.rs`.

---

### Task 2: Market-data onto the helper

**Files:**
- Modify: `crates/geode-marketdata/src/tile.rs` (imports; struct fields ~475-574; constructor ~868-905; frame observer ~787-809; `follows_changed`…`requery`…`deliver` ~998-1160; `promote` ~1700-1713; `set_visible` ~1715-1738; `set_key` ~4270-4285; `acted_is_none` ~4452; tests), `crates/geode-marketdata/README.md`, `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: Task 1's `FollowingQuery`, `FrameDoor`, `Delivered`, `Promotion`, `Unanswered`.
- Produces: `MarketDataTile.following: FollowingQuery<Arc<Snapshot>>` (private); `MarketDataTile::differs_on_followed(FrameVersions, FrameVersions) -> bool` unchanged. Task 7 adds `closed`.

- [ ] **Step 1: Write the new test (it passes on current code; it pins rule 1's order across the refactor)**

Add after `a_key_change_drops_what_was_staged_for_the_old_key` in the `tile.rs` test module:

```rust
    /// A flip promotes before the visibility check: a panel hidden after it
    /// staged still lands its answer when the barrier releases, rather than
    /// holding a stage nobody promotes until it is shown again.
    #[gpui::test]
    fn a_stage_held_when_the_panel_hides_promotes_on_the_flip(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, first, Arc::new(cvi(BASE)));

        let other = QueryKey(TILE + 1);
        open_barrier_on_as_of(&h, &mut vcx, &[QueryKey(TILE), other], 60);
        let second = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            second,
            Arc::new(document_of(&["t0", "t1", "t2", "t3", "t4"], &NODES, BASE)),
        );
        assert_eq!(h.rows(&vcx), 2, "staged behind the other tile");

        h.visible(&mut vcx, false);
        let now = h.versions(&vcx);
        h.frame.update(&mut vcx, |f, cx| {
            assert!(f.arrived(other, now), "the other tile's answer releases it");
            cx.notify();
        });
        assert_eq!(
            h.rows(&vcx),
            5,
            "a hidden panel still promotes what it staged on the flip"
        );
    }
```

Run: `cargo test -p geode-marketdata a_stage_held_when_the_panel_hides_promotes_on_the_flip`
Expected: PASS (current behaviour, pinned before the move).

- [ ] **Step 2: Move the state onto the helper**

Imports: add `use geode_tile::following::{Delivered, FollowingQuery, FrameDoor, Promotion, Unanswered};` beside the other `geode_tile` imports.

Struct: delete the fields `tag: u64,`, `acted: Option<FrameVersions>,` with its doc comment ("The frame versions the last request was made under…"), `query_in_flight: bool,`, `staged: Option<(Arc<Snapshot>, FrameVersions)>,` with its doc comment, and `last_flip: u64,` with its doc comment. Add in the place of `tag`:

```rust
    /// This panel's document request under the flip barrier (see
    /// `geode_tile::following`): the versions it last asked under (whole,
    /// though only `as_of` and `data` decide a requery; the barrier is keyed
    /// by flip identity), whether it is out, the answer held for the barrier,
    /// the last flip seen, and the request tag.
    following: FollowingQuery<Arc<Snapshot>>,
```

Constructor: delete `tag: 0,`, `acted: None,`, `query_in_flight: false,`, `staged: None,`, `last_flip: 0,`; add `following: FollowingQuery::new(),`.

Frame observer (`cx.observe(&frame, |this, _frame, cx| { … })`, ~787): replace its body with:

```rust
            // Promote before the visibility check, so a panel hidden after
            // staging still lands its answer. A flip releases prepared
            // results; it never triggers a document query.
            let now = this.versions(cx);
            let promoted = this.following.on_flip(now, Self::differs_on_followed);
            if let Promotion::Apply(snapshot) = promoted {
                this.apply(snapshot, cx);
                this.changed(cx);
            }
            if !this.visible {
                return;
            }
            // Only `as_of` and `data` are followed (see the module doc);
            // a scope keystroke bumps `scope` on every character and must
            // not cost this panel a requery.
            if this.key.is_some() && this.following.follows_changed(now, Self::differs_on_followed)
            {
                // The barrier is answered on delivery instead, with the
                // versions this request was made under.
                this.requery(cx);
            } else {
                let key = QueryKey(this.id.0);
                this.following.self_arrive(&mut FrameDoor::new(&this.frame, cx), key, now);
            }
```

Delete the methods `follows_changed`, `self_arrive`, `arrive`, `arrive_and_release` and `promote`. Keep `versions` and `differs_on_followed` with their doc comments.

Replace `requery`'s body from `// A fresh question always supersedes…` to the end of the method with:

```rust
        let (as_of, versions) = {
            let frame = self.frame.read(cx);
            (frame.as_of().clone(), frame.versions_for(&self.publication))
        };
        // `begin` also drops whatever was staged for the previous question.
        let submitted = Instant::now();
        let tag = self.following.begin(versions, submitted);
        let key = QueryKey(self.id.0);
        let queued = self.data.document(DocumentParams {
            key,
            tag,
            submitted,
            dataset: self.spec.dataset.to_string(),
            document_key,
            as_of,
        });
        if let Err(refusal) = &queued {
            self.notice = Some(format!("document request refused: {refusal}").into());
        }
        self.following.submitted(
            queued.is_ok(),
            Unanswered::Retry,
            &mut FrameDoor::new(&self.frame, cx),
            key,
        );
        self.changed(cx);
```

Replace `deliver` (the document one, `pub fn deliver(&mut self, outcome: QueryOutcome, …)`) with:

```rust
    pub fn deliver(&mut self, outcome: QueryOutcome, cx: &mut Context<Self>) {
        let now = self.versions(cx);
        let key = QueryKey(self.id.0);
        let delivered = self.following.deliver(
            outcome.tag,
            outcome.snapshot,
            now,
            Self::differs_on_followed,
            &mut FrameDoor::new(&self.frame, cx),
            key,
        );
        match delivered {
            // A newer request is out; its own outcome answers the barrier.
            Delivered::Stale => return,
            // `apply` clears notices only when it paints the delivery, before
            // it writes any new restore, policy or validation notice.
            Delivered::Apply(snapshot) => self.apply(snapshot, cx),
            Delivered::Held => {}
            // Last good stays on screen: a failed select says nothing about
            // the document already painted.
            Delivered::Failed(e) => self.notice = Some(e.into()),
        }
        self.changed(cx);
    }
```

`set_visible`: in the visible branch replace `self.follows_changed(now)` with `self.following.follows_changed(now, Self::differs_on_followed)`. Replace the hide branch's three lines after the cancel (`// Clear acted…`, `self.acted = None;`, `self.query_in_flight = false;`) with:

```rust
            // Forget the cancelled request so showing the tile cannot treat
            // an undelivered request as current.
            self.following.abandon();
```

`set_key`: replace the block from `// Including anything STAGED for the old key:` through `self.tag += 1;` with:

```rust
        // A different document is a different question: drop what was held
        // for the old key (a key change bumps no frame counter, so a later
        // flip would otherwise promote it under the new key's header),
        // forget what was asked, and advance the tag even while hidden so
        // the old key's late delivery cannot enter the restored draft.
        self.following.reset();
        self.publication = None;
```

`acted_is_none`: body becomes `self.following.acted().is_none()`.

Tests: `sed -i '' 's/|t, _| t\.tag)/|t, _| t.following.tag())/g' crates/geode-marketdata/src/tile.rs` (the pattern occurs only in tests).

Check nothing is left: `grep -n 'self\.tag\b\|self\.acted\|query_in_flight\|self\.staged\|last_flip\|arrive_and_release\|self\.self_arrive' crates/geode-marketdata/src/tile.rs` prints nothing.

- [ ] **Step 3: Run the barrier tests unchanged**

```sh
cargo test -p geode-marketdata -- a_stale_tag_is_dropped an_error_outcome_keeps_the_last_model a_delivery_under_an_open_barrier_is_staged_until_the_flip a_key_change_drops_what_was_staged_for_the_old_key a_delivery_that_releases_the_barrier_promotes_at_once a_tile_hidden_mid_flight_requeries_on_reshow a_busy_document_refusal_says_busy a_refused_request_arrives_at_the_barrier_and_retries a_panel_self_arrives_on_a_scope_change a_panel_arrives_on_delivery_after_an_as_of_change a_failed_delivery_still_arrives a_stage_survives_a_barrier_replaced a_stage_is_dropped_when_a_counter_the_panel_follows_has_moved unrelated_documents_cannot_release a_stage_held_when_the_panel_hides_promotes_on_the_flip
cargo test -p geode-marketdata
```
Expected: all pass.

- [ ] **Step 4: README**

`crates/geode-marketdata/README.md`, the `tile` row: replace "stages under the barrier" with "runs its document request through `geode_tile::following` (following `as_of` and its watched document's data)".

- [ ] **Step 5: Re-aim harness entries**

Replace each named block (keep its comment and add one line saying the rule now lives in `geode_tile::following` and this module's route stays the named test):

```zsh
run_mutation "publication routing: a document query must really arrive" \
  crates/geode-tile/src/following.rs \
  '        if answering_now || !barrier.wants(key, now) {' \
  '        if !barrier.wants(key, now) {' \
  geode-marketdata unrelated_documents_cannot_release_a_barrier_or_discard_a_valid_stage

run_mutation "publication routing: panel promotion uses its own dependencies" \
  crates/geode-marketdata/src/tile.rs \
  '            let now = this.versions(cx);
            let promoted = this.following.on_flip(now, Self::differs_on_followed);' \
  '            let now = this.frame.read(cx).versions();
            let promoted = this.following.on_flip(now, Self::differs_on_followed);' \
  geode-marketdata unrelated_documents_cannot_release_a_barrier_or_discard_a_valid_stage

run_mutation "mdtile: a stale tag is dropped" \
  crates/geode-tile/src/following.rs \
  '        if tag != self.tag {' \
  '        if false {' \
  geode-marketdata \
  a_stale_tag_is_dropped

run_mutation "mdtile: a panel self-arrives on a change it does not requery for" \
  crates/geode-marketdata/src/tile.rs \
  '                this.following.self_arrive(&mut FrameDoor::new(&this.frame, cx), key, now);' \
  '                let _ = (key, now);' \
  geode-marketdata \
  a_panel_self_arrives_on_a_scope_change_it_does_not_requery_for

run_mutation "mdtile: a delivery answers the flip barrier" \
  crates/geode-tile/src/following.rs \
  '                if !barrier.arrive(key, held_under) {' \
  '                if true {' \
  geode-marketdata \
  a_panel_arrives_on_delivery_after_an_as_of_change

run_mutation "mdtile: a failed delivery answers the barrier too" \
  crates/geode-tile/src/following.rs \
  '                if let Some(under) = asked {
                    barrier.arrive(key, under);
                }' \
  '                let _ = (asked, key);' \
  geode-marketdata \
  a_failed_delivery_still_arrives

run_mutation "mdtile: a refused submit arrives and clears acted" \
  crates/geode-tile/src/following.rs \
  '            barrier.arrive(key, asked);
        }
        self.in_flight = None;' \
  '            let _ = (key, asked);
        }
        self.in_flight = None;' \
  geode-marketdata \
  a_refused_request_arrives_at_the_barrier_and_retries

run_mutation "mdtile: a key change drops what was staged for the old key" \
  crates/geode-tile/src/following.rs \
  '    pub fn reset(&mut self) {
        self.staged = None;' \
  '    pub fn reset(&mut self) {' \
  geode-marketdata \
  a_key_change_drops_what_was_staged_for_the_old_key

run_mutation "mdtile: a delivery under an open barrier is staged" \
  crates/geode-tile/src/following.rs \
  '                let Some(held_under) = asked.filter(|&under| barrier.wants(key, under)) else {' \
  '                let Some(held_under) = asked.filter(|_| false) else {' \
  geode-marketdata \
  a_delivery_under_an_open_barrier_is_staged_until_the_flip

run_mutation "final: a panel stage survives a barrier replaced by a change it does not follow" \
  crates/geode-tile/src/following.rs \
  '        if differs(staged_under, now) {' \
  '        if !staged_under.same_flip_identity(now) {' \
  geode-marketdata \
  a_stage_survives_a_barrier_replaced_by_a_change_the_panel_does_not_follow

run_mutation "final: a panel stage is dropped once a counter it follows has moved" \
  crates/geode-tile/src/following.rs \
  '        if differs(staged_under, now) {' \
  '        if false {' \
  geode-marketdata \
  a_stage_is_dropped_when_a_counter_the_panel_follows_has_moved
```

"mdtile: hiding a panel clears what it acted on" is unchanged: its anchor, the hide branch's `self.data.cancel(QueryKey(self.id.0));`, is untouched until Task 7.

New entries (insert above the last `if [[ -n "$changed_ref" ]]; then`):

```zsh
run_mutation "mdtile: a hidden panel's stage promotes on the flip" \
  crates/geode-marketdata/src/tile.rs \
  '            let promoted = this.following.on_flip(now, Self::differs_on_followed);' \
  '            let promoted = if this.visible { this.following.on_flip(now, Self::differs_on_followed) } else { Promotion::Empty };' \
  geode-marketdata a_stage_held_when_the_panel_hides_promotes_on_the_flip

run_mutation "mdtile: a refused submit forgets what it asked" \
  crates/geode-tile/src/following.rs \
  '        if unanswered == Unanswered::Retry {' \
  '        if false {' \
  geode-marketdata a_refused_request_arrives_at_the_barrier_and_retries
```

- [ ] **Step 6: Gates**

```sh
cargo test -p geode-marketdata
cargo test -p geode-tile
cargo clippy -p geode-marketdata --all-targets -- -D warnings
cargo fmt --check
zsh scripts/mutation-check.sh --anchors-only
```

- [ ] **Step 7: Commit**

```bash
git add crates/geode-marketdata scripts/mutation-check.sh
git commit -m "refactor(marketdata): document request runs on geode_tile::following

Behaviour-preserving. Tests change only mechanically: t.tag reads become
t.following.tag(). New: a_stage_held_when_the_panel_hides_promotes_on_the_flip.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 8: Verify the 13 re-aimed and new entries three ways**

`pgrep -f 'mutation-che[c]k'` prints nothing. Then, for each entry name in Step 5, run `zsh scripts/mutation-check.sh --build-check "<name>"` (no BUILD), `zsh scripts/mutation-check.sh "<name>"` (`caught`), and hand-apply the replacement: the named test fails on an assertion; `git checkout -- <file>`. If "publication routing: panel promotion uses its own dependencies" reports SURVIVED, stop and report. Do not weaken the entry.

---

### Task 3: Timeseries onto the helper

**Files:**
- Modify: `crates/geode-timeseries/src/tile/mod.rs` (imports; fields ~130-140; constructor ~341-345; frame observer ~229-276; `set_visible` ~405-440; `view_moved` ~715; `acted()` ~894), `crates/geode-timeseries/src/tile/data.rs` (`deliver`, `release_view`, `requery`, delete `follows_changed`/`self_arrive`/`arrive`/`arrive_and_release`/`promote`), `crates/geode-timeseries/README.md`, `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: Task 1's API.
- Produces: `TimeseriesTile.following: FollowingQuery<SeriesResult>` (private); `differs_on_followed` unchanged (`pub(super)`).

- [ ] **Step 1: Move the state onto the helper**

`tile/mod.rs` imports: add `use geode_tile::following::{Delivered, FollowingQuery, FrameDoor, Promotion, Unanswered};` (`data.rs` sees it through `use super::*`).

Fields: delete `tag` (with "The tag of the request in flight…"), `acted` (with its doc), `query_in_flight`, `staged` (with its doc), `last_flip` (with its doc). Add:

```rust
    /// The series query under the flip barrier (see `geode_tile::following`):
    /// only the frame's as-of invalidates it.
    following: FollowingQuery<SeriesResult>,
```

Constructor: delete the five initialisers (`tag: 0,` is found with `grep -n '            tag: 0,' crates/geode-timeseries/src/tile/mod.rs`); add `following: FollowingQuery::new(),`.

Frame observer: replace the flip block

```rust
            let now = frame.read(cx).versions();
            if now.flip != this.last_flip {
                this.last_flip = now.flip;
                this.promote(cx);
            }
```

with

```rust
            let now = frame.read(cx).versions();
            // The post-step: every promotion that took something releases a
            // view move waiting behind it.
            match this.following.on_flip(now, Self::differs_on_followed) {
                Promotion::Empty => {}
                Promotion::Superseded => this.release_view(cx),
                Promotion::Apply(result) => {
                    this.apply_result(result, cx);
                    cx.notify();
                    this.release_view(cx);
                }
            }
```

In the same observer: `this.follows_changed(now)` → `this.following.follows_changed(now, Self::differs_on_followed)`; `.acted` in the refetch gate → `.following.acted()` (the gate becomes `if this.following.acted().is_some_and(|acted| Self::differs_on_followed(acted, now))`, which rustfmt wraps); `this.self_arrive(now, cx);` →

```rust
                let key = QueryKey(this.id.0);
                this.following.self_arrive(&mut FrameDoor::new(&this.frame, cx), key, now);
```

`set_visible`: `self.follows_changed(now)` → `self.following.follows_changed(now, Self::differs_on_followed)`. In the hide branch replace `// Clear acted versions…`, `self.acted = None;`, `self.query_in_flight = false;` with:

```rust
            // Forget the cancelled query so showing the tile cannot treat it
            // as completed work.
            self.following.abandon();
```

`view_moved`: `if self.query_in_flight {` → `if self.following.in_flight() {`.

`acted()` (test accessor): body `self.following.acted()`.

`tile/data.rs`, replace `deliver`:

```rust
    /// A series answer for this tile's key.
    pub fn deliver(&mut self, outcome: SeriesOutcome, cx: &mut Context<Self>) {
        let now = self.frame.read(cx).versions();
        let key = QueryKey(self.id.0);
        let delivered = self.following.deliver(
            outcome.tag,
            outcome.result,
            now,
            Self::differs_on_followed,
            &mut FrameDoor::new(&self.frame, cx),
            key,
        );
        match delivered {
            // An old tag cannot answer the newer request or its barrier arrival.
            Delivered::Stale => return,
            Delivered::Apply(result) => self.apply_result(result, cx),
            Delivered::Held => {}
            // Last good stays on screen: a failed query says nothing about
            // the points already painted.
            Delivered::Failed(e) => self.notice = Some(e.into()),
        }
        // The post-step: an answered query releases a view move waiting
        // behind it (suppressed while a result is held for the barrier).
        self.release_view(cx);
        cx.notify();
    }
```

`release_view`'s guard: `if !self.view_waiting || self.following.in_flight() || self.following.is_staged() {`.

`requery`: replace from `// A fresh question supersedes whatever was staged…` to the end with:

```rust
        // It also asks for the current view, so a waiting view move has
        // nothing left to ask. `begin` drops whatever was staged.
        self.view_waiting = false;
        let (as_of, versions) = {
            let frame = self.frame.read(cx);
            (frame.as_of().clone(), frame.versions())
        };
        let tag = self.following.begin(versions, std::time::Instant::now());
        let key = QueryKey(self.id.0);
        let params = {
            let buckets = self.result.as_ref().map(|r| r.buckets.as_slice());
            request::params(
                &self.model,
                key,
                tag,
                Utc::now(),
                &as_of,
                buckets.unwrap_or(&[]),
            )
        };
        let submitted = match params {
            Some(params) => {
                let queued = self.data.series(params);
                if let Err(refusal) = &queued {
                    self.notice = Some(format!("series request refused: {refusal}").into());
                }
                queued.is_ok()
            }
            // Nothing to ask about (no slot, or no dataset yet).
            None => false,
        };
        self.following.submitted(
            submitted,
            Unanswered::Retry,
            &mut FrameDoor::new(&self.frame, cx),
            key,
        );
        cx.notify();
```

Update `requery`'s doc comment last sentence to "Refusal or nothing to ask answers an open barrier and forgets the versions for retry." Delete `follows_changed`, `self_arrive`, `arrive`, `arrive_and_release`, `promote` from `data.rs`; keep `differs_on_followed`. Change the module doc of `data.rs` to "Fetch tracking, tagged series requests and deliveries over `geode_tile::following`. Successful fetches trigger series queries; failed series queries retain the last installed result."

Check: `grep -n 'self\.tag\b\|this\.tag\b\|\.acted\b\|query_in_flight\|\.staged\b\|last_flip' crates/geode-timeseries/src/tile/*.rs` prints nothing outside `tests.rs` field-free helpers.

- [ ] **Step 2: Run the barrier tests unchanged**

```sh
cargo test -p geode-timeseries -- a_delivery_becomes_the_chart_model_and_a_stale_tag_is_dropped a_refused_submit_notices_and_still_answers_the_barrier the_tile_follows_as_of_only_and_stages_under_an_open_barrier a_hidden_tile_cancels_and_a_shown_one_requeries a_view_move_while_a_query_is_out_waits_for_its_answer an_as_of_change_refetches_a_pair_whose_fetch_has_not_answered a_range_change_refetches_a_pair_whose_fetch_has_not_answered
cargo test -p geode-timeseries
```
Expected: all pass, no test edited.

- [ ] **Step 3: README**

`crates/geode-timeseries/README.md`, `tile::data` row: "Fetch submission, series queries and delivery filtering over `geode_tile::following` (the barrier staging and promotion rules), and the post-step `release_view` after each promotion and delivery." Do the same in `docs/current/features.md`'s timeseries module table row for `tile::data`.

- [ ] **Step 4: Re-aim harness entries**

```zsh
run_mutation "publication routing: a series query must really arrive" \
  crates/geode-tile/src/following.rs \
  '        if answering_now || !barrier.wants(key, now) {' \
  '        if !barrier.wants(key, now) {' \
  geode-timeseries a_delivery_becomes_the_chart_model_and_a_stale_tag_is_dropped

run_mutation "timeseries: a stale tag is dropped" \
  crates/geode-tile/src/following.rs \
  '        if tag != self.tag {' \
  '        if false {' \
  geode-timeseries \
  a_delivery_becomes_the_chart_model_and_a_stale_tag_is_dropped

run_mutation "timeseries: a view move waits for the query in flight" \
  crates/geode-timeseries/src/tile/mod.rs \
  '            if self.following.in_flight() {' \
  '            if false {' \
  geode-timeseries a_view_move_while_a_query_is_out_waits_for_its_answer

run_mutation "timeseries: an answer releases the waiting view" \
  crates/geode-timeseries/src/tile/data.rs \
  '        if !self.view_waiting || self.following.in_flight() || self.following.is_staged() {' \
  '        if true {' \
  geode-timeseries a_view_move_while_a_query_is_out_waits_for_its_answer
```

"timeseries: the as-of refetch is gated on a real as-of move": FROM is the rustfmt-formatted gate, copied from `tile/mod.rs` after `cargo fmt`, starting at `                if this` and ending at the line `                {`; TO stays `'                if true\n                {'` (two lines). Filter and package unchanged.

"timeseries: only as_of is followed" and "timeseries: a hidden tile cancels in flight" are unchanged: their anchors did not move.

New entries:

```zsh
run_mutation "timeseries: a delivery releases the waiting view" \
  crates/geode-timeseries/src/tile/data.rs \
  '        self.release_view(cx);
        cx.notify();
    }' \
  '        cx.notify();
    }' \
  geode-timeseries a_view_move_while_a_query_is_out_waits_for_its_answer

run_mutation "timeseries: a refused submit answers the barrier" \
  crates/geode-tile/src/following.rs \
  '            barrier.arrive(key, asked);
        }
        self.in_flight = None;' \
  '            let _ = (key, asked);
        }
        self.in_flight = None;' \
  geode-timeseries a_refused_submit_notices_and_still_answers_the_barrier
```

- [ ] **Step 5: Gates**

```sh
cargo test -p geode-timeseries
cargo clippy -p geode-timeseries --all-targets -- -D warnings
cargo fmt --check
zsh scripts/mutation-check.sh --anchors-only
```

- [ ] **Step 6: Commit**

```bash
git add crates/geode-timeseries docs/current/features.md scripts/mutation-check.sh
git commit -m "refactor(timeseries): series query runs on geode_tile::following

Behaviour-preserving; release_view stays the post-step after every
promotion and delivery. No test edited.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 7: Verify the seven re-aimed and new entries three ways** (as in Task 2 Step 8).

---

### Task 4: Blotter onto the helper

**Files:**
- Modify: `crates/geode-blotter/src/tile.rs` (imports; fields ~164-196; constructor ~416-428; `last_query` ~445; `follows_changed`/`differs_on_followed`/`on_frame_changed`/`promote` ~568-641; `requery` ~676-788; `deliver` ~790-838; `set_visible` ~840; render affordance ~1766; tests at ~2598, ~5561, ~6072), `crates/geode-blotter/README.md`, `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: Task 1's API.
- Produces: `BlotterTile.following: FollowingQuery<(Arc<Snapshot>, Vec<String>)>`; private `Followed { scope, grouping, as_of }` with `differs(self, versions, now) -> bool`; `BlotterTile::followed(&self) -> Followed`; `differs_on_followed(&self, ..)` kept as a delegate.

- [ ] **Step 1: Move the state onto the helper**

Imports: add `use geode_tile::following::{Delivered, FollowingQuery, FrameDoor, Promotion, Unanswered};`.

Fields: delete `acted` (with doc), `tag: u64,`, `in_flight: Option<Instant>,`, `staged` (with its doc), `last_flip` (with its doc). Add:

```rust
    /// This tile's view query under the flip barrier (see
    /// `geode_tile::following`), with the grouping each result was asked
    /// under. Promotion compares only counters this tile follows (`Followed`),
    /// including watched data and configuration; a tile-local requery clears
    /// the stage because it moves no frame counter.
    following: FollowingQuery<(Arc<Snapshot>, Vec<String>)>,
```

Constructor: delete `acted: None,`, `tag: 0,` (grep for it), `in_flight: None,`, `staged: None,`, `last_flip: 0,`; add `following: FollowingQuery::new(),`.

`last_query`:

```rust
    pub fn last_query(&self) -> Option<(u64, Vec<String>)> {
        let tag = self.following.tag();
        (tag > 0).then(|| (tag, self.last_grouping.clone()))
    }
```

Above `impl BlotterTile` (module level, private):

```rust
/// The frame counters a blotter's answer depends on, copied out of the tile
/// so the barrier helper can compare versions while the tile's query state
/// is mutably borrowed. Watched data and configuration are always followed;
/// scope unless unscoped, grouping unless pinned, as-of unless pinned.
#[derive(Debug, Clone, Copy)]
struct Followed {
    scope: bool,
    grouping: bool,
    as_of: bool,
}

impl Followed {
    fn differs(self, versions: FrameVersions, now: FrameVersions) -> bool {
        (self.scope && versions.scope != now.scope)
            || (self.grouping && versions.grouping != now.grouping)
            || (self.as_of && versions.as_of != now.as_of)
            || versions.data != now.data
            || versions.config != now.config
    }
}
```

Replace `follows_changed`, `differs_on_followed`, `on_frame_changed` and `promote` with:

```rust
    fn followed(&self) -> Followed {
        Followed {
            scope: !self.unscoped,
            grouping: self.pin == Pin::None,
            as_of: matches!(self.tile_as_of, TileAsOf::Follow),
        }
    }

    /// Compare the counters this tile follows. Requery and staged-snapshot
    /// promotion share this comparison so they agree about which changes
    /// invalidate an answer.
    fn differs_on_followed(&self, versions: FrameVersions, now: FrameVersions) -> bool {
        self.followed().differs(versions, now)
    }

    fn on_frame_changed(&mut self, cx: &mut Context<Self>) {
        // Attempt promotion on every flip, including while hidden, so hiding
        // between staging and release does not leave a valid answer waiting.
        // `flip` itself is never a requery input.
        let now = self.versions(cx);
        let followed = self.followed();
        let differs = move |a, b| followed.differs(a, b);
        let promoted = self.following.on_flip(now, differs);
        if let Promotion::Apply((snapshot, grouping)) = promoted {
            self.apply(snapshot, grouping, cx);
        }
        if !self.visible {
            return;
        }
        if self.following.follows_changed(now, differs) {
            self.requery(cx);
        } else {
            // A tile can belong to the barrier without following its change;
            // unless its own query for this flip identity is still out, it
            // answers now so its siblings are not held.
            let key = QueryKey(self.tile.0);
            self.following.self_arrive(&mut FrameDoor::new(&self.frame, cx), key, now);
        }
    }
```

`requery`: the first statement `self.staged = None;` becomes `self.following.drop_stage();` (keep its comment). In the unresolved-scope arm replace from `// Supersede any query still in flight` through the `cx.notify(); return;` with:

```rust
                // Supersede any query still in flight: its outcome is for the
                // previous scope and must not paint over this error. `acted`
                // stays set: redefining the name bumps the config version,
                // which is the retry.
                self.following.begin(versions, Instant::now());
                let key = QueryKey(self.tile.0);
                self.following.submitted(
                    false,
                    Unanswered::KeepActed,
                    &mut FrameDoor::new(&self.frame, cx),
                    key,
                );
                cx.notify();
                return;
```

Replace from `self.tag += 1;` (the submitting path, after `max_depth`) through the end of the `if let Err(refusal) = queued { … }` block with:

```rust
        let submitted = Instant::now();
        let tag = self.following.begin(versions, submitted);
        self.last_grouping = grouping.clone();
        self.title = Self::compute_title(&self.view_name, &self.last_grouping);
        let key = QueryKey(self.tile.0);
        let queued = self.data.query(QueryParams {
            key,
            tag,
            submitted,
            view: self.view_name.clone(),
            grouping: Some(grouping),
            scope,
            as_of,
            max_depth,
        });
        if let Err(refusal) = &queued {
            // A stopped refusal retries on the next frame change too: each
            // attempt costs nothing and re-reports the same kind. The last
            // snapshot stays.
            self.error = Some(Notice::danger(format!("query refused: {refusal}")));
        }
        self.following.submitted(
            queued.is_ok(),
            Unanswered::Retry,
            &mut FrameDoor::new(&self.frame, cx),
            key,
        );
```

In the in-flight repaint task below it, `if t.in_flight.is_some() {` → `if t.following.in_flight() {`.

Replace `deliver`:

```rust
    pub fn deliver(&mut self, outcome: QueryOutcome, cx: &mut Context<Self>) {
        let now = self.versions(cx);
        let followed = self.followed();
        let result = outcome
            .snapshot
            .map(|snapshot| (snapshot, self.last_grouping.clone()));
        let key = QueryKey(self.tile.0);
        let delivered = self.following.deliver(
            outcome.tag,
            result,
            now,
            move |a, b| followed.differs(a, b),
            &mut FrameDoor::new(&self.frame, cx),
            key,
        );
        if let Delivered::Stale = delivered {
            return; // stale: a newer request is out
        }
        let micros = outcome.submitted.elapsed().as_micros() as u64;
        self.frame
            .update(cx, |f, _| f.requery.record_submit_to_snapshot(micros));
        match delivered {
            Delivered::Stale => {}
            Delivered::Apply((snapshot, grouping)) => {
                self.error = None;
                self.apply(snapshot, grouping, cx);
            }
            Delivered::Held => self.error = None,
            // The last good snapshot stays; the failure has already arrived.
            Delivered::Failed(e) => self.error = Some(Notice::danger(e)),
        }
        cx.notify();
    }
```

`set_visible`: `if self.follows_changed(now) {` → `if self.following.follows_changed(now, |a, b| self.differs_on_followed(a, b)) {`.

Render affordance: `.in_flight` → `.following.in_flight_since()` (so `self.following.in_flight_since().is_some_and(|t| t.elapsed() > IN_FLIGHT_AFTER)`).

Tests (mechanical): `t.acted.is_none()` → `t.following.acted().is_none()`; `t.differs_on_followed(t.acted.unwrap(), now)` → `t.differs_on_followed(t.following.acted().unwrap(), now)`; `t.staged.is_some()` → `t.following.is_staged()`.

Check: `grep -n 'self\.tag\b\|self\.acted\|\.in_flight\b\|self\.staged\|last_flip\|unwrap_or_default' crates/geode-blotter/src/tile.rs` shows no machine field (the `unwrap_or_default()` on `acted` is gone).

- [ ] **Step 2: Run the barrier tests unchanged**

```sh
cargo test -p geode-blotter -- a_refused_query_arrives_at_the_barrier a_refused_query_says_busy_or_stopped an_unresolved_named_expression_errors_without_querying a_stale_outcome_is_dropped a_pinned_tile_ignores_the_frames_as_of_and_answers_the_barrier two_tiles_promote_in_the_same_pass a_pinned_tile_arrives_from_on_frame_changed a_second_mutation_during_a_barrier_wait a_stage_is_dropped_when_a_counter_the_tile_follows_has_moved a_tile_local_requery_clears_the_stage unrelated_publications_neither_answer
cargo test -p geode-blotter
```
Expected: all pass.

- [ ] **Step 3: README**

`crates/geode-blotter/README.md`: replace the two bullets beginning "`FrameVersions.flip` is excluded from `follows_changed`" and "A staged snapshot is promoted only while…" with:

```markdown
- The query runs on `geode_tile::following`. `Followed` names the counters
  an answer depends on (scope unless unscoped, grouping unless pinned, as-of
  unless pinned, always watched data and configuration); it decides both
  requery and promotion, so the two agree. `flip` is never a requery input.
  Tile-local requeries clear the stage because they move no frame counter.
  An unresolved named scope arrives at the barrier but keeps what it acted
  on (`Unanswered::KeepActed`): the configuration change that defines the
  name is the retry.
```

- [ ] **Step 4: Re-aim harness entries**

```zsh
run_mutation "asof-pin: a pinned tile does not follow the frame's as-of" \
  crates/geode-blotter/src/tile.rs \
  '            as_of: matches!(self.tile_as_of, TileAsOf::Follow),' \
  '            as_of: true,' \
  geode-blotter \
  a_pinned_tile_ignores_the_frames_as_of_and_answers_the_barrier

run_mutation "tile: a stale tag is dropped" \
  crates/geode-tile/src/following.rs \
  '        if tag != self.tag {' \
  '        if false {' \
  geode-blotter \
  a_stale_outcome_is_dropped_an_error_keeps_the_last_snapshot_and_timing_is_recorded

run_mutation "flip: failure counts as arrival" \
  crates/geode-tile/src/following.rs \
  '                if let Some(under) = asked {
                    barrier.arrive(key, under);
                }' \
  '                let _ = (asked, key);' \
  geode-blotter two_tiles_promote_in_the_same_pass_and_a_failure_releases_the_barrier

run_mutation "flip: a staged snapshot waits for the barrier" \
  crates/geode-tile/src/following.rs \
  '                let Some(held_under) = asked.filter(|&under| barrier.wants(key, under)) else {' \
  '                let Some(held_under) = asked.filter(|_| false) else {' \
  geode-blotter two_tiles_promote_in_the_same_pass_and_a_failure_releases_the_barrier

run_mutation "publication routing: a blotter query must really arrive" \
  crates/geode-tile/src/following.rs \
  '        if answering_now || !barrier.wants(key, now) {' \
  '        if !barrier.wants(key, now) {' \
  geode-blotter unrelated_publications_neither_answer_a_query_nor_discard_its_stage

run_mutation "publication routing: blotter promotion uses its own dependencies" \
  crates/geode-blotter/src/tile.rs \
  '        let now = self.versions(cx);
        let followed = self.followed();
        let differs = move |a, b| followed.differs(a, b);
        let promoted = self.following.on_flip(now, differs);' \
  '        let now = self.frame.read(cx).versions();
        let followed = self.followed();
        let differs = move |a, b| followed.differs(a, b);
        let promoted = self.following.on_flip(now, differs);' \
  geode-blotter unrelated_publications_neither_answer_a_query_nor_discard_its_stage

run_mutation "flip: a non-following tile still arrives on its own" \
  crates/geode-blotter/src/tile.rs \
  '            self.following.self_arrive(&mut FrameDoor::new(&self.frame, cx), key, now);' \
  '            let _ = (key, now);' \
  geode-blotter a_pinned_tile_arrives_from_on_frame_changed_without_requerying

run_mutation "flip: promote only applies a staged snapshot that still answers what the tile follows" \
  crates/geode-tile/src/following.rs \
  '        if differs(staged_under, now) {' \
  '        if false {' \
  geode-blotter a_stage_is_dropped_when_a_counter_the_tile_follows_has_moved

run_mutation "flip: a fresh requery clears whatever was staged before it" \
  crates/geode-tile/src/following.rs \
  '    pub fn begin(&mut self, versions: FrameVersions, submitted: Instant) -> u64 {
        self.staged = None;' \
  '    pub fn begin(&mut self, versions: FrameVersions, submitted: Instant) -> u64 {' \
  geode-blotter a_tile_local_requery_clears_the_stage_a_barrier_left_behind

run_mutation "final: a pinned blotter promotes the stage a replaced barrier left it" \
  crates/geode-tile/src/following.rs \
  '        if differs(staged_under, now) {' \
  '        if !staged_under.same_flip_identity(now) {' \
  geode-blotter a_second_mutation_during_a_barrier_wait_clears_the_stale_staged_snapshot
```

(The requery-clear entry moved to `begin`: `drop_stage` in `requery` now only covers the unconfigured-view path, and `begin` clears on every submitting path. A mutant of `drop_stage` alone would survive behind it.)

- [ ] **Step 5: Gates**

```sh
cargo test -p geode-blotter
cargo clippy -p geode-blotter --all-targets -- -D warnings
cargo fmt --check
zsh scripts/mutation-check.sh --anchors-only
```

- [ ] **Step 6: Commit**

```bash
git add crates/geode-blotter scripts/mutation-check.sh
git commit -m "refactor(blotter): view query runs on geode_tile::following

Behaviour-preserving; the unresolved-name path keeps acted
(Unanswered::KeepActed) and the acted.unwrap_or_default() is gone. Tests
change only mechanically (t.acted / t.staged reads become accessors).

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 7: Verify the ten re-aimed entries three ways** (as in Task 2 Step 8).

---

### Task 5: Pricer and diagnostics arrive through `arrive_immediately`

**Files:**
- Modify: `crates/geode-pricer/src/tile.rs` (~565-576), `crates/geode-pricer/README.md`, `crates/geode-diagnostics/Cargo.toml`, `crates/geode-diagnostics/src/tile.rs` (~189-199), `crates/geode-diagnostics/README.md`, `crates/geode-tile/README.md`, `docs/current/architecture.md`, `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `geode_tile::following::{self, FrameDoor}`; `following::arrive_immediately(&mut FrameDoor::new(&frame, cx), key)`.

- [ ] **Step 1: Pricer**

Add `use geode_tile::following::{self, FrameDoor};`. Replace the observer:

```rust
        // Pricing does not follow frame queries, so there is no result to wait for.
        // Arrive immediately to avoid holding other tiles behind the flip barrier.
        cx.observe(&frame, |this, frame, cx| {
            following::arrive_immediately(&mut FrameDoor::new(&frame, cx), QueryKey(this.id.0));
        })
        .detach();
```

- [ ] **Step 2: Diagnostics**

`crates/geode-diagnostics/Cargo.toml` `[dependencies]`: add `geode-tile.workspace = true` after `geode-shell.workspace = true`, and add a comment line above `[dependencies]`: `# geode-tile for the flip-barrier arrival (following::arrive_immediately).`

`src/tile.rs`: add `use geode_tile::following::{self, FrameDoor};`. Replace the observer's tail from `let key = QueryKey(this.tile.0);` through the closing `}` of its `if frame.read(cx).barrier_wants(key, now) { … }` with:

```rust
            following::arrive_immediately(&mut FrameDoor::new(&frame, cx), QueryKey(this.tile.0));
```

Keep the comment above it.

- [ ] **Step 3: Run the unchanged tests**

```sh
cargo test -p geode-pricer the_tile_answers_a_flip_barrier_it_has_nothing_coming_for
cargo test -p geode-diagnostics the_tile_answers_a_flip_barrier_it_has_nothing_coming_for
cargo test -p geode-pricer
cargo test -p geode-diagnostics
```
Expected: pass.

- [ ] **Step 4: Docs**

`crates/geode-pricer/README.md` line "The tile arrives at flip barriers itself; it submits no view query." → append " (`geode_tile::following::arrive_immediately`)". `crates/geode-diagnostics/README.md` lines 52-53: same append. `docs/current/architecture.md`: replace "`geode-diagnostics` has no popover, menu, confirm or notice line and does not depend on `geode-tile`." with "`geode-diagnostics` has no popover, menu, confirm or notice line; it depends on `geode-tile` only for the flip-barrier arrival." In `crates/geode-tile/README.md` the "Used by" paragraph already names diagnostics (Task 1).

- [ ] **Step 5: Harness**

Re-aim:

```zsh
run_mutation "pricer tile: the flip barrier waits for the pricer" \
  crates/geode-pricer/src/tile.rs \
  '            following::arrive_immediately(&mut FrameDoor::new(&frame, cx), QueryKey(this.id.0));' \
  '            let _ = (&frame, QueryKey(this.id.0));' \
  geode-pricer the_tile_answers_a_flip_barrier_it_has_nothing_coming_for
```

New:

```zsh
run_mutation "diagnostics module: the tile answers a flip barrier itself" \
  crates/geode-diagnostics/src/tile.rs \
  '            following::arrive_immediately(&mut FrameDoor::new(&frame, cx), QueryKey(this.tile.0));' \
  '            let _ = (&frame, QueryKey(this.tile.0));' \
  geode-diagnostics the_tile_answers_a_flip_barrier_it_has_nothing_coming_for
```

- [ ] **Step 6: Gates**

```sh
cargo clippy -p geode-pricer -p geode-diagnostics --all-targets -- -D warnings
cargo fmt --check
zsh scripts/mutation-check.sh --anchors-only
```

- [ ] **Step 7: Commit**

```bash
git add crates/geode-pricer crates/geode-diagnostics crates/geode-tile/README.md docs/current/architecture.md Cargo.lock scripts/mutation-check.sh
git commit -m "refactor(pricer,diagnostics): arrive through geode_tile::following

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 8: Verify the two entries three ways** (as in Task 2 Step 8).

---

### Task 6: The shell's close hook

**Files:**
- Modify: `crates/geode-shell/src/module.rs` (`TileContent`, `recording::Recorded`, `RecordingContent`), `crates/geode-shell/src/shell/occupants.rs` (`ensure_occupants` ~163-182), `crates/geode-shell/src/shell/add_tile.rs` (~39-41), `crates/geode-shell/src/shell/tests/occupants.rs`, `crates/geode-shell/README.md`, `docs/current/shell.md`, `scripts/mutation-check.sh`

**Interfaces:**
- Produces: `TileContent::closed(&self, cx: &mut App)` (default no-op); `Recorded::Closed(TileId)`. Task 7 implements `closed` in three modules.

- [ ] **Step 1: Write the failing shell test**

In `crates/geode-shell/src/shell/tests/occupants.rs`, after `closing_a_tile_drops_its_occupant_and_switching_workspaces_toggles_visibility`:

```rust
/// Closing a tile tells its occupant it closed, after telling it it is
/// hidden; a workspace switch hides without closing. Following tiles cancel
/// only on `closed`, so a switch reported as a close would cancel a query
/// whose answer the trader expects on return.
#[gpui::test]
fn closing_a_tile_tells_its_occupant_it_closed_and_a_workspace_switch_does_not(
    cx: &mut gpui::TestAppContext,
) {
    use crate::module::recording::Recorded;
    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    cx.simulate_keystrokes("ctrl-v");
    let shell = shell_of(&window, &mut cx);
    let tile = shell.read_with(&cx, |s, _| {
        s.services.workspaces.active().focused_tile().unwrap()
    });
    cx.simulate_keystrokes("alt-2");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_keystrokes("alt-1");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        !log.borrow()
            .iter()
            .any(|r| matches!(r, Recorded::Closed(t) if *t == tile)),
        "a switch hides; it never closes: {:?}",
        log.borrow()
    );

    cx.simulate_keystrokes("ctrl-w");
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let log = log.borrow();
    let hidden = log
        .iter()
        .rposition(|r| matches!(r, Recorded::Visible(t, false) if *t == tile))
        .expect("hidden before removal");
    let closed = log
        .iter()
        .position(|r| matches!(r, Recorded::Closed(t) if *t == tile))
        .expect("closing a tile tells its occupant");
    assert!(hidden < closed, "hidden first, then closed: {log:?}");
}
```

Run: `cargo test -p geode-shell closing_a_tile_tells_its_occupant_it_closed`
Expected: FAIL to compile (`Recorded::Closed` does not exist).

- [ ] **Step 2: Add the hook and call it**

`module.rs`, in `TileContent` after `set_visible`:

```rust
    /// The shell removed this occupant for good (its tile closed, or a
    /// placeholder was filled in place) and drops it right after, following
    /// `set_visible(false)`. Hiding never calls this. A following tile
    /// cancels its in-flight query here and answers any open flip barrier
    /// still waiting on it, so a closed tile never holds the others to the
    /// deadline. The default does nothing.
    fn closed(&self, _cx: &mut App) {}
```

In `recording::Recorded` add, after `Visible(TileId, bool),`:

```rust
        /// `closed` reached this tile: its occupant is being removed.
        Closed(TileId),
```

In `impl TileContent for RecordingContent`, after `set_visible`:

```rust
        fn closed(&self, _: &mut App) {
            self.log.borrow_mut().push(Recorded::Closed(self.tile));
        }
```

`shell/occupants.rs`, `ensure_occupants`: the removal loop becomes

```rust
        for (id, o) in self.occupants.iter() {
            if !all.contains(id) {
                o.content.set_visible(false, cx);
                o.content.closed(cx);
            }
        }
```

and its comment reads: "Tell removed occupants they are hidden and then closed before dropping them, so they can release subscriptions and cancel their queries with a live GPUI context. The visibility diff below can only reach occupants still in the map." Update the method's doc line "notify removed occupants that they are hidden, then drop them" to "tell removed occupants they are hidden and closed, then drop them".

`shell/add_tile.rs`: the placeholder removal becomes

```rust
            if let Some(o) = self.occupants.remove(&tile) {
                o.content.set_visible(false, cx);
                o.content.closed(cx);
            }
```

- [ ] **Step 3: Run the test**

Run: `cargo test -p geode-shell closing_a_tile_tells_its_occupant_it_closed`
Expected: PASS.

- [ ] **Step 4: Docs**

`crates/geode-shell/README.md`, `module` row: after "`TileContent` (including" insert "`closed`, called once on removal after `set_visible(false)`, ". `docs/current/shell.md`, the paragraph "Tile occupants are created through the app-supplied `ModuleRoster`…": append "Removing an occupant (closing its tile, or filling a placeholder in place) calls `set_visible(false)` and then `closed`, once, before the occupant is dropped. Hiding never calls `closed`."

- [ ] **Step 5: Harness**

Re-aim "shell: MAJ-2 — ensure_occupants drops a vanished tile's occupant without unwatching it":

```zsh
run_mutation "shell: MAJ-2 — ensure_occupants drops a vanished tile's occupant without unwatching it" \
  crates/geode-shell/src/shell/occupants.rs \
  '        for (id, o) in self.occupants.iter() {
            if !all.contains(id) {
                o.content.set_visible(false, cx);
                o.content.closed(cx);
            }
        }
        self.occupants.retain(|id, _| all.contains(id));' \
  '        self.occupants.retain(|id, _| all.contains(id));' \
  geode-shell closing_a_watching_tile_unwatches_the_diagnostics_entity
```

("hosting: a vanished tile is told before its occupant is dropped" is unchanged: its three-line anchor is still a unique prefix.)

New:

```zsh
run_mutation "hosting: a removed occupant is told it closed" \
  crates/geode-shell/src/shell/occupants.rs \
  '                o.content.closed(cx);' \
  '                let _ = &o.content;' \
  geode-shell closing_a_tile_tells_its_occupant_it_closed_and_a_workspace_switch_does_not
```

- [ ] **Step 6: Gates**

```sh
cargo test -p geode-shell
cargo check -p geode-shell --features test-support --all-targets
cargo clippy -p geode-shell --all-targets -- -D warnings
cargo fmt --check
zsh scripts/mutation-check.sh --anchors-only
```

- [ ] **Step 7: Commit**

```bash
git add crates/geode-shell docs/current/shell.md scripts/mutation-check.sh
git commit -m "feat(shell): TileContent::closed — removal is told apart from hiding

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 8: Verify the two entries three ways** (as in Task 2 Step 8).

---

### Task 7: Hide lets the query finish; close cancels and arrives

**Files:**
- Modify: `crates/geode-tile/src/following.rs` (delete `abandon` and its test), `crates/geode-marketdata/src/tile.rs` (`set_visible`, new `closed`, tests), `crates/geode-marketdata/src/content.rs`, `crates/geode-timeseries/src/tile/mod.rs` (`set_visible`, new `closed`), `crates/geode-timeseries/src/tile/tests.rs`, `crates/geode-timeseries/src/content.rs`, `crates/geode-blotter/src/tile.rs` (new `closed`, tests), `crates/geode-blotter/src/content.rs`, `docs/current/shell.md`, `docs/current/features.md`, `crates/geode-tile/README.md`, `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `FollowingQuery::close`, `TileContent::closed` (Task 6).
- Produces: `MarketDataTile::closed`, `TimeseriesTile::closed`, `BlotterTile::closed` (each `pub fn closed(&mut self, cx: &mut Context<Self>)`), forwarded by each module's `TileContent::closed`.

- [ ] **Step 1: Write the new and rewritten tests (they fail on current behaviour)**

Market-data (`tile.rs` tests). Add to `impl Harness`:

```rust
        /// Everything on the channel since the last drain, `Cancel` included.
        fn raw_requests(&self) -> Vec<Request> {
            self.rx.try_iter().collect()
        }
```

Replace `a_tile_hidden_mid_flight_requeries_on_reshow` with:

```rust
    /// Hiding a panel cancels nothing and forgets nothing: the outstanding
    /// request finishes, its reply paints while the panel is hidden, and
    /// showing it again asks nothing because nothing it follows moved.
    #[gpui::test]
    fn a_panel_hidden_mid_flight_paints_the_reply_and_asks_nothing_on_reshow(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().expect("the first request");
        h.visible(&mut vcx, false);
        assert!(
            !h.raw_requests()
                .iter()
                .any(|r| matches!(r, Request::Cancel { .. })),
            "a hide is not a close: nothing is cancelled"
        );
        h.deliver(&mut vcx, first.tag, Arc::new(cvi(BASE)));
        assert_eq!(h.rows(&vcx), 2, "the reply applies while the panel is hidden");
        h.visible(&mut vcx, true);
        assert!(
            h.document_request().is_none(),
            "nothing it follows moved, so nothing is asked"
        );
        assert_eq!(h.rows(&vcx), 2);
    }

    /// The reply to a question asked before a followed change can land while
    /// the panel is hidden; reshow must still ask again.
    #[gpui::test]
    fn a_followed_change_while_hidden_requeries_on_reshow(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().expect("the first request");
        h.visible(&mut vcx, false);
        let at = chrono::DateTime::parse_from_rfc3339(BASE)
            .unwrap()
            .with_timezone(&chrono::Utc);
        h.frame.update(&mut vcx, |f, cx| {
            f.set_as_of(geode_core::query::AsOf::At(at));
            cx.notify();
        });
        assert!(h.document_request().is_none(), "a hidden panel asks nothing");
        h.deliver(&mut vcx, first.tag, Arc::new(cvi(BASE)));
        h.visible(&mut vcx, true);
        let second = h
            .document_request()
            .expect("the as-of moved while hidden: reshow asks again");
        assert!(second.tag > first.tag);
        assert_eq!(second.as_of, geode_core::query::AsOf::At(at));
    }

    /// A panel hidden while enrolled in an open barrier still answers it with
    /// its reply, and promotes on the flip while hidden, so a tab switch in
    /// the middle of a flip never holds the other tiles to the deadline.
    #[gpui::test]
    fn a_panel_hidden_mid_flip_still_answers_the_barrier(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, first, Arc::new(cvi(BASE)));
        let other = QueryKey(TILE + 1);
        open_barrier_on_as_of(&h, &mut vcx, &[QueryKey(TILE), other], 60);
        let second = h.document_request().unwrap().tag;
        h.visible(&mut vcx, false);
        h.deliver(
            &mut vcx,
            second,
            Arc::new(document_of(&["t0", "t1", "t2", "t3", "t4"], &NODES, BASE)),
        );
        let now = h.versions(&vcx);
        assert!(
            !h.frame
                .read_with(&vcx, |f, _| f.barrier_wants(QueryKey(TILE), now)),
            "the reply answered the barrier it was enrolled in, hidden or not"
        );
        assert_eq!(h.rows(&vcx), 2, "held behind the other tile");
        h.frame.update(&mut vcx, |f, cx| {
            assert!(f.arrived(other, now));
            cx.notify();
        });
        assert_eq!(h.rows(&vcx), 5, "and promoted on the flip while hidden");
    }

    /// Closing a panel cancels its request by key and answers the barrier,
    /// so the other tiles do not wait out the deadline; a late reply paints
    /// nothing.
    #[gpui::test]
    fn closing_a_panel_mid_flip_cancels_its_request_and_releases_the_barrier(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, first, Arc::new(cvi(BASE)));
        open_barrier_on_as_of(&h, &mut vcx, &[QueryKey(TILE)], 60);
        let second = h.document_request().expect("an as-of change requeries").tag;
        assert!(h.barrier_open(&vcx), "waiting on this panel's reply");
        vcx.update(|_, cx| h.content.closed(cx));
        assert!(
            h.raw_requests()
                .iter()
                .any(|r| matches!(r, Request::Cancel { key } if *key == QueryKey(TILE))),
            "a close cancels the request by key"
        );
        assert!(!h.barrier_open(&vcx), "and answers the barrier");
        h.deliver(
            &mut vcx,
            second,
            Arc::new(document_of(&["t0", "t1", "t2", "t3", "t4"], &NODES, BASE)),
        );
        assert_eq!(h.rows(&vcx), 2, "a late reply to a closed panel paints nothing");
    }
```

Timeseries (`tile/tests.rs`). Replace `a_hidden_tile_cancels_and_a_shown_one_requeries_and_a_restored_one_refetches_once` with the version below. Its restore half is unchanged:

```rust
#[gpui::test]
fn a_hidden_tile_keeps_its_query_and_a_shown_one_refetches_and_a_restored_one_refetches_once(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open(cx);
    h.visible(&mut vcx, true);
    h.command(&mut vcx, "add SPX.close").unwrap();
    h.requests();
    h.deliver_fetched(&mut vcx, "demo_kdb", "SPX.close", Ok(1));
    let tag = h.series_request().unwrap().tag;
    h.visible(&mut vcx, false);
    assert!(
        !h.raw_requests()
            .iter()
            .any(|r| matches!(r, Request::Cancel { .. })),
        "a hide is not a close: the query is kept"
    );
    h.deliver_series(&mut vcx, tag, result_with(&[1], 5));
    assert_eq!(h.chart(&vcx).buckets.len(), 5, "its answer applies while hidden");
    h.visible(&mut vcx, true);
    let reqs = h.requests();
    assert!(
        reqs.iter().any(|r| matches!(r, Request::Fetch(_))),
        "shown: refetch (§9.10)…"
    );
    assert!(
        !reqs.iter().any(|r| matches!(r, Request::Series(_))),
        "…but no query from the show itself: nothing it follows moved, and \
         the refetch's completion is what asks again"
    );
    let table = vcx.update(|_, cx| h.content.serialize(cx));
    let (h2, mut vcx2) = open_with(cx, Some(table));
    assert!(
        matches!(h2.model(&vcx2).slots()[0].state, SlotState::Idle),
        "restored: not yet asked"
    );
    h2.visible(&mut vcx2, true);
    assert!(matches!(
        h2.model(&vcx2).slots()[0].state,
        SlotState::Fetching
    ));
    let f = h2.fetch_request().expect("a restored tile refetches once");
    assert_eq!(f.identity, "SPX.close");
    assert!(h2.fetch_request().is_none());
    h2.visible(&mut vcx2, false);
    h2.visible(&mut vcx2, true);
    assert!(
        h2.fetch_request().is_some(),
        "every show refetches (coverage subtraction makes it cheap)"
    );
}

#[gpui::test]
fn an_as_of_change_while_hidden_requeries_on_reshow(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_loaded(cx, 5);
    h.visible(&mut vcx, false);
    h.requests();
    let at = chrono::Utc::now() - chrono::Duration::days(30);
    h.frame.update(&mut vcx, |f, cx| {
        f.set_as_of(AsOf::At(at));
        cx.notify();
    });
    assert!(h.requests().is_empty(), "a hidden tile asks nothing");
    h.visible(&mut vcx, true);
    let reqs = h.requests();
    let Some(Request::Series(q)) = reqs.iter().find(|r| matches!(r, Request::Series(_))) else {
        panic!("the as-of moved while hidden: reshow asks again: {reqs:?}");
    };
    assert_eq!(q.as_of, AsOf::At(at));
}

#[gpui::test]
fn closing_the_tile_mid_flip_cancels_its_query_and_releases_the_barrier(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_loaded(cx, 5);
    h.requests();
    let at = chrono::Utc::now() - chrono::Duration::days(30);
    open_barrier_on_as_of(&h, &mut vcx, &[QueryKey(TILE)], at);
    let q = h.series_request().expect("an as-of change queries");
    assert!(h.frame.read_with(&vcx, |f, _| f.barrier_open()));
    vcx.update(|_, cx| h.content.closed(cx));
    assert!(
        h.raw_requests()
            .iter()
            .any(|r| matches!(r, Request::Cancel { key } if *key == QueryKey(TILE))),
        "a close cancels the query by key"
    );
    assert!(
        !h.frame.read_with(&vcx, |f, _| f.barrier_open()),
        "and answers the barrier"
    );
    h.deliver_series(&mut vcx, q.tag, result_with(&[1], 9));
    assert_eq!(
        h.chart(&vcx).buckets.len(),
        5,
        "a late answer to a closed tile paints nothing"
    );
}
```

Blotter (`tile.rs` tests). Add `use geode_shell::module::TileContent;` to the test module's imports, then:

```rust
    /// The blotter never cancelled on hide; this pins the shared rule: the
    /// reply lands while hidden and reshow asks nothing when nothing it
    /// follows moved.
    #[gpui::test]
    fn a_tile_hidden_mid_flight_paints_the_reply_and_asks_nothing_on_reshow(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let p0 = next_query(&h.requests);
        h.tile.update(&mut vcx, |t, cx| t.set_visible(false, cx));
        assert!(
            h.requests
                .try_iter()
                .all(|r| !matches!(r, Request::Cancel { .. })),
            "a hide is not a close"
        );
        deliver(&h, &mut vcx, p0.tag, Ok(snapshot()));
        assert_eq!(
            shown_texts(&h.tile, &vcx),
            vec!["".to_string(), "L1".into(), "L2".into()]
        );
        h.tile.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        assert!(h.requests.try_recv().is_err(), "nothing it follows moved");
    }

    #[gpui::test]
    fn a_followed_change_while_hidden_requeries_on_reshow(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let p0 = next_query(&h.requests);
        deliver(&h, &mut vcx, p0.tag, Ok(snapshot()));
        h.tile.update(&mut vcx, |t, cx| t.set_visible(false, cx));
        h.frame.update(&mut vcx, |f, cx| {
            f.set_text(Some("A".into()));
            cx.notify();
        });
        assert!(h.requests.try_recv().is_err(), "a hidden tile asks nothing");
        h.tile.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let p1 = next_query(&h.requests);
        assert!(p1.tag > p0.tag, "the scope moved while hidden: reshow asks again");
    }

    /// Closing a tile the barrier still waits on cancels its query and
    /// answers the barrier, and the sibling that staged promotes in the same
    /// pass rather than at the deadline.
    #[gpui::test]
    fn closing_a_tile_mid_flip_cancels_its_query_and_releases_the_barrier(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_two(cx);
        h.a.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        h.b.update(&mut vcx, |t, cx| t.set_visible(true, cx));
        let pa0 = next_query(&h.requests);
        let pb0 = next_query(&h.requests);
        deliver_to(&h.a, QueryKey(7), &mut vcx, pa0.tag, Ok(snapshot()));
        deliver_to(&h.b, QueryKey(8), &mut vcx, pb0.tag, Ok(snapshot()));
        let frame = h.a.read_with(&vcx, |t, _| t.frame.clone());
        // The shell's order: the change and its barrier in one pass, then
        // each tile's observer.
        frame.update(&mut vcx, |f, cx| {
            f.set_text(Some("A".into()));
            f.open_flip([QueryKey(7), QueryKey(8)], Instant::now());
            cx.notify();
        });
        let _pa1 = next_query(&h.requests);
        let pb1 = next_query(&h.requests);
        deliver_to(&h.b, QueryKey(8), &mut vcx, pb1.tag, Ok(snapshot2()));
        let old_texts = vec!["".to_string(), "L1".into(), "L2".into()];
        assert_eq!(shown_texts(&h.b, &vcx), old_texts, "B holds: A has not answered");

        vcx.update(|_, cx| crate::content::BlotterContent::for_tile(h.a.clone()).closed(cx));
        assert!(
            h.requests
                .try_iter()
                .any(|r| matches!(r, Request::Cancel { key } if key == QueryKey(7))),
            "closing A cancels its query"
        );
        vcx.run_until_parked();
        assert!(
            !frame.read_with(&vcx, |f, _| f.barrier_open()),
            "and answers the barrier before any deadline"
        );
        let new_texts = vec!["".to_string(), "M1".into(), "M2".into()];
        assert_eq!(
            shown_texts(&h.b, &vcx),
            new_texts,
            "B promotes in the pass the close released"
        );
    }
```

In `crates/geode-tile/src/following.rs` delete `abandon_forgets_the_question_and_keeps_the_stage`.

Run:
```sh
cargo test -p geode-marketdata -- a_panel_hidden_mid_flight_paints a_followed_change_while_hidden a_panel_hidden_mid_flip_still_answers closing_a_panel_mid_flip
cargo test -p geode-timeseries -- a_hidden_tile_keeps_its_query an_as_of_change_while_hidden closing_the_tile_mid_flip
cargo test -p geode-blotter -- a_tile_hidden_mid_flight_paints a_followed_change_while_hidden closing_a_tile_mid_flip
```
Expected: everything compiles (the modules still inherit the trait's no-op `closed`). The three close tests FAIL on their Cancel assertion; the market-data and timeseries hide tests FAIL on their "nothing is cancelled" assertion; `a_panel_hidden_mid_flip_still_answers_the_barrier` FAILS (the hide cancelled and forgot, so the reply never arrives). The blotter hide tests and every `a_followed_change_while_hidden_requeries_on_reshow` / `an_as_of_change_while_hidden_requeries_on_reshow` PASS already (they pin behaviour that does not change).

- [ ] **Step 2: Implement hide and close**

`crates/geode-tile/src/following.rs`: delete `abandon`.

Market-data `set_visible` becomes:

```rust
    /// Hiding cancels nothing and forgets nothing: the outstanding request
    /// finishes and its reply applies when it lands (it still answers any
    /// barrier it was enrolled in). Showing again requeries only if a
    /// counter this panel follows moved since it last asked. Closing is
    /// `closed`.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            // The catalog is where `:key`'s completions come from, and
            // nothing else asks for one on this panel's behalf.
            self.request_catalog_if_needed(cx);
            let now = self.versions(cx);
            if self.key.is_some() && self.following.follows_changed(now, Self::differs_on_followed)
            {
                self.requery(cx);
            }
        }
        self.changed(cx);
    }

    /// The shell is removing this panel: cancel the document request by key
    /// and answer any barrier still waiting on it.
    pub fn closed(&mut self, cx: &mut Context<Self>) {
        let key = QueryKey(self.id.0);
        self.data.cancel(key);
        self.following.close(&mut FrameDoor::new(&self.frame, cx), key);
    }
```

`crates/geode-marketdata/src/content.rs`, after `set_visible`:

```rust
    fn closed(&self, cx: &mut App) {
        self.tile.update(cx, |t, cx| t.closed(cx))
    }
```

Timeseries `set_visible`: delete the three hide-branch lines `self.data.cancel(QueryKey(self.id.0));`, its comment, and the `abandon` call with its comment. Keep `self.view_waiting = false;` and `self.in_flight.clear();` under this comment:

```rust
            // Hidden tiles hear no fetch completions (the shell broadcasts
            // `SeriesFetched` to visible tiles only), so fetch tracking is
            // dropped and every show refetches. The series query itself is
            // kept: its answer applies when it lands.
```

Replace the doc comment's last two sentences ("Hiding attempts query cancellation … emitted by the data tier.") with "Hiding keeps the series query; closing (`closed`) cancels it." Add:

```rust
    /// The shell is removing this tile: cancel the series query by key and
    /// answer any barrier still waiting on it. Fetches run on; their
    /// completions reach no one.
    pub fn closed(&mut self, cx: &mut Context<Self>) {
        let key = QueryKey(self.id.0);
        self.data.cancel(key);
        self.following.close(&mut FrameDoor::new(&self.frame, cx), key);
    }
```

and in `crates/geode-timeseries/src/content.rs` the same `closed` forwarder as market-data's.

Blotter: add to `impl BlotterTile` after `set_visible`:

```rust
    /// The shell is removing this tile: cancel its view query by key and
    /// answer any barrier still waiting on it. Hiding cancels nothing.
    pub fn closed(&mut self, cx: &mut Context<Self>) {
        let key = QueryKey(self.tile.0);
        self.data.cancel(key);
        self.following.close(&mut FrameDoor::new(&self.frame, cx), key);
    }
```

and the same forwarder in `crates/geode-blotter/src/content.rs`.

Stale comments to fix in market-data tests: `document_request`'s doc ("skipping the `Cancel` a `set_visible(false)` puts on the same channel") → "skipping any `Cancel` (a close puts one on the same channel)"; `a_stage_is_dropped_when_a_counter_the_panel_follows_has_moved`'s doc ("`set_visible(false)` cancels the request but a stage already taken stays") → "a hidden panel keeps a stage already taken"; `acted_is_none`'s doc ("the refusal and hidden-mid-flight rules") → "the refusal rule". Timeseries `requests()` doc: "since a hide's cancel is housekeeping" → "since a close's cancel is housekeeping".

- [ ] **Step 3: Run the new tests and the suites**

```sh
cargo test -p geode-marketdata -- a_panel_hidden_mid_flight_paints a_followed_change_while_hidden a_panel_hidden_mid_flip_still_answers closing_a_panel_mid_flip a_stage_held_when_the_panel_hides a_key_change_drops_what_was_staged
cargo test -p geode-timeseries -- a_hidden_tile_keeps_its_query an_as_of_change_while_hidden closing_the_tile_mid_flip
cargo test -p geode-blotter -- a_tile_hidden_mid_flight_paints a_followed_change_while_hidden closing_a_tile_mid_flip
cargo test -p geode-tile -p geode-marketdata -p geode-timeseries -p geode-blotter
```
Expected: all pass.

- [ ] **Step 4: Docs**

`docs/current/shell.md`, after the paragraph "A scope, grouping, or as-of change opens a flip barrier. …", add:

```markdown
Only visible occupants are barrier participants. Hiding a following tile (a
stack, dock or workspace switch) cancels nothing: its in-flight query
finishes, the reply applies when it lands and still answers any barrier the
tile was enrolled in, and on return the tile requeries only if a counter it
follows moved while it was hidden. Closing is different: removal calls
`TileContent::closed`, and a following tile cancels its query by key and
answers any open barrier still waiting on it, so closing a tile during a
scope, grouping or as-of change never holds the others to the deadline.
Tiles that submit no frame query (pricer, diagnostics) answer every barrier
at once. The rules live once, in `geode_tile::following`.
```

`docs/current/features.md`, "Common lifecycle": replace "Hidden tiles may release subscriptions. On becoming visible they compare followed versions and request anything stale." with "Hidden tiles may release subscriptions but keep an in-flight query, whose reply applies when it lands. On becoming visible they compare followed versions and request anything stale. A closed tile cancels its query and answers any flip barrier still waiting on it." Timeseries section: replace "Returning a hidden tile to visibility refetches its source pairs." with "Hiding keeps a series query in flight. Returning a hidden tile to visibility refetches its source pairs (hidden tiles hear no fetch completions), and the refetch's completion requeries."

`crates/geode-tile/README.md`: in the `following` row, drop nothing (it never listed `abandon`); confirm no mention of cancel-on-hide remains.

- [ ] **Step 5: Harness**

Re-aim the two hide entries to the new rule (their contract was deliberately reversed; the entries now guard it):

```zsh
run_mutation "mdtile: hiding a panel keeps its question" \
  crates/geode-marketdata/src/tile.rs \
  '        self.visible = visible;
        if visible {' \
  '        self.visible = visible;
        if !visible {
            self.data.cancel(QueryKey(self.id.0));
            self.following.close(&mut FrameDoor::new(&self.frame, cx), QueryKey(self.id.0));
        }
        if visible {' \
  geode-marketdata \
  a_panel_hidden_mid_flight_paints_the_reply_and_asks_nothing_on_reshow

run_mutation "timeseries: hiding keeps the query in flight" \
  crates/geode-timeseries/src/tile/mod.rs \
  '            self.view_waiting = false;
            self.in_flight.clear();' \
  '            self.data.cancel(QueryKey(self.id.0));
            self.following.close(&mut FrameDoor::new(&self.frame, cx), QueryKey(self.id.0));
            self.view_waiting = false;
            self.in_flight.clear();' \
  geode-timeseries \
  a_hidden_tile_keeps_its_query_and_a_shown_one_refetches_and_a_restored_one_refetches_once
```

(These replace the blocks named "mdtile: hiding a panel clears what it acted on" and "timeseries: a hidden tile cancels in flight"; the comment above each gains one line: "The rule was reversed: hiding keeps the query; the entry now guards that.")

New:

```zsh
run_mutation "mdtile: a close cancels the request by key" \
  crates/geode-marketdata/src/tile.rs \
  '        self.data.cancel(key);
        self.following.close(' \
  '        self.following.close(' \
  geode-marketdata closing_a_panel_mid_flip_cancels_its_request_and_releases_the_barrier

run_mutation "mdtile: a close answers the barrier" \
  crates/geode-tile/src/following.rs \
  '        barrier.arrive(key, closing_under)' \
  '        false' \
  geode-marketdata closing_a_panel_mid_flip_cancels_its_request_and_releases_the_barrier

run_mutation "mdtile: a panel hidden mid-flip still answers the barrier" \
  crates/geode-tile/src/following.rs \
  '                if !barrier.arrive(key, held_under) {' \
  '                if true {' \
  geode-marketdata a_panel_hidden_mid_flip_still_answers_the_barrier

run_mutation "timeseries: a close cancels the query by key" \
  crates/geode-timeseries/src/tile/mod.rs \
  '        self.data.cancel(key);
        self.following.close(' \
  '        self.following.close(' \
  geode-timeseries closing_the_tile_mid_flip_cancels_its_query_and_releases_the_barrier

run_mutation "timeseries: a close answers the barrier" \
  crates/geode-tile/src/following.rs \
  '        barrier.arrive(key, closing_under)' \
  '        false' \
  geode-timeseries closing_the_tile_mid_flip_cancels_its_query_and_releases_the_barrier

run_mutation "blotter: a close cancels the query by key" \
  crates/geode-blotter/src/tile.rs \
  '        self.data.cancel(key);
        self.following.close(' \
  '        self.following.close(' \
  geode-blotter closing_a_tile_mid_flip_cancels_its_query_and_releases_the_barrier

run_mutation "blotter: a close answers the barrier" \
  crates/geode-tile/src/following.rs \
  '        barrier.arrive(key, closing_under)' \
  '        false' \
  geode-blotter closing_a_tile_mid_flip_cancels_its_query_and_releases_the_barrier
```

The three "a close answers the barrier" entries share one helper line with the `geode-tile` entry from Task 1. They differ in package and filter, so the checker reports no REDUNDANT; each module route stays its own named test.

- [ ] **Step 6: Gates**

```sh
cargo test -p geode-tile -p geode-marketdata -p geode-timeseries -p geode-blotter -p geode-shell
cargo clippy -p geode-tile -p geode-marketdata -p geode-timeseries -p geode-blotter --all-targets -- -D warnings
cargo fmt --check
zsh scripts/mutation-check.sh --anchors-only
```

- [ ] **Step 7: Commit**

```bash
git add crates/geode-tile crates/geode-marketdata crates/geode-timeseries crates/geode-blotter docs/current/shell.md docs/current/features.md scripts/mutation-check.sh
git commit -m "feat(tiles): hiding lets a query finish; closing cancels and answers the barrier

Rewrites the tests that pinned cancel-on-hide:
a_tile_hidden_mid_flight_requeries_on_reshow (market-data) becomes
a_panel_hidden_mid_flight_paints_the_reply_and_asks_nothing_on_reshow;
a_hidden_tile_cancels_and_a_shown_one_requeries_and_a_restored_one_refetches_once
(timeseries) becomes
a_hidden_tile_keeps_its_query_and_a_shown_one_refetches_and_a_restored_one_refetches_once.
Removes the transitional FollowingQuery::abandon.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 8: Verify the nine re-aimed and new entries three ways** (as in Task 2 Step 8). The hide re-aims must report `caught` by their new tests. Hand application: the reintroduced cancel makes the "a hide is not a close" assertion fail.

---

### Task 8: Final sweep

**Files:**
- Modify: any doc the sweep finds stale.

- [ ] **Step 1: Search for stale wording**

```sh
grep -rn -i "hid.* cancel\|cancel.* hid\|cancels on hide\|hiding cancels\|query_in_flight\|arrive_and_release\|self_arrive(now" docs/current crates/*/README.md crates/*/src | grep -v "^crates/geode-pricer"
```
Expected: no hit describing market-data, timeseries or blotter hide as cancelling. The pricer's own hide-cancels-pricing is separate and stays. Fix any hit in the same commit.

- [ ] **Step 2: Workspace gates**

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo check -p geode-shell --features test-support --all-targets
zsh scripts/mutation-check.sh --anchors-only
```
Expected: all green; the anchor check exits 0.

- [ ] **Step 3: Commit (only if Step 1 changed anything)**

```bash
git add -A docs crates
git commit -m "docs: following-query sweep

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Self-review

**Spec coverage.**
- §3 table fields → `FollowingQuery` (Task 1).
- Tile supplies key, `differs` and apply → Tasks 2–4.
- Rule 1 → `on_flip` before the visibility check, plus `self_arrive` (Tasks 1–4; pinned by `a_stage_held_when_the_panel_hides_promotes_on_the_flip`).
- Rules 2, 3 and 4 → `begin`/`submitted`, `deliver` and `promote` (Task 1, module routes in Tasks 2–4).
- Rules 5 and 6 → Task 7 `set_visible`.
- Rule 7 → Task 6 hook and Task 7 `closed`.
- Rule 8 → Task 5.
- Keep-`acted` option → `Unanswered::KeepActed` (Tasks 1 and 4).
- Post-step hook → timeseries `release_view` after each promotion and delivery (Task 3; deviation 1).
- The blotter's `unwrap_or_default` is removed (Task 4).
- §5 helper tests → Task 1. The ten listed map to `a_stale_tag_is_not_an_arrival`, `a_failed_outcome_still_arrives`, `a_refusal_arrives_under_what_it_asked_then_forgets_it`, the module test `a_stage_held_when_the_panel_hides_promotes_on_the_flip` (the helper has no visibility input), `a_stage_survives_a_barrier_replaced_by_a_change_it_does_not_follow`, `a_stage_is_dropped_once_a_counter_it_follows_moved`, `self_arrive_waits_for_a_same_identity_query_in_flight`, `keep_acted_arrives_and_remembers_what_it_answered`, `a_held_result_promotes_on_the_flip_and_only_once`, and `close_supersedes_the_question_and_answers_the_barrier`.
- New module tests per following tile → Task 7.
- Harness re-aims, never deletions → Tasks 2–7.
- §6 docs → Tasks 1, 3, 4, 5, 6, 7 and 8.

**Placeholder scan.** No step leaves code to invent. The one text copied at execution time is the rustfmt-wrapped timeseries gate anchor, whose exact extent is specified.

**Type consistency.** The same names are used from Task 1 onward: `following`, `FollowingQuery::{begin, submitted, deliver, on_flip, follows_changed, self_arrive, reset, close, drop_stage, tag, acted, in_flight, in_flight_since, is_staged}`, `Delivered::{Stale, Apply, Held, Failed}`, `Promotion::{Empty, Superseded, Apply}`, `Unanswered::{Retry, KeepActed}`, `FrameDoor::new(&frame, cx)`, `arrive_immediately`, `TileContent::closed`, `Recorded::Closed`. `abandon` exists from Task 1 to Task 7 only.

**Unverified at plan time.** Exact rustfmt shapes of the anchored lines. Whether `self.frame.read(self.cx)` auto-reborrows (fallback given). Whether the two "promotion uses its own dependencies" re-aims are caught: they mutate `now` for the whole observer, and are verified in-task, stopping on SURVIVED.
