//! The Geode binary. Loads layered config (spec §8), builds the action
//! registry, keymap, and starting workspace state, then opens the window on
//! `geode_shell::shell::ShellView` — the keyboard-driven shell root.

mod assets;
mod bridge;
mod crash;
mod demo;
mod demo_bus;
mod demo_series;

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use geode_blotter::BlotterFactory;
use geode_core::config::{ConfigSources, Diagnostic, LayerDoc, Severity};
use geode_core::log::{LevelControl, LogLevels, Ring, RingLayer};
use geode_diagnostics::DiagnosticsFactory;
use geode_marketdata::MarketDataFactory;
use geode_shell::actions::ActionRegistry;
use geode_shell::defaults::{
    BUILTIN_KEYMAP, mod_alias_from_config, modules_default_diagnostic, register_add_actions,
    register_builtin_actions, register_pick_actions, register_scope_actions,
};
use geode_shell::diagnostics::{ActionTail, Diagnostics};
use geode_shell::fonts;
use geode_shell::frame::Frame;
use geode_shell::keymap::build_keymap;
use geode_shell::keymap::fragments;
use geode_shell::module::{ModuleFactory, ModuleRoster, TileOccupant};
use geode_shell::session;
use geode_shell::shell::{LogServices, ShellServices, ShellView, pickable_columns, saved_scopes};
use geode_shell::theme;
use geode_shell::tiling::{TileId, Workspaces};
use geode_shell::vimfind::FindStyle;
use gpui::App;
use gpui::prelude::*;
use gpui::{Entity, Window};
use gpui_component::{Root, TitleBar};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Registry, fmt, reload};

