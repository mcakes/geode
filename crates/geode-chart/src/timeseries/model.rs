//! Prepared chart data and presentation shared with the painting element.
//!
//! A [`ChartModel`] contains display buckets, aligned values for each
//! [`ChartSlot`], and statistics supplied by the caller. The chart derives
//! coordinates and scales; it does not calculate percentiles or density bins.
//! Callers must change `version` whenever model contents change so cached
//! scales, labels and paths cannot survive a change to their inputs.

use std::sync::Arc;

use gpui::{Hsla, SharedString};

use crate::core::axis::{Axis, AxisMode, Pane, Side};
use crate::core::layout::LayoutOptions;
use crate::core::time::TimeScale;

/// One series slot as the chart paints it.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartSlot {
    pub number: u8,
    pub label: SharedString,
    /// `buckets.len()` long; `NaN` where the slot has no bucket.
    pub values: Vec<f64>,
    pub colour: Hsla,
    pub axis: Axis,
    pub visible: bool,
    /// `(fraction, value)`; empty when off.
    pub percentiles: Vec<(f64, f64)>,
    /// Pre-formatted `p5`/`p50`/`p95` tags, parallel to `percentiles`.
    pub percentile_labels: Vec<SharedString>,
    /// `(lo, hi, count)` ascending; empty when off.
    pub bins: Vec<(f64, f64, u32)>,
}

/// Everything one frame of the chart paints from.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartModel {
    /// Bumped by the builder on every change; the path cache key.
    pub version: u64,
    /// Epoch micros, ascending.
    pub buckets: Vec<i64>,
    /// One display bucket's width in micros (the frequency).
    pub step_us: i64,
    pub axis_mode: AxisMode,
    /// Seconds east of UTC for every displayed time.
    pub offset_secs: i32,
    pub split: f32,
    pub density: bool,
    pub slots: Vec<ChartSlot>,
}

impl ChartModel {
    pub fn empty() -> Arc<Self> {
        Arc::new(Self {
            version: 0,
            buckets: Vec::new(),
            step_us: 1,
            axis_mode: AxisMode::Session,
            offset_secs: 0,
            split: 0.7,
            density: false,
            slots: Vec::new(),
        })
    }

    pub fn time_scale(&self) -> TimeScale<'_> {
        match self.axis_mode {
            AxisMode::Session => TimeScale::Session {
                buckets: &self.buckets,
            },
            AxisMode::Continuous => TimeScale::Continuous {
                buckets: &self.buckets,
                step_us: self.step_us,
            },
        }
    }

    /// The loaded x range, in the current mode's own units.
    pub fn full(&self) -> (f64, f64) {
        self.time_scale().full()
    }

    fn uses(&self, pane: Pane, side: Side) -> bool {
        self.slots
            .iter()
            .any(|s| s.visible && s.axis.pane() == pane && s.axis.side() == side)
    }

    /// What the layout reserves: an axis column per side a VISIBLE slot
    /// uses, and the density strip only while a visible slot has bins.
    pub fn layout_options(&self, rem_px: f32) -> LayoutOptions {
        LayoutOptions {
            upper_left: self.uses(Pane::Upper, Side::Left),
            upper_right: self.uses(Pane::Upper, Side::Right),
            lower_left: self.uses(Pane::Lower, Side::Left),
            lower_right: self.uses(Pane::Lower, Side::Right),
            density: self.density && self.slots.iter().any(|s| s.visible && !s.bins.is_empty()),
            split: self.split,
            rem_px,
        }
    }

    /// The tag a percentile line wears: `p5`, `p50`, `p99.5`.
    pub fn percentile_label(fraction: f64) -> SharedString {
        let pct = fraction * 100.0;
        if (pct - pct.round()).abs() < 1e-9 {
            format!("p{}", pct.round() as i64).into()
        } else {
            format!("p{pct}").into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_options_follow_visible_slots_only() {
        let mut m = (*ChartModel::empty()).clone();
        let slot = |axis, visible| ChartSlot {
            number: 1,
            label: "s".into(),
            values: vec![],
            colour: gpui::black(),
            axis,
            visible,
            percentiles: vec![],
            percentile_labels: vec![],
            bins: vec![(0.0, 1.0, 1)],
        };
        m.slots = vec![slot(Axis::Left, true), slot(Axis::BottomRight, false)];
        m.density = true;
        let o = m.layout_options(12.0);
        assert!(o.upper_left && !o.upper_right && !o.lower_left && !o.lower_right);
        assert!(o.density);
        m.slots[1].visible = true;
        assert!(m.layout_options(12.0).lower_right);
        m.slots.iter_mut().for_each(|s| s.bins.clear());
        assert!(
            !m.layout_options(12.0).density,
            "density with no bins reserves nothing"
        );
        assert_eq!(ChartModel::percentile_label(0.05), SharedString::from("p5"));
        assert_eq!(
            ChartModel::percentile_label(0.995),
            SharedString::from("p99.5")
        );
    }
}
