//! The Scope dialog's pure model: the lane's scope as Current rows, the
//! saved rows, one cursor over both (`screen`), and the layer stack that
//! decides whether a commit returns to the screen or closes the dialog.
//! Those hold no GPUI types; `view` paints the screen and routes its keys;
//! steps that exist as their own modals are pushed over it.

pub(crate) mod definition;
pub(crate) mod prompt;
pub(crate) mod rows;
pub(crate) mod saved;
pub(crate) mod saved_view;
pub(crate) mod screen;
pub(crate) mod state;
pub(crate) mod view;
