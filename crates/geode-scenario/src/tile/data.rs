//! The query chain and following. Three reads run in sequence under ONE
//! `FollowingQuery` tag, all keyed by the tile: the axis domains (one
//! distinct per axis, unscoped, restricted to the dataset, only when they
//! are stale), the main grouped query at full depth, and the totals query
//! for a matrix with a Σ row. The query pool keeps one request per key and
//! the flip barrier one entry per key, so concurrent reads would supersede
//! each other; in sequence, the trio is handed to `FollowingQuery::deliver`
//! once and the barrier sees one arrival. Hiding keeps the chain in flight;
//! closing cancels by key and answers any open barrier through the
//! deferred door (a notify from a draw-time door is dropped).

use super::ScenarioTile;
use crate::core::pivot::{AxisDomains, Mode, Pivot};
use crate::core::session::TileAsOf;
use geode_core::query::{AsOf, DistinctOutcome, DistinctParams, QueryOutcome};
use geode_core::scenario::DiffBase;
use geode_core::scope::Scope;
use geode_core::snapshot::Snapshot;
use geode_data::{QueryParams, Refusal};
use geode_shell::frame::FrameVersions;
use geode_tile::following::{Arrival, DeferredDoor, Delivered, FrameDoor, Promotion, Unanswered};
use geode_tile::header::DatasetTime;
use geode_tile::notice::Notice;
use geode_tile::stale::StaleTimer;
use gpui::{App, Context};
use std::sync::Arc;
use std::time::Instant;

/// What the domains were read under. A different stamp means stale: a
/// publication on the dataset or another as-of; a scope change is not one,
/// since the domains are read unscoped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DomainStamp {
    pub data: u64,
    pub as_of: AsOf,
}

/// The last domains read, and what they were read under.
#[derive(Debug, Default)]
pub(crate) struct Domains {
    pub labels: AxisDomains,
    pub stamp: Option<DomainStamp>,
}

/// The chain's answer: everything the pivot needs.
#[derive(Debug)]
pub(crate) struct Fetched {
    pub domains: AxisDomains,
    pub main: Arc<Snapshot>,
    pub totals: Option<Arc<Snapshot>>,
}

/// Which read of the chain is out. An outcome under another tag, or of
/// the wrong kind for the step, is dropped.
#[derive(Debug)]
pub(crate) enum Fetch {
    Idle,
    Domains {
        tag: u64,
        /// The axes still to read, in order.
        remaining: Vec<String>,
        got: AxisDomains,
        stamp: DomainStamp,
        scope: Scope,
        as_of: AsOf,
    },
    Main {
        tag: u64,
        domains: AxisDomains,
        scope: Scope,
        as_of: AsOf,
    },
    Totals {
        tag: u64,
        domains: AxisDomains,
        main: Arc<Snapshot>,
    },
}

impl Fetch {
    fn tag(&self) -> Option<u64> {
        match self {
            Fetch::Idle => None,
            Fetch::Domains { tag, .. } | Fetch::Main { tag, .. } | Fetch::Totals { tag, .. } => {
                Some(*tag)
            }
        }
    }
}

/// The counters the picture depends on: the watched publications and
/// configuration always (a named expression's definition is config); the
/// scope unless the tile reads `:unscoped`; the as-of unless the tile is
/// pinned. Grouping never: the shape fixes it.
fn differs(scoped: bool, follows_as_of: bool) -> impl Fn(FrameVersions, FrameVersions) -> bool {
    move |a, b| {
        (scoped && a.scope != b.scope)
            || (follows_as_of && a.as_of != b.as_of)
            || a.data != b.data
            || a.config != b.config
    }
}

impl ScenarioTile {
    fn differs(&self) -> impl Fn(FrameVersions, FrameVersions) -> bool + use<> {
        differs(
            !self.state.unscoped,
            matches!(self.state.as_of, TileAsOf::Follow),
        )
    }

    fn versions(&self, cx: &App) -> FrameVersions {
        self.frame.read(cx).versions_for(&self.watches)
    }

    /// The scope and as-of the next chain runs under; `Err` is an
    /// unresolved name the caller shows instead of querying.
    fn query_inputs(&self, cx: &App) -> Result<(Scope, AsOf), String> {
        let frame = self.frame.read(cx);
        let scope = if self.state.unscoped {
            self.state.filter.clone()
        } else {
            frame.effective_scope(&self.state.filter)?
        };
        let as_of = match &self.state.as_of {
            TileAsOf::Follow => frame.as_of().clone(),
            TileAsOf::Pinned(p) => p.clone(),
        };
        Ok((scope, as_of))
    }

