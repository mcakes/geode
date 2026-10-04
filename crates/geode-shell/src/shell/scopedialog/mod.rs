//! The Scope dialog's pure model: the lane's scope as Current-screen rows,
//! the Saved screen's rows, and the layer stack that decides whether a
//! commit returns to a screen or closes the dialog. Those three hold no GPUI
//! types; `view` paints Current and routes its keys; steps that exist as
//! their own modals are pushed over it.

pub(crate) mod definition;
pub(crate) mod prompt;
pub(crate) mod rows;
pub(crate) mod saved;
pub(crate) mod saved_view;
pub(crate) mod state;
pub(crate) mod view;
