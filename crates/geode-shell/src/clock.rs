//! The app-wide clock, published as a GPUI global by the shell at startup
//! and whenever the `[time]` configuration changes.
//!
//! Modules observe [`AppClock`] with `cx.observe_global::<AppClock>()` and
//! read it with `cx.try_global::<AppClock>()`, falling back to
//! `Clock::machine().0` when a test fixture has not installed the global.
//! The shell installs it before use and can read it through `cx.global`.
//! This carries time-zone changes independently of the document-specific
//! `ConfigReloaded` events delivered to tiles.

use geode_core::clock::Clock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppClock(pub Clock);

impl gpui::Global for AppClock {}
