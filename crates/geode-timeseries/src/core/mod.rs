pub mod chart;
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
pub use rgb::{Rgb8, colour_from_pick, within_a_step};
