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
use crate::schema::{ColumnRole, ColumnType, SchemaSpec};
use std::path::Path;
use std::time::Duration;
use toml::Table;

/// `pub` (4c §19.3): the Sources dialog's `fields` spells these back as
/// text (`sources::spell_duration`) when a source omits the key, so the
/// row shows the value that will actually apply rather than a blank.
pub const DEFAULT_POLL: Duration = Duration::from_secs(30);
pub const DEFAULT_PENDING_TIMEOUT: Duration = Duration::from_secs(600);
/// A subscribed source's default `coalesce`: at most one publish per key
/// every 500ms rather than one per message — "0" opts a source back into
/// publishing every message.
pub const DEFAULT_COALESCE: Duration = Duration::from_millis(500);

/// §19.3 (ruling 2026-09-12): a source with nothing to poll is idle, not
/// broken — a warning, and skipped, so the dialog's `n` can create one
/// and let the trader type the globs in afterwards. One line, so the
/// mutation harness can flip its severity by anchoring on it.
pub const IDLE_PATHS: &str = "no 'paths' — the source is idle until one is set";

/// The reader's own default `adapter` (market-data-documents plan, Task
/// 5): a bare `[sources.<name>]` table with no `adapter` key is a
/// directory-of-CSVs source exactly as it always was — every subscribed
/// field below is meaningless for one and warned away if present.
pub const CSV_DIR_ADAPTER: &str = "csv_dir";

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

/// Which timestamp a subscribed source's publish is stamped with — the
/// same "as-of routing needs one honest clock" question a directory
/// source answers with the CSV's own mtime/sentinel, a subscribed one
/// has no file for. `"receive"` is the default: the moment this process
/// received the message. `"document:<field>"` names an attribute column
/// on the document itself (an `anchor_date`, say) whose value is used
/// instead — validated right here in `from_doc`, against the schema this
/// reader already has in hand: the field must be a document-level
/// attribute (`ColumnRole::Attribute { grain: None }`) of type `Date` or
/// `Utf8`, or the source is skipped with an Error at `.source_time`
/// naming the field and, when it exists but is the wrong shape, its type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceTime {
    Receive,
    Document(String),
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
    /// Which channel implementation feeds this source. `CSV_DIR_ADAPTER`
    /// (the default) is the reader's own directory-of-CSVs path above;
    /// anything else is a subscribed source (`is_subscribed`) and the
    /// fields below govern it instead of `paths`/`readiness`/
    /// `poll_interval`/`pending_timeout`/`batch_pattern`, which a
    /// subscribed source's table may still carry (a hand-edit, a
    /// half-migrated source) but which are warned and ignored.
    pub adapter: String,
    /// The document kind this source publishes (`DocumentRegistry`'s own
    /// key) — required when `adapter != CSV_DIR_ADAPTER`, since a
    /// subscribed source has no CSV header to infer a shape from.
    pub document: Option<String>,
    /// Topic patterns (this plan's one grammar: `>` trailing-levels,
    /// `*` one level, else literal, `/`-separated) this source
    /// subscribes to. Required non-empty when subscribed.
    pub topics: Vec<String>,
    /// At most one publish per key within this window — `DEFAULT_COALESCE`
    /// (500ms) unless overridden; `Duration::ZERO` ("0") opts back into
    /// publishing every message.
    pub coalesce: Duration,
    /// Which timestamp a publish is stamped with. See [`SourceTime`].
    pub source_time: SourceTime,
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

    /// Is this a subscribed source (a channel adapter) rather than the
    /// directory-of-CSVs path? The one door every other crate uses to
    /// tell the two apart — never a direct `adapter != "csv_dir"` string
    /// compare, so `CSV_DIR_ADAPTER` stays the one spelling of the
    /// default.
    pub fn is_subscribed(&self) -> bool {
        self.adapter != CSV_DIR_ADAPTER
    }

    /// A directory-of-CSVs source with every optional field at its
    /// default. The shape most call sites want; override with
    /// struct-update syntax (`..SourceSpec::directory(..)`) where a site
    /// needs a non-default `priority`, `poll_interval` or similar —
    /// fifteen sites across seven files build one of these by hand, so a
    /// shared constructor is the one place that fills the five
    /// subscribed-source fields with their defaults.
    pub fn directory(
        name: impl Into<String>,
        dataset: impl Into<String>,
        paths: Vec<String>,
    ) -> SourceSpec {
        SourceSpec {
            name: name.into(),
            dataset: dataset.into(),
            paths,
            readiness: Readiness::Sentinel,
            priority: Priority::LatestRisk,
            poll_interval: DEFAULT_POLL,
            pending_timeout: DEFAULT_PENDING_TIMEOUT,
            batch_pattern: None,
            adapter: CSV_DIR_ADAPTER.to_string(),
            document: None,
            topics: Vec::new(),
            coalesce: DEFAULT_COALESCE,
            source_time: SourceTime::Receive,
        }
    }
}