fn main() {
    // First thing: the subscriber, before `Application::new` and before
    // config load — the config load itself should log (spec §4.1, Phase
    // 4b Task 2). `[log]`'s levels are applied through the returned
    // control once `build_shell_services` has a `Config` to read them
    // from.
    //
    // `_log_guard` MUST stay bound (never `let _ = ..`) for the rest of
    // `main` — see `install_logging`'s own doc comment for why dropping
    // it silently stops the file layer.
    let (log_ring, log_control, _log_guard) = install_logging();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let demo_rows = match parse_args(&args) {
        Ok(rows) => rows,
        Err(message) => {
            tracing::error!(target: "geode::config", "{message}");
            // NEW-2 (final review round 2): `process::exit` runs no
            // destructors, so `_log_guard` bound above would otherwise
            // never drop and `tracing_appender`'s non-blocking worker
            // thread would never flush — this line (and any log line
            // still in its channel) could be missing from `logs/geode.
            // YYYY-MM-DD.log` entirely, even though the appender was
            // sent it. `drop` here runs the guard's flush-then-exit
            // logic before the process actually ends; the stderr layer
            // and the ring are both synchronous already and unaffected
            // either way.
            drop(_log_guard);
            std::process::exit(2);
        }
    };

    // `--demo` (spec §7.1): emit the generator's sample data once per row
    // count, off the render thread's critical path — this runs before
    // `gpui_platform::application()` even opens a window, so there is no
    // frame yet to stall. `ensure_emitted` is idempotent: a warm demo
    // directory from an earlier run is reused rather than regenerated.
    let demo_root = demo_rows.map(demo::demo_dir);
    if let (Some(rows), Some(root)) = (demo_rows, &demo_root)
        && let Err(e) = demo::ensure_emitted(root, rows)
    {
        tracing::error!(target: "geode::ingest", "failed to emit sample data into {root:?}: {e}");
        // NEW-2: see the other `process::exit` call's own comment above.
        drop(_log_guard);
        std::process::exit(1);
    }

    gpui_platform::application()
        .with_assets(assets::AppAssets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx); // must run before any component use
            // Reclaim `tab`/`shift-tab` (from gpui-component's `Root` focus
            // cycling) and `ctrl-f` (from its editor `Search` action, which
            // otherwise swallows the list dialogs' and command palette's
            // "page down" on Windows and Linux) — see that function's own
            // doc comment for the full mechanism per key and why each is
            // scoped the way it is. Shared with
            // `shell::tests::dialog_test_shell` so the dialogs' own tests
            // prove the `tab` reclaim, rather than merely assuming it holds
            // here; `ctrl-f` has no such test (see the function's doc
            // comment).
            geode_shell::shell::dialog::init_reclaimed_keybindings(cx);

            fonts::register(cx); // bundled Inter/JetBrains Mono (Task 10) —
            // after init (installs the Theme global this edits), before the
            // window opens so the first frame already carries them.

            // Reclaim `DataTable`'s own key bindings while a tile has
            // gpui focus for one frame after a click (Phase 3 §3.3) — see
            // `geode_blotter::init`'s own doc comment.
            geode_blotter::init(cx);
            geode_diagnostics::init(cx);
            // The market-data panel's body is a `DataTable` too (user
            // ruling 2026-09-14), so it owes the same reclaim; binding the
            // same keys to `NoAction` twice is harmless.
            geode_marketdata::init(cx);

            // The demo bus's adapter (market-data-documents plan, Task
            // 10): registered only under `--demo`, since it is the
            // demo's own producer for the market-data path — a
            // non-demo build never creates a `ChannelAdapter` and
            // passes `AdapterRegistry::default()` into `data_setup`
            // instead, exactly as it always has. Built here, before
            // `build_shell_services`, because `bridge::data_setup`
            // (called from inside it) is what actually registers the
            // `[cvi]` source against this adapter.
            let (demo_feed, adapters) = if demo_rows.is_some() {
                let (adapter, feed) = geode_data::adapter::ChannelAdapter::new("demo_bus");
                let mut adapters = geode_data::adapter::AdapterRegistry::default();
                adapters.register(adapter);
                // The timeseries demo sources (timeseries spec §5.6): the
                // same seed as the risk generator, one with a catalogue
                // and one without.
                adapters.register(demo_series::DemoSeries::new("demo_kdb", 42, true));
                adapters.register(demo_series::DemoSeries::new("demo_rest", 42, false));
                (Some(feed), adapters)
            } else {
                (None, geode_data::adapter::AdapterRegistry::default())
            };

            // Every build has the mock (line-pricer spec §5.5); a vendor
            // crate, when one exists, registers itself here behind its
            // feature gate.
            let mut pricers = geode_data::PricerRegistry::default();
            pricers.register(Arc::new(geode_pricing::MockPricer::new()));

            let (mut services, desk, user, bridge, diagnostics_factory) = build_shell_services(
                demo_root.as_deref(),
                log_ring,
                log_control,
                adapters,
                pricers,
                cx,
            );

            // The demo bus itself (Task 10): spawned once, right after
            // the services it feeds exist. `geode_demo_data::
            // demo_underlyings()` is the risk generator's own
            // vocabulary, so the CVI documents this bus publishes never
            // drift from the desk names `--demo`'s risk snapshot already
            // uses. Kept alive as `demo_bus` until the app quits (below)
            // — dropping it early would stop the thread and close its
            // `ChannelFeed`, which would in turn close the bus's inbound
            // channel out from under the data service's own subscription.
            let mut demo_bus = demo_feed.map(|feed| {
                let generator = geode_demo_data::documents::cvi::CviGenerator::new(
                    42,
                    geode_demo_data::demo_underlyings(),
                    chrono::Local::now().date_naive(),
                );
                demo_bus::spawn(
                    feed,
                    Arc::new(geode_documents::CviKind),
                    generator,
                    Duration::from_secs(5),
                    Duration::from_secs(2),
                    42,
                )
            });

            // The panic hook (Phase 4b Task 6), installed after the
            // subscriber (`install_logging`, at the very top of `main`)
            // and once `services` exists, since it needs `registry.
            // hash_names()` — a clone of the registry's shared hash → id
            // map (fix round 1, MIN-1: `register_pick_actions`/
            // `register_scope_actions` both run once each, here inside
            // `build_shell_services`, *before* this hook is installed —
            // there is no reload-time registration for the clone to
            // outlive; `hash_names()` is used because `ActionRegistry`
            // itself is not `Send`/`Sync` and cannot be captured by a
            // `'static` hook closure directly, not because the map
            // changes after this point). `log` is always `Some` on this
            // path (`install_logging` always builds a `Ring`); the
            // `if let` mirrors every other "missing = skipped, never a
            // panic" spot in this file rather than assuming it.
            if let Some(log) = &services.log {
                let names_snapshot = services.registry.hash_names();
                let names: Arc<dyn Fn(u64) -> Option<String> + Send + Sync> = Arc::new(move |h| {
                    // Fix round 1, MIN-2: `try_read`, not a blocking
                    // `read()` — see `crash.rs`'s own MIN-2 note on the
                    // action tail's `try_lock` for why a hook must never
                    // block on a lock the panicking thread itself might
                    // already hold. `register` only ever *writes* this
                    // map from the UI thread before the hook exists, so
                    // contention here would mean the panicking thread is
                    // the UI thread having panicked mid-`register` — the
                    // `None` fallback (rendered as `<unknown action ..>`
                    // by the hook) degrades gracefully rather than
                    // risking a self-deadlock.
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

            // Restore the session (Task 3) before constructing ShellView:
            // the saved workspace layout replaces the fresh `Workspaces::
            // new()` set above. Pure layout state now — theme choices are
            // ordinary config (`[theme]` in `app.toml`, written by
            // `ShellView::persist_theme` via `theme::persist_to_user_config`)
            // resolved by `apply_from_config` above like everything else, so
            // there is no session-side theme re-application step to run
            // here any more.
            if let Some(path) = &services.session_path {
                let restored = session::load(path);
                for warning in &restored.warnings {
                    tracing::warn!(target: "geode::session", "{warning}");
                }
                services.workspaces = restored.workspaces;
                services.restored_tiles = restored.tiles;
                services.restored_frame = restored.frame;
                services.restored_palette_usage = restored.palette_usage;
            }

            // Best-effort flush on quit: `App::on_app_quit` exists at the
            // pinned gpui rev (checked against the pinned release,
            // `gpui-pre-0.3.5/src/app.rs`), so wire it up as a
            // belt-and-suspenders save — the post-dispatch save in
            // `ShellView::dispatch` already covers crash-robustness
            // for every workspace-mutating action; this only additionally
            // catches a workspace mutation made just before quitting, ahead
            // of the background watcher's next ~500ms flush. (A theme
            // change persists synchronously the moment it applies —
            // `ShellView::persist_theme` — so it needs no quit-time flush
            // of its own any more.)
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

            // The data service's shutdown joins its own thread, which may
            // wait out an in-flight ingest or discovery scan (`DataHandle::
            // shutdown`'s own doc comment) — so it must run off the UI
            // thread, never as a side effect of a `DataHandle` simply
            // dropping on `main`'s own thread at quit.
            if let Some(bridge) = &bridge {
                let handle = bridge.handle.clone();
                cx.on_app_quit(move |cx| {
                    let handle = handle.clone();
                    cx.background_executor().spawn(async move {
                        handle.shutdown();
                    })
                })
                .detach();
            }

            // The demo bus's own shutdown (Task 10): stopped before
            // `_log_guard` drops at the end of `main` — off the UI
            // thread, for the same reason the data service's shutdown
            // above is, even though `DemoBus::stop` itself only ever
            // waits out `STOP_POLL`-sized slices rather than a long
            // cadence. `Arc<Mutex<Option<..>>>` (not a plain move) is
            // what lets this run inside an `FnMut`: `on_app_quit`'s
            // closure type must support being called more than once
            // even though a real quit fires it only the once, and
            // `DemoBus` cannot be cloned to give each hypothetical call
            // its own copy the way the bridge's `DataHandle` clone
            // above does — `take()` makes a second call a no-op instead.
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
                // Task 4: the toolbar IS the native title bar
                // (`geode_shell::shell::toolbar`), so the window itself
                // must be opened with gpui-component's title-bar-owned
                // options (window controls, drag/double-click ownership) —
                // see `TitleBar::window_options`'s own doc comment
                // (`gpui-component-0.6.2/src/title_bar.rs`), which carries
                // the same worked example upstream's `examples/window_title`
                // does.
                let window = cx
                    .open_window(TitleBar::window_options(), |window, cx| {
                        let view = cx.new(|cx| ShellView::new(services, desk, user, window, cx));
                        cx.new(|cx| Root::new(view, window, cx))
                    })
                    .expect("failed to open window");

                // Wire the data bridge to the shell that just opened
                // (Phase 3 §5.1): route query outcomes and health/publish
                // events in, forward config reloads back out.
                if let Some(bridge) = &bridge {
                    cx.update(|cx| bridge::attach(bridge, window, cx));
                }

                // The diagnostics factory's config refresh (Phase 4b Task
                // 5, fix round 1 MAJ-3): independent of the data bridge
                // above — this module needs no `DataHandle`, so it
                // subscribes for itself rather than piggybacking on
                // `bridge::attach`, which does not run at all when no
                // `[sources]`/`[datasets]` are configured. `ShellEvent::
                // ConfigReloaded` (the original wiring) only fires when a
                // reload changed `views`/`dimensions` — most reloads
                // (`[log]`, `[theme]`, `keymap.toml`, ...) never touch it,
                // so the config section's explainer went stale on exactly
                // the reload `:level`'s own persist causes. `Frame::
                // versions().config` now bumps on every *applied* reload
                // (`hot_reload::apply_reload`'s `note_config_reloaded`
                // call moved out from under the `views_changed` gate), so
                // observing the frame directly and comparing that counter
                // is the ungated signal this refresh actually needs — the
                // same counter `DiagnosticsTile`'s own `cx.observe(&frame,
                // ..)` narrows to (`config` + `as_of` only, fix round 1
                // MAJ-6). `last_config_version` is a `Cell` (not a plain
                // local) so the `FnMut` the observer boxes can update it
                // between calls — same shape as `bridge.rs`'s own
                // `catalog_tag: Rc<Cell<u64>>`. Guarded so a frame notify
                // for an unrelated reason (a scope keystroke, an as-of
                // change) does not re-clone `Config` for nothing.
                cx.update(|cx| {
                    let shell = window
                        .read(cx)
                        .ok()
                        .and_then(|root| root.view().clone().downcast::<ShellView>().ok())
                        .expect("the window's root view is the shell");
                    let frame = shell.read(cx).frame().clone();
                    let last_config_version =
                        std::rc::Rc::new(std::cell::Cell::new(frame.read(cx).versions().config));
                    // MIN-7 (final review): registered here, before any
                    // diagnostics tile can exist to register its own
                    // `cx.observe(&frame, ..)` — gpui invokes an entity's
                    // observers in registration order, so this refresh of
                    // `diagnostics_factory`'s shared `Rc<RefCell<Config>>`
                    // is guaranteed to run before `DiagnosticsTile`'s own
                    // frame observer (`geode-diagnostics/src/tile.rs`)
                    // rebuilds the config section FROM that same `Config`
                    // on the SAME `config` version bump. That ordering is
                    // load-bearing and not stated anywhere gpui enforces
                    // it structurally — if a future refactor ever let a
                    // tile's observer register before this one (a second
                    // window, a different construction order), the tile
                    // would rebuild from the previous `Config` and never
                    // rebuild again, since its own version is already
                    // consumed by the time this one updates.
                    cx.observe(&frame, move |frame, cx| {
                        let now = frame.read(cx).versions().config;
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

/// Installs the process-wide `tracing` subscriber (Phase 4b Task 2, spec
/// §4.1–4.3), before anything else in `main` runs: a level filter behind
/// a `reload::Layer` (starting at [`LogLevels::default`] — `[log]`'s real
/// levels apply once `build_shell_services` has loaded a `Config`), a
/// plain stderr `fmt` layer (replaces the old per-call-site stderr prints), the ring
/// every subscriber layer feeds (read later by the diagnostics tile),
/// and — only when the user config dir exists — a daily rolling file
/// under `<user>/logs/geode.YYYY-MM-DD.log`. `None` (no home) means no
/// file layer, exactly as `ShellServices::session_path` has no session
/// file: logging degrades to stderr-and-ring only, never a panic.
///
/// MIN-7 (fix round 1): that date is `tracing-appender`'s own clock,
/// which is UTC (`OffsetDateTime::now_utc`) — unlike every *displayed*
/// time in this app (Phase 4a's ruling: "times are the trader's local
/// clock throughout"), the log file's name is not local. West of UTC,
/// `geode.2026-09-08.log` can hold the evening of the 7th, local.
/// `trim_log_files`'s seven-file cap still sorts and counts correctly
/// (the names are still in age order relative to each other), only the
/// name's meaning is off — recorded here rather than fixed, since fixing
/// it means hand-rolling the rotation `tracing-appender`'s `Builder`
/// does not expose a local-clock option for.
///
/// Returns the file layer's [`tracing_appender::non_blocking::WorkerGuard`]
/// (MIN-8, final review), `None` when there is no user dir to log into.
/// **The caller MUST hold this for the life of the process — binding it
/// with a leading underscore (`let _log_guard = ..`), never discarding
/// it (`let _ = ..`) or letting the return value's temporary drop at the
/// end of the call statement.** Dropping a `WorkerGuard` stops that
/// background writer thread: `NonBlocking::write` after that silently
/// drops every further log line bound for the file (the channel's
/// receiver is gone, and `NonBlocking` fails open rather than blocking
/// or panicking) rather than erroring. This was a plain, synchronous
/// `RollingFileAppender` before this fix — every `tracing::*!` call that
/// reached the file layer blocked the calling thread on that write.
/// `non_blocking` moves the write onto a dedicated worker thread instead
/// (PHILOSOPHY.md: "nothing may stall the render thread" — a UI-thread
/// `warn`, the Global Constraint's one permitted level there, used to do
/// synchronous file I/O inline on a paint).
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
        // MIN-8 (final review): `non_blocking` hands the write to a
        // background thread rather than doing it on the calling thread
        // (see this function's own doc for the trade this makes and the
        // `WorkerGuard` contract). `crash.rs`'s panic hook used to rely
        // on the OLD blocking behaviour to guarantee every earlier log
        // line was already on disk by the time a panic reached it —
        // updated there too: the crash file itself is built from the
        // in-memory ring, not the file layer, so it is unaffected; only
        // the on-disk daily log can now lag a panic by a buffered batch,
        // an accepted trade documented at that call site.
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

/// `--demo [rows]` (default 100,000), or no arguments at all (spec §7.1).
/// Anything else is a usage error the caller should exit(2) on — argument
/// parsing has no config to fall back to, unlike a bad `*.toml`, so this
/// is the one place invalid input does not just degrade and continue.
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

/// Wraps the bridge's shared `Rc<BlotterFactory>` so it can go in the
/// roster, which wants an owned `Box<dyn ModuleFactory>` (§9.1) — the
/// factory itself has to stay an `Rc` because `bridge::attach`'s reload
/// handler also holds a clone, for `set_views`/`set_find_style`/
/// `set_stale_after` on every `ConfigReloaded` (spec §5.1). A thin
/// forwarding wrapper here, rather than `impl ModuleFactory for
/// Rc<BlotterFactory>` in `geode-blotter` itself, keeps that crate's
/// public surface exactly the one `ModuleFactory for BlotterFactory` impl
/// it already has.
struct BlotterFactoryHandle(Rc<BlotterFactory>);

impl ModuleFactory for BlotterFactoryHandle {
    fn kind(&self) -> &'static str {
        self.0.kind()
    }
    fn register_actions(&self, registry: &mut ActionRegistry) {
        self.0.register_actions(registry)
    }
    // Forwarded like everything else on this trait: a defaulted method
    // NOT forwarded here would silently answer for the wrapper (no
    // fragment, contexts = the kind) instead of for the factory it
    // wraps, and the blotter's whole keymap would vanish with no
    // diagnostic anywhere.
    fn contexts(&self) -> Vec<&'static str> {
        self.0.contexts()
    }
    fn default_keymap(&self) -> Option<&'static str> {
        self.0.default_keymap()
    }
    fn create(
        &self,
        tile: TileId,
        restored: Option<&toml::Table>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
        window: &mut Window,
        cx: &mut App,
    ) -> TileOccupant {
        self.0
            .create(tile, restored, frame, diagnostics, window, cx)
    }
}

/// Same shape as [`BlotterFactoryHandle`], for the market-data panel's
/// factory (market-data spec §8.1): the bridge's reload handler holds a
/// clone for `set_stale_after`, so the roster gets a forwarder rather
/// than the factory itself.
///
/// **Every defaulted trait method is forwarded**, for the reason
/// [`BlotterFactoryHandle::contexts`] gives — and it bites harder here
/// than anywhere else: this factory's kind (`cvi`) and its key context
/// (`marketdata`) are DIFFERENT words, so a wrapper answering the
/// trait's default would declare the context `cvi`, every one of the
/// fragment's bindings would be dropped with an error diagnostic, and
/// the panel would have no keys at all.
struct MarketDataFactoryHandle(Rc<MarketDataFactory>);

impl ModuleFactory for MarketDataFactoryHandle {
    fn kind(&self) -> &'static str {
        self.0.kind()
    }
    fn register_actions(&self, registry: &mut ActionRegistry) {
        self.0.register_actions(registry)
    }
    fn contexts(&self) -> Vec<&'static str> {
        self.0.contexts()
    }
    fn default_keymap(&self) -> Option<&'static str> {
        self.0.default_keymap()
    }
    fn create(
        &self,
        tile: TileId,
        restored: Option<&toml::Table>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
        window: &mut Window,
        cx: &mut App,
    ) -> TileOccupant {
        self.0
            .create(tile, restored, frame, diagnostics, window, cx)
    }
}

/// Same shape as [`BlotterFactoryHandle`], for the diagnostics factory:
/// `main`'s config-reload subscription (set up once a window exists, in
/// the `cx.spawn` block below) also holds a clone, for `set_config`.
struct DiagnosticsFactoryHandle(Rc<geode_diagnostics::DiagnosticsFactory>);

impl ModuleFactory for DiagnosticsFactoryHandle {
    fn kind(&self) -> &'static str {
        self.0.kind()
    }
    fn register_actions(&self, registry: &mut ActionRegistry) {
        self.0.register_actions(registry)
    }
    // Forwarded for the reason [`BlotterFactoryHandle::contexts`] gives.
    fn contexts(&self) -> Vec<&'static str> {
        self.0.contexts()
    }
    fn default_keymap(&self) -> Option<&'static str> {
        self.0.default_keymap()
    }
    fn create(
        &self,
        tile: TileId,
        restored: Option<&toml::Table>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
        window: &mut Window,
        cx: &mut App,
    ) -> TileOccupant {
        self.0
            .create(tile, restored, frame, diagnostics, window, cx)
    }
}

