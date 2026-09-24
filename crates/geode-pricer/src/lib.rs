//! The line pricer module (line-pricer spec §8): a tile whose rows are
//! option lines and packages, priced through the data tier's pricing
//! request. [`core`] is the pure half — it names no element, entity,
//! window, data service or pricing implementation; the rest is the tile.

pub mod core;

pub mod grid;
pub mod paint;
pub mod store;
