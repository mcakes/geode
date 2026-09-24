//! Yank and put (line-pricer spec §8.5's `y y`, `d d`, `p`/`shift+p`): a
//! row as the `RowSpec` it would be typed as — so a put takes fresh ids
//! and re-requests, never a copy of results — and where a put lands
//! (planning decision 12).

use crate::core::entry::place_for;
use crate::core::sheet::{LineSpec, Place, RowKind, RowSpec, Sheet};

fn line_spec(sheet: &Sheet, row: usize) -> LineSpec {
    LineSpec {
        instrument: sheet
            .instrument(row)
            .expect("a line has an instrument")
            .clone(),
        qty: sheet.qty(row),
        shift: sheet.shift(row),
    }
}

/// The row as a spec: a line (a leg included) as a line, a package with
/// its legs.
pub fn spec_of(sheet: &Sheet, row: usize) -> RowSpec {
    match sheet.kind(row) {
        RowKind::Package { template } => RowSpec::Package {
            template,
            legs: sheet.children(row).map(|l| line_spec(sheet, l)).collect(),
        },
        RowKind::Line | RowKind::Underlying => RowSpec::Line(line_spec(sheet, row)),
    }
}

/// Where a put lands. A package always goes to a root boundary — before
/// or after the cursor's whole root block — since depth is at most two;
/// a line follows `o`'s rule.
pub fn put_place(sheet: &Sheet, row: Option<usize>, below: bool, spec: &RowSpec) -> Place {
    match (spec, row) {
        (RowSpec::Line(_), _) | (_, None) => place_for(sheet, row, below),
        (RowSpec::Package { .. }, Some(row)) => {
            let root = sheet.parent(row).unwrap_or(row);
            Place::Root {
                at: if below {
                    sheet.children(root).end.max(root + 1)
                } else {
                    root
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sheet::tests::{callspread, line, push, spx};
    use crate::core::shorthand::parse;
    use geode_core::pricing::OptionKind;

    fn sheet() -> Sheet {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 3)]);
        push(&mut s, vec![callspread(-5)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s
    }

    #[test]
    fn a_yanked_row_is_its_spec_and_a_package_takes_its_legs() {
        let s = sheet();
        assert_eq!(spec_of(&s, 0), line(spx(5000.0, OptionKind::Call), 3));
        assert_eq!(spec_of(&s, 1), callspread(-5), "the package with its legs");
        let RowSpec::Line(leg) = spec_of(&s, 2) else {
            panic!("a leg yanks as a line")
        };
        assert_eq!(leg.qty, -5);
    }

    #[test]
    fn a_put_package_lands_at_a_root_boundary_and_a_line_follows_o() {
        let s = sheet();
        let cs = parse("SPX Z26 4800/5200 CS").unwrap();
        let one = parse("SPX Z26 5000 C").unwrap();
        assert_eq!(
            put_place(&s, Some(2), true, &cs),
            Place::Root { at: 4 },
            "after the leg's whole package"
        );
        assert_eq!(put_place(&s, Some(2), false, &cs), Place::Root { at: 1 });
        assert_eq!(put_place(&s, Some(1), true, &cs), Place::Root { at: 4 });
        assert_eq!(put_place(&s, None, true, &cs), Place::Root { at: 0 });
        assert_eq!(
            put_place(&s, Some(2), true, &one),
            Place::Leg { package: 1, leg: 1 }
        );
        assert_eq!(
            put_place(&s, Some(1), true, &one),
            Place::Leg { package: 1, leg: 0 }
        );
    }
}