/// Load config, register the shell's and modules' builtin actions,
/// compile the keymap, and build the starting (empty, workspace 1)
/// workspace state. Config and keymap diagnostics log at `geode::config`
/// (spec §10.1: invalid config must never stop the app from starting —
/// error/warn by [`Diagnostic::severity`], never a panic). Returns the
/// desk/user config directories alongside the services so the caller can
/// pass the same two directories into `ShellView::new` for the config
/// hot-reload watcher (Task 1c-1) — one `config_dirs()` call, one source
/// of truth for what's watched — and the data bridge (`None` when the
/// config has no datasets/views to serve), so the caller can attach it
/// to the window once it opens.
///
/// `demo_root` is `Some` under `--demo` (spec §7.1): the directory
/// `demo::ensure_emitted` has already populated with generated CSVs, one
/// level above its `src` subdirectory. Its config layer goes in ahead of
/// desk/user config, exactly where the builtin keymap already sits, so a
/// desk or user layer can still override any of it.
///
/// `log_ring`/`log_control` are `install_logging`'s output (Phase 4b
/// Task 2), threaded through here rather than reinstalled: `[log]` in
/// the just-loaded `config` is applied through `log_control` the moment
/// it's parsed, so it takes effect for everything logged after this
/// call, and both go onto the returned `ShellServices` for the
/// diagnostics tile and `:level` (later tasks) to reach.
///
/// `adapters` (market-data-documents plan, Task 10) is the caller's own
/// adapter roster — the `ChannelAdapter` registered under `--demo`, or
/// `AdapterRegistry::default()` otherwise — forwarded verbatim into
/// `bridge::data_setup`.
fn build_shell_services(
    demo_root: Option<&Path>,
    log_ring: Arc<Ring>,
    log_control: Arc<dyn LevelControl>,
    adapters: geode_data::adapter::AdapterRegistry,
    pricers: geode_data::PricerRegistry,
    cx: &mut App,
) -> (
    ShellServices,
    Option<PathBuf>,
    Option<PathBuf>,
    Option<bridge::Bridge>,
    Rc<DiagnosticsFactory>,
) {
    let (desk, user) = config_dirs();
    let mut builtin = vec![
        LayerDoc::builtin("keymap", BUILTIN_KEYMAP).expect("builtin keymap TOML is well-formed"),
    ];
    if let Some(root) = demo_root {
        builtin.extend(demo::layer(&root.join("src")));
    }
    // `ShellServices::config_and_builtin` derives `config` and `builtin`
    // from one `ConfigSources`, so the two cannot disagree (see that
    // function's doc comment — reconstructing `builtin` separately from
    // what `config` was loaded from silently deleted the demo desk on the
    // first config write of a session).
    let (config, builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin,
        desk: desk.clone(),
        user: user.clone(),
    });
    for diag in &config.diagnostics {
        print_diagnostic(diag);
    }

    // `[log]` (spec §4.2): parsed and applied through the reload control
    // now that a `Config` exists, so it governs everything logged for
    // the rest of startup and the run — the subscriber itself was
    // already up (`install_logging`, before config load) at the
    // conservative default so nothing logged before this point was lost.
    let (log_levels, log_diags) = LogLevels::from_doc(&config);
    for diag in &log_diags {
        print_diagnostic(diag);
    }
    // MIN-10 (fix round 1): unreachable at startup today (only a
    // poisoned or dropped reload handle can fail this, and neither
    // happens between `install_logging` and here) but `LevelControl::
    // set` is the same door `:level` (a later task) reloads through at
    // runtime, where a failure is real and swallowing it would be wrong
    // — so it's never discarded, even here.
    if let Err(e) = log_control.set(&log_levels) {
        tracing::warn!(target: "geode::config", "failed to apply [log]: {e}");
    }

    let mut registry = ActionRegistry::default();
    register_builtin_actions(&mut registry);
    // The dimension pickers' per-column actions (Phase 4a §3.3): from the
    // loaded schema, right after the shell's own builtins and before the
    // keymap builds — `register_pick_actions`' own doc comment has the
    // full ordering rationale.
    register_pick_actions(&mut registry, &pickable_columns(&config));
    // One `scope::<name>` action per saved scope (Phase 4a §3.11), same
    // ordering rationale as `register_pick_actions` just above — a scope
    // added by a live reload is not registered until restart (spec §1.3).
    // `false` (Phase 4b Task 1 fix round 1, MIN-8): `ShellView::new`'s own
    // load of the same doc, right after this, is the one startup caller
    // that reports diagnostics — this one printing too would show a
    // malformed `scopes.toml` entry twice on every launch.
    register_scope_actions(&mut registry, &saved_scopes(&config, false));

    // No default kind (spec 2026-09-08 add-tile §7.1): a tile is added by
    // naming the kind it hosts, and a tile nothing claims paints the
    // placeholder. `[app] modules.default` is no longer read at all — a
    // layer that still sets it gets the warning printed below.
    let mut roster = ModuleRoster::new();

    // The diagnostics module (Phase 4b Task 5, spec §4.6): registered
    // unconditionally, unlike the blotter factory just below — it needs
    // no data handle, so the palette's `Diagnostics: Split` row (or the
    // status bar's diagnostics-summary click) opens a tile even with no
    // `[sources]`/`[datasets]` configured at all. `diagnostics_factory` is
    // returned to the caller so it can subscribe to `ShellEvent::
    // ConfigReloaded` once a window (and so a `ShellView` to subscribe to)
    // exists — `set_config` refreshes the config section's explainer the
    // same way `BlotterFactory::set_views`/`set_schema` refresh theirs.
    let diagnostics_factory = Rc::new(DiagnosticsFactory::new(log_ring.clone(), config.clone()));
    roster.add(Box::new(DiagnosticsFactoryHandle(
        diagnostics_factory.clone(),
    )));

    // The data bridge (spec §5.1, §5.4): `None` when the config declares
    // no datasets/views. A blotter with no data handle would panic on its
    // first requery, so a roster with no bridge simply gets no blotter
    // factory at all — the palette then lists no "Blotter: Split" row,
    // and a tile nothing else claims paints the placeholder (2026-09-08
    // add-tile §7.1).
    let db = bridge::db_path(
        &config,
        demo_root,
        std::env::var("LOCALAPPDATA").ok(),
        std::env::var("HOME").ok(),
    );
    let bridge = bridge::data_setup(&config, db, adapters, pricers).map(|setup| {
        let find_style = FindStyle::from_config(&config);
        let stale_after = bridge::stale_after_from_config(&config);
        let bridge = bridge::start(setup, find_style, stale_after, cx);
        roster.add(Box::new(BlotterFactoryHandle(bridge.factory.clone())));
        // The market-data panel (spec §8.1), registered on the same
        // condition and for the same reason as the blotter: it asks for
        // its document through the bridge's `DataHandle`, so with no
        // bridge there is nothing for it to ask, and the palette then
        // lists no "CVI: Split" row either.
        roster.add(Box::new(MarketDataFactoryHandle(bridge.marketdata.clone())));
        bridge
    });

    // One "<Kind>: Split" palette row (plus the Horizontal/Vertical
    // pair) per registered kind (spec 2026-09-08 add-tile §3.2, retitled
    // by user ruling 2026-09-09), from the roster as it finally stands —
    // so a build with no data bridge lists no "Blotter: Split". Before
    // `build_keymap`, like every other registration in this function.
    register_add_actions(&mut registry, &roster.kinds());
    // Modules register their actions before the keymap builds (§3.2).
    roster.register_actions(&mut registry);

    // These two loops LOG; they do not seed the diagnostics entity.
    // `ShellView::new` recomputes both from `services.config` and folds
    // them into its own `note_config` call, in `apply_reload`'s order —
    // neither diagnostic lives in `config.diagnostics`, so seeding from
    // that list alone used to leave both out of the diagnostics tile
    // until the first hot reload happened to add them.
    let (mod_alias, mod_diags) = mod_alias_from_config(&config);
    for diag in &mod_diags {
        print_diagnostic(diag);
    }
    // `[app] modules.default` is retired (§7.1) — one warning so the key
    // does not silently rot in a desk file.
    if let Some(diag) = modules_default_diagnostic(&config) {
        print_diagnostic(&diag);
    }
    // Each module's own default bindings (market-data documents §8.4),
    // spliced above the compiled-in keymap and below every desk/user
    // layer — the roster is final by here (the bridge's blotter factory
    // was added above), so this is the first point every fragment
    // exists. Its diagnostics join `keymap_diags` below rather than
    // living in `config.diagnostics`: like the keymap's own they are
    // resolved against this registry and this roster, neither of which
    // `ShellView::new` can reconstruct.
    let (fragments, frag_diags) = roster.keymap_fragments();
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

    // Session file lives alongside user config (spec: state-as-config),
    // `user_config_dir()/session.toml` — `None` whenever there's no
    // writable user dir (mirrors desk/user themselves being optional).
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
        restored_palette_usage: geode_shell::palette_usage::PaletteUsage::new(),
        log: Some(LogServices {
            ring: log_ring,
            control: log_control,
            levels: log_levels,
        }),
        // Phase 4b Task 6: the shared handle the crash hook resolves
        // through, installed once `main`'s `run` closure has this
        // `ShellServices` back (`install_panic_hook` needs `registry.
        // hash_names()`, which only exists once `registry` — moved into
        // `services` above — is built).
        action_tail: Arc::new(Mutex::new(ActionTail::new())),
        // What `build_keymap` reported above. Printed already; carried
        // here because `ShellView::new` cannot recompute it (it needs
        // this registry, not just the config) and the diagnostics tile's
        // config section would otherwise miss it until a hot reload.
        keymap_diagnostics: keymap_diags,
        // The checked fragments themselves, so `apply_reload` can splice
        // the same ones back in at every hot reload — see the field's own
        // doc comment for why they are carried rather than recomputed.
        keymap_fragments: fragments,
        keymap_fragment_diagnostics: frag_diags,
    };
    (services, desk, user, bridge, diagnostics_factory)
}

