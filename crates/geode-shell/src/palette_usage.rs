//! Bounded usage history for palette ranking, with caller-supplied Unix time.
//! Each row records a count and last-use time. The palette snapshots bonuses at
//! open, keeping ranking work per query independent of clock reads and map lookup.
//!
//! History persists in session.toml, which the config reload scanner excludes.
//! Recording palette choices therefore participates in session saving without
//! triggering configuration reloads.

use std::collections::BTreeMap;

/// Maximum retained records. Recording prunes by lowest current bonus,
/// then oldest last-use time, then lexicographically first key.
pub const MAX_ENTRIES: usize = 256;

/// Largest row bonus: 18 points from recency plus capped frequency.
/// Only textual matches receive it; it can change ordering among close matches.
/// The scoring relationship is checked against two RUN_BONUS values below.
pub const MAX_BONUS: u32 = RECENCY_BONUS[0] + FREQUENCY_CAP;
const _: () = assert!(MAX_BONUS == 2 * crate::palette::RUN_BONUS);

/// Recency, bucketed by how long ago the last use was: within the last
/// hour, day, week, or longer ago. Buckets rather than a smooth decay so
/// two commands used minutes apart tie on recency and let frequency
/// decide, instead of the later keystroke always winning.
const RECENCY_BONUS: [u32; 4] = [12, 8, 4, 1];
const RECENCY_EDGES: [u64; 3] = [60 * 60, 24 * 60 * 60, 7 * 24 * 60 * 60];

/// Frequency contribution: count minus one, saturated at six.
/// The seventh and later uses therefore receive the maximum frequency bonus.
const FREQUENCY_CAP: u32 = 6;

/// One row's record: how many times it was chosen and when it was last
/// chosen (unix seconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UseRecord {
    pub count: u32,
    pub last_used: u64,
}

impl UseRecord {
    /// This record's ranking bonus as of `now`. A `last_used` ahead of
    /// `now` (a clock that went backwards between sessions) reads as just
    /// used rather than as an underflow.
    pub fn bonus(&self, now: u64) -> u32 {
        let age = now.saturating_sub(self.last_used);
        let bucket = RECENCY_EDGES
            .iter()
            .take_while(|&&edge| age >= edge)
            .count();
        RECENCY_BONUS[bucket] + self.count.saturating_sub(1).min(FREQUENCY_CAP)
    }
}

/// The per-row usage map, keyed by [`crate::palette::PaletteItem::usage_key`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaletteUsage {
    entries: BTreeMap<String, UseRecord>,
}

