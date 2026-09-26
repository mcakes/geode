//! `SeriesResult` + `Model` → `ChartModel` (spec §8.3, §8.5). Built once
//! per delivery or chrome change, never in `render`. Every field the
//! chart's cache keys do NOT carry (`axis_mode`, `step_us`) rides on
//! `version`, which the tile bumps on every rebuild.

use geode_chart::{ChartModel, ChartSlot};
use geode_core::series::SeriesResult;
use gpui::Hsla;

use super::model::{Color, Model};

pub fn build(
    result: &SeriesResult,
    model: &Model,
    version: u64,
    offset_secs: i32,
    colour_of: &dyn Fn(&Color) -> Hsla,
    default_source: Option<&str>,
) -> ChartModel {
    let n = result.buckets.len();
    let slots = model
        .slots()
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let r = result.slots.iter().find(|r| r.slot == s.number);
            let (values, percentiles, bins) = match r {
                Some(r) => (r.values.clone(), r.percentiles.clone(), r.bins.clone()),
                None => (vec![f64::NAN; n], Vec::new(), Vec::new()),
            };
            ChartSlot {
                number: s.number,
                label: model.label(i, default_source).into(),
                values,
                colour: colour_of(&s.color),
                axis: s.axis,
                visible: s.visible,
                percentile_labels: percentiles
                    .iter()
                    .map(|(f, _)| ChartModel::percentile_label(*f))
                    .collect(),
                percentiles,
                bins,
            }
        })
        .collect();
    ChartModel {
        version,
        buckets: result.buckets.clone(),
        step_us: model.frequency().seconds() * 1_000_000,
        axis_mode: model.axis_mode(),
        offset_secs,
        split: model.split(),
        density: model.density().is_some(),
        slots,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Color, Model};
    use geode_chart::Axis;
    use geode_core::series::{SeriesResult, SlotProvenance, SlotResult};
    use gpui::Hsla;

    fn result() -> SeriesResult {
        let prov = || SlotProvenance {
            loaded: None,
            latest_received_at: None,
            health: None,
        };
        SeriesResult {
            buckets: vec![1_000_000, 2_000_000, 3_000_000],
            slots: vec![
                SlotResult {
                    slot: 1,
                    values: vec![1.0, f64::NAN, 3.0],
                    percentiles: vec![(0.05, 1.1), (0.5, 2.0)],
                    bins: vec![(1.0, 2.0, 3)],
                    provenance: prov(),
                },
                SlotResult {
                    slot: 2,
                    values: vec![10.0, 20.0, 30.0],
                    percentiles: vec![],
                    bins: vec![],
                    provenance: prov(),
                },
            ],
        }
    }

    #[test]
    fn the_chart_model_mirrors_the_result_and_the_models_look() {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        m.add_source("VIX", "demo_kdb", "series").unwrap();
        m.cycle_axis(true, 1); // s2 → Right
        m.toggle_visible(); // s2 hidden
        let colour_of = |c: &Color| match c {
            Color::Palette(i) => Hsla {
                h: *i as f32 / 10.0,
                s: 1.0,
                l: 0.5,
                a: 1.0,
            },
            Color::Named(_) => gpui::black(),
            Color::Custom(c) => c.to_hsla(),
        };
        let cm = build(&result(), &m, 9, 3600, &colour_of, Some("demo_kdb"));
        assert_eq!(cm.version, 9);
        assert_eq!(cm.buckets, vec![1_000_000, 2_000_000, 3_000_000]);
        assert_eq!(cm.step_us, 86_400_000_000, "1d");
        assert_eq!(cm.offset_secs, 3600);
        assert_eq!(cm.split, 0.7);
        assert!(cm.density);
        assert_eq!(cm.slots.len(), 2);
        assert_eq!(cm.slots[0].number, 1);
        assert_eq!(cm.slots[0].label.as_ref(), "SPX.close");
        assert!(cm.slots[0].values[1].is_nan(), "a gap stays a gap");
        assert_eq!(cm.slots[0].percentiles, vec![(0.05, 1.1), (0.5, 2.0)]);
        assert_eq!(
            cm.slots[0]
                .percentile_labels
                .iter()
                .map(|l| l.as_ref())
                .collect::<Vec<_>>(),
            vec!["p5", "p50"]
        );
        assert_eq!(cm.slots[0].bins, vec![(1.0, 2.0, 3)]);
        assert_eq!(cm.slots[0].colour.h, 0.0);
        assert_eq!(cm.slots[1].axis, Axis::Right);
        assert!(!cm.slots[1].visible);
        assert_eq!(cm.slots[1].colour.h, 0.1);
    }

    #[test]
    fn a_slot_the_result_lacks_paints_no_points_and_a_result_slot_the_model_lacks_is_skipped() {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap(); // s1
        m.add_source("VIX", "demo_kdb", "series").unwrap(); // s2
        m.remove(2).unwrap();
        m.add_source("V2X", "demo_kdb", "series").unwrap(); // s3, not in the (older) result
        let cm = build(&result(), &m, 1, 0, &|_| gpui::black(), None);
        assert_eq!(cm.slots.len(), 2);
        assert_eq!(cm.slots[1].number, 3);
        assert!(
            cm.slots[1].values.iter().all(|v| v.is_nan()),
            "buckets.len() NaNs so the element's lengths agree"
        );
        assert_eq!(cm.slots[1].values.len(), 3);
    }
}
