//! Chart preparation and painting: a kit and one element per chart type.
//!
//! `core` is window-free geometry over slices: scales, layout, the view
//! window, hit testing, decimation and palette values. `paint` is what the
//! elements share once they have a window: rebuild counters, axis chrome and
//! stroke builders. Each chart type is a module holding its immutable model
//! and the element that paints it through gpui-component's `Plot` trait;
//! `timeseries` is the time-axis line chart. Data identities, source access
//! and shell state belong to the caller; the rem scale is a parameter.

pub mod core;
pub mod paint;
pub mod timeseries;
pub mod xy;

pub use crate::core::MAX_DENSITY_QUADS;
pub use crate::core::axis::{Axis, AxisMode, Pane, Side};
pub use crate::core::hit::{Hit, divider_band, hit_test};
pub use crate::core::layout::Layout;
pub use crate::core::view::View;
pub use paint::{chrome_rebuilds, rebuilds};
