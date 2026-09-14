//! The market-data panel module (market-data spec §8): one tile per
//! `PanelSpec`, painting one document of a document dataset as a grid —
//! pivoted on two axes, or a row per document row with the value columns
//! laid flat — with a draft of unsent edits over the top.
//!
//! `core` is the pure half (spec §8.2/§8.4): the panel spec, the matrix
//! model a frame paints from, the draft, and the cell parser. It names no
//! element, entity or window, so its tests run without one — the sole
//! `gpui` type it borrows is `SharedString`, a refcounted string, so that
//! a prepared cell hands a frame its text without allocating.
//!
//! The tile, the factory and the `:` vocabulary land in Task 6.

pub mod core;
