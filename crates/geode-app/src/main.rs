//! Application composition: initialize logging and GPUI, load layered config,
//! register data providers and module factories, restore the session, and open
//! the shell. This crate connects the shell, data service, and feature modules
//! without introducing dependencies between those layers.

mod assets;
mod bridge;
mod crash;
mod demo;
mod demo_bus;
mod demo_series;
mod events;

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use geode_core::config::{ConfigSources, Diagnostic, LayerDoc, Severity};
use geode_core::log::{LevelControl, LogLevels, Ring, RingLayer};
use geode_diagnostics::DiagnosticsPageFactory;
use geode_shell::actions::ActionRegistry;
use geode_shell::defaults::{
    BUILTIN_KEYMAP, mod_alias_from_config, modules_default_diagnostic, register_add_actions,
    register_builtin_actions, register_page_actions, register_pick_actions, register_scope_actions,
};
use geode_shell::diagnostics::ActionTail;
use geode_shell::fonts;
use geode_shell::keymap::build_keymap;
use geode_shell::keymap::fragments;
use geode_shell::module::{ModuleRoster, PageRoster};
use geode_shell::session;
use geode_shell::shell::{LogServices, ShellServices, ShellView, pickable_columns, saved_scopes};
use geode_shell::theme;
use geode_shell::tiling::Workspaces;
use geode_shell::vimfind::FindStyle;
use gpui::App;
use gpui::prelude::*;
use gpui_component::{Root, TitleBar};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Registry, fmt, reload};

fn main() {
    // Install logging before configuration loading so startup failures reach the
    // ring, stderr, and any available file sink. Apply configured levels once
    // configuration is loaded. Keep `_log_guard` bound until application exit;
    // dropping it stops the background file writer.
    let (log_ring, log_control, _log_guard) = install_logging();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let demo_rows = match parse_args(&args) {
        Ok(rows) => rows,
        Err(message) => {
            tracing::error!(target: "geode::config", "{message}");
            // `process::exit` skips destructors. Drop the file writer's guard first
            // to flush buffered startup diagnostics.
            drop(_log_guard);
            std::process::exit(2);
        }
    };

    // Prepare demo sources before opening a window. A warm source directory is
    // reused, keeping generation outside the UI's render work.
    let demo_root = demo_rows.map(demo::demo_dir);
    if let (Some(rows), Some(root)) = (demo_rows, &demo_root)
        && let Err(e) = demo::ensure_emitted(root, rows)
    {
        tracing::error!(target: "geode::ingest", "failed to emit sample data into {root:?}: {e}");
        // Flush the file writer before exiting without unwinding.
        drop(_log_guard);
        std::process::exit(1);
    }

    gpui_platform::application()
        .with_assets(assets::AppAssets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx); // must run before any component use
            // Install shell key overrides after component bindings so list dialogs and
            // the palette retain their navigation keys. Dialog test hosts use this same
            // initializer; individual key coverage is documented at its definition.
            geode_shell::shell::dialog::init_reclaimed_keybindings(cx);

            // Register bundled fonts after component init creates the theme and
            // before the first window frame reads it.
            fonts::register(cx);

            // Install tile key overrides after component initialization so a focused
            // table does not consume keys owned by the tile.
            geode_blotter::init(cx);
            geode_diagnostics::init(cx);
            // Market-data tables need the same tile-owned key routing.
            geode_marketdata::init(cx);
            // The range popup owns Tab navigation between its date fields. Its key
            // context overrides Root focus cycling while the popup is active.
            geode_timeseries::init(cx);
            // Keep table bindings from consuming the pricer's editing keys.
            geode_pricer::init(cx);

            // Register demo transports before data setup resolves configured sources
            // and upload targets. Non-demo startup supplies an empty adapter registry.
            let (demo_feed, adapters) = if demo_rows.is_some() {
                let (adapter, feed) = geode_data::adapter::ChannelAdapter::new("demo_bus");
                let mut adapters = geode_data::adapter::AdapterRegistry::default();
                adapters.register(adapter);
                // Use the risk generator's seed for both series sources, exercising
                // catalogue and manual-identity discovery.
                adapters.register(demo_series::DemoSeries::new("demo_kdb", 42, true));
                adapters.register(demo_series::DemoSeries::new("demo_rest", 42, false));
                (Some(feed), adapters)
            } else {
                (None, geode_data::adapter::AdapterRegistry::default())
            };

            // The mock pricing implementation is available in every build.
            let mut pricers = geode_data::PricerRegistry::default();
            pricers.register(Arc::new(geode_pricing::MockPricer::new()));
            // The demo vol model likewise; `[vol] model` selects it by name.
            let mut vol_models = geode_data::VolModelRegistry::default();
            vol_models.register(Arc::new(geode_pricing::DemoVolModel));

            let (mut services, desk, user, bridge, diagnostics_factory) = build_shell_services(
                demo_root.as_deref(),
                config_dirs(),
                log_ring,
                log_control,
                adapters,
                pricers,
                vol_models,
                cx,
            );

            // Start document producers after the service installs its subscriptions.
            // The three producers (`demo_bus::demo_producers`: CVI, dividend, chain)
            // share the risk generator's underlyings. Keep the bus alive until the
            // quit hook stops its thread.
            let mut demo_bus = demo_feed.map(|feed| {
                // Seed synthetic document dates from the configured application clock.
                let today = geode_core::clock::Clock::from_config(&services.config)
                    .0
                    .today(chrono::Utc::now());
                let producers =
                    demo_bus::demo_producers(geode_demo_data::demo_underlyings(), today);
                demo_bus::spawn(
                    feed,
                    producers,
                    Duration::from_secs(5),
                    Duration::from_secs(2),
                    42,
                )
            });

            // Install the panic hook after logging and action registration. It captures
            // only thread-safe handles: the log ring, action tail, and shared action-name
            // map. The registry itself remains on the UI thread.
            if let Some(log) = &services.log {
                let names_snapshot = services.registry.hash_names();
                let names: Arc<dyn Fn(u64) -> Option<String> + Send + Sync> = Arc::new(move |h| {
                    // A hook runs before unwinding releases locks. Avoid waiting on a lock
                    // the panicking thread may hold; an unavailable name becomes unknown
                    // in the report.
                    names_snapshot
                        .try_read()
                        .ok()
                        .and_then(|m| m.get(&h).cloned())
                });
                crash::install_panic_hook(
                    user.clone(),
                    log.ring.clone(),
                    services.action_tail.clone(),
                    names,
                );
            }

            for warning in services.theme.apply_from_config(&services.config, cx) {
                tracing::warn!(target: "geode::theme", "{warning}");
            }

            // Themes are restored from layered config; the session supplies
            // layout, module records, frame state, palette usage and page
            // state.
            restore_session(&mut services);

            // Save current session state synchronously at quit, including changes
            // since the last periodic snapshot. This is best-effort and does not
            // join any in-flight periodic session write; their renames may race.
            cx.on_app_quit(|cx| {
                for window in cx.windows() {
                    if let Some(handle) = window.downcast::<Root>() {
                        let _ = handle.update(cx, |root, _window, cx| {
                            if let Ok(shell) = root.view().clone().downcast::<ShellView>() {
                                shell.read(cx).save_session(cx);
                            }
                        });
                    }
                }
                async {}
            })
            .detach();

            // Unsaved sheets are saved, then the data service stops off the
            // UI thread (`bridge::stop_at_quit`), never as a side effect of a
            // `DataHandle` simply dropping on `main`'s own thread at quit.
            if let Some(bridge) = &bridge {
                bridge::stop_at_quit(bridge, cx);
            }

            // Stop the demo producer on a background executor so joining its thread
            // does not block the UI. Taking the bus from an Option makes repeated hook
            // calls harmless. GPUI's quit deadline still limits how long cleanup can run.
            if let Some(bus) = demo_bus.take() {
                let bus = Arc::new(Mutex::new(Some(bus)));
                cx.on_app_quit(move |cx| {
                    let bus = Arc::clone(&bus);
                    cx.background_executor().spawn(async move {
                        if let Some(mut bus) = bus.lock().unwrap().take() {
                            bus.stop();
                        }
                    })
                })
                .detach();
            }

            cx.spawn(async move |cx| {
                // The shell toolbar owns title-bar controls and drag gestures. Use the
                // matching window options when creating its native window.
                let window = cx
                    .open_window(TitleBar::window_options(), |window, cx| {
                        let view = cx.new(|cx| ShellView::new(services, desk, user, window, cx));
                        cx.new(|cx| Root::new(view, window, cx))
                    })
                    .expect("failed to open window");

                // Route service outcomes into the shell and forward configuration
                // changes once the window exists.
                if let Some(bridge) = &bridge {
                    cx.update(|cx| bridge::attach(bridge, window, cx));
                }

                // Refresh diagnostic configuration independently of the data bridge.
                // The frame's config counter changes on every applied reload, including
                // log, theme, and keymap edits that do not emit ConfigReloaded. Other
                // frame changes must not clone configuration or trigger this refresh.
                cx.update(|cx| {
                    let shell = window
                        .read(cx)
                        .ok()
                        .and_then(|root| root.view().clone().downcast::<ShellView>().ok())
                        .expect("the window's root view is the shell");
                    let frame = shell.read(cx).frame().clone();
                    let last_config_version =
                        std::rc::Rc::new(std::cell::Cell::new(frame.read(cx).config_version()));
                    // Register this observer before the diagnostics page registers its
                    // own. GPUI invokes observers in registration order: the shared
                    // factory configuration must be current before the page consumes
                    // the same version bump and rebuilds its config section.
                    cx.observe(&frame, move |frame, cx| {
                        let now = frame.read(cx).config_version();
                        if now != last_config_version.get() {
                            last_config_version.set(now);
                            diagnostics_factory.set_config(shell.read(cx).config().clone());
                        }
                    })
                    .detach();
                });
            })
            .detach();
        });
}

