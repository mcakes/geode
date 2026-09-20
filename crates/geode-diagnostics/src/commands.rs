//! The diagnostics tile's `:` line (Phase 4b Task 5, spec §4.6; command-line
//! locality 2026-09-20): `:section <name>`. `:level` and `:overlay` were
//! app-wide and are refusals now (`REFUSED_LEVEL`/`REFUSED_OVERLAY`); the
//! palette's `Set log level…` and `Toggle performance overlay` are their
//! doors. Pure — no gpui, no I/O — same discipline as
//! `geode_blotter::core::commands`.

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
    /// A word this line no longer runs because it was app-wide
    /// (command-line locality spec §5): the message names the door.
    Refused(&'static str),
}

/// The words `completions` offers at the start of a line; the tile's
/// sweep test reads it.
pub const COMMANDS: [&str; 1] = ["section"];

pub const REFUSED_LEVEL: &str = "log levels are app-wide — Set log level… in the palette";
pub const REFUSED_OVERLAY: &str =
    "the overlay is app-wide — Toggle performance overlay (mod+shift+p)";

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
        Some("level") => Ok(Command::Refused(REFUSED_LEVEL)),
        Some("overlay") => Ok(Command::Refused(REFUSED_OVERLAY)),
        Some(other) => Err(format!("unknown command '{other}'")),
        None => Err("empty command".to_string()),
    }
}

/// Completion candidates for the word under `cursor` on a `:` line — the
/// shell ranks and shows them; this only knows the vocabulary. Each
/// candidate is a bare WORD for that position (`ingest`, never `level
/// ingest`): the shell splices the accepted candidate into the line in
/// place of the word under the cursor (`commandline::accept`), and calls
/// a typed word exact only when the vocabulary contains it, so a
/// whole-line candidate turned `:level ` + accept into `level level
/// ingest` and Enter on a fully typed `:level ingest debug` into `level
/// ingest level ingest debug` (seen on a display 2026-09-08). The whole
/// vocabulary for the position is returned, unfiltered, as the blotter's
/// is: the shell's fuzzy (subsequence) ranking decides what a partial
/// word matches — which also means Enter on a short prefix that several
/// words contain (the blotter's `:so`, matched by more than one of its
/// commands) is refused as ambiguous rather than run; the old prefix
/// filter used to auto-run it. Words are split the way the
/// shell's `commandline::word_at` splits them — any whitespace or a
/// comma — so the position this computes is the word the shell will
/// splice over. `cursor` truncates `line` to the text so far (the word
/// under the cursor is the trailing token of that prefix, possibly
/// empty). `cursor` is walked
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
    let mut words: Vec<&str> = head
        .split(|c: char| c.is_whitespace() || c == ',')
        .collect();
    // The word under the cursor is the shell's to rank; only the words
    // before it decide which position is being completed (a doubled
    // space is no word, as `parse`'s `split_whitespace` agrees).
    words.pop();
    words.retain(|w| !w.is_empty());
    match words.as_slice() {
        [] => COMMANDS.iter().map(|c| (*c).to_string()).collect(),
        ["section"] => Section::ALL.iter().map(|s| s.name().to_string()).collect(),
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
    fn an_unknown_or_empty_command_is_an_error() {
        assert!(parse("bogus").is_err());
        assert!(parse("").is_err());
    }

    /// Command-line locality (2026-09-20): `level` and `overlay` are
    /// app-wide and left the line as refusals naming their door.
    #[test]
    fn level_and_overlay_are_refusals_and_not_completions() {
        assert_eq!(
            parse("level ingest debug"),
            Ok(Command::Refused(REFUSED_LEVEL))
        );
        assert_eq!(parse("level"), Ok(Command::Refused(REFUSED_LEVEL)));
        assert_eq!(parse("overlay"), Ok(Command::Refused(REFUSED_OVERLAY)));
        assert_eq!(completions("", 0), vec!["section"]);
        assert!(completions("level ", 6).is_empty());
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
    /// when the vocabulary contains it (`resolve_submit`). The blotter's
    /// vocabulary is bare words for the same reason. Never the line.
    #[test]
    fn a_candidate_is_the_word_under_the_cursor_not_the_line() {
        use geode_shell::commandline::{Submit, accept, rank_candidates, resolve_submit, word_at};
        let (line, pick, expect) = ("section ", "log", "section log");
        let cursor = line.len();
        let words = completions(line, cursor);
        assert!(
            words.iter().any(|w| w == pick),
            "{line:?}: {pick} is offered as a bare word, got {words:?}"
        );
        let (out, _) = accept(line, word_at(line, cursor), pick);
        assert_eq!(out, expect, "accepting {pick} on {line:?}");
        // Enter on the fully typed line runs it as typed: `log` is exact
        // against the vocabulary, so nothing is re-accepted.
        let line = "section log";
        let words = completions(line, line.len());
        let ranked = rank_candidates(&words, "log");
        assert!(
            matches!(
                resolve_submit(line, line.len(), &ranked, &words),
                Submit::Run(_)
            ),
            "a typed section is exact, not re-accepted"
        );
    }

    #[test]
    fn completions_offer_sections_after_the_section_word() {
        // The whole vocabulary for the position, as bare words — the
        // shell's ranking narrows it to the partial word.
        assert_eq!(
            completions("section l", 9),
            vec!["sources", "data", "config", "log", "perf"]
        );
    }

    /// The position is computed over the same delimiters the shell's
    /// `commandline::word_at` uses — any whitespace or a comma — so the
    /// vocabulary offered is for the word the shell will splice over. A
    /// doubled space is no word, as `parse`'s `split_whitespace` agrees.
    #[test]
    fn completions_split_words_the_way_the_shell_does() {
        let sections = vec!["sources", "data", "config", "log", "perf"];
        assert_eq!(completions("section,l", 9), sections, "comma");
        assert_eq!(completions("section\tl", 9), sections, "tab");
        assert_eq!(completions("section  l", 10), sections, "doubled space");
    }

    #[test]
    fn completions_at_the_start_offer_section_alone() {
        let c = completions("", 0);
        assert_eq!(c, vec!["section"]);
    }
}
