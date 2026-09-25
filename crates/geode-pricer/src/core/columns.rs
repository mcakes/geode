//! The fixed column vocabulary (line-pricer spec §6.5) and the pure
//! text of one cell — the half of §8.2's grid model that needs no theme,
//! so it is tested here without one (planning decision 10). Part 3 wraps
//! each `CellText` in a `SharedString` and picks the paint from its
//! `CellState`.

use crate::core::sheet::{LineState, Sheet};
use crate::core::shorthand::{render_barrier_kind, render_expiry, render_strike};
use geode_core::format::format_number;
use geode_core::pricing::{Instrument, OptionKind};
use geode_core::view::{Colour, ColumnFormat, Negative, Scale};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColumnKind {
    Qty,
    Underlying,
    Expiry,
    Strike,
    Type,
    Barrier,
    BarrierType,
    SpotShift,
    VolShift,
    Price,
    Delta,
    Gamma,
    Vega,
    Theta,
    Rho,
    PricedAt,
    Status,
}

/// Which rows a column has a value for (spec §6.5's "Applies to").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applies {
    EveryLine,
    BarrierLines,
    EveryRow,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDef {
    pub name: &'static str,
    /// The header a view shows when it sets no `label`: readable words
    /// carrying the unit where the name alone hides it (`spot %`, `vol
    /// pt`, matching the header chips `spot +2.0%` / `vol -1.0`), never
    /// the snake_case config name. Each fits `default_width` in the mono
    /// face at the largest font size
    /// (`every_default_label_and_worst_case_value_fits_its_width`).
    pub label: &'static str,
    pub kind: ColumnKind,
    pub editable: bool,
    pub applies_to: Applies,
    pub default_format: ColumnFormat,
    /// Pixels — the `view_presentation.toml` contract the blotter's
    /// widths follow; not on the rem scale (the known gap CLAUDE.md
    /// records for `TableDelegate::column`).
    pub default_width: f32,
}

/// Text columns: no grouping, no sign colour.
const TEXT: ColumnFormat = ColumnFormat::TEXT;
/// A strike or barrier level: two places, no thousands separator (a
/// strike reads as `5000`, not `5,000`).
const LEVEL: ColumnFormat = ColumnFormat {
    precision: 2,
    thousands: false,
    negative: Negative::Minus,
    colour: Colour::None,
    scale: Scale::None,
};
/// A shift: one place, signed by `cell_text` itself. The header's shift
/// chips format through the same constant and [`signed`], so a chip and
/// a cell spell one value one way.
pub(crate) const SHIFT: ColumnFormat = ColumnFormat {
    precision: 1,
    thousands: false,
    negative: Negative::Minus,
    colour: Colour::None,
    scale: Scale::None,
};
/// A price: the measure default (two places, grouped, sign-coloured).
const PRICE: ColumnFormat = ColumnFormat::MEASURE;
/// A greek: four places, grouped, sign-coloured. Its columns are wide
/// enough for `-123,456.7890` (see the fit test): a right-aligned cell
/// that overflows loses its LEADING digits, which reads as a plausible
/// wrong number rather than a clipped one.
const GREEK: ColumnFormat = ColumnFormat {
    precision: 4,
    thousands: true,
    negative: Negative::Minus,
    colour: Colour::Sign,
    scale: Scale::None,
};

const fn def(
    name: &'static str,
    label: &'static str,
    kind: ColumnKind,
    editable: bool,
    applies_to: Applies,
    default_format: ColumnFormat,
    default_width: f32,
) -> ColumnDef {
    ColumnDef {
        name,
        label,
        kind,
        editable,
        applies_to,
        default_format,
        default_width,
    }
}

use Applies::{BarrierLines, EveryLine, EveryRow};

