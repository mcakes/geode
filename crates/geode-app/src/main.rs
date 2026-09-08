//! The Geode binary. Loads layered config (spec §8), builds the action
//! registry, keymap, and starting workspace state, then opens the window on
//! `geode_shell::shell::ShellView` — the keyboard-driven shell root.

mod bridge;
mod crash;
mod demo;

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use geode_blotter::BlotterFactory;
use geode_core::config::{Config, ConfigSources, Diagnostic, LayerDoc, Severity};
use geode_core::log::{LevelControl, LogLevels, Ring, RingLayer};
use geode_shell::actions::ActionRegistry;
use geode_shell::defaults::{
    BUILTIN_KEYMAP, mod_alias_from_config, register_builtin_actions, register_pick_actions,
    register_scope_actions,
};
use geode_shell::fonts;
use geode_shell::frame::Frame;
use geode_shell::keymap::build_keymap;
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
    let (log_ring, log_control) = install_logging();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let demo_rows = match parse_args(&args) {
        Ok(rows) => rows,
        Err(message) => {
            tracing::error!(target: "geode::config", "{message}");
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
        std::process::exit(1);
    }

    gpui_platform::application()
        .with_assets(gpui_component_assets::Assets)
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

            let (mut services, desk, user, bridge) =
                build_shell_services(demo_root.as_deref(), log_ring, log_control, cx);
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
            }

            // Best-effort flush on quit: `App::on_app_quit` exists at the
            // pinned gpui rev (checked against the vendored checkout), so
            // wire it up as a belt-and-suspenders save — the post-dispatch
            // save in `ShellView::dispatch` already covers crash-robustness
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

            cx.spawn(async move |cx| {
                // Task 4: the toolbar IS the native title bar
                // (`geode_shell::shell::toolbar`), so the window itself
                // must be opened with gpui-component's title-bar-owned
                // options (window controls, drag/double-click ownership) —
                // see the doc comment on `TitleBar::window_options` and the
                // `window_title` example at the pinned checkout.
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
fn install_logging() -> (Arc<Ring>, Arc<dyn LevelControl>) {
    let ring = Arc::new(Ring::new(4096));
    let (filter, reload_handle) = reload::Layer::new(LogLevels::default().to_targets());

    let (_, user) = config_dirs();
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
        Some(fmt::layer().with_writer(appender).with_ansi(false))
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

    (ring, Arc::new(ReloadControl(reload_handle)))
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
    fn create(
        &self,
        tile: TileId,
        restored: Option<&toml::Table>,
        frame: Entity<Frame>,
        window: &mut Window,
        cx: &mut App,
    ) -> TileOccupant {
        self.0.create(tile, restored, frame, window, cx)
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
fn build_shell_services(
    demo_root: Option<&Path>,
    log_ring: Arc<Ring>,
    log_control: Arc<dyn LevelControl>,
    cx: &mut App,
) -> (
    ShellServices,
    Option<PathBuf>,
    Option<PathBuf>,
    Option<bridge::Bridge>,
) {
    let (desk, user) = config_dirs();
    let mut builtin = vec![
        LayerDoc::builtin("keymap", BUILTIN_KEYMAP).expect("builtin keymap TOML is well-formed"),
    ];
    if let Some(root) = demo_root {
        builtin.extend(demo::layer(&root.join("src")));
    }
    let config = Config::load(&ConfigSources {
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
    let _ = log_control.set(&log_levels);

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

    // The default kind is read from `[app] modules.default`, "blotter"
    // when unset.
    let default_kind = config
        .get("app", "modules.default")
        .and_then(|v| v.as_str())
        .unwrap_or("blotter")
        .to_string();
    let mut roster = ModuleRoster::new(default_kind);

    // The data bridge (spec §5.1, §5.4): `None` when the config declares
    // no datasets/views, in which case the roster's only occupant is
    // whatever `default_kind` names with nothing behind it — a blotter
    // with no data handle would panic on its first requery, so a roster
    // with no bridge simply gets no blotter factory at all, and every
    // tile falls back to the placeholder.
    let db = bridge::db_path(
        &config,
        demo_root,
        std::env::var("LOCALAPPDATA").ok(),
        std::env::var("HOME").ok(),
    );
    let bridge = bridge::data_setup(&config, db).map(|setup| {
        let find_style = FindStyle::from_config(&config);
        let stale_after = bridge::stale_after_from_config(&config);
        let bridge = bridge::start(setup, find_style, stale_after, cx);
        roster.add(Box::new(BlotterFactoryHandle(bridge.factory.clone())));
        bridge
    });

    // Modules register their actions before the keymap builds (§3.2).
    roster.register_actions(&mut registry);

    let (mod_alias, mod_diags) = mod_alias_from_config(&config);
    for diag in &mod_diags {
        print_diagnostic(diag);
    }
    let (keymap, keymap_diags) = build_keymap(config.layered_docs("keymap"), mod_alias, &registry);
    for diag in &keymap_diags {
        print_diagnostic(diag);
    }

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
        registry,
        keymap,
        mod_alias,
        workspaces: Workspaces::new(),
        theme,
        session_path,
        roster,
        restored_tiles: std::collections::BTreeMap::new(),
        restored_frame: None,
        log: Some(LogServices {
            ring: log_ring,
            control: log_control,
            levels: log_levels,
        }),
    };
    (services, desk, user, bridge)
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

    /// Recurses into `dir` collecting `"<path>:<line>"` for every
    /// `eprintln!` call site outside a `tests/` path component or an
    /// inline `#[cfg(test)] mod ... { ... }` block (tracked by brace
    /// depth — a plain count of `{`/`}` per line, which is exact for
    /// this codebase's formatting: `cargo fmt` never puts a brace inside
    /// a string or comment on a line that also opens/closes a block
    /// relevant here).
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
            let mut pending_cfg_test = false;
            let mut cfg_test_skip_until: Option<i32> = None; // Some(depth) once inside
            for (i, line) in text.lines().enumerate() {
                let trimmed = line.trim();
                let skipping = cfg_test_skip_until.is_some();

                if !skipping && trimmed.starts_with("#[cfg(test)]") {
                    pending_cfg_test = true;
                } else if !skipping && pending_cfg_test && trimmed.starts_with("mod ") {
                    cfg_test_skip_until = Some(depth);
                    pending_cfg_test = false;
                } else if !trimmed.starts_with("//") {
                    pending_cfg_test = false;
                }

                if !skipping && line.contains("eprintln!") {
                    offenders.push(format!("{}:{}", path.display(), i + 1));
                }

                let opens = line.matches('{').count() as i32;
                let closes = line.matches('}').count() as i32;
                depth += opens - closes;
                if let Some(start) = cfg_test_skip_until
                    && depth <= start
                    && (opens > 0 || closes > 0)
                {
                    cfg_test_skip_until = None;
                }
            }
        }
    }
}
