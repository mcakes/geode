//! The tile's data half: deliveries from the data tier, the fetch of
//! every pair still waiting, the query over the range, and the flip
//! barrier's staging and promotion (see the parent module's doc).

use super::*;

impl TimeseriesTile {
    // ---- deliveries --------------------------------------------------

    /// A series answer for this tile's key.
    pub fn deliver(&mut self, outcome: SeriesOutcome, cx: &mut Context<Self>) {
        if outcome.tag != self.tag {
            // Stale: a newer request is out — and deliberately NOT an
            // arrival. A barrier waits for the versions this tile last
            // ACTED under, which is the newer request's; arriving here
            // would answer for a question still in flight, and that
            // outcome's own delivery is what answers it.
            return;
        }
        self.query_in_flight = false;
        let acted = self.acted;
        match outcome.result {
            Ok(result) => {
                // Phase 4a §3.10: while a barrier still wants this key,
                // STAGE rather than paint. A chart's own answer may land
                // well before every blotter's, and a chart painting the
                // new as-of beside a blotter still on the old one is
                // exactly the half-updated screen the barrier exists to
                // prevent.
                let wants = acted.is_some_and(|acted| {
                    self.frame
                        .read(cx)
                        .barrier_wants(QueryKey(self.id.0), acted)
                });
                if wants {
                    let acted = acted.expect("`wants` is false without one");
                    self.staged = Some((result, acted));
                    // `arrived` may empty the barrier right here — when
                    // it does, promote at once rather than waiting for
                    // the `flip` bump to reach this tile's own observer
                    // on a later notify pass.
                    if self.arrive_and_release(cx) {
                        self.promote(cx);
                    }
                } else {
                    self.apply_result(result, cx);
                    self.arrive(cx);
                }
            }
            Err(e) => {
                // Last good stays on screen: a failed query says nothing
                // about the points already painted. It still counts as
                // an arrival — one broken tile must never hold every
                // other tile open until the deadline.
                self.notice = Some(e.into());
                self.arrive(cx);
            }
        }
        cx.notify();
    }

    /// A fetch finished for one `(source, identity)` pair. Keyed by the
    /// pair, so a tile that holds it marks every slot over it and a tile
    /// that does not is left alone by `set_pair_state`'s own `NONE`.
    ///
    /// Any `Ok` requeries — `Ok(0)` included, which means the span was
    /// already covered rather than that nothing is there.
    pub fn on_fetched(
        &mut self,
        source: &str,
        identity: &str,
        result: Result<u64, String>,
        cx: &mut Context<Self>,
    ) {
        // ABOVE the early return (review round 1, I-2): a pair this tile
        // no longer holds answers `NONE`, and leaving its entry behind
        // would make the set claim a fetch is still out for a pair that
        // could be re-added a moment later — which `fetch_pending` would
        // then skip, leaving a `Fetching` chip with nothing coming.
        self.in_flight
            .remove(&(source.to_string(), identity.to_string()));
        let state = match &result {
            Ok(_) => SlotState::Idle,
            Err(why) => SlotState::Failed(why.clone()),
        };
        let changed = self.model.set_pair_state(source, identity, state);
        if changed.is_none() {
            return;
        }
        if result.is_ok() && self.visible {
            self.requery(cx);
        }
        self.rebuild_chrome(cx);
        cx.notify();
    }

    // ---- the data flow -----------------------------------------------

