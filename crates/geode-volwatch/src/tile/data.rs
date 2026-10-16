//! The board's request lifecycle: the tenor listing, the board ask and
//! its tag, deliveries, the refresh timer, the prepared model and the
//! per-row stale wake-ups. The UI never waits: a refused submission is a
//! notice, and nothing retries by itself.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use geode_core::clock::Clock;
use geode_core::query::{HistoryKeysOutcome, HistoryKeysParams, QueryKey};
use geode_core::volboard::Role;
use geode_core::volboard::{VolBoardOutcome, VolBoardParams};
use geode_tile::header::TimeRun;
use geode_tile::stale::StaleTimer;
use gpui::{Context, SharedString};

use super::{MONO_ADVANCE_PX, RETRIES, VolwatchTile};
use crate::content::VolwatchSettings;
use crate::core::model::{BandModel, Inputs, Prepared, Row, prepare, prepare_band};
use crate::core::views::{Ask, REALIZED, SURFACE, VARSWAP, ask, needs_requery};

/// The surface key the tenor listing enumerates.
const TENOR: &str = "tenor";

impl VolwatchTile {
    /// The key every request of this tile is submitted and cancelled under.
    pub(crate) fn key(&self) -> QueryKey {
        QueryKey(self.tile.0)
    }

    /// Ask the surface for its tenors, once per list shown. The board does
    /// not wait for it: the views lay out their conventional tenors until
    /// it answers.
    pub(crate) fn ask_keys(&mut self, cx: &mut Context<Self>) {
        let tag = self.keys_tag + 1;
        let params = HistoryKeysParams {
            key: self.key(),
            tag,
            dataset: SURFACE.into(),
            column: TENOR.into(),
        };
        match self.data.history_keys(params) {
            Ok(()) => {
                self.keys_tag = tag;
                self.keys_for = self.state.list.clone();
            }
            Err(r) => {
                self.notices.refusal = Some(format!("{r} {RETRIES}"));
                self.rebuild_chrome(cx);
            }
        }
    }

    /// Ask for the board the view, lookback and band imply. Unforced, it
    /// asks only when that ask is not inside the one that will answer (the
    /// one out, else the landed board's): a narrower ask never requeries.
    /// `force` (`shift+r`, the timer, new members) asks regardless, under a
    /// new tag. Nothing is asked for an empty member list.
    pub(crate) fn resubmit(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.members.is_empty() {
            return;
        }
        let next = ask(
            self.state.view,
            &self.ordered_tenors(),
            self.state.lookback,
            self.state.band,
        );
        let base = if self.in_flight {
            self.asked.as_ref()
        } else {
            self.board_ask.as_ref()
        };
        if !force && base.is_some_and(|b| !needs_requery(b, &next)) {
            return;
        }
        let tag = self.tag + 1;
        let params = VolBoardParams {
            key: self.key(),
            tag,
            submitted: Instant::now(),
            surface: SURFACE.into(),
            realized: Some(REALIZED.into()),
            varswap: Some(VARSWAP.into()),
            members: self.members.clone(),
            measures: next.measures.clone(),
            lookbacks: vec![next.lookback],
            window: next.window,
            sparklines: next.sparklines.clone(),
        };
        match self.data.vol_board(params) {
            Ok(()) => {
                // Only an admitted ask supersedes the one out.
                self.tag = tag;
                self.in_flight = true;
                self.asked = Some(next);
                self.notices.refusal = None;
            }
            Err(r) => self.notices.refusal = Some(format!("{r} {RETRIES}")),
        }
        self.rebuild_chrome(cx);
    }

    /// A board for the ask out lands: rows, the prepared model, the stale
    /// wake-ups and the next refresh. A failure keeps the last board, paints
    /// it stale and stands a warning. Any other tag answers an ask that was
    /// superseded, and is dropped.
    pub(crate) fn deliver_board(&mut self, outcome: VolBoardOutcome, cx: &mut Context<Self>) {
        if outcome.key != self.key() || !self.in_flight || outcome.tag != self.tag {
            return;
        }
        self.in_flight = false;
        match outcome.result {
            Ok(board) => {
                self.board = Some(Arc::new(board));
                self.board_ask = self.asked.clone();
                self.notices.failure = None;
            }
            Err(e) => self.notices.failure = Some(format!("surface: {e} {RETRIES}")),
        }
        self.rebuild_rows(cx);
        self.rebuild_chrome(cx);
        self.arm_refresh(cx);
        cx.notify();
    }

