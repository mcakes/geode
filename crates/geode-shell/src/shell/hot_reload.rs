//! Config hot reload (Task 1c-1, spec section 4.5): the background
//! watcher's poll interval and `apply_reload`, which decides what a
//! freshly loaded `Config` changes at runtime (keymap, mod alias, theme,
//! an open palette's snapshot inputs, `restart_required`). Split out of
//! `shell/mod.rs` (Phase 3c Task 0) as the one seam that reacts to a
//! config change after startup.

use std::time::Duration;

use gpui::Context;

use crate::defaults::mod_alias_from_config;
use crate::fontsize::FontSize;
use crate::keymap::build_keymap;
use crate::reload;
use crate::vimfind::FindStyle;
use geode_core::config::Config;
use geode_core::dimensions::DerivedDimensions;
use geode_core::groupings::GroupingSlots;
use geode_core::schema::SchemaSpec;

use super::{ShellEvent, ShellView, docs_equal};

/// How often the background reload watcher polls the watched config
/// directories' `*.toml` mtimes (brief: "~500ms"). File scanning and
/// `Config::load` themselves run off the UI thread (`cx.background_executor
/// ().spawn`); only the cheap decision + entity mutation happens on the UI
/// thread, via the async entity handle (spec PHILOSOPHY.md: "nothing may
/// stall the render thread").
pub(super) const RELOAD_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Rebuild `GroupingSlots` from whatever `[groupings]` (plus the
/// `datasets`/`dimensions` docs it validates slots against) a `Config`
/// resolves to, printing any diagnostics the same way both call sites
/// did before this was factored out. A missing doc just means an empty
/// schema/dimension set — `(SchemaSpec, Vec<Diagnostic>)` and its
/// `DerivedDimensions` twin are both `Default`, so `unwrap_or_default` is
/// a real, valid "nothing configured yet" state, not a workaround.
/// Shared (Phase 3c Task 0, deferred 3b cleanup) between `ShellView::new`,
/// which seeds the frame's initial slots, and `apply_reload`, which
/// replaces them when `groupings`/`datasets`/`dimensions` changes.
pub(super) fn rebuild_slots(config: &Config) -> GroupingSlots {
    let (schema, _) = config
        .doc("datasets")
        .map(SchemaSpec::from_doc)
        .unwrap_or_default();
    let (dims, _) = config
        .doc("dimensions")
        .map(DerivedDimensions::from_doc)
        .unwrap_or_default();
    let (slots, diags) = config
        .doc("groupings")
        .map(|d| GroupingSlots::from_doc(d, &schema, &dims))
        .unwrap_or_default();
    for d in &diags {
        eprintln!("[groupings] {d}");
    }
    slots
}

impl ShellView {
    /// Apply (or reject) a freshly loaded `Config` (Task 1c-1): rebuild the
    /// keymap and mod alias from it, re-apply the theme only if `[theme]`
    /// actually changed (so a runtime `theme::toggle_mode` isn't silently
    /// clobbered by an unrelated reload — e.g. only `keymap.toml` edited),
    /// close an open palette only if ITS snapshot inputs actually changed
    /// (Review fix round 1, Finding 2 — refines the original brief's "must
    /// close on a successful reload": a theme-only reload no longer closes
    /// it, since the palette's items don't depend on `[theme]` at all; see
    /// `docs_equal` and the `palette_snapshot_changed` check below),
    /// and record the outcome for the status bar. Also reaches into the
    /// shared frame (§4.5): a changed `groupings`/`datasets`/`dimensions`
    /// doc replaces the frame's slots, a changed `views`/`dimensions` doc
    /// tells the frame a config reload happened and emits `ShellEvent::
    /// ConfigReloaded` for the app bridge to forward to the data thread,
    /// and a `sources`/`datasets` doc that disagrees with
    /// `sources_baseline`/`datasets_baseline` — the docs the data engine
    /// was actually built from, not merely the previous reload's config —
    /// sets `restart_required` and emits `ShellEvent::RestartRequired`;
    /// once the docs agree with that baseline again (M8, 3b final review:
    /// e.g. the offending edit is reverted) the message is cleared. That
    /// restart is about the data engine, not the frame: a `datasets`
    /// change also counts toward `groupings_changed` above, so slot
    /// labels — pure presentation, recomputed from whatever schema is on
    /// hand — are replaced immediately either way. What actually needs
    /// the restart is the data engine itself picking up new source paths
    /// or column definitions, which this reload never touches.
    ///
    /// Any error-severity diagnostic — from `Config::load` itself, or from
    /// building the keymap against the new config's docs — keeps the
    /// entire previous `Config` (and everything built from it) untouched
    /// (plan constraint: "Invalid config never panics: any error
    /// diagnostic ⇒ keep last-good entire Config"). Called by the
    /// background watcher above, and directly by tests: gpui's test
    /// executor never advances its simulated clock on `run_until_parked`
    /// (confirmed against the pinned rev's `TestScheduler::run`), so there
    /// is no practical way to drive the watcher's own timer loop through a
    /// `#[gpui::test]`; this is the real apply path either way; the
    /// watcher is just what schedules calling it.
    pub(super) fn apply_reload(&mut self, mut new_config: Config, cx: &mut Context<Self>) {
        let mod_alias = mod_alias_from_config(&new_config);
        let (keymap, keymap_diags) = build_keymap(
            new_config.layered_docs("keymap"),
            mod_alias,
            &self.services.registry,
        );
        new_config.diagnostics.extend(keymap_diags);

        let outcome = reload::decide(&new_config);
        if let reload::ReloadOutcome::Applied { warnings } = &outcome {
            // Fix wave, Fix 4: `decide` folds warning-severity diagnostics
            // (config + keymap-build) into `Applied { warnings }` rather
            // than discarding them, but nothing previously read that field
            // — a warning-only reload (e.g. an unknown-but-non-fatal
            // keymap key) applied silently with no trace anywhere. Surface
            // each on stderr, one line per warning, the same
            // `[source] warning: message` convention `main.rs`'s startup
            // diagnostics already use (these are plain `String`s by the
            // time they reach here — `decide` already extracted
            // `Diagnostic::message` — so there's no `Diagnostic` Display
            // impl to reuse here).
            for warning in warnings {
                eprintln!("[reload] warning: {warning}");
            }

            let theme_changed =
                self.services.config.get("app", "theme") != new_config.get("app", "theme");
            // Review fix round 1, Finding 2: only close the palette when
            // its own snapshot inputs (Task 6: bindings, built in
            // `toggle_palette` from the raw keymap docs + the resolved mod
            // alias) could actually have changed — a theme-only reload
            // (including the one our own `theme::persist_to_user_config`
            // write triggers, see that function's doc comment) must not
            // silently close an open palette out from under the user.
            let palette_snapshot_changed = self.services.mod_alias != mod_alias
                || !docs_equal(
                    self.services.config.layered_docs("keymap"),
                    new_config.layered_docs("keymap"),
                );

            // §4.5: which of the frame's inputs changed. Computed against
            // the still-current `self.services.config` before it's
            // overwritten below — `changed`'s last use is right here, so
            // the borrow ends before the move.
            let changed = |name: &str| {
                !docs_equal(
                    self.services.config.layered_docs(name),
                    new_config.layered_docs(name),
                )
            };
            let groupings_changed =
                changed("groupings") || changed("datasets") || changed("dimensions");
            let views_changed = changed("views") || changed("dimensions");
            // M8 (3b final review): compared against the docs the running
            // data engine was actually built from
            // (`sources_baseline`/`datasets_baseline`), not against the
            // previous reload's config — so reverting a `sources.toml`
            // edit back to that baseline clears `restart_required` below
            // instead of leaving a stale message up for the rest of the
            // session (comparing against the previous reload instead would
            // report "changed" on the revert too, since the value differs
            // from what was there a moment ago).
            let restart = [
                ("sources", &self.sources_baseline),
                ("datasets", &self.datasets_baseline),
            ]
            .into_iter()
            .filter(|(name, baseline)| !docs_equal(new_config.layered_docs(name), baseline))
            .map(|(name, _)| name)
            .collect::<Vec<_>>();

            self.services.config = new_config;
            self.services.mod_alias = mod_alias;
            self.services.keymap = keymap;
            // Cheap re-derive; `render` applies it only when it changed.
            self.font_size = FontSize::from_config(&self.services.config);
            self.find_style = FindStyle::from_config(&self.services.config);

            if theme_changed {
                self.services
                    .theme
                    .apply_from_config(&self.services.config, cx);
            }

            if palette_snapshot_changed {
                // Deliberately `self.palette = None` here, not `self.
                // close_palette(..)` (palette-input-polish task's own
                // helper, used everywhere else a close needs to hand focus
                // back to the shell root) — `apply_reload` has no `Window`
                // (it runs from the background reload watcher's plain
                // `Context<Self>` update, spec PHILOSOPHY.md: reload I/O
                // stays off the UI thread and this is the cheap synchronous
                // tail of that), so there is nothing to call `FocusHandle::
                // focus` with directly here. If the palette's `Entity<
                // InputState>` happened to hold real window focus at this
                // exact moment (a keymap/mod-alias edit landing while the
                // user is mid-query), silently dropping `self.palette`
                // would leave that `FocusId` orphaned — the dispatch tree
                // resolves an orphaned focus to its root node next frame,
                // not `ShellView`'s own `track_focus`'d div, so `handle_
                // key_down` (which lives on that div's `on_key_down`)
                // would simply stop firing: Escape, ctrl+k, hjkl, all of
                // it, dead until a mouse click claims focus somewhere else
                // first. Fix-round finding: `pending_focus_restore` below
                // is what closes that gap without needing a `Window` here.
                self.palette = None;
                self.pending_focus_restore = true;
            }

            if groupings_changed {
                let slots = rebuild_slots(&self.services.config);
                self.frame.update(cx, |f, cx| {
                    if f.replace_slots(slots) {
                        cx.notify();
                    }
                });
            }
            if views_changed {
                // I2 (final review): gpui flushes effects FIFO, so the
                // order these two calls *queue* their effects in is the
                // order they run in, regardless of subscriber
                // registration order. `frame.update`'s `cx.notify()`
                // queues the frame's own change notification — which is
                // what wakes every tile's `on_frame_changed` observer
                // and (if its followed versions moved) requeries — and
                // must not run before `ConfigReloaded` does: the app
                // bridge's handler for that event is what replaces a
                // factory's/handle's views (`ReplaceViews`), so a tile
                // that requeries first would query against the *old*
                // views while recording the *new* version, leaving
                // nothing to trigger the requery it actually needed.
                cx.emit(ShellEvent::ConfigReloaded);
                self.frame.update(cx, |f, cx| {
                    f.note_config_reloaded();
                    cx.notify();
                });
            }
            if !restart.is_empty() {
                let message = format!("{} changed — restart to apply", restart.join(" and "));
                self.restart_required = Some(message.clone());
                cx.emit(ShellEvent::RestartRequired(message));
            } else {
                // M8: both docs are back at the baseline the running data
                // engine was built from — the on-disk config no longer
                // disagrees with what's running, so the message is stale.
                self.restart_required = None;
            }
        }

        self.last_reload = outcome;
        cx.notify();
    }
}
