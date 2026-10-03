//! The Levels popover's pure state: which targets to list, their effective
//! level, and what a pick requests.
//!
//! `LogLevels` spells a target by its bare suffix (`"ingest"`, never
//! `"geode::ingest"`): that is the `[log]` key `from_doc` reads, what
//! `LogLevels::with` stores, and what the shell persists. Everything here
//! normalises to that spelling so a pick from this page lands exactly
//! where the palette's `Set log level…` pick lands.

use geode_core::log::{Level, LogLevels, TARGETS};

use crate::log::LogFilter;

const GEODE_PREFIX: &str = "geode::";

/// The level words in [`crate::log::LEVELS`] order.
const LEVEL_WORDS: [&str; 5] = ["error", "warn", "info", "debug", "trace"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelRow {
    /// `"default"` or the target's bare suffix.
    pub target: String,
    pub effective: Level,
    /// Whether `levels.targets` names it explicitly (else it inherits).
    pub explicit: bool,
}

/// The popover's transient state.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LevelsState {
    pub open: bool,
}

/// The read-only row: `request_level` files a target, and a `default`
/// target is not the default.
pub const DEFAULT_ROW: &str = "default";

pub fn level_word(level: Level) -> &'static str {
    LEVEL_WORDS[LogFilter::level_index(level)]
}

/// A target in the store's spelling: the `geode::` prefix dropped.
fn suffix(target: &str) -> &str {
    target.strip_prefix(GEODE_PREFIX).unwrap_or(target)
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
/// any configured target outside that list. Only [`TARGETS`] survive a
/// reload (`from_doc` drops other keys), so the popover offers no way to
/// add one; an extra row only shows what a hand-edited config set.
pub fn level_rows(levels: &LogLevels) -> Vec<LevelRow> {
    let mut rows = vec![LevelRow {
        target: DEFAULT_ROW.into(),
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
    use crate::log::LEVELS;

    #[test]
    fn rows_list_default_then_known_targets_then_extras_with_inheritance() {
        // Targets are spelled as `LogLevels::with` stores them: bare suffixes.
        let levels = LogLevels {
            default: Level::INFO,
            targets: vec![
                ("ingest".into(), Level::DEBUG),
                ("custom".into(), Level::TRACE),
            ],
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
}
