//! The panel's `:` line (market-data spec §8.3). Pure — no gpui, no
//! entity, no I/O — the same discipline `geode_blotter::core::commands`
//! and `geode_diagnostics::commands` keep, and the reason this half is
//! tested without a window.
//!
//! The vocabulary is `key <value>`, `revert`, `bump <delta> [row|col]`,
//! `rebase`, `discard`, `upload`. Part 3 builds `key` for real; the rest
//! parse here and the tile answers which task lands them, so the
//! grammar a trader types is the grammar Tasks 7 and 8 wire up rather
//! than one invented twice.

use geode_core::document::KEY_SEPARATOR;

/// The separator a multi-part document key is typed and displayed with.
///
/// A document's storage identity joins its key parts with
/// [`KEY_SEPARATOR`] (`\u{1f}`), which is deliberately untypeable — so
/// the panel's own spelling of the same key is `/`-separated, matching
/// the completions the catalog offers ([`crate::tile`]'s
/// `catalog_keys`).
pub const KEY_DISPLAY_SEPARATOR: char = '/';

/// Which way `:bump` walks from the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BumpAxis {
    /// Every cell in the cursor's row — the default, because a term's
    /// whole node ladder is the shape a trader nudges.
    #[default]
    Row,
    Col,
}

/// A parsed `:` line.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// The document key, already split on [`KEY_DISPLAY_SEPARATOR`] into
    /// the dataset's declared `key` order — what `DocumentParams::
    /// document_key` wants.
    Key(Vec<String>),
    Revert,
    Bump {
        delta: f64,
        axis: BumpAxis,
    },
    Rebase,
    Discard,
    Upload,
}

/// Every verb, in the order completions offer them. `rebase`/`discard`
/// are filtered by the caller's `behind` flag (spec §8.3: they are
/// offered only while a newer generation sits under the draft) — listed
/// here so one table is the vocabulary and the filter is one line.
const VERBS: [&str; 6] = ["key", "revert", "bump", "rebase", "discard", "upload"];

fn behind_only(verb: &str) -> bool {
    verb == "rebase" || verb == "discard"
}

/// Parse a `:` line, without its leading colon. `Err` is one line, shown
/// inline on the command line — the same contract every other module's
/// `command` keeps.
///
/// Parsing is deliberately complete even for the verbs Part 3 does not
/// execute: the tile answers "lands in Task 7/8" for those, so a typo is
/// still reported as a typo ("unknown command 'rebse'") rather than
/// being indistinguishable from a verb that is merely not built yet.
pub fn parse(line: &str) -> Result<Command, String> {
    let mut words = line.split_whitespace();
    match words.next() {
        Some("key") => {
            let value = words
                .next()
                .ok_or_else(|| "usage: key <value>".to_string())?;
            if words.next().is_some() {
                return Err(format!(
                    "a document key is one word; parts are separated by '{KEY_DISPLAY_SEPARATOR}'"
                ));
            }
            let parts: Vec<String> = value
                .split(KEY_DISPLAY_SEPARATOR)
                .map(str::to_string)
                .collect();
            if parts.iter().any(|p| p.is_empty()) {
                return Err(format!("'{value}' has an empty key part"));
            }
            // A key part carrying the storage separator would join back
            // into a different key than the one typed — refused here
            // rather than silently reinterpreted downstream.
            if parts.iter().any(|p| p.contains(KEY_SEPARATOR)) {
                return Err("a key part may not contain the storage separator".to_string());
            }
            Ok(Command::Key(parts))
        }
        Some("revert") => Ok(Command::Revert),
        Some("bump") => {
            let delta = words
                .next()
                .ok_or_else(|| "usage: bump <delta> [row|col]".to_string())?;
            let delta: f64 = delta
                .parse()
                .map_err(|_| format!("'{delta}' is not a number"))?;
            if !delta.is_finite() {
                return Err(format!("'{delta}' is not a finite number"));
            }
            let axis = match words.next() {
                None | Some("row") => BumpAxis::Row,
                Some("col") => BumpAxis::Col,
                Some(other) => return Err(format!("unknown axis '{other}' (row, col)")),
            };
            Ok(Command::Bump { delta, axis })
        }
        Some("rebase") => Ok(Command::Rebase),
        Some("discard") => Ok(Command::Discard),
        Some("upload") => Ok(Command::Upload),
        Some(other) => Err(format!("unknown command '{other}'")),
        None => Err("empty command".to_string()),
    }
}

