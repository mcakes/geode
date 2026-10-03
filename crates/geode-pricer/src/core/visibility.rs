//! Which sheet lines a frame scope hides. Hidden lines stay in the sheet
//! and keep pricing; they are only left out of the grid, of a package
//! row's aggregate, and of selection totals. A package shows when any of
//! its legs does, and then over those legs only.
//!
//! Evaluation runs `geode_core::scope`'s in-process evaluator over each
//! line as the `pricer` dataset's row ([`SheetRow`]). The evaluator
//! reports an error on the row whose value fails, where the SQL it mirrors
//! fails the whole query; so any error refuses the whole scope and hides
//! nothing. Dropping only the failing line would narrow the sheet in a way
//! no query ever does.

use crate::core::columns::{CellState, ColumnKind, cell_text, column};
use crate::core::dataset::pricer_dataset;
use crate::core::sheet::{LineState, RowKind, Sheet};
use geode_core::clock::Clock;
use geode_core::dimensions::DerivedDimensions;
use geode_core::document::Value;
use geode_core::pricing::{Expiry, Instrument, Strike};
use geode_core::scope::{RowValues, Scope};

/// Which sheet rows show under a scope.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Visibility {
    /// Indexed by sheet row; `true` = shown. A package is shown when any
    /// of its legs is; an empty package (no legs) is shown. A row past the
    /// end shows: a line added since the scope was applied is not hidden
    /// by a verdict that never saw it.
    shown: Vec<bool>,
    /// Lines (legs and bare lines) hidden by the scope.
    pub hidden: usize,
}

impl Visibility {
    /// Every row shown: no scope, or a refused one.
    pub fn all(sheet: &Sheet) -> Visibility {
        Visibility {
            shown: vec![true; sheet.len()],
            hidden: 0,
        }
    }

    pub fn is_shown(&self, row: usize) -> bool {
        self.shown.get(row).copied().unwrap_or(true)
    }

    /// The package's shown legs in sheet order.
    pub fn shown_legs(&self, sheet: &Sheet, package: usize) -> Vec<usize> {
        sheet
            .children(package)
            .filter(|&l| self.is_shown(l))
            .collect()
    }

    /// Whether `package` hides some of its legs: its row then aggregates
    /// only the shown ones and is read-only.
    pub fn is_partial(&self, sheet: &Sheet, package: usize) -> bool {
        sheet.children(package).any(|l| !self.is_shown(l))
    }
}

/// Columns `pricer` declares that a scope may not name. The sheet's
/// `position_ref` and `instrument_ref` are its own `p<id>` / `i<id>`,
/// never a desk reference, so a desk scope over them could only ever hide
/// every line: they are treated as columns the pricer lacks. They stay
/// declared, since grouping reads them.
const NOT_SCOPEABLE: [&str; 2] = ["position_ref", "instrument_ref"];

/// Whether a scope may name `column` (a derived dimension through its
/// source) on the pricer.
fn scope_column(
    ds: &geode_core::schema::DatasetSpec,
    dims: &DerivedDimensions,
    column: &str,
) -> bool {
    let base = dims.base_column(column);
    ds.column(base).is_some() && !NOT_SCOPEABLE.contains(&base)
}

/// Apply `scope` (already the frame's effective scope, named expressions
/// resolved) to `sheet`. Dimension selections on columns `pricer` lacks
/// or may not scope by ([`NOT_SCOPEABLE`]) are dropped first; such a
/// column in the expression refuses with `scope refused: '<col>' is not
/// a pricer column`; any evaluator error refuses with `scope refused:
/// <message>`, a row-independent one even over an empty sheet. A refusal
/// hides nothing — the caller shows the message and paints `all`.
pub fn apply_scope(
    sheet: &Sheet,
    scope: &Scope,
    dims: &DerivedDimensions,
    clock: Clock,
) -> Result<Visibility, String> {
    let ds = pricer_dataset();
    // `Scope::applicable_to`'s drop, widened to the synthetic keys.
    let mut scope = scope.clone();
    scope
        .dimensions
        .retain(|d| scope_column(ds, dims, &d.column));
    if scope.is_empty() {
        return Ok(Visibility::all(sheet));
    }
    if let Some(e) = &scope.expression {
        for c in e.columns() {
            if !scope_column(ds, dims, c) {
                return Err(format!("scope refused: '{c}' is not a pricer column"));
            }
        }
    }
    let refused = |e| format!("scope refused: {e}");
    // Once, before any line: an empty sheet refuses what a full one would.
    let bound = scope.bind(ds, dims).map_err(refused)?;
    let mut shown = vec![true; sheet.len()];
    let mut hidden = 0;
    for (row, slot) in shown.iter_mut().enumerate() {
        if !sheet.is_line(row) {
            continue;
        }
        let keep = bound
            .matches(&SheetRow { sheet, row, clock })
            .map_err(refused)?;
        if !keep {
            *slot = false;
            hidden += 1;
        }
    }
    for p in 0..sheet.len() {
        if sheet.is_package(p) {
            let legs = sheet.children(p);
            shown[p] = legs.is_empty() || legs.clone().any(|l| shown[l]);
        }
    }
    Ok(Visibility { shown, hidden })
}

