//! Config hot reload (Task 1c-1, spec section 4.5): the background
//! watcher's poll interval and `apply_reload`, which decides what a
//! freshly loaded `Config` changes at runtime (keymap, mod alias, theme,
//! an open palette's snapshot inputs, `restart_required`). Split out of
//! `shell/mod.rs` (Phase 3c Task 0) as the one seam that reacts to a
//! config change after startup.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gpui::Context;

use crate::defaults::mod_alias_from_config;
use crate::fontsize::FontSize;
use crate::keymap::build_keymap;
use crate::keymap::fragments;
use crate::reload;
use crate::vimfind::FindStyle;
use geode_core::config::{Config, Diagnostic, Severity};
use geode_core::dimensions::DerivedDimensions;
use geode_core::groupings::GroupingSlots;
use geode_core::log::LogLevels;
use geode_core::schema::SchemaSpec;

use super::{ShellEvent, ShellView, docs_equal, pickable_columns};

/// How often the background reload watcher polls the watched config
/// directories' `*.toml` mtimes (brief: "~500ms"). File scanning and
/// `Config::load` themselves run off the UI thread (`cx.background_executor
/// ().spawn`); only the cheap decision + entity mutation happens on the UI
/// thread, via the async entity handle (spec PHILOSOPHY.md: "nothing may
/// stall the render thread").
///
/// **Coupled to `perf::IDLE_CUTOFF` (also 500ms) — see that constant's own
/// doc comment for why.** This is also the tick `Diagnostics::
/// refresh_frame_hist` rides while a diagnostics tile is visible; keep the
/// two at least this close, or add a floor at the `refresh_frame_hist`
/// call site instead of relying on the coincidence.
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
        tracing::warn!(target: "geode::config", "{d}");
    }
    slots
}