/// Candidates for the word under `cursor`. Each is a bare WORD for that
/// position (`SPX.Z`, never `key SPX.Z`): the shell splices the accepted
/// one over the word under the cursor (`commandline::accept`), so a
/// whole-line candidate doubles the line. The whole vocabulary for the
/// position is returned unfiltered — the shell's own fuzzy ranking
/// narrows it, and Enter refuses an ambiguous prefix rather than
/// guessing.
///
/// `keys` is the catalog's own document keys in this panel's dataset, in
/// the `/`-separated display spelling, and `behind` gates `rebase`/
/// `discard` (spec §8.3). `behind` is a fourth parameter the brief's
/// sketch left out: the rule it implements is the brief's own, and this
/// core has no `Draft` to read it off.
pub fn completions(line: &str, cursor: usize, keys: &[String], behind: bool) -> Vec<String> {
    // The caller's cursor should land on a char boundary; this pure core
    // must not panic on the slice below if it ever does not — the same
    // guard `geode_blotter::core::commands::completions` keeps, itself
    // mirroring `commandline::word_at`'s.
    let mut cursor = cursor.min(line.len());
    while !line.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let head = &line[..cursor];
    // Split the way the shell's `commandline::word_at` splits, so the
    // position computed here is the word the shell will splice over.
    let mut words: Vec<&str> = head
        .split(|c: char| c.is_whitespace() || c == ',')
        .collect();
    // The word under the cursor is the shell's to rank; only the words
    // before it decide which position is being completed.
    words.pop();
    words.retain(|w| !w.is_empty());
    match words.as_slice() {
        [] => VERBS
            .iter()
            .filter(|v| behind || !behind_only(v))
            .map(|v| (*v).to_string())
            .collect(),
        ["key"] => keys.to_vec(),
        // `bump`'s delta is a number nothing can complete; its axis is a
        // two-word vocabulary.
        ["bump", _] => vec!["row".to_string(), "col".to_string()],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_splits_on_the_display_separator_and_refuses_a_blank_part() {
        assert_eq!(parse("key SPX.Z"), Ok(Command::Key(vec!["SPX.Z".into()])));
        assert_eq!(
            parse("key SPX.Z/EOD"),
            Ok(Command::Key(vec!["SPX.Z".into(), "EOD".into()])),
            "a multi-part key is typed with the display separator"
        );
        assert!(parse("key").is_err(), "no value");
        assert!(parse("key SPX.Z EOD").is_err(), "two words is not a key");
        assert!(parse("key SPX.Z/").is_err(), "an empty part");
        assert!(
            parse(&format!("key SPX{KEY_SEPARATOR}Z")).is_err(),
            "the storage separator is not typeable into a part"
        );
    }

    #[test]
    fn bump_reads_a_delta_and_an_optional_axis() {
        assert_eq!(
            parse("bump 0.25"),
            Ok(Command::Bump {
                delta: 0.25,
                axis: BumpAxis::Row
            }),
            "row is the default"
        );
        assert_eq!(
            parse("bump -1 col"),
            Ok(Command::Bump {
                delta: -1.0,
                axis: BumpAxis::Col
            })
        );
        assert!(parse("bump").is_err());
        assert!(parse("bump wide").is_err());
        assert!(parse("bump inf").is_err(), "not a finite number");
        assert!(parse("bump 1 diagonal").is_err());
    }

    #[test]
    fn the_other_verbs_parse_and_an_unknown_one_is_named() {
        assert_eq!(parse("revert"), Ok(Command::Revert));
        assert_eq!(parse("rebase"), Ok(Command::Rebase));
        assert_eq!(parse("discard"), Ok(Command::Discard));
        assert_eq!(parse("upload"), Ok(Command::Upload));
        assert_eq!(parse("rebse"), Err("unknown command 'rebse'".to_string()));
        assert_eq!(parse("   "), Err("empty command".to_string()));
    }

    #[test]
    fn completions_offer_the_verbs_then_the_catalog_keys() {
        let keys = vec!["NDX.Z".to_string(), "SPX.Z".to_string()];
        assert_eq!(
            completions("", 0, &keys, false),
            vec!["key", "revert", "bump", "upload"],
            "rebase and discard are offered only while behind"
        );
        assert_eq!(
            completions("", 0, &keys, true),
            vec!["key", "revert", "bump", "rebase", "discard", "upload"]
        );
        assert_eq!(completions("key ", 4, &keys, false), keys);
        assert_eq!(
            completions("key SP", 6, &keys, false),
            keys,
            "the whole vocabulary, unfiltered — the shell ranks it"
        );
        assert_eq!(completions("bump 1 ", 7, &keys, false), vec!["row", "col"]);
        assert_eq!(
            completions("bump ", 5, &keys, false),
            Vec::<String>::new(),
            "nothing completes a number"
        );
        assert_eq!(
            completions("revert ", 7, &keys, false),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_cursor_off_a_char_boundary_does_not_panic() {
        let keys = vec!["SPX.Z".to_string()];
        // `é` is two bytes: a cursor at 2 lands mid-char.
        assert!(!completions("kéy", 2, &keys, false).is_empty());
    }
}
