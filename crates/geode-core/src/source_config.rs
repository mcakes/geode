//! `sources.toml` (Phase 3 spec §5.2): one named table per source, atomic
//! by name like every other named config object (foundation §8). Every
//! problem is a diagnostic; a source that cannot be used is skipped and
//! the rest load.
//!
//! Lives in `geode-core`, not `geode-data`, because `geode-shell` may
//! never depend on `geode-data` (workspace layering rule) but still needs
//! to validate a `sources` doc for its own Sources config dialog (Phase 4c
//! §2.2) — this reader has no dependency beyond `MergedDoc`, `SchemaSpec`
//! and `Diagnostic`, all of which already live here, so the whole type
//! (including the `Readiness`/`Priority` field types it carries) moves
//! rather than being duplicated. `geode-data`'s ingest and discovery code
//! keeps using `SourceSpec` unchanged, by re-export
//! (`geode_data::source::SourceSpec`).

use crate::config::{Diagnostic, MergedDoc, Severity};
use crate::schema::SchemaSpec;
use std::path::Path;
use std::time::Duration;

/// `pub` (4c §19.3): the Sources dialog's `fields` spells these back as
/// text (`sources::spell_duration`) when a source omits the key, so the
/// row shows the value that will actually apply rather than a blank.
pub const DEFAULT_POLL: Duration = Duration::from_secs(30);
pub const DEFAULT_PENDING_TIMEOUT: Duration = Duration::from_secs(600);

/// §19.3 (ruling 2026-09-12): a source with nothing to poll is idle, not
/// broken — a warning, and skipped, so the dialog's `n` can create one
/// and let the trader type the globs in afterwards. One line, so the
/// mutation harness can flip its severity by anchoring on it.
pub const IDLE_PATHS: &str = "no 'paths' — the source is idle until one is set";

/// How a source decides a file is complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// `<name>.done` exists and is at least as new as the CSV.
    Sentinel,
    /// No sentinel convention: require a stable (size, mtime) across N polls.
    StableMtime { polls: u32 },
}

/// Where a source sits in the cold-start ladder (spec §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Current risk on screen first.
    LatestRisk,
    /// Vol, instrument reference, scenario data.
    LatestOther,
    /// Older files not already in the database.
    Backfill,
}

#[derive(Debug, Clone)]
pub struct SourceSpec {
    pub name: String,
    pub dataset: String,
    /// One or more directory globs (spec §5.1).
    pub paths: Vec<String>,
    pub readiness: Readiness,
    pub priority: Priority,
    pub poll_interval: Duration,
    pub pending_timeout: Duration,
    /// Regex with a named `batch` capture, applied to the file stem, that
    /// strips the date component so business dates share a partition
    /// (spec §4.3). Without one the whole stem is the batch.
    pub batch_pattern: Option<String>,
}

impl SourceSpec {
    pub fn batch_of(&self, csv: &Path) -> String {
        let stem = csv
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let Some(pattern) = &self.batch_pattern else {
            return stem;
        };
        let Ok(re) = regex::Regex::new(pattern) else {
            return stem;
        };
        re.captures(&stem)
            .and_then(|c| c.name("batch"))
            .map(|m| m.as_str().to_string())
            .unwrap_or(stem)
    }
}

/// `30s`, `10m`, `2h` — integers with one of three units. Nothing else:
/// a bare number has no unit and a fraction has no convention.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (idx, unit) = s.char_indices().last()?;
    let digits = &s[..idx];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u64 = digits.parse().ok()?;
    let secs = match unit {
        's' => n,
        'm' => n.checked_mul(60)?,
        'h' => n.checked_mul(3600)?,
        _ => return None,
    };
    Some(Duration::from_secs(secs))
}

/// Is `pattern` a `batch_pattern` the reader would accept — a regex that
/// compiles and names a `batch` capture? The one spelling of that rule,
/// shared by the reader below and the Sources dialog's inline refusal
/// (§19.3), so a pattern the dialog accepts is never one the reader would
/// go on to drop with a warning.
pub fn check_batch_pattern(pattern: &str) -> Result<(), String> {
    match regex::Regex::new(pattern) {
        Err(e) => Err(format!("batch_pattern does not compile: {e}")),
        Ok(re) if re.capture_names().any(|c| c == Some("batch")) => Ok(()),
        Ok(_) => Err("batch_pattern needs a named `batch` capture, like (?P<batch>.+)".to_string()),
    }
}

