//! The kit tile modules are built from: a tile mechanism two modules would
//! otherwise each write lives here, interaction behavior (keys, focus, open
//! and close, precedence) as well as paint. The shell hosts tiles and never
//! depends on this crate.

pub mod notice;
pub mod popover;
