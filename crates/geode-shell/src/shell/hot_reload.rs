//! Apply configuration reloads to shell settings, frame state, and app events.
//! File scanning and loading run in the background watcher; validation and
//! runtime updates here run on the UI thread.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gpui::{Context, SharedString};

use crate::defaults::mod_alias_from_config;
use crate::fontsize::FontSize;
use crate::keymap::build_keymap;
use crate::keymap::fragments;
use crate::reload;
use crate::vimfind::FindStyle;
use geode_core::config::{Config, Diagnostic, EXPRESSIONS_DOC, Severity};
use geode_core::dimensions::DerivedDimensions;
use geode_core::groupings::GroupingSlots;
use geode_core::log::LogLevels;
use geode_core::schema::SchemaSpec;

use super::{ShellEvent, ShellView, docs_equal, pickable_columns};

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

impl ShellView {
    /// Validate and apply a loaded candidate. File, modifier, clock, and keymap
    /// errors present at `reload::decide` retain the active configuration;
    /// both accepted and rejected attempts update diagnostics and reload status.
    /// Readers invoked after that decision do not roll back the whole reload.
    ///
    /// An accepted candidate updates shell settings, rebuilds affected frame
    /// state, and advances the frame's config revision. View-related changes
    /// emit `ConfigReloaded`, and `app` changes `AppSettingsReloaded`, before
    /// frame notifications. Source, dataset, egress, positions, panels,
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
            // Presentation, dimensions, and named colors all affect the views or
            // factory settings refreshed by the app's `ConfigReloaded` handler.
            let views_changed = changed("views")
                || changed("view_presentation")
                || changed("dataset_presentation")
                || changed("dimensions")
                || changed(geode_core::config::COLORS_DOC);
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
                cx.notify();
            }

            if pickable_changed {
                self.pickable = pickable_columns(&self.services.config);
                // Every open expression field re-ranks against the new columns
                // at once, not at its next keystroke — including one covered
                // by another dialog. `expr_suggest::completion_mut` reaches
                // only `top_kind()`'s field, so a covered `ScopeExpr` dialog
                // or a covered `Object` dialog's open expression field (live
                // or parked under another domain's dialog) is rebuilt directly
                // here instead, and shows fresh suggestions the moment it is
                // revealed rather than at its own next edit.
                self.expr_vocab = std::rc::Rc::new(super::expr_vocab(&self.services.config));
                let vocab = self.expr_vocab.clone();
                if let Some(state) = self.scope_expr_dialog.as_mut() {
                    state.completion.rebuild(&vocab);
                }
                for state in self
                    .object_dialog
                    .iter_mut()
                    .chain(super::dialog::parked_objects_mut(&mut self.modals))
                {
                    if let Some(expr) = state.expr.as_mut() {
                        expr.rebuild(&vocab);
                    }
                }
            }

            // Named colors may have changed; open column stages, covered or not,
            // offer the new set.
            super::objectdialog::render::refresh_color_choices(self);

            if theme_changed {
                self.services
                    .theme
                    .apply_from_config(&self.services.config, cx);
            }

            if palette_snapshot_changed {
                // There is no `Window` here to restore focus immediately. Arm restoration
                // for the next render before dropping a potentially focused palette;
                // otherwise its orphaned focus can leave shell key handlers unreachable.
                self.palette = None;
                self.pending_focus_restore = true;
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
                // An open expression dialog offers the new definitions at
                // once, not at its next staging.
                super::scope_expr_view::sync_named_offers(self, cx);
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
        // Log rejection errors as well as emitting them for dialogs and the bridge.
        if !rejected.is_empty() {
            for d in &rejected {
                tracing::error!(target: "geode::config", "{d}");
            }
            cx.emit(ShellEvent::ReloadRejected(rejected));
        }
        // Every open dialog re-derives against what this reload applied.
        self.refresh_dialog_rows(cx);
        cx.notify();
    }
}
