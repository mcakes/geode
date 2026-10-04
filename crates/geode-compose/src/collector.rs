//! The background collector's own settings, read from `[collector]` in
//! `app.toml`.

use geode_core::config::{Config, Diagnostic, Severity};

/// DuckDB's `memory_limit` for the collector's writer when `[collector]
/// memory_limit` is absent or unusable. Provisional: the collector runs
/// unattended beside the app, so it stays well under DuckDB's own default
/// (80% of physical memory).
pub const DEFAULT_MEMORY_LIMIT: &str = "512MB";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectorSettings {
    /// A DuckDB size string (`512MB`, `1.5 GiB`), set on the writer at open.
    pub memory_limit: String,
}

impl Default for CollectorSettings {
    fn default() -> Self {
        CollectorSettings {
            memory_limit: DEFAULT_MEMORY_LIMIT.to_string(),
        }
    }
}

/// `[collector]` from `app.toml`. An absent `memory_limit` uses
/// [`DEFAULT_MEMORY_LIMIT`] silently. A value that is not a string, or not a
/// number followed by one of DuckDB's size units (`B`, `KB`…`TB`,
/// `KiB`…`TiB`, any case, optional space), warns at
/// `app.collector.memory_limit` and uses the default, so the collector never
/// hands DuckDB a value it would refuse at open.
pub fn collector_settings(config: &Config) -> (CollectorSettings, Vec<Diagnostic>) {
    let mut settings = CollectorSettings::default();
    let mut diagnostics = Vec::new();
    if let Some(value) = config.get("app", "collector.memory_limit") {
        match value.as_str().filter(|v| is_memory_limit(v)) {
            Some(limit) => settings.memory_limit = limit.to_string(),
            None => diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                layer: config.explain("app", "collector.memory_limit"),
                file: None,
                message: format!(
                    "[collector] memory_limit = {value} is not a size such as \"512MB\" \
                     or \"1GiB\"; using {DEFAULT_MEMORY_LIMIT}"
                ),
                path: Some("app.collector.memory_limit".to_string()),
            }),
        }
    }
    (settings, diagnostics)
}

/// `^\d+(\.\d+)?\s*(B|KB|MB|GB|TB|KiB|MiB|GiB|TiB)$`, case-insensitive.
fn is_memory_limit(text: &str) -> bool {
    const UNITS: [&str; 9] = ["B", "KB", "MB", "GB", "TB", "KiB", "MiB", "GiB", "TiB"];
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, rest) = text.split_at(split);
    let number_ok = match number.split_once('.') {
        Some((whole, fraction)) => digits(whole) && digits(fraction),
        None => digits(number),
    };
    number_ok
        && UNITS
            .iter()
            .any(|u| u.eq_ignore_ascii_case(rest.trim_start()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::test_support::config_from;

    #[test]
    fn the_memory_limit_defaults_to_512mb() {
        let (settings, diags) = collector_settings(&config_from("app", ""));
        assert_eq!(settings.memory_limit, "512MB");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn the_memory_limit_reads_collector_memory_limit() {
        let config = config_from("app", "[collector]\nmemory_limit = \"1GB\"\n");
        let (settings, diags) = collector_settings(&config);
        assert_eq!(settings.memory_limit, "1GB");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn an_unparseable_memory_limit_warns_and_uses_the_default() {
        for value in ["\"lots\"", "512", "\"MB\"", "\"1.GB\""] {
            let config = config_from("app", &format!("[collector]\nmemory_limit = {value}\n"));
            let (settings, diags) = collector_settings(&config);
            assert_eq!(settings.memory_limit, "512MB", "{value}");
            assert_eq!(diags.len(), 1, "{value}: {diags:?}");
            assert_eq!(diags[0].severity, Severity::Warning);
            assert_eq!(diags[0].path.as_deref(), Some("app.collector.memory_limit"));
        }
    }

    #[test]
    fn every_duckdb_size_unit_is_accepted_in_any_case() {
        for value in [
            "100B", "1KB", "1.5 MB", "2gb", "1TB", "64KiB", "512mib", "1GiB", "2 TiB",
        ] {
            assert!(is_memory_limit(value), "{value}");
        }
        for value in ["", "GB", "1", "1 G", "-1GB", "1.GB", ".5GB", "1GBB", "1 PB"] {
            assert!(!is_memory_limit(value), "{value}");
        }
    }
}
