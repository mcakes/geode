//! Cells as the table paints them, prepared outside render: the formatted
//! text and its sign, the heat fill from the column's domain, the text
//! color made readable over that fill, and whether the row is the diff
//! base. Σ cells and the base row's diffed cells take no fill (they sit
//! outside the heat domain). Pure: colors are `Rgb`; the delegate converts.

use crate::core::pivot::{Column, Pivot};
use geode_core::colour::scale::Diverging;
use geode_core::colour::{Rgb, readable_text_on};
use geode_core::format::{Sign, format_number};
use geode_core::view::{Colour, ColumnFormat, Scale};
use std::ops::Range;
use std::sync::Arc;

/// The theme's five colors the cells are prepared from, as `Rgb` so the
/// prepare path stays gpui-free. Rebuilt by the tile only when the theme
/// signature changes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub bullish: Rgb,
    pub bearish: Rgb,
    pub background: Rgb,
    pub foreground: Rgb,
    pub muted: Rgb,
}

/// The table over a pivot: a label column, the pivot's columns, a Σ
/// column when the pivot has row sums; the pivot's rows, a Σ row when it
/// has column sums.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub data_rows: usize,
    pub data_cols: usize,
    pub sigma_row: bool,
    pub sigma_col: bool,
}

impl Layout {
    pub fn of(pivot: &Pivot) -> Layout {
        Layout {
            data_rows: pivot.rows.len(),
            data_cols: pivot.columns.len(),
            sigma_row: pivot.column_sums.is_some(),
            sigma_col: pivot.row_sums.is_some(),
        }
    }
    pub fn rows(&self) -> usize {
        self.data_rows + usize::from(self.sigma_row)
    }
    /// Table columns, the label column included.
    pub fn cols(&self) -> usize {
        1 + self.data_cols + usize::from(self.sigma_col)
    }
    pub fn is_sigma_row(&self, row: usize) -> bool {
        self.sigma_row && row == self.data_rows
    }
    pub fn is_sigma_col(&self, col: usize) -> bool {
        self.sigma_col && col == 1 + self.data_cols
    }
    /// The pivot column a table column paints; `None` for the label and Σ columns.
    pub fn data_col(&self, col: usize) -> Option<usize> {
        (1..=self.data_cols).contains(&col).then(|| col - 1)
    }
}

