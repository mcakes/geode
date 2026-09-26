//! The entry bar's pure half (line-pricer spec §8.4, entry-bar spec §4):
//! where `o` (always below) and `p`/`shift+p` land, where the next line
//! lands after a successful `enter`, the bar's target label, and the
//! `up`/`down` history.

use crate::core::sheet::{Place, RowKind, RowSpec, Sheet};

/// Where a new row lands relative to the cursor row. `row` is a flat row
/// index; `None` (no cursor row) lands at the end of the sheet.
pub fn place_for(sheet: &Sheet, row: Option<usize>, below: bool) -> Place {
    let Some(row) = row else {
        return Place::Root { at: sheet.len() };
    };
    if let Some(p) = sheet.parent(row) {
        let leg = row - p - 1;
        return Place::Leg {
            package: p,
            leg: if below { leg + 1 } else { leg },
        };
    }
    if sheet.is_package(row) {
        return if below {
            Place::Leg {
                package: row,
                leg: 0,
            }
        } else {
            Place::Root { at: row }
        };
    }
    Place::Root {
        at: if below { row + 1 } else { row },
    }
}

/// Flat rows `spec` occupies once inserted: a line one, a package itself
/// plus its legs.
fn span(spec: &RowSpec) -> usize {
    match spec {
        RowSpec::Line(_) => 1,
        RowSpec::Package { legs, .. } => 1 + legs.len(),
    }
}

/// The place after `inserted` landed at `place`, so a book of lines
/// is typed without another `o` (spec §8.4). A package at a leg place is
/// refused by `apply`, so that pair answers `place` unchanged.
pub fn next_place(place: Place, inserted: &RowSpec) -> Place {
    match (place, inserted) {
        (Place::Root { at }, spec) => Place::Root {
            at: at + span(spec),
        },
        (Place::Leg { package, leg }, RowSpec::Line(_)) => Place::Leg {
            package,
            leg: leg + 1,
        },
        (p @ Place::Leg { .. }, RowSpec::Package { .. }) => p,
    }
}

/// A row as the bar's label names it: its shorthand, or its template
/// token when the shorthand is empty or spans several lines (a custom
/// package's legs, one per line).
fn describe(sheet: &Sheet, row: usize) -> String {
    let text = sheet.shorthand(row);
    if !text.is_empty() && !text.contains('\n') {
        return text;
    }
    match sheet.kind(row) {
        RowKind::Package { template } => template.token().to_string(),
        _ => text,
    }
}

/// Where the entry bar's `enter` lands, as its muted label (entry-bar
/// spec §4.2). Computed from the place alone, so the label and the insert
/// can never disagree.
pub fn target_label(sheet: &Sheet, place: Place) -> String {
    match place {
        Place::Root { at } if at == 0 || at >= sheet.len() => "at end".to_string(),
        Place::Root { at } => {
            let before = at - 1;
            let root = sheet.parent(before).unwrap_or(before);
            format!("after {}", describe(sheet, root))
        }
        Place::Leg { package, leg: 0 } => match sheet.kind(package) {
            RowKind::Package { template } => format!("into {}", template.token()),
            _ => format!("into {}", describe(sheet, package)),
        },
        Place::Leg { package, leg } => format!("after {}", describe(sheet, package + leg)),
    }
}