    /// The tenor listing lands: Term and Skew lay out the surface's own
    /// tenors, and the board is asked again only for a tenor the ask lacks.
    /// A source that cannot enumerate leaves the conventions.
    pub(crate) fn deliver_keys(&mut self, outcome: HistoryKeysOutcome, cx: &mut Context<Self>) {
        if outcome.key != self.key()
            || outcome.tag != self.keys_tag
            || outcome.dataset != SURFACE
            || outcome.column != TENOR
        {
            return;
        }
        if outcome.values.is_some() {
            self.tenors = outcome.values;
        }
        self.rebuild_columns();
        self.resubmit(false, cx);
        self.rebuild_rows(cx);
        cx.notify();
    }

    /// One wake-up at the refresh interval (never below the minimum); none
    /// while hidden or with the timer off. Re-armed after every delivery,
    /// so a slow service is never asked twice; a wake-up with an ask out
    /// asks nothing.
    pub(crate) fn arm_refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh = None;
        if !self.visible {
            return;
        }
        let every = VolwatchSettings::clamp_refresh(self.shared.settings.borrow().refresh);
        let Some(every) = every else {
            return;
        };
        self.refresh = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(every).await;
            let _ = this.update(cx, |t, cx| {
                t.refresh = None;
                if !t.in_flight {
                    t.resubmit(true, cx);
                    // A refused timer ask waits for the next interval; a
                    // refused `shift+r` only says so.
                    if !t.in_flight {
                        t.arm_refresh(cx);
                    }
                    cx.notify();
                }
            });
        }));
    }

    /// Re-prepare the model and the header from the rows, the columns and
    /// the board's ask. A failed last board paints every present row
    /// stale, as does a row whose stale wake-up fired.
    pub(crate) fn prepare(&mut self) {
        let stale_after = self.shared.settings.borrow().stale_after;
        let empty = Ask::default();
        let asked = self.board_ask.as_ref().unwrap_or(&empty);
        let mut prepared = prepare(&self.inputs(asked));
        let failed = self.notices.failure.is_some();
        for (row, p) in self.grid.rows().iter().zip(prepared.rows.iter_mut()) {
            let fired = row
                .as_of
                .is_some_and(|at| self.stale_timer.fired_for(at, stale_after));
            if p.present && !p.stale && (failed || fired) {
                p.stale = true;
                for c in &mut p.cells {
                    c.muted = true;
                }
            }
        }
        let runs = time_runs(&TimeInputs {
            rows: self.grid.rows(),
            prepared: &prepared,
            board: self.board.is_some(),
            failed,
            now: Utc::now(),
            after: stale_after,
            timer: &self.stale_timer,
            clock: &self.clock,
        });
        (self.times, self.times_at) = runs.into_iter().unzip();
        self.prepared = std::rc::Rc::new(prepared);
        self.rebuild_header();
    }

    /// What the prepared model and the band read, through `asked` (the
    /// ask the board answered).
    fn inputs<'a>(&'a self, asked: &'a Ask) -> Inputs<'a> {
        Inputs {
            tile: self.tile.0,
            rows: self.grid.rows(),
            columns: &self.columns,
            asked,
            board_lookback: asked_lookback(self.board_ask.as_ref(), self.state.lookback),
            shown: self.state.lookback,
            paint: self.paint,
            stale_after: self.shared.settings.borrow().stale_after,
            now: Utc::now(),
            mono_advance_px: MONO_ADVANCE_PX,
        }
    }

    /// The band for the cursor row while it is open; empty while closed or
    /// with no row under the cursor. Read through the board's ask, so a
    /// band just opened shows dashes until the grown ask's board lands.
    pub(crate) fn rebuild_band(&mut self) {
        let row = self
            .grid
            .cursor()
            .and_then(|at| self.grid.visible().get(at))
            .map(|&i| self.grid.row(i));
        self.band_model = match row.filter(|_| self.state.band) {
            Some(row) => {
                let empty = Ask::default();
                let asked = self.board_ask.as_ref().unwrap_or(&empty);
                prepare_band(
                    &self.inputs(asked),
                    row,
                    &self.ordered_tenors(),
                    &self.clock,
                )
            }
            None => BandModel::default(),
        };
    }

    fn stale_slot(t: &mut Self) -> &mut StaleTimer {
        &mut t.stale_timer
    }

    /// A wake-up at each present row's `as_of + stale_after`, so one dead
    /// member turns its own row stale without marking the list, and at the
    /// realized and varswap runs' source times; none while hidden.
    pub(crate) fn arm_stale(&mut self, cx: &mut Context<Self>) {
        if !self.visible {
            self.stale_timer.disarm();
            return;
        }
        let times = self
            .grid
            .rows()
            .iter()
            .filter(|r| r.present)
            .filter_map(|r| r.as_of)
            .chain(self.times_at.iter().flatten().copied())
            .collect();
        let after = self.shared.settings.borrow().stale_after;
        self.stale_timer
            .arm_each(times, after, Utc::now(), cx, Self::stale_slot);
    }

    /// After any notify: a row or a source time the stale timer has turned
    /// stale since the last prepare is re-prepared. A cheap scan over tens
    /// of rows; `true` when it re-prepared.
    pub(crate) fn stale_fired(&mut self) -> bool {
        let after = self.shared.settings.borrow().stale_after;
        let run_turned = self.times.iter().zip(&self.times_at).any(|(run, at)| {
            !run.stale && at.is_some_and(|at| self.stale_timer.fired_for(at, after))
        });
        let turned = run_turned
            || self
                .grid
                .rows()
                .iter()
                .zip(&self.prepared.rows)
                .any(|(row, p)| {
                    p.present
                        && !p.stale
                        && row
                            .as_of
                            .is_some_and(|at| self.stale_timer.fired_for(at, after))
                });
        if turned {
            self.prepare();
        }
        turned
    }
}

