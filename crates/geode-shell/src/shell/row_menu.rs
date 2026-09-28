//! The row menu: the shell's menu over a row's dimension context
//! ([`crate::dimension::menu_rows`]).

use gpui::{Context, Window};

use super::ShellView;

/// What a [`crate::dimension::DimensionAction`] runs against: the shell,
/// its window and its context, once the row menu has closed.
#[expect(dead_code, reason = "read once the row menu runs its actions")]
pub struct ActionCx<'a, 'b> {
    pub(crate) shell: &'a mut ShellView,
    pub(crate) window: &'a mut Window,
    pub(crate) cx: &'a mut Context<'b, ShellView>,
}
