//! The background collector's own settings, read from `[collector]` in
//! `app.toml`.

use geode_core::config::{Config, Diagnostic, Severity};

/// The collector's settings. The default sets no DuckDB `memory_limit`
/// (DuckDB's own default, as the app): a finite 512MB limit made DuckDB
/// abort the collector with an internal assertion on large CSV loads
/// (`docs/perf.md`, 2026-10-04), which a service manager would restart into
/// the same load. A value can be set once the overnight footprint
/// measurement chooses one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CollectorSettings {
    /// A DuckDB size string (`1GB`, `1.5 GiB`) set on the writer at open;
    /// `None` leaves DuckDB's default.
    pub memory_limit: Option<String>,
}

/// `[collector]` from `app.toml`. An absent `memory_limit` sets none. A
/// value that is not a string, or not a number followed by one of DuckDB's
/// size units (`B`, `KB`…`TB`, `KiB`…`TiB`, any case, optional space),
/// warns at `app.collector.memory_limit` and sets none, so the collector
/// never hands DuckDB a value it would refuse at open.
pub fn collector_settings(config: &Config) -> (CollectorSettings, Vec<Diagnostic>) {
    let mut settings = CollectorSettings::default();
    let mut diagnostics = Vec::new();
    if let Some(value) = config.get("app", "collector.memory_limit") {
        match value.as_str().filter(|v| is_memory_limit(v)) {
            Some(limit) => settings.memory_limit = Some(limit.to_string()),
            None => diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                layer: config.explain("app", "collector.memory_limit"),
                file: None,
                message: format!(
                    "[collector] memory_limit = {value} is not a size such as \"1GB\" \
                     or \"1GiB\"; setting no limit (DuckDB's default)"
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
    fn the_memory_limit_defaults_to_none() {
        let (settings, diags) = collector_settings(&config_from("app", ""));
        assert_eq!(settings.memory_limit, None);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn the_memory_limit_reads_collector_memory_limit() {
        let config = config_from("app", "[collector]\nmemory_limit = \"1GB\"\n");
        let (settings, diags) = collector_settings(&config);
        assert_eq!(settings.memory_limit.as_deref(), Some("1GB"));
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn an_unparseable_memory_limit_warns_and_sets_none() {
        for value in ["\"lots\"", "512", "\"MB\"", "\"1.GB\""] {
            let config = config_from("app", &format!("[collector]\nmemory_limit = {value}\n"));
            let (settings, diags) = collector_settings(&config);
            assert_eq!(settings.memory_limit, None, "{value}");
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
