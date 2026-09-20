//! The x axis (spec §8.2): `Session` maps bucket INDEX to x so a span
//! with no bucket has no width; `Continuous` maps wall-clock micros.
//! Bucket `i` occupies `[i, i+1)` (session) or `[b_i, b_i + step)`
//! (continuous); its centre is where the point paints.

use super::Rect;
use super::view::View;
use chrono::{DateTime, Datelike, FixedOffset, TimeDelta, TimeZone, Timelike, Utc};

#[derive(Clone, Copy, Debug)]
pub enum TimeScale<'a> {
    Session { buckets: &'a [i64] },
    Continuous { buckets: &'a [i64], step_us: i64 },
}

/// The nearest bucket to a cursor (spec §8.1).
pub struct Crosshair;

impl<'a> TimeScale<'a> {
    pub fn buckets(&self) -> &'a [i64] {
        match self {
            TimeScale::Session { buckets } | TimeScale::Continuous { buckets, .. } => buckets,
        }
    }

    /// The loaded range in this scale's units: `(0, n)` for session,
    /// `(first, last + step)` for continuous; `(0, 0)` when empty.
    pub fn full(&self) -> (f64, f64) {
        match *self {
            TimeScale::Session { buckets } => (0.0, buckets.len() as f64),
            TimeScale::Continuous { buckets, step_us } => match (buckets.first(), buckets.last()) {
                (Some(&f), Some(&l)) => (f as f64, (l + step_us) as f64),
                _ => (0.0, 0.0),
            },
        }
    }

    /// Bucket `index`'s centre in its own units.
    fn centre(&self, index: usize) -> f64 {
        match *self {
            TimeScale::Session { .. } => index as f64 + 0.5,
            TimeScale::Continuous { buckets, step_us } => {
                buckets[index] as f64 + step_us as f64 / 2.0
            }
        }
    }

    fn plot_x(&self, u: f64, view: View, plot: Rect) -> f32 {
        let span = view.span();
        if span.is_nan() || span <= 0.0 {
            return plot.x;
        }
        plot.x + ((u - view.lo) / span) as f32 * plot.w
    }

    /// Bucket `index`'s x on the plot (its centre).
    pub fn x_of(&self, index: usize, view: View, plot: Rect) -> f32 {
        self.plot_x(self.centre(index), view, plot)
    }

    /// `[start, end)` of the buckets whose slot intersects the view.
    pub fn visible(&self, view: View) -> (usize, usize) {
        let b = self.buckets();
        if b.is_empty() {
            return (0, 0);
        }
        match *self {
            TimeScale::Session { .. } => {
                let start = view.lo.floor().max(0.0) as usize;
                let end = (view.hi.ceil().max(0.0) as usize).min(b.len());
                (start.min(end), end)
            }
            TimeScale::Continuous { buckets, step_us } => {
                // first bucket whose slot end is after view.lo
                let start = buckets.partition_point(|&t| ((t + step_us) as f64) <= view.lo);
                let end = buckets.partition_point(|&t| (t as f64) < view.hi);
                (start.min(end), end)
            }
        }
    }
}

impl Crosshair {
    /// The visible bucket whose centre is nearest `cursor_x`.
    pub fn at(cursor_x: f32, scale: &TimeScale, view: View, plot: Rect) -> Option<usize> {
        let (start, end) = scale.visible(view);
        if start >= end {
            return None;
        }
        // x is monotone in index: binary search the first centre at or
        // past the cursor, then compare with its predecessor.
        let mut lo = start;
        let mut hi = end;
        while lo < hi {
            let mid = (lo + hi) / 2;
            if scale.x_of(mid, view, plot) < cursor_x {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == start {
            return Some(start);
        }
        if lo == end {
            return Some(end - 1);
        }
        let before = (scale.x_of(lo - 1, view, plot) - cursor_x).abs();
        let after = (scale.x_of(lo, view, plot) - cursor_x).abs();
        Some(if after < before { lo } else { lo - 1 })
    }
}

/// A tick unit (spec §8.2), finest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Unit {
    Minute,
    Hour,
    Day,
    Month,
    Year,
}

impl Unit {
    /// Finest first.
    pub const ALL: [Unit; 5] = [Unit::Minute, Unit::Hour, Unit::Day, Unit::Month, Unit::Year];