pub struct Inputs<'a> {
    pub pivot: &'a Pivot,
    pub palette: &'a Palette,
    /// The tile's `:scale`; `None` keeps each column's own.
    pub scale: Option<Scale>,
    /// Heat paint on (`p`).
    pub heat: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CellColor {
    /// Already readable over the cell's fill.
    Fixed(Rgb),
    /// A desk-named color the painter resolves against the live theme,
    /// then passes through `Diverging::text_over` when a fill is under it.
    Named { name: Arc<str>, sign: Option<Sign> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedCell {
    pub text: Arc<str>,
    pub sign: Option<Sign>,
    pub fill: Option<Rgb>,
    pub color: CellColor,
    /// The diff base row (highlighted by the painter).
    pub base: bool,
}

/// The cell at table `(row, col)`; `None` is blank. Labels and Σ cells
/// take no fill; a value cell fills from its column's heat domain unless
/// it is the base row's diffed cell.
pub fn cell(i: &Inputs<'_>, row: usize, col: usize) -> Option<PreparedCell> {
    let p = i.pivot;
    let layout = Layout::of(p);
    let pal = i.palette;
    let base = p.base_row == Some(row);
    if col == 0 {
        return Some(if layout.is_sigma_row(row) {
            plain("Σ", pal.muted, false)
        } else {
            plain(&p.rows.get(row)?.text, pal.foreground, base)
        });
    }
    let format_of = |column: &Column| {
        let mut f = column.format.clone();
        if let Some(scale) = i.scale {
            f.scale = scale;
        }
        f
    };
    if layout.is_sigma_col(col) {
        let format = format_of(p.columns.first()?);
        let value = if layout.is_sigma_row(row) {
            p.grand_sum.flatten()?
        } else {
            p.row_sums.as_ref()?.get(row).copied().flatten()?
        };
        return Some(number(value, &format, None, pal, false));
    }
    let data_col = layout.data_col(col)?;
    let column = p.columns.get(data_col)?;
    let format = format_of(column);
    if layout.is_sigma_row(row) {
        let value = p.column_sums.as_ref()?.get(data_col).copied().flatten()?;
        return Some(number(value, &format, None, pal, false));
    }
    let value = p.cell(row, data_col)?;
    let fill = (i.heat && !(base && column.diffed))
        .then(|| {
            Diverging {
                domain: p.heat_domain(data_col),
                positive: pal.bullish,
                negative: pal.bearish,
                background: pal.background,
            }
            .fill(value)
        })
        .flatten();
    Some(number(value, &format, fill, pal, base))
}

fn plain(text: &str, color: Rgb, base: bool) -> PreparedCell {
    PreparedCell {
        text: Arc::from(text),
        sign: None,
        fill: None,
        color: CellColor::Fixed(color),
        base,
    }
}

fn number(
    value: f64,
    format: &ColumnFormat,
    fill: Option<Rgb>,
    pal: &Palette,
    base: bool,
) -> PreparedCell {
    let f = format_number(value, format);
    let color = match &format.colour {
        Colour::Named(name) => CellColor::Named {
            name: Arc::from(name.as_str()),
            sign: Some(f.sign),
        },
        Colour::Sign => {
            let c = match f.sign {
                Sign::Positive => pal.bullish,
                Sign::Negative => pal.bearish,
                Sign::Zero => pal.foreground,
            };
            CellColor::Fixed(fill.map_or(c, |fill| readable_text_on(c, fill)))
        }
        Colour::None => CellColor::Fixed(fill.map_or(pal.foreground, |fill| {
            readable_text_on(pal.foreground, fill)
        })),
    };
    PreparedCell {
        text: Arc::from(f.text.as_str()),
        sign: Some(f.sign),
        fill,
        color,
        base,
    }
}

/// Tab-separated rows for the clipboard over data coordinates: a header
/// line (`rows_axis`, then each head), then one line per row with its
/// label and raw values (`{v}`; NULL blank). Σ cells are never included.
pub fn tsv(pivot: &Pivot, rows_axis: &str, rows: Range<usize>, cols: Range<usize>) -> String {
    use crate::core::readout::head_text;
    use std::fmt::Write as _;
    let mut s = String::new();
    s.push_str(rows_axis);
    for c in cols.clone() {
        if let Some(column) = pivot.columns.get(c) {
            let _ = write!(s, "\t{}", head_text(&column.head));
        }
    }
    s.push('\n');
    for r in rows {
        let Some(row) = pivot.rows.get(r) else { break };
        s.push_str(&row.text);
        for c in cols.clone() {
            s.push('\t');
            if let Some(v) = pivot.cell(r, c) {
                let _ = write!(s, "{v}");
            }
        }
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::pivot::fixtures::*;
    use crate::core::pivot::{AxisDomains, Currency, Mode};
    use geode_core::scenario::{Columns, MeasureColumn};
    use geode_core::view::ColumnPresentation;

    fn palette() -> Palette {
        let g = |v: f32| Rgb { r: v, g: v, b: v };
        Palette {
            bullish: Rgb {
                r: 0.1,
                g: 0.8,
                b: 0.3,
            },
            bearish: Rgb {
                r: 0.9,
                g: 0.2,
                b: 0.2,
            },
            background: g(0.1),
            foreground: g(0.95),
            muted: g(0.6),
        }
    }
    fn matrix() -> Pivot {
        Pivot::build(
            &vega_main().snapshot("vega_buckets", Some(1)),
            Some(&vega_totals().snapshot("vega_buckets", Some(1))),
            &vega_kt(),
            Mode {
                currency: Currency::Native,
                diff: false,
            },
            &AxisDomains::default(),
        )
        .unwrap()
    }
    fn ladder(diff: bool) -> Pivot {
        Pivot::build(
            &ladder_main().snapshot("spot_ladder", Some(1)),
            None,
            &spot_ladder(),
            Mode {
                currency: Currency::Native,
                diff,
            },
            &AxisDomains::default(),
        )
        .unwrap()
    }
    fn inputs<'a>(pivot: &'a Pivot, palette: &'a Palette, heat: bool) -> Inputs<'a> {
        Inputs {
            pivot,
            palette,
            scale: None,
            heat,
        }
    }

    #[test]
    fn the_layout_adds_a_label_column_and_the_sigma_edges_the_pivot_has() {
        let m = Layout::of(&matrix());
        assert_eq!(
            (m.data_rows, m.data_cols, m.sigma_row, m.sigma_col),
            (3, 2, true, true)
        );
        assert_eq!((m.rows(), m.cols()), (4, 4));
        assert!(m.is_sigma_row(3) && m.is_sigma_col(3));
        assert_eq!(m.data_col(0), None, "the label column");
        assert_eq!(m.data_col(1), Some(0));
        assert_eq!(m.data_col(3), None, "the Σ column");
        let l = Layout::of(&ladder(false));
        assert_eq!(
            (l.sigma_row, l.sigma_col),
            (false, false),
            "a scenario rows axis, measure columns"
        );
        assert_eq!((l.rows(), l.cols()), (3, 4));
    }

    #[test]
    fn a_value_cell_is_formatted_sign_colored_and_filled_by_its_columns_domain() {
        let p = matrix();
        let pal = palette();
        let i = inputs(&p, &pal, true);
        // 1Y × 90 = 10 in a matrix whose domain is 30: a bullish fill.
        let c = cell(&i, 0, 1).expect("a value");
        assert_eq!(&*c.text, "10.00");
        assert_eq!(c.sign, Some(Sign::Positive));
        let fill = c.fill.expect("heat on");
        let expected = Diverging {
            domain: 30.0,
            positive: pal.bullish,
            negative: pal.bearish,
            background: pal.background,
        }
        .fill(10.0)
        .unwrap();
        assert_eq!(fill, expected);
        assert_eq!(
            c.color,
            CellColor::Fixed(readable_text_on(pal.bullish, fill))
        );
        assert!(!c.base);
        // Plain paint: no fill, the sign color as is.
        let plain = cell(&inputs(&p, &pal, false), 0, 1).unwrap();
        assert_eq!(plain.fill, None);
        assert_eq!(plain.color, CellColor::Fixed(pal.bullish));
        // The empty cell (2Y × 100) is blank.
        assert_eq!(cell(&i, 2, 2), None);
    }

    #[test]
    fn labels_and_sigma_cells_take_no_fill() {
        let p = matrix();
        let pal = palette();
        let i = inputs(&p, &pal, true);
        let label = cell(&i, 0, 0).unwrap();
        assert_eq!(&*label.text, "1Y");
        assert_eq!(label.color, CellColor::Fixed(pal.foreground));
        let sigma_label = cell(&i, 3, 0).unwrap();
        assert_eq!(&*sigma_label.text, "Σ");
        assert_eq!(sigma_label.color, CellColor::Fixed(pal.muted));
        // Row Σ of 1Y is the subtotal row's 31, not 10 + 20.
        let row_sum = cell(&i, 0, 3).unwrap();
        assert_eq!(&*row_sum.text, "31.00");
        assert_eq!(row_sum.fill, None);
        // Column Σ under 90 from the companion (40, not 10 + 30); the corner
        // is the main query's total row (66).
        assert_eq!(&*cell(&i, 3, 1).unwrap().text, "40.00");
        assert_eq!(&*cell(&i, 3, 3).unwrap().text, "66.00");
        assert_eq!(cell(&i, 3, 3).unwrap().fill, None);
    }

    #[test]
    fn a_ladder_fills_each_column_by_its_own_domain_and_the_base_row_unfilled() {
        let p = ladder(true);
        let pal = palette();
        let i = inputs(&p, &pal, true);
        // The base row (0.0) carries absolutes, highlighted, no fill under npv.
        let base = cell(&i, 1, 1).unwrap();
        assert!(base.base);
        assert_eq!(&*base.text, "100.00");
        assert_eq!(base.fill, None, "outside the heat domain");
        // Its delta cell is not diffed: it fills by delta's own domain (3).
        let base_delta = cell(&i, 1, 2).unwrap();
        assert!(base_delta.fill.is_some());
        // -5.0's npv is -10 against a domain of 15; theta -2 against 2 (the cap).
        let npv = cell(&i, 0, 1).unwrap();
        assert_eq!(&*npv.text, "-10.00");
        assert_eq!(npv.sign, Some(Sign::Negative));
        let theta = cell(&i, 0, 3).unwrap();
        let cap = Diverging {
            domain: 2.0,
            positive: pal.bullish,
            negative: pal.bearish,
            background: pal.background,
        }
        .fill(-2.0)
        .unwrap();
        assert_eq!(theta.fill, Some(cap));
    }

    #[test]
    fn a_sigma_cell_under_a_diffed_column_stays_blank() {
        // A bucket-axis ladder with a diff: Σ row present, npv diffed.
        let mut shape = spot_ladder();
        shape.axes[0].kind = geode_core::schema::axis::AxisKind::Bucket;
        let main = crate::core::pivot::fixtures::Rows {
            rows: ladder_main()
                .rows
                .into_iter()
                .map(|(l, d, v)| {
                    if d == 0 {
                        (
                            l,
                            d,
                            vec![Some(305.0), Some(335.5), Some(6.0), Some(6.6), Some(-6.0)],
                        )
                    } else {
                        (l, d, v)
                    }
                })
                .collect(),
            ..ladder_main()
        };
        let p = Pivot::build(
            &main.snapshot("spot_ladder", Some(1)),
            None,
            &shape,
            Mode {
                currency: Currency::Native,
                diff: true,
            },
            &AxisDomains::default(),
        )
        .unwrap();
        let pal = palette();
        let i = inputs(&p, &pal, true);
        let layout = Layout::of(&p);
        assert!(layout.sigma_row);
        assert_eq!(cell(&i, 3, 1), None, "no Σ under deltas");
        assert_eq!(
            &*cell(&i, 3, 2).unwrap().text,
            "6.00",
            "delta's Σ is the total row"
        );
        let absolute = Pivot::build(
            &main.snapshot("spot_ladder", Some(1)),
            None,
            &shape,
            Mode {
                currency: Currency::Native,
                diff: false,
            },
            &AxisDomains::default(),
        )
        .unwrap();
        assert_eq!(
            &*cell(&inputs(&absolute, &pal, true), 3, 1).unwrap().text,
            "305.00"
        );
    }

    #[test]
    fn the_tile_scale_overrides_then_restores_each_columns_own() {
        let mut shape = spot_ladder();
        if let Columns::Measures(cols) = &mut shape.columns {
            cols[1] = MeasureColumn {
                format: ColumnFormat::MEASURE.with(&ColumnPresentation {
                    scale: Some(Scale::Thousands),
                    precision: Some(0),
                    ..ColumnPresentation::default()
                }),
                ..cols[1].clone()
            };
        }
        let main = crate::core::pivot::fixtures::Rows {
            rows: ladder_main()
                .rows
                .into_iter()
                .map(|(l, d, v)| {
                    (
                        l,
                        d,
                        v.into_iter().map(|x| x.map(|x| x * 1_000_000.0)).collect(),
                    )
                })
                .collect(),
            ..ladder_main()
        };
        let p = Pivot::build(
            &main.snapshot("spot_ladder", Some(1)),
            None,
            &shape,
            Mode {
                currency: Currency::Native,
                diff: false,
            },
            &AxisDomains::default(),
        )
        .unwrap();
        let pal = palette();
        let own = Inputs {
            pivot: &p,
            palette: &pal,
            scale: None,
            heat: false,
        };
        assert_eq!(
            &*cell(&own, 0, 1).unwrap().text,
            "90,000,000.00",
            "npv: its own (none)"
        );
        assert_eq!(
            &*cell(&own, 0, 2).unwrap().text,
            "1,000",
            "delta: its own k"
        );
        let millions = Inputs {
            scale: Some(Scale::Millions),
            ..own
        };
        assert_eq!(&*cell(&millions, 0, 1).unwrap().text, "90.00");
        assert_eq!(
            &*cell(&millions, 0, 2).unwrap().text,
            "1",
            "the tile scale overrides delta's own k"
        );
        let back = Inputs { scale: None, ..own };
        assert_eq!(
            &*cell(&back, 0, 2).unwrap().text,
            "1,000",
            "restored, not flattened to none"
        );
    }

    #[test]
    fn a_named_color_is_left_for_the_painter_with_its_sign() {
        let mut shape = vega_kt();
        shape.value = shape.value.map(|v| MeasureColumn {
            format: ColumnFormat {
                colour: Colour::Named("ocean".into()),
                ..ColumnFormat::MEASURE
            },
            ..v
        });
        let p = Pivot::build(
            &vega_main().snapshot("vega_buckets", Some(1)),
            None,
            &shape,
            Mode {
                currency: Currency::Native,
                diff: false,
            },
            &AxisDomains::default(),
        )
        .unwrap();
        let pal = palette();
        let c = cell(&inputs(&p, &pal, true), 0, 1).unwrap();
        assert_eq!(
            c.color,
            CellColor::Named {
                name: "ocean".into(),
                sign: Some(Sign::Positive)
            }
        );
    }

    #[test]
    fn the_tsv_carries_heads_labels_and_raw_values_and_no_sigma() {
        let p = matrix();
        let out = tsv(&p, "tenor", 0..3, 0..2);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines,
            ["tenor\t90.0\t100.0", "1Y\t10\t20", "18M\t\t5", "2Y\t30\t"],
            "{out}"
        );
        assert!(out.ends_with('\n'));
        assert!(!out.contains("31") && !out.contains("66"), "no Σ");
        let block = tsv(&p, "tenor", 1..2, 1..2);
        assert_eq!(block, "tenor\t100.0\n18M\t5\n");
    }
}
