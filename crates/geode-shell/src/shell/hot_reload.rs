//! Apply configuration reloads to shell settings, frame state, and app events.
//! File scanning and loading run in the background watcher; validation and
//! runtime updates here run on the UI thread.
//!
//! The watcher and the reload are the app's (`ShellApp::spawn_watcher`,
//! `ShellApp::apply_reload`): one loop and one application for every
//! window. What a reload changes in a window — its picker vocabulary, an
//! open palette, its dialogs — the window does itself when it hears
//! `ShellEvent::Reloaded` (`ShellView::on_app_event`).

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui::{App, Context, SharedString, Window};

use crate::defaults::mod_alias_from_config;
use crate::fontsize::FontSize;
use crate::keymap::build_keymap;
use crate::keymap::fragments;
use crate::perf::FrameHistogram;
use crate::reload;
use crate::session;
use crate::vimfind::FindStyle;
use geode_core::config::{Config, Diagnostic, EXPRESSIONS_DOC, Severity};
use geode_core::dimensions::DerivedDimensions;
use geode_core::groupings::GroupingSlots;
use geode_core::log::LogLevels;
use geode_core::schema::SchemaSpec;

use super::{ShellApp, ShellEvent, ShellView, docs_equal, pickable_columns, text_dimension_names};

/// Delay between background reload polls. Scanning and loading run off the UI
/// thread; validation and application run on it. Work adds to this interval.
///
/// Also drives visible diagnostics frame-histogram refresh and the process
/// memory sample (`memory::sample`, logged whether or not diagnostics is
/// watched). Keep this interval no less than `perf::IDLE_CUTOFF` (500ms), or
/// add a floor at the `refresh_frame_hist` and `refresh_memory` call sites so
/// idle samples are excluded consistently.
pub(super) const RELOAD_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Read grouping slots against the current dataset schema and derived
/// dimensions, logging grouping diagnostics. Missing documents use defaults.
/// Schema and dimension diagnostics are not reported here. Shared by startup
/// and reload; reload calls this after the rejection decision.
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

/// The column names a grouping chain may hold under `config`: the Groupings
/// editor's vocabulary, and the test an ad hoc chain must still pass after a
/// reload or a session restore.
pub(super) fn groupable_names(config: &Config) -> Vec<String> {
    super::groupable_columns(config)
        .into_iter()
        .map(|pickable| pickable.column)
        .collect()
}