    /// The value that changes at this unit's boundary.
    fn value(self, t: &DateTime<FixedOffset>) -> (i32, u32, u32, u32, u32) {
        match self {
            Unit::Minute => (t.year(), t.month(), t.day(), t.hour(), t.minute()),
            Unit::Hour => (t.year(), t.month(), t.day(), t.hour(), 0),
            Unit::Day => (t.year(), t.month(), t.day(), 0, 0),
            Unit::Month => (t.year(), t.month(), 0, 0, 0),
            Unit::Year => (t.year(), 0, 0, 0, 0),
        }
    }

    pub fn label(self, t: &DateTime<FixedOffset>) -> String {
        match self {
            Unit::Year => t.format("%Y").to_string(),
            Unit::Month => t.format("%b %y").to_string(),
            // `%-d` is chrono's own no-pad flag (`Pad::None` in
            // `format/strftime.rs`), not the platform's strftime, so it
            // is portable to Windows.
            Unit::Day => t.format("%-d %b").to_string(),
            Unit::Hour | Unit::Minute => t.format("%H:%M").to_string(),
        }
    }

    /// The boundary at or after `t` (this unit's next roll-over at or
    /// after `t`; `t` itself when on one).
    fn ceil(self, t: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
        let floored = self.floor(t);
        if floored == t { t } else { self.next(floored) }
    }

    fn floor(self, t: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
        let tz = *t.offset();
        let (y, mo, d, h, mi) = (t.year(), t.month(), t.day(), t.hour(), t.minute());
        let out = match self {
            Unit::Minute => tz.with_ymd_and_hms(y, mo, d, h, mi, 0),
            Unit::Hour => tz.with_ymd_and_hms(y, mo, d, h, 0, 0),
            Unit::Day => tz.with_ymd_and_hms(y, mo, d, 0, 0, 0),
            Unit::Month => tz.with_ymd_and_hms(y, mo, 1, 0, 0, 0),
            Unit::Year => tz.with_ymd_and_hms(y, 1, 1, 0, 0, 0),
        };
        out.single().unwrap_or(t)
    }

    fn next(self, t: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
        match self {
            Unit::Minute => t + TimeDelta::minutes(1),
            Unit::Hour => t + TimeDelta::hours(1),
            Unit::Day => t + TimeDelta::days(1),
            Unit::Month => {
                let (y, m) = if t.month() == 12 {
                    (t.year() + 1, 1)
                } else {
                    (t.year(), t.month() + 1)
                };
                t.offset()
                    .with_ymd_and_hms(y, m, 1, 0, 0, 0)
                    .single()
                    .unwrap_or(t)
            }
            Unit::Year => t
                .offset()
                .with_ymd_and_hms(t.year() + 1, 1, 1, 0, 0, 0)
                .single()
                .unwrap_or(t),
        }
    }

