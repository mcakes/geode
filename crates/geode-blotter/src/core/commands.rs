//! The `:` vocabulary (Phase 3 spec §4.3) as data: a line parses into a
//! `Command` the tile applies, and the vocabulary for the word under the
//! cursor is what the shell ranks (§3.4).

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Group(Vec<String>),
    GroupSlot(u8),
    GroupSave(u8),
    Unpin,
    Unscoped,
    ScopeExpr(String),
    ScopeText(String),
    ScopeClear,
    ScopeUndo,
    AsOf(String),
    Live,
    View(String),
    Sort { column: String, descending: bool },
    SortClear,
}

const COMMANDS: [&str; 8] = [
    "asof", "group", "live", "scope", "sort", "unpin", "unscoped", "view",
];

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
        "live" => Ok(Command::Live),
        "group" => {
            let mut words = rest.split_whitespace();
            match words.next() {
                None => Err("group needs columns, `slot N` or `save N`".into()),
                Some("slot") => slot(words.next(), "group slot").map(Command::GroupSlot),
                Some("save") => slot(words.next(), "group save").map(Command::GroupSave),
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
        "scope" => match rest.split_once(char::is_whitespace) {
            None if rest == "clear" => Ok(Command::ScopeClear),
            None if rest == "undo" => Ok(Command::ScopeUndo),
            None if rest.is_empty() => {
                Err("scope needs an expression, `text …`, `clear` or `undo`".into())
            }
            Some(("text", words)) => Ok(Command::ScopeText(words.trim().to_string())),
            _ => Ok(Command::ScopeExpr(rest.to_string())),
        },
        "asof" => {
            if rest.is_empty() {
                Err("asof needs a time: HH:MM or RFC 3339".into())
            } else {
                Ok(Command::AsOf(rest.to_string()))
            }
        }
        "view" => {
            if rest.is_empty() {
                Err("view needs a name".into())
            } else {
                Ok(Command::View(rest.to_string()))
            }
        }
        "sort" => {
            let mut words = rest.split_whitespace();
            match (words.next(), words.next(), words.next()) {
                (None, _, _) => Err("sort needs a column, or `clear`".into()),
                (Some("clear"), None, _) => Ok(Command::SortClear),
                (Some(column), None, _) => Ok(Command::Sort {
                    column: column.into(),
                    descending: false,
                }),
                (Some(column), Some("desc"), None) => Ok(Command::Sort {
                    column: column.into(),
                    descending: true,
                }),
                (Some(column), Some("asc"), None) => Ok(Command::Sort {
                    column: column.into(),
                    descending: false,
                }),
                _ => Err("sort takes a column and optionally `desc`".into()),
            }
        }
        other => Err(format!("unknown command '{other}'")),
    }
}

#[derive(Debug, Clone, Default)]
pub struct Vocabulary {
    pub columns: Vec<String>,
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
        ["sort", _] => vec!["desc".into()],
        ["group"] => {
            let mut v = vocab.columns.clone();
            v.push("save".into());
            v.push("slot".into());
            v
        }
        ["group", "slot"] | ["group", "save"] => (1..=9).map(|n| n.to_string()).collect(),
        ["group", ..] => vocab.columns.clone(),
        ["scope"] => {
            let mut v = vocab.columns.clone();
            v.extend(["clear", "text", "undo"].map(String::from));
            v
        }
        ["scope", "text", ..] => Vec::new(),
        ["scope", ..] => vocab.columns.clone(),
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
        assert_eq!(parse("group save 9").unwrap(), Command::GroupSave(9));
        assert_eq!(parse("unpin").unwrap(), Command::Unpin);
        assert_eq!(parse("unscoped").unwrap(), Command::Unscoped);
        assert_eq!(
            parse("scope book = 'BK001' and delta01 > 5").unwrap(),
            Command::ScopeExpr("book = 'BK001' and delta01 > 5".into())
        );
        assert_eq!(
            parse("scope text spx rut").unwrap(),
            Command::ScopeText("spx rut".into())
        );
        assert_eq!(parse("scope clear").unwrap(), Command::ScopeClear);
        assert_eq!(parse("scope undo").unwrap(), Command::ScopeUndo);
        assert_eq!(parse("asof 14:05").unwrap(), Command::AsOf("14:05".into()));
        assert_eq!(parse("live").unwrap(), Command::Live);
        assert_eq!(parse("view wide").unwrap(), Command::View("wide".into()));
        assert_eq!(
            parse("sort delta01").unwrap(),
            Command::Sort {
                column: "delta01".into(),
                descending: false
            }
        );
        assert_eq!(
            parse("sort delta01 desc").unwrap(),
            Command::Sort {
                column: "delta01".into(),
                descending: true
            }
        );
        assert_eq!(parse("sort clear").unwrap(), Command::SortClear);
        assert_eq!(
            parse("  sort   delta01  ").unwrap(),
            Command::Sort {
                column: "delta01".into(),
                descending: false
            }
        );
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
        assert!(parse("group save x").unwrap_err().contains("1–9"));
        assert!(parse("sort").unwrap_err().contains("column"));
        assert!(parse("sort delta01 up").unwrap_err().contains("desc"));
        assert!(parse("view").unwrap_err().contains("name"));
        assert!(parse("scope").unwrap_err().contains("scope"));
        assert!(parse("asof").unwrap_err().contains("time"));
    }

    #[test]
    fn completions_follow_the_argument_position() {
        let v = vocab();
        let names = |line: &str| completions(line, line.len(), &v);
        assert_eq!(
            names(""),
            vec![
                "asof", "group", "live", "scope", "sort", "unpin", "unscoped", "view"
            ]
        );
        assert_eq!(
            names("so"),
            vec![
                "asof", "group", "live", "scope", "sort", "unpin", "unscoped", "view"
            ],
            "the shell ranks; the vocabulary is whole"
        );
        assert_eq!(names("sort "), vec!["book", "clear", "delta01", "lhu"]);
        assert_eq!(names("sort delta01 "), vec!["desc"]);
        assert_eq!(
            names("group "),
            vec!["book", "delta01", "lhu", "save", "slot"]
        );
        assert_eq!(names("group lhu,"), vec!["book", "delta01", "lhu"]);
        assert_eq!(
            names("group slot "),
            (1..=9).map(|n| n.to_string()).collect::<Vec<_>>()
        );
        assert_eq!(
            names("group save "),
            (1..=9).map(|n| n.to_string()).collect::<Vec<_>>()
        );
        assert_eq!(
            names("scope "),
            vec!["book", "clear", "delta01", "lhu", "text", "undo"]
        );
        assert_eq!(
            names("scope book = 'x' and "),
            vec!["book", "delta01", "lhu"]
        );
        assert_eq!(names("view "), vec!["tree", "wide"]);
        assert!(names("asof ").is_empty());
        assert!(names("sort delta01 desc ").is_empty());
        assert_eq!(
            completions("sort delta01", 2, &v),
            vec![
                "asof", "group", "live", "scope", "sort", "unpin", "unscoped", "view"
            ],
            "the cursor's word, not the last"
        );
    }

    #[test]
    fn completions_clamp_a_cursor_inside_a_multibyte_char() {
        let v = Vocabulary {
            columns: vec!["délta".into()],
            views: vec![],
        };
        let line = "sort dé";
        // `é` is two bytes; this cursor lands one byte past its start,
        // inside the character, not on a char boundary.
        let cursor = line.find('é').unwrap() + 1;
        assert_eq!(completions(line, cursor, &v), vec!["clear", "délta"]);
    }
}
