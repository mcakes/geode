//! The tile's data flow.
//!
//! Two document reads, `cvi_params` by the full key and `option_chain` by
//! the one-part prefix, run in sequence under ONE `FollowingQuery` tag,
//! both keyed by the tile. The query pool keeps one request per key and the
//! flip barrier one entry per key, so two concurrent reads would supersede
//! each other; in sequence, the pair is handed to `FollowingQuery::deliver`
//! once and the barrier sees one arrival. The vol batch is a follow-on of
//! whatever is installed: it is never staged behind a flip.
//!
//! A followed group's board is watched for the CVI document under the
//! underlying the group names. A board change is never staged behind a
//! flip either: it rebuilds the strip and submits a batch at once.

use std::sync::Arc;
use std::time::Instant;

use chrono::{NaiveDate, Utc};
use geode_chart::core::view::View;
use geode_chart::xy::XyModel;
use geode_core::document::{DocumentRows, join_key};
use geode_core::link::underlying_of;
use geode_core::query::{DocumentParams, QueryKey, QueryOutcome};
use geode_core::vol::VolSliceOutcome;
use geode_shell::frame::FrameVersions;
use geode_tile::following::{Arrival, DeferredDoor, Delivered, FrameDoor, Promotion, Unanswered};
use gpui::{App, Context};

use super::VolsliceTile;
use crate::core::build::{batch, min_span, model, padded};
use crate::core::docs::{CHAIN, CVI, ChainExpiry, chain_expiries, cvi_rows};
use crate::core::model::{Loaded, strip};

/// Both documents' answers for one underlying, as one barrier arrival.
#[derive(Debug)]
pub(super) struct Fetched {
    underlying: String,
    /// The versions the pair was asked under.
    asked: Option<FrameVersions>,
    cvi: Result<Option<Arc<DocumentRows>>, String>,
    chain: Result<Vec<ChainExpiry>, String>,
}

/// Which document read is out. Any outcome under another tag, or arriving
/// while the other read is out, is dropped.
#[derive(Debug)]
pub(super) enum Fetch {
    Idle,
    Cvi {
        tag: u64,
        underlying: String,
    },
    Chain {
        tag: u64,
        underlying: String,
        cvi: Result<Option<Arc<DocumentRows>>, String>,
    },
}

/// The counters the documents depend on. The as-of and the watched
/// publications always; the scope too while following a group, because a
/// group's scope change enrolls its followers in the open flip and names
/// their underlying. Unfollowed, the tile reads no scope at all.
fn differs(following: bool) -> impl Fn(FrameVersions, FrameVersions) -> bool {
    move |a, b| a.as_of != b.as_of || a.data != b.data || (following && a.scope != b.scope)
}

impl VolsliceTile {
    fn key(&self) -> QueryKey {
        QueryKey(self.id.0)
    }

    fn versions(&self, cx: &App) -> FrameVersions {
        self.frame.read(cx).versions_for(&self.watches)
    }

    fn is_following(&self, cx: &App) -> bool {
        self.frame.read(cx).following().is_some()
    }

    /// The underlying, resolved on every use: the followed group's single
    /// value (none when its scope names none or several), else the tile's
    /// own.
    pub(super) fn underlying(&self, cx: &App) -> Option<String> {
        let frame = self.frame.read(cx);
        match frame.following() {
            Some(_) => underlying_of(frame.scope()).map(str::to_string),
            None => self.state.underlying.clone(),
        }
    }

    pub(super) fn today(&self, cx: &App) -> NaiveDate {
        #[cfg(test)]
        if let Some(today) = self.today_pin {
            return today;
        }
        let clock = cx
            .try_global::<geode_shell::clock::AppClock>()
            .map(|c| c.0)
            .unwrap_or_else(|| geode_core::clock::Clock::machine().0);
        clock.today(Utc::now())
    }

    /// Report a submission through the door `arrival` names.
    fn submitted(
        &mut self,
        ok: bool,
        unanswered: Unanswered,
        arrival: Arrival,
        cx: &mut Context<Self>,
    ) {
        let key = self.key();
        self.following
            .submitted(ok, unanswered, &mut arrival.door(&self.frame, cx), key);
    }

    pub(crate) fn requery(&mut self, cx: &mut Context<Self>) {
        self.requery_with(Arrival::Now, cx);
    }

