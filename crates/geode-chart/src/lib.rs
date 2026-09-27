//! Reusable chart preparation and painting for the timeseries viewer.
//!
//! `core` is window-free geometry over slices; `model` is the immutable
//! input a caller prepares when data or presentation changes; `element`
//! paints it through gpui-component's `Plot` trait. Series identities, source
//! access and shell state belong to the caller; the rem scale is a parameter.

pub mod core;
pub mod element;
pub mod model;

pub use crate::core::MAX_DENSITY_QUADS;
pub use crate::core::axis::{Axis, AxisMode, Pane, Side};
pub use crate::core::hit::{Hit, divider_band, hit_test};
pub use crate::core::layout::Layout;
pub use crate::core::view::View;
pub use element::{ChartElement, chrome_rebuilds, density_quads, rebuilds};
pub use model::{ChartModel, ChartSlot};