/// Install process-wide tracing with a reloadable level filter, synchronous
/// stderr and in-memory ring layers, and an optional background file writer.
/// Default levels apply until configuration is loaded. The ring retains 4,096
/// records. File setup failure leaves stderr and ring logging available.
///
/// Daily files live under `<user>/logs/geode.YYYY-MM-DD.log`; their date and
/// rotation use UTC, independently of the configured display clock. Startup
/// trims matching files to seven before opening the current log. Rotation does
/// not prune files during the run.
///
/// Retain the returned [`tracing_appender::non_blocking::WorkerGuard`] for the
/// application lifetime and drop it before explicit process exits. Dropping it
/// stops the file writer; subsequent file-bound records are lost. Daily logs can
/// lag behind the synchronous ring that supplies panic reports.
fn install_logging() -> (
    Arc<Ring>,
    Arc<dyn LevelControl>,
    Option<tracing_appender::non_blocking::WorkerGuard>,
) {
    let ring = Arc::new(Ring::new(4096));
    let (filter, reload_handle) = reload::Layer::new(LogLevels::default().to_targets());

    let (_, user) = config_dirs();
    let mut log_guard = None;
    let file_layer = user.as_ref().and_then(|dir| {
        let logs = dir.join("logs");
        std::fs::create_dir_all(&logs).ok()?;
        // The seven-file cap, applied once at startup — `tracing_appender`
        // rotates going forward but never prunes files from before this
        // run.
        crash::trim_log_files(&logs, 7);
        let appender = tracing_appender::rolling::RollingFileAppender::builder()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("geode")
            .filename_suffix("log")
            .build(&logs)
            .ok()?;
        // File writes run on a dedicated thread. The panic hook reads the
        // synchronous ring, so its report does not wait for this buffer to flush.
        let (non_blocking, guard) = tracing_appender::non_blocking(appender);
        log_guard = Some(guard);
        Some(fmt::layer().with_writer(non_blocking).with_ansi(false))
    });

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(RingLayer::new(ring.clone()))
        .with(file_layer)
        .init();

    struct ReloadControl(reload::Handle<tracing_subscriber::filter::Targets, Registry>);
    impl LevelControl for ReloadControl {
        fn set(&self, levels: &LogLevels) -> Result<(), String> {
            self.0
                .reload(levels.to_targets())
                .map_err(|e| e.to_string())
        }
    }

    (ring, Arc::new(ReloadControl(reload_handle)), log_guard)
}

/// Parse no arguments or `--demo [rows]`, defaulting to 100,000 demo rows.
/// The row count must parse as `usize`; zero is accepted. Other inputs return
/// a usage error, which startup logs before exiting with status 2.
fn parse_args(args: &[String]) -> Result<Option<usize>, String> {
    match args {
        [] => Ok(None),
        [flag] if flag == "--demo" => Ok(Some(100_000)),
        [flag, rows] if flag == "--demo" => rows
            .parse::<usize>()
            .map(Some)
            .map_err(|_| usage(&format!("'{rows}' is not a row count"))),
        _ => Err(usage("unrecognised arguments")),
    }
}

fn usage(reason: &str) -> String {
    format!("{reason}\nusage: geode [--demo [rows]]")
}

/// Register modules backed by the bridge's shared factories and data handle.
/// The roster's `Rc` forwarding exposes every factory method, including
/// `accepts` and `launch_state`, while the bridge retains the factories for
/// live reloads.
/// Finally hands the roster's context columns to the data handle, so every
/// query carries them.
fn add_bridge_modules(roster: &mut ModuleRoster, bridge: &bridge::Bridge) {
    roster.add(Box::new(bridge.factory.clone()));
    // One tile kind per accepted panel, in `panels` order; only the first
    // ships the shared marketdata keymap fragment.
    for panel in &bridge.panels {
        roster.add(Box::new(panel.clone()));
    }
    roster.add(Box::new(bridge.timeseries.clone()));
    roster.add(Box::new(bridge.pricer.clone()));
    // Last, once every factory (and, from Part 2, every dimension action)
    // is registered: each row then carries the values they open on.
    bridge.handle.set_context_columns(roster.context_columns());
}

/// Every builtin config doc: the shell's keymap, the pricer's two bundled
/// views and seven package templates (a desk or user layer overrides a
/// view or a template by name), the pricer's two datasets (`pricer_sheets`,
/// its local documents, and `pricer`, its computed vocabulary), the builtin
/// market-data panels, and the `--demo` layer.
fn builtin_layer(demo_root: Option<&Path>) -> Vec<LayerDoc> {
    let mut builtin = vec![
        LayerDoc::builtin("keymap", BUILTIN_KEYMAP).expect("builtin keymap TOML is well-formed"),
        // The pricer's two bundled views, over its computed `pricer` dataset.
        // `views` merges per view name: a demo, desk or user views doc adds
        // its own views beside these and overrides one by name.
        LayerDoc::builtin("views", geode_pricer::core::BUILTIN_VIEWS)
            .expect("BUILTIN_VIEWS is well-formed TOML"),
        LayerDoc::builtin(
            geode_pricer::core::PRICER_TEMPLATES_DOC,
            geode_pricer::core::BUILTIN_TEMPLATES,
        )
        .expect("BUILTIN_TEMPLATES is well-formed TOML"),
        // The pricer's sheets, a local document dataset every build
        // declares. `datasets` merges per dataset name, so a demo, desk or
        // user `datasets` doc adds its own datasets beside these two.
        LayerDoc::builtin("datasets", geode_pricer::core::PRICER_SHEETS_DECLARATION)
            .expect("PRICER_SHEETS_DECLARATION is well-formed TOML"),
        // The builtin market-data panels. `panels` replaces per panel name,
        // so a desk or user panels doc adds panels beside these or replaces
        // one whole.
        LayerDoc::builtin(
            geode_core::panel::PANELS_DOC,
            geode_marketdata::core::BUILTIN_PANELS,
        )
        .expect("BUILTIN_PANELS is well-formed TOML"),
        // The pricer's vocabulary as a computed dataset: views, scopes and
        // groupings see its columns; nothing stores or queries it.
        LayerDoc::builtin("datasets", geode_pricer::core::PRICER_DATASET_DECLARATION)
            .expect("PRICER_DATASET_DECLARATION is well-formed TOML"),
    ];
    if let Some(root) = demo_root {
        builtin.extend(demo::layer(&root.join("src")));
    }
    builtin
}

