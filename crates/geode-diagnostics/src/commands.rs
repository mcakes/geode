//! The diagnostics tile's `:` line (Phase 4b Task 5, spec §4.6): `:section
//! <name>`, `:level <target> <level>`, `:overlay`. Pure — no gpui, no I/O —
//! same discipline as `geode_blotter::core::commands`.

use geode_core::log::Level;

/// One of the five sections a diagnostics tile can show (spec §4.6),
/// switched by `:section <name>` or `[`/`]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Sources,
    Data,
    Config,
    Log,
    Perf,
}

impl Section {
    pub const ALL: [Section; 5] = [
        Section::Sources,
        Section::Data,
        Section::Config,
        Section::Log,
        Section::Perf,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Section::Sources => "sources",
            Section::Data => "data",
            Section::Config => "config",
            Section::Log => "log",
            Section::Perf => "perf",
        }
    }
}

/// A parsed `:` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Section(Section),
    Level { target: String, level: Level },
    Overlay,
}

/// The `[log]` target suffixes `:level` accepts, derived from
/// `geode_core::log::TARGETS` rather than duplicated (Phase 4b Task 5 fix
/// round 1, MIN-2: a hardcoded copy here could silently drift from the
/// list `LogLevels::from_doc` actually validates `[log]` keys against —
/// add a target there and forget here, and `:level <it> debug` rejects
/// exactly the key `[log]` itself would accept). `strip_prefix` mirrors
/// `LogLevels::from_doc`'s own `t.strip_prefix("geode::") == Some(key.
/// as_str())` check for the same suffixes.
fn known_targets() -> impl Iterator<Item = &'static str> {
    geode_core::log::TARGETS
        .iter()
        .filter_map(|t| t.strip_prefix("geode::"))
}

const LEVELS: [(&str, Level); 5] = [
    ("error", Level::ERROR),
    ("warn", Level::WARN),
    ("info", Level::INFO),
    ("debug", Level::DEBUG),
    ("trace", Level::TRACE),
];

fn levels_hint() -> String {
    LEVELS
        .iter()
        .map(|(n, _)| *n)
        .collect::<Vec<_>>()
        .join(", ")
}

fn targets_hint() -> String {
    known_targets().collect::<Vec<_>>().join(", ")
}

/// Parse a `:` line, without its leading colon. `Err` is one line, shown
/// inline on the command line — same contract as every other module's
/// `command`/`parse`.
pub fn parse(line: &str) -> Result<Command, String> {
    let mut words = line.split_whitespace();
    match words.next() {
        Some("section") => {
            let name = words
                .next()
                .ok_or_else(|| "usage: section <name>".to_string())?;
            Section::ALL
                .iter()
                .find(|s| s.name() == name)
                .map(|s| Command::Section(*s))
                .ok_or_else(|| {
                    format!("unknown section '{name}' (sources, data, config, log, perf)")
                })
        }
        Some("level") => {
            let target = words
                .next()
                .ok_or_else(|| "usage: level <target> <level>".to_string())?;
            let level_str = words
                .next()
                .ok_or_else(|| "usage: level <target> <level>".to_string())?;
            if !known_targets().any(|t| t == target) {
                return Err(format!("unknown target '{target}' ({})", targets_hint()));
            }
            let level = LEVELS
                .iter()
                .find(|(n, _)| *n == level_str)
                .map(|(_, l)| *l)
                .ok_or_else(|| format!("unknown level '{level_str}' ({})", levels_hint()))?;
            Ok(Command::Level {
                target: target.to_string(),
                level,
            })
        }
        Some("overlay") => Ok(Command::Overlay),
        Some(other) => Err(format!("unknown command '{other}'")),
        None => Err("empty command".to_string()),
    }
}