    /// The scope the header reads out: what the next query would run under.
    pub(crate) fn effective_scope_for_title(&self, cx: &App) -> Scope {
        if self.state.unscoped {
            return self.state.filter.clone();
        }
        self.frame
            .read(cx)
            .effective_scope(&self.state.filter)
            .unwrap_or_else(|_| self.state.filter.clone())
    }

    pub(crate) fn requery(&mut self, cx: &mut Context<Self>) {
        self.requery_with(Arrival::Now, cx);
    }

    /// Start a chain: the domains when stale, else the main query. The
    /// barrier is answered by the chain's last delivery, or here when
    /// nothing went out.
    pub(crate) fn requery_with(&mut self, arrival: Arrival, cx: &mut Context<Self>) {
        let now = Instant::now();
        let key = self.key();
        let versions = self.versions(cx);
        let (scope, as_of) = match self.query_inputs(cx) {
            Ok(inputs) => inputs,
            Err(message) => {
                // An unresolved name: say so and answer the barrier without
                // asking. `KeepActed`: the tile's retry is the config change
                // that defines the name, which it follows; forgetting would
                // re-run the failing request on every frame notification.
                self.set_error(Notice::danger(message));
                self.fetch = Fetch::Idle;
                self.following.begin(versions, now);
                self.following.submitted(
                    false,
                    Unanswered::KeepActed,
                    &mut arrival.door(&self.frame, cx),
                    key,
                );
                cx.notify();
                return;
            }
        };
        // `begin` also drops whatever was staged for the previous question.
        let tag = self.following.begin(versions, now);
        let stamp = DomainStamp {
            data: versions.data,
            as_of: as_of.clone(),
        };
        let queued = if self.domains.stamp.as_ref() != Some(&stamp) {
            // The axes the pivot lays out, rows first; never empty, since
            // a shape always names its rows axis.
            let mut remaining = self.shape.grouping();
            let first = remaining.remove(0);
            let queued = self.submit_distinct(tag, &first, &as_of);
            if queued.is_ok() {
                self.fetch = Fetch::Domains {
                    tag,
                    remaining,
                    got: AxisDomains::default(),
                    stamp,
                    scope,
                    as_of,
                };
            }
            queued
        } else {
            let queued = self.submit_main(tag, &scope, &as_of);
            if queued.is_ok() {
                self.fetch = Fetch::Main {
                    tag,
                    domains: self.domains.labels.clone(),
                    scope,
                    as_of,
                };
            }
            queued
        };
        if let Err(refusal) = &queued {
            self.fetch = Fetch::Idle;
            self.set_error(Notice::danger(refused(refusal)));
        }
        self.following.submitted(
            queued.is_ok(),
            Unanswered::Retry,
            &mut arrival.door(&self.frame, cx),
            key,
        );
        cx.notify();
    }

    /// One axis's domain: unscoped, so labels outside the scope still
    /// paint, and restricted to the shape's dataset.
    fn submit_distinct(&self, tag: u64, axis: &str, as_of: &AsOf) -> Result<(), Refusal> {
        self.data.distinct(DistinctParams {
            key: self.key(),
            tag,
            column: axis.to_string(),
            scope: Scope::default(),
            as_of: as_of.clone(),
            dataset: Some(self.shape.dataset.clone()),
        })
    }

    /// The main query at full depth: the subtotal rows and the total row
    /// ride along, so no Σ is computed from cells.
    fn submit_main(&self, tag: u64, scope: &Scope, as_of: &AsOf) -> Result<(), Refusal> {
        self.submit_grouped(tag, self.shape.grouping(), scope, as_of)
    }

    fn submit_grouped(
        &self,
        tag: u64,
        grouping: Vec<String>,
        scope: &Scope,
        as_of: &AsOf,
    ) -> Result<(), Refusal> {
        self.data.query(QueryParams {
            key: self.key(),
            tag,
            submitted: Instant::now(),
            view: self.shape.view_name(),
            max_depth: grouping.len(),
            grouping: Some(grouping),
            scope: scope.clone(),
            as_of: as_of.clone(),
        })
    }