/// Spec §6.5's table, in its order. A `static`, not a `const`:
/// `column()` answers `&'static ColumnDef` into it, and a `const` whose
/// type carries a `String` (`Colour::Named`) is not promotable to a
/// `'static` borrow.
pub static COLUMNS: [ColumnDef; 17] = [
    def("qty", "qty", ColumnKind::Qty, true, EveryLine, TEXT, 48.0),
    def(
        "underlying",
        "underlying",
        ColumnKind::Underlying,
        true,
        EveryLine,
        TEXT,
        88.0,
    ),
    def(
        "expiry",
        "expiry",
        ColumnKind::Expiry,
        true,
        EveryLine,
        TEXT,
        72.0,
    ),
    def(
        "strike",
        "strike",
        ColumnKind::Strike,
        true,
        EveryLine,
        LEVEL,
        88.0,
    ),
    def(
        "type",
        "type",
        ColumnKind::Type,
        true,
        EveryLine,
        TEXT,
        48.0,
    ),
    def(
        "barrier",
        "barrier",
        ColumnKind::Barrier,
        true,
        BarrierLines,
        LEVEL,
        80.0,
    ),
    def(
        "barrier_type",
        "barrier type",
        ColumnKind::BarrierType,
        true,
        BarrierLines,
        TEXT,
        104.0,
    ),
    def(
        "spot_shift",
        "spot %",
        ColumnKind::SpotShift,
        true,
        EveryLine,
        SHIFT,
        72.0,
    ),
    def(
        "vol_shift",
        "vol pt",
        ColumnKind::VolShift,
        true,
        EveryLine,
        SHIFT,
        72.0,
    ),
    def(
        "price",
        "price",
        ColumnKind::Price,
        false,
        EveryRow,
        PRICE,
        104.0,
    ),
    def(
        "delta",
        "delta",
        ColumnKind::Delta,
        false,
        EveryRow,
        GREEK,
        112.0,
    ),
    def(
        "gamma",
        "gamma",
        ColumnKind::Gamma,
        false,
        EveryRow,
        GREEK,
        112.0,
    ),
    def(
        "vega",
        "vega",
        ColumnKind::Vega,
        false,
        EveryRow,
        GREEK,
        112.0,
    ),
    def(
        "theta",
        "theta",
        ColumnKind::Theta,
        false,
        EveryRow,
        GREEK,
        112.0,
    ),
    def("rho", "rho", ColumnKind::Rho, false, EveryRow, GREEK, 112.0),
    def(
        "priced_at",
        "priced at",
        ColumnKind::PricedAt,
        false,
        EveryRow,
        TEXT,
        80.0,
    ),
    def(
        "status",
        "status",
        ColumnKind::Status,
        false,
        EveryRow,
        TEXT,
        160.0,
    ),
];

