//! Frame-time instrumentation (spec §7.4): a cheap, always-compiled
//! fixed-bucket histogram of frame intervals, owned by `ShellView` and fed
//! from the top of its `render`. Pure logic — no gpui, no I/O, no clocks
//! (callers hand in durations they measured) — so it is fully unit-testable
//! and obeys the render discipline it exists to measure: recording is O(1),
//! allocation-free, and lock-free (a plain fixed-size array behind `&mut`).
//!
//! **What the recorded signal is** (recorded decision): `ShellView` stores
//! the `Instant` at the top of each `render` call and records the interval
//! between consecutive renders. Geode is event-driven — the window only
//! redraws on invalidation — so this measures *render-to-render intervals
//! while Geode is actually rendering*: during continuous interaction
//! (key-repeat resizes, divider drags, palette typing) consecutive samples
//! are honest frame times including gpui's full prepaint/paint/present of
//! the previous frame. It does NOT capture: compositor/display latency
//! beyond what delays the next render, frames where only a child entity
//! re-rendered without `ShellView::render` running, or the duration of the
//! *last* render before an idle gap. Intervals longer than [`IDLE_CUTOFF`]
//! are discarded (counted in [`FrameHistogram::discarded_idle`]) — after the
//! user pauses, the next render's interval measures the pause, not a frame.
//! The alternative (gpui's own draw-duration histograms) needs the `profiler`
//! feature; see `docs/perf.md` and the `profiling` feature flag for that.

use std::time::Duration;

/// Intervals at or above this are idle gaps between interaction bursts, not
/// frame times, and are excluded from the histogram (see the module doc).
/// Deliberately well above [`BUCKET_UPPER_BOUNDS_MICROS`]' top (100ms) so a
/// genuinely catastrophic-but-real 100–500ms frame still lands in the
/// overflow bucket and drives `max` instead of being mistaken for idleness.
///
/// **Coupled to `hot_reload::RELOAD_POLL_INTERVAL` (also 500ms) — not by
/// any code reference, only by value (Phase 4b final review, MAJ-4's
/// related finding).** A visible diagnostics tile's `Diagnostics::
/// refresh_frame_hist` copies `ShellView::perf` and calls `cx.notify()`
/// on that same ~500ms tick even when nothing new was recorded; that
/// notify alone drives a repaint, which records a fresh render interval,
/// which is exactly the reload tick's own period later — landing just
/// under this cutoff and being discarded as an idle gap rather than
/// counted. If either constant ever moves independently (a shorter
/// reload poll, or a lower cutoff to catch shorter real idle gaps), an
/// idle diagnostics tile could instead pin the app in a self-sustaining
/// full-repaint loop: notify -> repaint -> interval recorded as a real
/// frame -> `perf.record()` moves `count()`/`max_micros()` -> the next
/// tick's `refresh_frame_hist` sees a change and copies again -> notify.
/// Keep these two constants at least this close, or add a floor: a
/// notify with no real state change must not, on its own, ever produce
/// an interval below `IDLE_CUTOFF`.
pub const IDLE_CUTOFF: Duration = Duration::from_millis(500);

/// Upper bounds (inclusive ceiling of each bucket, in microseconds) of the
/// log-spaced buckets: 12 buckets per decade over 0.1ms → 100ms, i.e.
/// `100µs * 10^((i+1)/12)` rounded to integer microseconds. A hardcoded
/// table rather than runtime `powf` so recording is a pure integer binary
/// search — no floats on the record path, and the boundaries are directly
/// testable. `boundaries_match_the_log_spacing_formula` pins the formula.
pub const BUCKET_UPPER_BOUNDS_MICROS: [u64; 36] = [
    121, 147, 178, 215, 261, 316, 383, 464, 562, 681, 825, 1_000, 1_212, 1_468, 1_778, 2_154,
    2_610, 3_162, 3_831, 4_642, 5_623, 6_813, 8_254, 10_000, 12_115, 14_678, 17_783, 21_544,
    26_102, 31_623, 38_312, 46_416, 56_234, 68_129, 82_540, 100_000,
];

/// Number of regular buckets (the overflow bucket is stored separately).
pub const NUM_BUCKETS: usize = BUCKET_UPPER_BOUNDS_MICROS.len();

