//! The tile's data flow: the source documents in, the runs out.
//!
//! Every source stage's dataset is read by the underlying's key prefix, so
//! a two-part-key dataset (a chain per expiry, a curve per series) answers
//! every document of the underlying in one snapshot. The reads go out in
//! sequence under ONE `FollowingQuery` tag, all keyed by the tile: the
//! query pool keeps one request per key and the flip barrier one entry per
//! key, so concurrent reads would supersede each other. The set is the
//! tile's one arrival; when it is complete and the pipeline runs on input,
//! a run goes out, tagged by the tile's own run counter.
//!
//! Each source is watched for publications under the underlying. A publish
//! asks that source again and no other; the rest keep their rows, and the
//! run goes out again once the set is complete. A followed group's scope
//! naming another underlying clears the results and the documents at once,
//! before the new underlying's arrive: a card must never show one
//! underlying's fit under another's name.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use geode_core::calibration::{
    CalibrationOutcome, CalibrationRequest, Pipeline, REQUEST_STAGE, RunAction, RunMode,
    StageInput, StageKind, StageName, StageStatus,
};
use geode_core::clock::Clock;
use geode_core::document::{DocumentRows, documents_from_snapshot, join_key};
use geode_core::link::{Group, underlying_of};
use geode_core::query::{DocumentParams, QueryKey, QueryOutcome};
use geode_shell::frame::{FrameVersions, PublicationWatch};
use geode_tile::following::{Arrival, DeferredDoor, Delivered, FrameDoor, Promotion, Unanswered};
use geode_tile::notice::Notice;
use gpui::{App, Context};

use super::CalibrationTile;
use super::paint::Chrome;
use crate::core::layout::{LayoutInput, layout};
use crate::core::model::{ModelInput, build};

/// The run refusal's notice: the data handle's calibration queue is full.
pub const QUEUE_FULL: &str = "calibration queue full";

/// The source documents in hand and what is still owed.
#[derive(Default)]
pub(super) struct Sources {
    /// Each source stage's documents, one per key tuple under the
    /// underlying, in the answer's order.
    pub rows: BTreeMap<StageName, Vec<Arc<DocumentRows>>>,
    /// Source stages asked and not yet answered, or whose read failed: the
    /// model paints them `waiting` and no run goes out while one remains.
    pub pending: BTreeSet<StageName>,
    /// One publication watch per source stage, on its dataset under the
    /// underlying.
    pub watches: Vec<(StageName, PublicationWatch)>,
    /// Each source's watch revision when it was last asked; a watch past
    /// its entry is a source to ask again.
    asked: BTreeMap<StageName, u64>,
}

/// Which read is out. Any outcome under another tag is dropped.
pub(super) enum Fetch {
    Idle,
    Reading {
        tag: u64,
        underlying: String,
        /// The stage and dataset of the read out now.
        out: (StageName, String),
        /// The reads still to make, in pipeline order.
        queue: VecDeque<(StageName, String)>,
        /// The notices the reads so far earned (a failed read, an empty
        /// answer), kept until the sequence ends.
        notices: Vec<Notice>,
    },
}

/// The counters the documents depend on: the as-of and the watched
/// publications always; the scope too while following a group, because a
/// group's scope names the underlying. Unfollowed, the tile reads no scope.
fn differs(following: bool) -> impl Fn(FrameVersions, FrameVersions) -> bool {
    move |a, b| a.as_of != b.as_of || a.data != b.data || (following && a.scope != b.scope)
}

/// The source stages of `pipeline` with their datasets, in document order.
fn source_stages(pipeline: &Pipeline) -> Vec<(StageName, String)> {
    pipeline
        .stages
        .iter()
        .filter_map(|s| match &s.kind {
            StageKind::Source { dataset, .. } => Some((s.name.clone(), dataset.clone())),
            _ => None,
        })
        .collect()
}

/// The notice while no underlying is set: the group's, when following.
pub(super) fn no_underlying(following: Option<Group>) -> String {
    match following {
        Some(g) => format!("no underlying in {}", g.letter()),
        None => "no underlying".to_string(),
    }
}

impl CalibrationTile {
    pub(super) fn key(&self) -> QueryKey {
        QueryKey(self.id.0)
    }

    fn versions(&self, cx: &App) -> FrameVersions {
        self.frame
            .read(cx)
            .versions_for(self.sources.watches.iter().map(|(_, w)| w))
    }