    /// Ask the CVI document for the current underlying; the chain follows
    /// its answer under the same tag.
    fn requery_with(&mut self, arrival: Arrival, cx: &mut Context<Self>) {
        let following = self.frame.read(cx).following();
        let Some(u) = self.underlying(cx) else {
            // Nothing to ask: clear what belonged to the last underlying and
            // answer the barrier. `KeepActed` keeps the versions asked under,
            // so only a change the tile follows asks again rather than every
            // frame notification; the tag moves, so a late answer for the
            // old underlying is stale.
            self.loaded = Loaded::default();
            self.loaded_for = None;
            self.loaded_ok = None;
            self.loaded_gen += 1;
            self.strip.clear();
            self.clear_model();
            self.fetch = Fetch::Idle;
            self.notices = vec![crate::header::no_underlying(following)];
            let versions = self.versions(cx);
            self.following.begin(versions, Instant::now());
            self.submitted(false, Unanswered::KeepActed, arrival, cx);
            cx.notify();
            return;
        };
        if self.watched_for.as_deref() != Some(u.as_str()) {
            let batch = join_key(std::slice::from_ref(&u));
            self.watches = self.frame.update(cx, |f, _| {
                vec![
                    f.watch_publications(CVI, Some(&batch)),
                    f.watch_publications(CHAIN, Some(&batch)),
                ]
            });
            self.watched_for = Some(u.clone());
        }
        let (as_of, versions) = {
            let frame = self.frame.read(cx);
            (frame.as_of().clone(), frame.versions_for(&self.watches))
        };
        // `begin` also drops whatever was staged for the previous question.
        let submitted = Instant::now();
        let tag = self.following.begin(versions, submitted);
        let queued = self.data.document(DocumentParams {
            key: self.key(),
            tag,
            submitted,
            dataset: CVI.to_string(),
            document_key: vec![u.clone()],
            as_of,
        });
        match &queued {
            Ok(()) => self.fetch = Fetch::Cvi { tag, underlying: u },
            Err(refusal) => {
                self.fetch = Fetch::Idle;
                self.picture_failed_for(&u);
                self.notice(format!("document request refused: {refusal}"));
            }
        }
        self.submitted(queued.is_ok(), Unanswered::Retry, arrival, cx);
        cx.notify();
    }

    /// A document read's outcome: the cvi's leads to the chain read under
    /// the same tag; the chain's completes the pair, which is the tile's
    /// one arrival.
    pub(crate) fn deliver_query(&mut self, outcome: QueryOutcome, cx: &mut Context<Self>) {
        let current = match &self.fetch {
            Fetch::Cvi { tag, .. } | Fetch::Chain { tag, .. } => *tag,
            Fetch::Idle => return,
        };
        if outcome.tag != current {
            return;
        }
        match std::mem::replace(&mut self.fetch, Fetch::Idle) {
            Fetch::Cvi { tag, underlying } => {
                // A failed or unreadable CVI read is a notice, not a failed
                // fetch: the chain still paints.
                let cvi = outcome
                    .snapshot
                    .and_then(|s| cvi_rows(&s))
                    .map(|d| d.map(Arc::new));
                let as_of = self.frame.read(cx).as_of().clone();
                let queued = self.data.document(DocumentParams {
                    key: self.key(),
                    tag,
                    submitted: Instant::now(),
                    dataset: CHAIN.to_string(),
                    document_key: vec![underlying.clone()],
                    as_of,
                });
                match queued {
                    Ok(()) => {
                        self.fetch = Fetch::Chain {
                            tag,
                            underlying,
                            cvi,
                        }
                    }
                    // No answer is owed: fail the fetch, which arrives.
                    Err(refusal) => self.hand_over(
                        tag,
                        &underlying,
                        Err(format!("document request refused: {refusal}")),
                        cx,
                    ),
                }
            }
            Fetch::Chain {
                tag,
                underlying,
                cvi,
            } => {
                let today = self.today(cx);
                let chain = outcome.snapshot.and_then(|s| chain_expiries(&s, today));
                let asked = underlying.clone();
                let fetched = Fetched {
                    underlying,
                    // The tag is current, so `acted` is still the versions
                    // this pair was asked under.
                    asked: self.following.acted(),
                    cvi,
                    chain,
                };
                self.hand_over(tag, &asked, Ok(Arc::new(fetched)), cx);
            }
            Fetch::Idle => {}
        }
        cx.notify();
    }

