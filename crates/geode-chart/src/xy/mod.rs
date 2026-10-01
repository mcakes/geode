//! The xy chart: lines and point marks against a linear x axis, in up to
//! two panes with four y axes. [`XyModel`] is the immutable input a caller
//! prepares; [`XyElement`] paints it.
//!
//! Everything a caller names to build a model and host the element is here
//! or at the crate root (`Axis`, `View`, the layout and hit test): the x
//! axis's format and its scale are re-exported from the kit.

pub mod element;
pub mod model;

pub use crate::core::linear::{LinearX, XFormat};
pub use element::XyElement;
pub use model::{SlotKind, Style, XAxis, XyModel, XySlot, YFormat};
