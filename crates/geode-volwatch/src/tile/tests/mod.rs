//! The tile's window tests: `harness` hosts it as the shell does, `data`
//! drives the request lifecycle, `keys` the grid, `grouping` the tree,
//! `gutter` the line-number gutter, `glyphs` the per-column cell glyphs
//! and `chrome` the header, view strip and footer.

mod chrome;
mod data;
mod glyphs;
mod grouping;
mod gutter;
mod harness;
mod keys;