/// A sheet line as the `pricer` dataset's row: every declared column by
/// name, typed as declared (strike the number as typed; a shift the one
/// the cell paints, the sheet's when the line has none; measures result ×
/// qty in local and usd; blank → None). One implementation, used by the
/// evaluator, so scope values and cell values cannot drift: a text column
/// reads the text [`cell_text`] paints, except
///
/// - a leg's `template`, its package's (a position-grain column; the
///   leg's own cell is blank);
/// - `status`, `fresh` for a fresh line, whose cell paints blank (the
///   vocabulary is `fresh` / `pricing…` / the failure text);
/// - `expiry`, the ISO date `YYYY-MM-DD` of a dated expiry, as desk data
///   spells it, where the cell paints `Z26` / `20DEC26`; a tenor is its
///   text.
pub struct SheetRow<'a> {
    pub sheet: &'a Sheet,
    pub row: usize,
    pub clock: Clock,
}

impl RowValues for SheetRow<'_> {
    fn value(&self, name: &str) -> Option<Value> {
        let (sheet, row) = (self.sheet, self.row);
        let def = column(name)?;
        let instrument = sheet.instrument(row);
        match def.kind {
            ColumnKind::Template => {
                let position = sheet.parent(row).unwrap_or(row);
                match sheet.kind(position) {
                    RowKind::Package { template } => Some(Value::Utf8(template.token().into())),
                    RowKind::Line | RowKind::Underlying => None,
                }
            }
            ColumnKind::Qty => instrument.map(|_| Value::I64(sheet.qty(row))),
            ColumnKind::Strike => instrument.map(|i| {
                Value::F64(match i.strike() {
                    Strike::Absolute(v) | Strike::Percent(v) => v,
                })
            }),
            ColumnKind::Barrier => match instrument {
                Some(Instrument::Barrier(b)) => Some(Value::F64(b.level)),
                _ => None,
            },
            ColumnKind::SpotShift => instrument.and_then(|_| {
                let own = sheet.shift(row).spot_pct;
                own.or(sheet.sheet_shift().spot_pct).map(Value::F64)
            }),
            ColumnKind::VolShift => instrument.and_then(|_| {
                let own = sheet.shift(row).vol_pts;
                own.or(sheet.sheet_shift().vol_pts).map(Value::F64)
            }),
            // A failed line keeps its last result but shows none, as its
            // cells and the selection totals do; a local figure priced in a
            // currency the line no longer asks for is a gap, as its cell
            // paints it (`Sheet::shown_result`).
            ColumnKind::Measure { measure, usd } => {
                match (sheet.state(row), sheet.shown_result(row)) {
                    (LineState::Failed(_), _) | (_, None) => None,
                    (_, Some(_)) if sheet.is_package(row) => None,
                    (_, Some(r)) if !usd && r.currency.is_mixed() => None,
                    (_, Some(r)) => Some(Value::F64(r.get(measure, usd) * sheet.qty(row) as f64)),
                }
            }
            ColumnKind::Expiry => instrument.map(|i| {
                Value::Utf8(match i.expiry() {
                    Expiry::Date(d) => d.format("%Y-%m-%d").to_string(),
                    Expiry::Tenor(t) => t.clone(),
                })
            }),
            // A line without a currency paints `needs currency` whatever
            // its state (`cell_text`), so it must not scope as `fresh`.
            ColumnKind::Status
                if matches!(sheet.state(row), LineState::Fresh)
                    && !(sheet.is_line(row) && sheet.currency(row).is_none()) =>
            {
                Some(Value::Utf8("fresh".into()))
            }
            ColumnKind::SheetName
            | ColumnKind::PositionRef
            | ColumnKind::InstrumentRef
            | ColumnKind::UnderlyingRef
            | ColumnKind::OptionType
            | ColumnKind::Currency
            | ColumnKind::BarrierType
            | ColumnKind::PricedAt
            | ColumnKind::Status => {
                let cell = cell_text(sheet, row, def, &def.default_format, self.clock);
                (cell.state != CellState::Blank).then_some(Value::Utf8(cell.text))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::columns::COLUMNS;
    use crate::core::sheet::tests::{at, in_usd, push, result};
    use crate::core::shorthand::parse_builtin;
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::scope::{DimensionSelection, parse_expr};

    /// Rows: 0 SPX line, 1 CS, 2 and 3 its legs (4800 C, 5200 C), 4 NDX
    /// line, 5 CAL, 6 and 7 its legs (H27 C, Z26 C).
    fn sheet() -> Sheet {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            [
                "SPX Z26 4000 P",
                "SPX Z26 4800/5200 CS",
                "NDX Z26 5000 C",
                "SPX Z26/H27 5000 CAL",
            ]
            .iter()
            .map(|l| in_usd(parse_builtin(l).unwrap()))
            .collect(),
        );
        assert_eq!(s.len(), 8);
        s
    }

    fn expr(text: &str) -> Scope {
        Scope {
            expression: Some(parse_expr(text).unwrap()),
            ..Scope::default()
        }
    }

    fn apply(s: &Sheet, scope: &Scope) -> Result<Visibility, String> {
        apply_scope(s, scope, &DerivedDimensions::default(), Clock::utc())
    }

    fn shown(s: &Sheet, v: &Visibility) -> Vec<usize> {
        (0..s.len()).filter(|&r| v.is_shown(r)).collect()
    }

    fn price_all(s: &mut Sheet, price: f64) {
        let answers: Vec<_> = (0..s.len())
            .filter(|&r| s.is_line(r))
            .map(|r| (s.id(r), s.revision(r), Ok(result(price))))
            .collect();
        s.deliver_all(answers, at(0));
    }

    #[test]
    fn an_empty_scope_shows_everything_and_hides_nothing() {
        let s = sheet();
        let v = apply(&s, &Scope::default()).unwrap();
        assert_eq!(v, Visibility::all(&s));
        assert_eq!(v.hidden, 0);
    }

    #[test]
    fn an_expression_hides_non_matching_lines_and_keeps_a_package_with_any_match() {
        let s = sheet();
        let v = apply(&s, &expr("strike > 5000")).unwrap();
        assert_eq!(shown(&s, &v), vec![1, 3], "the CS with its 5200 leg");
        assert_eq!(v.shown_legs(&s, 1), vec![3]);
        assert!(v.is_partial(&s, 1));
        assert_eq!(v.hidden, 5, "SPX line, one CS leg, NDX line, both CAL legs");
    }

    #[test]
    fn a_package_with_no_matching_leg_hides() {
        let s = sheet();
        let v = apply(&s, &expr("underlying_ref = 'NDX'")).unwrap();
        assert_eq!(shown(&s, &v), vec![4]);
        assert_eq!(v.hidden, 5);
    }

    #[test]
    fn a_selection_on_a_column_pricer_lacks_is_dropped_not_refused() {
        let s = sheet();
        let scope = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["X".into()],
            }],
            ..Scope::default()
        };
        assert_eq!(apply(&s, &scope), Ok(Visibility::all(&s)));
    }

    #[test]
    fn an_expression_naming_a_non_pricer_column_refuses_and_hides_nothing() {
        let s = sheet();
        assert_eq!(
            apply(&s, &expr("book = 'X' and strike > 0")),
            Err("scope refused: 'book' is not a pricer column".to_string())
        );
    }

    #[test]
    fn an_evaluator_error_on_any_line_refuses_the_whole_scope() {
        // Text compared with a number casts the column per row: every
        // underlying fails to parse, so the SQL fails the whole query.
        let s = sheet();
        let r = apply(&s, &expr("underlying_ref = 5"));
        assert!(
            r.as_ref().is_err_and(|e| e.starts_with("scope refused: ")),
            "{r:?}"
        );
    }

    #[test]
    fn an_unpriced_line_never_matches_a_measure_and_a_blank_currency_is_null() {
        let s = sheet();
        let v = apply(&s, &expr("npv > 0")).unwrap();
        assert_eq!(shown(&s, &v), Vec::<usize>::new());
        let v = apply(&s, &expr("currency = 'USD'")).unwrap();
        assert!(v.is_shown(0), "an unpriced line has its own currency");
        let mut blank = Sheet::new("t");
        push(&mut blank, vec![parse_builtin("SPX Z26 4000 P").unwrap()]);
        let v = apply(&blank, &expr("not (currency = 'EUR')")).unwrap();
        assert!(!v.is_shown(0), "a blank currency is NULL");
        assert_eq!(v.hidden, 1);
        // Priced, the same lines match.
        let mut p = sheet();
        price_all(&mut p, 1.0);
        let v = apply(&p, &expr("npv > 0")).unwrap();
        assert!(v.is_shown(0), "a long line with a positive price");
    }

    #[test]
    fn a_measure_is_the_position_value_result_times_qty() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![in_usd(parse_builtin("-2 SPX Z26 4000 P").unwrap())],
        );
        price_all(&mut s, 3.0);
        assert_eq!(
            SheetRow {
                sheet: &s,
                row: 0,
                clock: Clock::utc()
            }
            .value("npv"),
            Some(Value::F64(-6.0))
        );
        let v = apply(&s, &expr("npv < 0")).unwrap();
        assert!(v.is_shown(0));
    }

    #[test]
    fn a_line_moved_off_its_priced_currency_has_no_local_measure() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![in_usd(parse_builtin("SPX Z26 4000 P").unwrap())],
        );
        price_all(&mut s, 3.0);
        s.apply(crate::core::edit::Edit::SetCurrency {
            row: 0,
            currency: Some(geode_core::pricing::Currency::parse("EUR").unwrap()),
        })
        .unwrap();
        let row = SheetRow {
            sheet: &s,
            row: 0,
            clock: Clock::utc(),
        };
        assert_eq!(row.value("npv"), None, "a USD figure is not an EUR one");
        assert_eq!(row.value("npv_usd"), Some(Value::F64(3.0 * 1.08)));
    }

    #[test]
    fn the_text_filter_searches_the_textual_columns() {
        let s = sheet();
        let scope = Scope {
            text: Some("ndx".into()),
            ..Scope::default()
        };
        let v = apply(&s, &scope).unwrap();
        assert_eq!(shown(&s, &v), vec![4]);
    }

    #[test]
    fn a_derived_dimension_over_underlying_ref_filters() {
        let s = sheet();
        let doc = merge_docs(
            "dimensions",
            &[LayerDoc::builtin(
                "dimensions",
                "[index]\nfrom = \"underlying_ref\"\n[index.values]\nbroad = [\"SPX\"]\ntech = [\"NDX\"]\n",
            )
            .unwrap()],
        );
        let (dims, diags) = DerivedDimensions::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        let v = apply_scope(&s, &expr("index = 'tech'"), &dims, Clock::utc()).unwrap();
        assert_eq!(shown(&s, &v), vec![4]);
        let sel = Scope {
            dimensions: vec![DimensionSelection {
                column: "index".into(),
                values: vec!["broad".into()],
            }],
            ..Scope::default()
        };
        let v = apply_scope(&s, &sel, &dims, Clock::utc()).unwrap();
        assert_eq!(shown(&s, &v), vec![0, 1, 2, 3, 5, 6, 7]);
    }

    #[test]
    fn a_leg_carries_its_packages_template() {
        let s = sheet();
        let v = apply(&s, &expr("template = 'CS'")).unwrap();
        assert_eq!(shown(&s, &v), vec![1, 2, 3]);
    }

    /// A fresh line's status reads `fresh` (its painted cell is blank):
    /// `status = 'fresh'` shows priced lines and hides the ones pricing.
    #[test]
    fn status_fresh_shows_priced_lines_and_hides_stale_ones() {
        let mut s = sheet();
        s.deliver_all(vec![(s.id(0), s.revision(0), Ok(result(1.0)))], at(0));
        let v = apply(&s, &expr("status = 'fresh'")).unwrap();
        assert_eq!(shown(&s, &v), vec![0], "only the priced SPX line");
        let v = apply(&s, &expr("status = 'pricing…'")).unwrap();
        assert!(!v.is_shown(0));
        assert!(v.is_shown(4), "the unpriced NDX line is pricing");
    }

    /// The status cell reads `needs currency` ahead of the state, so a
    /// blank line the sheet holds as `Fresh` scopes as it paints, never as
    /// `fresh`; a package (which has no currency of its own) still does.
    #[test]
    fn a_fresh_line_without_a_currency_scopes_as_needs_currency() {
        let mut s = sheet();
        push(&mut s, vec![parse_builtin("SPX Z26 4400 P").unwrap()]);
        let blank = s.len() - 1;
        price_all(&mut s, 1.0);
        assert_eq!(s.currency(blank), None);
        assert_eq!(s.state(blank), &LineState::Fresh);
        let v = apply(&s, &expr("status = 'fresh'")).unwrap();
        assert!(!v.is_shown(blank), "a blank line is not fresh");
        assert!(
            v.is_shown(0) && v.is_shown(1),
            "priced lines and packages are"
        );
        let v = apply(&s, &expr("status = 'needs currency'")).unwrap();
        assert_eq!(shown(&s, &v), vec![blank]);
    }

    /// `position_ref` and `instrument_ref` are the sheet's own `p<id>` /
    /// `i<id>`, never a desk reference: a selection on them is dropped
    /// (as a column the pricer lacks) and an expression naming them
    /// refuses, rather than hiding every line.
    #[test]
    fn the_synthetic_keys_are_not_scope_columns() {
        let s = sheet();
        for column in ["position_ref", "instrument_ref"] {
            let sel = Scope {
                dimensions: vec![DimensionSelection {
                    column: column.into(),
                    values: vec!["POS-1".into()],
                }],
                ..Scope::default()
            };
            assert_eq!(apply(&s, &sel), Ok(Visibility::all(&s)), "{column}");
            assert_eq!(
                apply(&s, &expr(&format!("{column} = 'p1'"))),
                Err(format!("scope refused: '{column}' is not a pricer column")),
            );
        }
    }

    /// A derived dimension over a synthetic key is not applicable either.
    #[test]
    fn a_derived_dimension_over_a_synthetic_key_is_not_applicable() {
        let s = sheet();
        let doc = merge_docs(
            "dimensions",
            &[LayerDoc::builtin(
                "dimensions",
                "[desk_pos]\nfrom = \"position_ref\"\n[desk_pos.values]\nmine = [\"p1\"]\n",
            )
            .unwrap()],
        );
        let (dims, diags) = DerivedDimensions::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        let sel = Scope {
            dimensions: vec![DimensionSelection {
                column: "desk_pos".into(),
                values: vec!["mine".into()],
            }],
            ..Scope::default()
        };
        assert_eq!(
            apply_scope(&s, &sel, &dims, Clock::utc()),
            Ok(Visibility::all(&s))
        );
        assert_eq!(
            apply_scope(&s, &expr("desk_pos = 'mine'"), &dims, Clock::utc()),
            Err("scope refused: 'desk_pos' is not a pricer column".to_string())
        );
    }

    /// A dated expiry's scope value is its ISO date, as the desk's data
    /// spells it (the cell still paints `Z26`); a tenor is its text.
    #[test]
    fn expiry_scopes_by_iso_date_or_tenor() {
        let mut s = sheet();
        push(
            &mut s,
            vec![in_usd(parse_builtin("SPX 3m 5000 C").unwrap())],
        );
        let v = apply(&s, &expr("expiry = '2026-12-18'")).unwrap();
        // Row 7 is the CAL's Z26 leg; its H27 leg (row 6) hides.
        assert_eq!(shown(&s, &v), vec![0, 1, 2, 3, 4, 5, 7], "every Z26 line");
        let v = apply(&s, &expr("expiry = '3m'")).unwrap();
        assert_eq!(shown(&s, &v), vec![8]);
        let v = apply(&s, &expr("expiry = 'Z26'")).unwrap();
        assert_eq!(v.hidden, 7, "the painted month code is not the value");
    }

    /// A row-independent refusal refuses with no line to evaluate.
    #[test]
    fn an_empty_sheet_still_refuses_a_bind_error() {
        let s = Sheet::new("t");
        let r = apply(&s, &expr("strike like '5%'"));
        assert!(
            r.as_ref().is_err_and(|e| e.starts_with("scope refused: ")),
            "{r:?}"
        );
    }

    #[test]
    fn sheet_row_values_match_the_painted_cells() {
        let mut s = sheet();
        // A barrier line and a percent strike, then a priced sheet with a
        // failed line and a stale one.
        push(
            &mut s,
            vec![
                in_usd(parse_builtin("SPX Z26 5000 C UO 5500").unwrap()),
                in_usd(parse_builtin("SPX Z26 100% P").unwrap()),
            ],
        );
        s.apply(crate::core::Edit::SetSheetShift(crate::core::OwnShifts {
            spot_pct: Some(2.0),
            vol_pts: None,
        }))
        .unwrap();
        // A line priced in USD then moved to EUR (its local figures are
        // gaps), and a priced line without a currency (`needs currency`).
        push(
            &mut s,
            vec![
                in_usd(parse_builtin("NDX Z26 4000 P").unwrap()),
                parse_builtin("NDX Z26 4400 P").unwrap(),
            ],
        );
        price_all(&mut s, 1.5);
        let moved = s.len() - 2;
        s.apply(crate::core::edit::Edit::SetCurrency {
            row: moved,
            currency: Some(geode_core::pricing::Currency::parse("EUR").unwrap()),
        })
        .unwrap();
        assert!(s.shown_result(moved).unwrap().currency.is_mixed());
        let failed = s.len() - 3;
        s.deliver_all(
            vec![(s.id(failed), s.revision(failed), Err("no vol".into()))],
            at(1),
        );
        let clock = Clock::utc();
        for row in (0..s.len()).filter(|&r| s.is_line(r)) {
            let values = SheetRow {
                sheet: &s,
                row,
                clock,
            };
            for def in COLUMNS.iter() {
                let cell = cell_text(&s, row, def, &def.default_format, clock);
                let got = values.value(def.name);
                let ctx = format!("row {row} column {}", def.name);
                match def.kind {
                    ColumnKind::Template => {} // a leg reads its package's; tested above
                    // A fresh status paints blank but scopes as `fresh`
                    // (`status_fresh_...`); stale and failed match below.
                    ColumnKind::Status
                        if matches!(s.state(row), LineState::Fresh)
                            && s.currency(row).is_some() =>
                    {
                        assert_eq!(got, Some(Value::Utf8("fresh".into())), "{ctx}")
                    }
                    // The cell paints `Z26`; the scope value is the ISO
                    // date desk data carries (`expiry_scopes_by_...`).
                    ColumnKind::Expiry => {}
                    ColumnKind::Qty => {
                        assert_eq!(got, Some(Value::I64(cell.text.parse().unwrap())), "{ctx}")
                    }
                    ColumnKind::Strike
                    | ColumnKind::Barrier
                    | ColumnKind::SpotShift
                    | ColumnKind::VolShift => {
                        let painted = cell.text.trim_end_matches('%');
                        let want =
                            (!painted.is_empty()).then(|| Value::F64(painted.parse().unwrap()));
                        assert_eq!(got, want, "{ctx}");
                    }
                    ColumnKind::Measure { measure, usd } => {
                        let want = match (s.state(row), s.shown_result(row)) {
                            (LineState::Failed(_), _) | (_, None) => None,
                            (_, Some(r)) if !usd && r.currency.is_mixed() => None,
                            (_, Some(r)) => {
                                Some(Value::F64(r.get(measure, usd) * s.qty(row) as f64))
                            }
                        };
                        assert_eq!(got, want, "{ctx}");
                        assert_eq!(
                            got.is_none(),
                            cell.text.is_empty() || cell.text == "—",
                            "{ctx}"
                        );
                    }
                    _ => {
                        let want = (cell.state != CellState::Blank)
                            .then(|| Value::Utf8(cell.text.clone()));
                        assert_eq!(got, want, "{ctx}");
                    }
                }
            }
        }
        assert_eq!(
            SheetRow {
                sheet: &s,
                row: failed,
                clock
            }
            .value("status"),
            Some(Value::Utf8("no vol".into()))
        );
        assert_eq!(
            SheetRow {
                sheet: &s,
                row: 0,
                clock
            }
            .value("expiry"),
            Some(Value::Utf8("2026-12-18".into()))
        );
        assert_eq!(
            SheetRow {
                sheet: &s,
                row: 0,
                clock
            }
            .value("book"),
            None
        );
    }
}
