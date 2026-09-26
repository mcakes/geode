//! The cell editor's pure half: what an editable cell opens with,
//! whether a commit produces an `Edit`, and how an arrow
//! key nudges the open text. The tile only opens an `InputState` (or, for
//! an expiry, a segmented date field) on the answer and hands the
//! committed text or date back here.

use crate::core::columns::ColumnKind;
use crate::core::edit::Edit;
use crate::core::sheet::{OwnShifts, Sheet};
use crate::core::shorthand::{
    parse_barrier_kind, parse_expiry, parse_strike, render_barrier_kind, render_strike,
};
use chrono::NaiveDate;
use geode_core::nudge::nudge_text;
use geode_core::pricing::{Expiry, Instrument, OptionKind, Vanilla};
use geode_core::schema::ColumnType;

/// The footer's word for a cell that does not edit.
pub const READ_ONLY: &str = "read-only";

const TYPES: [&str; 2] = ["C", "P"];
const BARRIER_TYPES: [&str; 4] = ["UI", "UO", "DI", "DO"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellEditor {
    /// A text field opened on this text.
    Text(String),
    /// A typeahead over `options`, highlighted on `current`; `free` lets an
    /// unmatched query commit as typed, as in the underlying cell.
    Choice {
        options: Vec<String>,
        current: String,
        free: bool,
    },
    /// A segmented date field for every expiry. `Some` is a date expiry's date;
    /// `None` is a tenor, which has no date here: the pricer never
    /// resolves a tenor (the library's calendar does), so the host seeds
    /// the field from its clock's today.
    Date(Option<NaiveDate>),
}

/// The line's instrument, or `READ_ONLY` for a package. Every editable
/// column reads an instrument; package cells are derived from their legs.
fn instrument(sheet: &Sheet, row: usize) -> Result<&Instrument, &'static str> {
    sheet.instrument(row).ok_or(READ_ONLY)
}

fn barrier(i: &Instrument) -> Result<(f64, geode_core::pricing::BarrierKind), &'static str> {
    match i {
        Instrument::Barrier(b) => Ok((b.level, b.barrier)),
        Instrument::Vanilla(_) => Err(READ_ONLY),
    }
}

/// `{}` of an `f64`: `2` for `2.0`, `4250.5` as typed — the editor opens on
/// the shortest spelling that parses back.
fn plain(v: f64) -> String {
    format!("{v}")
}

fn kind_token(kind: OptionKind) -> &'static str {
    match kind {
        OptionKind::Call => "C",
        OptionKind::Put => "P",
    }
}

/// What the editor opens with on (`row`, `kind`), or why it does not open.
pub fn editor_for(sheet: &Sheet, row: usize, kind: ColumnKind) -> Result<CellEditor, &'static str> {
    let i = instrument(sheet, row)?;
    Ok(match kind {
        ColumnKind::Qty => CellEditor::Text(sheet.qty(row).to_string()),
        ColumnKind::Expiry => CellEditor::Date(match i.expiry() {
            Expiry::Date(d) => Some(*d),
            Expiry::Tenor(_) => None,
        }),
        ColumnKind::Strike => CellEditor::Text(render_strike(i.strike())),
        ColumnKind::Barrier => CellEditor::Text(plain(barrier(i)?.0)),
        ColumnKind::SpotShift => {
            CellEditor::Text(sheet.shift(row).spot_pct.map(plain).unwrap_or_default())
        }
        ColumnKind::VolShift => {
            CellEditor::Text(sheet.shift(row).vol_pts.map(plain).unwrap_or_default())
        }
        ColumnKind::Type => CellEditor::Choice {
            options: TYPES.iter().map(|s| s.to_string()).collect(),
            current: kind_token(i.kind()).to_string(),
            free: false,
        },
        ColumnKind::BarrierType => CellEditor::Choice {
            options: BARRIER_TYPES.iter().map(|s| s.to_string()).collect(),
            current: render_barrier_kind(barrier(i)?.1).to_string(),
            free: false,
        },
        ColumnKind::Underlying => {
            let mut options: Vec<String> = (0..sheet.len())
                .filter_map(|r| sheet.instrument(r).map(|i| i.underlying().to_string()))
                .collect();
            options.sort();
            options.dedup();
            CellEditor::Choice {
                options,
                current: i.underlying().to_string(),
                free: true,
            }
        }
        ColumnKind::Price
        | ColumnKind::Delta
        | ColumnKind::Gamma
        | ColumnKind::Vega
        | ColumnKind::Theta
        | ColumnKind::Rho
        | ColumnKind::PricedAt
        | ColumnKind::Status => return Err(READ_ONLY),
    })
}

