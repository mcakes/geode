//! The tile's `:` grammar: `sort <column> [asc|desc|abs [asc|desc]]`, a bare
//! `sort` or `sort clear` to restore the default order, `view <name>` to
//! show a tab by name, `group <dims> | slot N | none` to pin a grouping
//! over the frame's and `unpin` to release it. A column is named by its
//! id, or by its head when the head is one word; a column `s` refuses
//! (underlying, a sparkline) refuses here too. Whether a view name, a
//! dimension or a slot exists is the tile's to say.

use geode_core::sort::{SortArg, SortOrder};

use super::views::{Column, sortable};

pub const GROUP_USAGE: &str = "group needs dimensions, slot N or none";
/// The blotter's refusal, so the two tiles say the same thing.
pub const REFUSED_GROUP_SAVE: &str =
    "saving a slot is in the Grouping dialog (palette: Pick a grouping\u{2026})";
const SLOT_USAGE: &str = "group slot needs a number 1\u{2013}9";

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Sort {
        column: String,
        order: SortOrder,
    },
    SortClear,
    View(String),
    /// `group <dims>`: pin an ad hoc grouping, dimensions in order.
    Group(Vec<String>),
    /// `group slot N`: pin a saved grouping slot, re-read when it changes.
    GroupSlot(u8),
    /// `group none`: pin the tile flat, whatever the frame groups by.
    GroupNone,
    /// `unpin`: release the pin and follow the frame again.
    Unpin,
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
        "group" => {
            let mut words = rest.split_whitespace();
            match words.next() {
                None => Err(GROUP_USAGE.into()),
                Some("save") => Err(REFUSED_GROUP_SAVE.into()),
                Some("slot") => match (words.next(), words.next()) {
                    (Some(n), None) => n
                        .parse::<u8>()
                        .ok()
                        .filter(|n| (1..=9).contains(n))
                        .map(Command::GroupSlot)
                        .ok_or_else(|| SLOT_USAGE.into()),
                    _ => Err(SLOT_USAGE.into()),
                },
                Some("none") => match words.next() {
                    None => Ok(Command::GroupNone),
                    Some(_) => Err("usage: group none".into()),
                },
                Some(_) => {
                    let dims: Vec<String> = rest
                        .split(|c: char| c == ',' || c.is_whitespace())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect();
                    // `none` is the flat pin, never a dimension.
                    if dims.iter().any(|d| d == "none") {
                        return Err("none stands alone: `group none`".into());
                    }
                    Ok(Command::Group(dims))
                }
            }
        }
        "unpin" if rest.is_empty() => Ok(Command::Unpin),
        "unpin" => Err("unpin takes nothing".into()),
        "" => Err("empty command".into()),
        other => Err(format!("unknown command '{other}'")),
    }
}

