//! The Geode binary. Loads layered config (spec §8), builds the action
//! registry, keymap, and starting workspace state, then opens the window on
//! `geode_shell::shell::ShellView` — the keyboard-driven shell root.

use std::path::PathBuf;

use geode_core::config::{Config, ConfigSources, Diagnostic, LayerDoc, Severity};
use geode_shell::actions::ActionRegistry;
use geode_shell::defaults::{BUILTIN_KEYMAP, mod_alias_from_config, register_builtin_actions};
use geode_shell::keymap::build_keymap;
use geode_shell::shell::{ShellServices, ShellView};
use geode_shell::tiling::Workspaces;
use gpui::prelude::*;
use gpui::{App, WindowOptions};
use gpui_component::Root;

fn main() {
    gpui_platform::application()
        .with_assets(gpui_component_assets::Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx); // must run before any component use

            let services = build_shell_services();

            cx.spawn(async move |cx| {
                cx.open_window(WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(services, window, cx));
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
/// (spec §10.1).
fn build_shell_services() -> ShellServices {
    let (desk, user) = config_dirs();
    let builtin_keymap =
        LayerDoc::builtin("keymap", BUILTIN_KEYMAP).expect("builtin keymap TOML is well-formed");
    let config = Config::load(&ConfigSources {
        builtin: vec![builtin_keymap],
        desk,
        user,
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

    ShellServices {
        config,
        registry,
        keymap,
        mod_alias,
        workspaces: Workspaces::new(),
    }
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
