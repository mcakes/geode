//! The `:` vocabulary (Phase 3 spec §4.3) as data: a line parses into a
//! `Command` the tile applies, and the vocabulary for the word under the
//! cursor is what the shell ranks (§3.4).

use crate::core::flatten::SortOrder;

/// A `:asof` argument (command-line locality spec §3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsOfArg {
    /// Pin the tile to this instant; parsed by `parse_as_of` at apply time.
    At(String),
    /// Pin the tile to live.
    Live,
    /// Follow the frame again.
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Group(Vec<String>),
    GroupSlot(u8),
    Unpin,
    Unscoped,
    FilterExpr(String),
    FilterText(String),
    FilterClear,
    AsOf(AsOfArg),
    View(String),
    Sort {
        column: String,
        order: SortOrder,
    },
    SortClear,
    /// `:autosize` fits every column to the header and the loaded rows;
    /// `:autosize reset` returns to the view's widths.
    Autosize {
        reset: bool,
    },
    /// A word this line no longer runs because it was frame-wide
    /// (command-line locality spec §5): the message names the door it
    /// moved to. The tile shows it inline like any parse error. Never a
    /// completion — a refusal is not a suggestion.
    Refused(&'static str),
}

/// The words `completions` offers at the start of a line — every word
/// `parse` accepts EXCEPT the refusals. The tile's sweep test
/// (`every_colon_command_leaves_the_frame_alone`) reads this list so a
/// word added here is swept the day it lands.
pub const COMMANDS: [&str; 8] = [
    "asof", "autosize", "filter", "group", "sort", "unpin", "unscoped", "view",
];

/// The refusal messages (spec §5). Frame-wide verbs left the `:` line on
/// 2026-09-20; each message names the door that replaced it.
pub const REFUSED_SCOPE: &str = ":scope is frame-wide — the scope bar (mod+/), Set scope expression…, or the palette's Scope: entries";
pub const REFUSED_ASOF_UNDO: &str =
    "frame as-of undo is in the palette (Swap to the previous as of)";
pub const REFUSED_LIVE: &str = ":asof live pins this tile; Return to live (palette) sets the frame";
pub const REFUSED_GROUP_SAVE: &str =
    "saving a slot is in the Groupings dialog (palette: Edit groupings…)";

fn slot(arg: Option<&str>, what: &str) -> Result<u8, String> {
    arg.and_then(|a| a.parse::<u8>().ok())
        .filter(|n| (1..=9).contains(n))
        .ok_or_else(|| format!("{what} needs a slot number 1–9"))
}

/// Parse a `:` line (without the leading colon) into a `Command`. `Err`
/// carries a message a user can act on — never a panic (spec §4.3).
pub fn parse(line: &str) -> Result<Command, String> {
    let line = line.trim();
    if line.is_empty() {
        return Err("empty command".into());
    }
    let (head, rest) = match line.split_once(char::is_whitespace) {
        Some((h, r)) => (h, r.trim()),
        None => (line, ""),
    };
    match head {
        "unpin" => Ok(Command::Unpin),
        "unscoped" => Ok(Command::Unscoped),
        "autosize" => match rest {
            "" => Ok(Command::Autosize { reset: false }),
            "reset" => Ok(Command::Autosize { reset: true }),
            _ => Err("autosize takes nothing, or `reset`".into()),
        },
        "live" => Ok(Command::Refused(REFUSED_LIVE)),
        "group" => {
            let mut words = rest.split_whitespace();
            match words.next() {
                None => Err("group needs columns or `slot N`".into()),
                Some("slot") => slot(words.next(), "group slot").map(Command::GroupSlot),
                Some("save") => Ok(Command::Refused(REFUSED_GROUP_SAVE)),
                Some(_) => {
                    let columns: Vec<String> = rest
                        .split(|c: char| c == ',' || c.is_whitespace())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect();
                    Ok(Command::Group(columns))
                }
            }
        }
        "scope" => Ok(Command::Refused(REFUSED_SCOPE)),
        "filter" => {
            let mut words = rest.splitn(2, char::is_whitespace);
            match (words.next(), words.next().map(str::trim)) {
                (Some("clear"), None) => Ok(Command::FilterClear),
                (Some("text"), second) => Ok(Command::FilterText(second.unwrap_or("").to_string())),
                (Some(""), _) => Err("filter needs an expression, `text …` or `clear`".into()),
                _ => Ok(Command::FilterExpr(rest.to_string())),
            }
        }
        "asof" => match rest {
            "" => Err(
                "asof needs a time (HH:MM, HH:MM:SS, YYYY-MM-DD[ HH:MM[:SS]] or RFC 3339), \
                 `live` or `clear`"
                    .into(),
            ),
            "undo" => Ok(Command::Refused(REFUSED_ASOF_UNDO)),
            "live" => Ok(Command::AsOf(AsOfArg::Live)),
            "clear" => Ok(Command::AsOf(AsOfArg::Clear)),
            t => Ok(Command::AsOf(AsOfArg::At(t.to_string()))),
        },
        "view" => {
            if rest.is_empty() {
                Err("view needs a name".into())
            } else {
                Ok(Command::View(rest.to_string()))
            }
        }
        "sort" => {
            let mut words = rest.split_whitespace();
            let column = match words.next() {
                None => return Err("sort needs a column, or `clear`".into()),
                Some("clear") => {
                    return if words.next().is_none() {
                        Ok(Command::SortClear)
                    } else {
                        Err("sort clear takes nothing after it".into())
                    };
                }
                Some(c) => c,
            };
            // A bare `abs` is `abs desc`: the biggest exposures first.
            let order = match (words.next(), words.next(), words.next()) {
                (None, _, _) => SortOrder::Asc,
                (Some("asc"), None, _) => SortOrder::Asc,
                (Some("desc"), None, _) => SortOrder::Desc,
                (Some("abs"), None, _) | (Some("abs"), Some("desc"), None) => SortOrder::AbsDesc,
                (Some("abs"), Some("asc"), None) => SortOrder::AbsAsc,
                _ => {
                    return Err(
                        "sort takes a column and optionally `asc`, `desc`, `abs`, `abs asc` or `abs desc`"
                            .into(),
                    );
                }
            };
            Ok(Command::Sort {
                column: column.into(),
                order,
            })
        }
        other => Err(format!("unknown command '{other}'")),
    }
}

