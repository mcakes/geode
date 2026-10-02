//! One wake-up at the moment a tile's painted source time turns stale.
//! Render decides staleness by comparing the source time with the clock, so a
//! tile nothing repaints would keep its fresh tone forever. The timer arms once
//! per (source time, threshold) at `source_at + stale_after`, notifies the tile
//! when it fires, and records that it fired, so the tile's next render reads
//! stale even where the wall clock disagrees (a test clock, a clock stepped
//! back). Arming a different pair — a newer delivery, a changed threshold —
//! forgets the old verdict, so a fresh delivery never paints stale. It owns
//! its `Task`: re-arming replaces it, and `disarm` or dropping the tile
//! cancels it. Nothing here runs per frame.

use std::time::Duration;

use chrono::{DateTime, Utc};
use gpui::{Context, Task};

#[derive(Default)]
pub struct StaleTimer {
    /// The (source time, threshold) pair this timer answers for.
    armed: Option<(DateTime<Utc>, Duration)>,
    /// Whether the wake-up for `armed` fired. Reset with every new pair.
    fired: bool,
    task: Option<Task<()>>,
}

impl StaleTimer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Arm one wake-up at `at + after`, measured from `now`, unless this pair
    /// is already armed or has fired. `None` disarms. A deadline already
    /// passed arms nothing: render's own comparison says stale. `slot` finds
    /// this timer on the tile when the task wakes.
    pub fn arm<T: 'static>(
        &mut self,
        at: Option<DateTime<Utc>>,
        after: Duration,
        now: DateTime<Utc>,
        cx: &mut Context<T>,
        slot: fn(&mut T) -> &mut StaleTimer,
    ) {
        let Some(at) = at else {
            self.disarm();
            return;
        };
        if self.armed == Some((at, after)) && (self.task.is_some() || self.fired) {
            return;
        }
        self.disarm();
        self.armed = Some((at, after));
        let wait = chrono::Duration::from_std(after)
            .ok()
            .and_then(|after| at.checked_add_signed(after))
            .and_then(|deadline| deadline.signed_duration_since(now).to_std().ok());
        let Some(wait) = wait else {
            return;
        };
        self.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |tile, cx| {
                let timer = slot(tile);
                timer.task = None;
                timer.fired = true;
                cx.notify();
            });
        }));
    }

    /// Drop the wake-up and forget the pair and its verdict: a hidden tile
    /// wakes for nothing, and its show arms again.
    pub fn disarm(&mut self) {
        self.task = None;
        self.armed = None;
        self.fired = false;
    }

    /// Whether this timer fired for a source time at or before `at` under the
    /// same threshold — the verdict render ORs with its clock comparison.
    pub fn fired_for(&self, at: DateTime<Utc>, after: Duration) -> bool {
        self.fired
            && self
                .armed
                .is_some_and(|(armed_at, armed_after)| armed_after == after && at <= armed_at)
    }

    /// Whether a wake-up is pending.
    pub fn is_armed(&self) -> bool {
        self.task.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, TestAppContext};

    struct Tile {
        stale: StaleTimer,
    }

    fn slot(t: &mut Tile) -> &mut StaleTimer {
        &mut t.stale
    }

    fn t0() -> DateTime<Utc> {
        "2026-10-01T12:00:00Z".parse().unwrap()
    }

    const AFTER: Duration = Duration::from_secs(900);

    fn tile(cx: &mut TestAppContext) -> gpui::Entity<Tile> {
        cx.new(|_| Tile {
            stale: StaleTimer::new(),
        })
    }

    #[gpui::test]
    fn the_timer_fires_once_at_source_time_plus_threshold(cx: &mut TestAppContext) {
        let tile = tile(cx);
        // Armed 60 s after the source time: 840 s to wait.
        tile.update(cx, |t, cx| {
            t.stale.arm(
                Some(t0()),
                AFTER,
                t0() + chrono::Duration::seconds(60),
                cx,
                slot,
            )
        });
        cx.executor().advance_clock(Duration::from_secs(839));
        cx.run_until_parked();
        assert!(!tile.read_with(cx, |t, _| t.stale.fired_for(t0(), AFTER)));
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        assert!(tile.read_with(cx, |t, _| t.stale.fired_for(t0(), AFTER)));
        assert!(
            !tile.read_with(cx, |t, _| t.stale.is_armed()),
            "one wake-up, then done"
        );
        assert!(
            !tile.read_with(cx, |t, _| t
                .stale
                .fired_for(t0(), Duration::from_secs(1800))),
            "a different threshold is not this timer's verdict"
        );
    }

    #[gpui::test]
    fn arming_the_same_pair_again_keeps_the_first_deadline(cx: &mut TestAppContext) {
        let tile = tile(cx);
        let now = t0() + chrono::Duration::seconds(60);
        tile.update(cx, |t, cx| t.stale.arm(Some(t0()), AFTER, now, cx, slot));
        cx.executor().advance_clock(Duration::from_secs(800));
        tile.update(cx, |t, cx| t.stale.arm(Some(t0()), AFTER, now, cx, slot));
        cx.executor().advance_clock(Duration::from_secs(50));
        cx.run_until_parked();
        assert!(tile.read_with(cx, |t, _| t.stale.fired_for(t0(), AFTER)));
    }

    #[gpui::test]
    fn a_newer_source_time_replaces_the_wake_up(cx: &mut TestAppContext) {
        let tile = tile(cx);
        tile.update(cx, |t, cx| t.stale.arm(Some(t0()), AFTER, t0(), cx, slot));
        cx.executor().advance_clock(Duration::from_secs(600));
        let t1 = t0() + chrono::Duration::seconds(600);
        tile.update(cx, |t, cx| t.stale.arm(Some(t1), AFTER, t1, cx, slot));
        cx.executor().advance_clock(Duration::from_secs(400));
        cx.run_until_parked();
        assert!(!tile.read_with(cx, |t, _| t.stale.fired_for(t1, AFTER)));
        assert!(!tile.read_with(cx, |t, _| t.stale.fired_for(t0(), AFTER)));
    }

    /// A fired verdict belongs to its pair: a newer source time or a changed
    /// threshold re-arms with the verdict cleared, so a fresh delivery never
    /// reads stale.
    #[gpui::test]
    fn re_arming_after_a_fire_clears_the_verdict(cx: &mut TestAppContext) {
        let tile = tile(cx);
        tile.update(cx, |t, cx| t.stale.arm(Some(t0()), AFTER, t0(), cx, slot));
        cx.executor().advance_clock(AFTER + Duration::from_secs(1));
        cx.run_until_parked();
        assert!(tile.read_with(cx, |t, _| t.stale.fired_for(t0(), AFTER)));

        // A newer source time: fresh, with a wake-up pending.
        let t1 = t0() + AFTER + chrono::Duration::seconds(1);
        tile.update(cx, |t, cx| t.stale.arm(Some(t1), AFTER, t1, cx, slot));
        assert!(!tile.read_with(cx, |t, _| t.stale.fired_for(t1, AFTER)));
        assert!(!tile.read_with(cx, |t, _| t.stale.fired_for(t0(), AFTER)));
        assert!(tile.read_with(cx, |t, _| t.stale.is_armed()));

        // Fire again, then lengthen the threshold over the same source time.
        cx.executor().advance_clock(AFTER + Duration::from_secs(1));
        cx.run_until_parked();
        assert!(tile.read_with(cx, |t, _| t.stale.fired_for(t1, AFTER)));
        let longer = AFTER * 2;
        tile.update(cx, |t, cx| t.stale.arm(Some(t1), longer, t1, cx, slot));
        assert!(!tile.read_with(cx, |t, _| t.stale.fired_for(t1, longer)));
        assert!(!tile.read_with(cx, |t, _| t.stale.fired_for(t1, AFTER)));
        assert!(tile.read_with(cx, |t, _| t.stale.is_armed()));
    }

    #[gpui::test]
    fn disarming_drops_the_wake_up(cx: &mut TestAppContext) {
        let tile = tile(cx);
        tile.update(cx, |t, cx| t.stale.arm(Some(t0()), AFTER, t0(), cx, slot));
        tile.update(cx, |t, _| t.stale.disarm());
        cx.executor().advance_clock(AFTER * 2);
        cx.run_until_parked();
        assert!(!tile.read_with(cx, |t, _| t.stale.fired_for(t0(), AFTER)));
        assert!(!tile.read_with(cx, |t, _| t.stale.is_armed()));
    }

    #[gpui::test]
    fn a_dropped_tile_cancels_its_wake_up(cx: &mut TestAppContext) {
        let tile = tile(cx);
        tile.update(cx, |t, cx| t.stale.arm(Some(t0()), AFTER, t0(), cx, slot));
        let weak = tile.downgrade();
        drop(tile);
        cx.run_until_parked();
        assert!(weak.upgrade().is_none(), "the tile and its task are gone");
        cx.executor().advance_clock(AFTER * 2);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn a_deadline_already_passed_arms_nothing(cx: &mut TestAppContext) {
        let tile = tile(cx);
        tile.update(cx, |t, cx| {
            t.stale.arm(
                Some(t0()),
                AFTER,
                t0() + chrono::Duration::seconds(901),
                cx,
                slot,
            )
        });
        assert!(!tile.read_with(cx, |t, _| t.stale.is_armed()));
    }
}