    fn approx_secs(self) -> f64 {
        match self {
            Unit::Minute => 60.0,
            Unit::Hour => 3_600.0,
            Unit::Day => 86_400.0,
            Unit::Month => 30.0 * 86_400.0,
            Unit::Year => 365.0 * 86_400.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tick {
    pub x: f32,
    pub label: String,
}

/// Fewest ticks an axis is worth painting: below this the chooser keeps
/// looking, since two labels read as a pair of dates, not a scale.
pub const MIN_TICKS: usize = 3;

/// Most boundaries a continuous unit may have inside a view before it is
/// skipped as unfittable.
const MAX_CANDIDATES: usize = 4_096;

/// A candidate boundary: its x on the plot and the time it labels.
type Candidate = (f32, DateTime<FixedOffset>);

fn at(us: i64, offset: FixedOffset) -> DateTime<FixedOffset> {
    DateTime::<Utc>::from_timestamp_micros(us)
        .unwrap_or_default()
        .with_timezone(&offset)
}

/// Ticks for the view into `out` (cleared first); the unit chosen, or
/// `None` when nothing is visible.
///
/// The chooser builds one candidate `Vec` per unit, so a call allocates
/// up to five short-lived vectors. That is deliberate: ticks are
/// recomputed on a chrome-key miss — a view, size, offset or bucket
/// change — never per frame, so the clarity is worth more here than the
/// churn; the per-frame paths stay allocation-free.
pub fn ticks(
    scale: &TimeScale,
    view: View,
    plot: Rect,
    tick_gap_px: f32,
    offset_secs: i32,
    out: &mut Vec<Tick>,
) -> Option<Unit> {
    out.clear();
    let offset =
        FixedOffset::east_opt(offset_secs).unwrap_or_else(|| FixedOffset::east_opt(0).unwrap());
    let (start, end) = scale.visible(view);
    if start >= end {
        return None;
    }
    // A unit finer than the data's own step says nothing the step does
    // not: on daily buckets every bucket is a new minute AND a new day,
    // so the two units offer the SAME candidates and only the coarser
    // one's label ("5 Jan", not "00:00") names what changed.
    let floor = resolution_floor(scale, start, end);
    let cands: [Vec<Candidate>; 5] = std::array::from_fn(|i| {
        let unit = Unit::ALL[i];
        if unit < floor {
            Vec::new()
        } else {
            candidates(scale, view, plot, unit, offset, start, end)
        }
    });

    // The four rules, in order (each pinned by a test below):
    //   1. the FINEST unit with MIN_TICKS candidates that already fit;
    //   2. else the COARSEST unit that still keeps MIN_TICKS once thinned;
    //   3. else the FINEST unit with two candidates, thinned;
    //   4. else one day-labelled tick on the first visible bucket.
    let chosen = (0..Unit::ALL.len())
        .find(|&i| cands[i].len() >= MIN_TICKS && min_gap(&cands[i]) >= tick_gap_px)
        .map(|i| (i, 1usize))
        .or_else(|| {
            (0..Unit::ALL.len()).rev().find_map(|i| {
                let k = thin_by(&cands[i], tick_gap_px);
                (cands[i].len().div_ceil(k) >= MIN_TICKS).then_some((i, k))
            })
        })
        .or_else(|| {
            (0..Unit::ALL.len())
                .find(|&i| cands[i].len() >= 2)
                .map(|i| (i, thin_by(&cands[i], tick_gap_px)))
        });
    let Some((index, thin)) = chosen else {
        let t = at(scale.buckets()[start], offset);
        out.push(Tick {
            x: scale.x_of(start, view, plot),
            label: Unit::Day.label(&t),
        });
        return Some(Unit::Day);
    };

    let unit = Unit::ALL[index];
    let mut last_x = f32::NEG_INFINITY;
    for (i, (x, t)) in cands[index].iter().enumerate() {
        // `x <= last_x` keeps x strictly increasing however coarse the
        // plot: two candidates can round to the same pixel.
        if i % thin != 0 || *x <= last_x {
            continue;
        }
        out.push(Tick {
            x: *x,
            label: unit.label(t),
        });
        last_x = *x;
    }
    Some(unit)
}

/// The smallest distance between two consecutive candidates, or
/// infinity when there are fewer than two.
fn min_gap(cands: &[Candidate]) -> f32 {
    cands
        .windows(2)
        .map(|w| w[1].0 - w[0].0)
        .fold(f32::INFINITY, f32::min)
}

/// Keep every `k`-th candidate: the smallest `k` whose stride clears
/// `tick_gap_px`. A degenerate gap — zero or negative, from a plot with
/// no width — keeps the first candidate alone.
fn thin_by(cands: &[Candidate], tick_gap_px: f32) -> usize {
    let gap = min_gap(cands);
    if !gap.is_finite() || gap <= 0.0 {
        return cands.len().max(1);
    }
    (tick_gap_px / gap).ceil().max(1.0) as usize
}

/// The finest unit worth asking about: the coarsest whose own length the
/// data's step still fills (minute bars → `Minute`, daily sessions →
/// `Day`); `Minute` when the step is finer than a minute or unknown.
fn resolution_floor(scale: &TimeScale, start: usize, end: usize) -> Unit {
    let step_us = match *scale {
        TimeScale::Continuous { step_us, .. } => step_us,
        TimeScale::Session { buckets } => {
            let mut smallest = i64::MAX;
            for i in start.max(1)..end {
                let d = buckets[i] - buckets[i - 1];
                if d > 0 && d < smallest {
                    smallest = d;
                }
            }
            smallest
        }
    };
    if step_us <= 0 || step_us == i64::MAX {
        return Unit::Minute;
    }
    let secs = step_us as f64 / 1e6;
    let mut floor = Unit::Minute;
    for unit in Unit::ALL {
        if unit.approx_secs() <= secs {
            floor = unit;
        }
    }
    floor
}

fn candidates(
    scale: &TimeScale,
    view: View,
    plot: Rect,
    unit: Unit,
    offset: FixedOffset,
    start: usize,
    end: usize,
) -> Vec<Candidate> {
    let b = scale.buckets();
    match *scale {
        TimeScale::Session { .. } => {
            let mut out = Vec::new();
            for i in start..end {
                let t = at(b[i], offset);
                // Compared against the PREVIOUS bucket even when it is
                // outside the view: the tick is where the unit CHANGES,
                // so a view opening mid-month must not paint one on its
                // first bucket. `i == 0` is the one unconditional
                // candidate.
                let is_tick = i == 0 || unit.value(&t) != unit.value(&at(b[i - 1], offset));
                if is_tick {
                    out.push((scale.x_of(i, view, plot), t));
                }
            }
            out
        }
        TimeScale::Continuous { .. } => {
            let span = view.span();
            if !span.is_finite() || span <= 0.0 {
                return Vec::new();
            }
            if span / 1e6 / unit.approx_secs() > MAX_CANDIDATES as f64 {
                return Vec::new();
            }
            let mut out = Vec::new();
            let mut t = unit.ceil(at(view.lo as i64, offset));
            let hi = view.hi as i64;
            while t.timestamp_micros() < hi && out.len() <= MAX_CANDIDATES {
                let u = t.timestamp_micros() as f64;
                let x = plot.x + ((u - view.lo) / span) as f32 * plot.w;
                out.push((x, t));
                t = unit.next(t);
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PLOT: Rect = Rect::new(100.0, 0.0, 1000.0, 100.0);
    const DAY: i64 = 86_400_000_000;
    // Mon..Fri, then Mon (a weekend gap).
    const BUCKETS: [i64; 6] = [0, DAY, 2 * DAY, 3 * DAY, 4 * DAY, 7 * DAY];

    #[test]
    fn a_session_scale_gives_every_bucket_the_same_width_whatever_its_gap() {
        let s = TimeScale::Session { buckets: &BUCKETS };
        assert_eq!(s.full(), (0.0, 6.0));
        let v = View::full(s.full());
        let xs: Vec<f32> = (0..6).map(|i| s.x_of(i, v, PLOT)).collect();
        for w in xs.windows(2) {
            assert!((w[1] - w[0] - 1000.0 / 6.0).abs() < 1e-3, "{xs:?}");
        }
        assert!(
            (xs[0] - (100.0 + 1000.0 / 12.0)).abs() < 1e-3,
            "centred in its slot"
        );
    }

    #[test]
    fn a_continuous_scale_leaves_the_weekend_its_width() {
        let s = TimeScale::Continuous {
            buckets: &BUCKETS,
            step_us: DAY,
        };
        assert_eq!(s.full(), (0.0, 8.0 * DAY as f64));
        let v = View::full(s.full());
        let fri = s.x_of(4, v, PLOT);
        let mon = s.x_of(5, v, PLOT);
        let thu = s.x_of(3, v, PLOT);
        assert!(
            (mon - fri) > 2.5 * (fri - thu),
            "the gap is three days wide"
        );
    }

    #[test]
    fn visible_is_the_index_window_the_view_intersects() {
        let s = TimeScale::Session { buckets: &BUCKETS };
        assert_eq!(s.visible(View { lo: 0.0, hi: 6.0 }), (0, 6));
        assert_eq!(s.visible(View { lo: 1.5, hi: 3.5 }), (1, 4));
        assert_eq!(s.visible(View { lo: 2.0, hi: 3.0 }), (2, 3));
        let c = TimeScale::Continuous {
            buckets: &BUCKETS,
            step_us: DAY,
        };
        assert_eq!(
            c.visible(View {
                lo: 0.5 * DAY as f64,
                hi: 5.0 * DAY as f64
            }),
            (0, 5)
        );
        assert_eq!(
            c.visible(View {
                lo: 5.5 * DAY as f64,
                hi: 6.5 * DAY as f64
            }),
            (5, 5),
            "the gap holds no bucket"
        );
        let e = TimeScale::Session { buckets: &[] };
        assert_eq!(e.visible(View { lo: 0.0, hi: 1.0 }), (0, 0));
    }

    #[test]
    fn the_crosshair_picks_the_nearest_bucket() {
        let s = TimeScale::Session { buckets: &BUCKETS };
        let v = View::full(s.full());
        let x2 = s.x_of(2, v, PLOT);
        let x3 = s.x_of(3, v, PLOT);
        assert_eq!(Crosshair::at(x2 + 1.0, &s, v, PLOT), Some(2));
        assert_eq!(Crosshair::at(x3 - 1.0, &s, v, PLOT), Some(3));
        assert_eq!(Crosshair::at((x2 + x3) / 2.0 + 0.5, &s, v, PLOT), Some(3));
        assert_eq!(
            Crosshair::at(PLOT.x - 50.0, &s, v, PLOT),
            Some(0),
            "left of the plot snaps to the first"
        );
        assert_eq!(Crosshair::at(PLOT.right() + 50.0, &s, v, PLOT), Some(5));
        let zoomed = View { lo: 2.0, hi: 4.0 };
        assert_eq!(
            Crosshair::at(PLOT.x + 1.0, &s, zoomed, PLOT),
            Some(2),
            "only visible buckets"
        );
        assert_eq!(Crosshair::at(PLOT.right() - 1.0, &s, zoomed, PLOT), Some(3));
        assert_eq!(
            Crosshair::at(500.0, &TimeScale::Session { buckets: &[] }, v, PLOT),
            None
        );
    }

    fn us(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .unwrap()
            .timestamp_micros()
    }

    /// Weekdays only, 2026-01-05 (a Monday) onward, `days` of them.
    fn weekdays(days: usize) -> Vec<i64> {
        let mut out = Vec::new();
        let mut day = chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        while out.len() < days {
            if day.weekday().num_days_from_monday() < 5 {
                out.push(
                    day.and_hms_opt(0, 0, 0)
                        .unwrap()
                        .and_utc()
                        .timestamp_micros(),
                );
            }
            day = day.succ_opt().unwrap();
        }
        out
    }

    /// A wall-clock micros value's x on the plot, straight from the
    /// view — no scale, so a continuous tick is checked against the
    /// geometry rather than against `x_of`'s bucket centres.
    fn s_to_x(u: f64, v: View, plot: Rect) -> f32 {
        plot.x + ((u - v.lo) / v.span()) as f32 * plot.w
    }

    #[test]
    fn session_ticks_fall_where_the_month_changes() {
        let b = weekdays(250); // ~a year of sessions
        let s = TimeScale::Session { buckets: &b };
        let v = View::full(s.full());
        let plot = Rect::new(0.0, 0.0, 1200.0, 100.0);
        let mut out = Vec::new();
        let unit = ticks(&s, v, plot, 64.0, 0, &mut out);
        assert_eq!(unit, Some(Unit::Month));
        assert_eq!(
            out.len(),
            12,
            "{:?}",
            out.iter().map(|t| &t.label).collect::<Vec<_>>()
        );
        assert_eq!(out[0].label, "Jan 26");
        assert_eq!(out[1].label, "Feb 26");
        // the tick is the FIRST bucket of the month, not the last of the old one
        let first_feb = b.iter().position(|&t| t >= us(2026, 2, 1, 0, 0)).unwrap();
        assert!((out[1].x - s.x_of(first_feb, v, plot)).abs() < 1e-3);
    }

    #[test]
    fn ticks_respect_the_gap() {
        let b = weekdays(250);
        let s = TimeScale::Session { buckets: &b };
        let v = View::full(s.full());
        let mut out = Vec::new();
        // 300 px: a month tick every 25 px cannot fit at 64 — the chooser
        // falls through to the coarsest unit that still keeps three ticks
        // once thinned (Month, since Year has one candidate).
        let unit = ticks(&s, v, Rect::new(0.0, 0.0, 300.0, 100.0), 64.0, 0, &mut out);
        assert_eq!(unit, Some(Unit::Month));
        assert!(out.len() >= 3 && out.len() <= 5, "{}", out.len());
        for w in out.windows(2) {
            assert!(w[1].x - w[0].x >= 64.0 - 1e-3, "{:?}", (w[0].x, w[1].x));
        }
        // a wide plot fits days
        let unit = ticks(
            &s,
            View { lo: 0.0, hi: 10.0 },
            Rect::new(0.0, 0.0, 1200.0, 100.0),
            64.0,
            0,
            &mut out,
        );
        assert_eq!(unit, Some(Unit::Day));
        assert_eq!(out[0].label, "5 Jan");
        assert_eq!(out.len(), 10);
    }

    #[test]
    fn a_day_of_minute_bars_shows_thinned_hours() {
        let start = us(2026, 1, 5, 14, 30);
        let b: Vec<i64> = (0..600).map(|i| start + i * 60_000_000).collect();
        let s = TimeScale::Session { buckets: &b };
        let v = View::full(s.full());
        let mut out = Vec::new();
        let unit = ticks(&s, v, Rect::new(0.0, 0.0, 400.0, 100.0), 64.0, 0, &mut out);
        assert_eq!(unit, Some(Unit::Hour));
        assert!(out.len() >= 3, "{}", out.len());
        assert_eq!(
            out[0].label, "14:30",
            "the first bucket is always a candidate"
        );
        assert_eq!(out[1].label, "18:00", "every fourth hour at 400 px");
    }

    #[test]
    fn labels_read_at_the_given_offset() {
        let b = [us(2026, 1, 5, 23, 30), us(2026, 1, 6, 0, 30)];
        let s = TimeScale::Session { buckets: &b };
        let v = View::full(s.full());
        let mut out = Vec::new();
        ticks(&s, v, Rect::new(0.0, 0.0, 400.0, 100.0), 64.0, 0, &mut out);
        assert_eq!(
            out.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(),
            ["23:30", "00:30"]
        );
        // at UTC+1 both buckets sit in the same day: hours still, one hour later
        ticks(
            &s,
            v,
            Rect::new(0.0, 0.0, 400.0, 100.0),
            64.0,
            3600,
            &mut out,
        );
        assert_eq!(
            out.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(),
            ["00:30", "01:30"]
        );
    }

    #[test]
    fn continuous_ticks_sit_on_unit_boundaries_not_buckets() {
        let b = weekdays(10);
        let s = TimeScale::Continuous {
            buckets: &b,
            step_us: 86_400_000_000,
        };
        let v = View::full(s.full());
        let plot = Rect::new(0.0, 0.0, 1400.0, 100.0);
        let mut out = Vec::new();
        assert_eq!(ticks(&s, v, plot, 64.0, 0, &mut out), Some(Unit::Day));
        assert_eq!(
            out.len(),
            12,
            "every calendar day boundary in the span, weekend included"
        );
        let sat = us(2026, 1, 10, 0, 0) as f64;
        assert!(
            out.iter()
                .any(|t| (t.x - s_to_x(sat, v, plot)).abs() < 1e-3),
            "a weekend day has a tick"
        );
    }

    #[test]
    fn one_visible_bucket_gets_one_day_tick() {
        let b = [us(2026, 1, 5, 14, 30)];
        let s = TimeScale::Session { buckets: &b };
        let mut out = Vec::new();
        assert_eq!(
            ticks(
                &s,
                View::full(s.full()),
                Rect::new(0.0, 0.0, 400.0, 100.0),
                64.0,
                0,
                &mut out
            ),
            Some(Unit::Day)
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label, "5 Jan");
        assert_eq!(
            ticks(
                &TimeScale::Session { buckets: &[] },
                View { lo: 0.0, hi: 1.0 },
                Rect::new(0.0, 0.0, 400.0, 100.0),
                64.0,
                0,
                &mut out
            ),
            None
        );
        assert!(out.is_empty());
    }

    #[test]
    fn ticks_strictly_increase_in_x() {
        let b = weekdays(400);
        let s = TimeScale::Session { buckets: &b };
        let mut out = Vec::new();
        for (lo, hi, w) in [
            (0.0, 400.0, 900.0),
            (10.0, 30.0, 200.0),
            (100.0, 101.0, 50.0),
            (0.0, 400.0, 30.0),
        ] {
            ticks(
                &s,
                View { lo, hi },
                Rect::new(0.0, 0.0, w, 100.0),
                64.0,
                0,
                &mut out,
            );
            for p in out.windows(2) {
                assert!(p[1].x > p[0].x, "{lo}..{hi} at {w}: {:?}", (p[0].x, p[1].x));
            }
        }
    }
}