#[derive(Debug, Clone, Default)]
pub struct Vocabulary {
    /// What `sort` can rank: the view's own column plan (the tree column
    /// plus its declared measures) — what is actually displayed, not the
    /// dataset's full dimension set.
    pub columns: Vec<String>,
    /// Every column the tile's dataset carries as a dimension at any
    /// grain it has, plus every derived dimension (Phase 4a §3.2, §6.8)
    /// — distinct from `columns` since a dimension the view does not
    /// display (e.g. `book`, `currency`) is still a legal `group` or
    /// `filter` target. `group` completes from this alone; `filter`
    /// completes from this union `columns` (an expression can also name
    /// a measure).
    pub dimensions: Vec<String>,
    pub views: Vec<String>,
}

/// The candidates for the word at `cursor`. Sorted, so the shell's
/// ranking of an empty word is stable.
pub fn completions(line: &str, cursor: usize, vocab: &Vocabulary) -> Vec<String> {
    let mut cursor = cursor.min(line.len());
    // The caller's cursor should always be on a char boundary, but this
    // pure core must not depend on that — clamp down to the nearest
    // boundary at or before it rather than panicking on the slice below
    // (mirrors `commandline::word_at`'s guard for the same case).
    while !line.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let before = &line[..cursor];
    // The words completed so far, and whether the cursor is at the start
    // of a fresh word.
    let words: Vec<&str> = before
        .split(|c: char| c.is_whitespace() || c == ',')
        .collect();
    let (done, _current) = words.split_at(words.len().saturating_sub(1));
    let done: Vec<&str> = done.iter().copied().filter(|w| !w.is_empty()).collect();
    let mut out: Vec<String> = match done.as_slice() {
        [] => COMMANDS.iter().map(|s| s.to_string()).collect(),
        ["sort"] => {
            let mut v = vocab.columns.clone();
            v.push("clear".into());
            v
        }
        ["sort", "clear"] => Vec::new(),
        ["sort", _] => vec!["abs".into(), "asc".into(), "desc".into()],
        ["sort", _, "abs"] => vec!["asc".into(), "desc".into()],
        ["group"] => {
            let mut v = vocab.dimensions.clone();
            v.push("slot".into());
            v
        }
        ["group", "slot"] => (1..=9).map(|n| n.to_string()).collect(),
        ["group", ..] => vocab.dimensions.clone(),
        ["filter"] => {
            let mut v = vocab.dimensions.clone();
            v.extend(vocab.columns.clone());
            v.extend(["clear", "text"].map(String::from));
            v
        }
        ["filter", "text", ..] => Vec::new(),
        ["filter", ..] => {
            let mut v = vocab.dimensions.clone();
            v.extend(vocab.columns.clone());
            v
        }
        ["asof"] => vec!["clear".into(), "live".into()],
        ["autosize"] => vec!["reset".into()],
        ["view"] => vocab.views.clone(),
        _ => Vec::new(),
    };
    out.sort();
    out.dedup();
    out
}

