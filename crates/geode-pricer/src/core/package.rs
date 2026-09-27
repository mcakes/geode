//! A package row's aggregated cells (package-row spec): its legs' distinct
//! values in leg order, joined with `/`, and the package quantity while
//! the legs fit the package's template. Values compare as values, not as
//! text, and each part is spelled as a line's cell spells it. Pure: the
//! grid model prepares these cells outside render, as it does a line's.

use crate::core::columns::{CellState, CellText, ColumnKind, signed};
use crate::core::sheet::{OwnShifts, RowKind, Sheet};
use crate::core::shorthand::{render_barrier_kind, render_expiry, render_package, render_strike};
use geode_core::pricing::{Instrument, OptionKind, Strike};
use geode_core::view::ColumnFormat;

/// Whether a package row aggregates its legs for `kind`: qty and the eight
/// text columns. Results and status stay the package's own.
pub fn aggregates(kind: ColumnKind) -> bool {
    matches!(
        kind,
        ColumnKind::Qty
            | ColumnKind::Underlying
            | ColumnKind::Expiry
            | ColumnKind::Strike
            | ColumnKind::Type
            | ColumnKind::Barrier
            | ColumnKind::BarrierType
            | ColumnKind::SpotShift
            | ColumnKind::VolShift
    )
}

/// The legs of a package grouped by one column's value, groups in the
/// order their first leg appears. `display` is the cell's spelling of a
/// group (empty for an unset shift, which a mixed cell paints as
/// [`UNSET`]); `edit` is the spelling the line editor would open on, taken
/// from the group's first leg, so a list typed back parses as the line cell
/// parses it (an empty part clears that group's shift).
pub(crate) struct Group {
    pub display: String,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the package cell editor opens on it")
    )]
    pub edit: String,
    pub legs: Vec<usize>,
}

/// An unset shift's part in a cell whose other legs set one: `+2.0/—`.
pub(crate) const UNSET: &str = "—";

/// Groups legs by their value: values compare as values, not as text.
fn group_by<T: PartialEq + Clone>(
    legs: impl Iterator<Item = (usize, T)>,
    spell: impl Fn(&T) -> (String, String),
) -> Vec<Group> {
    group_by_key(legs, T::clone, spell)
}

/// Groups legs by `key`; the group's spelling is its first leg's.
fn group_by_key<T, K: PartialEq>(
    legs: impl Iterator<Item = (usize, T)>,
    key: impl Fn(&T) -> K,
    spell: impl Fn(&T) -> (String, String),
) -> Vec<Group> {
    let mut keys: Vec<K> = Vec::new();
    let mut out: Vec<Group> = Vec::new();
    for (leg, v) in legs {
        let k = key(&v);
        match keys.iter().position(|seen| *seen == k) {
            Some(i) => out[i].legs.push(leg),
            None => {
                let (display, edit) = spell(&v);
                out.push(Group {
                    display,
                    edit,
                    legs: vec![leg],
                });
                keys.push(k);
            }
        }
    }
    out
}

/// `{}` of an `f64`, as the line editor spells a shift or a barrier level.
fn plain(v: f64) -> String {
    format!("{v}")
}

/// The shift field a shift column reads; `None` for any other column.
fn shift_pick(kind: ColumnKind) -> Option<fn(OwnShifts) -> Option<f64>> {
    match kind {
        ColumnKind::SpotShift => Some(|s| s.spot_pct),
        ColumnKind::VolShift => Some(|s| s.vol_pts),
        _ => None,
    }
}

/// The package quantity and the template's weights, when the legs fit
/// the package's template (the shorthand's own test).
pub(crate) fn package_qty(sheet: &Sheet, row: usize) -> Option<(i64, Vec<i64>)> {
    let RowKind::Package { template } = sheet.kind(row) else {
        return None;
    };
    let def = sheet.templates().resolve(template.token())?;
    let legs: Vec<(i64, &Instrument)> = sheet
        .children(row)
        .filter_map(|l| sheet.instrument(l).map(|i| (sheet.qty(l), i)))
        .collect();
    render_package(def, &legs)?;
    let q = legs.first()?.0.checked_div(def.legs.first()?.weight)?;
    Some((q, def.legs.iter().map(|l| l.weight).collect()))
}

