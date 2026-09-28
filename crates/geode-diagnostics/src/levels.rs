//! The Levels popover's pure state: which targets to list, their effective
//! level, and what a pick requests.
//!
//! `LogLevels` spells a target by its bare suffix (`"ingest"`, never
//! `"geode::ingest"`): that is the `[log]` key `from_doc` reads, what
//! `LogLevels::with` stores, and what the shell persists. Everything here
//! normalises to that spelling so a pick from this page lands exactly
//! where the palette's `Set log level…` pick lands.

use geode_core::log::{Level, LogLevels, TARGETS};

use crate::log::{LEVELS, LogFilter};

const GEODE_PREFIX: &str = "geode::";

/// The level words in [`LEVELS`] order.
const LEVEL_WORDS: [&str; 5] = ["error", "warn", "info", "debug", "trace"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelRow {
    /// `"default"` or the target's bare suffix.
    pub target: String,
    pub effective: Level,
    /// Whether `levels.targets` names it explicitly (else it inherits).
    pub explicit: bool,
}

/// The popover's transient state: whether it is open and what the
/// new-target field holds (mirrored from the input on every change so
/// paint reads no entity).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LevelsState {
    pub open: bool,
    pub new_target: String,
}

pub fn level_word(level: Level) -> &'static str {
    LEVEL_WORDS[LogFilter::level_index(level)]
}

/// A target in the store's spelling: the `geode::` prefix dropped.
fn suffix(target: &str) -> &str {
    target.strip_prefix(GEODE_PREFIX).unwrap_or(target)
}

/// The target a new-target field's text requests, or `None` when it
/// names nothing: trimmed, in the store's spelling, and not the default
/// row, which `request_level` cannot set (it would file a `default`
/// target rather than move the default).
pub fn new_target(text: &str) -> Option<String> {
    let t = suffix(text.trim());
    (!t.is_empty() && t != "default").then(|| t.to_string())
}

/// The level a target logs at: its explicit entry, else the longest
/// configured prefix (`ingest` covers `ingest::csv`), else the default.
pub fn effective_level(levels: &LogLevels, target: &str) -> Level {
    let short = suffix(target);
    levels
        .targets
        .iter()
        .map(|(t, l)| (suffix(t), *l))
        .filter(|(t, _)| {
            short == *t || (short.starts_with(t) && short[t.len()..].starts_with("::"))
        })
        .max_by_key(|(t, _)| t.len())
        .map(|(_, l)| l)
        .unwrap_or(levels.default)
}

/// `"default"` first, then the known targets in [`TARGETS`] order, then
/// any configured target outside that list.
pub fn level_rows(levels: &LogLevels) -> Vec<LevelRow> {
    let mut rows = vec![LevelRow {
        target: "default".into(),
        effective: levels.default,
        explicit: true,
    }];
    let mut names: Vec<&str> = TARGETS.iter().map(|t| suffix(t)).collect();
    for (t, _) in &levels.targets {
        let short = suffix(t);
        if !names.contains(&short) {
            names.push(short);
        }
    }
    for name in names {
        rows.push(LevelRow {
            explicit: levels.targets.iter().any(|(t, _)| suffix(t) == name),
            effective: effective_level(levels, name),
            target: name.to_string(),
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_list_default_then_known_targets_then_extras_with_inheritance() {
        // Targets are spelled as `LogLevels::with` stores them: bare suffixes.
        let levels = LogLevels {
            default: Level::INFO,
            targets: vec![("ingest".into(), Level::DEBUG), ("custom".into(), Level::TRACE)],
        };
        let rows = level_rows(&levels);
        assert_eq!(
            rows[0],
            LevelRow {
                target: "default".into(),
                effective: Level::INFO,
                explicit: true
            }
        );
        let ingest = rows.iter().find(|r| r.target == "ingest").unwrap();
        assert_eq!((ingest.effective, ingest.explicit), (Level::DEBUG, true));
        let query = rows.iter().find(|r| r.target == "query").unwrap();
        assert_eq!((query.effective, query.explicit), (Level::INFO, false));
        assert_eq!(rows.last().unwrap().target, "custom");
        assert_eq!(rows.len(), 1 + TARGETS.len() + 1);
        assert_eq!(
            effective_level(&levels, "geode::ingest::csv"),
            Level::DEBUG,
            "prefix inherits"
        );
        assert_eq!(
            effective_level(&levels, "geode::ingestion"),
            Level::INFO,
            "a longer name is not a child of the shorter one"
        );
    }

    #[test]
    fn a_prefixed_store_entry_is_read_by_its_suffix() {
        let levels = LogLevels {
            default: Level::WARN,
            targets: vec![("geode::query".into(), Level::TRACE)],
        };
        assert_eq!(effective_level(&levels, "query"), Level::TRACE);
        let rows = level_rows(&levels);
        assert_eq!(rows.len(), 1 + TARGETS.len(), "no duplicate query row");
        let query = rows.iter().find(|r| r.target == "query").unwrap();
        assert!(query.explicit);
    }

    #[test]
    fn level_words_follow_the_filter_order() {
        for (ix, level) in LEVELS.iter().enumerate() {
            assert_eq!(level_word(*level), LEVEL_WORDS[ix]);
            assert_eq!(level_word(*level), level.to_string().to_ascii_lowercase());
        }
    }

    #[test]
    fn a_new_target_is_trimmed_stripped_and_never_the_default() {
        assert_eq!(new_target("  geode::custom "), Some("custom".into()));
        assert_eq!(new_target("custom"), Some("custom".into()));
        assert_eq!(new_target("   "), None);
        assert_eq!(new_target("default"), None);
        assert_eq!(new_target("geode::"), None);
    }
}