// Count calls that report saved-scope diagnostics, allowing tests to check
// startup deduplication without capturing logs.
#[cfg(test)]
thread_local! {
    pub(crate) static SAVED_SCOPES_REPORT_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Read saved scopes against the current dataset schema and dimensions.
/// Only scope diagnostics are logged, and only when `report_diagnostics` is
/// true. The shell's startup and reload paths report; startup action
/// registration reads silently to avoid duplicate logs. Re-exported as
/// `crate::shell::saved_scopes` for the application.
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

/// Read named scope expressions, checking each against the vocabulary the
/// current datasets and dimensions define, and log the entry diagnostics.
/// An invalid entry is kept so a reference to it reports "invalid" rather
/// than "missing". Shared by startup and reload.
pub fn rebuild_named_expressions(config: &Config) -> geode_core::named::NamedExpressions {
    let vocab = super::expr_vocab(config);
    let (named, diags) = config
        .doc(EXPRESSIONS_DOC)
        .map(|d| geode_core::named::NamedExpressions::from_doc(d, &vocab))
        .unwrap_or_default();
    for d in &diags {
        tracing::warn!(target: "geode::config", "{d}");
    }
    named
}

impl ShellApp {
    /// Start the one periodic loop, called once by `ShellApp::new`. Every
    /// tick, in this order: sweep the flip barrier, refresh the diagnostics
    /// histogram and memory sample, refresh the cached date, write a dirty
    /// session, then scan the config directories and apply a changed
    /// candidate. The loop ends once every registered view's window has
    /// closed (`watcher_runs`): it never ticks for windows nobody sees.
    pub(super) fn spawn_watcher(&mut self, cx: &mut Context<Self>) {
        // Seed the config snapshot on the background executor. Directory scans
        // perform filesystem I/O and must not run during UI construction.
        cx.spawn(async move |this, cx| {
            let mut is_first_poll = true;
            let mut memory = crate::memory::MemoryTracker::new();
            loop {
                cx.background_executor().timer(RELOAD_POLL_INTERVAL).await;

                // The loop ends with the last window, not with this entity or
                // its views: the quit hooks hold the app and the bridge's
                // closures a view past the windows, and a tick then would
                // still copy memory into diagnostics, write the session and
                // reload config for nobody, its notifies waking data
                // submissions.
                let Ok(runs) = this.update(cx, |app, cx| app.watcher_runs(cx)) else {
                    return; // app gone; stop polling
                };
                if !runs {
                    return;
                }

                // Sweep the flip barrier on the shared poll loop. An expired barrier
                // releases on the next iteration, so delay includes the timer, session
                // write, and config scan rather than only the timer interval.
                let Ok(frame) = this.update(cx, |app, _cx| app.frame.clone()) else {
                    return; // app gone; stop polling
                };
                frame.update(cx, |f, cx| {
                    if f.sweep(Instant::now()) {
                        cx.notify();
                    }
                });

                // Refresh the diagnostics histogram only while a consumer watches it.
                // The source gather is gated as well; the destination compares before
                // copying and notifying.
                let Ok((diagnostics, watched)) = this.update(cx, |app, cx| {
                    let watched = app.diagnostics.read(cx).watchers() > 0;
                    (app.diagnostics.clone(), watched)
                }) else {
                    return; // app gone; stop polling
                };
                if watched {
                    let Ok(perf) = this.update(cx, |app, cx| app.frame_histogram(cx)) else {
                        return; // app gone; stop polling
                    };
                    diagnostics.update(cx, |d, cx| {
                        if d.refresh_frame_hist(&perf) {
                            cx.notify();
                        }
                    });
                }

                // Sample process memory on every tick, watched or not, so the
                // `geode::memory` log keeps its peak record with the page
                // closed. The copy into diagnostics is gated on watchers and
                // on `refresh_memory`'s hysteresis, so idle jitter never
                // notifies.
                if let Some(sample) = crate::memory::sample() {
                    let log = memory.observe(sample, Instant::now(), SystemTime::now());
                    if let Some(reading) = memory.reading().copied() {
                        crate::memory::emit(log, &reading);
                        if watched {
                            diagnostics.update(cx, |d, cx| {
                                if d.refresh_memory(&reading) {
                                    cx.notify();
                                }
                            });
                        }
                    }
                }

                // Refresh the cached date; a change reaches every window as
                // `DayChanged`.
                if this.update(cx, |app, cx| app.refresh_today(cx)).is_err() {
                    return; // app gone; stop polling
                }

                // Extract session state on the UI thread and await its write on the
                // background executor. Run before config-scan early returns so saving
                // does not depend on a config change. Write failures are logged without
                // restoring the snapshot's dirty flag or comparison baselines. The
                // snapshot is the app's: one writer over every window's records.
                let Ok(pending_write) = this.update(cx, |app, cx| app.take_dirty_session_write(cx))
                else {
                    return; // app gone; stop polling
                };
                if let Some((path, text)) = pending_write {
                    cx.background_executor()
                        .spawn(async move { session::write_atomic(&path, &text) })
                        .await
                        .unwrap_or_else(|e| {
                            tracing::warn!(target: "geode::session", "failed to save session: {e}")
                        });
                }

                let Ok((desk_dir, user_dir)) =
                    this.update(cx, |app, _cx| (app.desk_dir.clone(), app.user_dir.clone()))
                else {
                    return; // app gone; stop polling
                };

                let scan_desk = desk_dir.clone();
                let scan_user = user_dir.clone();
                let snapshot = cx
                    .background_executor()
                    .spawn(async move { reload::scan(scan_desk.as_deref(), scan_user.as_deref()) })
                    .await;

                if is_first_poll {
                    is_first_poll = false;
                    // Seed-only: store this first background-thread scan as
                    // the baseline and move on, without comparing it to the
                    // `Snapshot::default()` placeholder — that comparison
                    // would always read as "changed" (default is empty,
                    // and a real desk/user dir practically never is) and
                    // trigger a reload of the config `new` was just handed,
                    // at every start.
                    if this
                        .update(cx, |app, _cx| app.last_snapshot = snapshot)
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }

                let Ok(changed) = this.update(cx, |app, _cx| {
                    let changed = snapshot.changed_since(&app.last_snapshot);
                    if changed {
                        app.last_snapshot = snapshot.clone();
                    }
                    changed
                }) else {
                    return;
                };
                if !changed {
                    continue;
                }

                // The builtin layer is read off the live services rather
                // than rebuilt here (see `ShellServices::builtin`), and
                // only once a change has actually been seen — a clone per
                // 500ms poll would be pure per-frame churn for a reload
                // that almost never happens.
                let Ok(builtin) = this.update(cx, |app, _cx| app.services.builtin.clone()) else {
                    return;
                };
                let new_config = cx
                    .background_executor()
                    .spawn(async move { reload::load_config(builtin, desk_dir, user_dir) })
                    .await;

                if this
                    .update(cx, |app, cx| app.apply_reload(new_config, cx))
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    /// Every open window's frame-interval histogram, summed: the one
    /// histogram the diagnostics page shows. A closed window's view, held
    /// by a host, contributes nothing: its frozen counts would otherwise
    /// sit in the page for the rest of the run.
    fn frame_histogram(&self, cx: &App) -> FrameHistogram {
        let mut merged = FrameHistogram::new();
        for view in self.open_views(cx) {
            merged.merge(&view.read(cx).perf);
        }
        merged
    }

    /// Re-read today's date from `AppClock`; a change emits `DayChanged`
    /// so every window repaints its scope bar. Nothing is emitted while
    /// the date stands.
    fn refresh_today(&mut self, cx: &mut Context<Self>) {
        let today = cx
            .global::<crate::clock::AppClock>()
            .0
            .today(chrono::Utc::now());
        if self.today != today {
            self.today = today;
            cx.emit(ShellEvent::DayChanged);
        }
    }

    /// Validate and apply a loaded candidate. File, modifier, clock, and keymap
    /// errors present at `reload::decide` retain the active configuration;
    /// both accepted and rejected attempts update diagnostics and reload status.
    /// Readers invoked after that decision do not roll back the whole reload.
    ///
    /// An accepted candidate updates shell settings, rebuilds affected frame
    /// state, and advances the frame's config revision. View-related changes
    /// emit `ConfigReloaded`, and `app` changes `AppSettingsReloaded`, then
    /// `Reloaded` tells every window what changed, all before frame
    /// notifications. Source, dataset, egress, positions, panels,
    /// pricing-adapter and vol-model differences from startup require a
    /// restart; returning to those baselines clears the restart message.
    pub(super) fn apply_reload(&mut self, mut new_config: Config, cx: &mut Context<Self>) {
        let (mod_alias, mod_diags) = mod_alias_from_config(&new_config);
        // Retain compiled module fragments between builtins and desk/user layers.
        // Reuse the checked fragments so module bindings survive every reload.
        let layered = fragments::splice(
            new_config.layered_docs("keymap"),
            &self.services.keymap_fragments,
        );
        let (keymap, keymap_diags) = build_keymap(&layered, mod_alias, &self.services.registry);
        // Modifier errors must reach the rejection decision.
        new_config.diagnostics.extend(mod_diags);
        // Warn when the candidate contains the unused `modules.default` key.
        let modules_default = crate::defaults::modules_default_diagnostic(&new_config);
        new_config.diagnostics.extend(modules_default);
        // Report an unconfigured default fetch source without rejecting the reload.
        let default_source = crate::series::default_source_diagnostic(&new_config);
        new_config.diagnostics.extend(default_source);
        // Invalid time-zone or day-boundary settings reject the whole candidate.
        let (clock, clock_diags) = geode_core::clock::Clock::from_config(&new_config);
        new_config.diagnostics.extend(clock_diags.iter().cloned());
        new_config.diagnostics.extend(keymap_diags);

        let outcome = reload::decide(&new_config);
        // Replace the displayed config batch even when the candidate is rejected.
        // Retain compiled-fragment diagnostics after the decision: their invalid
        // bindings were already dropped, and editing user config cannot fix them.
        // They must remain visible without blocking user reloads.
        let mut config_section = new_config.diagnostics.clone();
        config_section.extend(self.services.keymap_fragment_diagnostics.iter().cloned());
        config_section.extend(self.services.composition_diagnostics.iter().cloned());
        self.diagnostics.update(cx, |d, cx| {
            let before = d.version();
            d.note_config(config_section, SystemTime::now());
            if d.version() != before {
                cx.notify();
            }
        });
        // Only errors that caused rejection belong in the rejection event.
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
            // Log accepted warnings as well as retaining them in diagnostics.
            for warning in warnings {
                tracing::warn!(target: "geode::config", "{warning}");
            }

            let theme_changed =
                self.services.config.get("app", "theme") != new_config.get("app", "theme");
            // Only bindings and the resolved modifier alias invalidate the palette's
            // snapshot. An unrelated edit, such as a theme change, leaves it open.
            let palette_snapshot_changed = self.services.mod_alias != mod_alias
                || !docs_equal(
                    self.services.config.layered_docs("keymap"),
                    new_config.layered_docs("keymap"),
                );

            // Compare layered inputs with the active config before replacing it.
            let changed = |name: &str| {
                !docs_equal(
                    self.services.config.layered_docs(name),
                    new_config.layered_docs(name),
                )
            };
            let groupings_changed =
                changed("groupings") || changed("datasets") || changed("dimensions");
            let scopes_changed = changed("scopes") || changed("datasets") || changed("dimensions");
            // Entry warnings name columns, so a schema change re-reads the entries.
            let named_changed =
                changed(EXPRESSIONS_DOC) || changed("datasets") || changed("dimensions");
            // Presentation, dimensions, named colors, and value colors all
            // affect the views or factory settings refreshed by the app's
            // `ConfigReloaded` handler.
            let views_changed = changed("views")
                || changed("view_presentation")
                || changed("dataset_presentation")
                || changed("dimensions")
                || changed(geode_core::config::COLORS_DOC)
                || changed(geode_core::config::VALUE_COLORS_DOC)
                // The bridge re-folds watchlist rules on this event: their
                // saved scopes and named expressions live in these docs.
                || changed(geode_core::config::WATCHLISTS_DOC)
                || changed("scopes")
                || changed(EXPRESSIONS_DOC);
            // `app` settings the bridge hands to module factories.
            let app_changed = changed("app");
            // Dimension picker columns depend on dataset columns and derived dimensions.
            let pickable_changed = changed("datasets") || changed("dimensions");
            // Compare with the data engine's startup inputs so reverting a change
            // clears the restart requirement.
            let mut restart = [
                ("sources", &self.sources_baseline),
                ("datasets", &self.datasets_baseline),
                ("egress", &self.egress_baseline),
                ("positions", &self.positions_baseline),
                ("panels", &self.panels_baseline),
                ("scenarios", &self.scenarios_baseline),
            ]
            .into_iter()
            .filter(|(name, baseline)| !docs_equal(new_config.layered_docs(name), baseline))
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
            // The pricing adapter and the vol model are fixed at startup, so each
            // requires restart when it differs; `refresh` is a live setting.
            if new_config.get("app", "pricing.adapter").cloned() != self.pricing_baseline {
                restart.push("pricing");
            }
            if new_config.get("app", "vol.model").cloned() != self.vol_baseline {
                restart.push("vol");
            }

            // Apply log levels without persisting them again: the candidate already
            // came from disk. Report control errors and update the diagnostics model.
            if app_changed {
                let (new_levels, log_diags) = LogLevels::from_doc(&new_config);
                for d in &log_diags {
                    tracing::warn!(target: "geode::config", "{d}");
                }
                // Keep the diagnostics model current even without a log subscriber.
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
            self.config_revision += 1;
            let bindings = self.services.keymap.bindings().to_vec();
            cx.set_global(crate::tips::Chords(Arc::new(bindings)));
            // Cheap re-derive; each window's `render` applies it only when it changed.
            self.font_size = FontSize::from_config(&self.services.config);
            self.find_style = FindStyle::from_config(&self.services.config);
            self.add_direction = crate::tileadd::AddDirection::from_config(&self.services.config);
            let line_numbers = crate::linenumbers::LineNumbers::from_config(&self.services.config);
            if line_numbers != self.line_numbers {
                self.line_numbers = line_numbers;
                cx.set_global(crate::linenumbers::UiSettings { line_numbers });
            }
            // A changed composition rule moves no frame generation, so each
            // window re-pulls its emitters on `Reloaded` or the groups keep
            // the old rule.
            let link_filter = crate::linkfilter::from_config(&self.services.config);
            let link_filter_changed = link_filter != self.link_include_tile_filter;
            self.link_include_tile_filter = link_filter;
            // `[timeseries] default_source` and the fetch sources beside
            // it, re-derived and republished only on a change — a
            // `set_global` on every reload poll would wake every
            // `observe_global` subscriber for nothing.
            let series = crate::series::SeriesSettings::from_config(&self.services.config);
            if cx
                .try_global::<crate::series::SeriesSettings>()
                .is_none_or(|global| *global != series)
            {
                self.default_source = series.default_source.clone();
                cx.set_global(series);
            }
            // Republish a changed display clock and today's date. This does not
            // change the frame's scope, grouping, or as-of selection.
            if clock != cx.global::<crate::clock::AppClock>().0 {
                cx.set_global(crate::clock::AppClock(clock));
                self.today = clock.today(chrono::Utc::now());
            }

            if theme_changed {
                // The theme reads the config beside it and needs the context
                // mutably.
                let services = &mut self.services;
                services.theme.apply_from_config(&services.config, cx);
            }

            // Queue this event before ANY frame update that can notify. GPUI keeps
            // an entity's first notification position when deduplicating effects.
            // A dimensions edit can change both groupings and views: an earlier
            // frame notification would let tiles query old factory/handle views
            // while recording the new config revision, with no later retry.
            // The bridge must refresh those views before tiles observe the revision.
            if views_changed {
                cx.emit(ShellEvent::ConfigReloaded);
            }
            if app_changed {
                cx.emit(ShellEvent::AppSettingsReloaded);
            }
            // Every window's own reload work (`ShellView::on_app_event`),
            // queued after the bridge's events: a window sees the new
            // revision only once the bridge has refreshed its views.
            cx.emit(ShellEvent::Reloaded {
                revision: self.config_revision,
                pickable_changed,
                palette_snapshot_changed,
                link_filter_changed,
                named_changed,
            });
            if groupings_changed {
                let slots = rebuild_slots(&self.services.config);
                // The same inputs decide what an ad hoc chain may name. A
                // chain left naming a removed column would be refused by
                // every following tile's query with no way to see why.
                let groupable = groupable_names(&self.services.config);
                self.frame.update(cx, |f, cx| {
                    let replaced = f.replace_slots(slots);
                    let dropped = f.retain_ad_hoc(|column| groupable.iter().any(|g| g == column));
                    for column in &dropped {
                        tracing::warn!(
                            target: "geode::config",
                            "ad hoc grouping dropped: '{column}' is no longer a groupable column"
                        );
                    }
                    if replaced || !dropped.is_empty() {
                        cx.notify();
                    }
                });
            }
            if scopes_changed {
                // Report diagnostics for the newly loaded saved scopes.
                let saved = rebuild_saved_scopes(&self.services.config, true);
                self.frame.update(cx, |f, cx| {
                    if f.replace_saved_scopes(saved) {
                        cx.notify();
                    }
                });
            }
            if named_changed {
                let named = rebuild_named_expressions(&self.services.config);
                self.frame.update(cx, |f, cx| {
                    if f.replace_named_expressions(named) {
                        cx.notify();
                    }
                });
            }
            // Every applied reload advances the config revision, including changes
            // that do not emit the view-specific `ConfigReloaded` event.
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
                // All restart-sensitive inputs match the running engine's baselines.
                None
            };
            self.restart_required = restart_message.clone().map(SharedString::from);
            // Expose the same restart state through the diagnostics model.
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
        self.reload_status = self.last_reload.status_message().map(SharedString::from);
        // Log rejection errors as well as emitting them for dialogs, the
        // windows and the bridge.
        if !rejected.is_empty() {
            for d in &rejected {
                tracing::error!(target: "geode::config", "{d}");
            }
            cx.emit(ShellEvent::ReloadRejected(rejected));
        }
    }
}

impl ShellView {
    /// What this window does when the app announces a change. `Reloaded`
    /// carries the reload's "changed" flags, computed once by
    /// `ShellApp::apply_reload`, so each window rebuilds only what the
    /// reload touched; `DayChanged` and `ReloadRejected` repaint. The
    /// bridge's events (`ConfigReloaded`, `AppSettingsReloaded`,
    /// `RestartRequired`, `DistinctRequested`) are not this window's to
    /// act on, and `Park` has no handler yet.
    pub(super) fn on_app_event(
        &mut self,
        event: &ShellEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ShellEvent::Reloaded {
                pickable_changed,
                palette_snapshot_changed,
                link_filter_changed,
                named_changed,
                ..
            } => {
                if *link_filter_changed {
                    self.force_repull_emitters(cx);
                }

                if *pickable_changed {
                    self.pickable = pickable_columns(&self.services(cx).config);
                    self.text_dims = text_dimension_names(&self.services(cx).config);
                    // The frame expression dialog re-ranks against the new columns
                    // at once, not at its next keystroke — including when another
                    // dialog covers it. `expr_suggest::completion_mut` reaches
                    // only `top_kind()`'s field, so a covered `ScopeExpr` dialog is
                    // rebuilt directly here instead, and shows fresh suggestions
                    // the moment it is revealed rather than at its own next edit.
                    self.expr_vocab =
                        std::rc::Rc::new(super::expr_vocab(&self.services(cx).config));
                    let vocab = self.expr_vocab.clone();
                    if let Some(state) = self.scope_expr_dialog.as_mut() {
                        state.completion.rebuild(&vocab);
                    }
                }

                // Named colors may have changed; open column stages, covered or not,
                // offer the new set.
                super::objectdialog::render::refresh_color_choices(self, cx);

                if *palette_snapshot_changed {
                    // Arm restoration for the next render before dropping a potentially
                    // focused palette; otherwise its orphaned focus can leave shell key
                    // handlers unreachable.
                    self.palette = None;
                    self.pending_focus_restore = true;
                }

                if *named_changed {
                    // An open expression dialog offers the new definitions at
                    // once, not at its next staging.
                    super::scope_expr_view::sync_named_offers(self, cx);
                }

                // Every open dialog re-derives against what this reload applied.
                self.refresh_dialog_rows(cx);
                cx.notify();
            }
            // The rejected reload changed no revision, but its status text
            // and diagnostics did: repaint, and let a dialog that just
            // wrote the file re-derive.
            ShellEvent::ReloadRejected(_) => {
                self.refresh_dialog_rows(cx);
                cx.notify();
            }
            // The scope bar paints the app's cached date.
            ShellEvent::DayChanged => cx.notify(),
            ShellEvent::ConfigReloaded
            | ShellEvent::AppSettingsReloaded
            | ShellEvent::RestartRequired(_)
            | ShellEvent::DistinctRequested(_)
            | ShellEvent::Park { .. } => {}
        }
    }
}
