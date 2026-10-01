//! The xy chart: lines and point marks against a linear x axis, in up to
//! two panes with four y axes. [`XyModel`] is the immutable input a caller
//! prepares; [`XyElement`] paints it.

pub mod element;
pub mod model;

pub use element::XyElement;
pub use model::{SlotKind, Style, XAxis, XyModel, XySlot, YFormat};