/// Completion candidates for the word under `cursor` on a `:` line — the
/// shell ranks and shows them; this only knows the vocabulary. Each
/// candidate is a bare WORD for that position (`ingest`, never `level
/// ingest`): the shell splices the accepted candidate into the line in
/// place of the word under the cursor (`commandline::accept`), and calls
/// a typed word exact only when a candidate equals it, so a whole-line
/// candidate turned `:level ` + accept into `level level ingest` and Enter
/// on a fully typed `:level ingest debug` into `level ingest level ingest
/// debug` (seen on a display 2026-09-08). The whole vocabulary for the
/// position is returned, unfiltered, as the blotter's is: the shell's
/// fuzzy ranking decides what a partial word matches. `cursor`
/// truncates `line` to the text so far (the word under the cursor is the
/// trailing token of that prefix, possibly empty). `cursor` is walked
/// back to the nearest char boundary at or before it first (Phase 4b Task
/// 5 fix round 1, MIN-1): the caller's cursor should always land on one,
/// but this pure core must not depend on that and panic on the slice
/// below otherwise — mirrors `geode_blotter::core::commands::completions`'
/// own guard for the identical case, itself mirroring `commandline::
/// word_at`'s.
pub fn completions(line: &str, cursor: usize) -> Vec<String> {
    let mut cursor = cursor.min(line.len());
    while !line.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let head = &line[..cursor];
    let mut words: Vec<&str> = head.split(' ').collect();
    // The word under the cursor is the shell's to rank; only the words
    // before it decide which position is being completed.
    words.pop();
    match words.as_slice() {
        [] => ["section", "level", "overlay"]
            .iter()
            .map(|c| (*c).to_string())
            .collect(),
        ["section"] => Section::ALL.iter().map(|s| s.name().to_string()).collect(),
        ["level"] => known_targets().map(str::to_string).collect(),
        ["level", target] => {
            if !known_targets().any(|t| t == *target) {
                return Vec::new();
            }
            LEVELS.iter().map(|(n, _)| (*n).to_string()).collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_parses_each_name_and_rejects_unknown() {
        assert!(matches!(
            parse("section log"),
            Ok(Command::Section(Section::Log))
        ));
        assert!(matches!(
            parse("section sources"),
            Ok(Command::Section(Section::Sources))
        ));
        assert!(matches!(
            parse("section data"),
            Ok(Command::Section(Section::Data))
        ));
        assert!(matches!(
            parse("section config"),
            Ok(Command::Section(Section::Config))
        ));
        assert!(matches!(
            parse("section perf"),
            Ok(Command::Section(Section::Perf))
        ));
        assert_eq!(
            parse("section nope").unwrap_err(),
            "unknown section 'nope' (sources, data, config, log, perf)"
        );
    }

    #[test]
    fn level_parses_target_and_level() {
        assert!(matches!(
            parse("level ingest debug"),
            Ok(Command::Level { .. })
        ));
        assert_eq!(
            parse("level ingest debug"),
            Ok(Command::Level {
                target: "ingest".into(),
                level: Level::DEBUG
            })
        );
        assert_eq!(
            parse("level ingest loud").unwrap_err(),
            "unknown level 'loud' (error, warn, info, debug, trace)"
        );
        assert_eq!(
            parse("level nope info").unwrap_err(),
            "unknown target 'nope' (ingest, query, config, session, shell, theme)"
        );
    }

    #[test]
    fn overlay_parses() {
        assert_eq!(parse("overlay"), Ok(Command::Overlay));
    }

    #[test]
    fn an_unknown_or_empty_command_is_an_error() {
        assert!(parse("bogus").is_err());
        assert!(parse("").is_err());
    }

    /// MIN-2: `known_targets` derives from `geode_core::log::TARGETS`
    /// rather than a hand-copied list — every one of `[log]`'s own
    /// accepted target suffixes must parse for `:level` too.
    #[test]
    fn every_log_target_suffix_is_a_known_level_target() {
        for target in geode_core::log::TARGETS {
            let suffix = target.strip_prefix("geode::").unwrap();
            assert!(
                matches!(
                    parse(&format!("level {suffix} debug")),
                    Ok(Command::Level { .. })
                ),
                "{suffix} (from geode_core::log::TARGETS) must be a known :level target"
            );
        }
    }

    /// MIN-1: a cursor that does not land on a char boundary must not
    /// panic — clamp back to the nearest one at or before it, same
    /// guard `geode_blotter::core::commands::completions` and
    /// `commandline::word_at` both already carry for this exact case.
    #[test]
    fn completions_clamp_a_cursor_inside_a_multibyte_char() {
        let line = "section ✓og";
        // "✓" is 3 bytes (U+2713); its first byte sits right after
        // "section ", so `cursor` inside it (not on a char boundary) must
        // not panic and should behave as if clamped to the boundary
        // before it.
        let mid_char = line.find('✓').unwrap() + 1;
        assert!(!line.is_char_boundary(mid_char));
        let result = std::panic::catch_unwind(|| completions(line, mid_char));
        assert!(result.is_ok(), "must not panic on a non-boundary cursor");
    }

    /// Seen on a display 2026-09-08: `:level ` then accepting the
    /// `ingest` candidate produced `level level ingest`, and Enter on a
    /// fully typed `:level ingest debug` "accepted" its own last word
    /// into `level ingest level ingest debug`. A candidate is the WORD
    /// under the cursor: the shell splices it into the line in place of
    /// that word (`commandline::accept`) and calls a word exact only
    /// when a candidate equals it (`resolve_submit`). The blotter's
    /// vocabulary is bare words for the same reason. Never the line.
    #[test]
    fn a_candidate_is_the_word_under_the_cursor_not_the_line() {
        use geode_shell::commandline::{Submit, accept, rank_candidates, resolve_submit, word_at};
        for (line, pick, expect) in [
            ("section ", "log", "section log"),
            ("level ", "ingest", "level ingest"),
            ("level ingest ", "debug", "level ingest debug"),
        ] {
            let cursor = line.len();
            let words = completions(line, cursor);
            assert!(
                words.iter().any(|w| w == pick),
                "{line:?}: {pick} is offered as a bare word, got {words:?}"
            );
            let (out, _) = accept(line, word_at(line, cursor), pick);
            assert_eq!(out, expect, "accepting {pick} on {line:?}");
        }
        // Enter on the fully typed line runs it as typed: `debug` is exact
        // against the vocabulary, so nothing is re-accepted.
        let line = "level ingest debug";
        let words = completions(line, line.len());
        let ranked = rank_candidates(&words, "debug");
        assert!(
            matches!(
                resolve_submit(line, line.len(), &ranked, &words),
                Submit::Run(_)
            ),
            "a typed level is exact, not re-accepted"
        );
    }

    #[test]
    fn completions_offer_sections_then_targets_then_levels() {
        // The whole vocabulary for the position, as bare words — the
        // shell's ranking narrows it to the partial word.
        let sections: Vec<String> = Section::ALL.iter().map(|s| s.name().to_string()).collect();
        assert_eq!(completions("section l", 9), sections);
        let targets: Vec<String> = known_targets().map(str::to_string).collect();
        assert!(targets.iter().any(|t| t == "ingest"));
        assert_eq!(completions("level in", 8), targets);
        assert_eq!(
            completions("level ingest d", 14),
            vec!["error", "warn", "info", "debug", "trace"]
        );
    }

    #[test]
    fn completions_at_the_start_offer_the_three_commands() {
        let c = completions("", 0);
        assert_eq!(c, vec!["section", "level", "overlay"]);
    }

    #[test]
    fn completions_for_an_unknown_level_target_are_empty() {
        assert!(completions("level nope ", 11).is_empty());
    }
}