/// Fixed-size log-bucket histogram of frame intervals. All counters
/// saturate instead of wrapping: per-bucket `u32` (4 billion samples per
/// bucket — weeks of 120Hz rendering) and a `u64` total. Percentile
/// queries walk the 37 counters — they run only when the debug overlay
/// renders, never on the plain record path.
#[derive(Debug, Clone)]
pub struct FrameHistogram {
    buckets: [u32; NUM_BUCKETS],
    /// Samples above the last bucket's bound (100ms) but below the caller's
    /// idle cutoff — real, terrible frames worth counting.
    overflow: u32,
    /// Total recorded samples (regular buckets + overflow).
    count: u64,
    /// Largest recorded sample, in microseconds.
    max_micros: u64,
    /// Samples the caller discarded as idle gaps rather than recording
    /// (bumped via [`Self::note_discarded_idle`]) — kept out of `max` so
    /// idle time never reads as a slow frame. Today only the
    /// profiling-gated `perf::dump` surfaces this count; the default
    /// overlay does not show it.
    discarded_idle: u64,
}

impl Default for FrameHistogram {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameHistogram {
    pub const fn new() -> Self {
        FrameHistogram {
            buckets: [0; NUM_BUCKETS],
            overflow: 0,
            count: 0,
            max_micros: 0,
            discarded_idle: 0,
        }
    }

    /// Record one frame interval of `micros` microseconds. O(1): a binary
    /// search over 36 integers plus two saturating adds. No allocation.
    #[inline]
    pub fn record_micros(&mut self, micros: u64) {
        self.record_micros_n(micros, 1);
    }

    /// [`Self::record_micros`]'s core, adding `n` identical samples in one
    /// step — public so saturation is testable without four billion calls.
    pub fn record_micros_n(&mut self, micros: u64, n: u32) {
        if n == 0 {
            return;
        }
        // partition_point over `b < micros` = index of the first bucket
        // whose upper bound holds this sample. Bounds are INCLUSIVE
        // ceilings: bucket i covers (bound[i-1], bound[i]], so a sample
        // equal to a bound belongs to that bucket, not the next one.
        // Concretely: 121µs lands in bucket 0, 122µs in bucket 1
        // (`samples_land_in_the_documented_buckets` pins this).
        let idx = BUCKET_UPPER_BOUNDS_MICROS.partition_point(|&b| b < micros);
        if idx < NUM_BUCKETS {
            self.buckets[idx] = self.buckets[idx].saturating_add(n);
        } else {
            self.overflow = self.overflow.saturating_add(n);
        }
        self.count = self.count.saturating_add(n as u64);
        if micros > self.max_micros {
            self.max_micros = micros;
        }
    }

    /// Count (without recording) an interval the caller classified as an
    /// idle gap (>= [`IDLE_CUTOFF`]).
    #[inline]
    pub fn note_discarded_idle(&mut self) {
        self.discarded_idle = self.discarded_idle.saturating_add(1);
    }

    /// Total recorded samples since construction or the last [`Self::reset`].
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Largest recorded sample in microseconds, 0 when empty.
    pub fn max_micros(&self) -> u64 {
        self.max_micros
    }

    /// Idle gaps noted since the last reset (see [`Self::note_discarded_idle`]).
    pub fn discarded_idle(&self) -> u64 {
        self.discarded_idle
    }

    /// Approximate percentile (`p` in 0..=100), as microseconds: the upper
    /// bound of the bucket containing the p-th sample, capped at the
    /// observed max (so p100 — and any percentile landing in the top or
    /// overflow bucket — reports the real max, not a bucket ceiling).
    /// `None` when no samples have been recorded. Cost: one walk of 37
    /// counters — fine for the overlay, not meant for any hot path.
    pub fn percentile_micros(&self, p: f64) -> Option<u64> {
        if self.count == 0 {
            return None;
        }
        let p = p.clamp(0.0, 100.0);
        // Rank of the target sample, 1-based: p50 of 4 samples → rank 2.
        let rank = ((p / 100.0) * self.count as f64).ceil().max(1.0) as u64;
        let mut seen: u64 = 0;
        for (i, &n) in self.buckets.iter().enumerate() {
            seen += n as u64;
            if seen >= rank {
                return Some(BUCKET_UPPER_BOUNDS_MICROS[i].min(self.max_micros));
            }
        }
        // Rank falls in the overflow bucket: the honest answer is the max.
        Some(self.max_micros)
    }

