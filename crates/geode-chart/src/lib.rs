//! `geode-chart`: the chart crate behind the timeseries viewer (spec §8).
//!
//! `core` is window-free geometry over slices; `model` is the immutable
//! input a tile builds per delivery; `element` paints it through
//! gpui-component's `Plot` trait. Nothing here knows a series, a source
//! or the shell — the rem scale is a parameter.

// `element` and `model` are created in Task 7; the `pub use` re-exports of
// `Axis`/`AxisMode`/`Pane`/`Side`/`View`/`ChartElement`/`ChartModel`/
// `ChartSlot` land there too. Until then this crate is `core` alone.
pub mod core;