    /// A domain read's outcome: the next axis, or the main query, under the
    /// same tag.
    pub(crate) fn deliver_distinct(&mut self, outcome: DistinctOutcome, cx: &mut Context<Self>) {
        if self.fetch.tag() != Some(outcome.tag) {
            return;
        }
        let Fetch::Domains {
            tag,
            mut remaining,
            mut got,
            stamp,
            scope,
            as_of,
        } = std::mem::replace(&mut self.fetch, Fetch::Idle)
        else {
            return;
        };
        let values = match outcome.values {
            Ok(values) => values,
            Err(e) => return self.hand_over(tag, Err(e), cx),
        };
        got.insert(outcome.column, values.into_iter().map(|(v, _)| v).collect());
        if remaining.is_empty() {
            self.domains = Domains {
                labels: got.clone(),
                stamp: Some(stamp),
            };
            match self.submit_main(tag, &scope, &as_of) {
                Ok(()) => {
                    self.fetch = Fetch::Main {
                        tag,
                        domains: got,
                        scope,
                        as_of,
                    }
                }
                Err(r) => self.refuse_step(&r, cx),
            }
        } else {
            let next = remaining.remove(0);
            match self.submit_distinct(tag, &next, &as_of) {
                Ok(()) => {
                    self.fetch = Fetch::Domains {
                        tag,
                        remaining,
                        got,
                        stamp,
                        scope,
                        as_of,
                    }
                }
                Err(r) => self.refuse_step(&r, cx),
            }
        }
    }

    /// The main query's outcome leads to the totals query for a matrix with
    /// a Σ row; the last snapshot completes the chain.
    pub(crate) fn deliver_query(&mut self, outcome: QueryOutcome, cx: &mut Context<Self>) {
        if self.fetch.tag() != Some(outcome.tag) {
            return;
        }
        match std::mem::replace(&mut self.fetch, Fetch::Idle) {
            Fetch::Main {
                tag,
                domains,
                scope,
                as_of,
            } => match outcome.snapshot {
                Err(e) => self.hand_over(tag, Err(e), cx),
                Ok(main) => match self.shape.totals_grouping() {
                    None => self.hand_over(
                        tag,
                        Ok(Arc::new(Fetched {
                            domains,
                            main,
                            totals: None,
                        })),
                        cx,
                    ),
                    Some(grouping) => match self.submit_grouped(tag, grouping, &scope, &as_of) {
                        Ok(()) => self.fetch = Fetch::Totals { tag, domains, main },
                        Err(r) => self.refuse_step(&r, cx),
                    },
                },
            },
            Fetch::Totals { tag, domains, main } => match outcome.snapshot {
                Err(e) => self.hand_over(tag, Err(e), cx),
                Ok(totals) => self.hand_over(
                    tag,
                    Ok(Arc::new(Fetched {
                        domains,
                        main,
                        totals: Some(totals),
                    })),
                    cx,
                ),
            },
            // A distinct is out: a query under its tag is not its answer.
            other @ Fetch::Domains { .. } => self.fetch = other,
            Fetch::Idle => {}
        }
    }

    /// A later step of the chain was refused. No outcome is owed, so the
    /// barrier is answered here, and the versions asked under are
    /// forgotten (`Retry`): the next frame notification asks again, rather
    /// than self-arriving while the notice stands.
    fn refuse_step(&mut self, refusal: &Refusal, cx: &mut Context<Self>) {
        let key = self.key();
        self.fetch = Fetch::Idle;
        self.set_error(Notice::danger(refused(refusal)));
        self.following.submitted(
            false,
            Unanswered::Retry,
            &mut FrameDoor::new(&self.frame, cx),
            key,
        );
        cx.notify();
    }

    /// Hand the chain's answer, or its failure, to the barrier: the one
    /// arrival the chain makes.
    fn hand_over(
        &mut self,
        tag: u64,
        result: Result<Arc<Fetched>, String>,
        cx: &mut Context<Self>,
    ) {
        let now = self.versions(cx);
        let key = self.key();
        let differs = self.differs();
        let delivered = self.following.deliver(
            tag,
            result,
            now,
            differs,
            &mut FrameDoor::new(&self.frame, cx),
            key,
        );
        match delivered {
            Delivered::Apply(fetched) => self.install(fetched, cx),
            // The last picture stays; the error stands until a delivery
            // replaces it (spec §7).
            Delivered::Failed(e) => self.set_error(Notice::danger(e)),
            // Held: the flip's promotion installs it. Stale or superseded:
            // an answer to a question nobody is asking.
            Delivered::Held | Delivered::Stale | Delivered::Superseded => {}
        }
        cx.notify();
    }

