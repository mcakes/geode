//! Models and transformations for timeseries state, queries, menus, and
//! persistence. The tile owns I/O, focus, and retained entities; these modules
//! prepare data and validate operations without accessing a window.

pub mod chart;
pub mod complete;
pub mod menu;
pub mod model;
pub mod range;
pub mod request;
pub mod resolve;
pub mod rgb;
pub mod session;

pub use model::{Changed, Color, Model, Removal, Slot, SlotState};
pub use range::{Preset, Range};
pub use resolve::resolve;
pub use rgb::{Rgb8, color_from_pick, within_a_step};
