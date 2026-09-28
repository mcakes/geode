//! Fixed column definitions and pure cell formatting. [`cell_text`] returns text plus a
//! semantic [`CellState`]; the table delegate caches that text and resolves state to
//! theme colours. Formatting takes the configured clock explicitly and needs no GPUI
//! context.

use crate::core::sheet::{LineState, RowKind, Sheet};
use crate::core::shorthand::{render_barrier_kind, render_expiry, render_strike};
use geode_core::format::{Sign, format_number};
use geode_core::pricing::{Instrument, Measure, OptionKind};
use geode_core::view::{Colour, ColumnFormat, Negative, Scale};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColumnKind {
    /// The sheet's name, on every row.
    SheetName,
    /// The row's position: a package or bare line is its own, a leg is
    /// its package's.
    PositionRef,
    /// A line's instrument identity; a package has none.
    InstrumentRef,
    /// A package's template token; blank on a line.
    Template,
    Qty,
    UnderlyingRef,
    Expiry,
    Strike,
    OptionType,
    /// The result's currency, blank until priced.
    Currency,
    Barrier,
    BarrierType,
    SpotShift,
    VolShift,
    /// One of the 28 result columns: a measure, local or its `_usd` twin.
    Measure {
        measure: Measure,
        usd: bool,
    },
    PricedAt,
    Status,
}

/// Which rows a column can display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applies {
    EveryLine,
    BarrierLines,
    EveryRow,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDef {
    pub name: &'static str,
    /// Default display label when the view supplies none, including units where needed.
    /// Labels remain separate from configuration names and are checked against default
    /// widths at the largest font size.
    pub label: &'static str,
    pub kind: ColumnKind,
    pub editable: bool,
    pub applies_to: Applies,
    pub default_format: ColumnFormat,
    /// Default width in logical pixels, matching view presentation widths. The delegate
    /// applies it directly rather than scaling it with rem.
    pub default_width: f32,
}

/// Text columns: no grouping, no sign colour.
const TEXT: ColumnFormat = ColumnFormat::TEXT;
/// Default presentation for strike and barrier columns. Their cell text uses
/// shorthand's round-trip spelling, without grouping or fixed decimal padding.
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
/// An npv: the measure default (two places, grouped, sign-coloured).
const NPV: ColumnFormat = ColumnFormat::MEASURE;
/// Greek format with four decimals, grouping, and sign colouring. Default widths are
/// checked against representative large values such as -1,234,567.8900.
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

/// A measure column's default header: words carrying the bump, under the
/// pricer's label rule (no config spelling; fits 16 characters in 128 px
/// at the largest font size). The config name stays `Measure::name`.
const fn label(m: Measure) -> &'static str {
    match m {
        Measure::Npv => "npv",
        Measure::Delta01 => "delta 1%",
        Measure::Delta02 => "delta 2%",
        Measure::Delta05 => "delta 5%",
        Measure::Gamma01 => "gamma 1%",
        Measure::Gamma02 => "gamma 2%",
        Measure::Gamma05 => "gamma 5%",
        Measure::Vega01 => "vega 1pt",
        Measure::NormalizedVega01 => "nvega 1pt",
        Measure::Skew01 => "skew 1pt",
        Measure::Rho010 => "rho 10bp",
        Measure::RhoRfr010 => "rho rfr 10bp",
        Measure::RhoOis010 => "rho ois 10bp",
        Measure::CleanThetaBusinessDay => "theta bd",
    }
}

/// The `_usd` twin's header: the local label and ` usd`. Spelled out
/// because a `const fn` cannot concatenate; a test pins the rule.
const fn usd_label(m: Measure) -> &'static str {
    match m {
        Measure::Npv => "npv usd",
        Measure::Delta01 => "delta 1% usd",
        Measure::Delta02 => "delta 2% usd",
        Measure::Delta05 => "delta 5% usd",
        Measure::Gamma01 => "gamma 1% usd",
        Measure::Gamma02 => "gamma 2% usd",
        Measure::Gamma05 => "gamma 5% usd",
        Measure::Vega01 => "vega 1pt usd",
        Measure::NormalizedVega01 => "nvega 1pt usd",
        Measure::Skew01 => "skew 1pt usd",
        Measure::Rho010 => "rho 10bp usd",
        Measure::RhoRfr010 => "rho rfr 10bp usd",
        Measure::RhoOis010 => "rho ois 10bp usd",
        Measure::CleanThetaBusinessDay => "theta bd usd",
    }
}

