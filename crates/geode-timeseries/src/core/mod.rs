pub mod model;
pub mod range;
pub mod resolve;

pub use model::{Changed, Colour, Model, Removal, Slot, SlotState};
pub use range::{Preset, Range};
pub use resolve::resolve;
