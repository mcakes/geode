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
use geode_shell::frame::{FrameRef, FrameVersions, FrameViewMut};
use gpui::App;

/// The flip barrier as a following query sees it. [`FrameViewMut`]
/// implements it directly, for pure tests; [`FrameDoor`] implements it over
/// the frame entity through a tile's [`FrameRef`]. Both read one tile's
/// versions: grouping and as-of from its workspace's lane, so a tile in a
/// pinned workspace never answers with the shared lane's counters, and the
/// scope generation from the link group it follows, or from that lane when
/// it follows none.
pub trait Barrier {
    /// This tile's reading of the frame's counters now. An open barrier
    /// holds each awaited key to the identity its own tile reads: the shell
    /// enrolls every tile under its reading (`Frame::open_flip_each`), a
    /// later scope, grouping or as-of change replaces the barrier, and a
    /// tile that starts or stops following a link group has its key
    /// re-identified. So these are what a closing tile answers with.
    fn current(&self) -> FrameVersions;
    /// Whether an open barrier waits for `key` at `versions`.
    fn wants(&self, key: QueryKey, versions: FrameVersions) -> bool;
    /// Record `key`'s arrival for `versions`; `true` exactly when this
    /// arrival released the barrier.
    fn arrive(&mut self, key: QueryKey, versions: FrameVersions) -> bool;
}

impl Barrier for FrameViewMut<'_> {
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

/// A tile's frame handle as a [`Barrier`]: versions are the tile's own
/// reading through [`FrameRef::read`], its lane's with the scope generation
/// of the link group it follows. An arrival that releases the barrier
/// notifies the frame, so every other tile's observer sees the `flip` bump
/// and promotes what it staged in the same pass; without the notification
/// they would sit on their stages until an unrelated frame change.
pub struct FrameDoor<'a> {
    frame: &'a FrameRef,
    cx: &'a mut App,
}

