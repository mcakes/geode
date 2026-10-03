//! Typed `sources.toml` configuration, with one top-level table per source.
//! Layer merging replaces a source's whole table by name. Parsing returns
//! usable sources and field-addressed diagnostics; invalid sources are skipped.
//!
//! This reader performs no I/O and depends only on shared configuration and
//! schema types. Both the shell and data service use it without depending on
//! each other. Transport and document-kind availability are checked by the
//! assembled application and service. See `docs/current/configuration.md`.

use crate::config::{Diagnostic, MergedDoc, Severity};
use crate::schema::{ColumnRole, ColumnType, Family, SchemaSpec};
use std::path::Path;
use std::time::Duration;
use toml::Table;

/// Directory polling defaults, also used by configuration editors to display
/// the effective values of omitted settings.
pub const DEFAULT_POLL: Duration = Duration::from_secs(30);
pub const DEFAULT_PENDING_TIMEOUT: Duration = Duration::from_secs(600);
/// Snapshot polling default: a reference table changes rarely, and each poll
/// rereads it whole.
pub const DEFAULT_SNAPSHOT_POLL: Duration = Duration::from_secs(300);
/// Default minimum spacing between coalescer releases for each document key.
/// This limits submission rate, not publication timing on the ingest writer.
/// A zero window releases every accepted, valid message without coalescing.
pub const DEFAULT_COALESCE: Duration = Duration::from_millis(500);
/// How long a subscription's recovery waits for replies; sent with each
/// GET request, so the transport stops answering when the receiver stops
/// listening.
pub const DEFAULT_RECOVER_TIMEOUT: Duration = Duration::from_secs(10);
/// Recorded topics not received for this long are pruned at open and never
/// asked for: a retired instrument stops answering, and asking for it on
/// every start would only add unanswered requests.
pub const DEFAULT_RECOVER_MAX_AGE: Duration = Duration::from_secs(7 * 86_400);

/// A directory source without usable paths is idle: warn and skip it while
/// allowing its configuration to be saved and completed later.
pub const IDLE_PATHS: &str = "no 'paths' — the source is idle until one is set";

/// Default adapter name. `csv_dir` selects directory discovery; other names
/// select a subscription or fetch worker according to the dataset family.
pub const CSV_DIR_ADAPTER: &str = "csv_dir";

/// How a source decides a file is complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// `<csv filename>.done` exists and is at least as new as the CSV.
    Sentinel,
    /// Requested stability across N polls. Configuration accepts this setting,
    /// but discovery has no poll history and reports it as unsupported.
    StableMtime { polls: u32 },
}

/// File-ingestion priority. Current candidates precede historical backfill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Current risk on screen first.
    LatestRisk,
    /// Vol, instrument reference, scenario data.
    LatestOther,
    /// Older files not already in the database.
    Backfill,
}

/// Timestamp policy for subscribed documents. Receive uses message arrival;
/// Document names a document-level Date or Utf8 attribute. The reader rejects
/// an absent or incompatible schema field with a source_time error. The
/// receiver separately validates the value in each message. Directory sources
/// use sentinel source time instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceTime {
    Receive,
    Document(String),
}

/// Runtime pipeline selected by adapter and dataset family. `csv_dir` uses
/// discovery; another adapter uses fetching for series, snapshot polling for
/// reference datasets, and subscription for documents. Derive this from the
/// schema rather than storing a second answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceShape {
    Directory,
    Subscribed,
    Fetch,
    /// Another adapter over a reference dataset: a worker polls the whole
    /// table on `poll_interval`.
    Snapshot,
}