/// Load layered configuration, construct the action registry and module roster,
/// compile keybindings, and prepare shell services. The initial workspace is
/// empty; session restoration happens before the shell is constructed. Config
/// and keymap diagnostics are logged with their declared severity.
///
/// `dirs` are the desk and user config directories (`config_dirs()` in
/// production; temporary directories in tests, which must never read the
/// real user layer). Return the same directories so the shell watches the
/// sources it loaded. Return the data bridge for attachment after window
/// creation; data setup requires datasets and views documents. The builtin
/// pricer declarations supply these even outside demo mode.
///
/// `demo_root` names an already prepared demo directory. Its configuration is
/// part of the builtin layer, below desk and user overrides. Provider registries
/// are passed to data setup; logging uses the existing subscriber and applies
/// configured levels through its reload control.
// The composition root's inputs: config directories, logging and the three
// provider registries, each consumed once here.
#[allow(clippy::too_many_arguments)]
fn build_shell_services(
    demo_root: Option<&Path>,
    dirs: (Option<PathBuf>, Option<PathBuf>),
    log_ring: Arc<Ring>,
    log_control: Arc<dyn LevelControl>,
    adapters: geode_data::adapter::AdapterRegistry,
    pricers: geode_data::PricerRegistry,
    vol_models: geode_data::VolModelRegistry,
    cx: &mut App,
) -> (
    ShellServices,
    Option<PathBuf>,
    Option<PathBuf>,
    Option<bridge::Bridge>,
    Rc<DiagnosticsPageFactory>,
) {
    let (desk, user) = dirs;
    let builtin = builtin_layer(demo_root);
    // Load configuration and retain its builtin documents from one source set.
    // Configuration writes and reloads must keep the same demo and pricer defaults.
    let (config, builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin,
        desk: desk.clone(),
        user: user.clone(),
    });
    for diag in &config.diagnostics {
        print_diagnostic(diag);
    }

    // Apply configured levels for the remainder of startup and the run.
    // Earlier messages used the subscriber's default filter.
    let (log_levels, log_diags) = LogLevels::from_doc(&config);
    for diag in &log_diags {
        print_diagnostic(diag);
    }
    // Report filter-reload failures rather than silently retaining old levels.
    if let Err(e) = log_control.set(&log_levels) {
        tracing::warn!(target: "geode::config", "failed to apply [log]: {e}");
    }

    let mut registry = ActionRegistry::default();
    register_builtin_actions(&mut registry);
    // Register schema-derived picker actions before resolving keymap bindings.
    register_pick_actions(&mut registry, &pickable_columns(&config));
    // Register saved-scope actions before keymap compilation. New scope action
    // names require restart. ShellView reports this document's diagnostics at
    // startup, so suppress duplicate reporting here.
    register_scope_actions(&mut registry, &saved_scopes(&config, false));

    // Tiles name their module kind explicitly. An unclaimed kind paints a
    // placeholder; a deprecated modules.default setting is diagnosed below.
    let mut roster = ModuleRoster::new();

    let mut pages = PageRoster::new();
    // Diagnostics needs no data handle and is always registered. Return its
    // shared factory so the window's frame-config observer can refresh it.
    let diagnostics_factory = Rc::new(DiagnosticsPageFactory::new(
        log_ring.clone(),
        config.clone(),
    ));
    pages.add(Box::new(diagnostics_factory.clone()));

    // Data-backed factories require a successful data setup. If setup is absent,
    // those kinds have no add-tile actions and restored tiles remain placeholders.
    // The builtin pricer dataset and views normally supply the required documents.
    let db = bridge::db_path(
        &config,
        demo_root,
        std::env::var("LOCALAPPDATA").ok(),
        std::env::var("HOME").ok(),
    );
    // Refused panels: printed here and carried into the shell's config
    // section, where they stay until a restart can change which panels exist.
    let mut composition_diagnostics = Vec::new();
    let bridge = bridge::data_setup(&config, db, adapters, pricers, vol_models).map(|setup| {
        composition_diagnostics = setup.panel_diagnostics.clone();
        let find_style = FindStyle::from_config(&config);
        let stale_after = bridge::stale_after_from_config(&config);
        let bridge = bridge::start(setup, find_style, stale_after, cx);
        add_bridge_modules(&mut roster, &bridge);
        bridge
    });
    for diag in &composition_diagnostics {
        print_diagnostic(diag);
    }

    // Register split actions for the complete roster before keymap compilation.
    // A kind whose add-tile ids would collide is refused rather than panicking;
    // its Error joins the refused panels in the config section.
    let add_diags = register_add_actions(&mut registry, &roster.kinds());
    for diag in &add_diags {
        print_diagnostic(diag);
    }
    composition_diagnostics.extend(add_diags);
    // Register module actions before resolving their keybindings.
    roster.register_actions(&mut registry);
    // Page toggles and each page's own actions, likewise before the keymap.
    let page_titles: Vec<(&str, &str)> = pages.entries().map(|e| (e.kind, e.title)).collect();
    register_page_actions(&mut registry, &page_titles);
    pages.register_actions(&mut registry);

    // Log modifier-alias and deprecated-setting diagnostics here. ShellView
    // recomputes these for its diagnostics entity; they are separate from the
    // configuration loader's diagnostic list.
    let (mod_alias, mod_diags) = mod_alias_from_config(&config);
    for diag in &mod_diags {
        print_diagnostic(diag);
    }
    // Report a deprecated default-module setting instead of silently ignoring it.
    if let Some(diag) = modules_default_diagnostic(&config) {
        print_diagnostic(&diag);
    }
    // Insert validated module bindings above builtin defaults and below desk/user
    // layers. Their diagnostics depend on the completed roster and registry, so
    // carry them into ShellServices alongside keymap compilation diagnostics.
    let (mut fragments, mut frag_diags) = roster.keymap_fragments();
    // Page fragments after module fragments: the toggle bindings are
    // shell-generated docs the roster emits unchecked.
    let (page_fragments, page_diags) = pages.keymap_fragments();
    fragments.extend(page_fragments);
    frag_diags.extend(page_diags);
    for diag in &frag_diags {
        print_diagnostic(diag);
    }
    let layered = fragments::splice(config.layered_docs("keymap"), &fragments);
    let (keymap, mut keymap_diags) = build_keymap(&layered, mod_alias, &registry);
    for diag in &keymap_diags {
        print_diagnostic(diag);
    }
    // Into `keymap_diagnostics` for `ShellView::new`'s startup seeding,
    // and kept as their own list for `apply_reload` to re-state on every
    // reload — see `ShellServices::keymap_fragment_diagnostics`.
    keymap_diags.extend(frag_diags.iter().cloned());

    let (theme, theme_warnings) = theme::load_bundled();
    for warning in &theme_warnings {
        tracing::warn!(target: "geode::theme", "{warning}");
    }

    // Use the resolved user directory for session state when available. Path
    // resolution does not check writability; the session writer reports failures.
    let session_path = user.as_ref().map(|dir| dir.join("session.toml"));

    let services = ShellServices {
        config,
        builtin,
        registry,
        keymap,
        mod_alias,
        workspaces: Workspaces::new(),
        theme,
        session_path,
        roster,
        restored_tiles: std::collections::BTreeMap::new(),
        restored_frame: None,
        restored_pinned: Default::default(),
        restored_palette_usage: geode_shell::palette_usage::PaletteUsage::new(),
        log: Some(LogServices {
            ring: log_ring,
            control: log_control,
            levels: log_levels,
        }),
        // Share recent action hashes with the panic hook installed after services
        // and the action-name registry have been built.
        action_tail: Arc::new(Mutex::new(ActionTail::new())),
        // What `build_keymap` reported above. Printed already; carried
        // here because `ShellView::new` cannot recompute it (it needs
        // this registry, not just the config) and the diagnostics page's
        // config section would otherwise miss it until a hot reload.
        keymap_diagnostics: keymap_diags,
        // The checked fragments themselves, so `apply_reload` can splice
        // the same ones back in at every hot reload — see the field's own
        // doc comment for why they are carried rather than recomputed.
        keymap_fragments: fragments,
        keymap_fragment_diagnostics: frag_diags,
        composition_diagnostics,
        pages,
        restored_pages: std::collections::BTreeMap::new(),
    };
    (services, desk, user, bridge, diagnostics_factory)
}

