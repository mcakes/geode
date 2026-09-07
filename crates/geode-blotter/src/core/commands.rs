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
    ScopeRedo,
    ScopeDrop(String),
    ScopeSave(String),
    ScopeLoad(String),
    FilterExpr(String),
    FilterText(String),
    FilterClear,
    AsOf(String),
    AsOfUndo,
    Live,
    View(String),
    Sort { column: String, descending: bool },
    SortClear,
}

const COMMANDS: [&str; 9] = [
    "asof", "filter", "group", "live", "scope", "sort", "unpin", "unscoped", "view",
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
        "scope" => {
            // `rest.splitn(2, ..).next()` returns `Some("")`, never `None`,
            // when `rest` is empty (the same one-piece-for-no-match
            // behaviour `str::split` has on `""`) — so the tuple match
            // below can never itself reach its own `(None, _)` arm for a
            // bare `scope`. Guarding here keeps that arm's error message
            // reachable instead of silently falling through to
            // `(Some(_), _) => Ok(Command::ScopeExpr(rest.to_string()))`
            // with an empty expression.
            if rest.is_empty() {
                return Err(
                    "scope needs an expression, `text …`, `clear`, `undo`, `redo`, \
                     `drop <dim>`, `save <name>` or `load <name>`"
                        .into(),
                );
            }
            let mut words = rest.splitn(2, char::is_whitespace);
            match (words.next(), words.next().map(str::trim)) {
                (Some("clear"), None) => Ok(Command::ScopeClear),
                (Some("undo"), None) => Ok(Command::ScopeUndo),
                (Some("redo"), None) => Ok(Command::ScopeRedo),
                (Some("drop"), Some(d)) if !d.is_empty() => Ok(Command::ScopeDrop(d.to_string())),
                (Some("drop"), _) => Err("scope drop needs a dimension".into()),
                (Some("save"), Some(n)) if !n.is_empty() => Ok(Command::ScopeSave(n.to_string())),
                (Some("save"), _) => Err("scope save needs a name".into()),
                (Some("load"), Some(n)) if !n.is_empty() => Ok(Command::ScopeLoad(n.to_string())),
                (Some("load"), _) => Err("scope load needs a name".into()),
                (Some("text"), Some(w)) => Ok(Command::ScopeText(w.to_string())),
                (Some("text"), None) => Ok(Command::ScopeText(String::new())),
                (Some(_), _) => Ok(Command::ScopeExpr(rest.to_string())),
                (None, _) => Err(
                    "scope needs an expression, `text …`, `clear`, `undo`, `redo`, \
                     `drop <dim>`, `save <name>` or `load <name>`"
                        .into(),
                ),
            }
        }
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
            "" => Err("asof needs a time: HH:MM, HH:MM:SS or RFC 3339, or `undo`".into()),
            "undo" => Ok(Command::AsOfUndo),
            t => Ok(Command::AsOf(t.to_string())),
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
    /// What `sort` can rank: the view's own column plan (the tree column
    /// plus its declared measures) — what is actually displayed, not the
    /// dataset's full dimension set.
    pub columns: Vec<String>,
    /// Every column the tile's dataset carries as a dimension at any
    /// grain it has, plus every derived dimension (Phase 4a §3.2, §6.8)
    /// — distinct from `columns` since a dimension the view does not
    /// display (e.g. `book`, `currency`) is still a legal `group`,
    /// `scope drop`, `scope` or `filter` target. `group`/`scope drop`
    /// complete from this alone; `scope`/`filter` complete from this
    /// union `columns` (an expression can also name a measure).
    pub dimensions: Vec<String>,
    pub views: Vec<String>,
    /// Saved-scope names (Phase 4a §3.9), for `scope load`'s completion.
    pub scopes: Vec<String>,
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
            let mut v = vocab.dimensions.clone();
            v.push("save".into());
            v.push("slot".into());
            v
        }
        ["group", "slot"] | ["group", "save"] => (1..=9).map(|n| n.to_string()).collect(),
        ["group", ..] => vocab.dimensions.clone(),
        ["scope"] => {
            let mut v = vocab.dimensions.clone();
            v.extend(vocab.columns.clone());
            v.extend(["clear", "drop", "load", "redo", "save", "text", "undo"].map(String::from));
            v
        }
        ["scope", "drop"] => vocab.dimensions.clone(),
        ["scope", "load"] => vocab.scopes.clone(),
        ["scope", "text", ..] => Vec::new(),
        ["scope", ..] => {
            let mut v = vocab.dimensions.clone();
            v.extend(vocab.columns.clone());
            v
        }
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
        ["asof"] => vec!["undo".into()],
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
            scopes: vec![],
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
    fn new_scope_and_asof_forms_parse() {
        assert_eq!(
            parse("scope drop book").unwrap(),
            Command::ScopeDrop("book".into())
        );
        assert_eq!(parse("scope redo").unwrap(), Command::ScopeRedo);
        assert_eq!(
            parse("scope save mine").unwrap(),
            Command::ScopeSave("mine".into())
        );
        assert_eq!(
            parse("scope load mine").unwrap(),
            Command::ScopeLoad("mine".into())
        );
        assert_eq!(parse("asof undo").unwrap(), Command::AsOfUndo);
        assert!(parse("scope drop").unwrap_err().contains("dimension"));
        assert!(parse("scope save").unwrap_err().contains("name"));
    }

    #[test]
    fn completions_offer_dimensions_after_drop_and_scope_names_after_load() {
        let vocab = Vocabulary {
            columns: vec!["book".into()],
            dimensions: vec!["book".into()],
            views: vec![],
            scopes: vec!["mine".into()],
        };
        assert_eq!(completions("scope drop ", 11, &vocab), vec!["book"]);
        assert_eq!(completions("scope load ", 11, &vocab), vec!["mine"]);
        assert!(completions("", 0, &vocab).contains(&"filter".to_string()));
    }

    #[test]
    fn completions_follow_the_argument_position() {
        let v = vocab();
        let names = |line: &str| completions(line, line.len(), &v);
        assert_eq!(
            names(""),
            vec![
                "asof", "filter", "group", "live", "scope", "sort", "unpin", "unscoped", "view"
            ]
        );
        assert_eq!(
            names("so"),
            vec![
                "asof", "filter", "group", "live", "scope", "sort", "unpin", "unscoped", "view"
            ],
            "the shell ranks; the vocabulary is whole"
        );
        assert_eq!(names("sort "), vec!["book", "clear", "delta01", "lhu"]);
        assert_eq!(names("sort delta01 "), vec!["desc"]);
        assert_eq!(
            names("group "),
            vec!["book", "lhu", "save", "slot"],
            "group completes dimensions (book, lhu), never the delta01 measure"
        );
        assert_eq!(names("group lhu,"), vec!["book", "lhu"]);
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
            vec![
                "book", "clear", "delta01", "drop", "lhu", "load", "redo", "save", "text", "undo"
            ]
        );
        assert_eq!(
            names("scope book = 'x' and "),
            vec!["book", "delta01", "lhu"]
        );
        assert_eq!(names("view "), vec!["tree", "wide"]);
        assert_eq!(names("asof "), vec!["undo"]);
        assert!(names("sort delta01 desc ").is_empty());
        assert_eq!(
            completions("sort delta01", 2, &v),
            vec![
                "asof", "filter", "group", "live", "scope", "sort", "unpin", "unscoped", "view"
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
            scopes: vec![],
        };
        let line = "sort dé";
        // `é` is two bytes; this cursor lands one byte past its start,
        // inside the character, not on a char boundary.
        let cursor = line.find('é').unwrap() + 1;
        assert_eq!(completions(line, cursor, &v), vec!["clear", "délta"]);
    }

    /// Regression: before this fix, `group`/`scope drop`/`scope`/`filter`
    /// all completed from `Vocabulary::columns` alone — the view's own
    /// column plan (what's *displayed*), so a dimension the view does
    /// not show (`book`, `currency`) never appeared after `:group `,
    /// `:scope drop ` or `:filter `. `sort` is unaffected: it still
    /// ranks only what's actually a column in the view.
    ///
    /// `group`'s expected vector includes `save`/`slot` — its own
    /// existing keyword completions, untouched by this fix (only the
    /// column source changed from `columns` to `dimensions`).
    #[test]
    fn group_and_drop_complete_dimensions_not_measures() {
        let vocab = Vocabulary {
            columns: vec!["npv".into(), "delta01".into()],
            dimensions: vec!["book".into(), "currency".into()],
            views: vec![],
            scopes: vec![],
        };
        assert_eq!(
            completions("group ", 6, &vocab),
            vec!["book", "currency", "save", "slot"]
        );
        assert_eq!(
            completions("scope drop ", 11, &vocab),
            vec!["book", "currency"]
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
}
