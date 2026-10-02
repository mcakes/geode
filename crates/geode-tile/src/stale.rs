//! Wake-ups at the moments a tile's painted source times turn stale.
//! Render decides staleness by comparing each source time with the clock, so a
//! tile nothing repaints would keep its fresh tone forever. The timer arms per
//! (set of source times, threshold): one wake-up at the earliest
//! `source_at + stale_after` still ahead, and when it fires it records that
//! time as stale, notifies the tile and arms the next time's wake-up itself,
//! so every time in the set turns stale at its own deadline while the tile
//! is idle. The recorded verdict holds where the wall clock disagrees (a test
//! clock, a clock stepped back). Arming a different set or threshold — a
//! newer delivery, a reload — forgets the old verdict, so a fresh delivery
//! never paints stale. It owns its `Task`: re-arming replaces it, and
//! `disarm` or dropping the tile cancels it. Nothing here runs per frame.

use std::time::Duration;

use chrono::{DateTime, Utc};
use gpui::{Context, Task};

#[derive(Default)]
pub struct StaleTimer {
    /// The source times (ascending, distinct) and threshold this timer
    /// answers for; empty when disarmed.
    times: Vec<DateTime<Utc>>,
    after: Duration,
    /// The latest of `times` whose wake-up fired: it and every earlier time
    /// read stale. Reset with every new set or threshold.
    fired: Option<DateTime<Utc>>,
    task: Option<Task<()>>,
}

impl StaleTimer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Arm for one source time; `None` disarms. See [`StaleTimer::arm_each`].
    pub fn arm<T: 'static>(
        &mut self,
        at: Option<DateTime<Utc>>,
        after: Duration,
        now: DateTime<Utc>,
        cx: &mut Context<T>,
        slot: fn(&mut T) -> &mut StaleTimer,
    ) {
        self.arm_each(at.into_iter().collect(), after, now, cx, slot);
    }

    /// Arm a wake-up at each `at + after` still ahead of `now`, one at a
    /// time in order, unless this set and threshold are already armed. An
    /// empty set disarms. A deadline already passed arms nothing: render's
    /// own comparison says stale. `slot` finds this timer on the tile when
    /// the task wakes.
    pub fn arm_each<T: 'static>(
        &mut self,
        mut times: Vec<DateTime<Utc>>,
        after: Duration,
        now: DateTime<Utc>,
        cx: &mut Context<T>,
        slot: fn(&mut T) -> &mut StaleTimer,
    ) {
        times.sort();
        times.dedup();
        if times.is_empty() {
            self.disarm();
            return;
        }
        if self.times == times && self.after == after {
            return;
        }
        self.disarm();
        self.times = times;
        self.after = after;
        let ahead = self.times.iter().copied().find_map(|at| {
            let wait = deadline(at, after)?
                .signed_duration_since(now)
                .to_std()
                .ok()?;
            Some((at, wait))
        });
        if let Some((at, wait)) = ahead {
            self.schedule(at, wait, cx, slot);
        }
    }

    /// Wake after `wait` for `at`; on waking, record `at` stale and schedule
    /// the next time in the set, measured from this deadline.
    fn schedule<T: 'static>(
        &mut self,
        at: DateTime<Utc>,
        wait: Duration,
        cx: &mut Context<T>,
        slot: fn(&mut T) -> &mut StaleTimer,
    ) {
        self.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |tile, cx| {
                let timer = slot(tile);
                timer.task = None;
                timer.fired = Some(at);
                let next = timer.times.iter().copied().find(|t| *t > at);
                if let Some(next) = next {
                    let wait = (next - at).to_std().unwrap_or_default();
                    timer.schedule(next, wait, cx, slot);
                }
                cx.notify();
            });
        }));
    }

    /// Drop the wake-up and forget the set and its verdict: a hidden tile
    /// wakes for nothing, and its show arms again.
    pub fn disarm(&mut self) {
        self.task = None;
        self.times.clear();
        self.fired = None;
    }

    /// Whether this timer fired for a source time at or after `at` under the
    /// same threshold — the verdict render ORs with its clock comparison.
    pub fn fired_for(&self, at: DateTime<Utc>, after: Duration) -> bool {
        self.after == after && self.fired.is_some_and(|fired| at <= fired)
    }

    /// Whether a wake-up is pending.
    pub fn is_armed(&self) -> bool {
        self.task.is_some()
    }
}

fn deadline(at: DateTime<Utc>, after: Duration) -> Option<DateTime<Utc>> {
    at.checked_add_signed(chrono::Duration::from_std(after).ok()?)
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

    /// Each time in a set turns stale at its own deadline: the first
    /// wake-up arms the next, with no re-arm from the tile.
    #[gpui::test]
    fn each_time_in_a_set_fires_at_its_own_deadline(cx: &mut TestAppContext) {
        let tile = tile(cx);
        let t1 = t0() + chrono::Duration::seconds(600);
        tile.update(cx, |t, cx| {
            t.stale.arm_each(vec![t1, t0()], AFTER, t0(), cx, slot)
        });
        cx.executor().advance_clock(AFTER + Duration::from_secs(1));
        cx.run_until_parked();
        assert!(tile.read_with(cx, |t, _| t.stale.fired_for(t0(), AFTER)));
        assert!(!tile.read_with(cx, |t, _| t.stale.fired_for(t1, AFTER)));
        assert!(
            tile.read_with(cx, |t, _| t.stale.is_armed()),
            "the next wake-up"
        );
        cx.executor().advance_clock(Duration::from_secs(600));
        cx.run_until_parked();
        assert!(tile.read_with(cx, |t, _| t.stale.fired_for(t1, AFTER)));
        assert!(tile.read_with(cx, |t, _| t.stale.fired_for(t0(), AFTER)));
        assert!(!tile.read_with(cx, |t, _| t.stale.is_armed()));
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
