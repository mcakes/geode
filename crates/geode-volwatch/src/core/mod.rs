//! The pure parts: views and the board ask they imply, and the prepared
//! model render reads.
pub mod commands;
pub mod lens;
// replaced by the next task: model and session still name the old View.
#[cfg(any())]
pub mod model;
// replaced by the next task
#[cfg(any())]
pub mod session;
pub mod views;