/// What the cluster's times are read from.
struct TimeInputs<'a> {
    rows: &'a [Row],
    prepared: &'a Prepared,
    /// A board has landed; with none there are no times.
    board: bool,
    /// The last board failed: every run is stale.
    failed: bool,
    now: chrono::DateTime<Utc>,
    after: Duration,
    timer: &'a StaleTimer,
    clock: &'a Clock,
}

/// One run per dataset the board read, with its source time: `surface
/// HH:MM` from the stalest present row's `as_of` (stale exactly when that
/// row paints stale), then `realized` and `varswap` from the oldest
/// `current_at` among the cells that read them, each only when the board
/// asked a measure reading it. A dataset with no time reads `—` and is
/// never stale.
fn time_runs(i: &TimeInputs<'_>) -> Vec<(TimeRun, Option<chrono::DateTime<Utc>>)> {
    if !i.board {
        return Vec::new();
    }
    let label = |role: Role, at: Option<chrono::DateTime<Utc>>| -> SharedString {
        match at {
            Some(at) => format!("{} {}", role.name(), i.clock.hm(at)).into(),
            None => format!("{} \u{2014}", role.name()).into(),
        }
    };
    let aged = |at: chrono::DateTime<Utc>| {
        (i.now - at).to_std().is_ok_and(|age| age > i.after) || i.timer.fired_for(at, i.after)
    };
    let stalest = i
        .rows
        .iter()
        .zip(&i.prepared.rows)
        .filter(|(r, _)| r.present)
        .filter_map(|(r, p)| r.as_of.map(|at| (at, p.stale)))
        .min_by_key(|(at, _)| *at);
    let mut out = vec![(
        TimeRun {
            label: label(Role::Surface, stalest.map(|(at, _)| at)),
            stale_label: None,
            stale: stalest.is_some_and(|(_, stale)| stale),
        },
        stalest.map(|(at, _)| at),
    )];
    for role in [Role::Realized, Role::Varswap] {
        let mut read = false;
        let mut oldest: Option<chrono::DateTime<Utc>> = None;
        for c in i
            .rows
            .iter()
            .filter_map(|r| r.board.as_ref())
            .flat_map(|b| &b.cells)
        {
            if !c.measure.roles().contains(&role) {
                continue;
            }
            read = true;
            if let Some(at) = c.current_at {
                oldest = Some(oldest.map_or(at, |o| o.min(at)));
            }
        }
        if read {
            out.push((
                TimeRun {
                    label: label(role, oldest),
                    stale_label: None,
                    stale: oldest.is_some_and(|at| i.failed || aged(at)),
                },
                oldest,
            ));
        }
    }
    out
}

/// The lookback the board was asked with; the one in force before any.
fn asked_lookback(
    board_ask: Option<&Ask>,
    shown: geode_core::volboard::Lookback,
) -> geode_core::volboard::Lookback {
    board_ask.map_or(shown, |a| a.lookback)
}
