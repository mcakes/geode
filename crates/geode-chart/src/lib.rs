//! `geode-chart`: the chart crate behind the timeseries viewer (spec §8).
//!
//! `core` is window-free geometry over slices; `model` is the immutable
//! input a tile builds per delivery; `element` paints it through
//! gpui-component's `Plot` trait. Nothing here knows a series, a source
//! or the shell — the rem scale is a parameter.

pub mod core;
pub mod element;
pub mod model;

pub use crate::core::axis::{Axis, AxisMode, Pane, Side};
pub use crate::core::view::View;
pub use element::{ChartElement, rebuilds};
pub use model::{ChartModel, ChartSlot};
