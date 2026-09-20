//! The app-wide clock as a gpui global (as-of dialog spec 2026-09-20
//! §6.1) — the workspace's THIRD global beside `linenumbers::UiSettings`
//! and `tips::Chords`, under the same rule: written by the shell alone
//! (startup in `ShellView::new`, a changed `[time]` in `apply_reload`),
//! read by modules with `cx.try_global::<AppClock>().map(|c|
//! c.0).unwrap_or_else(|| Clock::machine().0)` — `try_global`, same as
//! `UiSettings`/`Chords`: a module test fixture that never installed
//! `AppClock` would otherwise panic — and followed with
//! `cx.observe_global::<AppClock>`. The shell itself may read it with
//! the bare `cx.global::<AppClock>()` (it is the one that installs it).
//! A module has no path to `ShellView`, and `ConfigReloaded` fires only
//! for the docs a tile runs on, so neither existing route could carry a
//! zone change to a live tile.

use geode_core::clock::Clock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppClock(pub Clock);

impl gpui::Global for AppClock {}