#[derive(Debug, Clone)]
pub struct SourceSpec {
    pub name: String,
    pub dataset: String,
    /// Directory globs. Parsed adapter-backed sources have an empty list.
    pub paths: Vec<String>,
    pub readiness: Readiness,
    pub priority: Priority,
    pub poll_interval: Duration,
    pub pending_timeout: Duration,
    /// Regex applied to the file stem, with a named `batch` capture. Capturing
    /// the date-independent part keeps successive dates in the same partition.
    /// Absent, invalid, or nonmatching patterns fall back to the whole stem.
    pub batch_pattern: Option<String>,
    /// Transport name. `csv_dir` selects directory discovery; other adapters
    /// use subscription or fetching according to [`SourceSpec::shape`].
    pub adapter: String,
    /// Document registry key, required for subscriptions. Fetch sources have no
    /// document kind and ignore this setting.
    pub document: Option<String>,
    /// Subscription topic patterns: `/` separates levels, `*` matches one level,
    /// and a final `>` matches one or more trailing levels. Required for
    /// subscriptions; unused by directory and fetch sources.
    pub topics: Vec<String>,
    /// Minimum interval between coalescer releases for a document key. Defaults
    /// to 500 ms; zero disables coalescing. Already queued ingest jobs are unaffected.
    pub coalesce: Duration,
    /// Subscriptions only: how long recovery at start and reconnect waits for
    /// the transport's replies. Defaults to [`DEFAULT_RECOVER_TIMEOUT`].
    /// Zero leaves only the receiver's one-second grace for replies.
    pub recover_timeout: Duration,
    /// Subscriptions only: recorded topics older than this are pruned and not
    /// asked for in recovery. Defaults to [`DEFAULT_RECOVER_MAX_AGE`].
    pub recover_max_age: Duration,
    /// Which timestamp a publish is stamped with. See [`SourceTime`].
    pub source_time: SourceTime,
    /// Snapshot only: the adapter-defined name of the table to read.
    pub table: Option<String>,
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

    /// Whether the source uses an adapter other than `csv_dir`. This includes
    /// fetch sources despite the method name; use [`SourceSpec::shape`] to choose
    /// between a subscription receiver and a fetch worker.
    pub fn is_subscribed(&self) -> bool {
        self.adapter != CSV_DIR_ADAPTER
    }

    pub fn shape(&self, schema: &SchemaSpec) -> SourceShape {
        let family = schema.dataset(&self.dataset).map(|d| d.family);
        if !self.is_subscribed() {
            SourceShape::Directory
        } else if family == Some(Family::Series) {
            SourceShape::Fetch
        } else if family == Some(Family::Reference) {
            SourceShape::Snapshot
        } else {
            SourceShape::Subscribed
        }
    }

    /// Construct a directory source with default optional settings. Callers can
    /// override individual fields with struct-update syntax. This constructor
    /// does not perform the validation in [`SourceSpec::from_doc`].
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
            recover_timeout: DEFAULT_RECOVER_TIMEOUT,
            recover_max_age: DEFAULT_RECOVER_MAX_AGE,
            source_time: SourceTime::Receive,
            table: None,
        }
    }
}

/// Parse a nonnegative integer with unit `ms`, `s`, `m`, `h`, `d`, or `y`.
/// Days are 24 hours and years are 365 days. A bare `0` is also accepted;
/// other bare numbers, fractions, signs, and overflowing values are rejected.
/// Surrounding whitespace is ignored.
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
        'd' => n.checked_mul(86_400)?,
        'y' => n.checked_mul(365 * 86_400)?,
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

/// Check that a regex compiles and declares a named `batch` capture. Shared
/// by config parsing and the source editor. This does not require a match
/// against any particular filename.
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

/// Address diagnostics as `sources.<name>[.<key>]`. Use the deepest known
/// field, or only the source name when the entry is not a table.
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