/// A config or keymap diagnostic (spec §10.1): logged at `geode::config`,
/// error or warn by [`Diagnostic::severity`] — the message formatted as
/// it always has been (`Diagnostic`'s own `Display`), minus the
/// `[source]` prefix this used to carry: the target now says where it
/// came from.
fn print_diagnostic(diag: &Diagnostic) {
    match diag.severity {
        Severity::Warning => tracing::warn!(target: "geode::config", "{diag}"),
        Severity::Error => tracing::error!(target: "geode::config", "{diag}"),
    }
}

/// Desk and user config directories (spec §8): desk comes from
/// `GEODE_DESK_CONFIG` if set; user is a platform-appropriate per-user
/// config directory. No `dirs` crate — see the workspace invariant in
/// CLAUDE.md — so this hand-rolls the two cases that matter: Windows'
/// `%APPDATA%`, and everything else's `$HOME/.config`.
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

    /// Phase 4b Task 2's migration invariant, kept true rather than
    /// merely checked once: every `crates/*/src/**/*.rs` file outside a
    /// `tests/` directory or an inline `#[cfg(test)] mod ... { ... }`
    /// block is `eprintln!`-free — every real call site now goes through
    /// `tracing` with a target and a level, so the ring/file/stderr
    /// layers see everything. A test-only debug print inside an inline
    /// `#[cfg(test)]` module (unlike `shell/tests/reload.rs`'s own,
    /// which sits in a whole `tests/` directory and so is already
    /// excluded) is deliberately still allowed.
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

    /// MIN-5 (fix round 1), blind spot 1: `#[cfg(test)] mod tests;` (no
    /// body — the real one lives under a `tests/` directory) must not
    /// swallow everything for the rest of the file.
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

    /// MIN-5, blind spot 2: `#[cfg(any(test, feature = "test-support"))]`
    /// gating a block is test scope too, the same as a plain
    /// `#[cfg(test)]` — and the gated item need not be a `mod`.
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

    /// MIN-5, blind spot 3: mentioning the macro's name in prose,
    /// uninvoked, is not a call — narrowed from a bare `eprintln!`
    /// substring match to the call shape `eprintln!(`.
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

    /// Recurses into `dir` collecting `"<path>:<line>"` for every
    /// `eprintln!(` call site outside a `tests/` path component or an
    /// inline test-support block gated `#[cfg(test)]` or
    /// `#[cfg(any(test, ...))]` (tracked by brace depth — a plain count
    /// of `{`/`}` per line, which is exact for this codebase's
    /// formatting: `cargo fmt` never puts a brace inside a string or
    /// comment on a line that also opens/closes a block relevant here).
    ///
    /// Fix round 1 (MIN-5) closed three blind spots the first version
    /// had:
    /// - A gate followed by a semicolon-terminated declaration with no
    ///   body of its own (`#[cfg(test)] mod tests;` — the real body is
    ///   `mod.rs`'s own `tests/` directory, already excluded by path)
    ///   used to be treated as "entered a skip block" with nothing ever
    ///   bringing the brace depth back down to end it, silently
    ///   swallowing everything after it in the file. Now: a gate is only
    ///   "entered" when the very next real line actually opens a brace.
    /// - The gated item no longer has to be a `mod`: `#[cfg(any(test,
    ///   feature = "test-support"))] pub mod test_support { ... }`
    ///   (`geode_core::config`) and the same gate on a bare `pub fn`/
    ///   `impl` block (`geode_data::handle::DataHandle::for_tests`,
    ///   `geode_shell::module::recording`) are recognised the same way
    ///   `#[cfg(test)] mod tests { ... }` always was — any line that
    ///   opens a brace right after either gate starts a skipped block.
    /// - The eprintln! match narrowed from a bare substring to
    ///   `eprintln!(` (the call shape), so prose mentioning the macro by
    ///   name without invoking it (a comment reads "replaces the old
    ///   `eprintln!` call") no longer trips the check — this only
    ///   narrows the false-positive surface, it doesn't eliminate every
    ///   one (a backticked code example quoting a full call would still
    ///   match, same as a real call would).
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
