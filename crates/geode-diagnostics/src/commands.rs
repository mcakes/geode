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

/// The `[log]` target suffixes `:level` accepts — mirrors
/// `geode_core::log::TARGETS`, spelled without the `geode::` prefix (as a
/// user types them and as `[log]` itself keys on).
const TARGETS: [&str; 6] = ["ingest", "query", "config", "session", "shell", "theme"];

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
    TARGETS.join(", ")
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
            if !TARGETS.contains(&target) {
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
/// shell ranks and shows them; this only knows the vocabulary. `cursor`
/// truncates `line` to the text so far (the word under the cursor is the
/// trailing token of that prefix, possibly empty).
pub fn completions(line: &str, cursor: usize) -> Vec<String> {
    let head = &line[..cursor.min(line.len())];
    let mut words: Vec<&str> = head.split(' ').collect();
    let partial = words.pop().unwrap_or("");
    match words.as_slice() {
        [] => ["section", "level", "overlay"]
            .iter()
            .filter(|c| c.starts_with(partial))
            .map(|c| (*c).to_string())
            .collect(),
        ["section"] => Section::ALL
            .iter()
            .filter(|s| s.name().starts_with(partial))
            .map(|s| format!("section {}", s.name()))
            .collect(),
        ["level"] => TARGETS
            .iter()
            .filter(|t| t.starts_with(partial))
            .map(|t| format!("level {t}"))
            .collect(),
        ["level", target] => {
            if !TARGETS.contains(target) {
                return Vec::new();
            }
            LEVELS
                .iter()
                .filter(|(n, _)| n.starts_with(partial))
                .map(|(n, _)| format!("level {target} {n}"))
                .collect()
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

    #[test]
    fn completions_offer_sections_then_targets_then_levels() {
        assert_eq!(completions("section l", 9), vec!["section log"]);
        assert_eq!(completions("level in", 8), vec!["level ingest"]);
        assert_eq!(
            completions("level ingest d", 14),
            vec!["level ingest debug"]
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