// `parse_as_of` lives in `geode_core::query` now (both the shell and the
// data layer need it, and this crate depended only on `chrono`, which
// `geode-core` already has).
pub use geode_core::query::parse_as_of;

#[cfg(test)]
mod tests {
    use super::*;

    fn vocab() -> Vocabulary {
        Vocabulary {
            columns: vec!["book".into(), "lhu".into(), "delta01".into()],
            dimensions: vec!["book".into(), "lhu".into()],
            views: vec!["tree".into(), "wide".into()],
        }
    }

    #[test]
    fn every_command_parses() {
        assert_eq!(
            parse("group lhu,book").unwrap(),
            Command::Group(vec!["lhu".into(), "book".into()])
        );
        assert_eq!(
            parse("group lhu, book").unwrap(),
            Command::Group(vec!["lhu".into(), "book".into()])
        );
        assert_eq!(parse("group slot 3").unwrap(), Command::GroupSlot(3));
        assert_eq!(parse("unpin").unwrap(), Command::Unpin);
        assert_eq!(parse("unscoped").unwrap(), Command::Unscoped);
        assert_eq!(
            parse("asof 14:05").unwrap(),
            Command::AsOf(AsOfArg::At("14:05".into()))
        );
        assert_eq!(parse("asof live").unwrap(), Command::AsOf(AsOfArg::Live));
        assert_eq!(parse("asof clear").unwrap(), Command::AsOf(AsOfArg::Clear));
        assert_eq!(parse("view wide").unwrap(), Command::View("wide".into()));
        let sort = |column: &str, order| Command::Sort {
            column: column.into(),
            order,
        };
        assert_eq!(
            parse("sort delta01").unwrap(),
            sort("delta01", SortOrder::Asc)
        );
        assert_eq!(
            parse("sort delta01 asc").unwrap(),
            sort("delta01", SortOrder::Asc)
        );
        assert_eq!(
            parse("sort delta01 desc").unwrap(),
            sort("delta01", SortOrder::Desc)
        );
        // A bare `abs` is the biggest exposures first: what a trader
        // reaches for when the sign is noise.
        assert_eq!(
            parse("sort delta01 abs").unwrap(),
            sort("delta01", SortOrder::AbsDesc)
        );
        assert_eq!(
            parse("sort delta01 abs desc").unwrap(),
            sort("delta01", SortOrder::AbsDesc)
        );
        assert_eq!(
            parse("sort delta01 abs asc").unwrap(),
            sort("delta01", SortOrder::AbsAsc)
        );
        assert_eq!(parse("sort clear").unwrap(), Command::SortClear);
        assert_eq!(
            parse("  sort   delta01  ").unwrap(),
            sort("delta01", SortOrder::Asc)
        );
    }

    #[test]
    fn autosize_parses_with_an_optional_reset_and_completes_it() {
        assert_eq!(
            parse("autosize").unwrap(),
            Command::Autosize { reset: false }
        );
        assert_eq!(
            parse("autosize reset").unwrap(),
            Command::Autosize { reset: true }
        );
        assert!(parse("autosize wide").unwrap_err().contains("reset"));
        assert_eq!(completions("autosize ", 9, &vocab()), vec!["reset"]);
        assert!(completions("", 0, &vocab()).contains(&"autosize".to_string()));
    }

    #[test]
    fn errors_name_the_problem() {
        assert!(parse("").unwrap_err().contains("empty"));
        assert!(
            parse("frobnicate")
                .unwrap_err()
                .contains("unknown command 'frobnicate'")
        );
        assert!(parse("group").unwrap_err().contains("group"));
        assert!(parse("group slot 12").unwrap_err().contains("1–9"));
        assert!(parse("sort").unwrap_err().contains("column"));
        assert!(parse("sort delta01 up").unwrap_err().contains("abs"));
        assert!(parse("sort clear extra").unwrap_err().contains("clear"));
        assert!(parse("sort delta01 abs up").unwrap_err().contains("abs"));
        assert!(parse("sort delta01 desc abs").unwrap_err().contains("abs"));
        assert!(parse("view").unwrap_err().contains("name"));
        assert!(parse("asof").unwrap_err().contains("time"));
    }

    #[test]
    fn filter_forms_parse() {
        assert_eq!(
            parse("filter npv > 0").unwrap(),
            Command::FilterExpr("npv > 0".into())
        );
        assert_eq!(
            parse("filter text spx").unwrap(),
            Command::FilterText("spx".into())
        );
        assert_eq!(parse("filter clear").unwrap(), Command::FilterClear);
        assert!(parse("filter").unwrap_err().contains("filter"));
        assert_eq!(
            parse("filter text").unwrap(),
            Command::FilterText(String::new())
        );
        assert_eq!(
            parse("filter text   ").unwrap(),
            Command::FilterText(String::new())
        );
    }