/// `30s`, `10m`, `2h`, `500ms` — integers with one of four units. Nothing
/// else: a bare number has no unit and a fraction has no convention,
/// except a bare `"0"` alone, which needs no unit to be unambiguous (a
/// subscribed source's `coalesce = "0"` — publish every message).
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s == "0" {
        return Some(Duration::ZERO);
    }
    if let Some(digits) = s.strip_suffix("ms") {
        return if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            digits.parse().ok().map(Duration::from_millis)
        } else {
            None
        };
    }
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

/// `500ms` where the value has a sub-second remainder, `Ns` otherwise —
/// the inverse of [`parse_duration`], used only to spell a duration back
/// into a diagnostic's "using X" tail so the unit it reports is one
/// `parse_duration` itself would accept.
fn spell_duration(d: Duration) -> String {
    if d.subsec_millis() > 0 {
        format!("{}ms", d.as_millis())
    } else {
        format!("{}s", d.as_secs())
    }
}

/// The config-grammar spelling of a `ColumnType` (`ColumnType::parse`'s
/// own vocabulary, spelled back) — used only to name a `source_time`
/// field's actual type in the "wrong shape" diagnostic below, so the
/// message says something a trader can act on rather than a bare
/// `{:?}` derive.
fn type_name(t: ColumnType) -> &'static str {
    match t {
        ColumnType::Utf8 => "utf8",
        ColumnType::F64 => "f64",
        ColumnType::I64 => "i64",
        ColumnType::Date => "date",
        ColumnType::Timestamp => "timestamp",
        ColumnType::Bool => "bool",
    }
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
        Ok(_) => Err(
            "batch_pattern needs a named `batch` capture, like (?P<batch>.+) \
                      (every file's batch would be its whole stem)"
                .to_string(),
        ),
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

