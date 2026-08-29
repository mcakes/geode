//! The Geode binary. Loads layered config (spec §8), builds the action
//! registry, keymap, and starting workspace state, then opens the window on
//! `geode_shell::shell::ShellView` — the keyboard-driven shell root.

use std::path::PathBuf;

use geode_core::config::{Config, ConfigSources, Diagnostic, LayerDoc, Severity};
use geode_shell::actions::ActionRegistry;
use geode_shell::defaults::{BUILTIN_KEYMAP, mod_alias_from_config, register_builtin_actions};
use geode_shell::keymap::build_keymap;
use geode_shell::session;
use geode_shell::shell::{ShellServices, ShellView};
use geode_shell::theme;
use geode_shell::tiling::Workspaces;
use gpui::prelude::*;
use gpui::{App, WindowOptions};
use gpui_component::Root;

fn main() {
    gpui_platform::application()
        .with_assets(gpui_component_assets::Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx); // must run before any component use

            let (mut services, desk, user) = build_shell_services();
            for warning in services.theme.apply_from_config(&services.config, cx) {
                eprintln!("[theme] warning: {warning}");
            }

            // Restore the session (Task 3) before constructing ShellView:
            // the saved workspace layout replaces the fresh `Workspaces::
            // new()` set above, and — deliberately AFTER apply_from_config
            // just ran — a saved theme mode is re-applied on top of it, so
            // a session's toggled mode wins over the config's default
            // mode (a runtime `theme::toggle_mode` survives a restart even
            // if `app.toml`'s `[theme].mode` still says the old value).
            if let Some(path) = &services.session_path {
                let (workspaces, extra, warnings) = session::load(path);
                for warning in &warnings {
                    eprintln!("[session] warning: {warning}");
                }
                services.workspaces = workspaces;
                if let Some(mode) = extra.theme_mode.as_deref() {
                    match mode {
                        "light" => services.theme.set_mode(theme::Mode::Light, cx),
                        "dark" => services.theme.set_mode(theme::Mode::Dark, cx),
                        other => {
                            eprintln!("[session] warning: unknown theme_mode '{other}'; ignoring")
                        }
                    }
                }
            }

            // Best-effort flush on quit: `App::on_app_quit` exists at the
            // pinned gpui rev (checked against the vendored checkout), so
            // wire it up as a belt-and-suspenders save — the post-dispatch
            // save in `ShellView::dispatch` already covers crash-robustness
            // for every workspace-mutating action; this only additionally
            // catches a theme-mode-only change (`theme::toggle_mode`, which
            // is not itself a workspace-mutating dispatch) made just before
            // quitting with no workspace action after it.
            cx.on_app_quit(|cx| {
                for window in cx.windows() {
                    if let Some(handle) = window.downcast::<Root>() {
                        let _ = handle.update(cx, |root, _window, cx| {
                            if let Ok(shell) = root.view().clone().downcast::<ShellView>() {
                                shell.read(cx).save_session();
                            }
                        });
                    }
                }
                async {}
            })
            .detach();

            cx.spawn(async move |cx| {
                cx.open_window(WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(services, desk, user, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
                .expect("failed to open window");
            })
            .detach();
        });
}

/// Load config, register the shell's builtin actions, compile the keymap,
/// and build the starting (empty, workspace 1) workspace state. Config and
/// keymap diagnostics print to stderr, one line each — a diagnostics UI is
/// a later phase; invalid config must never stop the app from starting
/// (spec §10.1). Returns the desk/user config directories alongside the
/// services so the caller can pass the same two directories into
/// `ShellView::new` for the config hot-reload watcher (Task 1c-1) — one
/// `config_dirs()` call, one source of truth for what's watched.
fn build_shell_services() -> (ShellServices, Option<PathBuf>, Option<PathBuf>) {
    let (desk, user) = config_dirs();
    let builtin_keymap =
        LayerDoc::builtin("keymap", BUILTIN_KEYMAP).expect("builtin keymap TOML is well-formed");
    let config = Config::load(&ConfigSources {
        builtin: vec![builtin_keymap],
        desk: desk.clone(),
        user: user.clone(),
    });
    for diag in &config.diagnostics {
        print_diagnostic("config", diag);
    }

    let mut registry = ActionRegistry::default();
    register_builtin_actions(&mut registry);

    let mod_alias = mod_alias_from_config(&config);
    let (keymap, keymap_diags) = build_keymap(config.layered_docs("keymap"), mod_alias, &registry);
    for diag in &keymap_diags {
        print_diagnostic("keymap", diag);
    }

    let (theme, theme_warnings) = theme::load_bundled();
    for warning in &theme_warnings {
        eprintln!("[theme] warning: {warning}");
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
    };
    (services, desk, user)
}

fn print_diagnostic(source: &str, diag: &Diagnostic) {
    let severity = match diag.severity {
        Severity::Warning => "warning",
        Severity::Error => "error",
    };
    let layer = diag.layer.map(|l| l.name()).unwrap_or("-");
    let file = diag
        .file
        .as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "-".to_string());
    eprintln!("[{source}] {severity} ({layer} {file}): {}", diag.message);
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
}