pub fn column(name: &str) -> Option<&'static ColumnDef> {
    COLUMNS.iter().find(|c| c.name == name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellState {
    /// No value here: the column does not apply, or nothing has arrived.
    Blank,
    /// A value of the row's own.
    Own,
    /// A shift inherited from the sheet (paints muted, spec ruling 8).
    Inherited,
    /// A result the row is repricing (paints muted, spec §8.2).
    Stale,
    /// A failed row's result cells and status (paints danger text).
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellText {
    pub text: String,
    pub state: CellState,
}

fn blank() -> CellText {
    CellText {
        text: String::new(),
        state: CellState::Blank,
    }
}

fn own(text: impl Into<String>) -> CellText {
    CellText {
        text: text.into(),
        state: CellState::Own,
    }
}

/// `+2.0` / `-1.5` at the format's precision: a shift is a delta from
/// the market, so its sign is the information. The minus is the ASCII
/// hyphen-minus every other number in the table carries
/// (`format_number`'s `Negative::Minus`), so one sheet spells a negative
/// one way — cells, header chips and the yanked text alike.
pub(crate) fn signed(value: f64, format: &ColumnFormat) -> String {
    let n = format_number(value.abs(), format).text;
    if value < 0.0 {
        format!("-{n}")
    } else {
        format!("+{n}")
    }
}

fn number(
    sheet: &Sheet,
    row: usize,
    pick: fn(&geode_core::pricing::PriceResult) -> f64,
    format: &ColumnFormat,
) -> CellText {
    match sheet.state(row) {
        LineState::Failed(_) => CellText {
            text: "—".into(),
            state: CellState::Failed,
        },
        state => match sheet.result(row) {
            None => blank(),
            Some(r) => CellText {
                text: format_number(pick(r), format).text,
                state: if *state == LineState::Stale {
                    CellState::Stale
                } else {
                    CellState::Own
                },
            },
        },
    }
}

/// The text and state of one cell (spec §6.5, §8.2). `clock` is the
/// trader's configured clock (`[time] zone`, as-of dialog Part 2) — Part
/// 3's tile reads it off the `AppClock` global and hands it in here,
/// since this crate is pure core with no gpui and no global of its own.
pub fn cell_text(
    sheet: &Sheet,
    row: usize,
    def: &ColumnDef,
    format: &ColumnFormat,
    clock: geode_core::clock::Clock,
) -> CellText {
    let instrument: Option<&Instrument> = sheet.instrument(row);
    let applies = match def.applies_to {
        Applies::EveryRow => true,
        Applies::EveryLine => instrument.is_some(),
        Applies::BarrierLines => matches!(instrument, Some(Instrument::Barrier(_))),
    };
    if !applies {
        return blank();
    }
    match def.kind {
        ColumnKind::Qty => own(sheet.qty(row).to_string()),
        ColumnKind::Underlying => own(instrument.expect("applies").underlying()),
        ColumnKind::Expiry => own(render_expiry(instrument.expect("applies").expiry())),
        ColumnKind::Strike => own(render_strike(instrument.expect("applies").strike())),
        ColumnKind::Type => own(match instrument.expect("applies").kind() {
            OptionKind::Call => "C",
            OptionKind::Put => "P",
        }),
        ColumnKind::Barrier => match instrument {
            Some(Instrument::Barrier(b)) => own(render_strike(
                geode_core::pricing::Strike::Absolute(b.level),
            )),
            _ => blank(),
        },
        ColumnKind::BarrierType => match instrument {
            Some(Instrument::Barrier(b)) => own(render_barrier_kind(b.barrier)),
            _ => blank(),
        },
        ColumnKind::SpotShift => shift_cell(
            sheet.shift(row).spot_pct,
            sheet.sheet_shift().spot_pct,
            format,
        ),
        ColumnKind::VolShift => shift_cell(
            sheet.shift(row).vol_pts,
            sheet.sheet_shift().vol_pts,
            format,
        ),
        ColumnKind::Price => number(sheet, row, |r| r.price, format),
        ColumnKind::Delta => number(sheet, row, |r| r.delta, format),
        ColumnKind::Gamma => number(sheet, row, |r| r.gamma, format),
        ColumnKind::Vega => number(sheet, row, |r| r.vega, format),
        ColumnKind::Theta => number(sheet, row, |r| r.theta, format),
        ColumnKind::Rho => number(sheet, row, |r| r.rho, format),
        ColumnKind::PricedAt => match sheet.priced_at(row) {
            // The trader's configured clock, like every displayed time
            // (Phase 4a ruling, as-of dialog Part 2).
            Some(t) => own(clock.hms(t)),
            None => blank(),
        },
        ColumnKind::Status => match sheet.state(row) {
            LineState::Fresh => own(""),
            LineState::Stale => CellText {
                text: "pricing…".into(),
                state: CellState::Stale,
            },
            LineState::Failed(m) => CellText {
                text: m.clone(),
                state: CellState::Failed,
            },
        },
    }
}

fn shift_cell(own_value: Option<f64>, sheet_value: Option<f64>, format: &ColumnFormat) -> CellText {
    match (own_value, sheet_value) {
        (Some(v), _) => own(signed(v, format)),
        (None, Some(v)) => CellText {
            text: signed(v, format),
            state: CellState::Inherited,
        },
        (None, None) => blank(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::edit::Edit;
    use crate::core::sheet::tests::{at, callspread, line, push, result, spx};
    use crate::core::sheet::{OwnShifts, Sheet};
    use geode_core::pricing::OptionKind;

    fn cell(sheet: &Sheet, row: usize, name: &str) -> CellText {
        let def = column(name).unwrap_or_else(|| panic!("no column {name}"));
        cell_text(
            sheet,
            row,
            def,
            &def.default_format,
            geode_core::clock::Clock::utc(),
        )
    }

    #[test]
    fn the_vocabulary_is_the_specs_table_in_order() {
        let names: Vec<&str> = COLUMNS.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            vec![
                "qty",
                "underlying",
                "expiry",
                "strike",
                "type",
                "barrier",
                "barrier_type",
                "spot_shift",
                "vol_shift",
                "price",
                "delta",
                "gamma",
                "vega",
                "theta",
                "rho",
                "priced_at",
                "status",
            ]
        );
        for c in &COLUMNS {
            assert_eq!(column(c.name), Some(c), "{}", c.name);
            assert!(c.default_width > 0.0, "{}", c.name);
        }
        assert_eq!(column("npv"), None);
        let editable: Vec<&str> = COLUMNS
            .iter()
            .filter(|c| c.editable)
            .map(|c| c.name)
            .collect();
        assert_eq!(
            editable,
            vec![
                "qty",
                "underlying",
                "expiry",
                "strike",
                "type",
                "barrier",
                "barrier_type",
                "spot_shift",
                "vol_shift"
            ]
        );
        assert_eq!(column("barrier").unwrap().applies_to, Applies::BarrierLines);
        assert_eq!(
            column("barrier_type").unwrap().applies_to,
            Applies::BarrierLines
        );
        assert_eq!(column("price").unwrap().applies_to, Applies::EveryRow);
        assert_eq!(column("status").unwrap().applies_to, Applies::EveryRow);
        assert_eq!(column("qty").unwrap().applies_to, Applies::EveryLine);
        assert_eq!(column("spot_shift").unwrap().applies_to, Applies::EveryLine);
    }

    #[test]
    fn instrument_cells_render_the_grammar_and_a_package_paints_them_blank() {
        let mut s = Sheet::new("t");
        let barrier = crate::core::shorthand::parse("-3 SPX 20DEC26 5000 P DO 4200").unwrap();
        push(
            &mut s,
            vec![
                line(spx(4250.5, OptionKind::Call), 2),
                barrier,
                callspread(1),
            ],
        );
        assert_eq!(
            cell(&s, 0, "qty"),
            CellText {
                text: "2".into(),
                state: CellState::Own
            }
        );
        assert_eq!(cell(&s, 0, "underlying").text, "SPX");
        assert_eq!(cell(&s, 0, "expiry").text, "Z26");
        assert_eq!(cell(&s, 0, "strike").text, "4250.5");
        assert_eq!(cell(&s, 0, "type").text, "C");
        assert_eq!(
            cell(&s, 0, "barrier"),
            CellText {
                text: String::new(),
                state: CellState::Blank
            },
            "not a barrier line"
        );
        assert_eq!(cell(&s, 0, "barrier_type").state, CellState::Blank);
        assert_eq!(cell(&s, 1, "qty").text, "-3");
        assert_eq!(cell(&s, 1, "expiry").text, "20DEC26");
        assert_eq!(cell(&s, 1, "type").text, "P");
        assert_eq!(
            cell(&s, 1, "barrier"),
            CellText {
                text: "4200".into(),
                state: CellState::Own
            }
        );
        assert_eq!(
            cell(&s, 1, "barrier_type"),
            CellText {
                text: "DO".into(),
                state: CellState::Own
            }
        );
        // The package row.
        for name in [
            "qty",
            "underlying",
            "expiry",
            "strike",
            "type",
            "barrier",
            "barrier_type",
            "spot_shift",
            "vol_shift",
        ] {
            assert_eq!(
                cell(&s, 2, name),
                CellText {
                    text: String::new(),
                    state: CellState::Blank
                },
                "{name}"
            );
        }
        // Its legs are lines.
        assert_eq!(cell(&s, 3, "strike").text, "4800");
        assert_eq!(cell(&s, 4, "qty").text, "-1");
    }

    #[test]
    fn a_shift_cell_is_own_inherited_or_blank() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![
                line(spx(5000.0, OptionKind::Call), 1),
                line(spx(5100.0, OptionKind::Call), 1),
            ],
        );
        assert_eq!(
            cell(&s, 0, "spot_shift"),
            CellText {
                text: String::new(),
                state: CellState::Blank
            }
        );
        s.apply(Edit::SetSheetShift(OwnShifts {
            spot_pct: Some(2.0),
            vol_pts: Some(-1.5),
        }))
        .unwrap();
        assert_eq!(
            cell(&s, 0, "spot_shift"),
            CellText {
                text: "+2.0".into(),
                state: CellState::Inherited
            }
        );
        assert_eq!(
            cell(&s, 0, "vol_shift"),
            CellText {
                text: "-1.5".into(),
                state: CellState::Inherited
            }
        );
        s.apply(Edit::SetShift {
            row: 1,
            shift: OwnShifts {
                spot_pct: Some(-5.0),
                vol_pts: None,
            },
        })
        .unwrap();
        assert_eq!(
            cell(&s, 1, "spot_shift"),
            CellText {
                text: "-5.0".into(),
                state: CellState::Own
            }
        );
        assert_eq!(
            cell(&s, 1, "vol_shift"),
            CellText {
                text: "-1.5".into(),
                state: CellState::Inherited
            }
        );
        // Zero is still a value, own or inherited.
        s.apply(Edit::SetShift {
            row: 1,
            shift: OwnShifts {
                spot_pct: Some(0.0),
                vol_pts: None,
            },
        })
        .unwrap();
        assert_eq!(
            cell(&s, 1, "spot_shift"),
            CellText {
                text: "+0.0".into(),
                state: CellState::Own
            }
        );
    }

    #[test]
    fn result_cells_follow_the_rows_state() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![line(spx(5000.0, OptionKind::Call), 1), callspread(-2)],
        );
        // Unpriced: blank, and status says pricing.
        assert_eq!(
            cell(&s, 0, "price"),
            CellText {
                text: String::new(),
                state: CellState::Blank
            }
        );
        assert_eq!(
            cell(&s, 0, "status"),
            CellText {
                text: "pricing…".into(),
                state: CellState::Stale
            }
        );
        assert_eq!(cell(&s, 0, "priced_at").state, CellState::Blank);
        s.deliver(s.id(0), 1, Ok(result(1234.5678)), at(0));
        assert_eq!(
            cell(&s, 0, "price"),
            CellText {
                text: "1,234.57".into(),
                state: CellState::Own
            }
        );
        assert_eq!(
            cell(&s, 0, "delta"),
            CellText {
                text: "123.4568".into(),
                state: CellState::Own
            }
        );
        assert_eq!(
            cell(&s, 0, "status"),
            CellText {
                text: String::new(),
                state: CellState::Own
            }
        );
        assert_eq!(cell(&s, 0, "priced_at").state, CellState::Own);
        assert_eq!(cell(&s, 0, "priced_at").text.len(), 8, "HH:MM:SS");
        // Stale after an edit: the old number, muted.
        s.apply(Edit::SetInstrument {
            row: 0,
            instrument: spx(5050.0, OptionKind::Call),
        })
        .unwrap();
        assert_eq!(
            cell(&s, 0, "price"),
            CellText {
                text: "1,234.57".into(),
                state: CellState::Stale
            }
        );
        assert_eq!(cell(&s, 0, "status").state, CellState::Stale);
        // Failed: a dash, and the message.
        s.deliver(s.id(0), 2, Err("refused by the mock".into()), at(1));
        assert_eq!(
            cell(&s, 0, "price"),
            CellText {
                text: "—".into(),
                state: CellState::Failed
            }
        );
        assert_eq!(
            cell(&s, 0, "rho"),
            CellText {
                text: "—".into(),
                state: CellState::Failed
            }
        );
        assert_eq!(
            cell(&s, 0, "status"),
            CellText {
                text: "refused by the mock".into(),
                state: CellState::Failed
            }
        );
        // A package paints its sums like a line.
        s.deliver(s.id(2), 1, Ok(result(100.0)), at(2));
        s.deliver(s.id(3), 1, Ok(result(40.0)), at(3));
        assert_eq!(
            cell(&s, 1, "price"),
            CellText {
                text: "-120.00".into(),
                state: CellState::Own
            }
        );
        assert_eq!(cell(&s, 1, "status").text, "");
        // A custom format from a view applies.
        let def = column("price").unwrap();
        let precise = geode_core::view::ColumnFormat {
            precision: 4,
            ..def.default_format.clone()
        };
        assert_eq!(
            cell_text(&s, 1, def, &precise, geode_core::clock::Clock::utc()).text,
            "-120.0000"
        );
    }
}