    /// Zero every counter (including `max` and the idle-gap count).
    pub fn reset(&mut self) {
        *self = FrameHistogram::new();
    }
}

/// Requery timing (Phase 3 §6.8): the two halves of §7.1's "query +
/// snapshot handoff + first painted frame" that the headless benchmarks
/// cannot see. The blotter records submit→snapshot on `deliver` and
/// snapshot→paint on the first render after it. Fixed-size, allocation-
/// free, never notifies — the same discipline as `FrameHistogram`.
#[derive(Debug)]
pub struct RequeryStats {
    submit_to_snapshot: FrameHistogram,
    snapshot_to_paint: FrameHistogram,
    pending_snapshot: Option<u64>,
    last: Option<(u64, u64)>,
}

impl Default for RequeryStats {
    fn default() -> Self {
        Self::new()
    }
}

impl RequeryStats {
    pub const fn new() -> Self {
        RequeryStats {
            submit_to_snapshot: FrameHistogram::new(),
            snapshot_to_paint: FrameHistogram::new(),
            pending_snapshot: None,
            last: None,
        }
    }

    pub fn record_submit_to_snapshot(&mut self, micros: u64) {
        self.submit_to_snapshot.record_micros(micros);
        self.pending_snapshot = Some(micros);
    }

    pub fn record_snapshot_to_paint(&mut self, micros: u64) {
        self.snapshot_to_paint.record_micros(micros);
        if let Some(first) = self.pending_snapshot.take() {
            self.last = Some((first, micros));
        }
    }

    pub fn last(&self) -> Option<(u64, u64)> {
        self.last
    }

    pub fn submit_to_snapshot(&self) -> &FrameHistogram {
        &self.submit_to_snapshot
    }

    pub fn snapshot_to_paint(&self) -> &FrameHistogram {
        &self.snapshot_to_paint
    }