/// The groups of `row`'s legs for `kind`; empty for a column that does not
/// aggregate or a package with no leg the column reads.
pub(crate) fn groups(
    sheet: &Sheet,
    row: usize,
    kind: ColumnKind,
    format: &ColumnFormat,
) -> Vec<Group> {
    let legs = || {
        sheet
            .children(row)
            .filter_map(|l| sheet.instrument(l).map(|i| (l, i)))
    };
    let barrier_legs = || {
        legs().filter_map(|(l, i)| match i {
            Instrument::Barrier(b) => Some((l, b)),
            Instrument::Vanilla(_) => None,
        })
    };
    let same = |s: String| (s.clone(), s);
    match kind {
        ColumnKind::Qty => group_by(sheet.children(row).map(|l| (l, sheet.qty(l))), |q| {
            same(q.to_string())
        }),
        ColumnKind::Underlying => group_by(legs().map(|(l, i)| (l, i.underlying())), |u| {
            same(u.to_string())
        }),
        ColumnKind::Expiry => group_by(legs().map(|(l, i)| (l, i.expiry())), |e| {
            same(render_expiry(e))
        }),
        ColumnKind::Strike => group_by(legs().map(|(l, i)| (l, i.strike())), |k| {
            same(render_strike(*k))
        }),
        ColumnKind::Type => group_by(legs().map(|(l, i)| (l, i.kind())), |k| {
            same(
                match k {
                    OptionKind::Call => "C",
                    OptionKind::Put => "P",
                }
                .into(),
            )
        }),
        ColumnKind::Barrier => group_by(barrier_legs().map(|(l, b)| (l, b.level)), |v| {
            (render_strike(Strike::Absolute(*v)), plain(*v))
        }),
        ColumnKind::BarrierType => group_by(barrier_legs().map(|(l, b)| (l, b.barrier)), |k| {
            same(render_barrier_kind(*k).to_string())
        }),
        ColumnKind::SpotShift | ColumnKind::VolShift => {
            let pick = shift_pick(kind).expect("a shift column");
            let sheet_value = pick(sheet.sheet_shift());
            let spell = |v: &Option<f64>| match v {
                Some(v) => (signed(*v, format), plain(*v)),
                None => (String::new(), String::new()),
            };
            // Shifts group by their spelled text (spec §2): an own 2.04 and
            // an inherited 2.0 that both paint `+2.0` are one part.
            group_by_key(
                sheet
                    .children(row)
                    .map(|l| (l, pick(sheet.shift(l)).or(sheet_value))),
                |v| spell(v).0,
                spell,
            )
        }
        _ => Vec::new(),
    }
}

