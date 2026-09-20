//! The sheet's pure core (line-pricer spec §6): the row model, the one
//! edit door with its inverses, the shorthand grammar both ways, the
//! package templates, the column vocabulary, the views doc and the
//! storage row shape. No gpui type appears here.

pub mod shorthand;
pub mod template;

pub use shorthand::{
    LineSpec, OwnShifts, ParseError, RowSpec, parse, render_expiry, render_line, render_package,
    render_strike,
};
pub use template::{LegSpec, Template};