    /// Hand the pair for `underlying`, or the fetch's failure, to the
    /// barrier.
    fn hand_over(
        &mut self,
        tag: u64,
        underlying: &str,
        result: Result<Arc<Fetched>, String>,
        cx: &mut Context<Self>,
    ) {
        let now = self.versions(cx);
        let following = self.is_following(cx);
        let key = self.key();
        let delivered = self.following.deliver(
            tag,
            result,
            now,
            differs(following),
            &mut FrameDoor::new(&self.frame, cx),
            key,
        );
        match delivered {
            Delivered::Apply(fetched) => self.install(&fetched, cx),
            // Held: the flip's promotion installs it. Stale or superseded:
            // an answer to a question nobody is asking.
            Delivered::Held | Delivered::Stale | Delivered::Superseded => {}
            // The last good documents stay on screen while they are the
            // asked underlying's.
            Delivered::Failed(e) => {
                self.picture_failed_for(underlying);
                self.notice(e);
            }
        }
    }

    /// A read for `asked` was refused or failed. The last good picture
    /// stays while it is `asked`'s; another underlying's goes, documents,
    /// strip and curves, so none of it sits under the new name. Either way
    /// the documents are no longer a good answer to the question asked.
    fn picture_failed_for(&mut self, asked: &str) {
        self.loaded_ok = None;
        if self.loaded_for.as_deref() == Some(asked) {
            return;
        }
        self.loaded = Loaded::default();
        self.loaded_for = None;
        self.loaded_gen += 1;
        self.strip.clear();
        self.clear_model();
    }

    /// Put a fetched pair on screen and ask for its batch.
    fn install(&mut self, fetched: &Fetched, cx: &mut Context<Self>) {
        let u = &fetched.underlying;
        self.notices.clear();
        // An error clears the kind rather than keeping the last underlying's
        // document beside this one's.
        self.loaded.cvi = match &fetched.cvi {
            Ok(Some(doc)) => Some(Arc::clone(doc)),
            Ok(None) => {
                self.notices.push(format!("no CVI document for {u}"));
                None
            }
            Err(e) => {
                self.notices.push(e.clone());
                None
            }
        };
        self.loaded.chain = match &fetched.chain {
            Ok(chain) => {
                if chain.is_empty() {
                    self.notices.push(format!("no option chain for {u}"));
                }
                chain.clone()
            }
            Err(e) => {
                self.notices.push(e.clone());
                Vec::new()
            }
        };
        self.loaded_for = Some(u.clone());
        // A failed read installs beside a notice: shown, but not an answer
        // a scope change may keep.
        self.loaded_ok = fetched
            .asked
            .filter(|_| fetched.cvi.is_ok() && fetched.chain.is_ok());
        self.loaded_gen += 1;
        self.compose_draft();
        self.restrip(cx);
        self.submit_batch(cx);
    }

    /// Rebuild the strip from what is loaded and fit the active set to it.
    fn restrip(&mut self, cx: &App) {
        self.strip = strip(&self.loaded, self.today(cx));
        self.state.reconcile(&self.strip);
    }

    /// Put the board's draft into `loaded` when it names the underlying
    /// whose documents are loaded, else none. `true` when that changed.
    fn compose_draft(&mut self) -> bool {
        let next = self
            .board_draft
            .as_ref()
            .filter(|(u, _, _)| self.loaded_for.as_ref() == Some(u))
            .map(|(_, rows, mark)| (Arc::clone(rows), *mark));
        let same = match (&self.loaded.draft, &next) {
            (None, None) => true,
            (Some((a, ma)), Some((b, mb))) => Arc::ptr_eq(a, b) && ma == mb,
            _ => false,
        };
        self.loaded.draft = next;
        if !same {
            self.loaded_gen += 1;
        }
        !same
    }

    fn clear_model(&mut self) {
        // The tag moves so a batch still out cannot paint over the clear.
        self.vol_tag += 1;
        self.plan = None;
        self.model_gen = None;
        self.model = XyModel::empty();
        self.model_notices.clear();
    }

