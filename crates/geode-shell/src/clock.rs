//! The app-wide clock as a gpui global (as-of dialog spec 2026-09-20
//! §6.1) — the workspace's THIRD global beside `linenumbers::UiSettings`
//! and `tips::Chords`, under the same rule: written by the shell alone
//! (startup in `ShellView::new`, a changed `[time]` in `apply_reload`),
//! read by modules with `cx.global::<AppClock>()` and followed with
//! `cx.observe_global::<AppClock>`. A module has no path to `ShellView`,
//! and `ConfigReloaded` fires only for the docs a tile runs on, so
//! neither existing route could carry a zone change to a live tile.

use geode_core::clock::Clock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppClock(pub Clock);

impl gpui::Global for AppClock {}