/// The instrument with its vanilla part changed — through a barrier too.
fn with_vanilla(i: &Instrument, f: impl FnOnce(&mut Vanilla)) -> Instrument {
    let mut out = i.clone();
    match &mut out {
        Instrument::Vanilla(v) => f(v),
        Instrument::Barrier(b) => f(&mut b.vanilla),
    }
    out
}

fn set(row: usize, instrument: Instrument) -> Edit {
    Edit::SetInstrument { row, instrument }
}

fn shift(text: &str, what: &str) -> Result<Option<f64>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    t.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .map(Some)
        .ok_or_else(|| format!("{what} '{t}' is not a number"))
}

/// Validate a committed cell and return its edit, `Ok(None)` when the parsed
/// value is unchanged, or a refusal for the footer. The tile checks the target
/// line and column before applying the edit so it cannot change another cell.
pub fn commit(
    sheet: &Sheet,
    row: usize,
    kind: ColumnKind,
    text: &str,
) -> Result<Option<Edit>, String> {
    edit_for(sheet, row, kind, text).map(|edit| changed(sheet, row, edit))
}

/// `edit` unless it would leave the line exactly as it is. Values are
/// compared, never text: `5000` and `5000.0` are one strike, and an empty
/// shift on an inherited one stays inherited — while an explicit value is
/// a change from inherited to own even when it equals what was inherited.
/// An unchanged commit is no edit: no undo entry, no reprice, no save.
fn changed(sheet: &Sheet, row: usize, edit: Edit) -> Option<Edit> {
    let same = match &edit {
        Edit::SetQty { qty, .. } => *qty == sheet.qty(row),
        Edit::SetShift { shift, .. } => *shift == sheet.shift(row),
        Edit::SetInstrument { instrument, .. } => sheet.instrument(row) == Some(instrument),
        _ => false,
    };
    (!same).then_some(edit)
}

