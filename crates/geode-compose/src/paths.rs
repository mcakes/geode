//! Where the store and the configuration layers live on disk.

use std::path::{Path, PathBuf};

use geode_core::config::Config;

/// Choose data.db_path from app.toml, then the demo directory, then the
/// supplied platform data directory/home fallback.
pub fn db_path(
    config: &Config,
    demo: Option<&Path>,
    local_app_data: Option<String>,
    home: Option<String>,
) -> PathBuf {
    if let Some(p) = config.get("app", "data.db_path").and_then(|v| v.as_str()) {
        return PathBuf::from(p);
    }
    if let Some(dir) = demo {
        return dir.join("geode.duckdb");
    }
    if let Some(lad) = local_app_data {
        return PathBuf::from(lad).join("Geode").join("geode.duckdb");
    }
    let home = home.unwrap_or_else(|| ".".into());
    if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library/Application Support/Geode/geode.duckdb")
    } else {
        PathBuf::from(home).join(".local/share/geode/geode.duckdb")
    }
}

/// Resolve the desk directory from `GEODE_DESK_CONFIG` and the user directory
/// from `APPDATA`, falling back to `HOME/.config`. Missing or non-Unicode
/// environment values are treated as absent. No filesystem checks are made.
pub fn config_dirs() -> (Option<PathBuf>, Option<PathBuf>) {
    let desk = std::env::var("GEODE_DESK_CONFIG").ok().map(PathBuf::from);
    let user = user_config_dir(std::env::var("APPDATA").ok(), std::env::var("HOME").ok());
    (desk, user)
}

/// Pure core of the user-config-directory resolution in [`config_dirs`]:
/// `%APPDATA%/geode` when set, else `$HOME/.config/geode`, else `None`.
/// Kept as a pure function of its inputs so it's unit-testable without
/// touching the real environment.
pub fn user_config_dir(appdata: Option<String>, home: Option<String>) -> Option<PathBuf> {
    if let Some(appdata) = appdata {
        return Some(PathBuf::from(appdata).join("geode"));
    }
    home.map(|home| PathBuf::from(home).join(".config").join("geode"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{ConfigSources, LayerDoc};

    #[test]
    fn the_database_path_prefers_config_then_demo_then_the_platform_dir() {
        let empty = Config::load(&ConfigSources::default());
        assert_eq!(
            db_path(
                &empty,
                None,
                Some("C:\\Users\\me\\AppData\\Local".into()),
                Some("/home/me".into())
            ),
            PathBuf::from("C:\\Users\\me\\AppData\\Local")
                .join("Geode")
                .join("geode.duckdb")
        );
        let unix = db_path(&empty, None, None, Some("/home/me".into()));
        assert!(
            unix.ends_with("Geode/geode.duckdb") || unix.ends_with("geode/geode.duckdb"),
            "{unix:?}"
        );
        assert_eq!(
            db_path(
                &empty,
                Some(std::path::Path::new("/tmp/geode-demo/100000-42")),
                None,
                None
            ),
            PathBuf::from("/tmp/geode-demo/100000-42/geode.duckdb")
        );
        let configured = Config::load(&ConfigSources {
            builtin: vec![
                LayerDoc::builtin("app", "[data]\ndb_path = \"/var/geode/x.duckdb\"\n").unwrap(),
            ],
            ..ConfigSources::default()
        });
        assert_eq!(
            db_path(
                &configured,
                Some(std::path::Path::new("/tmp/d")),
                None,
                None
            ),
            PathBuf::from("/var/geode/x.duckdb"),
            "config wins even over demo"
        );
    }

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