/// The candidates for the word at `cursor`, sorted and de-duplicated:
/// the verbs first, then a column id or order word after `sort`, or one
/// of `views` after `view`. After `group` they are `dims` (the applicable
/// dimensions, kept in the chain's order), then `none` and `slot`.
pub fn completions(
    line: &str,
    cursor: usize,
    columns: &[Column],
    views: &[String],
    dims: &[String],
) -> Vec<String> {
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
        [] => vec!["group".into(), "sort".into(), "unpin".into(), "view".into()],
        ["sort", after @ ..] => {
            let ids: Vec<String> = columns.iter().map(|c| c.id.clone()).collect();
            geode_core::sort::completions(after, &ids)
        }
        ["view"] => views.to_vec(),
        ["group"] => {
            let mut v = dims.to_vec();
            v.extend(["none".to_string(), "slot".to_string()]);
            v
        }
        ["group", "slot"] => (1..=9).map(|n| n.to_string()).collect(),
        ["group", "none", ..] => Vec::new(),
        ["group", ..] => dims.to_vec(),
        _ => Vec::new(),
    };
    // The group candidates keep the chain's order; the rest read sorted.
    if !matches!(done, ["group", ..]) {
        out.sort();
        out.dedup();
    }
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
        assert_eq!(
            completions("", 0, &cols, &views, &[]),
            ["group", "sort", "unpin", "view"]
        );
        assert_eq!(completions("view ", 5, &cols, &views, &[]), ["skew", "vol"]);
        assert_eq!(
            completions("view s", 6, &cols, &views, &[]),
            ["skew", "vol"]
        );
        assert!(completions("view skew ", 10, &cols, &views, &[]).is_empty());
    }

    #[test]
    fn completions_offer_ids_then_order_words() {
        let cols = cols();
        let views = views();
        let first = completions("sort ", 5, &cols, &views, &[]);
        assert!(first.contains(&"clear".to_string()));
        assert!(first.contains(&cols[0].id));
        assert_eq!(
            completions("sort atfm", 9, &cols, &views, &[]).len(),
            first.len()
        );
        let id = &cols[0].id;
        let line = format!("sort {id} ");
        assert_eq!(
            completions(&line, line.len(), &cols, &views, &[]),
            ["abs", "asc", "desc"]
        );
        assert_eq!(
            completions("", 0, &cols, &views, &[]),
            ["group", "sort", "unpin", "view"]
        );
        assert_eq!(
            completions("so", 2, &cols, &views, &[]),
            ["group", "sort", "unpin", "view"]
        );
    }

    #[test]
    fn a_first_word_other_than_sort_or_view_completes_to_nothing() {
        let cols = cols();
        let views = views();
        assert!(completions("filter x", 8, &cols, &views, &[]).is_empty());
        assert!(completions("filter ", 7, &cols, &views, &[]).is_empty());
    }

    fn dims() -> Vec<String> {
        vec!["underlying_ref".into(), "region".into(), "sector".into()]
    }

    #[test]
    fn colon_group_parses_dimensions_slot_none_and_refuses_the_rest() {
        let cols = cols();
        assert_eq!(
            parse("group region sector", &cols).unwrap(),
            Command::Group(vec!["region".into(), "sector".into()])
        );
        assert_eq!(
            parse("group region,sector", &cols).unwrap(),
            Command::Group(vec!["region".into(), "sector".into()])
        );
        assert_eq!(parse("group slot 3", &cols).unwrap(), Command::GroupSlot(3));
        assert_eq!(parse("group none", &cols).unwrap(), Command::GroupNone);
        assert_eq!(parse("unpin", &cols).unwrap(), Command::Unpin);
        assert_eq!(parse("group", &cols).unwrap_err(), GROUP_USAGE);
        assert_eq!(parse("group save", &cols).unwrap_err(), REFUSED_GROUP_SAVE);
        assert_eq!(
            parse("group slot", &cols).unwrap_err(),
            "group slot needs a number 1\u{2013}9"
        );
        assert_eq!(
            parse("group slot 0", &cols).unwrap_err(),
            "group slot needs a number 1\u{2013}9"
        );
        assert_eq!(
            parse("group slot 2 3", &cols).unwrap_err(),
            "group slot needs a number 1\u{2013}9"
        );
        assert_eq!(
            parse("group none region", &cols).unwrap_err(),
            "usage: group none"
        );
        assert_eq!(
            parse("group region none", &cols).unwrap_err(),
            "none stands alone: `group none`"
        );
        assert_eq!(
            parse("unpin now", &cols).unwrap_err(),
            "unpin takes nothing"
        );
    }

    #[test]
    fn colon_group_completes_the_applicable_dimensions_none_and_slot() {
        let cols = cols();
        let views = views();
        let d = dims();
        assert_eq!(
            completions("", 0, &cols, &views, &d),
            ["group", "sort", "unpin", "view"]
        );
        assert_eq!(
            completions("group ", 6, &cols, &views, &d),
            ["underlying_ref", "region", "sector", "none", "slot"]
        );
        assert_eq!(
            completions("group slot ", 11, &cols, &views, &d),
            ["1", "2", "3", "4", "5", "6", "7", "8", "9"]
        );
        assert_eq!(
            completions("group none ", 11, &cols, &views, &d),
            Vec::<String>::new()
        );
        assert_eq!(
            completions("group region ", 13, &cols, &views, &d),
            ["underlying_ref", "region", "sector"]
        );
    }
}
