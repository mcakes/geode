//! Shared application controls with pure state and GPUI painters. Hosts retain
//! widget state, route input, and supply presentation values. The crate depends
//! on neither the shell nor feature crates, allowing them to share controls.
//!
//! The date field uses `SharedString` for prepared display text. Hosts can cache
//! that text between edits and pass it to the painter without reformatting.

pub mod datefield;
