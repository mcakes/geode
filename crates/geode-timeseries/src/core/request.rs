//! From the model to a `SeriesParams` (spec §6.1, §9.10).

use chrono::{DateTime, TimeZone, Utc};
use geode_chart::AxisMode;
use geode_core::query::{AsOf, QueryKey};
use geode_core::series::{SeriesParams, SeriesSpec};

use super::model::Model;

/// The visible span (ruling 10: stats are over the VISIBLE window). With
/// no buckets yet the window is the range. Under `Session` the view is
/// an index window; the span runs from the first visible bucket to the
/// last visible bucket plus one frequency step (half-open). Under
/// `Continuous` the view IS micros.
pub fn window(model: &Model, buckets: &[i64]) -> (DateTime<Utc>, DateTime<Utc>) {
    let step = model.frequency().seconds() * 1_000_000;
    let at = |us: i64| {
        Utc.timestamp_micros(us)
            .single()
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
    };
    let v = model.view();
    match model.axis_mode() {
        AxisMode::Session => {
            let lo = (v.lo.floor().max(0.0) as usize).min(buckets.len().saturating_sub(1));
            let hi = (v.hi.ceil().max(0.0) as usize).clamp(lo + 1, buckets.len().max(1));
            (at(buckets[lo]), at(buckets[hi - 1] + step))
        }
        AxisMode::Continuous => (at(v.lo as i64), at(v.hi as i64)),
    }
}

pub fn params(
    model: &Model,
    key: QueryKey,
    tag: u64,
    now: DateTime<Utc>,
    as_of: &AsOf,
    buckets: &[i64],
) -> Option<SeriesParams> {
    let dataset = model.dataset()?.to_string();
    if model.slots().is_empty() {
        return None;
    }
    let range = model.range().resolve(now, as_of);
    let window = if buckets.is_empty() {
        range
    } else {
        let (a, b) = window(model, buckets);
        (a.max(range.0), b.min(range.1).max(a))
    };
    Some(SeriesParams {
        key,
        tag,
        submitted: std::time::Instant::now(),
        dataset,
        range,
        window,
        as_of: as_of.clone(),
        frequency: model.frequency(),
        series: model
            .slots()
            .iter()
            .map(|s| SeriesSpec {
                slot: s.number,
                kind: s.kind.clone(),
            })
            .collect(),
        percentiles: model.percentiles().to_vec(),
        bins: model.density(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Model, Preset, Range};
    use chrono::{TimeZone, Utc};
    use geode_chart::AxisMode;
    use geode_core::query::{AsOf, QueryKey};
    use geode_core::series::{Frequency, SlotKind};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 19, 15, 0, 0).unwrap()
    }
    fn us(s: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .timestamp_micros()
    }

    #[test]
    fn params_carry_every_slot_in_order_and_the_settings() {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        m.add_source("VIX", "demo_kdb", "series").unwrap();
        let e = crate::core::resolve("s1 / s2", m.slots(), Some("demo_kdb"), None).unwrap();
        m.add_expr("s1 / s2", e).unwrap();
        let p = params(&m, QueryKey(7), 3, now(), &AsOf::Live, &[]).unwrap();
        assert_eq!(p.key, QueryKey(7));
        assert_eq!(p.tag, 3);
        assert_eq!(p.dataset, "series");
        assert_eq!(
            p.range,
            Range::Relative(Preset::Y1).resolve(now(), &AsOf::Live)
        );
        assert_eq!(p.window, p.range, "no buckets yet: the window is the range");
        assert_eq!(p.frequency, Frequency::D1);
        assert_eq!(
            p.series.iter().map(|s| s.slot).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(matches!(p.series[2].kind, SlotKind::Expr(_)));
        assert_eq!(p.percentiles, vec![0.05, 0.5, 0.95]);
        assert_eq!(p.bins, Some(40));
        assert!(p.as_of.is_live());
        assert!(
            params(&Model::new(), QueryKey(7), 1, now(), &AsOf::Live, &[]).is_none(),
            "nothing to ask"
        );
    }

    #[test]
    fn the_window_is_the_visible_span_in_both_axis_modes() {
        let mut m = Model::new();
        m.add_source("A", "demo_kdb", "series").unwrap();
        let buckets: Vec<i64> = (0..10)
            .map(|i| us("2026-01-05T00:00:00Z") + i * 86_400_000_000)
            .collect();
        m.set_full((0.0, 10.0));
        m.zoom_in(1); // 10 → 8 wide, centred: [1, 9)
        let (from, to) = window(&m, &buckets);
        assert_eq!(from, Utc.timestamp_micros(buckets[1]).unwrap());
        assert_eq!(
            to,
            Utc.timestamp_micros(buckets[8]).unwrap() + chrono::Duration::days(1),
            "the last visible bucket plus one frequency step, half-open"
        );
        m.set_axis_mode(AxisMode::Continuous);
        m.set_full((buckets[0] as f64, buckets[9] as f64 + 86_400_000_000.0));
        m.reset_view();
        m.zoom_in(1);
        let (from, to) = window(&m, &buckets);
        assert_eq!(from.timestamp_micros(), m.view().lo as i64);
        assert_eq!(to.timestamp_micros(), m.view().hi as i64);
        // A full view is the whole range.
        m.reset_view();
        let p = params(&m, QueryKey(1), 1, now(), &AsOf::Live, &buckets).unwrap();
        assert_eq!(p.window.0.timestamp_micros(), buckets[0]);
    }
}