impl<'a> FrameDoor<'a> {
    pub fn new(frame: &'a FrameRef, cx: &'a mut App) -> FrameDoor<'a> {
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
            // The barrier is frame-wide; only the versions are per tile.
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
    /// The current request's answer, but a counter the tile follows moved
    /// since it asked: dropped, like a superseded stage. The query is
    /// answered; `acted` still names the old versions, so the next show or
    /// frame change asks again.
    Superseded,
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
    /// tile follows only some counters: the barrier holds this tile's key
    /// to a flip identity, so answering it needs the versions the request
    /// was made under.
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
    fn promote(
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
            && self
                .acted
                .is_some_and(|asked| asked.same_flip_identity(now));
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
                    // The same check promotion makes: a counter the tile
                    // follows moved since it asked (in practice only while
                    // hidden: a visible tile requeries on the change, and
                    // the old tag goes stale), so the answer is to a
                    // question nobody is asking. Applying it would paint,
                    // and run any side effect of applying, under the old
                    // versions. Not wanted by the barrier, so not an arrival
                    // either.
                    if asked.is_some_and(|under| differs(under, now)) {
                        return Delivered::Superseded;
                    }
                    return Delivered::Apply(value);
                };
                self.staged = Some((value, held_under));
                // This arrival may be what empties the barrier; promote at
                // once rather than waiting for the flip to reach the tile's
                // observer on a later pass.
                if !barrier.arrive(key, held_under) {
                    return Delivered::Held;
                }
                // Released at once but superseded: a followed counter moved
                // since the question was asked, so there is nothing to apply
                // and nothing left held. Reporting it as held would let the
                // caller treat a dropped answer as a pending one.
                match self.promote(now, differs) {
                    Promotion::Apply(value) => Delivered::Apply(value),
                    Promotion::Superseded => Delivered::Superseded,
                    Promotion::Empty => Delivered::Held,
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
    use geode_core::link::Group;
    use geode_core::scope::Scope;
    use geode_core::scopes::SavedScopes;
    use geode_shell::frame::{FLIP_DEADLINE, Frame};
    use geode_shell::tiling::{TileId, WorkspaceIx};
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
    fn flip_scope(
        f: &mut FrameViewMut<'_>,
        text: &str,
        keys: &[QueryKey],
        at: Instant,
    ) -> FrameVersions {
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
        let mut frame = fresh_frame();
        // Pure tests run in the shared lane: an unpinned workspace's.
        let mut f = frame.shared_mut();
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
        let mut frame = fresh_frame();
        // Pure tests run in the shared lane: an unpinned workspace's.
        let mut f = frame.shared_mut();
        let t0 = Instant::now();
        let v = flip_scope(&mut f, "a", &[K], t0);
        let mut q = FollowingQuery::<u32>::new();
        let tag = q.begin(v, t0);
        assert_eq!(
            q.deliver(tag, Err("boom"), v, follows_config, &mut f, K),
            Delivered::Failed("boom")
        );
        assert!(
            !f.barrier_open(),
            "one broken tile never holds the rest open"
        );
        assert!(!q.in_flight());
    }

    #[test]
    fn a_refusal_arrives_under_what_it_asked_then_forgets_it() {
        let mut frame = fresh_frame();
        // Pure tests run in the shared lane: an unpinned workspace's.
        let mut f = frame.shared_mut();
        let t0 = Instant::now();
        let v = flip_scope(&mut f, "a", &[K, OTHER], t0);
        let mut q = FollowingQuery::<u32>::new();
        q.begin(v, t0);
        q.submitted(false, Unanswered::Retry, &mut f, K);
        assert!(
            !f.barrier_wants(K, v),
            "nothing is coming, so it answered at once"
        );
        assert!(f.barrier_open(), "for itself only");
        assert_eq!(q.acted(), None, "forgotten, so the next change retries");
        assert!(!q.in_flight());
    }

    #[test]
    fn keep_acted_arrives_and_remembers_what_it_answered() {
        let mut frame = fresh_frame();
        // Pure tests run in the shared lane: an unpinned workspace's.
        let mut f = frame.shared_mut();
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
        let mut frame = fresh_frame();
        // Pure tests run in the shared lane: an unpinned workspace's.
        let mut f = frame.shared_mut();
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
        let mut frame = fresh_frame();
        // Pure tests run in the shared lane: an unpinned workspace's.
        let mut f = frame.shared_mut();
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
        let mut frame = fresh_frame();
        // Pure tests run in the shared lane: an unpinned workspace's.
        let mut f = frame.shared_mut();
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
        let mut frame = fresh_frame();
        // Pure tests run in the shared lane: an unpinned workspace's.
        let mut f = frame.shared_mut();
        let v = f.versions();
        let mut q = FollowingQuery::<u32>::new();
        let tag = q.begin(v, Instant::now());
        assert_eq!(
            q.deliver::<&str>(tag, Ok(2), v, follows_config, &mut f, K),
            Delivered::Apply(2)
        );
    }

    /// The direct path agrees with promotion: an answer asked under
    /// versions a followed counter has since left is dropped, not applied,
    /// and is no arrival; a change the tile does not follow applies.
    #[test]
    fn a_delivery_asked_before_a_followed_change_is_superseded() {
        let mut frame = fresh_frame();
        // Pure tests run in the shared lane: an unpinned workspace's.
        let mut f = frame.shared_mut();
        let asked = f.versions();
        let mut q = FollowingQuery::<u32>::new();
        let tag = q.begin(asked, Instant::now());
        assert!(f.set_text(Some("a".into())), "a change it does not follow");
        assert_eq!(
            q.deliver::<&str>(tag, Ok(1), f.versions(), follows_config, &mut f, K),
            Delivered::Apply(1)
        );
        let tag = q.begin(f.versions(), Instant::now());
        f.note_config_reloaded();
        assert_eq!(
            q.deliver::<&str>(tag, Ok(2), f.versions(), follows_config, &mut f, K),
            Delivered::Superseded
        );
        assert!(!q.in_flight(), "answered");
        assert!(!q.is_staged(), "and nothing held");
        assert!(
            q.follows_changed(f.versions(), follows_config),
            "so the next show asks again"
        );
    }

    /// A reply whose own arrival releases the barrier, asked before a
    /// followed counter moved (a change that opens no barrier of its own),
    /// is dropped: it answers the barrier but applies nothing.
    #[test]
    fn a_releasing_delivery_asked_before_a_followed_change_is_superseded() {
        let mut frame = fresh_frame();
        // Pure tests run in the shared lane: an unpinned workspace's.
        let mut f = frame.shared_mut();
        let t0 = Instant::now();
        let v = flip_scope(&mut f, "a", &[K], t0);
        let mut q = FollowingQuery::<u32>::new();
        let tag = q.begin(v, t0);
        f.note_config_reloaded();
        assert!(
            f.barrier_wants(K, v),
            "the barrier still waits on the reply"
        );
        assert_eq!(
            q.deliver::<&str>(tag, Ok(4), f.versions(), follows_config, &mut f, K),
            Delivered::Superseded
        );
        assert!(!f.barrier_open(), "its arrival released the barrier");
        assert!(!q.is_staged(), "and nothing is left held");
        assert!(!q.in_flight());
    }

    #[test]
    fn a_stage_survives_a_barrier_replaced_by_a_change_it_does_not_follow() {
        let mut frame = fresh_frame();
        // Pure tests run in the shared lane: an unpinned workspace's.
        let mut f = frame.shared_mut();
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
        assert!(
            f.sweep(t0 + FLIP_DEADLINE),
            "the deadline releases the replacement"
        );
        assert_eq!(q.on_flip(f.versions(), follows_config), Promotion::Apply(1));
    }

    #[test]
    fn a_stage_is_dropped_once_a_counter_it_follows_moved() {
        let mut frame = fresh_frame();
        // Pure tests run in the shared lane: an unpinned workspace's.
        let mut f = frame.shared_mut();
        let t0 = Instant::now();
        let v = flip_scope(&mut f, "a", &[K, OTHER], t0);
        let mut q = FollowingQuery::<u32>::new();
        let tag = q.begin(v, t0);
        q.deliver::<&str>(tag, Ok(1), v, follows_config, &mut f, K);
        f.note_config_reloaded();
        assert!(
            f.arrived(OTHER, v),
            "configuration is not part of the flip identity"
        );
        assert_eq!(
            q.on_flip(f.versions(), follows_config),
            Promotion::Superseded
        );
    }

    #[test]
    fn self_arrive_waits_for_a_same_identity_query_in_flight() {
        let mut frame = fresh_frame();
        // Pure tests run in the shared lane: an unpinned workspace's.
        let mut f = frame.shared_mut();
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
        assert!(
            !f.barrier_wants(QueryKey(9), v),
            "an idle tile answers at once"
        );
    }

    #[test]
    fn follows_changed_is_true_before_the_first_question() {
        let mut frame = fresh_frame();
        // Pure tests run in the shared lane: an unpinned workspace's.
        let mut f = frame.shared_mut();
        let mut q = FollowingQuery::<u32>::new();
        assert!(q.follows_changed(f.versions(), follows_config));
        q.begin(f.versions(), Instant::now());
        assert!(!q.follows_changed(f.versions(), follows_config));
        f.note_config_reloaded();
        assert!(q.follows_changed(f.versions(), follows_config));
    }

    #[test]
    fn reset_forgets_the_question_and_its_stage() {
        let mut frame = fresh_frame();
        // Pure tests run in the shared lane: an unpinned workspace's.
        let mut f = frame.shared_mut();
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
    fn close_supersedes_the_question_and_answers_the_barrier() {
        let mut frame = fresh_frame();
        // Pure tests run in the shared lane: an unpinned workspace's.
        let mut f = frame.shared_mut();
        let t0 = Instant::now();
        let v = flip_scope(&mut f, "a", &[K, OTHER], t0);
        let mut q = FollowingQuery::<u32>::new();
        let tag = q.begin(v, t0);
        assert!(
            !q.close(&mut f, K),
            "the other tile still holds the barrier"
        );
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
        let mut frame = fresh_frame();
        // Pure tests run in the shared lane: an unpinned workspace's.
        let mut f = frame.shared_mut();
        let t0 = Instant::now();
        let v = flip_scope(&mut f, "a", &[K], t0);
        let mut q = FollowingQuery::<u32>::new();
        q.begin(v, t0);
        assert!(q.close(&mut f, K));
        assert!(!f.barrier_open());
    }

    #[test]
    fn close_with_nothing_open_changes_nothing() {
        let mut frame = fresh_frame();
        // Pure tests run in the shared lane: an unpinned workspace's.
        let mut f = frame.shared_mut();
        let before = f.versions();
        let mut q = FollowingQuery::<u32>::new();
        assert!(!q.close(&mut f, K));
        assert_eq!(f.versions(), before);
    }

    #[test]
    fn arrive_immediately_answers_only_a_barrier_that_wants_the_key() {
        let mut frame = fresh_frame();
        // Pure tests run in the shared lane: an unpinned workspace's.
        let mut f = frame.shared_mut();
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
        let v = frame.update(cx, |f, _| {
            flip_scope(&mut f.shared_mut(), "a", &[K, OTHER], t0)
        });
        let tile = FrameRef::new(frame.clone(), WorkspaceIx::FIRST);
        cx.update(|cx| {
            let mut door = FrameDoor::new(&tile, cx);
            assert!(!door.arrive(K, v), "the other tile still holds it");
        });
        cx.run_until_parked();
        assert_eq!(
            heard.get(),
            0,
            "an arrival that releases nothing is not news"
        );
        cx.update(|cx| {
            let mut door = FrameDoor::new(&tile, cx);
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

    /// A tile in a pinned workspace answers the barrier with its own lane's
    /// counters: reading the shared lane would hand a pinned tile versions
    /// it never followed, and it would stage or promote against the wrong
    /// scope.
    #[gpui::test]
    fn the_door_reads_the_tiles_own_lane(cx: &mut gpui::TestAppContext) {
        let frame = cx.update(|cx| cx.new(|_| fresh_frame()));
        let ws = WorkspaceIx::new(2).unwrap();
        let (pinned, shared) = frame.update(cx, |f, _| {
            assert!(f.pin(ws));
            assert!(
                f.view_mut(ws).set_text(Some("a".into())),
                "a pinned-lane edit"
            );
            (f.view(ws).versions(), f.shared().versions())
        });
        assert_ne!(pinned, shared, "the edit moved only the pinned lane");
        let tile = FrameRef::new(frame.clone(), ws);
        cx.update(|cx| {
            let door = FrameDoor::new(&tile, cx);
            assert_eq!(door.current(), pinned);
        });
    }

    /// A tile that follows a link group answers the barrier with the
    /// group's scope generation. Reading its workspace's lane instead would
    /// hand it an identity for a scope it does not query under: it would
    /// requery on lane edits it never reads and sit still when its group
    /// moved.
    #[gpui::test]
    fn the_door_reads_a_followers_group_scope(cx: &mut gpui::TestAppContext) {
        let frame = cx.update(|cx| cx.new(|_| fresh_frame()));
        let ws = WorkspaceIx::FIRST;
        let id = TileId(7);
        frame.update(cx, |f, _| assert!(f.follow(id, Some(Group::A))));
        let tile = FrameRef::for_tile(frame.clone(), ws, id);
        let workspace = FrameRef::new(frame.clone(), ws);
        assert_eq!(tile.tile(), Some(id));
        assert_eq!(workspace.tile(), None);
        let wrote = tile.update(cx, |f, _| {
            f.set_scope(Scope::one("underlying_ref", "SPX.Z"))
        });
        assert!(wrote);
        let (group, lane) = frame.update(cx, |f, _| {
            assert_eq!(
                f.group_scope(Group::A).sole("underlying_ref"),
                Some("SPX.Z"),
                "a follower's handle writes its group"
            );
            assert!(f.view(ws).scope().is_empty(), "and not its lane");
            (
                f.group_scope_gens()[Group::A.index()],
                f.view(ws).versions().scope,
            )
        });
        assert_ne!(group, lane);
        cx.update(|cx| {
            assert_eq!(FrameDoor::new(&tile, cx).current().scope, group);
            assert_eq!(FrameDoor::new(&workspace, cx).current().scope, lane);
        });
    }
}