    /// Put a fetched picture on screen. The pivot is built first: only a
    /// picture that fits the shape replaces the last one, clears the error
    /// it answers and arms the stale timer on its source times. One that
    /// does not fit (a stale or foreign delivery) is an error, and the last
    /// picture, its times and what `c`/`m` rebuild over all stay (spec §7).
    fn install(&mut self, fetched: Arc<Fetched>, cx: &mut Context<Self>) {
        match self.build(&fetched) {
            Ok(pivot) => {
                self.error = None;
                self.picture = Some(fetched.clone());
                self.prepare_times(cx);
                self.show_pivot(&fetched.main, pivot, cx);
                self.arm_stale(cx);
            }
            Err(e) => self.set_error(Notice::danger(format!("picture: {e}"))),
        }
        cx.notify();
    }

    /// The painted picture's dataset times, labelled on the display clock:
    /// at install and when the clock changes.
    pub(crate) fn prepare_times(&mut self, cx: &App) {
        self.times = match &self.picture {
            Some(p) => DatasetTime::from_provenance(p.main.provenance(), super::clock(cx)),
            None => Vec::new(),
        };
    }

    fn build(&self, p: &Fetched) -> Result<Pivot, String> {
        Pivot::build(
            &p.main,
            p.totals.as_deref(),
            &self.shape,
            self.mode(),
            &p.domains,
        )
    }

    /// Paint `pivot` and recompute the notice the picture carries.
    fn show_pivot(&mut self, main: &Snapshot, pivot: Pivot, cx: &mut Context<Self>) {
        self.standing = self.picture_notice(main, &pivot);
        let pivot = Arc::new(pivot);
        let colours = Arc::clone(&self.colours.borrow());
        self.table.update(cx, |t, cx| {
            // The definitions a `format.color` resolves against, refreshed
            // with the picture, so a reloaded `colours.toml` reaches the
            // paint on the requery the reload triggers.
            t.delegate_mut().set_colours(colours);
            t.delegate_mut().set_picture(Some(pivot));
            // `refresh` re-lays the columns and, with them, the two-level
            // header's groups (`refresh_header_layout`'s own work).
            t.refresh(cx);
        });
        self.prune_dismissed();
        // The currency chip reads the new pivot's mixed flag.
        self.prepare_chips(cx);
    }

    /// Rebuild the pivot for the current mode over the last picture and
    /// recompute the notice the picture itself carries (the missing diff
    /// base, no data yet). A query error is not the picture's and stays.
    ///
    /// A picture that cannot be built in the new mode (a snapshot without
    /// the USD twin `c` reads) puts `previous` back and refuses the verb:
    /// the chips and the cells keep describing the same picture, never a
    /// new mode over the old numbers.
    pub(crate) fn rebuild_pivot(&mut self, previous: Mode, cx: &mut Context<Self>) {
        let Some(p) = self.picture.clone() else {
            return;
        };
        match self.build(&p) {
            Ok(pivot) => self.show_pivot(&p.main, pivot, cx),
            Err(e) => {
                self.state.currency = previous.currency;
                self.state.diff = previous.diff;
                self.refuse(e, cx);
            }
        }
        cx.notify();
    }

    /// The mode the installed picture is drawn in.
    pub(crate) fn mode(&self) -> Mode {
        Mode {
            currency: self.state.currency,
            diff: self.state.diff,
        }
    }

    /// Spec §7: diff with no base row in scope is a warning naming the axis
    /// and base. No leaf, and a dataset freshness with neither a generation
    /// nor a source time, is "no data yet": a historical read carries no
    /// generation but its resolved time, a live one both once anything is
    /// published. An empty scope over published data is an empty grid and
    /// says nothing.
    fn picture_notice(&self, main: &Snapshot, pivot: &Pivot) -> Option<Notice> {
        if pivot.diff_base_missing {
            let base = match self.shape.diff.as_ref().map(|d| &d.base) {
                Some(DiffBase::Number(n)) => n.to_string(),
                Some(DiffBase::Text(t)) => t.clone(),
                None => String::new(),
            };
            return Some(Notice::warning(format!(
                "diff: no base row ({} = {base}) in scope",
                self.shape.rows
            )));
        }
        let leaf_depth = self.shape.grouping().len();
        let has_leaves = (0..main.rows()).any(|r| main.depth_of_row(r) == Some(leaf_depth));
        let published = main
            .provenance()
            .datasets
            .iter()
            .find(|f| f.dataset == self.shape.dataset)
            .is_some_and(|f| f.generation.is_some() || f.as_of.is_some());
        (!has_leaves && !published)
            .then(|| Notice::status(format!("no {} data yet", self.shape.dataset)))
    }