    /// Ask for every source slot that is waiting for data and has no
    /// fetch out already (see [`Self::in_flight`]), one request per
    /// PAIR: two slots over the same `identity@source` are one span.
    ///
    /// The whole visible range is asked for every time; the data tier
    /// subtracts what a pair already covers and queues one span per gap,
    /// so re-asking costs a round trip to `DataService` and nothing
    /// upstream.
    pub(super) fn fetch_pending(&mut self, cx: &mut Context<Self>) {
        if self.in_flight_range.as_ref() != Some(self.model.range()) {
            self.in_flight.clear();
            self.in_flight_range = Some(self.model.range().clone());
        }
        let (now, as_of) = self.now_and_as_of(cx);
        let (from, to) = self.model.range().resolve(now, &as_of);
        let mut pending: Vec<(String, String)> = Vec::new();
        for slot in self.model.slots() {
            let SlotKind::Source {
                source, identity, ..
            } = &slot.kind
            else {
                continue;
            };
            if slot.state != SlotState::Fetching {
                continue;
            }
            let pair = (source.clone(), identity.clone());
            if self.in_flight.contains(&pair) || pending.contains(&pair) {
                continue;
            }
            pending.push(pair);
        }
        for (source, identity) in pending {
            let queued = self.data.fetch(FetchParams {
                key: QueryKey(self.id.0),
                source: source.clone(),
                identity: identity.clone(),
                from,
                to,
            });
            if queued {
                self.in_flight.insert((source, identity));
            } else {
                // Nothing is coming, and a chip left `Fetching` for ever
                // would say the opposite.
                self.model.set_pair_state(
                    &source,
                    &identity,
                    SlotState::Failed("fetch refused: the data service is busy or gone".into()),
                );
            }
        }
    }

    /// Submit this tile's series request, keyed by the tile so two
    /// charts never supersede each other. The stats ride over the
    /// VISIBLE window and the points over the whole range, which is what
    /// `request::params` builds from the current result's buckets.
    /// Drop every in-flight entry for a pair the model no longer holds
    /// (review round 1, I-2). Called wherever slots LEAVE — `remove`
    /// (which takes an operand's dependants with it) and `:clear` —
    /// because an answer for a pair the tile has dropped never clears
    /// its own entry through the model, and a stale entry is
    /// indistinguishable from a live fetch: the same pair, re-added,
    /// would be skipped for the tile's whole life.
    pub(super) fn prune_in_flight(&mut self) {
        let model = &self.model;
        self.in_flight
            .retain(|(source, identity)| model.holds_pair(source, identity));
    }

    pub(super) fn requery(&mut self, cx: &mut Context<Self>) {
        // A fresh question supersedes whatever was staged for the old
        // one, whether or not `promote`'s own version check would have
        // caught it.
        self.staged = None;
        let (as_of, versions) = {
            let frame = self.frame.read(cx);
            (frame.as_of().clone(), frame.versions())
        };
        self.tag += 1;
        self.acted = Some(versions);
        let params = {
            let buckets = self.result.as_ref().map(|r| r.buckets.as_slice());
            request::params(
                &self.model,
                QueryKey(self.id.0),
                self.tag,
                Utc::now(),
                &as_of,
                buckets.unwrap_or(&[]),
            )
        };
        let submitted = match params {
            Some(params) => {
                let queued = self.data.series(params);
                if !queued {
                    self.notice =
                        Some("series request refused: the data service is busy or gone".into());
                }
                queued
            }
            // Nothing to ask about (no slot, or no dataset yet).
            None => false,
        };
        self.query_in_flight = submitted;
        if !submitted {
            // Nothing is coming: arrive, or an open barrier holds every
            // other tile to the 250 ms deadline waiting for an outcome
            // that will never exist — then clear `acted`, so the next
            // frame change retries rather than deciding this tile is
            // already up to date. In that order: `arrive` reads `acted`.
            self.arrive(cx);
            self.acted = None;
            self.query_in_flight = false;
        }
        cx.notify();
    }

    /// Whether the frame has moved in a way a series request depends on.
    /// `None` (nothing asked yet) is always a change.
    pub(super) fn follows_changed(&self, now: FrameVersions) -> bool {
        let Some(acted) = self.acted else {
            return true;
        };
        Self::differs_on_followed(acted, now)
    }

    /// `as_of` and nothing else (spec §6.5). The one comparison
    /// [`Self::follows_changed`] and [`Self::promote`]'s own gate both go
    /// through, so "what this tile requeries for" and "what invalidates
    /// something it has already staged" cannot drift apart.
    pub(super) fn differs_on_followed(versions: FrameVersions, now: FrameVersions) -> bool {
        versions.as_of != now.as_of
    }

