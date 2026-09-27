//! Build series requests from the tile model and frame as-of. The query range
//! covers the configured dates; the visible window bounds statistics. Failed
//! legacy expressions are excluded because they have no resolved operands.

use chrono::{DateTime, TimeZone, Utc};
use geode_chart::AxisMode;
use geode_core::query::{AsOf, QueryKey};
use geode_core::series::{SeriesParams, SeriesSpec};

use super::model::Model;

/// Convert the visible view to a half-open UTC span for statistics. Session
/// mode maps bucket indices to timestamps, ending one frequency step after
/// the last visible bucket. Continuous mode stores microseconds directly.
/// With no buckets, return `None`; [`params`] falls back to the query range.
pub fn window(model: &Model, buckets: &[i64]) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    if buckets.is_empty() {
        return None;
    }
    let step = model.frequency().seconds() * 1_000_000;
    let at = |us: i64| {
        Utc.timestamp_micros(us)
            .single()
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
    };
    let v = model.view();
    Some(match model.axis_mode() {
        AxisMode::Session => {
            let lo = (v.lo.floor().max(0.0) as usize).min(buckets.len() - 1);
            let hi = (v.hi.ceil().max(0.0) as usize).clamp(lo + 1, buckets.len());
            (at(buckets[lo]), at(buckets[hi - 1] + step))
        }
        AxisMode::Continuous => (at(v.lo as i64), at(v.hi as i64)),
    })
}

/// Build a request when the model has a dataset and at least one slot.
/// Carry all nonlegacy slots, including hidden slots and expression operands.
/// Clamp the statistics window to the query range, allowing an empty window.
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
    let window = match window(model, buckets) {
        Some((a, b)) => {
            let a = a.max(range.0);
            (a, b.min(range.1).max(a))
        }
        None => range,
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
        // A legacy slot's kind is a placeholder with no operands.
        series: model
            .slots()
            .iter()
            .filter(|s| !s.legacy)
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
        let e = crate::core::resolve("SPX.close / VIX", m.slots(), Some("demo_kdb")).unwrap();
        m.add_expr("SPX.close / VIX", e).unwrap();
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

    /// A saved expression that could not be rewritten holds no operands;
    /// sending it would have the compiler refuse the whole request.
    #[test]
    fn a_legacy_expression_is_never_sent() {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        m.add_legacy_expr("s1 / s9", "gone".into()).unwrap();
        let p = params(&m, QueryKey(7), 3, now(), &AsOf::Live, &[]).unwrap();
        assert_eq!(p.series.iter().map(|s| s.slot).collect::<Vec<_>>(), vec![1]);
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
        let (from, to) = window(&m, &buckets).unwrap();
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
        let (from, to) = window(&m, &buckets).unwrap();
        assert_eq!(from.timestamp_micros(), m.view().lo as i64);
        assert_eq!(to.timestamp_micros(), m.view().hi as i64);
        // A full view is the whole range.
        m.reset_view();
        let p = params(&m, QueryKey(1), 1, now(), &AsOf::Live, &buckets).unwrap();
        assert_eq!(p.window.0.timestamp_micros(), buckets[0]);
    }

    #[test]
    fn window_answers_none_on_no_buckets_and_a_span_on_one() {
        let mut m = Model::new();
        m.add_source("A", "demo_kdb", "series").unwrap();
        assert_eq!(window(&m, &[]), None, "Session, no buckets");
        m.set_axis_mode(AxisMode::Continuous);
        assert_eq!(window(&m, &[]), None, "Continuous, no buckets");
        m.set_axis_mode(AxisMode::Session);
        let one = [us("2026-01-05T00:00:00Z")];
        m.set_full((0.0, 1.0));
        let (from, to) = window(&m, &one).unwrap();
        assert_eq!(from, Utc.timestamp_micros(one[0]).unwrap());
        assert_eq!(
            to,
            Utc.timestamp_micros(one[0]).unwrap() + chrono::Duration::days(1),
            "a one-bucket slice is [b0, b0 + step)"
        );
    }
}