fn edit_for(sheet: &Sheet, row: usize, kind: ColumnKind, text: &str) -> Result<Edit, String> {
    let i = instrument(sheet, row).map_err(String::from)?;
    let t = text.trim();
    match kind {
        ColumnKind::Qty => {
            let qty: i64 = t
                .parse()
                .map_err(|_| format!("quantity '{t}' is not a whole number"))?;
            if qty == 0 {
                return Err("quantity must not be zero".into());
            }
            Ok(Edit::SetQty { row, qty })
        }
        ColumnKind::Underlying => {
            if t.is_empty() || t.contains(char::is_whitespace) {
                return Err(format!("underlying '{t}': one word"));
            }
            let u = t.to_ascii_uppercase();
            Ok(set(row, with_vanilla(i, |v| v.underlying = u)))
        }
        ColumnKind::Expiry => {
            let e = parse_expiry(t)?;
            Ok(set(row, with_vanilla(i, |v| v.expiry = e)))
        }
        ColumnKind::Strike => {
            let s = parse_strike(t)?;
            Ok(set(row, with_vanilla(i, |v| v.strike = s)))
        }
        ColumnKind::Type => {
            let k = match t.to_ascii_uppercase().as_str() {
                "C" => OptionKind::Call,
                "P" => OptionKind::Put,
                _ => return Err(format!("type '{t}': C or P")),
            };
            Ok(set(row, with_vanilla(i, |v| v.kind = k)))
        }
        ColumnKind::Barrier => {
            barrier(i).map_err(String::from)?;
            let level = t
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v > 0.0)
                .ok_or_else(|| format!("barrier '{t}' is not a number"))?;
            let mut out = i.clone();
            if let Instrument::Barrier(b) = &mut out {
                b.level = level;
            }
            Ok(set(row, out))
        }
        ColumnKind::BarrierType => {
            barrier(i).map_err(String::from)?;
            let k =
                parse_barrier_kind(t).ok_or_else(|| format!("barrier type '{t}': UI UO DI DO"))?;
            let mut out = i.clone();
            if let Instrument::Barrier(b) = &mut out {
                b.barrier = k;
            }
            Ok(set(row, out))
        }
        ColumnKind::SpotShift => {
            let own = sheet.shift(row);
            Ok(Edit::SetShift {
                row,
                shift: OwnShifts {
                    spot_pct: shift(t, "spot shift")?,
                    ..own
                },
            })
        }
        ColumnKind::VolShift => {
            let own = sheet.shift(row);
            Ok(Edit::SetShift {
                row,
                shift: OwnShifts {
                    vol_pts: shift(t, "vol shift")?,
                    ..own
                },
            })
        }
        ColumnKind::Price
        | ColumnKind::Delta
        | ColumnKind::Gamma
        | ColumnKind::Vega
        | ColumnKind::Theta
        | ColumnKind::Rho
        | ColumnKind::PricedAt
        | ColumnKind::Status => Err(READ_ONLY.into()),
    }
}

/// The one `Edit` a committed expiry date means, `Ok(None)` when the line
/// already expires on exactly that date (nothing to apply: no undo entry,
/// no reprice, no save), or the footer's refusal. A tenor line always
/// changes — committing turns it into a date expiry.
pub fn commit_date(sheet: &Sheet, row: usize, date: NaiveDate) -> Result<Option<Edit>, String> {
    let i = instrument(sheet, row).map_err(String::from)?;
    let edit = set(row, with_vanilla(i, |v| v.expiry = Expiry::Date(date)));
    Ok(changed(sheet, row, edit))
}

