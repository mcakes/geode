//! A package row's aggregated cells (package-row spec): its legs' distinct
//! values in leg order, joined with `/`, and the package quantity while
//! the legs fit the package's template. Values compare as values, not as
//! text, and each part is spelled as a line's cell spells it. Pure: the
//! grid model prepares these cells outside render, as it does a line's.

use crate::core::columns::{CellState, CellText, ColumnKind, signed};
use crate::core::edit::Edit;
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
            let display = |v: &Option<f64>| v.map(|v| signed(v, format)).unwrap_or_default();
            // Shifts group by their spelled text: an own 2.04 and an
            // inherited 2.0 that both paint `+2.0` are one part, so the
            // cell never shows two parts that read alike. The edit
            // spelling is set per group below.
            let mut gs = group_by_key(
                sheet
                    .children(row)
                    .map(|l| (l, pick(sheet.shift(l)).or(sheet_value))),
                display,
                |v| (display(v), String::new()),
            );
            // The editor opens on an own value only, as the line editor
            // does: a group whose legs all inherit opens empty, else on its
            // first own leg's value. Opening on the effective value would
            // make an unchanged Enter detach inherited legs.
            for g in &mut gs {
                g.edit = g
                    .legs
                    .iter()
                    .find_map(|&l| pick(sheet.shift(l)))
                    .map(plain)
                    .unwrap_or_default();
            }
            gs
        }
        _ => Vec::new(),
    }
}

