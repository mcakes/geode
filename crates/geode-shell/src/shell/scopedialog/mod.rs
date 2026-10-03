//! The Scope dialog's pure model: the lane's scope as Current-screen rows,
//! the Saved screen's rows, and the layer stack that decides whether a
//! commit returns to a screen or closes the dialog. No GPUI types here; the
//! view paints these and routes keys to frame edits.

pub(crate) mod rows;
pub(crate) mod saved;
pub(crate) mod state;
