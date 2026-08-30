//! The `.done` sentinel (spec §5.3). Permissive by contract: only source
//! time and the expected column list are required, every unrecognised field
//! is ignored, and a missing required field is a health error naming the
//! file rather than a failure to ingest anything at all.
//!
//! The production shape is an open question (spec §10.1); this parser is
//! written so that only those two fields need to survive being wrong.

use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sentinel {
    /// Authoritative source time: orders generations, drives as-of, and
    /// decides what is "most recent" (spec §4.4). Never file mtime.
    pub as_of: DateTime<Utc>,
    /// Column spelling as it appears in the CSV header.
    pub columns: Vec<String>,
    pub books: Vec<String>,
    pub row_count: Option<usize>,
    pub dataset: Option<String>,
    pub business_date: Option<String>,
}

#[derive(Debug)]
pub enum SentinelError {
    Json(serde_json::Error),
    MissingField(&'static str),
    BadTimestamp {
        value: String,
        source: chrono::ParseError,
    },
}

impl std::fmt::Display for SentinelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SentinelError::Json(e) => write!(f, "sentinel is not valid JSON: {e}"),
            SentinelError::MissingField(name) => {
                write!(f, "sentinel is missing required field '{name}'")
            }
            SentinelError::BadTimestamp { value, source } => {
                write!(
                    f,
                    "sentinel 'as_of' value '{value}' is not RFC 3339: {source}"
                )
            }
        }
    }
}

impl std::error::Error for SentinelError {}

/// Only the fields we understand; `serde` ignores the rest by default.
#[derive(Deserialize)]
struct Raw {
    as_of: Option<String>,
    columns: Option<Vec<String>>,
    #[serde(default)]
    books: Vec<String>,
    row_count: Option<usize>,
    dataset: Option<String>,
    business_date: Option<String>,
}

pub fn parse_sentinel(text: &str) -> Result<Sentinel, SentinelError> {
    let raw: Raw = serde_json::from_str(text).map_err(SentinelError::Json)?;
    let as_of_str = raw.as_of.ok_or(SentinelError::MissingField("as_of"))?;
    let as_of = DateTime::parse_from_rfc3339(&as_of_str)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|source| SentinelError::BadTimestamp {
            value: as_of_str,
            source,
        })?;
    let columns = raw.columns.ok_or(SentinelError::MissingField("columns"))?;
    Ok(Sentinel {
        as_of,
        columns,
        books: raw.books,
        row_count: raw.row_count,
        dataset: raw.dataset,
        business_date: raw.business_date,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"{
        "dataset": "risk_snapshot",
        "as_of": "2026-08-30T14:32:05Z",
        "business_date": "2026-08-30",
        "books": ["BK003", "BK011"],
        "row_count": 184203,
        "columns": ["Book", "LHU", "Delta01"]
    }"#;

    #[test]
    fn parses_the_documented_shape() {
        let s = parse_sentinel(FULL).unwrap();
        assert_eq!(s.columns, vec!["Book", "LHU", "Delta01"]);
        assert_eq!(s.books, vec!["BK003", "BK011"]);
        assert_eq!(s.row_count, Some(184_203));
        assert_eq!(s.dataset.as_deref(), Some("risk_snapshot"));
        assert_eq!(s.as_of.to_rfc3339(), "2026-08-30T14:32:05+00:00");
    }

    #[test]
    fn requires_only_as_of_and_columns() {
        let s = parse_sentinel(r#"{"as_of":"2026-08-30T14:32:05Z","columns":["A"]}"#).unwrap();
        assert_eq!(s.columns, vec!["A"]);
        assert!(s.books.is_empty());
        assert_eq!(s.row_count, None);
    }

    #[test]
    fn ignores_unrecognised_fields() {
        let text = r#"{
            "as_of": "2026-08-30T14:32:05Z",
            "columns": ["A"],
            "producer": "riskrun",
            "nested": {"anything": [1, 2, 3]}
        }"#;
        assert!(parse_sentinel(text).is_ok());
    }

    #[test]
    fn missing_required_fields_name_what_is_missing() {
        let e = parse_sentinel(r#"{"columns":["A"]}"#).unwrap_err();
        assert!(e.to_string().contains("as_of"), "{e}");
        let e = parse_sentinel(r#"{"as_of":"2026-08-30T14:32:05Z"}"#).unwrap_err();
        assert!(e.to_string().contains("columns"), "{e}");
    }

    #[test]
    fn malformed_json_and_bad_timestamps_are_errors_not_panics() {
        assert!(parse_sentinel("not json").is_err());
        assert!(parse_sentinel(r#"{"as_of":"yesterday","columns":["A"]}"#).is_err());
    }

    #[test]
    fn accepts_an_offset_other_than_utc() {
        let s = parse_sentinel(r#"{"as_of":"2026-08-30T14:32:05+02:00","columns":["A"]}"#).unwrap();
        // Normalized to UTC on parse: 14:32:05+02:00 is 12:32:05Z.
        assert_eq!(s.as_of.to_rfc3339(), "2026-08-30T12:32:05+00:00");
    }
}
