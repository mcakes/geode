//! The tile's `:` grammar: `sort <column> [asc|desc|abs [asc|desc]]`, a bare
//! `sort` or `sort clear` to restore the default order, and `view <name>`
//! to show a tab by name. A column is named by its id, or by its head when
//! the head is one word; a column `s` refuses (underlying, a sparkline)
//! refuses here too. Whether a view name exists is the tile's to say.

use geode_core::sort::{SortArg, SortOrder};

use super::views::{Column, sortable};

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Sort { column: String, order: SortOrder },
    SortClear,
    View(String),
}

/// Resolve a typed word to a sortable column id: exact id first, then a
/// one-word head, case-insensitively. A column with no number to order by
/// refuses as `s` does, so `:sort` never sets a sort that orders nothing.
fn resolve(word: &str, columns: &[Column]) -> Result<String, String> {
    let c = columns
        .iter()
        .find(|c| c.id == word)
        .or_else(|| {
            columns.iter().find(|c| {
                !c.head.contains(char::is_whitespace) && c.head.eq_ignore_ascii_case(word)
            })
        })
        .ok_or_else(|| format!("no column '{word}'"))?;
    if !sortable(&c.kind) {
        return Err(format!("{} is not sortable", c.head));
    }
    Ok(c.id.clone())
}

pub fn parse(line: &str, columns: &[Column]) -> Result<Command, String> {
    let line = line.trim();
    let (cmd, rest) = line
        .split_once(char::is_whitespace)
        .map_or((line, ""), |(c, r)| (c, r.trim()));
    match cmd {
        "sort" if rest.is_empty() => Ok(Command::SortClear),
        "sort" => match geode_core::sort::parse_args(rest)? {
            SortArg::Clear => Ok(Command::SortClear),
            SortArg::Column { column, order } => {
                resolve(&column, columns).map(|column| Command::Sort { column, order })
            }
        },
        "view" => match rest.split_whitespace().collect::<Vec<_>>().as_slice() {
            [] => Err("view needs a name".into()),
            [name] => Ok(Command::View((*name).to_string())),
            _ => Err("view takes one name".into()),
        },
        "" => Err("empty command".into()),
        other => Err(format!("unknown command '{other}'")),
    }
}

/// The candidates for the word at `cursor`, sorted and de-duplicated:
/// the verbs first, then a column id or order word after `sort`, or one
/// of `views` after `view`.
pub fn completions(line: &str, cursor: usize, columns: &[Column], views: &[String]) -> Vec<String> {
    let mut cursor = cursor.min(line.len());
    while !line.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let words: Vec<&str> = line[..cursor].split_whitespace().collect();
    let fresh = line[..cursor].ends_with(char::is_whitespace);
    // Words completed so far: all of them when the cursor starts a fresh
    // word, else all but the one under the cursor.
    let done = if fresh || words.is_empty() {
        &words[..]
    } else {
        &words[..words.len() - 1]
    };
    let mut out: Vec<String> = match done {
        [] => vec!["sort".into(), "view".into()],
        ["sort", after @ ..] => {
            let ids: Vec<String> = columns.iter().map(|c| c.id.clone()).collect();
            geode_core::sort::completions(after, &ids)
        }
        ["view"] => views.to_vec(),
        _ => Vec::new(),
    };
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::lens::LensState;
    use crate::core::views::{ColumnKind, builtin, columns};

    fn cols() -> Vec<Column> {
        columns(builtin().get("vol").unwrap(), &LensState::default(), true)
    }

    fn views() -> Vec<String> {
        vec!["vol".to_string(), "skew".into()]
    }

    #[test]
    fn colon_sort_names_a_column_by_head_or_id_and_bare_sort_clears() {
        let cols = cols();
        let level = cols
            .iter()
            .find(|c| matches!(c.kind, ColumnKind::Level { .. }))
            .expect("a level column");
        assert_eq!(
            parse(&format!("sort {} abs", level.id), &cols).unwrap(),
            Command::Sort {
                column: level.id.clone(),
                order: SortOrder::AbsDesc
            }
        );
        assert_eq!(parse("sort", &cols).unwrap(), Command::SortClear);
        assert_eq!(parse("sort clear", &cols).unwrap(), Command::SortClear);
        assert_eq!(parse("sort nope", &cols).unwrap_err(), "no column 'nope'");
    }

    #[test]
    fn colon_sort_refuses_a_column_s_refuses() {
        let cols = cols();
        for kind in ["underlying", "spark"] {
            let c = cols
                .iter()
                .find(|c| {
                    matches!(
                        (&c.kind, kind),
                        (ColumnKind::Underlying, "underlying")
                            | (ColumnKind::Spark { .. }, "spark")
                    )
                })
                .expect(kind);
            assert_eq!(
                parse(&format!("sort {}", c.id), &cols).unwrap_err(),
                format!("{} is not sortable", c.head),
                "{kind}"
            );
        }
    }

    #[test]
    fn a_one_word_head_resolves_case_insensitively_and_orders_parse() {
        let cols = cols();
        let c = cols
            .iter()
            .find(|c| !c.head.contains(char::is_whitespace) && sortable(&c.kind))
            .expect("a sortable one-word head");
        let typed = c.head.to_uppercase();
        for (words, order) in [
            ("", SortOrder::Asc),
            (" asc", SortOrder::Asc),
            (" desc", SortOrder::Desc),
            (" abs", SortOrder::AbsDesc),
            (" abs desc", SortOrder::AbsDesc),
            (" abs asc", SortOrder::AbsAsc),
        ] {
            assert_eq!(
                parse(&format!("sort {typed}{words}"), &cols).unwrap(),
                Command::Sort {
                    column: c.id.clone(),
                    order
                },
                "{words}"
            );
        }
        assert!(parse("sort x y z w", &cols).is_err());
        assert!(parse("filter x", &cols).is_err());
    }

    #[test]
    fn colon_view_names_a_view_and_completes_the_names() {
        let cols = cols();
        let views = views();
        assert_eq!(
            parse("view skew", &cols).unwrap(),
            Command::View("skew".into())
        );
        assert_eq!(parse("view", &cols).unwrap_err(), "view needs a name");
        assert_eq!(parse("view a b", &cols).unwrap_err(), "view takes one name");
        assert_eq!(completions("", 0, &cols, &views), ["sort", "view"]);
        assert_eq!(completions("view ", 5, &cols, &views), ["skew", "vol"]);
        assert_eq!(completions("view s", 6, &cols, &views), ["skew", "vol"]);
        assert!(completions("view skew ", 10, &cols, &views).is_empty());
    }

    #[test]
    fn completions_offer_ids_then_order_words() {
        let cols = cols();
        let views = views();
        let first = completions("sort ", 5, &cols, &views);
        assert!(first.contains(&"clear".to_string()));
        assert!(first.contains(&cols[0].id));
        assert_eq!(
            completions("sort atfm", 9, &cols, &views).len(),
            first.len()
        );
        let id = &cols[0].id;
        let line = format!("sort {id} ");
        assert_eq!(
            completions(&line, line.len(), &cols, &views),
            ["abs", "asc", "desc"]
        );
        assert_eq!(completions("", 0, &cols, &views), ["sort", "view"]);
        assert_eq!(completions("so", 2, &cols, &views), ["sort", "view"]);
    }

    #[test]
    fn a_first_word_other_than_sort_or_view_completes_to_nothing() {
        let cols = cols();
        let views = views();
        assert!(completions("filter x", 8, &cols, &views).is_empty());
        assert!(completions("filter ", 7, &cols, &views).is_empty());
    }
}
