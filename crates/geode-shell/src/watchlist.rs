//! Resolved watchlists shared with hosted modules.
//!
//! The shell installs an empty [`WatchlistGlobal`] at startup; the app's
//! bridge replaces it when a list's definition, members or state changes,
//! and only then, so `cx.observe_global::<WatchlistGlobal>()` wakes a module
//! for a real change. Lists are always resolved live, never at the frame's
//! as-of. A failed resolution keeps the last members and says so in the
//! list's status.

use std::sync::Arc;

use geode_core::watchlist::state::WatchlistSnapshot;

/// Every defined watchlist with its members. Written by the bridge only.
#[derive(Debug, Clone, Default)]
pub struct WatchlistGlobal(pub Arc<WatchlistSnapshot>);

impl gpui::Global for WatchlistGlobal {}