/// The groups joined as a mixed cell paints them: an unset part is
/// [`UNSET`], so the parts line up with the legs' values. The cell and the
/// wrong-count refusal both read this, so the refusal quotes what is shown.
fn painted(gs: &[Group]) -> String {
    gs.iter()
        .map(|g| {
            if g.display.is_empty() {
                UNSET
            } else {
                g.display.as_str()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// The text the editor opens on: the package quantity while the legs fit
/// the template, else the groups in the line editor's spellings joined
/// with `/`. `None` for a column that does not aggregate. `format` is the
/// planned column's — the one the cell paints with — because shifts group
/// by their spelled text: another precision would count other groups.
pub fn editor_text(
    sheet: &Sheet,
    row: usize,
    kind: ColumnKind,
    format: &ColumnFormat,
) -> Option<String> {
    if !aggregates(kind) {
        return None;
    }
    if kind == ColumnKind::Qty
        && let Some((q, _)) = package_qty(sheet, row)
    {
        return Some(q.to_string());
    }
    // No leg the column reads (a barrier column on vanillas): the cell
    // refuses at open, as a vanilla line's does.
    let gs = groups(sheet, row, kind, format);
    if gs.is_empty() {
        return None;
    }
    Some(
        gs.iter()
            .map(|g| g.edit.as_str())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// The edits a committed package cell means. One part goes to every
/// group; a `/` list of exactly as many parts as groups maps by position;
/// any other count is refused quoting the cell as painted. The package
/// quantity (while the legs fit the template) takes one non-zero integer
/// and sets each leg to it times the leg's weight. Every part is validated
/// through the line cell's own `edit_for` before anything is returned, so
/// a refusal changes nothing; only legs that change produce an edit, and
/// an empty vector is no change. `format` is the planned column's, as for
/// [`editor_text`], so the groups and the refusal match the painted cell.
pub fn commit(
    sheet: &Sheet,
    row: usize,
    kind: ColumnKind,
    format: &ColumnFormat,
    text: &str,
) -> Result<Vec<Edit>, String> {
    let read_only = || String::from(crate::core::cell::READ_ONLY);
    if !aggregates(kind) {
        return Err(read_only());
    }
    let t = text.trim();
    if kind == ColumnKind::Qty
        && let Some((_, weights)) = package_qty(sheet, row)
    {
        if t.contains('/') {
            return Err("one quantity".into());
        }
        let q: i64 = t
            .parse()
            .map_err(|_| format!("quantity '{t}' is not a whole number"))?;
        if q == 0 {
            return Err("quantity must not be zero".into());
        }
        let mut edits = Vec::new();
        for (leg, w) in sheet.children(row).zip(weights) {
            let qty = q.checked_mul(w).ok_or("quantity out of range")?;
            if qty != sheet.qty(leg) {
                edits.push(Edit::SetQty { row: leg, qty });
            }
        }
        return Ok(edits);
    }
    let gs = groups(sheet, row, kind, format);
    if gs.is_empty() {
        return Err(read_only());
    }
    let parts: Vec<&str> = t.split('/').collect();
    if parts.len() != 1 && parts.len() != gs.len() {
        let n = gs.len();
        let s = if n == 1 { "" } else { "s" };
        return Err(format!("{n} value{s}: {}", painted(&gs)));
    }
    let part = |i: usize| -> &str { if parts.len() == 1 { parts[0] } else { parts[i] } };
    let mut edits = Vec::new();
    for (i, g) in gs.iter().enumerate() {
        // A part typed back as the group opened is no change for the whole
        // group: a merged shift group (own 2.04, inherited 2.0) must not
        // rewrite its other legs to the first own value.
        if part(i).trim() == g.edit {
            continue;
        }
        for &leg in &g.legs {
            let edit = crate::core::cell::edit_for(sheet, leg, kind, part(i))?;
            if let Some(edit) = crate::core::cell::changed(sheet, leg, edit) {
                edits.push(edit);
            }
        }
    }
    Ok(edits)
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
    let text = painted(&gs);
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

    /// The column's default format, as a view without overrides plans it.
    fn fmt(kind: ColumnKind) -> &'static ColumnFormat {
        &crate::core::columns::COLUMNS
            .iter()
            .find(|c| c.kind == kind)
            .unwrap()
            .default_format
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

    fn apply(s: &mut Sheet, row: usize, name: &str, text: &str) -> Result<usize, String> {
        let kind = column(name).unwrap().kind;
        let edits = commit(s, row, kind, fmt(kind), text)?;
        let n = edits.len();
        for e in edits {
            s.apply(e).unwrap();
        }
        Ok(n)
    }

    #[test]
    fn a_single_value_goes_to_every_leg() {
        let mut s = sheet_of(&["-5 SPX Z26 7400/7800 CS"]);
        assert_eq!(apply(&mut s, 0, "underlying", "sx5e"), Ok(2));
        assert_eq!(text(&s, 0, "underlying").0, "SX5E");
        assert_eq!(apply(&mut s, 0, "expiry", "H27"), Ok(2));
        assert_eq!(text(&s, 0, "expiry").0, "H27");
        assert_eq!(apply(&mut s, 0, "strike", "7600"), Ok(2));
        assert_eq!(text(&s, 0, "strike").0, "7600", "both legs on one strike");
    }

    #[test]
    fn a_list_maps_by_position_and_a_fly_body_moves_once() {
        let mut s = sheet_of(&["-5 SPX Z26 7400/7800 CS"]);
        assert_eq!(apply(&mut s, 0, "strike", "7500/7900"), Ok(2));
        assert_eq!(text(&s, 0, "strike").0, "7500/7900");
        assert_eq!(s.shorthand(0), "-5 SPX Z26 7500/7900 CS", "still a CS");
        let mut f = sheet_of(&["SPX Z26 7400/7600/7800 FLY"]);
        assert_eq!(
            apply(&mut f, 0, "strike", "7300/7600/7900"),
            Ok(2),
            "the body is unchanged: two edits"
        );
        assert_eq!(f.shorthand(0), "SPX Z26 7300/7600/7900 FLY");
        let mut c = sheet_of(&["SPX Z26/H27 7600 CAL"]);
        let before = text(&c, 0, "expiry").0;
        let parts: Vec<&str> = before.split('/').collect();
        assert_eq!(
            apply(&mut c, 0, "expiry", &format!("{}/{}", parts[0], "M27")),
            Ok(1)
        );
    }

    #[test]
    fn a_wrong_count_or_a_bad_part_refuses_and_changes_nothing() {
        let mut s = sheet_of(&["-5 SPX Z26 7400/7800 CS"]);
        assert_eq!(
            apply(&mut s, 0, "strike", "7400/7600/7800"),
            Err("2 values: 7400/7800".into())
        );
        assert!(apply(&mut s, 0, "strike", "7500/abc").is_err());
        assert_eq!(text(&s, 0, "strike").0, "7400/7800", "nothing applied");
    }

    #[test]
    fn a_wrong_count_shows_the_painted_cell() {
        let mut s = sheet_of(&["SPX Z26 7400/7800 CS"]);
        s.apply(Edit::SetShift {
            row: 1,
            shift: OwnShifts {
                spot_pct: Some(2.0),
                vol_pts: None,
            },
        })
        .unwrap();
        assert_eq!(
            apply(&mut s, 0, "spot_shift", "1/2/3"),
            Err("2 values: +2.0/—".into()),
            "an unset part reads as the cell paints it"
        );
    }

    #[test]
    fn package_qty_rescales_legs_by_weight() {
        let mut f = sheet_of(&["SPX Z26 7400/7600/7800 FLY"]);
        assert_eq!(apply(&mut f, 0, "qty", "10"), Ok(3));
        assert_eq!(f.shorthand(0), "10 SPX Z26 7400/7600/7800 FLY");
        let legs: Vec<i64> = f.children(0).map(|l| f.qty(l)).collect();
        assert_eq!(legs, [10, -20, 10]);
        assert_eq!(apply(&mut f, 0, "qty", "1/2"), Err("one quantity".into()));
        assert_eq!(
            apply(&mut f, 0, "qty", "0"),
            Err("quantity must not be zero".into())
        );
        assert_eq!(
            apply(&mut f, 0, "qty", &i64::MAX.to_string()),
            Err("quantity out of range".into())
        );
    }

    #[test]
    fn qty_in_list_form_maps_by_position() {
        let mut s = sheet_of(&["-5 SPX Z26 7400/7800 CS"]);
        s.apply(Edit::SetQty { row: 2, qty: 3 }).unwrap();
        assert_eq!(apply(&mut s, 0, "qty", "-4/4"), Ok(2));
        assert_eq!(text(&s, 0, "qty").0, "-4", "back in CS form");
    }

    #[test]
    fn an_unchanged_commit_is_no_edit_and_crossed_strikes_are_allowed() {
        let mut s = sheet_of(&["-5 SPX Z26 7400/7800 CS"]);
        assert_eq!(apply(&mut s, 0, "strike", "7400/7800"), Ok(0));
        // Crossed strikes are allowed (the CS table has no strike order, so
        // it still prints as a CS, now long the higher strike).
        assert_eq!(apply(&mut s, 0, "strike", "7800/7400"), Ok(2));
        assert_eq!(s.shorthand(0), "-5 SPX Z26 7800/7400 CS");
        // A change that breaks the table keeps the name and prints the legs.
        assert_eq!(
            apply(&mut s, 0, "type", "P/C"),
            Err("1 value: C".into()),
            "one type shown: one value or refused"
        );
        assert_eq!(apply(&mut s, 0, "type", "P"), Ok(2));
        assert!(
            s.shorthand(0).contains('\n'),
            "puts no longer fit CS: legs one per line"
        );
        assert_eq!(
            s.kind(0),
            crate::core::sheet::RowKind::Package {
                template: Template::CS
            },
            "keeps its name"
        );
    }

    #[test]
    fn the_editor_opens_on_the_line_editors_spellings() {
        let s = sheet_of(&["-5 SPX Z26 7400/7800 CS"]);
        assert_eq!(
            editor_text(&s, 0, ColumnKind::Strike, fmt(ColumnKind::Strike)).as_deref(),
            Some("7400/7800")
        );
        assert_eq!(
            editor_text(&s, 0, ColumnKind::Qty, fmt(ColumnKind::Qty)).as_deref(),
            Some("-5")
        );
        assert_eq!(
            editor_text(&s, 0, ColumnKind::Price, fmt(ColumnKind::Price)),
            None
        );
        assert_eq!(
            editor_text(&s, 0, ColumnKind::Barrier, fmt(ColumnKind::Barrier)),
            None,
            "no leg reads a barrier: the editor refuses at open"
        );
        assert_eq!(
            editor_text(&s, 0, ColumnKind::BarrierType, fmt(ColumnKind::BarrierType)),
            None
        );
    }

    /// Commits the text the editor opens on, unchanged.
    fn enter_unchanged(s: &mut Sheet, name: &str) -> Result<usize, String> {
        let kind = column(name).unwrap().kind;
        let opened = editor_text(s, 0, kind, fmt(kind)).expect("the cell opens");
        apply(s, 0, name, &opened)
    }

    #[test]
    fn an_inherited_shift_opens_empty_and_enter_changes_nothing() {
        let mut s = sheet_of(&["SPX Z26 7400/7800 CS"]);
        s.apply(Edit::SetSheetShift(OwnShifts {
            spot_pct: Some(2.0),
            vol_pts: None,
        }))
        .unwrap();
        assert_eq!(
            editor_text(&s, 0, ColumnKind::SpotShift, fmt(ColumnKind::SpotShift)).as_deref(),
            Some(""),
            "no leg owns a shift: the line editor's rule"
        );
        assert_eq!(
            enter_unchanged(&mut s, "spot_shift"),
            Ok(0),
            "no leg is detached"
        );
        assert_eq!(text(&s, 0, "spot_shift").1, CellState::Inherited);
        assert_eq!(
            apply(&mut s, 0, "spot_shift", "3"),
            Ok(2),
            "a new value sets every leg"
        );
    }

    #[test]
    fn a_merged_own_and_inherited_group_commits_unchanged_as_no_edit() {
        let mut s = sheet_of(&["SPX Z26 7400/7800 CS"]);
        s.apply(Edit::SetSheetShift(OwnShifts {
            spot_pct: Some(2.0),
            vol_pts: None,
        }))
        .unwrap();
        s.apply(Edit::SetShift {
            row: 2,
            shift: OwnShifts {
                spot_pct: Some(2.04),
                vol_pts: None,
            },
        })
        .unwrap();
        assert_eq!(
            editor_text(&s, 0, ColumnKind::SpotShift, fmt(ColumnKind::SpotShift)).as_deref(),
            Some("2.04"),
            "the first leg that owns one"
        );
        assert_eq!(
            enter_unchanged(&mut s, "spot_shift"),
            Ok(0),
            "the inherited leg is not rewritten"
        );
        assert_eq!(s.shift(1).spot_pct, None);
        assert_eq!(
            apply(&mut s, 0, "spot_shift", ""),
            Ok(1),
            "empty clears the one own leg"
        );
        assert_eq!(s.shift(2).spot_pct, None);
    }

    #[test]
    fn a_view_precision_groups_the_cell_the_editor_and_the_commit_alike() {
        let mut s = sheet_of(&["SPX Z26 7400/7800 CS"]);
        for (row, v) in [(1, 2.25), (2, 2.30)] {
            s.apply(Edit::SetShift {
                row,
                shift: OwnShifts {
                    spot_pct: Some(v),
                    vol_pts: None,
                },
            })
            .unwrap();
        }
        // A view override: two places where the column default has one.
        let two = ColumnFormat {
            precision: 2,
            ..crate::core::columns::SHIFT
        };
        let kind = ColumnKind::SpotShift;
        assert_eq!(aggregate(&s, 0, kind, &two).text, "+2.25/+2.30");
        assert_eq!(
            editor_text(&s, 0, kind, &two).as_deref(),
            Some("2.25/2.3"),
            "the editor opens on the two parts the cell paints"
        );
        assert_eq!(
            commit(&s, 0, kind, &two, "1/2/3"),
            Err("2 values: +2.25/+2.30".into()),
            "the refusal quotes the cell on screen"
        );
        let edits = commit(&s, 0, kind, &two, "2.25/2.4").unwrap();
        assert_eq!(
            edits.len(),
            1,
            "two groups by position: only the second leg moves"
        );
    }
}