/// Reads a duration-with-unit key, warning and falling back to `default`
/// only when the key is present but unparseable — never when it is
/// simply absent, which is the ordinary "use the default" case with
/// nothing to warn about. Shared by `poll_interval`/`pending_timeout`
/// (directory sources) and `coalesce` (subscribed sources) so the
/// grammar and its error message live in exactly one place.
fn read_duration_or_warn(
    table: &Table,
    diags: &mut Vec<Diagnostic>,
    name: &str,
    key: &str,
    default: Duration,
) -> Duration {
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
                        "'{key}' must be an integer with unit s, m, h or ms \
                         (got {v}); using {}",
                        spell_duration(default)
                    ),
                ));
                default
            }
        },
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

            let adapter = table
                .get("adapter")
                .and_then(|v| v.as_str())
                .unwrap_or(CSV_DIR_ADAPTER)
                .to_string();
            let subscribed = adapter != CSV_DIR_ADAPTER;

            // A subscribed source has no CSV row to infer a shape from —
            // it publishes `DocumentRows` straight into a document-family
            // table, never a measures one (market-data-documents plan).
            if subscribed && !schema.dataset(&dataset).is_some_and(|d| d.is_document()) {
                diags.push(diag(
                    Severity::Error,
                    name,
                    Some("dataset"),
                    format!(
                        "adapter '{adapter}' needs a document family dataset; \
                         '{dataset}' is not one"
                    ),
                ));
                continue;
            }

            // Directory-only keys, meaningful for `csv_dir` alone: warned
            // (and never read for their value below) on a subscribed
            // source rather than silently half-applied.
            for key in [
                "readiness",
                "poll_interval",
                "pending_timeout",
                "batch_pattern",
            ] {
                if subscribed && table.contains_key(key) {
                    diags.push(diag(
                        Severity::Warning,
                        name,
                        Some(key),
                        format!(
                            "'{key}' is ignored by a subscribed source \
                             (adapter != \"{CSV_DIR_ADAPTER}\")"
                        ),
                    ));
                }
            }
            // The subscribed-only keys, the same rule the other way.
            for key in ["document", "topics", "coalesce", "source_time"] {
                if !subscribed && table.contains_key(key) {
                    diags.push(diag(
                        Severity::Warning,
                        name,
                        Some(key),
                        format!(
                            "'{key}' is ignored by a directory source \
                             (adapter == \"{CSV_DIR_ADAPTER}\")"
                        ),
                    ));
                }
            }

            let mut paths: Vec<String> = table
                .get("paths")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            if subscribed {
                if !paths.is_empty() {
                    diags.push(diag(
                        Severity::Warning,
                        name,
                        Some("paths"),
                        format!(
                            "'paths' is ignored by a subscribed source \
                             (adapter != \"{CSV_DIR_ADAPTER}\")"
                        ),
                    ));
                    // Dropped, not merely left unread: a reader must not
                    // STORE what it has just said it ignores. A surface
                    // reading `paths` back — the diagnostics tile's
                    // sources section does — has no other way to know
                    // this source was never going to be polled, and
                    // painted a leftover glob as if it were live.
                    paths.clear();
                }
            } else if paths.is_empty() {
                // §19.3 (ruling 2026-09-12): a source with nothing to poll
                // is idle, not broken — a warning, and skipped, so the
                // dialog's `n` can create one and let the trader type the
                // globs in afterwards. One line, so the harness can flip
                // its severity by anchoring on it. Directory sources only
                // — a subscribed source has nothing to be idle about.
                diags.push(diag(Severity::Warning, name, Some("paths"), IDLE_PATHS));
                continue;
            }

            let readiness = if subscribed {
                Readiness::Sentinel
            } else {
                match table.get("readiness") {
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
                }
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

            let (poll_interval, pending_timeout, batch_pattern) = if subscribed {
                (DEFAULT_POLL, DEFAULT_PENDING_TIMEOUT, None)
            } else {
                let poll_interval =
                    read_duration_or_warn(table, &mut diags, name, "poll_interval", DEFAULT_POLL);
                let pending_timeout = read_duration_or_warn(
                    table,
                    &mut diags,
                    name,
                    "pending_timeout",
                    DEFAULT_PENDING_TIMEOUT,
                );
                let batch_pattern = match table.get("batch_pattern").and_then(|v| v.as_str()) {
                    None => None,
                    Some(p) => match check_batch_pattern(p) {
                        Ok(()) => Some(p.to_string()),
                        Err(e) => {
                            diags.push(diag(
                                Severity::Warning,
                                name,
                                Some("batch_pattern"),
                                format!("{e}; ignoring it"),
                            ));
                            None
                        }
                    },
                };
                (poll_interval, pending_timeout, batch_pattern)
            };

            let (document, topics, coalesce, source_time) = if subscribed {
                let document = match table.get("document").and_then(|v| v.as_str()) {
                    Some(d) => Some(d.to_string()),
                    None => {
                        diags.push(diag(
                            Severity::Error,
                            name,
                            Some("document"),
                            format!(
                                "adapter '{adapter}' needs 'document' \
                                 (the document kind this source publishes)"
                            ),
                        ));
                        continue;
                    }
                };
                let topics: Vec<String> = table
                    .get("topics")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                if topics.is_empty() {
                    diags.push(diag(
                        Severity::Error,
                        name,
                        Some("topics"),
                        format!("adapter '{adapter}' needs at least one topic pattern"),
                    ));
                    continue;
                }
                let coalesce =
                    read_duration_or_warn(table, &mut diags, name, "coalesce", DEFAULT_COALESCE);
                let source_time = match table.get("source_time").and_then(|v| v.as_str()) {
                    None | Some("receive") => SourceTime::Receive,
                    Some(s) => match s.strip_prefix("document:") {
                        Some(field) => {
                            // Validated right here, against the schema
                            // this reader already has in hand — no need
                            // to defer it to whatever later reads
                            // `SourceTime::Document` (controller ruling,
                            // market-data-documents plan Task 5 review).
                            // `schema.dataset(&dataset)` is `Some` and
                            // document-family: checked above, before
                            // this source could reach `subscribed` code
                            // at all.
                            let column = schema.dataset(&dataset).and_then(|d| d.column(field));
                            match column {
                                Some(c)
                                    if matches!(c.role, ColumnRole::Attribute { grain: None })
                                        && matches!(c.ty, ColumnType::Date | ColumnType::Utf8) =>
                                {
                                    SourceTime::Document(field.to_string())
                                }
                                Some(c)
                                    if matches!(c.role, ColumnRole::Attribute { grain: None }) =>
                                {
                                    diags.push(diag(
                                        Severity::Error,
                                        name,
                                        Some("source_time"),
                                        format!(
                                            "'document:{field}' needs a date or utf8 \
                                             attribute; '{field}' is {}",
                                            type_name(c.ty)
                                        ),
                                    ));
                                    continue;
                                }
                                Some(_) => {
                                    diags.push(diag(
                                        Severity::Error,
                                        name,
                                        Some("source_time"),
                                        format!(
                                            "'document:{field}' needs a document-level \
                                             attribute; '{field}' is not one"
                                        ),
                                    ));
                                    continue;
                                }
                                None => {
                                    diags.push(diag(
                                        Severity::Error,
                                        name,
                                        Some("source_time"),
                                        format!(
                                            "'document:{field}' names no column on \
                                             dataset '{dataset}'"
                                        ),
                                    ));
                                    continue;
                                }
                            }
                        }
                        None => {
                            diags.push(diag(
                                Severity::Warning,
                                name,
                                Some("source_time"),
                                format!("unrecognised source_time '{s}'; using \"receive\""),
                            ));
                            SourceTime::Receive
                        }
                    },
                };
                (document, topics, coalesce, source_time)
            } else {
                (None, Vec::new(), DEFAULT_COALESCE, SourceTime::Receive)
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
                adapter,
                document,
                topics,
                coalesce,
                source_time,
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
        // `cvi_params` (copied from `examples/demo-config/datasets.toml`)
        // sits beside the measure-family `risk_snapshot` so the
        // "a subscribed source needs a document family dataset" rule has
        // both a dataset that satisfies it and one that doesn't.
        let text = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"

[cvi_params]
family = "document"
key = ["underlying_ref"]
axes = ["term", "node"]

[cvi_params.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true
[cvi_params.columns.term]
type = "date"
role = "axis"
[cvi_params.columns.node]
type = "f64"
role = "axis"
[cvi_params.columns.param]
type = "f64"
role = "value"
[cvi_params.columns.anchor_date]
type = "date"
role = "attribute"
[cvi_params.columns.spot_ref]
type = "f64"
role = "attribute"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc).0
    }

    fn from(text: &str) -> (Vec<SourceSpec>, Vec<Diagnostic>) {
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
        let (specs, diags) = from(
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
        let (specs, diags) = from(
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
        let (specs, _) = from(
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
        let (specs, diags) = from(
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
        let (specs, diags) = from(
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
        let (specs, diags) = from(
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
        let (specs, diags) = from(
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
        let (specs, diags) = from(
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
        let (_, diags) = from("[live]\npaths = [\"/x/*.csv\"]\n");
        assert_eq!(
            diags[0].path.as_deref(),
            Some("sources.live.dataset"),
            "{diags:?}"
        );
    }

    #[test]
    fn a_bad_poll_interval_diagnostic_carries_its_field_path() {
        let (_, diags) = from(
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
        let (specs, diags) = from("config_version = 1\n");
        assert!(specs.is_empty());
        assert!(
            diags.is_empty(),
            "config_version is not a source and not a complaint: {diags:?}"
        );
        let (specs, diags) = from("stray = 3\n");
        assert!(specs.is_empty());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn parse_duration_accepts_milliseconds_and_a_bare_zero() {
        assert_eq!(parse_duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(parse_duration("0"), Some(Duration::ZERO));
        assert_eq!(parse_duration("2s"), Some(Duration::from_secs(2)));
        assert_eq!(
            parse_duration("5"),
            None,
            "a bare non-zero number has no unit"
        );
        assert_eq!(parse_duration("ms"), None);
    }

    #[test]
    fn a_subscribed_source_parses_its_adapter_fields() {
        let (sources, diags) = from(
            r#"
[cvi]
adapter = "demo_bus"
dataset = "cvi_params"
document = "cvi_params"
topics = ["marketdata/cvi/>"]
coalesce = "250ms"
source_time = "receive"
priority = "latest_other"
"#,
        );
        assert!(diags.is_empty(), "{diags:?}");
        let s = &sources[0];
        assert!(s.is_subscribed());
        assert_eq!(
            (s.adapter.as_str(), s.document.as_deref()),
            ("demo_bus", Some("cvi_params"))
        );
        assert_eq!(s.topics, vec!["marketdata/cvi/>".to_string()]);
        assert_eq!(s.coalesce, Duration::from_millis(250));
        assert_eq!(s.source_time, SourceTime::Receive);
        assert!(s.paths.is_empty());
    }

    /// A subscribed source's `paths` is warned about AND dropped. Storing
    /// what the reader has just said it ignores is how a surface comes to
    /// paint a subscribed source as a directory one — the diagnostics
    /// tile's sources section reads `SourceSpec::paths` and has no other
    /// way to know it was never going to be polled.
    #[test]
    fn a_subscribed_sources_paths_are_warned_about_and_cleared() {
        let (sources, diags) = from(
            r#"
[cvi]
adapter = "demo_bus"
dataset = "cvi_params"
document = "cvi_params"
topics = ["marketdata/cvi/>"]
paths = ["/tmp/leftover/*.csv"]
"#,
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].severity, Severity::Warning);
        assert_eq!(diags[0].path.as_deref(), Some("sources.cvi.paths"));
        assert!(diags[0].message.contains("ignored"), "{:?}", diags[0]);
        assert!(
            sources[0].paths.is_empty(),
            "ignored means dropped, not stored: {:?}",
            sources[0].paths
        );
    }

    #[test]
    fn a_directory_source_is_unchanged_and_defaults_its_adapter() {
        let (sources, diags) = from(
            r#"
[demo]
dataset = "risk_snapshot"
paths = ["/tmp/*.csv"]
"#,
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(sources[0].adapter, CSV_DIR_ADAPTER);
        assert!(!sources[0].is_subscribed());
        assert_eq!(
            sources[0].coalesce,
            Duration::from_millis(500),
            "the default, unused by a directory source"
        );
    }

    #[test]
    fn a_subscribed_source_needs_topics_and_a_document() {
        let (sources, diags) = from(
            "[cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\ndocument = \"cvi_params\"\n",
        );
        assert!(sources.is_empty());
        let d = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("sources.cvi.topics"))
            .unwrap();
        assert_eq!(d.severity, Severity::Error);
        let (sources, diags) =
            from("[cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\ntopics = [\"a/>\"]\n");
        assert!(sources.is_empty());
        assert!(
            diags
                .iter()
                .any(|d| d.path.as_deref() == Some("sources.cvi.document")
                    && d.severity == Severity::Error)
        );
    }

    #[test]
    fn directory_keys_on_a_subscribed_source_warn_and_a_directory_source_still_needs_paths() {
        let (sources, diags) = from(
            "[cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\ndocument = \"cvi_params\"\n\
             topics = [\"a/>\"]\npaths = [\"/x\"]\npoll_interval = \"1s\"\n",
        );
        assert_eq!(sources.len(), 1);
        for key in ["paths", "poll_interval"] {
            assert!(
                diags
                    .iter()
                    .any(|d| d.path.as_deref() == Some(&format!("sources.cvi.{key}"))
                        && d.severity == Severity::Warning),
                "{key}"
            );
        }
        let (sources, diags) = from("[demo]\ndataset = \"risk_snapshot\"\n");
        assert!(sources.is_empty());
        assert!(
            diags
                .iter()
                .any(|d| d.path.as_deref() == Some("sources.demo.paths"))
        );
    }

    #[test]
    fn source_time_document_names_its_field_and_a_bad_value_warns_to_receive() {
        let (sources, _) = from(
            "[cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\ndocument = \"cvi_params\"\n\
             topics = [\"a/>\"]\nsource_time = \"document:anchor_date\"\n",
        );
        assert_eq!(
            sources[0].source_time,
            SourceTime::Document("anchor_date".into())
        );
        let (sources, diags) = from(
            "[cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\ndocument = \"cvi_params\"\n\
             topics = [\"a/>\"]\nsource_time = \"yesterday\"\n",
        );
        assert_eq!(sources[0].source_time, SourceTime::Receive);
        assert!(
            diags
                .iter()
                .any(|d| d.path.as_deref() == Some("sources.cvi.source_time")
                    && d.severity == Severity::Warning)
        );
    }

    /// Task 5 review (2026-09-13): `document:<field>` is validated at
    /// load, against the schema this reader already has — a typo names
    /// no column at all, an f64 attribute is the wrong type, and a real
    /// document-level date/utf8 attribute is accepted.
    #[test]
    fn source_time_document_field_is_validated_against_the_schema() {
        let (sources, diags) = from(
            "[cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\ndocument = \"cvi_params\"\n\
             topics = [\"a/>\"]\nsource_time = \"document:anhor_date\"\n",
        );
        assert!(sources.is_empty(), "{sources:?}");
        let d = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("sources.cvi.source_time"))
            .unwrap();
        assert_eq!(d.severity, Severity::Error);

        let (sources, diags) = from(
            "[cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\ndocument = \"cvi_params\"\n\
             topics = [\"a/>\"]\nsource_time = \"document:spot_ref\"\n",
        );
        assert!(sources.is_empty(), "{sources:?}");
        let d = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("sources.cvi.source_time"))
            .unwrap();
        assert_eq!(d.severity, Severity::Error);
        assert!(d.message.contains("f64"), "{}", d.message);

        let (sources, diags) = from(
            "[cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\ndocument = \"cvi_params\"\n\
             topics = [\"a/>\"]\nsource_time = \"document:anchor_date\"\n",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(
            sources[0].source_time,
            SourceTime::Document("anchor_date".into())
        );
    }

    #[test]
    fn a_subscribed_source_on_a_measure_dataset_is_an_error() {
        let (sources, diags) = from(
            "[x]\nadapter = \"demo_bus\"\ndataset = \"risk_snapshot\"\ndocument = \"cvi_params\"\ntopics = [\"a/>\"]\n",
        );
        assert!(sources.is_empty());
        assert!(
            diags
                .iter()
                .any(|d| d.path.as_deref() == Some("sources.x.dataset")
                    && d.message.contains("document family"))
        );
    }
}