/// Load layout, module records, frame state, palette usage and page state
/// from the session file before the shell is constructed. Recovery warnings are
/// logged; a missing or unreadable session starts fresh.
fn restore_session(services: &mut ShellServices) {
    let Some(path) = &services.session_path else {
        return;
    };
    let restored = session::load(path);
    for warning in &restored.warnings {
        tracing::warn!(target: "geode::session", "{warning}");
    }
    services.workspaces = restored.workspaces;
    services.restored_tiles = restored.tiles;
    services.restored_frame = restored.frame;
    services.restored_pinned = restored.pinned;
    services.restored_palette_usage = restored.palette_usage;
    services.restored_pages = restored.pages;
}

/// Log a configuration or keymap diagnostic at `geode::config`, using its
/// severity and Display message.
fn print_diagnostic(diag: &Diagnostic) {
    match diag.severity {
        Severity::Warning => tracing::warn!(target: "geode::config", "{diag}"),
        Severity::Error => tracing::error!(target: "geode::config", "{diag}"),
    }
}

/// Resolve the desk directory from `GEODE_DESK_CONFIG` and the user directory
/// from `APPDATA`, falling back to `HOME/.config`. Missing or non-Unicode
/// environment values are treated as absent. No filesystem checks are made.
fn config_dirs() -> (Option<PathBuf>, Option<PathBuf>) {
    let desk = std::env::var("GEODE_DESK_CONFIG").ok().map(PathBuf::from);
    let user = user_config_dir(std::env::var("APPDATA").ok(), std::env::var("HOME").ok());
    (desk, user)
}