/// `sources.<name>[.<key>]` (§19.5): `key` is the deepest field the call
/// site honestly knows — `None` only for "not a table", where there is no
/// field to point into at all.
fn diag(
    severity: Severity,
    name: &str,
    key: Option<&str>,
    m: impl std::fmt::Display,
) -> Diagnostic {
    Diagnostic {
        severity,
        layer: None,
        file: None,
        message: format!("source '{name}': {m}"),
        path: Some(match key {
            Some(k) => format!("sources.{name}.{k}"),
            None => format!("sources.{name}"),
        }),
    }
}

impl SourceSpec {
    pub fn from_doc(doc: &MergedDoc, schema: &SchemaSpec) -> (Vec<SourceSpec>, Vec<Diagnostic>) {
        let mut out = Vec::new();
        let mut diags = Vec::new();

        for (name, value) in &doc.value {
            if name == "config_version" {
                continue;
            }
            let Some(table) = value.as_table() else {
                diags.push(diag(Severity::Warning, name, None, "not a table"));
                continue;
            };

            let dataset = match table.get("dataset").and_then(|v| v.as_str()) {
                Some(d) if schema.dataset(d).is_some() => d.to_string(),
                Some(d) => {
                    diags.push(diag(
                        Severity::Error,
                        name,
                        Some("dataset"),
                        format!("names undeclared dataset '{d}'"),
                    ));
                    continue;
                }
                None => {
                    diags.push(diag(
                        Severity::Error,
                        name,
                        Some("dataset"),
                        "missing 'dataset'",
                    ));
                    continue;
                }
            };

            let paths: Vec<String> = table
                .get("paths")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            if paths.is_empty() {
                // §19.3 (ruling 2026-09-12): a source with nothing to poll
                // is idle, not broken — a warning, and skipped, so the
                // dialog's `n` can create one and let the trader type the
                // globs in afterwards. One line, so the harness can flip
                // its severity by anchoring on it.
                diags.push(diag(Severity::Warning, name, Some("paths"), IDLE_PATHS));
                continue;
            }

            let readiness = match table.get("readiness") {
                None => Readiness::Sentinel,
                Some(v) if v.as_str() == Some("sentinel") => Readiness::Sentinel,
                Some(v) => match v
                    .as_table()
                    .and_then(|t| t.get("stable_mtime"))
                    .and_then(|p| p.as_integer())
                {
                    Some(polls) if polls > 0 => Readiness::StableMtime {
                        polls: polls as u32,
                    },
                    _ => {
                        diags.push(diag(
                            Severity::Warning,
                            name,
                            Some("readiness"),
                            format!("unrecognised readiness {v}; using \"sentinel\""),
                        ));
                        Readiness::Sentinel
                    }
                },
            };

            let priority = match table.get("priority").and_then(|v| v.as_str()) {
                None | Some("latest_risk") => Priority::LatestRisk,
                Some("latest_other") => Priority::LatestOther,
                Some("backfill") => Priority::Backfill,
                Some(other) => {
                    diags.push(diag(
                        Severity::Warning,
                        name,
                        Some("priority"),
                        format!("unknown priority '{other}'; using \"latest_risk\""),
                    ));
                    Priority::LatestRisk
                }
            };

            let mut duration = |key: &str, default: Duration| -> Duration {
                match table.get(key) {
                    None => default,
                    Some(v) => match v.as_str().and_then(parse_duration) {
                        Some(d) => d,
                        None => {
                            diags.push(diag(
                                Severity::Warning,
                                name,
                                Some(key),
                                format!(
                                    "'{key}' must be an integer with unit s, m or h \
                                     (got {v}); using {}s",
                                    default.as_secs()
                                ),
                            ));
                            default
                        }
                    },
                }
            };
            let poll_interval = duration("poll_interval", DEFAULT_POLL);
            let pending_timeout = duration("pending_timeout", DEFAULT_PENDING_TIMEOUT);

            let batch_pattern = match table.get("batch_pattern").and_then(|v| v.as_str()) {
                None => None,
                Some(p) => match check_batch_pattern(p) {
                    Ok(()) => Some(p.to_string()),
                    Err(e) => {
                        diags.push(diag(
                            Severity::Warning,
                            name,
                            Some("batch_pattern"),
                            format!("'{e}'; ignoring it"),
                        ));
                        None
                    }
                },
            };

            out.push(SourceSpec {
                name: name.clone(),
                dataset,
                paths,
                readiness,
                priority,
                poll_interval,
                pending_timeout,
                batch_pattern,
            });
        }

        (out, diags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, Severity, merge_docs};
    use crate::schema::SchemaSpec;

    fn schema() -> SchemaSpec {
        let text = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc).0
    }

