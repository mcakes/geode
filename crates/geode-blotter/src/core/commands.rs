//! Tile-local `:` commands and completion candidates. Parsing produces a
//! `Command` for the tile to apply; the shell ranks the candidates for the
//! word at the cursor. Frame-wide commands return actionable refusals.

use geode_core::sort::{SortArg, SortOrder};

/// A tile-local `:asof` override, or a return to the frame's as-of state.
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
    /// A command reserved for a frame-wide or configuration action. The tile
    /// shows its message inline, naming the supported route. Refused commands
    /// are excluded from completion candidates.
    Refused(&'static str),
}

/// Top-level command completion vocabulary, excluding refused commands.
/// The tile's `every_colon_command_leaves_the_frame_alone` test checks each
/// entry for frame-state isolation.
pub const COMMANDS: [&str; 8] = [
    "asof", "autosize", "filter", "group", "sort", "unpin", "unscoped", "view",
];

/// Refusal messages direct frame-wide and configuration commands to their
/// scope-bar, palette, or dialog routes.
pub const REFUSED_SCOPE: &str = ":scope is frame-wide — the scope bar (mod+/), Set scope expression…, or the palette's Scope: entries";
pub const REFUSED_ASOF_UNDO: &str =
    "frame as-of undo is in the palette (Swap to the previous as of)";
pub const REFUSED_LIVE: &str = ":asof live pins this tile; Return to live (palette) sets the frame";
pub const REFUSED_GROUP_SAVE: &str =
    "saving a slot is in the Groupings dialog (palette: Edit groupings…)";

pub const GROUP_NONE_REFUSED: &str = "the blotter always groups: :group takes columns or `slot N`";

fn slot(arg: Option<&str>, what: &str) -> Result<u8, String> {
    arg.and_then(|a| a.parse::<u8>().ok())
        .filter(|n| (1..=9).contains(n))
        .ok_or_else(|| format!("{what} needs a slot number 1–9"))
}

/// Parse a command line without the leading colon. Syntax errors carry an
/// actionable message. Column names, expressions, and timestamps are validated
/// when the tile applies the command.
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
                    // An empty grouping would be one grand-total row, not the
                    // trader's "ungrouped"; `none` stays reserved (the
                    // pricer's `:group none`) so it is never read as a column.
                    if columns.iter().any(|c| c == "none") {
                        return Err(GROUP_NONE_REFUSED.into());
                    }
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
        "sort" => Ok(match geode_core::sort::parse_args(rest)? {
            SortArg::Column { column, order } => Command::Sort { column, order },
            SortArg::Clear => Command::SortClear,
        }),
        other => Err(format!("unknown command '{other}'")),
    }
}

#[derive(Debug, Clone, Default)]
pub struct Vocabulary {
    /// Named columns available to `sort`: displayed measure and dimension
    /// columns, excluding the tree column. Before a plan is available, the tile
    /// uses the view's declared columns.
    pub columns: Vec<String>,
    /// Groupable columns from all grains in the dataset, plus derived
    /// dimensions, including those absent from the view. `group` completes
    /// from this list; `filter` also includes `columns` so expressions can
    /// refer to measures.
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
        ["sort", after @ ..] => geode_core::sort::completions(after, &vocab.columns),
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

// Use the shared timestamp parser so tile and frame commands interpret
// the same timestamp forms.
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

    /// The blotter always groups: `:group none` in any form is refused
    /// (an empty grouping is one grand-total row), and `none` is never
    /// read as a column. Completion does not offer it.
    #[test]
    fn group_none_is_refused_and_never_a_column() {
        for line in [
            "group none",
            "group  none ",
            "group none lhu",
            "group lhu none",
            "group lhu,none",
        ] {
            assert_eq!(parse(line), Err(GROUP_NONE_REFUSED.to_string()), "{line}");
        }
        assert_eq!(
            parse("group").unwrap_err(),
            "group needs columns or `slot N`"
        );
        assert_eq!(
            completions("group ", 6, &vocab()),
            vec!["book", "lhu", "slot"]
        );
    }

    /// Grouping and filtering offer dimensions absent from the displayed
    /// columns. Sorting offers only the column vocabulary. `group` also
    /// offers its `slot` subcommand.
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

    /// Frame-wide commands return route-specific refusals and are never
    /// completion candidates.
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