    fn is_following(&self, cx: &App) -> bool {
        self.frame.read(cx).following().is_some()
    }

    /// The underlying, owned: the followed group's single value, else the
    /// tile's own.
    pub(super) fn underlying(&self, cx: &App) -> Option<String> {
        let frame = self.frame.read(cx);
        match frame.following() {
            Some(_) => underlying_of(frame.scope()).map(str::to_string),
            None => self.underlying.clone(),
        }
    }

    /// The pipeline shown, cloned out of the shared config; `None` with no
    /// config or no pipeline of the tile's name.
    pub(super) fn pipeline(&self) -> Option<Pipeline> {
        let config = self.shared.config.borrow();
        config.as_ref().and_then(|c| self.pipeline_in(c)).cloned()
    }

    pub(super) fn clock(&self, cx: &App) -> Clock {
        cx.try_global::<geode_shell::clock::AppClock>()
            .map(|c| c.0)
            .unwrap_or_else(|| Clock::machine().0)
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

    /// Rebuild the render model from the pipeline, the results, the
    /// waiting sources, the cursor, the filter and the viewport paint
    /// measured. Called after every change to one of them, never in
    /// render.
    pub(super) fn rebuild_model(&mut self, cx: &App) {
        let config = self.shared.config.borrow().clone();
        let Some(pipeline) = config.as_ref().and_then(|c| self.pipeline_in(c)) else {
            self.model = None;
            self.chrome = Chrome::default();
            return;
        };
        self.model_version += 1;
        let placed = layout(LayoutInput {
            pipeline,
            width: self.viewport.width,
            height: self.viewport.height,
            rem: self.viewport.rem,
        });
        let model = build(ModelInput {
            pipeline,
            results: &self.results,
            waiting: &self.sources.pending,
            cursor: self.cursor.as_deref(),
            filter: self.filter.as_deref(),
            // The connect picker's proposal, once it exists.
            preview_edge: None,
            uploaded_at: self.uploaded_at,
            layout: placed,
            clock: self.clock(cx),
            version: self.model_version,
        });
        self.chrome = Chrome::prepare(self.id.0, &model);
        self.model = Some(Rc::new(model));
        // An open menu's rows promise what the results allow.
        self.refresh_menu(cx);
    }

    pub(crate) fn requery(&mut self, cx: &mut Context<Self>) {
        self.requery_with(Arrival::Now, cx);
    }

    /// Ask every source for the current underlying's documents.
    pub(super) fn requery_with(&mut self, arrival: Arrival, cx: &mut Context<Self>) {
        self.requery_sources(None, arrival, cx);
    }

    /// Forget the last underlying's documents and results. A run, publish
    /// or upload still out for it is no longer this tile's: its tags are
    /// forgotten, so its answer is dropped, and the verbs' notices about
    /// it leave with it. A question armed over it is withdrawn, never
    /// answered: `y` must not publish another underlying than the one the
    /// question named. The menu's rows were about it too.
    fn clear_loaded(&mut self, cx: &mut Context<Self>) {
        self.results.clear();
        self.sources.rows.clear();
        self.sources.pending.clear();
        self.sources.asked.clear();
        self.last_run_tag = None;
        self.last_user_tag = None;
        self.run_notice = None;
        self.run_due = false;
        self.fetch = Fetch::Idle;
        self.publishing = None;
        self.uploads.clear();
        self.uploading = Default::default();
        self.uploaded_at = None;
        self.verb_notice = None;
        self.upload_notices.clear();
        let _ = geode_tile::confirm::withdraw(self, cx);
        self.close_menu(cx);
    }

    /// `:pipeline <name>`: show another pipeline of the document. What was
    /// loaded belonged to the old one, so the results, the documents and
    /// the watches go, the question and the menu close, a cursor the new
    /// pipeline lacks is dropped, and a shown tile asks the new sources.
    pub(super) fn switch_pipeline(
        &mut self,
        name: String,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let known = {
            let config = self.shared.config.borrow();
            config
                .as_ref()
                .is_some_and(|c| c.pipelines.pipelines.iter().any(|p| p.name == name))
        };
        if !known {
            return Err(format!("unknown pipeline '{name}'"));
        }
        if self.pipeline_name.as_deref() == Some(name.as_str()) {
            return Ok(());
        }
        self.pipeline_name = Some(name);
        self.clear_loaded(cx);
        self.loaded_for = None;
        self.sources.watches.clear();
        let keep = self
            .pipeline()
            .is_some_and(|p| self.cursor.as_deref().is_some_and(|c| p.stage(c).is_some()));
        if !keep {
            self.cursor = None;
        }
        self.refresh(cx);
        if self.visible {
            self.requery(cx);
        }
        cx.notify();
        Ok(())
    }

    /// The request for `action` over `pipeline` and every source's
    /// documents in hand, under the next tag. Every request of either lane
    /// takes the next value, so no two answers share a tag.
    pub(super) fn request(
        &mut self,
        pipeline: Pipeline,
        underlying: String,
        action: RunAction,
    ) -> CalibrationRequest {
        let inputs: Vec<StageInput> = source_stages(&pipeline)
            .into_iter()
            .map(|(stage, _)| StageInput {
                rows: self.sources.rows.get(&stage).cloned().unwrap_or_default(),
                stage,
            })
            .collect();
        self.run_tag += 1;
        CalibrationRequest {
            key: self.key(),
            tag: self.run_tag,
            submitted: Instant::now(),
            pipeline,
            underlying,
            inputs,
            action,
        }
    }

    /// Ask the sources in `only` (every source when `None`), plus any still
    /// pending from an earlier ask, for the current underlying, in
    /// sequence under one new tag. The others keep their rows. A change of
    /// underlying first clears everything the last one left and watches
    /// the datasets under the new one.
    fn requery_sources(
        &mut self,
        only: Option<BTreeSet<StageName>>,
        arrival: Arrival,
        cx: &mut Context<Self>,
    ) {
        let following = self.frame.read(cx).following();
        let (underlying, pipeline) = (self.underlying(cx), self.pipeline());
        let (Some(u), Some(pipeline)) = (underlying, pipeline) else {
            // Nothing to ask: clear what belonged to the last underlying
            // and answer the barrier. `KeepActed` keeps the versions asked
            // under, so only a change the tile follows asks again rather
            // than every frame notification; the tag moves, so a late
            // answer for the old underlying is stale.
            self.clear_loaded(cx);
            self.loaded_for = None;
            self.sources.watches.clear();
            self.read_notices = vec![Notice::status(no_underlying(following))];
            let versions = self.versions(cx);
            self.following.begin(versions, Instant::now());
            self.submitted(false, Unanswered::KeepActed, arrival, cx);
            self.rebuild_model(cx);
            cx.notify();
            return;
        };
        let sources = source_stages(&pipeline);
        let mut only = only;
        if self.loaded_for.as_deref() != Some(u.as_str()) {
            self.clear_loaded(cx);
            self.loaded_for = Some(u.clone());
            only = None;
        }
        // The watches follow the underlying and the pipeline's sources.
        let batch = join_key(std::slice::from_ref(&u));
        let watched = self.sources.watches.len() == sources.len()
            && self
                .sources
                .watches
                .iter()
                .zip(&sources)
                .all(|((ws, w), (s, ds))| ws == s && w.is_for(ds, Some(&batch)));
        if !watched {
            self.sources.watches = self.frame.update(cx, |f, _| {
                sources
                    .iter()
                    .map(|(s, ds)| (s.clone(), f.watch_publications(ds, Some(&batch))))
                    .collect()
            });
            only = None;
        }
        let stages: Vec<(StageName, String)> = sources
            .iter()
            .filter(|(s, _)| {
                only.as_ref()
                    .is_none_or(|set| set.contains(s) || self.sources.pending.contains(s))
            })
            .cloned()
            .collect();
        // `begin` drops whatever was staged for the previous question.
        let submitted = Instant::now();
        let versions = self.versions(cx);
        let tag = self.following.begin(versions, submitted);
        self.read_notices.clear();
        self.run_due = false;
        {
            let frame = self.frame.read(cx);
            for (stage, _) in &stages {
                self.sources.pending.insert(stage.clone());
                let revision = self
                    .sources
                    .watches
                    .iter()
                    .find(|(s, _)| s == stage)
                    .map_or(0, |(_, w)| frame.versions_for([w]).data);
                self.sources.asked.insert(stage.clone(), revision);
            }
        }
        let mut queue: VecDeque<(StageName, String)> = stages.into();
        let Some(first) = queue.pop_front() else {
            // Nothing to ask: no source stage, or none owed. The barrier is
            // answered and the set is complete as it stands.
            self.fetch = Fetch::Idle;
            self.submitted(false, Unanswered::KeepActed, arrival, cx);
            self.on_sources_complete(cx);
            self.rebuild_model(cx);
            cx.notify();
            return;
        };
        let queued = self.ask(&u, &first.1, tag, cx);
        match &queued {
            Ok(()) => {
                self.fetch = Fetch::Reading {
                    tag,
                    underlying: u,
                    out: first,
                    queue,
                    notices: Vec::new(),
                }
            }
            Err(refusal) => {
                self.fetch = Fetch::Idle;
                self.read_notices.push(Notice::danger(format!(
                    "document request refused: {refusal}"
                )));
            }
        }
        self.submitted(queued.is_ok(), Unanswered::Retry, arrival, cx);
        self.rebuild_model(cx);
        cx.notify();
    }

    /// One document read: `dataset` under the underlying's key prefix.
    fn ask(
        &self,
        underlying: &str,
        dataset: &str,
        tag: u64,
        cx: &App,
    ) -> Result<(), geode_data::Refusal> {
        let as_of = self.frame.read(cx).as_of().clone();
        self.shared.data.document(DocumentParams {
            key: self.key(),
            tag,
            submitted: Instant::now(),
            dataset: dataset.to_string(),
            document_key: vec![underlying.to_string()],
            as_of,
        })
    }

    /// A document read's answer: the read out is decoded and its stage
    /// filled, the next read goes out under the same tag, and the last
    /// answer completes the set, which is the tile's one arrival. A failed
    /// or unreadable answer leaves its stage pending with a notice; the
    /// sequence continues, so the other sources still arrive. So does an
    /// empty answer: a live read of a key no document was ever published
    /// under answers zero rows, not an error, and the query seam cannot
    /// tell that from a document published empty, so both mean "not yet
    /// published" here and the source waits for the dataset's next
    /// publication, which asks it again.
    pub fn deliver_query(&mut self, outcome: QueryOutcome, cx: &mut Context<Self>) {
        let Fetch::Reading { tag, .. } = &self.fetch else {
            return;
        };
        if outcome.tag != *tag {
            return;
        }
        let Fetch::Reading {
            tag,
            underlying,
            out: (stage, dataset),
            mut queue,
            mut notices,
        } = std::mem::replace(&mut self.fetch, Fetch::Idle)
        else {
            return;
        };
        let decoded = outcome.snapshot.and_then(|s| {
            let config = self.shared.config.borrow().clone();
            let schema = config
                .as_ref()
                .map(|c| Arc::clone(&c.schema))
                .ok_or_else(|| "no configuration".to_string())?;
            let ds = schema
                .dataset(&dataset)
                .ok_or_else(|| format!("dataset '{dataset}' is not declared"))?;
            documents_from_snapshot(ds, &s)
        });
        match decoded {
            // Nothing published under the key: the source is still owed.
            // Whatever it held before is gone too, so a run can never read
            // a document the store no longer has.
            Ok(docs) if docs.is_empty() => {
                self.sources.rows.remove(&stage);
                notices.push(Notice::status(format!(
                    "{dataset} has no document for {underlying}"
                )));
            }
            Ok(docs) => {
                self.sources
                    .rows
                    .insert(stage.clone(), docs.into_iter().map(Arc::new).collect());
                self.sources.pending.remove(&stage);
            }
            Err(e) => notices.push(Notice::danger(format!("{stage}: {e}"))),
        }
        if let Some(next) = queue.pop_front() {
            match self.ask(&underlying, &next.1, tag, cx) {
                Ok(()) => {
                    self.fetch = Fetch::Reading {
                        tag,
                        underlying,
                        out: next,
                        queue,
                        notices,
                    }
                }
                Err(refusal) => {
                    notices.push(Notice::danger(format!(
                        "{}: document request refused: {refusal}",
                        next.0
                    )));
                    self.finish(tag, notices, cx);
                }
            }
        } else {
            self.finish(tag, notices, cx);
        }
        self.rebuild_model(cx);
        cx.notify();
    }

    /// The sequence has ended: hand the set to the barrier. Failed and
    /// empty reads are notices, and their stages stay pending, which is
    /// what keeps the run from going out; the set still arrives, so one
    /// broken read never holds every other tile to the deadline.
    fn finish(&mut self, tag: u64, notices: Vec<Notice>, cx: &mut Context<Self>) {
        self.read_notices = notices;
        let now = self.versions(cx);
        let following = self.is_following(cx);
        let key = self.key();
        let delivered = self.following.deliver::<String>(
            tag,
            Ok(()),
            now,
            differs(following),
            &mut FrameDoor::new(&self.frame, cx),
            key,
        );
        match delivered {
            Delivered::Apply(()) => self.on_sources_complete(cx),
            // Held: the flip's promotion runs it. Stale or superseded: an
            // answer to a question nobody is asking.
            Delivered::Held | Delivered::Stale | Delivered::Superseded | Delivered::Failed(_) => {}
        }
    }

    /// The source set is in (or was promoted): run when the pipeline runs
    /// on input and the tile is on screen. Hidden, the run is owed and
    /// goes out on show; manual, it waits for the verb.
    fn on_sources_complete(&mut self, cx: &mut Context<Self>) {
        let Some(pipeline) = self.pipeline() else {
            return;
        };
        if pipeline.run != RunMode::OnInput {
            self.run_due = false;
            return;
        }
        if !self.visible {
            self.run_due = true;
            return;
        }
        self.submit_run(None, cx);
    }

    /// Submit a run from `from` (every stage when `None`) over every
    /// source's documents, tagged by the tile's run counter. `false`, and
    /// nothing sent, while a source is still owed or there is no pipeline
    /// or underlying; `false` with the queue-full notice on a refusal, the
    /// documents kept so the next change retries.
    pub(crate) fn submit_run(&mut self, from: Option<StageName>, cx: &mut Context<Self>) -> bool {
        let (Some(pipeline), Some(underlying)) = (self.pipeline(), self.loaded_for.clone()) else {
            return false;
        };
        // A run with a source missing would fit whatever the engine makes
        // of the gap; the pipeline waits for every source instead.
        if !self.sources.pending.is_empty() {
            return false;
        }
        let request = self.request(pipeline, underlying, RunAction::Run { from });
        let tag = request.tag;
        let sent = match self.shared.data.calibrate(request) {
            Ok(()) => {
                self.last_run_tag = Some(tag);
                self.run_notice = None;
                true
            }
            Err(_) => {
                self.run_notice = Some(Notice::status(QUEUE_FULL));
                false
            }
        };
        self.run_due = false;
        cx.notify();
        sent
    }

    /// A run's or a publish's answer. Only the latest request of its lane
    /// is listened for: a `Run` outcome must carry the tag of the last run
    /// submitted, any other action the tag of the last user action, so a
    /// late run can never be mistaken for a publish answer, nor another
    /// tile's for this one's. The synthetic `request` stage is a refused or
    /// failed request: its reason is a header notice, never a card.
    /// Otherwise the stages delivered replace their results and every
    /// other card stands. A publish's answer then sends its rows on; rows
    /// on any other action are not a publish's and go nowhere.
    pub fn deliver_calibration(&mut self, outcome: CalibrationOutcome, cx: &mut Context<Self>) {
        let CalibrationOutcome {
            key,
            tag,
            action,
            stages,
            publish_rows,
            ..
        } = outcome;
        let expected = match &action {
            RunAction::Run { .. } => self.last_run_tag,
            _ => self.last_user_tag,
        };
        if key != self.key() || expected != Some(tag) {
            return;
        }
        let request_failed = match stages.as_slice() {
            [only] if only.name == REQUEST_STAGE => Some(match &only.status {
                StageStatus::Failed(reason) => reason.clone(),
                _ => only.summary.clone(),
            }),
            _ => None,
        };
        let failed = request_failed.is_some();
        match request_failed {
            Some(reason) => self.run_notice = Some(Notice::danger(reason)),
            None => {
                self.run_notice = None;
                for stage in stages {
                    self.results.insert(stage.name.clone(), stage);
                }
            }
        }
        if let RunAction::Publish { .. } = action {
            // A failed publish has nothing to send; the flight is over
            // either way.
            self.publish_answered(if failed { None } else { publish_rows }, cx);
        }
        self.rebuild_model(cx);
        cx.notify();
    }

    /// The sources whose watch moved since they were last asked.
    fn stale_sources(&self, cx: &App) -> BTreeSet<StageName> {
        let frame = self.frame.read(cx);
        self.sources
            .watches
            .iter()
            .filter(|(stage, w)| {
                frame.versions_for([w]).data != self.sources.asked.get(stage).copied().unwrap_or(0)
            })
            .map(|(stage, _)| stage.clone())
            .collect()
    }

    /// The frame observer. The header follows the underlying; the data
    /// flow follows the counters the documents depend on.
    pub(super) fn on_frame_changed(&mut self, cx: &mut Context<Self>) {
        if self.underlying_str(cx).map(str::to_string) != self.header.underlying {
            self.refresh(cx);
            cx.notify();
        }
        if self.sync_following(cx) {
            if self.visible {
                self.requery(cx);
            }
            return;
        }
        // Promote before the visibility check, so a tile hidden after
        // staging still lands its answer.
        let now = self.versions(cx);
        let following = self.is_following(cx);
        if let Promotion::Apply(()) = self.following.on_flip(now, differs(following)) {
            self.on_sources_complete(cx);
        }
        if !self.visible {
            return;
        }
        if !self.following.follows_changed(now, differs(following)) {
            let key = self.key();
            self.following
                .self_arrive(&mut FrameDoor::new(&self.frame, cx), key, now);
            return;
        }
        // A counter the documents follow moved. The as-of or the underlying:
        // everything is asked again. A publication alone: only its source.
        // A scope change keeping the underlying with nothing out and no
        // publication: nothing to ask, and the flip is answered at once
        // rather than held behind three reads that answer what is on
        // screen. While a read is out, asking again is the safe course: the
        // old answer would be superseded under the new versions and the
        // flip left waiting.
        let as_of_moved = self.following.acted().is_none_or(|a| a.as_of != now.as_of);
        let underlying_changed = self.loaded_for != self.underlying(cx);
        let settled = !self.following.in_flight()
            && !self.following.is_staged()
            && matches!(self.fetch, Fetch::Idle);
        let stale = self.stale_sources(cx);
        if as_of_moved || underlying_changed || !settled {
            self.requery(cx);
        } else if !stale.is_empty() {
            self.requery_sources(Some(stale), Arrival::Now, cx);
        } else {
            let key = self.key();
            self.following
                .self_arrive(&mut FrameDoor::new(&self.frame, cx), key, now);
        }
    }

    /// Notice a change of followed group, which moves no version when
    /// neither scope was ever written. The old question is forgotten; the
    /// requery that follows clears the documents if the underlying
    /// changed. `true` when it changed.
    fn sync_following(&mut self, cx: &mut Context<Self>) -> bool {
        let following = self.frame.read(cx).following();
        if following == self.last_following {
            return false;
        }
        self.last_following = following;
        self.fetch = Fetch::Idle;
        self.following.reset();
        cx.notify();
        true
    }

    /// Showing asks again when a counter the documents follow moved while
    /// hidden, or the underlying changed, or nothing was ever asked; else
    /// it submits the run that completed while hidden, if one is owed.
    /// Hiding keeps the reads in flight; their answers land when they come,
    /// and no run goes out until the tile is shown. An arrival made here is
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
            if changed
                || self.following.follows_changed(now, differs(following))
                || self.loaded_for != self.underlying(cx)
            {
                self.requery_with(Arrival::Deferred, cx);
            } else {
                if self.run_due {
                    self.submit_run(None, cx);
                }
                // Nothing to ask, but a flip may be waiting on this tile.
                let key = self.key();
                self.following
                    .self_arrive(&mut DeferredDoor::new(&self.frame, cx), key, now);
            }
        }
        cx.notify();
    }

    /// The shell is removing this tile: cancel its reads by key, then
    /// answer any barrier still waiting on it. Runs inside the shell's
    /// occupant reconciliation, so its arrival is deferred: a release
    /// notified during the shell's draw would be dropped, holding every
    /// other tile to the barrier's deadline.
    pub fn closed(&mut self, cx: &mut Context<Self>) {
        let key = self.key();
        self.shared.data.cancel(key);
        self.fetch = Fetch::Idle;
        self.following
            .close(&mut DeferredDoor::new(&self.frame, cx), key);
    }
}
