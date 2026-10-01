//! The time-axis line chart: an immutable [`ChartModel`] of display buckets
//! and aligned slot values, painted by [`ChartElement`] with percentile rules
//! and density bars. The caller supplies the statistics; the chart derives
//! coordinates and scales.

pub mod element;
pub mod model;

pub use element::{ChartElement, density_quads};
pub use model::{ChartModel, ChartSlot};