/// The entry field's history (spec §8.4: "the sheet's own lines, most
/// recent first"): every ROOT row's shorthand, newest id first, a repeat
/// kept only at its newest. A custom package spells on several lines and
/// cannot be one entry, so it is left out.
pub fn history(sheet: &Sheet) -> Vec<String> {
    let mut roots: Vec<usize> = sheet.roots().collect();
    roots.sort_by_key(|r| std::cmp::Reverse(sheet.id(*r)));
    let mut out: Vec<String> = Vec::new();
    for r in roots {
        let text = sheet.shorthand(r);
        if text.is_empty() || text.contains('\n') || out.contains(&text) {
            continue;
        }
        out.push(text);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sheet::tests::{callspread, line, push, spx};
    use crate::core::shorthand::parse;
    use geode_core::pricing::OptionKind;

    /// [A, P(L1, L2), B] — flat rows 0..5.
    fn sheet() -> Sheet {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![callspread(1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s
    }

    #[test]
    fn a_place_lands_after_the_cursor_row_or_before_it() {
        let s = sheet();
        assert_eq!(
            place_for(&s, None, true),
            Place::Root { at: 5 },
            "no cursor row: the end"
        );
        assert_eq!(place_for(&s, Some(0), true), Place::Root { at: 1 });
        assert_eq!(place_for(&s, Some(0), false), Place::Root { at: 0 });
        assert_eq!(
            place_for(&s, Some(1), true),
            Place::Leg { package: 1, leg: 0 },
            "a package row: its first leg"
        );
        assert_eq!(
            place_for(&s, Some(1), false),
            Place::Root { at: 1 },
            "before the package"
        );
        assert_eq!(
            place_for(&s, Some(2), true),
            Place::Leg { package: 1, leg: 1 }
        );
        assert_eq!(
            place_for(&s, Some(2), false),
            Place::Leg { package: 1, leg: 0 }
        );
        assert_eq!(
            place_for(&s, Some(3), true),
            Place::Leg { package: 1, leg: 2 }
        );
        assert_eq!(place_for(&s, Some(4), true), Place::Root { at: 5 });
    }

    #[test]
    fn the_next_placeholder_follows_what_was_just_inserted() {
        let one = parse("SPX Z26 5000 C").unwrap();
        let cs = parse("SPX Z26 4800/5200 CS").unwrap();
        assert_eq!(
            next_place(Place::Root { at: 1 }, &one),
            Place::Root { at: 2 }
        );
        assert_eq!(
            next_place(Place::Root { at: 1 }, &cs),
            Place::Root { at: 4 },
            "a package and its two legs"
        );
        assert_eq!(
            next_place(Place::Leg { package: 1, leg: 1 }, &one),
            Place::Leg { package: 1, leg: 2 }
        );
    }

    #[test]
    fn history_is_the_roots_shorthand_newest_first_without_repeats() {
        let mut s = sheet();
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]); // repeats A's text
        let h = history(&s);
        assert_eq!(
            h,
            vec![
                "SPX Z26 5000 C".to_string(),
                "SPX Z26 4000 P".to_string(),
                "SPX Z26 4800/5200 CS".to_string(),
            ],
            "newest id first; A's repeat collapses into the newest; legs never appear"
        );
    }

    #[test]
    fn with_no_cursor_row_a_line_lands_at_the_end() {
        let s = sheet();
        assert_eq!(place_for(&s, None, true), Place::Root { at: 5 });
        let empty = Sheet::new("t");
        assert_eq!(place_for(&empty, None, true), Place::Root { at: 0 });
    }

    /// [A, P(L1, L2), B] — flat rows 0..5.
    #[test]
    fn the_label_names_where_enter_lands() {
        let s = sheet();
        assert_eq!(target_label(&s, Place::Root { at: 5 }), "at end");
        assert_eq!(
            target_label(&s, Place::Root { at: 1 }),
            format!("after {}", s.shorthand(0))
        );
        assert_eq!(
            target_label(&s, Place::Root { at: 4 }),
            format!("after {}", s.shorthand(1)),
            "a root after a package names the package, not its last leg"
        );
        assert_eq!(
            target_label(&s, Place::Leg { package: 1, leg: 0 }),
            "into CS"
        );
        assert_eq!(
            target_label(&s, Place::Leg { package: 1, leg: 2 }),
            format!("after {}", s.shorthand(3))
        );
        assert_eq!(
            target_label(&Sheet::new("t"), Place::Root { at: 0 }),
            "at end"
        );
    }

    #[test]
    fn a_custom_package_is_named_by_its_token_in_the_label() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s.apply(crate::core::Edit::Group {
            first: 0,
            count: 2,
            template: crate::core::Template::Custom,
            id: None,
        })
        .unwrap();
        assert_eq!(target_label(&s, Place::Root { at: 3 }), "at end");
        s.apply(crate::core::Edit::Insert {
            place: Place::Root { at: 3 },
            rows: vec![parse("SPX Z26 3000 P").unwrap()],
        })
        .unwrap();
        assert_eq!(target_label(&s, Place::Root { at: 3 }), "after CUSTOM");
    }

    #[test]
    fn a_custom_package_spells_on_several_lines_and_is_left_out_of_history() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s.apply(crate::core::Edit::Group {
            first: 0,
            count: 2,
            template: crate::core::Template::Custom,
            id: None,
        })
        .unwrap();
        assert!(
            history(&s).is_empty(),
            "one entry field line cannot hold it"
        );
    }
}