/// Pure core of the user-config-directory resolution in [`config_dirs`]:
/// `%APPDATA%/geode` when set, else `$HOME/.config/geode`, else `None`.
/// Kept as a pure function of its inputs so it's unit-testable without
/// touching the real environment.
fn user_config_dir(appdata: Option<String>, home: Option<String>) -> Option<PathBuf> {
    if let Some(appdata) = appdata {
        return Some(PathBuf::from(appdata).join("geode"));
    }
    home.map(|home| PathBuf::from(home).join(".config").join("geode"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::Config;
    use geode_shell::module::ModuleFactory as _;

    /// A second CVI panel over the same dataset: another title and forward
    /// format, one action. It names every column the CVI document writes,
    /// header included, or it would be refused.
    const WIDE_CVI_PANEL: &str = r#"config_version = 1

[cvi_wide]
title = "CVI (wide)"
dataset = "cvi_params"
document = "cvi_params"
actions = ["marketdata::cvi_reanchor"]

[cvi_wide.value]
type = "f64"
format = { precision = 6 }

[cvi_wide.rows]
column = "term"
identity = "date"
label = "shown"

[cvi_wide.columns]
axis = "node"

[[cvi_wide.header]]
column = "anchor_date"
label = "anchor"
type = "date"

[[cvi_wide.header]]
column = "spot_ref"
label = "spot"
type = "f64"

[[cvi_wide.slice]]
column = "forward"
label = "fwd"
format = { precision = 3 }

[[cvi_wide.slice]]
column = "atm"
label = "atm"

[[cvi_wide.slice]]
column = "skew"
label = "skew"
"#;

    #[test]
    fn appdata_wins_when_set() {
        let dir = user_config_dir(
            Some("C:\\Users\\me\\AppData\\Roaming".to_string()),
            Some("/home/me".to_string()),
        );
        assert_eq!(
            dir,
            Some(PathBuf::from("C:\\Users\\me\\AppData\\Roaming").join("geode"))
        );
    }

    #[test]
    fn home_config_used_without_appdata() {
        let dir = user_config_dir(None, Some("/home/me".to_string()));
        assert_eq!(dir, Some(PathBuf::from("/home/me/.config/geode")));
    }

    #[test]
    fn none_when_neither_env_var_set() {
        assert_eq!(user_config_dir(None, None), None);
    }

    #[test]
    fn no_arguments_means_no_demo() {
        assert_eq!(parse_args(&[]), Ok(None));
    }

    #[test]
    fn bare_demo_flag_defaults_to_a_hundred_thousand_rows() {
        assert_eq!(parse_args(&["--demo".to_string()]), Ok(Some(100_000)));
    }

    #[test]
    fn demo_flag_with_a_row_count() {
        assert_eq!(
            parse_args(&["--demo".to_string(), "1000000".to_string()]),
            Ok(Some(1_000_000))
        );
    }

    #[test]
    fn a_non_numeric_row_count_is_a_usage_error() {
        assert!(parse_args(&["--demo".to_string(), "abc".to_string()]).is_err());
    }

    #[test]
    fn an_unrecognised_flag_is_a_usage_error() {
        assert!(parse_args(&["--nonesuch".to_string()]).is_err());
    }

    /// The two bundled views are `views` doc entries over the `pricer`
    /// dataset the same layer declares, so they validate clean at load.
    #[test]
    fn the_builtin_layer_carries_the_two_pricer_views() {
        let builtin = builtin_layer(None);
        let config = Config::load(&ConfigSources {
            builtin,
            desk: None,
            user: None,
        });
        assert!(config.doc("pricer_views").is_none(), "retired");
        let (specs, diags) = geode_core::config::load_views(&config);
        assert!(diags.is_empty(), "{diags:?}");
        let (views, diags) = geode_pricer::core::Views::from_specs(&specs);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(
            views.names().collect::<Vec<_>>(),
            vec!["barrier", "vanilla"]
        );
        // Both views also honour the builtin `datasets` declaration under
        // the same validation a desk or user view gets at load: every
        // column they name is one the computed `pricer` dataset declares.
        let (schema, diags) = geode_core::schema::SchemaSpec::from_doc(
            config.doc("datasets").expect("the builtin datasets doc"),
        );
        assert!(diags.is_empty(), "{diags:?}");
        let dims = geode_core::dimensions::DerivedDimensions::default();
        for spec in &specs {
            let diags = spec.validate(&schema, &dims);
            assert!(
                diags.iter().all(|d| d.severity != Severity::Error),
                "{}: {diags:?}",
                spec.name
            );
        }
    }

    /// The seven built-in templates are a builtin-layer doc a desk or user
    /// layer merges over, and they read clean.
    #[test]
    fn the_builtin_layer_carries_the_seven_pricer_templates() {
        let config = Config::load(&ConfigSources {
            builtin: builtin_layer(None),
            desk: None,
            user: None,
        });
        let (set, diags) = geode_pricer::core::TemplateSet::from_doc(
            config
                .doc(geode_pricer::core::PRICER_TEMPLATES_DOC)
                .expect("the doc"),
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(set, geode_pricer::core::TemplateSet::builtin());
    }

    /// The pricer's sheets live in a local document dataset every build
    /// declares, demo or not: the builtin layer carries it, clean.
    #[test]
    fn the_builtin_layer_declares_pricer_sheets_as_a_local_dataset() {
        let config = Config::load(&ConfigSources {
            builtin: builtin_layer(None),
            desk: None,
            user: None,
        });
        let (schema, diags) = geode_core::schema::SchemaSpec::from_doc(
            config.doc("datasets").expect("the builtin datasets doc"),
        );
        assert!(diags.is_empty(), "{diags:?}");
        let sheets = schema
            .dataset(geode_pricer::core::PRICER_SHEETS_DATASET)
            .expect("pricer_sheets is declared");
        assert!(sheets.local, "a sheet is written by the app, not a feed");
    }

    /// `datasets` merges per dataset name, so a demo or desk `datasets`
    /// doc adds its datasets beside the builtin `pricer_sheets` rather
    /// than replacing the doc.
    #[test]
    fn a_demo_or_desk_datasets_doc_unions_with_the_builtin_pricer_sheets() {
        let dir = tempfile::tempdir().unwrap();
        let desk = dir.path().join("desk");
        std::fs::create_dir_all(&desk).unwrap();
        std::fs::write(
            desk.join("datasets.toml"),
            "config_version = 1\n[desk_only]\n[desk_only.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
        )
        .unwrap();
        let config = Config::load(&ConfigSources {
            builtin: builtin_layer(Some(dir.path())),
            desk: Some(desk),
            user: None,
        });
        let (schema, _) = geode_core::schema::SchemaSpec::from_doc(
            config.doc("datasets").expect("the merged datasets doc"),
        );
        for name in [
            geode_pricer::core::PRICER_SHEETS_DATASET,
            "risk_snapshot",
            "desk_only",
        ] {
            assert!(schema.dataset(name).is_some(), "{name} is declared");
        }
        assert!(
            schema
                .dataset(geode_pricer::core::PRICER_SHEETS_DATASET)
                .unwrap()
                .local
        );
    }

    #[test]
    fn the_roster_lists_the_pricer_and_registers_its_add_action() {
        use geode_data::DataHandle;
        use geode_shell::actions::ActionId;

        let mut roster = ModuleRoster::new();
        let (data, _rx) = DataHandle::for_tests();
        roster.add(Box::new(Rc::new(
            geode_pricer::content::PricerFactory::new(
                data,
                Rc::new(geode_pricer::store::MemorySheetStore::default()),
                geode_pricer::core::Views::builtin(),
                geode_pricer::core::TemplateSet::builtin(),
                geode_pricer::content::PricerSettings::default(),
            ),
        )));
        assert!(roster.kinds().contains(&"pricer"));
        let mut registry = ActionRegistry::default();
        register_add_actions(&mut registry, &roster.kinds());
        roster.register_actions(&mut registry);
        assert_eq!(
            registry
                .get(&ActionId("tile::add_pricer".to_string()))
                .expect("an add-tile row")
                .title,
            "Pricer: Split"
        );
        assert!(
            registry
                .get(&ActionId("pricer::add_below".to_string()))
                .is_some()
        );
        let (docs, diags) = roster.keymap_fragments();
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(docs.len(), 1);
    }

    /// Every panel keeps its own add-tile rows; the shared `marketdata`
    /// fragment ships once however many panels there are.
    #[gpui::test]
    fn the_second_panel_ships_no_second_fragment_but_still_gets_an_add_tile_row(
        cx: &mut gpui::TestAppContext,
    ) {
        use geode_shell::actions::ActionId;
        let dir = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(user.path().join("panels.toml"), WIDE_CVI_PANEL).unwrap();
        let (config, _) = ShellServices::config_and_builtin(ConfigSources {
            builtin: builtin_layer(Some(dir.path())),
            desk: None,
            user: Some(user.path().to_path_buf()),
        });
        let setup = bridge::data_setup(
            &config,
            dir.path().join("geode.duckdb"),
            geode_data::adapter::AdapterRegistry::default(),
            geode_data::PricerRegistry::default(),
            geode_data::VolModelRegistry::default(),
        )
        .unwrap();
        assert!(
            setup.panel_diagnostics.is_empty(),
            "{:?}",
            setup.panel_diagnostics
        );
        let bridge =
            cx.update(|cx| bridge::start(setup, FindStyle::default(), Duration::from_secs(60), cx));
        let mut roster = ModuleRoster::new();
        add_bridge_modules(&mut roster, &bridge);
        let (docs, diags) = roster.keymap_fragments();
        assert!(diags.is_empty(), "{diags:?}");
        let marketdata: Vec<String> = docs
            .iter()
            .map(|d| d.file.to_string_lossy().into_owned())
            .filter(|f| {
                ["<module:cvi>", "<module:dividend>", "<module:cvi_wide>"].contains(&f.as_str())
            })
            .collect();
        assert_eq!(
            marketdata,
            ["<module:cvi>"],
            "one fragment for three panels"
        );
        let mut registry = ActionRegistry::default();
        register_add_actions(&mut registry, &roster.kinds());
        for (kind, title) in [
            ("cvi", "Cvi"),
            ("dividend", "Dividend"),
            ("cvi_wide", "Cvi_wide"),
        ] {
            assert_eq!(
                registry
                    .get(&ActionId(format!("tile::add_{kind}")))
                    .unwrap_or_else(|| panic!("{kind} has an add-tile row"))
                    .title,
                format!("{title}: Split")
            );
        }
        bridge.handle.shutdown();
    }

    /// `MODULE_KINDS` names every non-panel kind the production roster holds,
    /// so a panel can never shadow one.
    #[gpui::test]
    fn module_kinds_name_every_other_production_module(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (config, _) = ShellServices::config_and_builtin(ConfigSources {
            builtin: builtin_layer(Some(dir.path())),
            ..ConfigSources::default()
        });
        let setup = bridge::data_setup(
            &config,
            dir.path().join("geode.duckdb"),
            geode_data::adapter::AdapterRegistry::default(),
            geode_data::PricerRegistry::default(),
            geode_data::VolModelRegistry::default(),
        )
        .unwrap();
        let bridge =
            cx.update(|cx| bridge::start(setup, FindStyle::default(), Duration::from_secs(60), cx));
        let mut roster = ModuleRoster::new();
        add_bridge_modules(&mut roster, &bridge);
        let panels: Vec<&str> = bridge.panels.iter().map(|f| f.kind()).collect();
        for kind in roster.kinds().into_iter().filter(|k| !panels.contains(k)) {
            assert!(bridge::MODULE_KINDS.contains(&kind), "{kind}");
        }
        bridge.handle.shutdown();
    }

    /// The timeseries forwarder must expose the factory's kind, registered
    /// actions, and keymap fragment. Check `timeseries::add` explicitly because
    /// the shipped fragment binds it to `a`.
    #[test]
    fn the_roster_lists_timeseries_and_registers_its_add_action() {
        use geode_data::DataHandle;
        use geode_shell::actions::ActionId;

        let mut roster = ModuleRoster::new();
        let (data, _rx) = DataHandle::for_tests();
        roster.add(Box::new(Rc::new(
            geode_timeseries::content::TimeseriesFactory::new(data, Default::default()),
        )));
        assert!(roster.kinds().contains(&"timeseries"));

        let mut registry = ActionRegistry::default();
        register_add_actions(&mut registry, &roster.kinds());
        roster.register_actions(&mut registry);
        assert_eq!(
            registry
                .get(&ActionId("tile::add_timeseries".to_string()))
                .expect("the timeseries kind gets an add-tile row")
                .title,
            "Timeseries: Split"
        );
        assert!(
            registry
                .get(&ActionId("timeseries::add".to_string()))
                .is_some(),
            "the forwarder's `register_actions` reaches the factory"
        );
        // And its keymap fragment: `contexts()` is forwarded, so
        // `check_fragment` accepts a binding into `timeseries`.
        let (docs, diags) = roster.keymap_fragments();
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].file.to_string_lossy(), "<module:timeseries>");
    }

    /// The complete builtin/demo keymap, built as `run` builds it: the
    /// production registry, the bridge's real module factories and their
    /// spliced fragments. Returns the keymap with its diagnostics, the
    /// registry and the mod alias; the temp dir holds the bridge's database.
    fn production_keymap(
        cx: &mut gpui::TestAppContext,
    ) -> (
        geode_shell::keymap::Keymap,
        Vec<Diagnostic>,
        ActionRegistry,
        geode_shell::keymap::Modifiers,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (config, _) = ShellServices::config_and_builtin(ConfigSources {
            builtin: builtin_layer(Some(dir.path())),
            ..ConfigSources::default()
        });
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);

        // The registry in `run`'s own order.
        let mut registry = ActionRegistry::default();
        register_builtin_actions(&mut registry);
        register_pick_actions(&mut registry, &pickable_columns(&config));
        register_scope_actions(&mut registry, &saved_scopes(&config, false));

        // Construct real factories through bridge::start, then reproduce the
        // application roster to check their combined fragments and action IDs.
        let mut pricers = geode_data::PricerRegistry::default();
        pricers.register(std::sync::Arc::new(geode_pricing::MockPricer::new()));
        let mut vol_models = geode_data::VolModelRegistry::default();
        vol_models.register(std::sync::Arc::new(geode_pricing::DemoVolModel));
        let setup = bridge::data_setup(
            &config,
            dir.path().join("geode.duckdb"),
            geode_data::adapter::AdapterRegistry::default(),
            pricers,
            vol_models,
        )
        .expect("the demo layer declares datasets and views");
        let bridge =
            cx.update(|cx| bridge::start(setup, FindStyle::default(), Duration::from_secs(60), cx));
        let mut roster = ModuleRoster::new();
        add_bridge_modules(&mut roster, &bridge);
        let mut pages = PageRoster::new();
        pages.add(Box::new(Rc::new(DiagnosticsPageFactory::new(
            Arc::new(Ring::new(16)),
            config.clone(),
        ))));
        register_add_actions(&mut registry, &roster.kinds());
        roster.register_actions(&mut registry);
        let page_titles: Vec<(&str, &str)> = pages.entries().map(|e| (e.kind, e.title)).collect();
        register_page_actions(&mut registry, &page_titles);
        pages.register_actions(&mut registry);

        let (mut fragments, mut frag_diags) = roster.keymap_fragments();
        let (page_fragments, page_diags) = pages.keymap_fragments();
        fragments.extend(page_fragments);
        frag_diags.extend(page_diags);
        assert!(frag_diags.is_empty(), "{frag_diags:?}");
        let layered = fragments::splice(config.layered_docs("keymap"), &fragments);
        let (mod_alias, mod_diags) = mod_alias_from_config(&config);
        assert!(mod_diags.is_empty(), "{mod_diags:?}");
        let (keymap, keymap_diags) = build_keymap(&layered, mod_alias, &registry);
        (keymap, keymap_diags, registry, mod_alias, dir)
    }

    /// Build the complete builtin/demo keymap with the production registry and
    /// module factories. This checks keystroke syntax as well as declared contexts
    /// and registered action IDs; fragment validation alone does not parse keys.
    #[gpui::test]
    fn the_whole_production_keymap_builds_with_no_diagnostics(cx: &mut gpui::TestAppContext) {
        let (_keymap, keymap_diags, _registry, _mod_alias, _dir) = production_keymap(cx);
        assert!(
            keymap_diags.is_empty(),
            "every shipped binding must name a parseable keystroke and a \
             registered action: {keymap_diags:?}"
        );
    }

    /// `g .` opens the shell's row menu from a focused blotter or pricer in
    /// normal mode, through the production keymap: the builtin
    /// `tile::context_menu` is registered, and both modules' shipped
    /// fragments bind it once spliced.
    #[gpui::test]
    fn the_production_keymap_binds_g_dot_to_row_actions(cx: &mut gpui::TestAppContext) {
        use geode_shell::keymap::{KeyContext, MatchResult, Matcher, parse_keystroke};
        let (keymap, keymap_diags, registry, mod_alias, _dir) = production_keymap(cx);
        assert!(keymap_diags.is_empty(), "{keymap_diags:?}");
        let id = geode_shell::actions::ActionId("tile::context_menu".to_string());
        assert_eq!(
            registry.get(&id).map(|d| d.title.as_str()),
            Some("Row actions\u{2026}")
        );
        for kind in ["blotter", "pricer"] {
            // The stack the shell publishes over a focused `kind` tile.
            let stack = [
                KeyContext::new("workspace"),
                KeyContext::new("tile"),
                KeyContext::new(kind).grid().pair("mode", "normal").counts(),
            ];
            let mut m = Matcher::default();
            let g = parse_keystroke("g", mod_alias).unwrap();
            assert_eq!(m.press(&keymap, g, &stack), MatchResult::Pending, "{kind}");
            let dot = parse_keystroke(".", mod_alias).unwrap();
            match m.press(&keymap, dot, &stack) {
                MatchResult::Matched { action, .. } => {
                    assert_eq!(action.0, "tile::context_menu", "{kind}")
                }
                other => panic!("{kind}: g . resolved to {other:?}"),
            }
        }
    }

    /// The production roster exposes `underlying_ref`-based launch state for
    /// every accepted panel (the builtin CVI and dividend here), names that
    /// column in `context_columns`, and startup hands that list to the data
    /// handle. Exercising startup's registration path checks that shared
    /// factory forwarding preserves `accepts` and `launch_state`.
    #[gpui::test]
    fn the_production_roster_opens_market_data_on_an_underlying(cx: &mut gpui::TestAppContext) {
        use geode_core::context::DimensionContext;
        let dir = tempfile::tempdir().unwrap();
        let (config, _) = ShellServices::config_and_builtin(ConfigSources {
            builtin: builtin_layer(Some(dir.path())),
            ..ConfigSources::default()
        });
        let setup = bridge::data_setup(
            &config,
            dir.path().join("geode.duckdb"),
            geode_data::adapter::AdapterRegistry::default(),
            geode_data::PricerRegistry::default(),
            geode_data::VolModelRegistry::default(),
        )
        .expect("the demo layer declares datasets and views");
        let bridge =
            cx.update(|cx| bridge::start(setup, FindStyle::default(), Duration::from_secs(60), cx));
        let mut roster = ModuleRoster::new();
        add_bridge_modules(&mut roster, &bridge);

        let spx = DimensionContext::of(&[("lhu", "7"), ("underlying_ref", "SPX")]);
        let accepting: Vec<&str> = roster
            .kinds()
            .into_iter()
            .filter(|k| roster.factory(k).is_some_and(|f| spx.offers(f.accepts())))
            .collect();
        assert_eq!(accepting, vec!["cvi", "dividend"]);
        assert_eq!(roster.context_columns(), vec!["underlying_ref".to_string()]);
        for kind in ["cvi", "dividend"] {
            let f = roster.factory(kind).unwrap();
            assert_eq!(f.accepts(), &["underlying_ref"], "{kind}");
            let state = f.launch_state(&spx).expect("a state for an underlying");
            assert_eq!(
                state.get("underlying"),
                Some(&toml::Value::Array(vec![toml::Value::String("SPX".into())])),
                "{kind}"
            );
        }
        assert_eq!(
            bridge.handle.context_columns(),
            vec!["underlying_ref".to_string()],
            "startup hands the roster's context columns to the data service"
        );
    }

    struct NoLevels;
    impl LevelControl for NoLevels {
        fn set(&self, _: &LogLevels) -> Result<(), String> {
            Ok(())
        }
    }

    /// The real startup composition over a temporary demo root with `user`
    /// as the user layer and no desk layer, the session restored as `main`
    /// restores it. Never the real config directories.
    fn compose(
        cx: &mut gpui::TestAppContext,
        demo: &Path,
        user: &Path,
    ) -> (ShellServices, bridge::Bridge) {
        let mut pricers = geode_data::PricerRegistry::default();
        pricers.register(Arc::new(geode_pricing::MockPricer::new()));
        let (mut services, _desk, _user, bridge, _diagnostics) = cx.update(|cx| {
            build_shell_services(
                Some(demo),
                (None, Some(user.to_path_buf())),
                Arc::new(Ring::new(16)),
                Arc::new(NoLevels),
                geode_data::adapter::AdapterRegistry::default(),
                pricers,
                geode_data::VolModelRegistry::default(),
                cx,
            )
        });
        restore_session(&mut services);
        (
            services,
            bridge.expect("the demo layer declares datasets and views"),
        )
    }

    /// A session whose workspace 1 holds one focused tile of `kind`.
    fn write_session(user: &Path, kind: &str) {
        let mut table = session::to_toml(
            &Workspaces::new(),
            &session::TileRecords::new(),
            None,
            &session::PinnedRecords::new(),
            &geode_shell::palette_usage::PaletteUsage::new(),
            &session::PageRecords::new(),
        );
        let ws1: toml::Table = format!(
            "focused = 1\n[node]\nkind = \"leaf\"\nid = 1\n[tiles.1]\nmodule = \"{kind}\"\n"
        )
        .parse()
        .unwrap();
        let Some(toml::Value::Table(ws)) = table.get_mut("workspaces") else {
            panic!("to_toml writes a workspaces table");
        };
        ws.insert("1".to_string(), toml::Value::Table(ws1));
        std::fs::write(user.join("session.toml"), toml::to_string(&table).unwrap()).unwrap();
    }

    /// Open the shell window as `main` does, market-data key overrides
    /// after component init, and paint one frame. The bridge is not
    /// attached: its event drain would be woken from the real data thread,
    /// which gpui's test scheduler refuses as nondeterministic.
    fn open(
        cx: &mut gpui::TestAppContext,
        services: ShellServices,
    ) -> (gpui::WindowHandle<Root>, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(geode_marketdata::init);
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(services, None, None, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
            })
            .unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        vcx.run_until_parked();
        (window, vcx)
    }

    fn shell_of(
        window: &gpui::WindowHandle<Root>,
        cx: &mut gpui::VisualTestContext,
    ) -> gpui::Entity<ShellView> {
        window
            .read_with(cx, |root, _| root.view().clone())
            .unwrap()
            .downcast::<ShellView>()
            .unwrap()
    }

    /// A user panel over `cvi_params` becomes a tile kind: it restores from
    /// a session, its factory carries the configured spec, `g m` offers it,
    /// and the shared marketdata keys reach it. That it paints its own
    /// title and formats is `geode-marketdata`'s
    /// `a_user_layer_panel_paints_its_own_title_and_formats`.
    #[gpui::test]
    fn a_user_panel_over_cvi_params_becomes_a_working_tile_kind(cx: &mut gpui::TestAppContext) {
        use geode_shell::diagnostics::fnv1a;
        use geode_shell::tiling::TileId;
        let demo = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(user.path().join("panels.toml"), WIDE_CVI_PANEL).unwrap();
        write_session(user.path(), "cvi_wide");
        let (services, bridge) = compose(cx, demo.path(), user.path());
        assert!(
            services.composition_diagnostics.is_empty(),
            "{:?}",
            services.composition_diagnostics
        );
        assert!(services.roster.kinds().contains(&"cvi_wide"));
        let wide = bridge
            .panels
            .iter()
            .find(|f| f.kind() == "cvi_wide")
            .expect("the user panel has a factory");
        assert_eq!(wide.spec().title, "CVI (wide)");
        assert_eq!(wide.spec().format.precision, 6);
        assert_eq!(
            wide.spec().slice_value("forward").unwrap().format.precision,
            3
        );
        assert_eq!(
            wide.accepts(),
            &["underlying_ref"],
            "`g m` offers every panel"
        );
        let tail = services.action_tail.clone();
        let (window, mut vcx) = open(cx, services);
        let shell = shell_of(&window, &mut vcx);
        assert_eq!(
            shell.read_with(&vcx, |s, _| s.occupant_kind(TileId(1))),
            Some("cvi_wide")
        );
        vcx.simulate_keystrokes(".");
        vcx.run_until_parked();
        let menu = fnv1a("marketdata::menu");
        assert_eq!(
            tail.lock().unwrap().recent().filter(|h| *h == menu).count(),
            1,
            "the shared marketdata keys reach the configured panel"
        );
        bridge.handle.shutdown();
    }

    /// Existing sessions keep working: a saved `cvi` tile restores as the
    /// config-built CVI panel, not a placeholder.
    #[gpui::test]
    fn a_saved_cvi_tile_restores_as_the_config_built_cvi_panel(cx: &mut gpui::TestAppContext) {
        use geode_shell::tiling::TileId;
        let demo = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        write_session(user.path(), "cvi");
        let (services, bridge) = compose(cx, demo.path(), user.path());
        let (window, mut vcx) = open(cx, services);
        let shell = shell_of(&window, &mut vcx);
        assert_eq!(
            shell.read_with(&vcx, |s, _| s.occupant_kind(TileId(1))),
            Some("cvi")
        );
        bridge.handle.shutdown();
    }

    /// A panel named `cvi_stacked` would register `tile::add_cvi_stacked`,
    /// the builtin CVI panel's stack id: once a registry panic at startup.
    /// It is refused by name, and CVI keeps its own stack row.
    #[gpui::test]
    fn a_panel_named_like_another_kinds_placement_is_refused_without_panicking(
        cx: &mut gpui::TestAppContext,
    ) {
        use geode_shell::actions::ActionId;
        let demo = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("panels.toml"),
            WIDE_CVI_PANEL.replace("cvi_wide", "cvi_stacked"),
        )
        .unwrap();
        let (services, bridge) = compose(cx, demo.path(), user.path());
        assert_eq!(
            services
                .composition_diagnostics
                .iter()
                .map(|d| d.path.as_deref().unwrap_or("-"))
                .collect::<Vec<_>>(),
            ["panels.cvi_stacked"]
        );
        assert!(!services.roster.kinds().contains(&"cvi_stacked"));
        assert_eq!(
            services
                .registry
                .get(&ActionId("tile::add_cvi_stacked".into()))
                .expect("the CVI panel's stack row")
                .title,
            "Cvi: Stack"
        );
        bridge.handle.shutdown();
    }

    /// A refused panel is absent from the tile picker, and its Error reaches
    /// the status bar's config count and the diagnostics tile through the
    /// real startup composition.
    #[gpui::test]
    fn a_refused_panel_is_absent_from_the_picker_and_named_in_diagnostics(
        cx: &mut gpui::TestAppContext,
    ) {
        use geode_shell::actions::ActionId;
        let demo = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        let bad = WIDE_CVI_PANEL.replace("cvi_wide", "vol_bad").replacen(
            "dataset = \"cvi_params\"",
            "dataset = \"nonesuch\"",
            1,
        );
        std::fs::write(user.path().join("panels.toml"), bad).unwrap();
        let (services, bridge) = compose(cx, demo.path(), user.path());
        assert!(!services.roster.kinds().contains(&"vol_bad"));
        assert!(
            services
                .registry
                .get(&ActionId("tile::add_vol_bad".into()))
                .is_none(),
            "no add-tile row, so no picker entry"
        );
        let (window, mut vcx) = open(cx, services);
        let shell = shell_of(&window, &mut vcx);
        let (summary, named) = shell.read_with(&vcx, |s, cx| {
            let d = s.diagnostics().read(cx);
            (
                d.summary().to_string(),
                d.config
                    .iter()
                    .any(|d| d.path.as_deref() == Some("panels.vol_bad.dataset")),
            )
        });
        assert!(summary.contains("config 1 error"), "{summary}");
        assert!(named, "the diagnostics tile names the refused panel");
        bridge.handle.shutdown();
    }

    /// The likeliest real refusal: a user `[cvi]` holding only a title
    /// replaces the builtin whole and is refused. A saved `cvi` tile
    /// restores as the placeholder and its record is kept, so a fix and a
    /// restart bring it back.
    #[gpui::test]
    fn a_partial_cvi_override_is_refused_and_saved_cvi_tiles_keep_their_records(
        cx: &mut gpui::TestAppContext,
    ) {
        use geode_shell::tiling::TileId;
        let demo = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("panels.toml"),
            "config_version = 1\n[cvi]\ntitle = \"Mine\"\n",
        )
        .unwrap();
        write_session(user.path(), "cvi");
        let (services, bridge) = compose(cx, demo.path(), user.path());
        assert_eq!(
            services
                .composition_diagnostics
                .iter()
                .map(|d| d.path.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["panels.cvi.dataset"]
        );
        let path = services.session_path.clone().unwrap();
        let (window, mut vcx) = open(cx, services);
        let shell = shell_of(&window, &mut vcx);
        assert_eq!(
            shell.read_with(&vcx, |s, _| s.occupant_kind(TileId(1))),
            Some(geode_shell::module::placeholder::PLACEHOLDER_KIND)
        );
        shell.read_with(&vcx, |s, cx| s.save_session(cx));
        assert_eq!(
            session::load(&path).tiles.get(&1).map(|r| r.kind.as_str()),
            Some("cvi"),
            "the record is kept for a later restart"
        );
        bridge.handle.shutdown();
    }

    /// Editing `panels` paints `restart required` and changes nothing in
    /// the running shell: the open panel keeps its kind and its title, and
    /// a panel the edit adds gets no roster kind and no add-tile action.
    #[gpui::test]
    fn editing_panels_asks_for_a_restart_and_changes_no_open_panel(cx: &mut gpui::TestAppContext) {
        use geode_shell::actions::ActionId;
        use geode_shell::tiling::TileId;
        let demo = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(user.path().join("panels.toml"), WIDE_CVI_PANEL).unwrap();
        write_session(user.path(), "cvi_wide");
        let (services, bridge) = compose(cx, demo.path(), user.path());
        let builtin = services.builtin.clone();
        let (window, mut vcx) = open(cx, services);
        let shell = shell_of(&window, &mut vcx);
        let title = |vcx: &gpui::VisualTestContext| {
            shell.read_with(vcx, |s, cx| {
                s.occupant_title(TileId(1), cx).map(|t| t.to_string())
            })
        };
        assert_eq!(title(&vcx).as_deref(), Some("CVI (wide)"));
        assert_eq!(
            shell.read_with(&vcx, |s, cx| s
                .diagnostics()
                .read(cx)
                .restart_required
                .clone()),
            None,
            "a clean start asks for nothing"
        );
        std::fs::write(
            user.path().join("panels.toml"),
            format!(
                "{}\n{}",
                WIDE_CVI_PANEL.replace("CVI (wide)", "CVI (renamed)"),
                WIDE_CVI_PANEL
                    .replace("config_version = 1\n", "")
                    .replace("cvi_wide", "cvi_new")
            ),
        )
        .unwrap();
        let candidate = Config::load(&ConfigSources {
            builtin,
            desk: None,
            user: Some(user.path().to_path_buf()),
        });
        shell.update(&mut vcx, |s, cx| s.apply_reload_for_test(candidate, cx));
        vcx.run_until_parked();
        let restart = shell.read_with(&vcx, |s, cx| {
            s.diagnostics().read(cx).restart_required.clone()
        });
        assert!(
            restart.as_deref().is_some_and(|m| m.contains("panels")),
            "{restart:?}"
        );
        assert_eq!(
            shell.read_with(&vcx, |s, _| s.occupant_kind(TileId(1))),
            Some("cvi_wide")
        );
        assert_eq!(
            title(&vcx).as_deref(),
            Some("CVI (wide)"),
            "the open panel keeps its title, not the edited one"
        );
        let (kinds, add_row, startup_row) = shell.read_with(&vcx, |s, _| {
            let registry = &s.services().registry;
            (
                s.services().roster.kinds(),
                registry
                    .get(&ActionId("tile::add_cvi_new".into()))
                    .is_some(),
                registry
                    .get(&ActionId("tile::add_cvi_wide".into()))
                    .is_some(),
            )
        });
        assert!(startup_row, "the startup panel's add-tile action is there");
        assert!(kinds.contains(&"cvi_wide"), "{kinds:?}");
        assert!(!kinds.contains(&"cvi_new"), "{kinds:?}");
        assert!(!add_row, "no add-tile action for a panel added mid-session");
        bridge.handle.shutdown();
    }

    /// Production workspace source uses tracing so level filters and the
    /// ring/file/stderr sinks see its diagnostics. Debug prints are allowed only
    /// under tests directories or recognized inline test gates.
    #[test]
    fn no_eprintln_outside_tests_in_workspace_src() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir
            .parent()
            .and_then(|p| p.parent())
            .expect("geode-app sits at <workspace root>/crates/geode-app");
        let crates_dir = workspace_root.join("crates");

        let mut offenders = Vec::new();
        for entry in std::fs::read_dir(&crates_dir).expect("workspace crates/ dir exists") {
            let entry = entry.unwrap();
            if !entry.file_type().unwrap().is_dir() {
                continue;
            }
            let src = entry.path().join("src");
            if src.is_dir() {
                walk_rs_files(&src, &mut offenders);
            }
        }

        assert!(
            offenders.is_empty(),
            "eprintln! found outside a tests/ dir or an inline #[cfg(test)] module:\n{}",
            offenders.join("\n")
        );
    }

    /// A bodyless test module declaration must not hide subsequent production
    /// lines from the debug-print scan.
    #[test]
    fn walk_rs_files_does_not_get_stuck_after_a_bodyless_cfg_test_mod_declaration() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "#[cfg(test)]\nmod tests;\n\nfn oops() {\n    eprintln!(\"real\");\n}\n",
        )
        .unwrap();
        let mut offenders = Vec::new();
        walk_rs_files(dir.path(), &mut offenders);
        assert_eq!(
            offenders.len(),
            1,
            "the real call after the bodyless declaration must still be seen: {offenders:?}"
        );
    }

    /// Recognize cfg(any(test, ...)) blocks for any item kind, including
    /// functions as well as modules.
    #[test]
    fn walk_rs_files_recognises_a_cfg_any_test_support_gated_block_as_test_scope() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "#[cfg(any(test, feature = \"test-support\"))]\npub fn fixture() {\n    eprintln!(\"debug\");\n}\n",
        )
        .unwrap();
        let mut offenders = Vec::new();
        walk_rs_files(dir.path(), &mut offenders);
        assert!(
            offenders.is_empty(),
            "a cfg(any(test, ...)) item is test scope: {offenders:?}"
        );
    }

    /// Mentioning the macro name without invoking it must not count as a call.
    #[test]
    fn walk_rs_files_ignores_prose_mentioning_the_macro_without_calling_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "// replaces the old eprintln! prints\nfn f() {}\n",
        )
        .unwrap();
        let mut offenders = Vec::new();
        walk_rs_files(dir.path(), &mut offenders);
        assert!(
            offenders.is_empty(),
            "naming the macro without invoking it is not a call: {offenders:?}"
        );
    }

    /// Collect path/line locations of debug-print calls outside tests directories
    /// and blocks gated by cfg(test) or cfg(any(test, ...)). A gate applies to
    /// the next nonempty, non-comment line only when that line opens a block;
    /// bodyless declarations do not start a skipped region.
    ///
    /// This is a textual check, not a Rust parser. It counts braces even inside
    /// strings/comments and matches the macro's call spelling there too. Formatting
    /// and quoted examples can therefore affect its scope or produce false positives.
    fn walk_rs_files(dir: &std::path::Path, offenders: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("readable src subdirectory") {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                walk_rs_files(&path, offenders);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            if path.components().any(|c| c.as_os_str() == "tests") {
                continue; // a whole tests/ seam file, e.g. shell/tests/reload.rs
            }
            let text = std::fs::read_to_string(&path).unwrap_or_default();

            let mut depth: i32 = 0;
            let mut pending_cfg_gate = false;
            let mut skip_until: Option<i32> = None; // Some(depth before the gated item)
            for (i, line) in text.lines().enumerate() {
                let trimmed = line.trim();
                let skipping = skip_until.is_some();

                if !skipping
                    && (trimmed.starts_with("#[cfg(test)]")
                        || trimmed.starts_with("#[cfg(any(test,"))
                {
                    pending_cfg_gate = true;
                } else if !skipping
                    && pending_cfg_gate
                    && !trimmed.is_empty()
                    && !trimmed.starts_with("//")
                {
                    // The first real line after the gate: a block
                    // (`mod`/`fn`/`impl`/...) opening a brace here is
                    // what gets skipped; a semicolon-only declaration or
                    // anything else opens nothing.
                    if line.contains('{') {
                        skip_until = Some(depth);
                    }
                    pending_cfg_gate = false;
                }

                if !skipping && line.contains("eprintln!(") {
                    offenders.push(format!("{}:{}", path.display(), i + 1));
                }

                let opens = line.matches('{').count() as i32;
                let closes = line.matches('}').count() as i32;
                depth += opens - closes;
                if let Some(start) = skip_until
                    && depth <= start
                    && (opens > 0 || closes > 0)
                {
                    skip_until = None;
                }
            }
        }
    }
}