    /// Ask for the batch the current state and documents need. Called after
    /// documents are installed, after a board change, and after every state
    /// change that alters a job.
    pub(crate) fn submit_batch(&mut self, cx: &mut Context<Self>) {
        let plan = batch(&self.state, &self.loaded, &self.strip);
        if plan.jobs.is_empty() {
            // The data notices say why there is nothing to paint.
            self.clear_model();
            cx.notify();
            return;
        }
        self.vol_tag += 1;
        let params = plan.params(self.key(), self.vol_tag, Instant::now());
        match self.data.vol_slices(params) {
            Ok(()) => self.plan = Some(plan),
            // The painted model stays while it was built from the
            // documents on screen; the next change retries. One built from
            // other documents (another underlying's, a draft that since
            // left, a superseded publication) would sit under the new strip
            // and chips: it clears. The documents stay, so the next change
            // still has a batch to ask.
            Err(refusal) => {
                if self.model_gen != Some(self.loaded_gen) {
                    self.clear_model();
                }
                self.model_notices = vec![format!("vol request refused: {refusal}")];
            }
        }
        cx.notify();
    }

    /// A vol batch's answer: swap the model when it answers the batch out.
    pub(crate) fn deliver_vol(&mut self, outcome: VolSliceOutcome, cx: &mut Context<Self>) {
        if outcome.tag != self.vol_tag {
            return;
        }
        let Some(plan) = &self.plan else {
            return;
        };
        let Some(built) = model(
            plan,
            &outcome,
            &self.loaded,
            &self.palette,
            self.state.split,
            self.version + 1,
        ) else {
            return;
        };
        let narrowest = min_span(plan.coordinate, &self.loaded);
        self.version += 1;
        self.model = built.model;
        // Every change to `loaded` either submits (moving the tag) or
        // clears the model, so an answer under the current tag was planned
        // from the documents now loaded.
        self.model_gen = Some(self.loaded_gen);
        self.model_notices = built.notices;
        self.full = padded(built.full, narrowest);
        let full = self.full;
        match &mut self.view {
            Some(view) if !self.reset_view => {
                view.min_span = narrowest;
                // A kept view wholly outside the new extent would paint
                // nothing; one overlapping it stays where the trader left it.
                if view.hi < full.0 || view.lo > full.1 {
                    view.reset(full);
                }
            }
            _ => {
                self.view = Some(View::with_min_span(full, narrowest));
                self.reset_view = false;
            }
        }
        self.state.view = self.view.map(|v| (v.lo, v.hi));
        cx.notify();
    }

    /// Whether the documents must be asked for again: a counter they follow
    /// moved since the last ask. A followed group's scope change that still
    /// names the underlying whose documents are loaded is not one, when
    /// nothing is out or held and neither the as-of nor a watched
    /// publication moved since the loaded documents were read successfully
    /// (`loaded_ok`): the documents depend on nothing else, and refetching
    /// both would hold the flip behind two reads that answer what is
    /// already on screen. Compared with what was read, not with what was
    /// last asked: a failed or refused read leaves an older or partial
    /// picture on screen under the newer `acted`, and skipping then would
    /// leave the failure standing with no other retry while following. The
    /// caller then self-arrives, which answers the flip under the new
    /// versions; `acted` keeps the old ones, so each later notification
    /// repeats this check rather than a fetch.
    fn documents_stale(&self, now: FrameVersions, following: bool, cx: &App) -> bool {
        if !self.following.follows_changed(now, differs(following)) {
            return false;
        }
        let Some(read) = self.loaded_ok else {
            return true;
        };
        let scope_only = read.as_of == now.as_of && read.data == now.data;
        let settled = !self.following.in_flight()
            && !self.following.is_staged()
            && matches!(self.fetch, Fetch::Idle);
        let same_underlying = self
            .underlying(cx)
            .is_some_and(|u| self.loaded_for.as_deref() == Some(u.as_str()));
        !(scope_only && settled && same_underlying)
    }