impl PaletteUsage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get(&self, key: &str) -> Option<&UseRecord> {
        self.entries.get(key)
    }

    /// Count one more use of `key` at `now`, then prune to
    /// [`MAX_ENTRIES`] by dropping the lowest-bonus entry as of `now`.
    pub fn record(&mut self, key: &str, now: u64) {
        match self.entries.get_mut(key) {
            Some(record) => {
                record.count = record.count.saturating_add(1);
                record.last_used = now;
            }
            None => {
                self.entries.insert(
                    key.to_string(),
                    UseRecord {
                        count: 1,
                        last_used: now,
                    },
                );
            }
        }
        while self.entries.len() > MAX_ENTRIES {
            let Some(lowest) = self
                .entries
                .iter()
                .min_by_key(|(key, record)| (record.bonus(now), record.last_used, *key))
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.entries.remove(&lowest);
        }
    }

    /// The ranking bonus for `key` as of `now`: 0 for a row never chosen,
    /// otherwise [`UseRecord::bonus`].
    pub fn bonus(&self, key: &str, now: u64) -> u32 {
        self.entries.get(key).map_or(0, |record| record.bonus(now))
    }

    /// Serialize count and last-used fields per key; timestamps saturate at i64::MAX.
    pub fn to_toml(&self) -> toml::Table {
        let mut table = toml::Table::new();
        for (key, record) in &self.entries {
            let mut entry = toml::Table::new();
            entry.insert(
                "count".to_string(),
                toml::Value::Integer(i64::from(record.count)),
            );
            entry.insert(
                "last_used".to_string(),
                toml::Value::Integer(i64::try_from(record.last_used).unwrap_or(i64::MAX)),
            );
            table.insert(key.clone(), toml::Value::Table(entry));
        }
        table
    }

    /// Read usage entries independently, warning and skipping malformed tables
    /// or missing/non-integer/negative fields. Counts saturate at u32::MAX. Oversized
    /// maps retain the most recent records, then highest counts, then smallest keys;
    /// this load-time ordering does not use the time-dependent ranking bonus.
    pub fn from_toml(table: &toml::Table, warnings: &mut Vec<String>) -> Self {
        let mut entries = BTreeMap::new();
        for (key, value) in table {
            let Some(entry) = value.as_table() else {
                warnings.push(format!("palette.usage.{key} is not a table; ignored"));
                continue;
            };
            let field = |name: &str| -> Option<u64> {
                entry
                    .get(name)
                    .and_then(toml::Value::as_integer)
                    .and_then(|n| u64::try_from(n).ok())
            };
            let (Some(count), Some(last_used)) = (field("count"), field("last_used")) else {
                warnings.push(format!(
                    "palette.usage.{key} needs non-negative integer count and last_used; ignored"
                ));
                continue;
            };
            entries.insert(
                key.clone(),
                UseRecord {
                    count: u32::try_from(count).unwrap_or(u32::MAX),
                    last_used,
                },
            );
        }
        if entries.len() > MAX_ENTRIES {
            let mut ranked: Vec<(&String, &UseRecord)> = entries.iter().collect();
            ranked.sort_by_key(|(key, record)| {
                std::cmp::Reverse((record.last_used, record.count, std::cmp::Reverse(*key)))
            });
            let keep: std::collections::BTreeSet<String> = ranked
                .into_iter()
                .take(MAX_ENTRIES)
                .map(|(key, _)| key.clone())
                .collect();
            entries.retain(|key, _| keep.contains(key));
        }
        PaletteUsage { entries }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 60 * 60;
    const DAY: u64 = 24 * HOUR;
    const NOW: u64 = 1_800_000_000;

    #[test]
    fn a_key_never_chosen_earns_no_bonus() {
        let usage = PaletteUsage::new();
        assert_eq!(usage.bonus("action:x", NOW), 0);
    }

    #[test]
    fn a_recorded_key_earns_a_bonus() {
        let mut usage = PaletteUsage::new();
        usage.record("action:x", NOW);
        assert!(usage.bonus("action:x", NOW) > 0);
        assert_eq!(usage.get("action:x").map(|r| r.count), Some(1));
    }

    #[test]
    fn a_more_recent_use_earns_more_than_an_older_one_at_equal_count() {
        let mut usage = PaletteUsage::new();
        usage.record("recent", NOW - 5 * 60);
        usage.record("older", NOW - 2 * DAY);
        usage.record("ancient", NOW - 30 * DAY);
        let (recent, older, ancient) = (
            usage.bonus("recent", NOW),
            usage.bonus("older", NOW),
            usage.bonus("ancient", NOW),
        );
        assert!(recent > older, "{recent} vs {older}");
        assert!(older > ancient, "{older} vs {ancient}");
        assert!(ancient > 0, "an old use still counts for something");
    }

    #[test]
    fn more_uses_earn_more_at_equal_recency() {
        let mut usage = PaletteUsage::new();
        usage.record("once", NOW);
        usage.record("thrice", NOW);
        usage.record("thrice", NOW);
        usage.record("thrice", NOW);
        assert!(usage.bonus("thrice", NOW) > usage.bonus("once", NOW));
    }

    #[test]
    fn uses_minutes_apart_tie_on_recency_so_frequency_decides() {
        let mut usage = PaletteUsage::new();
        usage.record("later-once", NOW - 60);
        usage.record("earlier-twice", NOW - 20 * 60);
        usage.record("earlier-twice", NOW - 10 * 60);
        assert!(usage.bonus("earlier-twice", NOW) > usage.bonus("later-once", NOW));
    }

    #[test]
    fn the_bonus_is_capped_at_two_run_bonuses() {
        let mut usage = PaletteUsage::new();
        for _ in 0..100 {
            usage.record("habit", NOW);
        }
        assert_eq!(usage.bonus("habit", NOW), MAX_BONUS);
        assert_eq!(MAX_BONUS, 2 * crate::palette::RUN_BONUS);
    }

    #[test]
    fn a_clock_that_went_backwards_reads_as_just_used() {
        let mut usage = PaletteUsage::new();
        usage.record("future", NOW + DAY);
        assert_eq!(usage.bonus("future", NOW), usage.bonus("future", NOW + DAY));
    }

    #[test]
    fn recording_past_the_cap_drops_the_lowest_bonus_entry() {
        let mut usage = PaletteUsage::new();
        // The first entry is the oldest and least used — the one to go.
        usage.record("stale", NOW - 60 * DAY);
        for i in 1..MAX_ENTRIES {
            usage.record(&format!("k{i}"), NOW - DAY);
        }
        assert_eq!(usage.len(), MAX_ENTRIES);
        usage.record("newest", NOW);
        assert_eq!(usage.len(), MAX_ENTRIES);
        assert!(
            usage.get("stale").is_none(),
            "the lowest-bonus entry is dropped"
        );
        assert!(
            usage.get("newest").is_some(),
            "the entry just recorded is kept"
        );
        assert!(usage.get("k1").is_some());
    }

    #[test]
    fn toml_round_trips_every_entry() {
        let mut usage = PaletteUsage::new();
        usage.record("action:workspace::close_tile", NOW - HOUR);
        usage.record("action:workspace::close_tile", NOW);
        usage.record("theme:Gruvbox Dark", NOW - DAY);
        let table = usage.to_toml();
        let mut warnings = Vec::new();
        let back = PaletteUsage::from_toml(&table, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(back, usage);
        // And through real TOML text, since the keys carry `::` and spaces.
        let text = toml::to_string(&table).unwrap();
        let reparsed: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(PaletteUsage::from_toml(&reparsed, &mut warnings), usage);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn a_malformed_entry_is_dropped_with_a_warning_and_the_rest_kept() {
        let text = r#"
            "action:good" = { count = 2, last_used = 1800000000 }
            "action:not-a-table" = 3
            "action:negative" = { count = -1, last_used = 1800000000 }
            "action:missing" = { count = 1 }
        "#;
        let table: toml::Table = toml::from_str(text).unwrap();
        let mut warnings = Vec::new();
        let usage = PaletteUsage::from_toml(&table, &mut warnings);
        assert_eq!(usage.len(), 1);
        assert_eq!(
            usage.get("action:good"),
            Some(&UseRecord {
                count: 2,
                last_used: 1_800_000_000
            })
        );
        assert_eq!(warnings.len(), 3, "{warnings:?}");
    }

    /// Load-time trimming keeps the most recent records, breaking ties by frequency.
    #[test]
    fn an_oversize_file_is_pruned_once_on_load_keeping_the_most_recent() {
        let mut usage = PaletteUsage::new();
        for i in 0..(MAX_ENTRIES + 50) {
            usage.entries.insert(
                format!("k{i}"),
                UseRecord {
                    count: 1,
                    last_used: NOW - i as u64,
                },
            );
        }
        let mut warnings = Vec::new();
        let loaded = PaletteUsage::from_toml(&usage.to_toml(), &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(loaded.len(), MAX_ENTRIES);
        assert!(loaded.get("k0").is_some(), "the most recent entry survives");
        assert!(
            loaded.get(&format!("k{}", MAX_ENTRIES + 49)).is_none(),
            "the least recent entry is dropped"
        );
    }
}