    /// Answer an open flip barrier for a change this tile is NOT going to
    /// requery for (a scope or grouping bump, or no slot to ask about).
    ///
    /// `ShellView::visible_tile_keys` cannot know which tiles follow
    /// which counters, so every visible occupant is in the barrier's key
    /// set. Left unanswered, this tile would hold every blotter on
    /// screen open until `FLIP_DEADLINE` — 250 ms — on every scope
    /// keystroke, with nothing of its own coming.
    pub(super) fn self_arrive(&mut self, now: FrameVersions, cx: &mut Context<Self>) {
        // An unrelated notification is not an answer to the query this
        // barrier is already waiting for.
        if self.query_in_flight
            && self
                .acted
                .is_some_and(|acted| acted.same_flip_identity(now))
        {
            return;
        }
        let key = QueryKey(self.id.0);
        if self.frame.read(cx).barrier_wants(key, now) {
            self.frame.update(cx, |f, cx| {
                if f.arrived(key, now) {
                    cx.notify();
                }
            });
        }
    }

    /// Tell an open barrier this tile's own outcome has landed, under the
    /// versions the request was made with — a failed outcome counts too.
    pub(super) fn arrive(&mut self, cx: &mut Context<Self>) {
        let _ = self.arrive_and_release(cx);
    }

    /// [`Self::arrive`], answering whether this arrival is what EMPTIED
    /// the barrier — the caller uses that to promote its own staged
    /// result at once rather than waiting for the `flip` bump to reach
    /// its observer on a later notify pass.
    pub(super) fn arrive_and_release(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(acted) = self.acted else {
            return false;
        };
        let key = QueryKey(self.id.0);
        self.frame.update(cx, |f, cx| {
            let released = f.arrived(key, acted);
            if released {
                cx.notify();
            }
            released
        })
    }

    /// Put a staged result on screen once the flip released it — unless a
    /// counter this tile follows moved under it, in which case it answers
    /// a question nobody is asking any more (reachable while hidden,
    /// where no requery replaces it).
    pub(super) fn promote(&mut self, cx: &mut Context<Self>) {
        let Some((result, versions)) = self.staged.take() else {
            return;
        };
        if !Self::differs_on_followed(versions, self.frame.read(cx).versions()) {
            self.apply_result(result, cx);
            cx.notify();
        }
    }

    /// Install a delivered result: the new full extent, the view, the
    /// points and the chart model built from them.
    pub(super) fn apply_result(&mut self, result: SeriesResult, cx: &mut Context<Self>) {
        let full = self.full_of(&result);
        self.model.set_full(full);
        if std::mem::take(&mut self.reset_view) {
            self.model.reset_view();
        }
        // Assigned OVER the old `Arc`, never through a `None` first: a
        // tile that dropped its only result mid-update would paint an
        // empty chart on any frame drawn in between.
        self.result_seq += 1;
        self.result = Some(Arc::new(result));
        self.notice = None;
        self.rebuild_chrome(cx);
    }

    /// The x extent a result spans, in the units the current axis mode
    /// counts in: bucket INDICES under a session axis, epoch micros
    /// under a continuous one (where the last bucket's own width is part
    /// of the extent, since a bucket is drawn from its start).
    pub(super) fn full_of(&self, result: &SeriesResult) -> (f64, f64) {
        match self.model.axis_mode() {
            AxisMode::Session => (0.0, result.buckets.len() as f64),
            AxisMode::Continuous => {
                let step = (self.model.frequency().seconds() * 1_000_000) as f64;
                (
                    result.buckets.first().copied().unwrap_or(0) as f64,
                    result.buckets.last().copied().unwrap_or(0) as f64 + step,
                )
            }
        }
    }

    pub(super) fn now_and_as_of(&self, cx: &App) -> (DateTime<Utc>, AsOf) {
        (Utc::now(), self.frame.read(cx).as_of().clone())
    }
}