// Test-only counter (Phase 4b Task 1 fix round 1, MIN-8): incremented
// once per `rebuild_saved_scopes` call that actually prints its
// diagnostics (`report_diagnostics: true`), so a test can pin "the two
// startup callers together print at most once" without capturing
// `stderr` — see `shell/tests/reload.rs`'s
// `rebuild_saved_scopes_prints_only_when_asked`.
#[cfg(test)]
thread_local! {
    pub(crate) static SAVED_SCOPES_REPORT_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Rebuild [`SavedScopes`](geode_core::scopes::SavedScopes) from whatever
/// `[scopes]` (plus the `datasets`/`dimensions` docs a scope validates
/// against) a `Config` resolves to — same shape as [`rebuild_slots`], and
/// shared the same way between `ShellView::new` (seeds the frame's
/// initial saved scopes) and `apply_reload` (replaces them when `scopes`/
/// `datasets`/`dimensions` changes, spec §4.5-style live pickup).
/// `pub`, not `pub(super)` (Phase 4b M15): `shell::mod` re-exports this
/// as `shell::saved_scopes` (`pub use`) so `main.rs`, across the crate
/// boundary, can call it without a second copy of its load logic — a
/// `pub use` cannot re-export an item less visible than the path it is
/// re-exported through, and `saved_scopes` is reached from outside this
/// crate. `hot_reload` the *module* stays private either way (`mod
/// hot_reload;`, no `pub`), so this doesn't otherwise widen what's
/// reachable — only the one re-exported name is.
///
/// `report_diagnostics` (Phase 4b Task 1 fix round 1, MIN-8): M15's
/// re-export gave `main.rs` a second startup caller of this function
/// (`register_scope_actions(&mut registry, &saved_scopes(&config))`)
/// alongside `ShellView::new`'s own — before M15, `main.rs`'s copy
/// printed nothing at all (the bug M15 fixed), but printing from both
/// unconditionally means a single malformed `scopes.toml` entry prints
/// twice at every launch, reading as two distinct problems. `ShellView::
/// new` passes `true` (the frame's own load is the one that reports);
/// `main.rs` passes `false`; `apply_reload`'s live-reload call also
/// passes `true` — a config change actually happening is exactly when a
/// fresh diagnostic should surface.
pub fn rebuild_saved_scopes(
    config: &Config,
    report_diagnostics: bool,
) -> geode_core::scopes::SavedScopes {
    let (schema, _) = config
        .doc("datasets")
        .map(SchemaSpec::from_doc)
        .unwrap_or_default();
    let (dims, _) = config
        .doc("dimensions")
        .map(DerivedDimensions::from_doc)
        .unwrap_or_default();
    let (saved, diags) = config
        .doc("scopes")
        .map(|d| geode_core::scopes::saved_scopes_from_doc(d, &schema, &dims))
        .unwrap_or_default();
    if report_diagnostics {
        #[cfg(test)]
        SAVED_SCOPES_REPORT_CALLS.with(|c| c.set(c.get() + 1));
        for d in &diags {
            tracing::warn!(target: "geode::config", "{d}");
        }
    }
    saved
}

impl ShellView {
    /// Apply (or reject) a freshly loaded `Config` (Task 1c-1): rebuild the
    /// keymap and mod alias from it, re-apply the theme only if `[theme]`
    /// actually changed (so a runtime palette theme pick isn't silently
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
    /// and a `sources`/`datasets` doc — or the `[pricing] adapter` key
    /// (line-pricer §5.5) — that disagrees with
    /// `sources_baseline`/`datasets_baseline`/`pricing_baseline` — the
    /// docs (and key) the data engine was actually built from, not
    /// merely the previous reload's config — sets `restart_required` and
    /// emits `ShellEvent::RestartRequired`;
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
        let (mod_alias, mod_diags) = mod_alias_from_config(&new_config);
        // The modules' fragments go back in at the same point in the
        // layer order `main.rs` put them (§8.4): above the builtin docs,
        // below desk and user. Splicing them here rather than
        // recomputing them from the roster is what keeps a reload from
        // re-reporting every fragment diagnostic — and omitting the
        // splice entirely would unbind every module key on the first
        // config write of a session, the same class of bug `builtin`'s
        // own doc comment records.
        let layered = fragments::splice(
            new_config.layered_docs("keymap"),
            &self.services.keymap_fragments,
        );
        let (keymap, keymap_diags) = build_keymap(&layered, mod_alias, &self.services.registry);
        // `mod_diags` extended in here, BEFORE `reload::decide` runs below: an
        // error-severity diagnostic (the refused `keymap.mod = "ctrl"`
        // alias, Task 4b) must reject the whole reload as last-good,
        // exactly like any other invalid config — `decide` only ever
        // looks at `new_config.diagnostics`.
        new_config.diagnostics.extend(mod_diags);
        // A reload that adds (or keeps) `[app] modules.default` says so
        // too — the key is no longer read (spec 2026-09-08 add-tile
        // §7.1), and a warning is the only thing standing between a desk
        // file and silent rot. An `Option` extends as zero or one.
        let modules_default = crate::defaults::modules_default_diagnostic(&new_config);
        new_config.diagnostics.extend(modules_default);
        new_config.diagnostics.extend(keymap_diags);

        let outcome = reload::decide(&new_config);
        // Phase 4b §4.4: every diagnostic this load produced (config
        // parse/merge problems, the mod-alias/keymap-build diagnostics
        // just extended in above) reaches the entity's "config" section
        // regardless of whether `decide` applies or rejects the reload —
        // a rejected reload's own fatal diagnostic is exactly the kind of
        // thing a trader watching the diagnostics tile needs to see.
        //
        // The modules' fragment diagnostics (§8.4) join that batch here,
        // AFTER `decide` and without touching `new_config.diagnostics`, on
        // both counts deliberately. They must be re-stated every reload
        // because `check_fragment` already dropped the offending binding,
        // so `build_keymap` has nothing left to report about it and the
        // diagnostic would otherwise disappear from the tile at the first
        // reload of the session. And they must NOT reach `decide` (nor
        // the `rejected` list below): a compiled-in fragment's mistake is
        // the module author's, not the trader's, and rejecting their
        // config reload over it would be both wrong and unfixable from
        // any file they can edit.
        let mut config_section = new_config.diagnostics.clone();
        config_section.extend(self.services.keymap_fragment_diagnostics.iter().cloned());
        self.diagnostics.update(cx, |d, cx| {
            let before = d.version();
            d.note_config(config_section, SystemTime::now());
            if d.version() != before {
                cx.notify();
            }
        });
        // §19.6: computed here, before `new_config` moves into
        // `self.services.config` inside the `Applied` arm below — a
        // rejected outcome never reaches that arm, but the diagnostics
        // still live on `new_config` and this is the last point both are
        // in hand together. Only `Severity::Error` ones: those are
        // exactly what made `decide` keep last-good, so this is the
        // subset a dialog or the bridge needs to say "your write was
        // refused, and here is why" — a warning here would be describing
        // an applied reload, which this branch, by construction, never is.
        let rejected: Vec<Diagnostic> =
            if matches!(outcome, reload::ReloadOutcome::KeptLastGood { .. }) {
                new_config
                    .diagnostics
                    .iter()
                    .filter(|d| d.severity == Severity::Error)
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };
        if let reload::ReloadOutcome::Applied { warnings } = &outcome {
            // Fix wave, Fix 4: `decide` folds warning-severity diagnostics
            // (config + keymap-build) into `Applied { warnings }` rather
            // than discarding them, but nothing previously read that field
            // — a warning-only reload (e.g. an unknown-but-non-fatal
            // keymap key) applied silently with no trace anywhere. Surface
            // each at `geode::config` warn, one event per warning — the
            // same target `main.rs`'s startup diagnostics log at (Phase
            // 4b Task 2; these are plain `String`s by the time they reach
            // here — `decide` already extracted `Diagnostic::message` —
            // so there's no `Diagnostic` Display impl to reuse here).
            for warning in warnings {
                tracing::warn!(target: "geode::config", "{warning}");
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
            let scopes_changed = changed("scopes") || changed("datasets") || changed("dimensions");
            // `view_presentation` is merged over the views in
            // `geode_core::config::load_views`, so a change to it changes
            // the `ViewSpec`s a tile runs on exactly as a `views` edit
            // does — and it is the doc the Views dialog writes on the
            // commonest edit there is (a column width). Omitted here, a
            // personalisation would sit on disk until the next restart
            // (spec §5.6).
            // `colours` rides here too (Part 2c §6.2): a named colour is
            // part of what a tile PAINTS, and the app's bridge re-reads
            // the doc in this very event's handler to hand the blotter
            // factory the new definitions. Omitted, a trader's colour
            // edit would sit on disk until the next restart — the same
            // failure `view_presentation`'s inclusion just above fixed.
            // dataset_presentation rides here for the same reason
            // view_presentation does (dataset-presentation spec §6):
            // load_views merges it into every ViewSpec of that dataset.
            let views_changed = changed("views")
                || changed("view_presentation")
                || changed("dataset_presentation")
                || changed("dimensions")
                || changed("colours");
            // The dimension pickers' column list (Phase 4a §3.3):
            // `pickable_columns` depends on exactly `datasets` (categorical
            // columns) and `dimensions` (derived dimensions) — the same
            // pair `groupings_changed`/`scopes_changed` already check
            // alongside their own doc, so this reuses `changed` rather than
            // re-deriving the condition.
            let pickable_changed = changed("datasets") || changed("dimensions");
            // M8 (3b final review): compared against the docs the running
            // data engine was actually built from
            // (`sources_baseline`/`datasets_baseline`), not against the
            // previous reload's config — so reverting a `sources.toml`
            // edit back to that baseline clears `restart_required` below
            // instead of leaving a stale message up for the rest of the
            // session (comparing against the previous reload instead would
            // report "changed" on the revert too, since the value differs
            // from what was there a moment ago).
            let mut restart = [
                ("sources", &self.sources_baseline),
                ("datasets", &self.datasets_baseline),
            ]
            .into_iter()
            .filter(|(name, baseline)| !docs_equal(new_config.layered_docs(name), baseline))
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
            // Same rule, for the `[pricing] adapter` key the data
            // engine's pricer was chosen from at startup (line-pricer
            // §5.5) — see `pricing_baseline`'s field doc. Narrowed to
            // this one key: `refresh` is a live sheet setting (Part 3)
            // and must not demand a restart.
            if new_config.get("app", "pricing.adapter").cloned() != self.pricing_baseline {
                restart.push("pricing");
            }

            // Phase 4b §4.3: an `[log]` change applies through the same
            // `LevelControl` door `:level` (a later task) uses, and
            // updates the entity so the diagnostics tile's own log
            // section reflects it — never re-persisted here (this
            // *picked up* a change already on disk; re-writing it back
            // would be pointless, and `Diagnostics::set_levels` is
            // deliberately the no-persist twin of `request_level`).
            if changed("app") {
                let (new_levels, log_diags) = LogLevels::from_doc(&new_config);
                for d in &log_diags {
                    tracing::warn!(target: "geode::config", "{d}");
                }
                // Phase 4b Task 4 fix round 1, MIN-9: `set_levels` runs
                // whenever the levels actually changed, regardless of
                // whether `self.services.log` is wired up — only
                // `LevelControl::set` (the real subscriber) needs a real
                // `LogServices` to call. Before this fix both were
                // guarded by the same `if let Some(log) = ..`, so every
                // test setup that opts out of logging (`log: None`,
                // every fixture that doesn't build one explicitly) let
                // `Diagnostics.levels` silently drift from what's on
                // disk on every reload.
                if new_levels != self.diagnostics.read(cx).levels {
                    if let Some(log) = &self.services.log
                        && let Err(e) = log.control.set(&new_levels)
                    {
                        tracing::warn!(target: "geode::config", "failed to apply [log]: {e}");
                    }
                    self.diagnostics.update(cx, |d, cx| {
                        if d.set_levels(new_levels) {
                            cx.notify();
                        }
                    });
                }
            }

            self.services.config = new_config;
            self.services.mod_alias = mod_alias;
            self.services.keymap = keymap;
            cx.set_global(crate::tips::Chords(Arc::new(
                self.services.keymap.bindings().to_vec(),
            )));
            // Cheap re-derive; `render` applies it only when it changed.
            self.font_size = FontSize::from_config(&self.services.config);
            self.find_style = FindStyle::from_config(&self.services.config);
            self.add_direction = crate::tileadd::AddDirection::from_config(&self.services.config);
            let line_numbers = crate::linenumbers::LineNumbers::from_config(&self.services.config);
            if line_numbers != self.line_numbers {
                self.line_numbers = line_numbers;
                cx.set_global(crate::linenumbers::UiSettings { line_numbers });
            }

            if pickable_changed {
                self.pickable = pickable_columns(&self.services.config);
            }

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

            // I2 (final review, residual fix): this `cx.emit` must be
            // queued before ANY `self.frame.update(..)` call below —
            // including the `groupings_changed` one right after it, not
            // just the `views_changed` block further down — so it has to
            // sit here, above both. gpui doesn't flush effects in
            // subscriber-registration order; it flushes the queue it
            // built while this function ran, and an `Effect::Notify` is
            // deduped to the position of its *first* queueing per emitter
            // (`pending_notifications` in gpui's `App::push_effect`,
            // `gpui-pre-0.3.5/src/app.rs`). `groupings_changed` and
            // `views_changed` both key off `dimensions` (line ~153), and
            // `GroupingSlots::from_doc` really does depend on that doc, so
            // a single reload can make `groupings_changed`'s own
            // `frame.update(..) { .. cx.notify() }` fire below. If the
            // emit ran after that block, the frame's Notify would already
            // hold first position in the queue — `views_changed`'s later
            // `cx.notify()` on the same entity would just no-op against
            // that same slot — and the frame's tiles would requery
            // (`on_frame_changed`) before the app bridge's `ConfigReloaded`
            // handler replaces the factory's/handle's views
            // (`ReplaceViews`), querying the *old* views while recording
            // the *new* version and never triggering the requery they
            // actually needed. Emitting here, before either
            // `frame.update`, guarantees `ConfigReloaded` occupies the
            // earlier queue slot regardless of which branch below touches
            // the frame first.
            if views_changed {
                cx.emit(ShellEvent::ConfigReloaded);
            }
            if groupings_changed {
                let slots = rebuild_slots(&self.services.config);
                self.frame.update(cx, |f, cx| {
                    if f.replace_slots(slots) {
                        cx.notify();
                    }
                });
            }
            if scopes_changed {
                // `true` (Phase 4b Task 1 fix round 1, MIN-8): a live
                // reload actually changing `scopes.toml` is exactly when
                // a fresh diagnostic should surface, unlike `main.rs`'s
                // one-shot startup call for action registration.
                let saved = rebuild_saved_scopes(&self.services.config, true);
                self.frame.update(cx, |f, cx| {
                    if f.replace_saved_scopes(saved) {
                        cx.notify();
                    }
                });
            }
            // Phase 4b Task 5 fix round 1, MAJ-3: unconditional now,
            // unlike the `ConfigReloaded` emission just above — that event
            // is scoped to what the DATA thread needs (`views`/
            // `dimensions`), but `versions.config` is a general "some
            // config was reloaded" signal other observers key off (the
            // diagnostics module's config-section explainer, `main.rs`'s
            // own config-refresh subscription) and must bump on every
            // applied reload, including an `[log]`- or `[theme]`-only
            // edit that leaves `views`/`dimensions` untouched — otherwise
            // exactly the reload `:level`'s own persist write causes
            // never refreshes the one tile whose job is to show it.
            // `BlotterTile::follows_changed` also reads this counter (a
            // requery on config != data change), so this does mean a
            // visible blotter tile now requeries on every applied reload,
            // not only a `views`/`dimensions` one — accepted: reloads are
            // rare, deliberate file edits (the ~500ms poll only acts when
            // something actually changed), not a per-frame or per-
            // keystroke cost.
            {
                self.frame.update(cx, |f, cx| {
                    f.note_config_reloaded();
                    cx.notify();
                });
            }
            let restart_message = if !restart.is_empty() {
                Some(format!(
                    "{} changed — restart to apply",
                    restart.join(" and ")
                ))
            } else {
                // M8: both docs are back at the baseline the running data
                // engine was built from — the on-disk config no longer
                // disagrees with what's running, so the message is stale.
                None
            };
            self.restart_required = restart_message.clone();
            // Phase 4b §4.4: the entity's own copy, so the diagnostics
            // tile can show it without reaching back into `ShellView`.
            self.diagnostics.update(cx, |d, cx| {
                let before = d.version();
                d.set_restart_required(restart_message.clone());
                if d.version() != before {
                    cx.notify();
                }
            });
            if let Some(message) = restart_message {
                cx.emit(ShellEvent::RestartRequired(message));
            }
        }

        self.last_reload = outcome;
        // §19.6: logged before the emit, at the same target and level
        // `Applied`'s own warnings use above, so a rejected reload's
        // reason ends up in the log the same way an applied one's does —
        // the emit alone would leave nothing behind for a trader who
        // isn't watching whatever consumes `ShellEvent::ReloadRejected`
        // at that moment.
        if !rejected.is_empty() {
            for d in &rejected {
                tracing::error!(target: "geode::config", "{d}");
            }
            cx.emit(ShellEvent::ReloadRejected(rejected));
        }
        cx.notify();
    }
}