    fn parse(text: &str) -> (Vec<SourceSpec>, Vec<Diagnostic>) {
        let doc = merge_docs("sources", &[LayerDoc::builtin("sources", text).unwrap()]);
        SourceSpec::from_doc(&doc, &schema())
    }

    #[test]
    fn durations_are_seconds_minutes_or_hours() {
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("10m"), Some(Duration::from_secs(600)));
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration("0s"), Some(Duration::ZERO));
        assert_eq!(parse_duration("30"), None, "a bare number has no unit");
        assert_eq!(parse_duration("1.5h"), None, "integers only");
        assert_eq!(parse_duration("s"), None);
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("10µ"), None);
        assert_eq!(
            parse_duration("5€"),
            None,
            "a multi-byte unit is not a panic"
        );
    }

    #[test]
    fn a_full_declaration_round_trips() {
        let (specs, diags) = parse(
            r#"
[risk_files]
dataset = "risk_snapshot"
paths = ["/mnt/risk/current/*.csv", "//share/risk/**/*.csv"]
readiness = "sentinel"
priority = "latest_other"
poll_interval = "45s"
pending_timeout = "2h"
batch_pattern = '^risk_\d{4}-\d{2}-\d{2}_(?P<batch>.+)$'
"#,
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(specs.len(), 1);
        let s = &specs[0];
        assert_eq!(s.name, "risk_files");
        assert_eq!(s.dataset, "risk_snapshot");
        assert_eq!(s.paths.len(), 2);
        assert_eq!(s.readiness, Readiness::Sentinel);
        assert_eq!(s.priority, Priority::LatestOther);
        assert_eq!(s.poll_interval, Duration::from_secs(45));
        assert_eq!(s.pending_timeout, Duration::from_secs(7200));
        assert_eq!(
            s.batch_of(std::path::Path::new("/x/risk_2026-09-03_BK000_part1.csv")),
            "BK000_part1"
        );
    }

    #[test]
    fn defaults_fill_what_is_omitted() {
        let (specs, diags) = parse(
            r#"
[risk_files]
dataset = "risk_snapshot"
paths = ["/mnt/risk/*.csv"]
"#,
        );
        assert!(diags.is_empty(), "{diags:?}");
        let s = &specs[0];
        assert_eq!(s.readiness, Readiness::Sentinel);
        assert_eq!(s.priority, Priority::LatestRisk);
        assert_eq!(s.poll_interval, Duration::from_secs(30));
        assert_eq!(s.pending_timeout, Duration::from_secs(600));
        assert_eq!(s.batch_pattern, None);
    }

    #[test]
    fn stable_mtime_readiness_is_a_table() {
        let (specs, _) = parse(
            r#"
[vol]
dataset = "risk_snapshot"
paths = ["/mnt/vol/*.csv"]
readiness = { stable_mtime = 3 }
"#,
        );
        assert_eq!(specs[0].readiness, Readiness::StableMtime { polls: 3 });
    }

    #[test]
    fn a_missing_or_unknown_dataset_is_an_error_and_the_source_is_skipped() {
        let (specs, diags) = parse(
            r#"
[a]
paths = ["/x/*.csv"]
[b]
dataset = "nonesuch"
paths = ["/x/*.csv"]
[c]
dataset = "risk_snapshot"
paths = ["/x/*.csv"]
"#,
        );
        assert_eq!(specs.len(), 1, "only c survives: {specs:?}");
        assert_eq!(specs[0].name, "c");
        let errors: Vec<&str> = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.message.as_str())
            .collect();
        assert_eq!(errors.len(), 2, "{diags:?}");
        assert!(errors[0].contains("'a'") && errors[0].contains("dataset"));
        assert!(errors[1].contains("'b'") && errors[1].contains("nonesuch"));
    }

    /// §19.3 (ruling 2026-09-12, superseding this test's old name): a
    /// missing `paths` key and an explicit `paths = []` reach the same
    /// branch — both are idle, both are warnings, neither is an error.
    #[test]
    fn missing_or_empty_paths_is_idle_not_an_error() {
        let (specs, diags) = parse(
            r#"
[a]
dataset = "risk_snapshot"
[b]
dataset = "risk_snapshot"
paths = []
"#,
        );
        assert!(specs.is_empty(), "{specs:?}");
        assert!(
            diags.iter().all(|d| d.severity != Severity::Error),
            "{diags:?}"
        );
        assert_eq!(
            diags
                .iter()
                .filter(|d| d.severity == Severity::Warning)
                .count(),
            2,
            "{diags:?}"
        );
    }

    #[test]
    fn empty_paths_is_a_warning_and_the_source_is_skipped() {
        let doc = merge_docs(
            "sources",
            &[LayerDoc::builtin(
                "sources",
                "[idle]\ndataset = \"risk_snapshot\"\npaths = []\n",
            )
            .unwrap()],
        );
        let (specs, diags) = SourceSpec::from_doc(&doc, &schema());
        assert!(
            specs.is_empty(),
            "an idle source never reaches the scheduler"
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning, "{diags:?}");
        assert!(diags[0].message.contains("idle"), "{}", diags[0].message);
    }

    #[test]
    fn check_batch_pattern_needs_a_compiling_regex_with_a_batch_capture() {
        assert!(check_batch_pattern("(?P<batch>.+)").is_ok());
        assert!(
            check_batch_pattern("(.+")
                .unwrap_err()
                .contains("does not compile")
        );
        assert!(check_batch_pattern("(.+)").unwrap_err().contains("batch"));
    }

    #[test]
    fn a_bad_duration_priority_or_readiness_warns_and_uses_the_default() {
        let (specs, diags) = parse(
            r#"
[a]
dataset = "risk_snapshot"
paths = ["/x/*.csv"]
poll_interval = "soon"
pending_timeout = "1.5h"
priority = "urgent"
readiness = "hope"
"#,
        );
        assert_eq!(specs.len(), 1);
        let s = &specs[0];
        assert_eq!(s.poll_interval, Duration::from_secs(30));
        assert_eq!(s.pending_timeout, Duration::from_secs(600));
        assert_eq!(s.priority, Priority::LatestRisk);
        assert_eq!(s.readiness, Readiness::Sentinel);
        assert_eq!(
            diags
                .iter()
                .filter(|d| d.severity == Severity::Warning)
                .count(),
            4,
            "{diags:?}"
        );
    }

    #[test]
    fn an_uncompilable_batch_pattern_is_dropped_with_a_warning() {
        let (specs, diags) = parse(
            r#"
[a]
dataset = "risk_snapshot"
paths = ["/x/*.csv"]
batch_pattern = "(?P<batch>unclosed"
"#,
        );
        assert_eq!(specs[0].batch_pattern, None);
        assert!(
            diags.iter().any(|d| d.message.contains("batch_pattern")),
            "{diags:?}"
        );
    }

    #[test]
    fn a_pattern_without_a_batch_capture_is_dropped_with_a_warning() {
        // A pattern that compiles but never captures `batch` would make
        // every file's batch its whole stem — silently defeating §4.3.
        let (specs, diags) = parse(
            r#"
[a]
dataset = "risk_snapshot"
paths = ["/x/*.csv"]
batch_pattern = "^risk_.*$"
"#,
        );
        assert_eq!(specs[0].batch_pattern, None);
        assert!(
            diags.iter().any(|d| d.message.contains("batch")),
            "{diags:?}"
        );
    }

    #[test]
    fn a_missing_dataset_diagnostic_carries_its_field_path() {
        let (_, diags) = parse("[live]\npaths = [\"/x/*.csv\"]\n");
        assert_eq!(
            diags[0].path.as_deref(),
            Some("sources.live.dataset"),
            "{diags:?}"
        );
    }

    #[test]
    fn a_bad_poll_interval_diagnostic_carries_its_field_path() {
        let (_, diags) = parse(
            r#"
[live]
dataset = "risk_snapshot"
paths = ["/x/*.csv"]
poll_interval = "soon"
"#,
        );
        assert_eq!(
            diags[0].path.as_deref(),
            Some("sources.live.poll_interval"),
            "{diags:?}"
        );
    }

    #[test]
    fn a_non_table_entry_is_skipped_with_a_warning() {
        let (specs, diags) = parse("config_version = 1\n");
        assert!(specs.is_empty());
        assert!(
            diags.is_empty(),
            "config_version is not a source and not a complaint: {diags:?}"
        );
        let (specs, diags) = parse("stray = 3\n");
        assert!(specs.is_empty());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }
}