    /// The frame observer.
    pub(super) fn on_frame_changed(&mut self, cx: &mut Context<Self>) {
        if self.sync_following(cx) {
            if self.visible {
                self.requery(cx);
                self.sync_board(cx);
            }
            return;
        }
        // Promote before the visibility check, so a tile hidden after
        // staging still lands its answer.
        let now = self.versions(cx);
        let following = self.is_following(cx);
        if let Promotion::Apply(fetched) = self.following.on_flip(now, differs(following)) {
            self.install(&fetched, cx);
        }
        if !self.visible {
            return;
        }
        if self.documents_stale(now, following, cx) {
            // The barrier is answered on delivery, under the versions this
            // request was made with.
            self.requery(cx);
        } else {
            let key = self.key();
            self.following
                .self_arrive(&mut FrameDoor::new(&self.frame, cx), key, now);
        }
        self.sync_board(cx);
    }

    /// Notice a change of followed group, which moves no version when
    /// neither scope was ever written. The old group's draft and board
    /// watch go, and the old question is forgotten. The model goes too: it
    /// may paint the old group's draft, and a batch still out for it would
    /// land after the leave, a dashed trace under no draft chip that stays
    /// while the requery fails. `true` when it changed.
    fn sync_following(&mut self, cx: &mut Context<Self>) -> bool {
        let following = self.frame.read(cx).following();
        if following == self.last_following {
            return false;
        }
        self.last_following = following;
        self.board = None;
        self.board_draft = None;
        self.loaded.draft = None;
        self.loaded_gen += 1;
        self.clear_model();
        self.fetch = Fetch::Idle;
        self.following.reset();
        // The model is gone, so the documents no longer answer anything on
        // screen: a hidden tile records this change and shows later, when
        // only `loaded_ok` decides whether it asks again.
        self.loaded_ok = None;
        cx.notify();
        true
    }

    /// Keep the board watch on the followed group's CVI document for the
    /// underlying it names, and act on a board change: never staged behind
    /// a flip.
    fn sync_board(&mut self, cx: &mut Context<Self>) {
        let group = self.frame.read(cx).following();
        let underlying = self.underlying(cx);
        let (Some(group), Some(u)) = (group, underlying) else {
            self.board = None;
            if self.board_draft.take().is_some() && self.compose_draft() {
                self.restrip(cx);
                self.submit_batch(cx);
            }
            return;
        };
        let joined = join_key(std::slice::from_ref(&u));
        if !self
            .board
            .as_ref()
            .is_some_and(|(_, w, _)| w.is_for(group, CVI, Some(&joined)))
        {
            let watch = self
                .frame
                .update(cx, |f, _| f.watch_board(group, CVI, Some(&joined)));
            // Never acted on: the entry is read below whatever the revision.
            self.board = Some((group, watch, u64::MAX));
        }
        let Some((_, watch, acted)) = self.board.as_mut() else {
            return;
        };
        let revision = watch.revision();
        if revision == *acted {
            return;
        }
        *acted = revision;
        self.board_draft = self
            .frame
            .read(cx)
            .board_entry(group, CVI, std::slice::from_ref(&u))
            .map(|e| (u.clone(), e.rows, e.mark));
        if self.compose_draft() {
            self.restrip(cx);
            self.submit_batch(cx);
        }
    }

    /// Showing asks again only when nothing has loaded or a counter the
    /// documents follow moved while hidden. Hiding keeps the query in
    /// flight; its answer lands when it comes. An arrival made here is
    /// deferred: the shell calls this while it draws.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            let changed = self.sync_following(cx);
            let now = self.versions(cx);
            let following = self.is_following(cx);
            if changed || self.documents_stale(now, following, cx) {
                self.requery_with(Arrival::Deferred, cx);
            } else {
                // Nothing to ask, but a flip may be waiting on this tile.
                let key = self.key();
                self.following
                    .self_arrive(&mut DeferredDoor::new(&self.frame, cx), key, now);
            }
            self.sync_board(cx);
        }
        cx.notify();
    }

    /// The shell is removing this tile: cancel by key (the document reads
    /// and the vol batch at once), then answer any barrier still waiting on
    /// it. Runs inside the shell's occupant reconciliation, so it updates
    /// only the frame and the data handle, and its arrival is deferred: a
    /// release notified during the shell's draw would be dropped, holding
    /// every other tile to the barrier's deadline.
    pub fn closed(&mut self, cx: &mut Context<Self>) {
        let key = self.key();
        self.data.cancel(key);
        self.following
            .close(&mut DeferredDoor::new(&self.frame, cx), key);
    }
}