    #[test]
    fn completions_follow_the_argument_position() {
        let v = vocab();
        let names = |line: &str| completions(line, line.len(), &v);
        assert_eq!(
            names(""),
            vec![
                "asof", "autosize", "filter", "group", "sort", "unpin", "unscoped", "view"
            ]
        );
        assert_eq!(
            names("so"),
            vec![
                "asof", "autosize", "filter", "group", "sort", "unpin", "unscoped", "view"
            ],
            "the shell ranks; the vocabulary is whole"
        );
        assert_eq!(names("sort "), vec!["book", "clear", "delta01", "lhu"]);
        assert_eq!(names("sort delta01 "), vec!["abs", "asc", "desc"]);
        assert_eq!(names("sort delta01 abs "), vec!["asc", "desc"]);
        assert!(names("sort clear ").is_empty());
        assert_eq!(
            names("group "),
            vec!["book", "lhu", "slot"],
            "group completes dimensions (book, lhu), never the delta01 measure"
        );
        assert_eq!(names("group lhu,"), vec!["book", "lhu"]);
        assert_eq!(
            names("group slot "),
            (1..=9).map(|n| n.to_string()).collect::<Vec<_>>()
        );
        assert_eq!(names("view "), vec!["tree", "wide"]);
        assert_eq!(names("asof "), vec!["clear", "live"]);
        assert!(names("sort delta01 desc ").is_empty());
        assert_eq!(
            completions("sort delta01", 2, &v),
            vec![
                "asof", "autosize", "filter", "group", "sort", "unpin", "unscoped", "view"
            ],
            "the cursor's word, not the last"
        );
    }

    #[test]
    fn completions_clamp_a_cursor_inside_a_multibyte_char() {
        let v = Vocabulary {
            columns: vec!["délta".into()],
            dimensions: vec![],
            views: vec![],
        };
        let line = "sort dé";
        // `é` is two bytes; this cursor lands one byte past its start,
        // inside the character, not on a char boundary.
        let cursor = line.find('é').unwrap() + 1;
        assert_eq!(completions(line, cursor, &v), vec!["clear", "délta"]);
    }

    /// Regression: before this fix, `group`/`filter` all completed from
    /// `Vocabulary::columns` alone — the view's own column plan (what's
    /// *displayed*), so a dimension the view does not show (`book`,
    /// `currency`) never appeared after `:group ` or `:filter `. `sort`
    /// is unaffected: it still ranks only what's actually a column in
    /// the view.
    ///
    /// `group`'s expected vector includes `slot` — its own existing
    /// keyword completion, untouched by this fix (only the column
    /// source changed from `columns` to `dimensions`).
    #[test]
    fn group_and_drop_complete_dimensions_not_measures() {
        let vocab = Vocabulary {
            columns: vec!["npv".into(), "delta01".into()],
            dimensions: vec!["book".into(), "currency".into()],
            views: vec![],
        };
        assert_eq!(
            completions("group ", 6, &vocab),
            vec!["book", "currency", "slot"]
        );
        assert_eq!(
            completions("sort ", 5, &vocab),
            vec!["clear", "delta01", "npv"]
        );
        assert_eq!(
            completions("filter ", 7, &vocab),
            vec!["book", "clear", "currency", "delta01", "npv", "text"]
        );
    }

    /// Command-line locality (2026-09-20): the frame-wide words are
    /// refusals whose message names the door, and none is a completion.
    #[test]
    fn frame_wide_words_are_refusals_that_name_their_door() {
        assert_eq!(
            parse("scope lhu = 'L1'").unwrap(),
            Command::Refused(REFUSED_SCOPE)
        );
        assert_eq!(
            parse("scope clear").unwrap(),
            Command::Refused(REFUSED_SCOPE)
        );
        assert_eq!(
            parse("asof undo").unwrap(),
            Command::Refused(REFUSED_ASOF_UNDO)
        );
        assert_eq!(parse("live").unwrap(), Command::Refused(REFUSED_LIVE));
        assert_eq!(
            parse("group save 3").unwrap(),
            Command::Refused(REFUSED_GROUP_SAVE)
        );
        for refused in ["scope", "live"] {
            assert!(
                !COMMANDS.contains(&refused),
                "`{refused}` must not be offered"
            );
        }
        let first = completions("", 0, &vocab());
        assert_eq!(
            first,
            COMMANDS.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        );
        assert_eq!(completions("asof ", 5, &vocab()), vec!["clear", "live"]);
        assert_eq!(
            completions("group ", 6, &vocab()),
            vec!["book", "lhu", "slot"],
            "`save` is no longer offered after `group`"
        );
    }
}
