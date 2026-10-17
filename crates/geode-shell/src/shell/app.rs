//! The app-level owner of [`ShellServices`]: one entity every window's
//! [`ShellView`](super::ShellView) reads through its [`WindowKey`].
//!
//! A window never owns config, the keymap, the registry, the theme or the
//! workspaces; it reads them from here and writes them through
//! `ShellView::with_services` and its narrower siblings. One owner means a
//! second window sees the same config revision and the same tile
//! allocator as the first, with nothing to reconcile between them.

use std::path::PathBuf;

use gpui::Context;

use super::ShellServices;
use crate::tiling::WindowKey;
use geode_core::config::Config;

pub struct ShellApp {
    pub(crate) services: ShellServices,
    /// Config directories read by each watcher poll; `None` layers are
    /// skipped by the scan and every persist path is a silent no-op
    /// without a user directory.
    pub(crate) desk_dir: Option<PathBuf>,
    pub(crate) user_dir: Option<PathBuf>,
    /// The last key issued by [`Self::register_window`]; keys count up
    /// from 1 and are never reused.
    next_window: u64,
}

impl ShellApp {
    pub fn new(
        services: ShellServices,
        desk_dir: Option<PathBuf>,
        user_dir: Option<PathBuf>,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            services,
            desk_dir,
            user_dir,
            next_window: 0,
        }
    }

    /// Issue the next window key. Called once per window, before its
    /// `ShellView` is built.
    pub fn register_window(&mut self) -> WindowKey {
        self.next_window += 1;
        WindowKey(self.next_window)
    }

    pub fn services(&self) -> &ShellServices {
        &self.services
    }

    pub fn config(&self) -> &Config {
        &self.services.config
    }
}