    /// A query error or refusal: it stands until a picture installs.
    fn set_error(&mut self, notice: Notice) {
        self.error = Some(notice);
        self.prune_dismissed();
    }

    /// Forget dismissals of notices no longer reported, so one that comes
    /// back is news.
    fn prune_dismissed(&mut self) {
        let reported = self.reported_notices();
        self.dismissed.prune(reported.iter());
    }

    /// The frame observer.
    pub(crate) fn on_frame_changed(&mut self, cx: &mut Context<Self>) {
        // The title reads the frame's scope, whatever else happens.
        cx.notify();
        if self.sync_following(cx) {
            if self.visible {
                self.requery(cx);
            }
            return;
        }
        // Promote before the visibility check, so a tile hidden after
        // staging still lands its answer.
        let now = self.versions(cx);
        if let Promotion::Apply(fetched) = self.following.on_flip(now, self.differs()) {
            self.install(fetched, cx);
        }
        if !self.visible {
            return;
        }
        if self.following.follows_changed(now, self.differs()) {
            // The barrier is answered on delivery, under the versions this
            // chain was asked with.
            self.requery(cx);
        } else {
            let key = self.key();
            self.following
                .self_arrive(&mut FrameDoor::new(&self.frame, cx), key, now);
        }
    }

    /// A change of followed group, which moves no version when neither
    /// scope was ever written: the old question is forgotten and the
    /// picture, which answered it, goes (spec §5.2). `true` when it changed.
    fn sync_following(&mut self, cx: &mut Context<Self>) -> bool {
        let following = self.frame.read(cx).following();
        if following == self.last_following {
            return false;
        }
        self.last_following = following;
        self.fetch = Fetch::Idle;
        self.following.reset();
        self.picture = None;
        self.times.clear();
        self.standing = None;
        self.prune_dismissed();
        self.table.update(cx, |t, cx| {
            t.delegate_mut().set_picture(None);
            t.refresh(cx);
        });
        self.prepare_chips(cx);
        cx.notify();
        true
    }

    /// Showing asks again only when nothing has been asked or a counter the
    /// picture follows moved while hidden. Hiding keeps the chain in
    /// flight; its answer lands when it comes. An arrival made here is
    /// deferred: the shell calls this while it draws.
    pub(crate) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            let changed = self.sync_following(cx);
            let now = self.versions(cx);
            if changed || self.following.follows_changed(now, self.differs()) {
                self.requery_with(Arrival::Deferred, cx);
            } else {
                // Nothing to ask, but a flip may be waiting on this tile.
                let key = self.key();
                self.following
                    .self_arrive(&mut DeferredDoor::new(&self.frame, cx), key, now);
            }
        }
        self.arm_stale(cx);
        cx.notify();
    }

    /// The shell is removing this tile: cancel by key (whichever read of
    /// the chain is out), then answer any barrier still waiting on it,
    /// deferred, since this runs inside the shell's draw.
    pub(crate) fn closed(&mut self, cx: &mut Context<Self>) {
        let key = self.key();
        self.data.cancel(key);
        self.fetch = Fetch::Idle;
        self.following
            .close(&mut DeferredDoor::new(&self.frame, cx), key);
        self.stale_timer.disarm();
    }

    fn stale_slot(t: &mut Self) -> &mut StaleTimer {
        &mut t.stale_timer
    }

    /// Wake when a painted source time turns stale; nothing while hidden.
    fn arm_stale(&mut self, cx: &mut Context<Self>) {
        if !self.visible {
            self.stale_timer.disarm();
            return;
        }
        let times = self.times.iter().filter_map(|t| t.at).collect();
        self.stale_timer.arm_each(
            times,
            self.stale_after.get(),
            chrono::Utc::now(),
            cx,
            Self::stale_slot,
        );
    }
}

fn refused(r: &Refusal) -> String {
    format!("query refused: {r}")
}
