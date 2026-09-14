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
//! [`commands`] is the second pure half: the `:` vocabulary.
//!
//! [`tile`] and [`content`] are the gpui half: the entity that requests
//! its document through `DataHandle` and paints it, and the
//! `TileContent`/`ModuleFactory` pair the shell hosts it through. Cell
//! editing (insert mode, `:bump`, `:revert`) is Task 7 and the draft
//! states (`Behind`, `:rebase`, `:discard`) are Task 8; the vocabulary and
//! the plumbing for both are here, answering which task lands them.

pub mod commands;
pub mod content;
pub mod core;
pub mod tile;

pub use content::{ACTIONS, DEFAULT_KEYMAP, MarketDataFactory};
pub use tile::MarketDataTile;