/// Validate subscription patterns before delivery. Reject an empty pattern,
/// empty slash-separated levels, and a `>` level outside the final position.
/// Only whole levels `*` and final `>` are wildcards; other text is literal.
/// The runtime matcher remains in geode-data and performs no validation.
fn validate_topic_pattern(pattern: &str) -> Result<(), String> {
    if pattern.is_empty() {
        return Err("empty topic pattern".to_string());
    }
    let levels: Vec<&str> = pattern.split('/').collect();
    let last = levels.len() - 1;
    for (i, level) in levels.iter().enumerate() {
        if level.is_empty() {
            return Err(format!("empty level in topic pattern '{pattern}'"));
        }
        if *level == ">" && i != last {
            return Err(format!(
                "'>' is only valid as the final level in topic pattern '{pattern}'"
            ));
        }
    }
    Ok(())
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
        // Reference dataset → the accepted snapshot source filling it, in TOML order.
        let mut claimed: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        for (name, value) in &doc.value {
            if name == "config_version" {
                continue;
            }
            let Some(table) = value.as_table() else {
                diags.push(diag(Severity::Warning, name, None, "not a table"));
                continue;
            };

            let dataset = match table.get("dataset").and_then(|v| v.as_str()) {
                Some(d) if schema.dataset(d).is_some_and(|ds| ds.computed) => {
                    diags.push(diag(
                        Severity::Error,
                        name,
                        Some("dataset"),
                        format!(
                            "'{d}' is a computed dataset: a module answers for it and no \
                             source may feed it"
                        ),
                    ));
                    continue;
                }
                Some(d) if schema.dataset(d).is_some_and(|ds| ds.local) => {
                    diags.push(diag(
                        Severity::Error,
                        name,
                        Some("dataset"),
                        format!(
                            "'{d}' is a local dataset: the app is its only writer and no \
                             source may feed it"
                        ),
                    ));
                    continue;
                }
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
            let family = schema.dataset(&dataset).map(|d| d.family);
            let fetch = subscribed && family == Some(Family::Series);
            let snapshot = subscribed && family == Some(Family::Reference);

            // Directory loading writes grain tables, so it cannot fill a series dataset.
            if !subscribed && family == Some(Family::Series) {
                diags.push(diag(
                    Severity::Error,
                    name,
                    Some("dataset"),
                    format!(
                        "a directory source cannot fill the series dataset '{dataset}'; \
                         name a fetch adapter"
                    ),
                ));
                continue;
            }
            // Nor a reference dataset, whose whole table a snapshot worker replaces.
            if !subscribed && family == Some(Family::Reference) {
                diags.push(diag(
                    Severity::Error,
                    name,
                    Some("dataset"),
                    format!(
                        "a directory source cannot fill the reference dataset '{dataset}'; \
                         name a snapshot adapter"
                    ),
                ));
                continue;
            }
            // A snapshot publishes the whole table as one batch named after the
            // dataset, so a second source would replace the first's rows on every
            // poll: never unchanged, and an archive growing without bound.
            if snapshot && let Some(first) = claimed.get(&dataset) {
                diags.push(diag(
                    Severity::Error,
                    name,
                    Some("dataset"),
                    format!(
                        "snapshot source '{first}' already fills the reference dataset \
                         '{dataset}'; a reference dataset has at most one source"
                    ),
                ));
                continue;
            }
            // A subscription adapter publishes DocumentRows and requires a document dataset.
            if subscribed
                && !fetch
                && !snapshot
                && !schema.dataset(&dataset).is_some_and(|d| d.is_document())
            {
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

            // Warn about directory settings on adapter-backed sources. Use defaults in
            // the typed value rather than retaining settings the runtime will ignore.
            let ignored = |key: &str, diags: &mut Vec<Diagnostic>| {
                if !table.contains_key(key) {
                    return;
                }
                let m = if fetch {
                    format!("'{key}' is ignored by a fetch source (a series dataset)")
                } else if snapshot {
                    format!("'{key}' is ignored by a snapshot source (a reference dataset)")
                } else if subscribed {
                    format!(
                        "'{key}' is ignored by a subscribed source \
                         (adapter != \"{CSV_DIR_ADAPTER}\")"
                    )
                } else {
                    format!(
                        "'{key}' is ignored by a directory source \
                         (adapter == \"{CSV_DIR_ADAPTER}\")"
                    )
                };
                diags.push(diag(Severity::Warning, name, Some(key), m));
            };
            for key in [
                "readiness",
                "poll_interval",
                "pending_timeout",
                "batch_pattern",
            ] {
                // A snapshot worker polls on its own interval.
                if subscribed && !(snapshot && key == "poll_interval") {
                    ignored(key, &mut diags);
                }
            }
            // Only subscriptions use document kind, topics, coalescing, recovery,
            // and source time.
            for key in [
                "document",
                "topics",
                "coalesce",
                "recover_timeout",
                "recover_max_age",
                "source_time",
            ] {
                if !subscribed || fetch || snapshot {
                    ignored(key, &mut diags);
                }
            }
            if !snapshot {
                ignored("table", &mut diags);
            }
            // Snapshots are taken ahead of every file: priority orders files only.
            if snapshot {
                ignored("priority", &mut diags);
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
                    let m = if fetch {
                        "'paths' is ignored by a fetch source (a series dataset)".to_string()
                    } else if snapshot {
                        "'paths' is ignored by a snapshot source (a reference dataset)".to_string()
                    } else {
                        format!(
                            "'paths' is ignored by a subscribed source \
                             (adapter != \"{CSV_DIR_ADAPTER}\")"
                        )
                    };
                    diags.push(diag(Severity::Warning, name, Some("paths"), m));
                    // Clear ignored paths so consumers of the typed configuration cannot
                    // mistake them for active directory globs.
                    paths.clear();
                }
            } else if paths.is_empty() {
                // An incomplete directory source is idle, not invalid. Warn and skip it;
                // adapter-backed sources do not need directory paths.
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

            // A snapshot's priority is never read: its setting warned above, and
            // the spec carries the directory default only to have a value.
            let setting = if snapshot {
                None
            } else {
                table.get("priority")
            };
            let priority = match setting.and_then(|v| v.as_str()) {
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

            let (poll_interval, pending_timeout, batch_pattern) = if snapshot {
                let poll = read_duration_or_warn(
                    table,
                    &mut diags,
                    name,
                    "poll_interval",
                    DEFAULT_SNAPSHOT_POLL,
                );
                // A zero interval would reread the whole table in a tight loop.
                let poll = if poll.is_zero() {
                    diags.push(diag(
                        Severity::Warning,
                        name,
                        Some("poll_interval"),
                        format!(
                            "'poll_interval' must be greater than zero; using {}",
                            spell_duration(DEFAULT_SNAPSHOT_POLL)
                        ),
                    ));
                    DEFAULT_SNAPSHOT_POLL
                } else {
                    poll
                };
                (poll, DEFAULT_PENDING_TIMEOUT, None)
            } else if subscribed {
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

            let table_name = if snapshot {
                match table.get("table").and_then(|v| v.as_str()) {
                    Some(t) if !t.is_empty() => Some(t.to_string()),
                    _ => {
                        diags.push(diag(
                            Severity::Error,
                            name,
                            Some("table"),
                            format!("adapter '{adapter}' needs 'table' naming what to read"),
                        ));
                        continue;
                    }
                }
            } else {
                None
            };

            let (document, topics, coalesce, recover, source_time) = if subscribed
                && !fetch
                && !snapshot
            {
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
                if let Some(reason) = topics.iter().find_map(|t| validate_topic_pattern(t).err()) {
                    diags.push(diag(Severity::Error, name, Some("topics"), reason));
                    continue;
                }
                let coalesce =
                    read_duration_or_warn(table, &mut diags, name, "coalesce", DEFAULT_COALESCE);
                let recover = (
                    read_duration_or_warn(
                        table,
                        &mut diags,
                        name,
                        "recover_timeout",
                        DEFAULT_RECOVER_TIMEOUT,
                    ),
                    read_duration_or_warn(
                        table,
                        &mut diags,
                        name,
                        "recover_max_age",
                        DEFAULT_RECOVER_MAX_AGE,
                    ),
                );
                let source_time = match table.get("source_time").and_then(|v| v.as_str()) {
                    None | Some("receive") => SourceTime::Receive,
                    Some(s) => match s.strip_prefix("document:") {
                        Some(field) => {
                            // Validate against the already resolved document schema. The selected
                            // attribute must have a type the receiver can convert to source time.
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
                (document, topics, coalesce, recover, source_time)
            } else {
                let recover = (DEFAULT_RECOVER_TIMEOUT, DEFAULT_RECOVER_MAX_AGE);
                (
                    None,
                    Vec::new(),
                    DEFAULT_COALESCE,
                    recover,
                    SourceTime::Receive,
                )
            };

            if snapshot {
                claimed.insert(dataset.clone(), name.clone());
            }
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
                recover_timeout: recover.0,
                recover_max_age: recover.1,
                source_time,
                table: table_name,
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
        // Include a document dataset and a measure dataset to exercise both sides
        // of the subscription-family validation rule.
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

[series]
family = "series"

[underlyings]
family = "reference"
key = ["underlying_ref"]
[underlyings.columns.underlying_ref]
type = "utf8"
role = "dimension"
[underlyings.columns.currency]
type = "utf8"
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
        assert_eq!(
            parse_duration("30d"),
            Some(Duration::from_secs(30 * 86_400))
        );
        assert_eq!(
            parse_duration("5y"),
            Some(Duration::from_secs(5 * 365 * 86_400))
        );
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

    /// Missing paths and an empty list both produce an idle warning and no
    /// runtime source, allowing incomplete configurations to be saved.
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
        // A regex without a named batch capture would silently use the whole stem,
        // separating business dates that should share a partition.
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

    #[test]
    fn a_subscribed_source_reads_its_recovery_settings() {
        let (sources, diags) = from(
            r#"[cvi]
adapter = "bus"
dataset = "cvi_params"
document = "cvi_params"
topics = ["marketdata/cvi/*/NOTIFY"]
recover_timeout = "3s"
recover_max_age = "2d"
"#,
        );
        assert!(diags.is_empty(), "{diags:?}");
        let s = &sources[0];
        assert_eq!(s.recover_timeout, Duration::from_secs(3));
        assert_eq!(s.recover_max_age, Duration::from_secs(2 * 86_400));
    }

    #[test]
    fn recovery_settings_default_when_absent() {
        let (sources, diags) = from(
            r#"[cvi]
adapter = "bus"
dataset = "cvi_params"
document = "cvi_params"
topics = ["marketdata/cvi/*/NOTIFY"]
"#,
        );
        assert!(diags.is_empty(), "{diags:?}");
        let s = &sources[0];
        assert_eq!(s.recover_timeout, DEFAULT_RECOVER_TIMEOUT);
        assert_eq!(s.recover_max_age, DEFAULT_RECOVER_MAX_AGE);
    }

    #[test]
    fn recovery_settings_on_a_directory_source_warn_and_are_ignored() {
        let (sources, diags) = from(
            r#"[risk]
dataset = "risk_snapshot"
paths = ["/tmp/*.csv"]
recover_timeout = "3s"
recover_max_age = "2d"
"#,
        );
        assert_eq!(sources.len(), 1, "{diags:?}");
        for key in ["recover_timeout", "recover_max_age"] {
            assert!(
                diags.iter().any(
                    |d| d.path.as_deref() == Some(&format!("sources.risk.{key}"))
                        && d.severity == Severity::Warning
                ),
                "{key}: {diags:?}"
            );
        }
        assert_eq!(sources[0].recover_timeout, DEFAULT_RECOVER_TIMEOUT);
        assert_eq!(sources[0].recover_max_age, DEFAULT_RECOVER_MAX_AGE);
    }

    /// Ignored subscription paths must be removed from the typed configuration,
    /// so downstream diagnostics do not present them as active directory globs.
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
    fn a_source_naming_a_local_dataset_is_refused() {
        let text = r#"
[sheets]
family = "document"
local = true
key = ["sheet"]
axes = ["line"]
[sheets.columns.sheet]
type = "utf8"
role = "dimension"
[sheets.columns.line]
type = "i64"
role = "axis"
[sheets.columns.qty]
type = "i64"
role = "value"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        let schema = SchemaSpec::from_doc(&doc).0;

        let sources_doc = merge_docs(
            "sources",
            &[LayerDoc::builtin(
                "sources",
                r#"
[feed]
dataset = "sheets"
paths = ["/x/*.csv"]
"#,
            )
            .unwrap()],
        );
        let (sources, diags) = SourceSpec::from_doc(&sources_doc, &schema);
        assert!(sources.is_empty());
        let d = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("sources.feed.dataset"))
            .unwrap();
        assert_eq!(d.severity, Severity::Error);
        assert!(d.message.contains("local"), "{}", d.message);
    }

    #[test]
    fn a_source_naming_a_computed_dataset_is_refused() {
        let text =
            "[pricer]\ncomputed = true\n[pricer.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\n";
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        let schema = SchemaSpec::from_doc(&doc).0;

        let sources_doc = merge_docs(
            "sources",
            &[LayerDoc::builtin(
                "sources",
                r#"
[feed]
dataset = "pricer"
paths = ["/x/*.csv"]
"#,
            )
            .unwrap()],
        );
        let (sources, diags) = SourceSpec::from_doc(&sources_doc, &schema);
        assert!(sources.is_empty());
        let d = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("sources.feed.dataset"))
            .unwrap();
        assert_eq!(d.severity, Severity::Error);
        assert!(d.message.contains("computed"), "{}", d.message);
    }

    /// Reject malformed topic patterns at config load: empty patterns, empty
    /// levels, and a non-final `>` must produce a diagnostic and skip the source.
    #[test]
    fn a_topic_pattern_with_an_empty_string_is_refused() {
        let (sources, diags) = from(
            "[cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\n\
             document = \"cvi_params\"\ntopics = [\"\"]\n",
        );
        assert!(sources.is_empty());
        let d = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("sources.cvi.topics"))
            .expect("an error diagnostic on topics");
        assert_eq!(d.severity, Severity::Error);
    }

    #[test]
    fn a_topic_pattern_with_an_empty_level_is_refused() {
        let (sources, diags) = from(
            "[cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\n\
             document = \"cvi_params\"\ntopics = [\"a//b\"]\n",
        );
        assert!(sources.is_empty());
        let d = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("sources.cvi.topics"))
            .expect("an error diagnostic on topics");
        assert_eq!(d.severity, Severity::Error);
    }

    #[test]
    fn a_topic_pattern_with_a_non_final_greater_than_is_refused() {
        let (sources, diags) = from(
            "[cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\n\
             document = \"cvi_params\"\ntopics = [\"a/>/b\"]\n",
        );
        assert!(sources.is_empty());
        let d = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("sources.cvi.topics"))
            .expect("an error diagnostic on topics");
        assert_eq!(d.severity, Severity::Error);
    }

    #[test]
    fn topic_patterns_with_star_and_a_final_greater_than_are_accepted() {
        let (sources, diags) = from(
            "[cvi]\nadapter = \"demo_bus\"\ndataset = \"cvi_params\"\n\
             document = \"cvi_params\"\ntopics = [\"marketdata/*/SPX.Z\", \"marketdata/cvi/>\"]\n",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(sources.len(), 1);
        assert_eq!(
            sources[0].topics,
            vec![
                "marketdata/*/SPX.Z".to_string(),
                "marketdata/cvi/>".to_string()
            ]
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

    /// Validate document timestamp fields against the schema: reject unknown
    /// columns and incompatible types; accept document-level Date/Utf8 attributes.
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

    #[test]
    fn a_source_over_a_series_dataset_is_a_fetch_source() {
        let (specs, diags) = from(
            r#"
[kdb_hist]
adapter = "kdb"
dataset = "series"
"#,
        );
        assert!(diags.is_empty(), "{diags:?}");
        let s = &specs[0];
        assert_eq!(s.shape(&schema()), SourceShape::Fetch);
        assert!(s.is_subscribed(), "a fetch source is not a directory");
        assert_eq!(s.document, None);
        assert!(s.topics.is_empty());
        assert!(s.paths.is_empty());
    }

    #[test]
    fn subscribed_and_directory_keys_are_warned_on_a_fetch_source() {
        let (specs, diags) = from(
            r#"
[kdb_hist]
adapter = "kdb"
dataset = "series"
topics = ["a/>"]
document = "cvi_params"
paths = ["/x/*.csv"]
poll_interval = "2s"
"#,
        );
        assert_eq!(specs.len(), 1);
        assert!(specs[0].paths.is_empty(), "paths are dropped, not stored");
        for key in ["topics", "document", "paths", "poll_interval"] {
            assert!(
                diags.iter().any(|d| d.severity == Severity::Warning
                    && d.path.as_deref() == Some(&format!("sources.kdb_hist.{key}"))
                    && d.message.contains("fetch source")),
                "missing warning for {key}: {diags:?}"
            );
        }
    }

    #[test]
    fn a_csv_dir_source_over_a_series_dataset_is_refused() {
        let (specs, diags) = from(
            r#"
[files]
dataset = "series"
paths = ["/x/*.csv"]
"#,
        );
        assert!(specs.is_empty());
        assert!(
            diags.iter().any(|d| d.severity == Severity::Error
                && d.path.as_deref() == Some("sources.files.dataset")
                && d.message.contains("series")),
            "{diags:?}"
        );
    }

    #[test]
    fn shape_names_all_four() {
        let (specs, _) = from(
            r#"
[a]
dataset = "risk_snapshot"
paths = ["/x/*.csv"]
[b]
adapter = "solace"
dataset = "cvi_params"
document = "cvi_params"
topics = ["t/>"]
[c]
adapter = "kdb"
dataset = "series"
[d]
adapter = "sql"
dataset = "underlyings"
table = "underlyings"
"#,
        );
        let shapes: Vec<SourceShape> = specs.iter().map(|s| s.shape(&schema())).collect();
        assert_eq!(
            shapes,
            vec![
                SourceShape::Directory,
                SourceShape::Subscribed,
                SourceShape::Fetch,
                SourceShape::Snapshot
            ]
        );
    }

    #[test]
    fn an_adapter_over_a_reference_dataset_is_a_snapshot_source() {
        let (specs, diags) = from(
            r#"
[refdb]
adapter = "sql"
dataset = "underlyings"
table = "underlyings"
"#,
        );
        assert!(diags.is_empty(), "{diags:?}");
        let s = &specs[0];
        assert_eq!(s.shape(&schema()), SourceShape::Snapshot);
        assert_eq!(s.table.as_deref(), Some("underlyings"));
        assert_eq!(s.poll_interval, DEFAULT_SNAPSHOT_POLL);
    }

    /// Snapshots are taken ahead of every file, so a snapshot source has no
    /// priority: one set warns like any other foreign setting and the typed
    /// value keeps the directory default.
    #[test]
    fn priority_on_a_snapshot_source_warns_as_ignored() {
        let (specs, diags) = from(
            "[refdb]\nadapter = \"sql\"\ndataset = \"underlyings\"\ntable = \"t\"\npriority = \"backfill\"\n",
        );
        assert_eq!(specs[0].priority, Priority::LatestRisk);
        let warning = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("sources.refdb.priority"))
            .unwrap_or_else(|| panic!("{diags:?}"));
        assert_eq!(warning.severity, Severity::Warning);
        assert!(
            warning.message.contains("ignored by a snapshot source"),
            "{}",
            warning.message
        );
    }

    /// One snapshot publishes the whole table as one batch named after the
    /// dataset, so a second source over it would replace the first's rows on
    /// every poll: never unchanged, and the archive grows without bound.
    #[test]
    fn a_second_snapshot_source_over_one_reference_dataset_is_refused() {
        let (specs, diags) = from(
            r#"
[refdb]
adapter = "sql"
dataset = "underlyings"
table = "a"
[refdb2]
adapter = "sql"
dataset = "underlyings"
table = "b"
"#,
        );
        assert_eq!(
            specs.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["refdb"]
        );
        let refusal = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("sources.refdb2.dataset"))
            .unwrap_or_else(|| panic!("{diags:?}"));
        assert_eq!(refusal.severity, Severity::Error);
        assert!(refusal.message.contains("'refdb'"), "{}", refusal.message);
    }

    #[test]
    fn a_snapshot_source_reads_its_poll_interval() {
        let (specs, _) = from(
            "[refdb]\nadapter = \"sql\"\ndataset = \"underlyings\"\ntable = \"t\"\npoll_interval = \"30s\"\n",
        );
        assert_eq!(specs[0].poll_interval, Duration::from_secs(30));
    }

    #[test]
    fn a_zero_snapshot_poll_interval_warns_and_uses_the_default() {
        let (specs, diags) = from(
            "[refdb]\nadapter = \"sql\"\ndataset = \"underlyings\"\ntable = \"t\"\npoll_interval = \"0\"\n",
        );
        assert_eq!(specs[0].poll_interval, DEFAULT_SNAPSHOT_POLL);
        assert!(
            diags.iter().any(|d| d.severity == Severity::Warning
                && d.path.as_deref() == Some("sources.refdb.poll_interval")),
            "{diags:?}"
        );
    }

    #[test]
    fn a_snapshot_source_without_a_table_is_refused() {
        let (specs, diags) = from("[refdb]\nadapter = \"sql\"\ndataset = \"underlyings\"\n");
        assert!(specs.is_empty());
        assert!(
            diags.iter().any(|d| d.severity == Severity::Error
                && d.path.as_deref() == Some("sources.refdb.table")),
            "{diags:?}"
        );
    }

    #[test]
    fn a_csv_dir_source_over_a_reference_dataset_is_refused() {
        let (specs, diags) = from("[files]\ndataset = \"underlyings\"\npaths = [\"/x/*.csv\"]\n");
        assert!(specs.is_empty());
        assert!(
            diags.iter().any(|d| d.severity == Severity::Error
                && d.path.as_deref() == Some("sources.files.dataset")
                && d.message.contains("reference")),
            "{diags:?}"
        );
    }

    #[test]
    fn table_is_ignored_with_a_warning_off_the_snapshot_shape() {
        let (specs, diags) = from(
            "[cvi]\nadapter = \"bus\"\ndataset = \"cvi_params\"\ndocument = \"cvi_params\"\ntopics = [\"a/>\"]\ntable = \"t\"\n",
        );
        assert_eq!(specs[0].table, None);
        assert!(
            diags.iter().any(|d| d.severity == Severity::Warning
                && d.path.as_deref() == Some("sources.cvi.table")),
            "{diags:?}"
        );
    }

    #[test]
    fn subscription_settings_on_a_snapshot_source_warn() {
        let (_, diags) = from(
            "[refdb]\nadapter = \"sql\"\ndataset = \"underlyings\"\ntable = \"t\"\ntopics = [\"a\"]\n",
        );
        assert!(
            diags.iter().any(|d| d.severity == Severity::Warning
                && d.path.as_deref() == Some("sources.refdb.topics")),
            "{diags:?}"
        );
    }
}
