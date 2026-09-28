//! Fetch tracking, tagged series requests and deliveries over
//! `geode_tile::following`. Successful fetches trigger series queries; failed
//! series queries retain the last installed result.

use super::*;

impl TimeseriesTile {
    // ---- deliveries --------------------------------------------------

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
            // Superseded: asked under an as-of the tile moved past while
            // hidden; the reshow asks again.
            Delivered::Held | Delivered::Superseded => {}
            // Last good stays on screen: a failed query says nothing about
            // the points already painted.
            Delivered::Failed(e) => self.notice = Some(e.into()),
        }
        // The post-step: an answered query releases a view move waiting
        // behind it (suppressed while a result is held for the barrier).
        self.release_view(cx);
        cx.notify();
    }

    /// Ask for the view a move left waiting behind the answered request.
    /// A staged result waits for its promotion, which releases instead, so
    /// the requery never discards a result the flip barrier still holds.
    pub(super) fn release_view(&mut self, cx: &mut Context<Self>) {
        if !self.view_waiting || self.following.in_flight() || self.following.is_staged() {
            return;
        }
        self.view_waiting = false;
        if self.visible && !self.model.slots().is_empty() {
            self.requery(cx);
        }
    }

    /// Complete fetch tracking and update every slot over this pair.
    /// A successful completion requeries while visible, including zero rows
    /// appended: the cache may already cover the requested span. Failure
    /// marks matching slots failed while retaining the chart's last result.
    pub fn on_fetched(
        &mut self,
        source: &str,
        identity: &str,
        result: Result<u64, String>,
        cx: &mut Context<Self>,
    ) {
        // Clear tracking even when no slot holds this pair anymore. Otherwise
        // re-adding it could suppress a needed fetch indefinitely.
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

    /// Fetch each waiting `(source, identity)` once per tracked span, even
    /// when several slots share the pair. Request the full resolved range;
    /// the data tier subtracts existing coverage before scheduling gaps.
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
            match queued {
                Ok(()) => {
                    self.in_flight.insert((source, identity));
                }
                // Nothing is coming, and a chip left `Fetching` for ever
                // would say the opposite.
                Err(refusal) => {
                    self.model.set_pair_state(
                        &source,
                        &identity,
                        SlotState::Failed(format!("fetch refused: {refusal}")),
                    );
                }
            }
        }
    }

    /// Forget fetch tracking for pairs removed from the model, including
    /// removal of dependent expressions and clearing every slot. Re-adding a
    /// pair must be able to submit its fetch again.
    pub(super) fn prune_in_flight(&mut self) {
        let model = &self.model;
        self.in_flight
            .retain(|(source, identity)| model.holds_pair(source, identity));
    }

    /// Submit with this tile's key and a fresh tag, superseding any staged result.
    /// Points cover the resolved range; statistics use the visible window from
    /// retained buckets, falling back to the range before the first result.
    /// Refusal or nothing to ask answers an open barrier and forgets the versions for retry.
    pub(super) fn requery(&mut self, cx: &mut Context<Self>) {
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
    }

    /// Only as-of invalidates an established series request or staged result.
    /// Share this comparison between requery decisions and promotion.
    pub(super) fn differs_on_followed(versions: FrameVersions, now: FrameVersions) -> bool {
        versions.as_of != now.as_of
    }

    /// Install a delivered result: the new full extent, the view, the
    /// points and the chart model built from them.
    pub(super) fn apply_result(&mut self, result: SeriesResult, cx: &mut Context<Self>) {
        let full = self.full_of(&result);
        self.model.set_full(full);
        if std::mem::take(&mut self.reset_view) {
            self.model.reset_view();
        }
        // Replace the retained result directly, keeping the last good data until
        // its successor is installed.
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