use Applies::{BarrierLines, EveryLine, EveryRow};

/// The fixed column vocabulary. A static allocation lets `column()` return references
/// with a `'static` lifetime even though formats can contain owned named-colour
/// strings.
pub static COLUMNS: [ColumnDef; 44] = [
    // The identity columns take risk_snapshot's spellings, so a scope or a
    // grouping written against the blotter reads the same on a sheet.
    def(
        "sheet",
        "sheet",
        ColumnKind::SheetName,
        false,
        EveryRow,
        TEXT,
        88.0,
    ),
    def(
        "position_ref",
        "position",
        ColumnKind::PositionRef,
        false,
        EveryRow,
        TEXT,
        72.0,
    ),
    def(
        "instrument_ref",
        "instrument",
        ColumnKind::InstrumentRef,
        false,
        EveryRow,
        TEXT,
        88.0,
    ),
    def(
        "template",
        "template",
        ColumnKind::Template,
        false,
        EveryRow,
        TEXT,
        72.0,
    ),
    def("qty", "qty", ColumnKind::Qty, true, EveryLine, TEXT, 56.0),
    def(
        "underlying_ref",
        "underlying",
        ColumnKind::UnderlyingRef,
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
        88.0,
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
        "option_type",
        "type",
        ColumnKind::OptionType,
        true,
        EveryLine,
        TEXT,
        48.0,
    ),
    def(
        "currency",
        "currency",
        ColumnKind::Currency,
        false,
        EveryRow,
        TEXT,
        72.0,
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
    // The 28 measure columns, two per measure in `Measure::ALL` order: the
    // local one, then its `_usd` twin. A macro cannot expand to two array
    // elements, so the pairs are written out; a test pins the order.
    def(
        Measure::Npv.name(),
        label(Measure::Npv),
        ColumnKind::Measure {
            measure: Measure::Npv,
            usd: false,
        },
        false,
        EveryRow,
        NPV,
        112.0,
    ),
    def(
        Measure::Npv.usd_name(),
        usd_label(Measure::Npv),
        ColumnKind::Measure {
            measure: Measure::Npv,
            usd: true,
        },
        false,
        EveryRow,
        NPV,
        112.0,
    ),
    def(
        Measure::Delta01.name(),
        label(Measure::Delta01),
        ColumnKind::Measure {
            measure: Measure::Delta01,
            usd: false,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::Delta01.usd_name(),
        usd_label(Measure::Delta01),
        ColumnKind::Measure {
            measure: Measure::Delta01,
            usd: true,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::Delta02.name(),
        label(Measure::Delta02),
        ColumnKind::Measure {
            measure: Measure::Delta02,
            usd: false,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::Delta02.usd_name(),
        usd_label(Measure::Delta02),
        ColumnKind::Measure {
            measure: Measure::Delta02,
            usd: true,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::Delta05.name(),
        label(Measure::Delta05),
        ColumnKind::Measure {
            measure: Measure::Delta05,
            usd: false,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::Delta05.usd_name(),
        usd_label(Measure::Delta05),
        ColumnKind::Measure {
            measure: Measure::Delta05,
            usd: true,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::Gamma01.name(),
        label(Measure::Gamma01),
        ColumnKind::Measure {
            measure: Measure::Gamma01,
            usd: false,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::Gamma01.usd_name(),
        usd_label(Measure::Gamma01),
        ColumnKind::Measure {
            measure: Measure::Gamma01,
            usd: true,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::Gamma02.name(),
        label(Measure::Gamma02),
        ColumnKind::Measure {
            measure: Measure::Gamma02,
            usd: false,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::Gamma02.usd_name(),
        usd_label(Measure::Gamma02),
        ColumnKind::Measure {
            measure: Measure::Gamma02,
            usd: true,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::Gamma05.name(),
        label(Measure::Gamma05),
        ColumnKind::Measure {
            measure: Measure::Gamma05,
            usd: false,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::Gamma05.usd_name(),
        usd_label(Measure::Gamma05),
        ColumnKind::Measure {
            measure: Measure::Gamma05,
            usd: true,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::Vega01.name(),
        label(Measure::Vega01),
        ColumnKind::Measure {
            measure: Measure::Vega01,
            usd: false,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::Vega01.usd_name(),
        usd_label(Measure::Vega01),
        ColumnKind::Measure {
            measure: Measure::Vega01,
            usd: true,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::NormalizedVega01.name(),
        label(Measure::NormalizedVega01),
        ColumnKind::Measure {
            measure: Measure::NormalizedVega01,
            usd: false,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::NormalizedVega01.usd_name(),
        usd_label(Measure::NormalizedVega01),
        ColumnKind::Measure {
            measure: Measure::NormalizedVega01,
            usd: true,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::Skew01.name(),
        label(Measure::Skew01),
        ColumnKind::Measure {
            measure: Measure::Skew01,
            usd: false,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::Skew01.usd_name(),
        usd_label(Measure::Skew01),
        ColumnKind::Measure {
            measure: Measure::Skew01,
            usd: true,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::Rho010.name(),
        label(Measure::Rho010),
        ColumnKind::Measure {
            measure: Measure::Rho010,
            usd: false,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::Rho010.usd_name(),
        usd_label(Measure::Rho010),
        ColumnKind::Measure {
            measure: Measure::Rho010,
            usd: true,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::RhoRfr010.name(),
        label(Measure::RhoRfr010),
        ColumnKind::Measure {
            measure: Measure::RhoRfr010,
            usd: false,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::RhoRfr010.usd_name(),
        usd_label(Measure::RhoRfr010),
        ColumnKind::Measure {
            measure: Measure::RhoRfr010,
            usd: true,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::RhoOis010.name(),
        label(Measure::RhoOis010),
        ColumnKind::Measure {
            measure: Measure::RhoOis010,
            usd: false,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::RhoOis010.usd_name(),
        usd_label(Measure::RhoOis010),
        ColumnKind::Measure {
            measure: Measure::RhoOis010,
            usd: true,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::CleanThetaBusinessDay.name(),
        label(Measure::CleanThetaBusinessDay),
        ColumnKind::Measure {
            measure: Measure::CleanThetaBusinessDay,
            usd: false,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
    def(
        Measure::CleanThetaBusinessDay.usd_name(),
        usd_label(Measure::CleanThetaBusinessDay),
        ColumnKind::Measure {
            measure: Measure::CleanThetaBusinessDay,
            usd: true,
        },
        false,
        EveryRow,
        GREEK,
        128.0,
    ),
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

/// The 28 measure columns in `COLUMNS` order (local, then usd, per measure).
pub fn measure_columns() -> impl Iterator<Item = &'static ColumnDef> {
    COLUMNS
        .iter()
        .filter(|c| matches!(c.kind, ColumnKind::Measure { .. }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellState {
    /// No value here: the column does not apply, or nothing has arrived.
    Blank,
    /// A value of the row's own.
    Own,
    /// A shift inherited from the sheet (paints muted).
    Inherited,
    /// A result the row is repricing (paints muted).
    Stale,
    /// A failed row's result cells and status (paints danger text).
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellText {
    pub text: String,
    pub state: CellState,
    /// The sign of the number a measure cell was formatted from, as the
    /// text shows it (a value that rounds to zero is `Zero`); `None` on
    /// every other cell. What a `sign` or `tint_sign` column colour
    /// paints by.
    pub sign: Option<Sign>,
}

fn blank() -> CellText {
    CellText {
        text: String::new(),
        state: CellState::Blank,
        sign: None,
    }
}

fn own(text: impl Into<String>) -> CellText {
    CellText {
        text: text.into(),
        state: CellState::Own,
        sign: None,
    }
}

/// Format a shift with an explicit ASCII sign, such as `+2.0` or `-1.5`.
/// Cells and header chips share this function; precision, grouping, and scale
/// come from the supplied format.
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
    measure: Measure,
    usd: bool,
    format: &ColumnFormat,
) -> CellText {
    match sheet.state(row) {
        LineState::Failed(_) => CellText {
            text: "—".into(),
            state: CellState::Failed,
            sign: None,
        },
        state => match sheet.result(row) {
            None => blank(),
            // A package whose legs priced in unlike currencies has no
            // local figure: the folded sum would read as a real one. The
            // `_usd` twin is converted per leg and still sums. Painted as
            // a stale cell is (muted `—`): no state names "no such
            // figure", and muted is the reading a gap needs.
            Some(r) if !usd && r.currency.is_mixed() => CellText {
                text: "—".into(),
                state: CellState::Stale,
                sign: None,
            },
            Some(r) => {
                let formatted = format_number(r.get(measure, usd), format);
                CellText {
                    text: formatted.text,
                    state: if *state == LineState::Stale {
                        CellState::Stale
                    } else {
                        CellState::Own
                    },
                    sign: Some(formatted.sign),
                }
            }
        },
    }
}

/// Format one cell and classify its presentation state. The tile supplies its
/// configured `AppClock` value; this pure function does not read globals. Columns that
/// do not apply to the row return blank text.
pub fn cell_text(
    sheet: &Sheet,
    row: usize,
    def: &ColumnDef,
    format: &ColumnFormat,
    clock: geode_core::clock::Clock,
) -> CellText {
    // A package row aggregates its legs' values (package-row spec).
    if sheet.is_package(row) && crate::core::package::aggregates(def.kind) {
        return crate::core::package::aggregate(sheet, row, def.kind, format);
    }
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
        ColumnKind::SheetName => own(sheet.name.as_str()),
        // A leg's position is its package's; a root row is its own.
        ColumnKind::PositionRef => {
            own(format!("p{}", sheet.id(sheet.parent(row).unwrap_or(row)).0))
        }
        ColumnKind::InstrumentRef => {
            if sheet.is_package(row) {
                blank()
            } else {
                own(format!("i{}", sheet.id(row).0))
            }
        }
        ColumnKind::Template => match sheet.kind(row) {
            RowKind::Package { template } => own(template.token()),
            RowKind::Line | RowKind::Underlying => blank(),
        },
        ColumnKind::Currency => match sheet.result(row) {
            Some(r) => own(r.currency.as_str()),
            None => blank(),
        },
        ColumnKind::Qty => own(sheet.qty(row).to_string()),
        ColumnKind::UnderlyingRef => own(instrument.expect("applies").underlying()),
        ColumnKind::Expiry => own(render_expiry(instrument.expect("applies").expiry())),
        ColumnKind::Strike => own(render_strike(instrument.expect("applies").strike())),
        ColumnKind::OptionType => own(match instrument.expect("applies").kind() {
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
        ColumnKind::Measure { measure, usd } => number(sheet, row, measure, usd, format),
        ColumnKind::PricedAt => match sheet.priced_at(row) {
            // Use the configured clock for the recorded pricing attempt time.
            Some(t) => own(clock.hms(t)),
            None => blank(),
        },
        ColumnKind::Status => match sheet.state(row) {
            LineState::Fresh => own(""),
            LineState::Stale => CellText {
                text: "pricing…".into(),
                state: CellState::Stale,
                sign: None,
            },
            LineState::Failed(m) => CellText {
                text: m.clone(),
                state: CellState::Failed,
                sign: None,
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
            sign: None,
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
                "sheet",
                "position_ref",
                "instrument_ref",
                "template",
                "qty",
                "underlying_ref",
                "expiry",
                "strike",
                "option_type",
                "currency",
                "barrier",
                "barrier_type",
                "spot_shift",
                "vol_shift",
                "npv",
                "npv_usd",
                "delta01",
                "delta01_usd",
                "delta02",
                "delta02_usd",
                "delta05",
                "delta05_usd",
                "gamma01",
                "gamma01_usd",
                "gamma02",
                "gamma02_usd",
                "gamma05",
                "gamma05_usd",
                "vega01",
                "vega01_usd",
                "normalized_vega01",
                "normalized_vega01_usd",
                "skew01",
                "skew01_usd",
                "rho010",
                "rho010_usd",
                "rho_rfr010",
                "rho_rfr010_usd",
                "rho_ois010",
                "rho_ois010_usd",
                "clean_theta_business_day",
                "clean_theta_business_day_usd",
                "priced_at",
                "status",
            ]
        );
        for c in &COLUMNS {
            assert_eq!(column(c.name), Some(c), "{}", c.name);
            assert!(c.default_width > 0.0, "{}", c.name);
        }
        assert_eq!(column("price"), None, "the analytic names are gone");
        let editable: Vec<&str> = COLUMNS
            .iter()
            .filter(|c| c.editable)
            .map(|c| c.name)
            .collect();
        assert_eq!(
            editable,
            vec![
                "qty",
                "underlying_ref",
                "expiry",
                "strike",
                "option_type",
                "barrier",
                "barrier_type",
                "spot_shift",
                "vol_shift"
            ]
        );
        // The identity and currency columns read the row, so a package
        // paints them too; their labels stay words that fit the width.
        for (name, label) in [
            ("sheet", "sheet"),
            ("position_ref", "position"),
            ("instrument_ref", "instrument"),
            ("template", "template"),
            ("currency", "currency"),
        ] {
            let c = column(name).unwrap();
            assert_eq!(c.label, label);
            assert!(!c.editable, "{name}");
            assert_eq!(c.applies_to, Applies::EveryRow, "{name}");
            assert_eq!(c.default_format, TEXT, "{name}");
        }
        assert_eq!(column("underlying_ref").unwrap().label, "underlying");
        assert_eq!(column("option_type").unwrap().label, "type");
        assert_eq!(column("barrier").unwrap().applies_to, Applies::BarrierLines);
        assert_eq!(
            column("barrier_type").unwrap().applies_to,
            Applies::BarrierLines
        );
        assert_eq!(column("npv").unwrap().applies_to, Applies::EveryRow);
        assert_eq!(column("status").unwrap().applies_to, Applies::EveryRow);
        assert_eq!(column("qty").unwrap().applies_to, Applies::EveryLine);
        assert_eq!(column("spot_shift").unwrap().applies_to, Applies::EveryLine);
    }

    #[test]
    fn every_measure_has_two_read_only_columns_in_measure_order() {
        let names: Vec<&str> = measure_columns().map(|c| c.name).collect();
        let expected: Vec<&str> = Measure::ALL
            .iter()
            .flat_map(|m| [m.name(), m.usd_name()])
            .collect();
        assert_eq!(names, expected);
        assert!(measure_columns().all(|c| !c.editable && c.applies_to == Applies::EveryRow));
    }

    #[test]
    fn measure_labels_are_words_and_the_usd_twin_appends_usd() {
        for m in Measure::ALL {
            let (local, usd) = (column(m.name()).unwrap(), column(m.usd_name()).unwrap());
            assert!(!local.label.contains('_'), "{}", local.label);
            assert_eq!(usd.label, format!("{} usd", local.label));
            assert!(
                usd.label.chars().count() <= 16,
                "{}: over 128 px",
                usd.label
            );
        }
        assert_eq!(column("delta01").unwrap().label, "delta 1%");
        assert_eq!(column("rho_ois010_usd").unwrap().label, "rho ois 10bp usd");
    }

    #[test]
    fn identity_columns_paint_the_row_ids_and_the_sheet() {
        // One package (CS: two legs) then one bare line, built by a shared
        // helper: rows 0 (package), 1 and 2 (legs), 3 (the bare line).
        let s = crate::core::sheet::tests::sheet_with_package_and_line();
        let (pkg, leg, bare) = (0, 1, 3);
        let text = |row, name| cell(&s, row, name).text;
        assert_eq!(text(pkg, "sheet"), "book");
        assert_eq!(text(pkg, "position_ref"), format!("p{}", s.id(pkg).0));
        assert_eq!(
            text(pkg, "instrument_ref"),
            "",
            "a package is no instrument"
        );
        assert_eq!(
            text(leg, "position_ref"),
            format!("p{}", s.id(pkg).0),
            "a leg belongs to its package's position"
        );
        assert_eq!(text(leg, "instrument_ref"), format!("i{}", s.id(leg).0));
        assert_eq!(
            text(bare, "position_ref"),
            format!("p{}", s.id(bare).0),
            "a bare line is its own position"
        );
        assert_eq!(text(bare, "instrument_ref"), format!("i{}", s.id(bare).0));
        assert_eq!(text(pkg, "template"), "CS");
        assert_eq!(text(bare, "template"), "");
        assert_eq!(text(bare, "currency"), "", "blank until priced");
        assert_eq!(cell(&s, bare, "currency").state, CellState::Blank);
    }

    #[test]
    fn a_priced_line_paints_its_currency() {
        let mut s = crate::core::sheet::tests::sheet_with_package_and_line();
        let bare = 3;
        s.deliver_all(vec![(s.id(bare), 1, Ok(result(1.0)))], at(0));
        assert_eq!(
            cell(&s, bare, "currency"),
            CellText {
                text: "USD".into(),
                state: CellState::Own,
                sign: None
            }
        );
    }

    #[test]
    fn instrument_cells_render_the_grammar_and_a_package_aggregates_them() {
        let mut s = Sheet::new("t");
        let barrier =
            crate::core::shorthand::parse_builtin("-3 SPX 20DEC26 5000 P DO 4200").unwrap();
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
                state: CellState::Own,
                sign: None
            }
        );
        assert_eq!(cell(&s, 0, "underlying_ref").text, "SPX");
        assert_eq!(cell(&s, 0, "expiry").text, "Z26");
        assert_eq!(cell(&s, 0, "strike").text, "4250.5");
        assert_eq!(cell(&s, 0, "option_type").text, "C");
        assert_eq!(
            cell(&s, 0, "barrier"),
            CellText {
                text: String::new(),
                state: CellState::Blank,
                sign: None
            },
            "not a barrier line"
        );
        assert_eq!(cell(&s, 0, "barrier_type").state, CellState::Blank);
        assert_eq!(cell(&s, 1, "qty").text, "-3");
        assert_eq!(cell(&s, 1, "expiry").text, "20DEC26");
        assert_eq!(cell(&s, 1, "option_type").text, "P");
        assert_eq!(
            cell(&s, 1, "barrier"),
            CellText {
                text: "4200".into(),
                state: CellState::Own,
                sign: None
            }
        );
        assert_eq!(
            cell(&s, 1, "barrier_type"),
            CellText {
                text: "DO".into(),
                state: CellState::Own,
                sign: None
            }
        );
        // The package row aggregates its legs.
        for (name, text) in [
            ("qty", "1"),
            ("underlying_ref", "SPX"),
            ("expiry", "Z26"),
            ("strike", "4800/5200"),
            ("option_type", "C"),
        ] {
            assert_eq!(
                cell(&s, 2, name),
                CellText {
                    text: text.into(),
                    state: CellState::Own,
                    sign: None
                },
                "{name}"
            );
        }
        for name in ["barrier", "barrier_type", "spot_shift", "vol_shift"] {
            assert_eq!(
                cell(&s, 2, name),
                CellText {
                    text: String::new(),
                    state: CellState::Blank,
                    sign: None
                },
                "{name}: no barrier leg, no shift set"
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
                state: CellState::Blank,
                sign: None
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
                state: CellState::Inherited,
                sign: None
            }
        );
        assert_eq!(
            cell(&s, 0, "vol_shift"),
            CellText {
                text: "-1.5".into(),
                state: CellState::Inherited,
                sign: None
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
                state: CellState::Own,
                sign: None
            }
        );
        assert_eq!(
            cell(&s, 1, "vol_shift"),
            CellText {
                text: "-1.5".into(),
                state: CellState::Inherited,
                sign: None
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
                state: CellState::Own,
                sign: None
            }
        );
    }

    #[test]
    fn a_mixed_currency_package_paints_a_gap_in_local_measures_and_sums_usd() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![callspread(-5)]); // legs: -5 × 4800 call, +5 × 5200 call
        let mut eur = result(40.0);
        eur.currency = geode_core::pricing::Currency::parse("EUR").unwrap();
        s.deliver(s.id(1), 1, Ok(result(100.0)), at(0));
        s.deliver(s.id(2), 1, Ok(eur), at(0));
        assert!(s.result(0).unwrap().currency.is_mixed());
        assert_eq!(
            cell(&s, 0, "npv"),
            CellText {
                text: "—".into(),
                state: CellState::Stale,
                sign: None
            },
            "a local sum over USD and EUR is a gap"
        );
        assert_eq!(cell(&s, 0, "delta01").text, "—");
        // -5 × 108 + 5 × 43.2 = -324
        assert_eq!(
            cell(&s, 0, "npv_usd"),
            CellText {
                text: "-324.00".into(),
                state: CellState::Own,
                sign: Some(Sign::Negative)
            },
            "the usd twin still sums"
        );
        assert_eq!(cell(&s, 0, "currency").text, "USD/EUR");
        // Each leg is a bare line in its own currency.
        assert_eq!(cell(&s, 1, "npv").text, "100.00");
        assert_eq!(cell(&s, 2, "npv").text, "40.00");
        assert_eq!(cell(&s, 2, "currency").text, "EUR");
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
            cell(&s, 0, "npv"),
            CellText {
                text: String::new(),
                state: CellState::Blank,
                sign: None
            }
        );
        assert_eq!(
            cell(&s, 0, "status"),
            CellText {
                text: "pricing…".into(),
                state: CellState::Stale,
                sign: None
            }
        );
        assert_eq!(cell(&s, 0, "priced_at").state, CellState::Blank);
        s.deliver(s.id(0), 1, Ok(result(1234.5678)), at(0));
        assert_eq!(
            cell(&s, 0, "npv"),
            CellText {
                text: "1,234.57".into(),
                state: CellState::Own,
                sign: Some(Sign::Positive)
            }
        );
        assert_eq!(
            cell(&s, 0, "delta01"),
            CellText {
                text: "123.4568".into(),
                state: CellState::Own,
                sign: Some(Sign::Positive)
            }
        );
        assert_eq!(
            cell(&s, 0, "status"),
            CellText {
                text: String::new(),
                state: CellState::Own,
                sign: None
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
            cell(&s, 0, "npv"),
            CellText {
                text: "1,234.57".into(),
                state: CellState::Stale,
                sign: Some(Sign::Positive)
            }
        );
        assert_eq!(cell(&s, 0, "status").state, CellState::Stale);
        // Failed: a dash, and the message.
        s.deliver(s.id(0), 2, Err("refused by the mock".into()), at(1));
        assert_eq!(
            cell(&s, 0, "npv"),
            CellText {
                text: "—".into(),
                state: CellState::Failed,
                sign: None
            }
        );
        assert_eq!(
            cell(&s, 0, "rho010"),
            CellText {
                text: "—".into(),
                state: CellState::Failed,
                sign: None
            }
        );
        assert_eq!(
            cell(&s, 0, "status"),
            CellText {
                text: "refused by the mock".into(),
                state: CellState::Failed,
                sign: None
            }
        );
        // A package paints its sums like a line.
        s.deliver(s.id(2), 1, Ok(result(100.0)), at(2));
        s.deliver(s.id(3), 1, Ok(result(40.0)), at(3));
        assert_eq!(
            cell(&s, 1, "npv"),
            CellText {
                text: "-120.00".into(),
                state: CellState::Own,
                sign: Some(Sign::Negative)
            }
        );
        assert_eq!(cell(&s, 1, "status").text, "");
        // A custom format from a view applies.
        let def = column("npv").unwrap();
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