    pub fn reset(&mut self) {
        self.submit_to_snapshot.reset();
        self.snapshot_to_paint.reset();
        self.pending_snapshot = None;
        self.last = None;
    }
}

/// Render a microsecond value as a short millisecond string for the debug
/// overlay: `0.12ms` under 1ms, `4.6ms` under 10ms, `83ms` beyond. Returns
/// a fresh small `String` — called only while the overlay renders (a
/// handful of calls per frame in the sanctioned small-String class), never
/// on the record path.
pub fn format_ms(micros: u64) -> String {
    let ms = micros as f64 / 1000.0;
    if ms < 1.0 {
        format!("{ms:.2}ms")
    } else if ms < 10.0 {
        format!("{ms:.1}ms")
    } else {
        format!("{ms:.0}ms")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundaries_match_the_log_spacing_formula() {
        for (i, &bound) in BUCKET_UPPER_BOUNDS_MICROS.iter().enumerate() {
            let expected = 100.0_f64 * 10.0_f64.powf((i as f64 + 1.0) / 12.0);
            assert_eq!(
                bound,
                expected.round() as u64,
                "bucket {i} bound {bound} != round({expected})"
            );
        }
        // The table spans exactly 0.1ms → 100ms.
        assert_eq!(BUCKET_UPPER_BOUNDS_MICROS[NUM_BUCKETS - 1], 100_000);
        assert_eq!(BUCKET_UPPER_BOUNDS_MICROS[11], 1_000);
        assert_eq!(BUCKET_UPPER_BOUNDS_MICROS[23], 10_000);
    }

    #[test]
    fn empty_histogram_reports_nothing() {
        let h = FrameHistogram::new();
        assert_eq!(h.count(), 0);
        assert_eq!(h.max_micros(), 0);
        assert_eq!(h.percentile_micros(50.0), None);
    }

    #[test]
    fn samples_land_in_the_documented_buckets() {
        let mut h = FrameHistogram::new();
        // At-or-below the floor → bucket 0; a bound value belongs to its
        // own bucket; one past a bound belongs to the next.
        h.record_micros(1); // bucket 0
        h.record_micros(121); // bucket 0 (bound inclusive)
        h.record_micros(122); // bucket 1
        h.record_micros(100_000); // last bucket (bound inclusive)
        h.record_micros(100_001); // overflow
        assert_eq!(h.count(), 5);
        assert_eq!(h.max_micros(), 100_001);
        // p20 → rank 1 → bucket 0's bound; p40 → rank 2 → still bucket 0.
        assert_eq!(h.percentile_micros(20.0), Some(121));
        assert_eq!(h.percentile_micros(40.0), Some(121));
        // p60 → rank 3 → bucket 1's bound (147).
        assert_eq!(h.percentile_micros(60.0), Some(147));
        // p80 → rank 4 → last bucket, capped at max? bound 100_000 < max.
        assert_eq!(h.percentile_micros(80.0), Some(100_000));
        // p100 → rank 5 → overflow → the real max.
        assert_eq!(h.percentile_micros(100.0), Some(100_001));
    }

    #[test]
    fn percentile_is_capped_at_the_observed_max() {
        let mut h = FrameHistogram::new();
        // A single 130µs sample sits in bucket 1 (bound 147); every
        // percentile of a one-sample histogram is that sample, and the
        // cap keeps the report at 130, not the bucket ceiling.
        h.record_micros(130);
        assert_eq!(h.percentile_micros(50.0), Some(130));
        assert_eq!(h.percentile_micros(100.0), Some(130));
    }

    #[test]
    fn percentiles_over_a_spread_pick_the_right_buckets() {
        let mut h = FrameHistogram::new();
        // 90 fast frames ~4ms, 10 slow ~40ms.
        h.record_micros_n(4_000, 90);
        h.record_micros_n(40_000, 10);
        assert_eq!(h.count(), 100);
        // p50 → rank 50 → the 4ms bucket (bound 4_642).
        assert_eq!(h.percentile_micros(50.0), Some(4_642));
        // p95 → rank 95 → the 40ms bucket (bound 46_416), capped at 40_000.
        assert_eq!(h.percentile_micros(95.0), Some(40_000));
    }

    #[test]
    fn bucket_counters_saturate_instead_of_wrapping() {
        let mut h = FrameHistogram::new();
        h.record_micros_n(200, u32::MAX);
        h.record_micros_n(200, u32::MAX);
        // The bucket pinned at u32::MAX, the u64 total kept counting.
        assert_eq!(h.count(), 2 * u32::MAX as u64);
        // Percentiles still answer (rank walk uses the u64 total; the
        // saturated bucket holds every visible sample).
        assert_eq!(h.percentile_micros(50.0), Some(200));
        assert_eq!(h.max_micros(), 200);
    }

    #[test]
    fn zero_count_add_is_a_no_op() {
        let mut h = FrameHistogram::new();
        h.record_micros_n(500, 0);
        assert_eq!(h.count(), 0);
        assert_eq!(h.percentile_micros(50.0), None);
    }

    #[test]
    fn reset_zeroes_everything() {
        let mut h = FrameHistogram::new();
        h.record_micros(5_000);
        h.record_micros(200_000);
        h.note_discarded_idle();
        assert!(h.count() > 0);
        h.reset();
        assert_eq!(h.count(), 0);
        assert_eq!(h.max_micros(), 0);
        assert_eq!(h.discarded_idle(), 0);
        assert_eq!(h.percentile_micros(99.0), None);
    }

    #[test]
    fn discarded_idle_is_counted_but_never_recorded() {
        let mut h = FrameHistogram::new();
        h.note_discarded_idle();
        h.note_discarded_idle();
        assert_eq!(h.discarded_idle(), 2);
        assert_eq!(h.count(), 0);
        assert_eq!(h.max_micros(), 0);
    }

    #[test]
    fn format_ms_picks_sensible_precision() {
        assert_eq!(format_ms(121), "0.12ms");
        // 0.999ms takes the <1ms branch and rounds up within it.
        assert_eq!(format_ms(999), "1.00ms");
        assert_eq!(format_ms(4_642), "4.6ms");
        assert_eq!(format_ms(83_000), "83ms");
        assert_eq!(format_ms(0), "0.00ms");
    }

    #[test]
    fn requery_stats_pair_the_two_halves_and_summarise_each() {
        let mut s = RequeryStats::new();
        assert_eq!(s.last(), None);
        s.record_submit_to_snapshot(12_000);
        assert_eq!(s.last(), None, "half a pair is not a pair");
        s.record_snapshot_to_paint(3_000);
        assert_eq!(s.last(), Some((12_000, 3_000)));
        s.record_submit_to_snapshot(20_000);
        s.record_snapshot_to_paint(4_000);
        assert_eq!(s.last(), Some((20_000, 4_000)));
        assert_eq!(s.submit_to_snapshot().count(), 2);
        assert_eq!(s.snapshot_to_paint().count(), 2);
        assert!(s.submit_to_snapshot().percentile_micros(50.0).unwrap() >= 12_000);
        s.reset();
        assert_eq!(s.last(), None);
        assert_eq!(s.submit_to_snapshot().count(), 0);
    }
}