/// A package row's cell for `kind` (see the module doc).
pub fn aggregate(sheet: &Sheet, row: usize, kind: ColumnKind, format: &ColumnFormat) -> CellText {
    if kind == ColumnKind::Qty
        && let Some((q, _)) = package_qty(sheet, row)
    {
        return CellText {
            text: q.to_string(),
            state: CellState::Own,
        };
    }
    let gs = groups(sheet, row, kind, format);
    // Only when every group is empty (no leg the column reads, or a shift
    // nobody sets) is the cell blank; an unset group among set ones paints
    // `UNSET`, so the parts still line up with the legs' values.
    if gs.iter().all(|g| g.display.is_empty()) {
        return CellText {
            text: String::new(),
            state: CellState::Blank,
        };
    }
    let text = gs
        .iter()
        .map(|g| {
            if g.display.is_empty() {
                UNSET
            } else {
                g.display.as_str()
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    let state = match shift_pick(kind) {
        Some(pick) if sheet.children(row).all(|l| pick(sheet.shift(l)).is_none()) => {
            CellState::Inherited
        }
        _ => CellState::Own,
    };
    CellText { text, state }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::columns::column;
    use crate::core::edit::Edit;
    use crate::core::sheet::tests::{line, push, spx};
    use crate::core::shorthand::parse_builtin;
    use crate::core::template::Template;
    use geode_core::pricing::{Barrier, BarrierKind};

    fn sheet_of(lines: &[&str]) -> Sheet {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            lines.iter().map(|l| parse_builtin(l).unwrap()).collect(),
        );
        s
    }

    fn text(s: &Sheet, row: usize, name: &str) -> (String, CellState) {
        let def = column(name).unwrap();
        let c = aggregate(s, row, def.kind, &def.default_format);
        (c.text, c.state)
    }

    #[test]
    fn a_call_spread_shows_one_underlying_one_expiry_and_both_strikes() {
        let s = sheet_of(&["-5 SPX Z26 7400/7800 CS"]);
        assert_eq!(text(&s, 0, "underlying").0, "SPX");
        assert_eq!(text(&s, 0, "expiry").0, "Z26");
        assert_eq!(text(&s, 0, "strike").0, "7400/7800");
        assert_eq!(text(&s, 0, "type").0, "C");
        assert_eq!(
            text(&s, 0, "qty"),
            ("-5".into(), CellState::Own),
            "the package quantity"
        );
        assert_eq!(text(&s, 0, "barrier").0, "", "no barrier leg");
    }

    #[test]
    fn distinct_values_keep_leg_order_and_collapse_repeats() {
        let s = sheet_of(&[
            "SPX Z26 7600 STRD",
            "SPX Z26 7400/7600/7800 FLY",
            "SPX Z26/H27 7600 CAL",
            "SPX Z26 7400/7800 RR",
        ]);
        let pkg = |i: usize| s.roots().nth(i).unwrap();
        assert_eq!(
            text(&s, pkg(0), "strike").0,
            "7600",
            "a straddle's one strike"
        );
        assert_eq!(text(&s, pkg(0), "type").0, "C/P");
        assert_eq!(
            text(&s, pkg(1), "strike").0,
            "7400/7600/7800",
            "the fly's body once"
        );
        assert_eq!(
            text(&s, pkg(2), "expiry").0,
            "H27/Z26",
            "leg order: CAL's first leg is the far expiry"
        );
        assert_eq!(text(&s, pkg(3), "type").0, "P/C", "RR: short put leg first");
    }

    #[test]
    fn qty_falls_back_to_the_leg_list_when_the_legs_do_not_fit() {
        let mut s = sheet_of(&["-5 SPX Z26 7400/7800 CS"]);
        s.apply(Edit::SetQty { row: 2, qty: 3 }).unwrap(); // legs -5 / 3: no longer a CS
        assert_eq!(text(&s, 0, "qty").0, "-5/3");
        let mut c = Sheet::new("t");
        push(&mut c, vec![line(spx(5000.0, OptionKind::Call), 2)]);
        push(&mut c, vec![line(spx(4000.0, OptionKind::Put), 2)]);
        c.apply(Edit::Group {
            first: 0,
            count: 2,
            template: Template::CUSTOM,
            id: None,
        })
        .unwrap();
        assert_eq!(
            text(&c, 0, "qty").0,
            "2",
            "a custom package's legs: one distinct quantity"
        );
    }

    #[test]
    fn barrier_columns_read_only_barrier_legs() {
        let mut s = Sheet::new("t");
        let barrier = Instrument::Barrier(Barrier {
            vanilla: match spx(5000.0, OptionKind::Call) {
                Instrument::Vanilla(v) => v,
                _ => unreachable!(),
            },
            barrier: BarrierKind::UpOut,
            level: 5500.0,
        });
        push(&mut s, vec![line(barrier, 1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s.apply(Edit::Group {
            first: 0,
            count: 2,
            template: Template::CUSTOM,
            id: None,
        })
        .unwrap();
        assert_eq!(text(&s, 0, "barrier").0, "5500");
        assert_eq!(text(&s, 0, "barrier_type").0, "UO");
    }

    #[test]
    fn a_group_opens_for_editing_in_the_line_editors_spelling() {
        let mut s = sheet_of(&["SPX Z26 7400/7800 CS"]);
        s.apply(Edit::SetShift {
            row: 1,
            shift: OwnShifts {
                spot_pct: Some(2.0),
                vol_pts: None,
            },
        })
        .unwrap();
        let def = column("spot_shift").unwrap();
        let gs = groups(&s, 0, def.kind, &def.default_format);
        let parts: Vec<_> = gs
            .iter()
            .map(|g| (g.display.as_str(), g.edit.as_str(), g.legs.clone()))
            .collect();
        assert_eq!(
            parts,
            vec![("+2.0", "2", vec![1]), ("", "", vec![2])],
            "the cell shows the signed shift; the editor opens on the plain number"
        );
        assert_eq!(
            aggregate(&s, 0, def.kind, &def.default_format).text,
            "+2.0/—",
            "a leg with no shift paints its part as a dash"
        );
    }

    #[test]
    fn shifts_spelled_alike_show_once() {
        let mut s = sheet_of(&["SPX Z26 7400/7800 CS"]);
        s.apply(Edit::SetSheetShift(OwnShifts {
            spot_pct: Some(2.0),
            vol_pts: None,
        }))
        .unwrap();
        s.apply(Edit::SetShift {
            row: 1,
            shift: OwnShifts {
                spot_pct: Some(2.04),
                vol_pts: None,
            },
        })
        .unwrap();
        let def = column("spot_shift").unwrap();
        let gs = groups(&s, 0, def.kind, &def.default_format);
        let parts: Vec<_> = gs
            .iter()
            .map(|g| (g.display.as_str(), g.edit.as_str(), g.legs.clone()))
            .collect();
        assert_eq!(
            parts,
            vec![("+2.0", "2.04", vec![1, 2])],
            "an own 2.04 and an inherited 2.0 both paint +2.0: one group, edited as its first leg's value"
        );
        assert_eq!(text(&s, 0, "spot_shift"), ("+2.0".into(), CellState::Own));
    }

    #[test]
    fn shifts_group_by_effective_value_and_mute_when_all_inherit() {
        let mut s = sheet_of(&["SPX Z26 7400/7800 CS"]);
        assert_eq!(
            text(&s, 0, "spot_shift"),
            ("".into(), CellState::Blank),
            "nothing set anywhere"
        );
        s.apply(Edit::SetSheetShift(OwnShifts {
            spot_pct: Some(2.0),
            vol_pts: None,
        }))
        .unwrap();
        let (t, state) = text(&s, 0, "spot_shift");
        assert_eq!(state, CellState::Inherited, "every leg inherits");
        assert!(!t.contains('/'), "one effective value: {t}");
        s.apply(Edit::SetShift {
            row: 2,
            shift: OwnShifts {
                spot_pct: Some(2.0),
                vol_pts: None,
            },
        })
        .unwrap();
        let (t2, state2) = text(&s, 0, "spot_shift");
        assert_eq!(t2, t, "an own 2 and an inherited 2 are one value");
        assert_eq!(state2, CellState::Own, "one leg sets its own");
    }
}
