//! The board's request lifecycle: the tenor listing, the board ask and
//! its tag, deliveries, the refresh timer, the prepared model and the
//! per-row stale wake-ups. The UI never waits: a refused submission is a
//! notice, and nothing retries by itself.

use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use geode_core::query::{HistoryKeysOutcome, HistoryKeysParams, QueryKey};
use geode_core::volboard::{VolBoardOutcome, VolBoardParams};
use geode_tile::stale::StaleTimer;
use gpui::Context;

use super::{MONO_ADVANCE_PX, RETRIES, VolwatchTile};
use crate::content::VolwatchSettings;
use crate::core::model::{Inputs, header, prepare};
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
            Err(e) => self.notices.failure = Some(format!("{e} {RETRIES}")),
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
        let inputs = Inputs {
            tile: self.tile.0,
            rows: self.grid.rows(),
            columns: &self.columns,
            asked,
            board_lookback: asked_lookback(self.board_ask.as_ref(), self.state.lookback),
            shown: self.state.lookback,
            paint: self.paint,
            stale_after,
            now: Utc::now(),
            mono_advance_px: MONO_ADVANCE_PX,
        };
        let mut prepared = prepare(&inputs);
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
        self.prepared = prepared;
        self.header = header(
            self.state.list.as_deref(),
            self.members.len(),
            self.state.lookback,
            None,
            self.board.as_deref(),
        );
    }

    fn stale_slot(t: &mut Self) -> &mut StaleTimer {
        &mut t.stale_timer
    }

    /// A wake-up at each present row's `as_of + stale_after`; none while
    /// hidden.
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
            .collect();
        let after = self.shared.settings.borrow().stale_after;
        self.stale_timer
            .arm_each(times, after, Utc::now(), cx, Self::stale_slot);
    }

    /// After any notify: a row the stale timer has turned stale since the
    /// last prepare is re-prepared. A cheap scan over tens of rows.
    pub(crate) fn stale_fired(&mut self) {
        let after = self.shared.settings.borrow().stale_after;
        let turned = self
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
    }
}

/// The lookback the board was asked with; the one in force before any.
fn asked_lookback(
    board_ask: Option<&Ask>,
    shown: geode_core::volboard::Lookback,
) -> geode_core::volboard::Lookback {
    board_ask.map_or(shown, |a| a.lookback)
}