/// `up`/`down` in an open numeric editor (spec §8.4): `steps` units of the
/// TEXT's own precision (planning decision 2), a strike's trailing `%`
/// kept, an empty shift nudged from `0`. A barrier level is absolute
/// (`commit` refuses a `%` on one), so a barrier's `%` is not a number to
/// nudge either — the two answer the same text the same way.
pub fn nudge(kind: ColumnKind, text: &str, steps: i64) -> Result<String, String> {
    let t = text.trim();
    match kind {
        ColumnKind::Qty => nudge_text(t, ColumnType::I64, None, steps),
        ColumnKind::Strike => match t.strip_suffix('%') {
            Some(n) => nudge_text(n, ColumnType::F64, None, steps).map(|s| format!("{s}%")),
            None => nudge_text(t, ColumnType::F64, None, steps),
        },
        ColumnKind::Barrier => nudge_text(t, ColumnType::F64, None, steps),
        ColumnKind::SpotShift | ColumnKind::VolShift => nudge_text(
            if t.is_empty() { "0" } else { t },
            ColumnType::F64,
            None,
            steps,
        ),
        _ => Err("this cell does not nudge".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::edit::Edit;
    use crate::core::sheet::tests::{callspread, line, push, spx};
    use crate::core::sheet::{OwnShifts, Sheet};
    use geode_core::pricing::{Barrier, BarrierKind, Expiry, Instrument, OptionKind, Strike};

    fn one_line() -> Sheet {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), -5)]);
        s
    }

    fn barrier_line() -> Sheet {
        let mut s = Sheet::new("t");
        let Instrument::Vanilla(v) = spx(5000.0, OptionKind::Call) else {
            unreachable!()
        };
        push(
            &mut s,
            vec![line(
                Instrument::Barrier(Barrier {
                    vanilla: v,
                    level: 4200.0,
                    barrier: BarrierKind::DownOut,
                }),
                1,
            )],
        );
        s
    }

    #[test]
    fn a_text_cell_opens_on_the_grammar_spelling_of_its_value() {
        let s = one_line();
        assert_eq!(
            editor_for(&s, 0, ColumnKind::Qty),
            Ok(CellEditor::Text("-5".into()))
        );
        assert_eq!(
            editor_for(&s, 0, ColumnKind::Expiry),
            Ok(CellEditor::Date(Some(
                chrono::NaiveDate::from_ymd_opt(2026, 12, 18).unwrap()
            ))),
            "an expiry always edits in a date field, on its own date"
        );
        assert_eq!(
            editor_for(&s, 0, ColumnKind::Strike),
            Ok(CellEditor::Text("5000".into()))
        );
        // An inherited shift opens EMPTY: an empty commit means "inherit".
        assert_eq!(
            editor_for(&s, 0, ColumnKind::SpotShift),
            Ok(CellEditor::Text(String::new()))
        );
        let b = barrier_line();
        assert_eq!(
            editor_for(&b, 0, ColumnKind::Barrier),
            Ok(CellEditor::Text("4200".into()))
        );
    }

    #[test]
    fn choice_cells_offer_their_vocabulary_and_the_underlying_takes_free_text() {
        let mut s = one_line();
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        assert_eq!(
            editor_for(&s, 0, ColumnKind::Type),
            Ok(CellEditor::Choice {
                options: vec!["C".into(), "P".into()],
                current: "C".into(),
                free: false
            })
        );
        assert_eq!(
            editor_for(&s, 0, ColumnKind::Underlying),
            Ok(CellEditor::Choice {
                options: vec!["SPX".into()],
                current: "SPX".into(),
                free: true
            }),
            "the sheet's own underlyings, deduplicated and sorted"
        );
        let b = barrier_line();
        assert_eq!(
            editor_for(&b, 0, ColumnKind::BarrierType),
            Ok(CellEditor::Choice {
                options: vec!["UI".into(), "UO".into(), "DI".into(), "DO".into()],
                current: "DO".into(),
                free: false
            })
        );
    }

    #[test]
    fn results_packages_and_barrier_columns_on_a_vanilla_are_read_only() {
        let mut s = one_line();
        push(&mut s, vec![callspread(1)]);
        for kind in [
            ColumnKind::Price,
            ColumnKind::Delta,
            ColumnKind::Gamma,
            ColumnKind::Vega,
            ColumnKind::Theta,
            ColumnKind::Rho,
            ColumnKind::PricedAt,
            ColumnKind::Status,
            ColumnKind::Barrier,
            ColumnKind::BarrierType,
        ] {
            assert_eq!(editor_for(&s, 0, kind), Err(READ_ONLY), "{kind:?}");
        }
        for kind in [ColumnKind::Qty, ColumnKind::Strike, ColumnKind::SpotShift] {
            assert_eq!(editor_for(&s, 1, kind), Err(READ_ONLY), "package {kind:?}");
        }
    }

    #[test]
    fn a_commit_becomes_the_one_edit_its_column_means() {
        let s = one_line();
        assert_eq!(
            commit(&s, 0, ColumnKind::Qty, " 10 "),
            Ok(Some(Edit::SetQty { row: 0, qty: 10 }))
        );
        let Ok(Some(Edit::SetInstrument { row: 0, instrument })) =
            commit(&s, 0, ColumnKind::Strike, "95%")
        else {
            panic!("a strike commit is SetInstrument")
        };
        assert_eq!(instrument.strike(), Strike::Percent(95.0));
        let Ok(Some(Edit::SetInstrument { instrument, .. })) =
            commit(&s, 0, ColumnKind::Expiry, "3m")
        else {
            panic!()
        };
        assert_eq!(instrument.expiry(), &Expiry::Tenor("3m".into()));
        let Ok(Some(Edit::SetInstrument { instrument, .. })) = commit(&s, 0, ColumnKind::Type, "p")
        else {
            panic!()
        };
        assert_eq!(instrument.kind(), OptionKind::Put);
        let Ok(Some(Edit::SetInstrument { instrument, .. })) =
            commit(&s, 0, ColumnKind::Underlying, "ndx")
        else {
            panic!()
        };
        assert_eq!(instrument.underlying(), "NDX");
    }

    #[test]
    fn an_empty_shift_commit_inherits_and_a_signed_number_is_owned() {
        let s = one_line();
        assert_eq!(
            commit(&s, 0, ColumnKind::SpotShift, "+2"),
            Ok(Some(Edit::SetShift {
                row: 0,
                shift: OwnShifts {
                    spot_pct: Some(2.0),
                    vol_pts: None
                }
            }))
        );
        assert_eq!(
            commit(&owned_spot(), 0, ColumnKind::SpotShift, "  "),
            Ok(Some(Edit::SetShift {
                row: 0,
                shift: OwnShifts::default()
            })),
            "empty means inherit, not zero"
        );
    }

    #[test]
    fn a_bad_commit_is_refused_with_the_reason_and_names_the_text() {
        let s = one_line();
        assert_eq!(
            commit(&s, 0, ColumnKind::Qty, "0"),
            Err("quantity must not be zero".into())
        );
        assert_eq!(
            commit(&s, 0, ColumnKind::Qty, "x"),
            Err("quantity 'x' is not a whole number".into())
        );
        assert!(
            commit(&s, 0, ColumnKind::Strike, "-1")
                .unwrap_err()
                .contains("positive")
        );
        assert_eq!(
            commit(&s, 0, ColumnKind::Type, "X"),
            Err("type 'X': C or P".into())
        );
        assert_eq!(
            commit(&s, 0, ColumnKind::Underlying, "S P"),
            Err("underlying 'S P': one word".into())
        );
        assert_eq!(commit(&s, 0, ColumnKind::Price, "1"), Err(READ_ONLY.into()));
        let b = barrier_line();
        assert_eq!(
            commit(&b, 0, ColumnKind::BarrierType, "UP"),
            Err("barrier type 'UP': UI UO DI DO".into())
        );
        assert_eq!(
            commit(&b, 0, ColumnKind::Barrier, "abc"),
            Err("barrier 'abc' is not a number".into())
        );
    }

    #[test]
    fn a_nudge_steps_by_the_texts_own_precision_and_keeps_a_percent() {
        assert_eq!(nudge(ColumnKind::Strike, "5000", 1), Ok("5001".into()));
        assert_eq!(nudge(ColumnKind::Strike, "95%", -10), Ok("85%".into()));
        assert_eq!(nudge(ColumnKind::Strike, "4250.5", 1), Ok("4250.6".into()));
        assert_eq!(nudge(ColumnKind::Qty, "-5", 1), Ok("-4".into()));
        assert_eq!(
            nudge(ColumnKind::SpotShift, "", 1),
            Ok("1".into()),
            "empty nudges from 0"
        );
        assert!(nudge(ColumnKind::Expiry, "Z26", 1).is_err());
    }

    /// Review finding: a barrier level is absolute. `commit` refuses
    /// `4200%`, so `nudge` must not step it into another `%` text that
    /// only a later commit would refuse.
    #[test]
    fn a_barrier_nudges_as_a_plain_level_and_refuses_a_percent_like_commit() {
        assert_eq!(nudge(ColumnKind::Barrier, "4200", 1), Ok("4201".into()));
        assert!(nudge(ColumnKind::Barrier, "95%", 1).is_err());
        let b = barrier_line();
        assert!(commit(&b, 0, ColumnKind::Barrier, "95%").is_err());
    }

    fn tenor_line() -> Sheet {
        let mut s = Sheet::new("t");
        let i = with_vanilla(&spx(5000.0, OptionKind::Call), |v| {
            v.expiry = Expiry::Tenor("3m".into())
        });
        push(&mut s, vec![line(i, 1)]);
        s
    }

    fn ymd(y: i32, m: u32, d: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn a_tenor_expiry_opens_a_date_field_with_no_date_of_its_own() {
        assert_eq!(
            editor_for(&tenor_line(), 0, ColumnKind::Expiry),
            Ok(CellEditor::Date(None)),
            "the pricer never resolves a tenor: the host seeds the field"
        );
    }

    #[test]
    fn a_date_commit_sets_a_date_expiry_and_an_unchanged_one_is_no_edit() {
        let s = one_line();
        let Ok(Some(Edit::SetInstrument { row: 0, instrument })) =
            commit_date(&s, 0, ymd(2027, 3, 19))
        else {
            panic!("a changed date is one SetInstrument")
        };
        assert_eq!(instrument.expiry(), &Expiry::Date(ymd(2027, 3, 19)));
        assert_eq!(
            instrument.strike(),
            Strike::Absolute(5000.0),
            "only the expiry"
        );
        assert_eq!(
            commit_date(&s, 0, ymd(2026, 12, 18)),
            Ok(None),
            "the line's own date: nothing to apply"
        );
    }

    #[test]
    fn a_date_commit_on_a_tenor_line_always_makes_it_a_date() {
        let s = tenor_line();
        let Ok(Some(Edit::SetInstrument { instrument, .. })) = commit_date(&s, 0, ymd(2026, 9, 26))
        else {
            panic!("a tenor committed to a date is an edit")
        };
        assert_eq!(instrument.expiry(), &Expiry::Date(ymd(2026, 9, 26)));
    }

    #[test]
    fn a_date_commit_on_a_package_is_read_only() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![callspread(1)]);
        assert_eq!(
            commit_date(&s, 0, ymd(2026, 9, 26)),
            Err(READ_ONLY.to_string())
        );
    }

    /// One line with its own spot shift of 2%.
    fn owned_spot() -> Sheet {
        let mut s = one_line();
        s.apply(Edit::SetShift {
            row: 0,
            shift: OwnShifts {
                spot_pct: Some(2.0),
                vol_pts: None,
            },
        })
        .unwrap();
        s
    }

    /// A commit that parses to the value the line already holds is no
    /// edit, in every editable cell: values are compared, not text.
    #[test]
    fn an_unchanged_commit_is_no_edit_in_every_cell() {
        let s = one_line();
        for (kind, text) in [
            (ColumnKind::Qty, "-5"),
            (ColumnKind::Qty, " -5 "),
            (ColumnKind::Strike, "5000"),
            (ColumnKind::Strike, "5000.0"),
            (ColumnKind::Expiry, "Z26"),
            (ColumnKind::Type, "c"),
            (ColumnKind::Underlying, "spx"),
            (ColumnKind::SpotShift, ""),
            (ColumnKind::VolShift, "  "),
        ] {
            assert_eq!(commit(&s, 0, kind, text), Ok(None), "{kind:?} '{text}'");
        }
        let b = barrier_line();
        assert_eq!(commit(&b, 0, ColumnKind::Barrier, "4200.0"), Ok(None));
        assert_eq!(commit(&b, 0, ColumnKind::BarrierType, "do"), Ok(None));
        let own = owned_spot();
        assert_eq!(commit(&own, 0, ColumnKind::SpotShift, "2.0"), Ok(None));
        assert!(
            matches!(commit(&own, 0, ColumnKind::SpotShift, ""), Ok(Some(_))),
            "own → inherited is a change"
        );
        assert!(
            matches!(commit(&s, 0, ColumnKind::SpotShift, "0"), Ok(Some(_))),
            "inherited → an explicit own value is a change, whatever it equals"
        );
        assert!(
            matches!(commit(&s, 0, ColumnKind::Strike, "5000%"), Ok(Some(_))),
            "a percent strike is not the absolute one"
        );
    }
}
