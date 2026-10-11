//! The pivot: a grouped snapshot read into the cells, Σ, diff and heat
//! domain a scenario tile paints. Leaf rows (every grouping column
//! present) fill the cells; Σ are read from the snapshot's subtotal and
//! total rows and from the second, column-totals snapshot, never summed
//! from the cells, so attribution and NULL rules hold. The axis domain is
//! the caller's (the dataset's, not the scope's) unioned with whatever the
//! snapshot holds, so a scoped row is never dropped and the matrix holds
//! still as the cursor moves. Pure: no entity, no window.

use geode_core::scenario::{AxisInfo, Columns, DiffBase, MeasureColumn, ShapeSpec};
use geode_core::schema::axis::sort_axis_text;
use geode_core::snapshot::Snapshot;
use geode_core::view::ColumnFormat;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

/// One axis value as the snapshot spelled it, parsed when the axis is
/// numeric (the compiler selects grouping columns as text).
#[derive(Debug, Clone, PartialEq)]
pub struct AxisLabel {
    pub text: String,
    pub number: Option<f64>,
}

impl AxisLabel {
    fn parse(text: &str, axis: &AxisInfo) -> AxisLabel {
        let number = if axis.is_numeric() {
            text.trim().parse::<f64>().ok()
        } else {
            None
        };
        AxisLabel {
            text: text.to_string(),
            number,
        }
    }

    /// Whether this is the diff's base row: by value for a numeric base
    /// (`"0.0"` and `"0"` both match `0`), by exact text otherwise.
    pub fn matches(&self, base: &DiffBase) -> bool {
        match base {
            DiffBase::Number(b) => self.number == Some(*b),
            DiffBase::Text(t) => self.text == *t,
        }
    }
}

/// The dataset-wide labels of each axis, as the tile learns them from
/// distinct-value queries. Empty means unknown: the pivot then shows what
/// the snapshot holds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AxisDomains {
    labels: BTreeMap<String, Vec<String>>,
}

impl AxisDomains {
    pub fn insert(&mut self, axis: impl Into<String>, labels: Vec<String>) {
        self.labels.insert(axis.into(), labels);
    }

    pub fn get(&self, axis: &str) -> &[String] {
        self.labels.get(axis).map_or(&[], Vec::as_slice)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Currency {
    Usd,
    Native,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub currency: Currency,
    /// Paint the shape's diff measures against the base row.
    pub diff: bool,
}

/// What a column's header says.
#[derive(Debug, Clone, PartialEq)]
pub enum Head {
    /// An axis value; `outer` is the first level of a two-level header.
    Axis {
        outer: Option<AxisLabel>,
        label: AxisLabel,
    },
    /// A ladder's measure.
    Measure { label: String, unit: Option<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub head: Head,
    /// The snapshot column the cells read: the measure, or its USD twin.
    pub measure: String,
    pub format: ColumnFormat,
    /// The cells (other than the base row) are value minus the base row's.
    pub diffed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pivot {
    /// The rows axis's labels, in axis order.
    pub rows: Vec<AxisLabel>,
    pub columns: Vec<Column>,
    /// Row-major, `rows.len() * columns.len()`; `None` is blank.
    cells: Vec<Option<f64>>,
    /// Σ across the columns, one per row (the subtotal rows); `None` when
    /// a columns axis is a scenario axis or the shape is a ladder.
    pub row_sums: Option<Vec<Option<f64>>>,
    /// Σ across the rows, one per column (the second query's leaves, or a
    /// ladder's total row). `None` means one of three things: the Σ row is
    /// not meaningful for the shape (`sums_rows` false, a scenario rows
    /// axis), the companion snapshot has not arrived, or the pair's
    /// generations disagree (`totals_disagree`). A diffed column's entry
    /// is `None`: the total row is absolute and never sits under
    /// value-minus-base cells.
    pub column_sums: Option<Vec<Option<f64>>>,
    /// The corner: the total row's value, present only with both Σ.
    pub grand_sum: Option<Option<f64>>,
    /// The two snapshots carry different generations: `column_sums` is
    /// withheld until a delivery agrees.
    pub totals_disagree: bool,
    /// The diff's base row, when diff is on and a scoped leaf carries the
    /// base label (a label only the axis domain supplies does not count).
    pub base_row: Option<usize>,
    /// Diff is on but no scoped leaf carries the base, or a diff measure's
    /// base value is NULL: those columns show absolutes.
    pub diff_base_missing: bool,
    /// The scoped rows span more than one `currency` (the ungrouped
    /// dimension's mixed flag on the total row).
    pub currency_mixed: bool,
    /// The largest |cell| painted: the heat scale's domain. Σ cells do not
    /// count, nor the base row's diff-measure cells in diff mode.
    pub heat_domain: f64,
}

impl Pivot {
    pub fn cell(&self, row: usize, col: usize) -> Option<f64> {
        if row >= self.rows.len() || col >= self.columns.len() {
            return None;
        }
        self.cells[row * self.columns.len() + col]
    }

    /// Read `main` (grouped `shape.grouping()`) and, for a matrix with a
    /// Σ row, `totals` (grouped `shape.totals_grouping()`) into a pivot.
    /// `Err` names a snapshot that does not fit the shape — a stale or
    /// foreign delivery — and a shape never checked against the schema.
    pub fn build(
        main: &Snapshot,
        totals: Option<&Snapshot>,
        shape: &ShapeSpec,
        mode: Mode,
        domains: &AxisDomains,
    ) -> Result<Pivot, String> {
        let grouping = shape.grouping();
        if main.grouping() != grouping.as_slice() {
            return Err(format!(
                "snapshot grouped by [{}]; the shape needs [{}]",
                main.grouping().join(", "),
                grouping.join(", ")
            ));
        }
        if !main.has_depth_column() {
            return Err(
                "snapshot lacks 'row_depth': its Σ come from the subtotal rows".to_string(),
            );
        }
        let rows_axis = shape
            .axis(&shape.rows)
            .ok_or_else(|| "the shape has not been checked against the schema".to_string())?;
        let col_axes: Vec<&AxisInfo> = shape
            .column_axes()
            .iter()
            .map(|c| {
                shape.axis(c).ok_or_else(|| {
                    format!("the shape has not been checked against the schema ('{c}')")
                })
            })
            .collect::<Result<_, _>>()?;
        let axis_idx: Vec<usize> = grouping
            .iter()
            .map(|g| {
                main.column_index(g)
                    .ok_or_else(|| format!("snapshot lacks '{g}'"))
            })
            .collect::<Result<_, _>>()?;
        let leaf_depth = grouping.len();

        // Which snapshot column each painted measure reads.
        let reads: Vec<(&MeasureColumn, String, usize)> = shape
            .measures()
            .into_iter()
            .map(|m| {
                let name = read_column(m, mode.currency);
                let idx = main
                    .column_index(&name)
                    .ok_or_else(|| format!("snapshot lacks '{name}'"))?;
                Ok((m, name, idx))
            })
            .collect::<Result<_, String>>()?;
        if reads.is_empty() {
            return Err("the shape paints no measure".to_string());
        }

        // Pass 1: which rows are leaves, subtotals and the total, and which
        // labels are present.
        let mut leaves = Vec::new();
        let mut row_labels: Vec<String> = domains.get(&shape.rows).to_vec();
        let mut col_keys: Vec<Vec<String>> = match col_axes.len() {
            1 => domains
                .get(&col_axes[0].name)
                .iter()
                .map(|l| vec![l.clone()])
                .collect(),
            _ => Vec::new(),
        };
        let mut total_row = None;
        for r in 0..main.rows() {
            let depth = main.depth_of_row(r).unwrap_or(leaf_depth);
            if depth == 0 {
                total_row = Some(r);
            }
            if depth >= 1
                && let Some(label) = main.text_at(axis_idx[0], r)
                && !row_labels.iter().any(|l| l == label)
            {
                row_labels.push(label.to_string());
            }
            if depth == leaf_depth {
                leaves.push(r);
                if !col_axes.is_empty() {
                    let key: Option<Vec<String>> = axis_idx[1..]
                        .iter()
                        .map(|&i| main.text_at(i, r).map(str::to_string))
                        .collect();
                    if let Some(key) = key
                        && !col_keys.contains(&key)
                    {
                        col_keys.push(key);
                    }
                }
            }
        }

        let rows = ordered(row_labels, rows_axis);
        let row_of: HashMap<&str, usize> = rows
            .iter()
            .enumerate()
            .map(|(i, l)| (l.text.as_str(), i))
            .collect();
        let (columns, col_of) = match &shape.columns {
            Columns::Axes(_) => axis_columns(col_keys, &col_axes, &reads[0]),
            Columns::Measures(_) => (
                reads
                    .iter()
                    .map(|(m, name, _)| Column {
                        head: Head::Measure {
                            label: m.label.clone(),
                            unit: m.unit.clone(),
                        },
                        measure: name.clone(),
                        format: m.format.clone(),
                        diffed: false,
                    })
                    .collect(),
                HashMap::new(),
            ),
        };
        let ncols = columns.len();

        // Pass 2: the cells.
        let mut cells = vec![None; rows.len() * ncols];
        // Rows a scoped leaf carries; a domain-only row has none.
        let mut has_leaf = vec![false; rows.len()];
        for &r in &leaves {
            let Some(&row) = main.text_at(axis_idx[0], r).and_then(|l| row_of.get(l)) else {
                continue;
            };
            has_leaf[row] = true;
            match &shape.columns {
                Columns::Axes(_) => {
                    let key: Option<Vec<String>> = axis_idx[1..]
                        .iter()
                        .map(|&i| main.text_at(i, r).map(str::to_string))
                        .collect();
                    if let Some(col) = key.and_then(|k| col_of.get(&k)) {
                        cells[row * ncols + col] = main.f64_at(reads[0].2, r);
                    }
                }
                Columns::Measures(_) => {
                    for (col, (_, _, idx)) in reads.iter().enumerate() {
                        cells[row * ncols + col] = main.f64_at(*idx, r);
                    }
                }
            }
        }

        // Σ, from the snapshot's own rows and never from the cells.
        let value_idx = reads[0].2;
        let row_sums = if shape.sums_columns() {
            let mut sums = vec![None; rows.len()];
            for r in 0..main.rows() {
                if main.depth_of_row(r) != Some(1) {
                    continue;
                }
                if let Some(row) = main.text_at(axis_idx[0], r).and_then(|l| row_of.get(l)) {
                    sums[*row] = main.f64_at(value_idx, r);
                }
            }
            Some(sums)
        } else {
            None
        };
        let mut totals_disagree = false;
        let mut column_sums = match (&shape.columns, shape.sums_rows()) {
            (Columns::Measures(_), true) => total_row.map(|r| {
                reads
                    .iter()
                    .map(|(_, _, idx)| main.f64_at(*idx, r))
                    .collect()
            }),
            (Columns::Axes(_), true) => match totals {
                None => None,
                Some(t) => {
                    let wanted = shape.totals_grouping().expect("sums_rows and axis columns");
                    if t.grouping() != wanted.as_slice() {
                        return Err(format!(
                            "totals snapshot grouped by [{}]; the shape needs [{}]",
                            t.grouping().join(", "),
                            wanted.join(", ")
                        ));
                    }
                    totals_disagree = matches!(
                        (generation(main, &shape.dataset), generation(t, &shape.dataset)),
                        (Some(a), Some(b)) if a != b
                    );
                    if totals_disagree {
                        None
                    } else {
                        Some(column_totals(t, &wanted, &reads[0].1, &col_of, ncols)?)
                    }
                }
            },
            (_, false) => None,
        };
        let grand_sum = match (&shape.columns, row_sums.is_some() && shape.sums_rows()) {
            (Columns::Axes(_), true) => Some(total_row.and_then(|r| main.f64_at(value_idx, r))),
            _ => None,
        };

        // Diff: the configured measures against the base row. The base is
        // the row a scoped leaf carries — a domain-only label has nothing
        // to subtract — and a diff measure whose base value is NULL stays
        // absolute rather than blanking its column; either is reported.
        let mut columns = columns;
        let mut base_row = None;
        let mut diff_base_missing = false;
        if let (true, Some(diff), Columns::Measures(_)) = (mode.diff, &shape.diff, &shape.columns) {
            base_row = (0..rows.len()).find(|&i| has_leaf[i] && rows[i].matches(&diff.base));
            match base_row {
                None => diff_base_missing = true,
                Some(base) => {
                    for (col, column) in columns.iter_mut().enumerate() {
                        let (m, _, _) = &reads[col];
                        if !diff.measures.contains(&m.column) {
                            continue;
                        }
                        let Some(b) = cells[base * ncols + col] else {
                            diff_base_missing = true;
                            continue;
                        };
                        column.diffed = true;
                        for row in 0..rows.len() {
                            if row == base {
                                continue;
                            }
                            let at = row * ncols + col;
                            cells[at] = cells[at].map(|v| v - b);
                        }
                    }
                }
            }
        }
        // The total row is absolute: no Σ under value-minus-base cells.
        if let Some(sums) = column_sums.as_mut() {
            for (sum, column) in sums.iter_mut().zip(&columns) {
                if column.diffed {
                    *sum = None;
                }
            }
        }

        let currency_mixed = match (main.column_index("currency"), total_row) {
            (Some(idx), Some(r)) => main.is_mixed_at(idx, r),
            _ => false,
        };
        let mut heat_domain = 0.0_f64;
        for row in 0..rows.len() {
            for (col, column) in columns.iter().enumerate() {
                if Some(row) == base_row && column.diffed {
                    continue;
                }
                if let Some(v) = cells[row * ncols + col]
                    && v.is_finite()
                {
                    heat_domain = heat_domain.max(v.abs());
                }
            }
        }
        Ok(Pivot {
            rows,
            columns,
            cells,
            row_sums,
            column_sums,
            grand_sum,
            totals_disagree,
            base_row,
            diff_base_missing,
            currency_mixed,
            heat_domain,
        })
    }
}

/// The dataset's generation a snapshot was read at; `None` when unknown.
fn generation(s: &Snapshot, dataset: &str) -> Option<i64> {
    s.provenance()
        .datasets
        .iter()
        .find(|f| f.dataset == dataset)
        .and_then(|f| f.generation)
}

/// The Σ row of a matrix: the second query's leaves (every columns axis
/// present), one per column key; a key the main query never showed is
/// ignored, a column the companion lacks stays blank.
fn column_totals(
    totals: &Snapshot,
    grouping: &[String],
    measure: &str,
    col_of: &HashMap<Vec<String>, usize>,
    ncols: usize,
) -> Result<Vec<Option<f64>>, String> {
    if !totals.has_depth_column() {
        return Err("totals snapshot lacks 'row_depth': its Σ come from the leaf rows".to_string());
    }
    let axis_idx: Vec<usize> = grouping
        .iter()
        .map(|g| {
            totals
                .column_index(g)
                .ok_or_else(|| format!("totals snapshot lacks '{g}'"))
        })
        .collect::<Result<_, _>>()?;
    let value_idx = totals
        .column_index(measure)
        .ok_or_else(|| format!("totals snapshot lacks '{measure}'"))?;
    let mut sums = vec![None; ncols];
    for r in 0..totals.rows() {
        if totals.depth_of_row(r).unwrap_or(grouping.len()) != grouping.len() {
            continue;
        }
        let key: Option<Vec<String>> = axis_idx
            .iter()
            .map(|&i| totals.text_at(i, r).map(str::to_string))
            .collect();
        if let Some(col) = key.and_then(|k| col_of.get(&k)) {
            sums[*col] = totals.f64_at(value_idx, r);
        }
    }
    Ok(sums)
}

/// The snapshot column a measure reads under `currency`: its twin in USD
/// when it has one, itself otherwise.
fn read_column(m: &MeasureColumn, currency: Currency) -> String {
    match (currency, &m.usd_twin) {
        (Currency::Usd, Some(twin)) => twin.clone(),
        _ => m.column.clone(),
    }
}

/// `labels` deduplicated and ordered by the axis: numerically for a numeric
/// axis (unparsable labels last, by text), by `sort_axis_text` otherwise.
fn ordered(labels: Vec<String>, axis: &AxisInfo) -> Vec<AxisLabel> {
    let mut labels = labels;
    labels.sort();
    labels.dedup();
    if axis.is_numeric() {
        let mut parsed: Vec<AxisLabel> = labels.iter().map(|t| AxisLabel::parse(t, axis)).collect();
        parsed.sort_by(|a, b| match (a.number, b.number) {
            (Some(x), Some(y)) => x
                .partial_cmp(&y)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.text.cmp(&b.text)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => a.text.cmp(&b.text),
        });
        parsed
    } else {
        sort_axis_text(&mut labels, axis.order);
        labels.iter().map(|t| AxisLabel::parse(t, axis)).collect()
    }
}

/// The columns of a matrix: one per key (one label, or an outer and an
/// inner), ordered outer then inner by each axis's rule, and the index of
/// each key.
fn axis_columns(
    keys: Vec<Vec<String>>,
    axes: &[&AxisInfo],
    value: &(&MeasureColumn, String, usize),
) -> (Vec<Column>, HashMap<Vec<String>, usize>) {
    // Rank every level's labels once, then order the keys by rank.
    let ranks: Vec<HashMap<String, usize>> = axes
        .iter()
        .enumerate()
        .map(|(level, axis)| {
            let labels = keys.iter().map(|k| k[level].clone()).collect();
            ordered(labels, axis)
                .into_iter()
                .enumerate()
                .map(|(i, l)| (l.text, i))
                .collect()
        })
        .collect();
    let mut keys = keys;
    keys.sort_by_key(|k| {
        k.iter()
            .enumerate()
            .map(|(level, l)| ranks[level][l])
            .collect::<Vec<_>>()
    });
    keys.dedup();
    let (m, name, _) = value;
    let columns = keys
        .iter()
        .map(|k| {
            let (outer, inner) = match axes.len() {
                2 => (
                    Some(AxisLabel::parse(&k[0], axes[0])),
                    AxisLabel::parse(&k[1], axes[1]),
                ),
                _ => (None, AxisLabel::parse(&k[0], axes[0])),
            };
            Column {
                head: Head::Axis {
                    outer,
                    label: inner,
                },
                measure: name.clone(),
                format: m.format.clone(),
                diffed: false,
            }
        })
        .collect();
    let col_of = keys.into_iter().enumerate().map(|(i, k)| (k, i)).collect();
    (columns, col_of)
}
#[cfg(test)]
pub(crate) mod fixtures {
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::scenario::{
        AxisInfo, Columns, Diff, DiffBase, MeasureColumn, Paint, ShapeSpec,
    };
    use geode_core::schema::ColumnType;
    use geode_core::schema::axis::{AxisKind, AxisOrder};
    use geode_core::snapshot::{ColumnMeta, Freshness, Provenance, Snapshot, TestColumn};
    use geode_core::view::ColumnFormat;

    pub fn axis(name: &str, kind: AxisKind, order: AxisOrder, ty: ColumnType) -> AxisInfo {
        AxisInfo {
            name: name.into(),
            kind,
            order,
            ty,
        }
    }

    pub fn measure(column: &str, label: &str, twin: Option<&str>) -> MeasureColumn {
        MeasureColumn {
            column: column.into(),
            label: label.into(),
            format: ColumnFormat::MEASURE,
            unit: None,
            usd_twin: twin.map(str::to_string),
        }
    }

    /// Vega K/T as `load_scenarios` would hand it over: tenor (bucket,
    /// tenor order, text) down, moneyness (bucket, f64) across, vega with
    /// its USD twin.
    pub fn vega_kt() -> ShapeSpec {
        ShapeSpec {
            kind: "vega_kt".into(),
            title: "Vega K/T".into(),
            dataset: "vega_buckets".into(),
            rows: "tenor".into(),
            columns: Columns::Axes(vec!["moneyness".into()]),
            value: Some(measure("vega", "vega", Some("vega_usd"))),
            diff: None,
            paint: Paint::Heat,
            axes: vec![
                axis(
                    "tenor",
                    AxisKind::Bucket,
                    AxisOrder::Tenor,
                    ColumnType::Utf8,
                ),
                axis(
                    "moneyness",
                    AxisKind::Bucket,
                    AxisOrder::Lexical,
                    ColumnType::F64,
                ),
            ],
        }
    }

    /// Vega by maturity: tenor down, spot shift (scenario) across.
    pub fn vega_spot() -> ShapeSpec {
        ShapeSpec {
            kind: "vega_spot".into(),
            dataset: "vega_spot_ladder".into(),
            columns: Columns::Axes(vec!["spot_shift".into()]),
            axes: vec![
                axis(
                    "tenor",
                    AxisKind::Bucket,
                    AxisOrder::Tenor,
                    ColumnType::Utf8,
                ),
                axis(
                    "spot_shift",
                    AxisKind::Scenario,
                    AxisOrder::Lexical,
                    ColumnType::F64,
                ),
            ],
            ..vega_kt()
        }
    }

    /// The spot ladder: spot shift (scenario) down, npv (diffed) and delta
    /// across, both with twins, theta without.
    pub fn spot_ladder() -> ShapeSpec {
        ShapeSpec {
            kind: "spot_ladder".into(),
            title: "Spot ladder".into(),
            dataset: "spot_ladder".into(),
            rows: "spot_shift".into(),
            columns: Columns::Measures(vec![
                measure("npv", "P&L vs base", Some("npv_usd")),
                measure("delta", "Δ01", Some("delta_usd")),
                MeasureColumn {
                    unit: Some("per day".into()),
                    ..measure("theta", "Θ", None)
                },
            ]),
            value: None,
            diff: Some(Diff {
                base: DiffBase::Number(0.0),
                measures: vec!["npv".into()],
            }),
            paint: Paint::Heat,
            axes: vec![axis(
                "spot_shift",
                AxisKind::Scenario,
                AxisOrder::Lexical,
                ColumnType::F64,
            )],
        }
    }

    /// By maturity: tenor (bucket) down, two measures across, no diff.
    pub fn by_maturity() -> ShapeSpec {
        ShapeSpec {
            kind: "by_maturity".into(),
            dataset: "term_sensitivities".into(),
            rows: "tenor".into(),
            columns: Columns::Measures(vec![
                measure("repo", "repo", None),
                measure("skew", "skew", None),
            ]),
            diff: None,
            paint: Paint::Plain,
            axes: vec![axis(
                "tenor",
                AxisKind::Bucket,
                AxisOrder::Tenor,
                ColumnType::Utf8,
            )],
            ..spot_ladder()
        }
    }

    /// Rho: tenor down, curve_ccy › curve_type across, all bucket.
    pub fn rho() -> ShapeSpec {
        ShapeSpec {
            kind: "rho".into(),
            dataset: "rho_curves".into(),
            columns: Columns::Axes(vec!["curve_ccy".into(), "curve_type".into()]),
            value: Some(measure("rho", "rho", None)),
            axes: vec![
                axis(
                    "tenor",
                    AxisKind::Bucket,
                    AxisOrder::Tenor,
                    ColumnType::Utf8,
                ),
                axis(
                    "curve_ccy",
                    AxisKind::Bucket,
                    AxisOrder::Lexical,
                    ColumnType::Utf8,
                ),
                axis(
                    "curve_type",
                    AxisKind::Bucket,
                    AxisOrder::Lexical,
                    ColumnType::Utf8,
                ),
            ],
            ..vega_kt()
        }
    }

    pub fn dim(name: &str, depth: usize) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            attribution_by_depth: vec![Attribution::Additive; depth + 1],
            scope_semantics: ScopeSemantics::Direct,
            summable: false,
            mixed_flag: None,
        }
    }

    pub fn sum(name: &str, depth: usize) -> ColumnMeta {
        ColumnMeta {
            summable: true,
            ..dim(name, depth)
        }
    }

    pub fn provenance(dataset: &str, generation: Option<i64>) -> Provenance {
        Provenance {
            datasets: vec![Freshness {
                dataset: dataset.into(),
                as_of: None,
                generation,
            }],
            as_of_request: None,
        }
    }

    /// One fixture row: grouping labels, depth, measure values.
    pub type Row = (Vec<Option<&'static str>>, i32, Vec<Option<f64>>);

    /// One row per tuple of (grouping labels…, depth, measure values…).
    /// Grouping labels are `None` on subtotal and total rows, as the
    /// compiler leaves them.
    pub struct Rows {
        pub grouping: Vec<&'static str>,
        pub measures: Vec<&'static str>,
        pub rows: Vec<Row>,
    }

    impl Rows {
        pub fn snapshot(&self, dataset: &str, generation: Option<i64>) -> Snapshot {
            let depth = self.grouping.len();
            let mut columns: Vec<(ColumnMeta, TestColumn)> = Vec::new();
            for (i, g) in self.grouping.iter().enumerate() {
                let values = self.rows.iter().map(|(labels, _, _)| labels[i]).collect();
                columns.push((dim(g, depth), TestColumn::Str(values)));
            }
            columns.push((
                dim("row_depth", depth),
                TestColumn::I32(self.rows.iter().map(|(_, d, _)| *d).collect()),
            ));
            for (i, m) in self.measures.iter().enumerate() {
                let values = self.rows.iter().map(|(_, _, v)| v[i]).collect();
                columns.push((sum(m, depth), TestColumn::F64(values)));
            }
            Snapshot::for_tests_with_provenance(columns, depth, provenance(dataset, generation))
        }
    }

    /// The vega K/T picture: three tenors (18M between 1Y and 2Y), two
    /// moneyness buckets whose text sorts the wrong way round, one empty
    /// cell (2Y × 100), and a 1Y subtotal that deliberately disagrees with
    /// its two leaves (31 against 10 + 20).
    pub fn vega_main() -> Rows {
        Rows {
            grouping: vec!["tenor", "moneyness"],
            measures: vec!["vega", "vega_usd"],
            rows: vec![
                (
                    vec![Some("1Y"), Some("90.0")],
                    2,
                    vec![Some(10.0), Some(12.0)],
                ),
                (
                    vec![Some("1Y"), Some("100.0")],
                    2,
                    vec![Some(20.0), Some(24.0)],
                ),
                (
                    vec![Some("2Y"), Some("90.0")],
                    2,
                    vec![Some(30.0), Some(36.0)],
                ),
                (
                    vec![Some("18M"), Some("100.0")],
                    2,
                    vec![Some(5.0), Some(6.0)],
                ),
                (vec![Some("1Y"), None], 1, vec![Some(31.0), Some(37.0)]),
                (vec![Some("2Y"), None], 1, vec![Some(30.0), Some(36.0)]),
                (vec![Some("18M"), None], 1, vec![Some(5.0), Some(6.0)]),
                (vec![None, None], 0, vec![Some(66.0), Some(79.0)]),
            ],
        }
    }

    /// The companion: grouped by moneyness alone.
    pub fn vega_totals() -> Rows {
        Rows {
            grouping: vec!["moneyness"],
            measures: vec!["vega", "vega_usd"],
            rows: vec![
                (vec![Some("90.0")], 1, vec![Some(40.0), Some(48.0)]),
                (vec![Some("100.0")], 1, vec![Some(25.0), Some(30.0)]),
                (vec![None], 0, vec![Some(65.0), Some(78.0)]),
            ],
        }
    }

    /// The spot ladder picture: shifts -5, 0, +5 with npv 90 / 100 / 115.
    pub fn ladder_main() -> Rows {
        Rows {
            grouping: vec!["spot_shift"],
            measures: vec!["npv", "npv_usd", "delta", "delta_usd", "theta"],
            rows: vec![
                (
                    vec![Some("-5.0")],
                    1,
                    vec![Some(90.0), Some(99.0), Some(1.0), Some(1.1), Some(-2.0)],
                ),
                (
                    vec![Some("0.0")],
                    1,
                    vec![Some(100.0), Some(110.0), Some(2.0), Some(2.2), Some(-2.0)],
                ),
                (
                    vec![Some("5.0")],
                    1,
                    vec![Some(115.0), Some(126.5), Some(3.0), Some(3.3), Some(-2.0)],
                ),
                // The total row: NULL by scenario-axis attribution.
                (vec![None], 0, vec![None, None, None, None, None]),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use geode_core::scenario::Diff;
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};

    fn usd() -> Mode {
        Mode {
            currency: Currency::Usd,
            diff: false,
        }
    }
    fn native() -> Mode {
        Mode {
            currency: Currency::Native,
            diff: false,
        }
    }
    fn texts(labels: &[AxisLabel]) -> Vec<&str> {
        labels.iter().map(|l| l.text.as_str()).collect()
    }
    fn build(shape: &ShapeSpec, main: &Rows, mode: Mode) -> Pivot {
        Pivot::build(
            &main.snapshot(&shape.dataset, Some(1)),
            None,
            shape,
            mode,
            &AxisDomains::default(),
        )
        .expect("builds")
    }

    #[test]
    fn leaf_cells_land_on_their_row_and_column() {
        let p = build(&vega_kt(), &vega_main(), native());
        assert_eq!(texts(&p.rows), ["1Y", "18M", "2Y"]);
        let col = |t: &str| {
            p.columns
                .iter()
                .position(|c| matches!(&c.head, Head::Axis { label, .. } if label.text == t))
                .unwrap()
        };
        assert_eq!(p.cell(0, col("90.0")), Some(10.0));
        assert_eq!(p.cell(0, col("100.0")), Some(20.0));
        assert_eq!(p.cell(1, col("100.0")), Some(5.0));
        assert_eq!(p.cell(2, col("90.0")), Some(30.0));
        assert_eq!(
            p.cell(2, col("100.0")),
            None,
            "no scoped rows: blank, not zero"
        );
    }

    #[test]
    fn numeric_axis_labels_order_numerically() {
        let p = build(&vega_kt(), &vega_main(), native());
        let labels: Vec<&AxisLabel> = p
            .columns
            .iter()
            .map(|c| match &c.head {
                Head::Axis { label, .. } => label,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(
            labels.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
            ["90.0", "100.0"],
            "100 after 90, not before it as text"
        );
        assert_eq!(labels[0].number, Some(90.0));
        assert_eq!(labels[1].number, Some(100.0));
    }

    #[test]
    fn text_axis_labels_order_by_the_axis_rule() {
        let p = build(&vega_kt(), &vega_main(), native());
        assert_eq!(
            texts(&p.rows),
            ["1Y", "18M", "2Y"],
            "tenor order, not lexical"
        );
        assert!(p.rows.iter().all(|l| l.number.is_none()));
    }

    #[test]
    fn the_domain_adds_labels_the_scope_lacks_and_never_drops_present_ones() {
        let mut domains = AxisDomains::default();
        domains.insert("tenor", vec!["1M".into(), "1Y".into(), "2Y".into()]);
        domains.insert(
            "moneyness",
            vec!["80.0".into(), "90.0".into(), "100.0".into()],
        );
        let shape = vega_kt();
        let p = Pivot::build(
            &vega_main().snapshot("vega_buckets", Some(1)),
            None,
            &shape,
            native(),
            &domains,
        )
        .unwrap();
        assert_eq!(
            texts(&p.rows),
            ["1M", "1Y", "18M", "2Y"],
            "18M is present and stays; 1M comes from the domain"
        );
        let heads: Vec<&str> = p
            .columns
            .iter()
            .map(|c| match &c.head {
                Head::Axis { label, .. } => label.text.as_str(),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(heads, ["80.0", "90.0", "100.0"]);
        assert!(
            (0..p.columns.len()).all(|c| p.cell(0, c).is_none()),
            "a domain-only row is blank"
        );
    }

    #[test]
    fn usd_mode_reads_the_twin_and_native_the_base() {
        let usd_p = build(&vega_kt(), &vega_main(), usd());
        let nat_p = build(&vega_kt(), &vega_main(), native());
        assert_eq!(usd_p.columns[0].measure, "vega_usd");
        assert_eq!(nat_p.columns[0].measure, "vega");
        assert_eq!(usd_p.cell(0, 0), Some(12.0));
        assert_eq!(nat_p.cell(0, 0), Some(10.0));
    }

    #[test]
    fn a_measure_without_a_twin_reads_itself_in_usd_mode() {
        let p = build(&spot_ladder(), &ladder_main(), usd());
        assert_eq!(p.columns[0].measure, "npv_usd");
        assert_eq!(p.columns[2].measure, "theta", "no twin: the base column");
        assert_eq!(p.cell(0, 2), Some(-2.0));
    }

    #[test]
    fn a_ladder_has_one_column_per_measure_with_label_and_unit() {
        let p = build(&spot_ladder(), &ladder_main(), native());
        assert_eq!(
            texts(&p.rows),
            ["-5.0", "0.0", "5.0"],
            "numeric order: -5 before 0"
        );
        assert_eq!(p.columns.len(), 3);
        assert_eq!(
            p.columns[0].head,
            Head::Measure {
                label: "P&L vs base".into(),
                unit: None
            }
        );
        assert_eq!(
            p.columns[2].head,
            Head::Measure {
                label: "Θ".into(),
                unit: Some("per day".into())
            }
        );
        assert_eq!(p.cell(2, 0), Some(115.0));
        assert_eq!(p.cell(1, 1), Some(2.0));
    }

    #[test]
    fn a_two_level_header_pairs_outer_and_inner_in_axis_order() {
        let main = Rows {
            grouping: vec!["tenor", "curve_ccy", "curve_type"],
            measures: vec!["rho"],
            rows: vec![
                (
                    vec![Some("1Y"), Some("USD"), Some("OIS")],
                    3,
                    vec![Some(1.0)],
                ),
                (
                    vec![Some("1Y"), Some("EUR"), Some("OIS")],
                    3,
                    vec![Some(2.0)],
                ),
                (
                    vec![Some("1Y"), Some("USD"), Some("RFR")],
                    3,
                    vec![Some(3.0)],
                ),
                (
                    vec![Some("2Y"), Some("USD"), Some("RFR")],
                    3,
                    vec![Some(4.0)],
                ),
                (vec![Some("1Y"), Some("USD"), None], 2, vec![Some(4.0)]),
                (vec![Some("1Y"), None, None], 1, vec![Some(6.0)]),
                (vec![None, None, None], 0, vec![Some(10.0)]),
            ],
        };
        let p = Pivot::build(
            &main.snapshot("rho_curves", Some(1)),
            None,
            &rho(),
            native(),
            &AxisDomains::default(),
        )
        .unwrap();
        let heads: Vec<(&str, &str)> = p
            .columns
            .iter()
            .map(|c| match &c.head {
                Head::Axis {
                    outer: Some(o),
                    label,
                } => (o.text.as_str(), label.text.as_str()),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            heads,
            [("EUR", "OIS"), ("USD", "OIS"), ("USD", "RFR")],
            "outer first, inner within; only pairs present"
        );
        assert_eq!(p.cell(0, 0), Some(2.0));
        assert_eq!(p.cell(0, 2), Some(3.0));
        assert_eq!(p.cell(1, 2), Some(4.0));
        assert_eq!(p.cell(1, 0), None);
    }

    #[test]
    fn a_snapshot_grouped_differently_is_refused() {
        let err = Pivot::build(
            &vega_totals().snapshot("vega_buckets", Some(1)),
            None,
            &vega_kt(),
            native(),
            &AxisDomains::default(),
        )
        .unwrap_err();
        assert!(err.contains("grouped by [moneyness]"), "{err}");
    }

    #[test]
    fn an_unchecked_shape_is_refused() {
        let shape = ShapeSpec {
            axes: Vec::new(),
            ..vega_kt()
        };
        let err = Pivot::build(
            &vega_main().snapshot("vega_buckets", Some(1)),
            None,
            &shape,
            native(),
            &AxisDomains::default(),
        )
        .unwrap_err();
        assert!(err.contains("checked"), "{err}");
    }

    #[test]
    fn a_snapshot_missing_the_measure_is_refused_not_blank() {
        let main = Rows {
            measures: vec!["vega"],
            rows: vega_main()
                .rows
                .into_iter()
                .map(|(l, d, v)| (l, d, vec![v[0]]))
                .collect(),
            ..vega_main()
        };
        let err = Pivot::build(
            &main.snapshot("vega_buckets", Some(1)),
            None,
            &vega_kt(),
            usd(),
            &AxisDomains::default(),
        )
        .unwrap_err();
        assert!(err.contains("vega_usd"), "{err}");
    }

    #[test]
    fn the_heat_domain_is_the_largest_cell_magnitude() {
        assert_eq!(build(&vega_kt(), &vega_main(), native()).heat_domain, 30.0);
        assert_eq!(build(&vega_kt(), &vega_main(), usd()).heat_domain, 36.0);
        // Σ rows do not count: 66 and 31 are not cells.
    }

    #[test]
    fn currency_mixed_reads_the_total_rows_flag() {
        let depth = 2;
        let columns: Vec<(ColumnMeta, TestColumn)> = vec![
            (
                dim("tenor", depth),
                TestColumn::Str(vec![Some("1Y"), Some("1Y"), None]),
            ),
            (
                dim("moneyness", depth),
                TestColumn::Str(vec![Some("90.0"), None, None]),
            ),
            (dim("row_depth", depth), TestColumn::I32(vec![2, 1, 0])),
            (
                sum("vega", depth),
                TestColumn::F64(vec![Some(1.0), Some(1.0), Some(1.0)]),
            ),
            (
                sum("vega_usd", depth),
                TestColumn::F64(vec![Some(1.0), Some(1.0), Some(1.0)]),
            ),
            (
                ColumnMeta {
                    mixed_flag: Some(6),
                    ..dim("currency", depth)
                },
                TestColumn::Str(vec![Some("USD"), Some("USD"), None]),
            ),
            (
                dim("currency_mixed", depth),
                TestColumn::Bool(vec![Some(false), Some(false), Some(true)]),
            ),
        ];
        let s = Snapshot::for_tests_with_provenance(
            columns,
            depth,
            provenance("vega_buckets", Some(1)),
        );
        let p = Pivot::build(&s, None, &vega_kt(), native(), &AxisDomains::default()).unwrap();
        assert!(p.currency_mixed);
        let plain = build(&vega_kt(), &vega_main(), native());
        assert!(!plain.currency_mixed, "no currency column: never mixed");
    }

    fn build_with_totals(
        shape: &ShapeSpec,
        main: &Rows,
        totals: Option<(&Rows, Option<i64>)>,
        main_gen: Option<i64>,
        mode: Mode,
    ) -> Pivot {
        let main = main.snapshot(&shape.dataset, main_gen);
        let totals = totals.map(|(t, g)| t.snapshot(&shape.dataset, g));
        Pivot::build(&main, totals.as_ref(), shape, mode, &AxisDomains::default()).expect("builds")
    }

    #[test]
    fn row_sums_come_from_the_subtotal_rows_not_the_cells() {
        let p = build(&vega_kt(), &vega_main(), native());
        let sums = p.row_sums.as_ref().expect("both axes are buckets");
        // 1Y's leaves sum to 30; the subtotal row says 31, and the subtotal
        // row is what the engine's attribution rules vouch for.
        assert_eq!(sums[0], Some(31.0));
        assert_eq!(sums[1], Some(5.0));
        assert_eq!(sums[2], Some(30.0));
    }

    #[test]
    fn the_grand_sum_is_the_total_row() {
        let p = build_with_totals(
            &vega_kt(),
            &vega_main(),
            Some((&vega_totals(), Some(1))),
            Some(1),
            native(),
        );
        assert_eq!(
            p.grand_sum,
            Some(Some(66.0)),
            "66, not the 65 the companion's total says and not the cells' 65"
        );
    }

    #[test]
    fn column_sums_come_from_the_totals_snapshot() {
        let p = build_with_totals(
            &vega_kt(),
            &vega_main(),
            Some((&vega_totals(), Some(1))),
            Some(1),
            native(),
        );
        assert_eq!(p.column_sums, Some(vec![Some(40.0), Some(25.0)]));
        assert!(!p.totals_disagree);
        let usd_p = build_with_totals(
            &vega_kt(),
            &vega_main(),
            Some((&vega_totals(), Some(1))),
            Some(1),
            usd(),
        );
        assert_eq!(
            usd_p.column_sums,
            Some(vec![Some(48.0), Some(30.0)]),
            "the twin's Σ in USD mode"
        );
    }

    #[test]
    fn column_sums_blank_until_the_totals_snapshot_arrives() {
        let p = build(&vega_kt(), &vega_main(), native());
        assert_eq!(p.column_sums, None);
        assert_eq!(
            p.grand_sum,
            Some(Some(66.0)),
            "the corner is the main query's and stays"
        );
        assert!(!p.totals_disagree);
    }

    #[test]
    fn column_sums_blank_while_the_pairs_generations_disagree() {
        let stale = build_with_totals(
            &vega_kt(),
            &vega_main(),
            Some((&vega_totals(), Some(2))),
            Some(1),
            native(),
        );
        assert!(stale.totals_disagree);
        assert_eq!(stale.column_sums, None);
        assert!(
            stale.row_sums.is_some(),
            "the main query's Σ are unaffected"
        );
        let unknown = build_with_totals(
            &vega_kt(),
            &vega_main(),
            Some((&vega_totals(), None)),
            Some(1),
            native(),
        );
        assert!(
            !unknown.totals_disagree,
            "an unknown generation is not a disagreement"
        );
        assert!(unknown.column_sums.is_some());
    }

    #[test]
    fn a_totals_snapshot_grouped_differently_is_refused() {
        let main = vega_main().snapshot("vega_buckets", Some(1));
        let wrong = vega_main().snapshot("vega_buckets", Some(1));
        let err = Pivot::build(
            &main,
            Some(&wrong),
            &vega_kt(),
            native(),
            &AxisDomains::default(),
        )
        .unwrap_err();
        assert!(err.contains("totals"), "{err}");
    }

    #[test]
    fn no_sums_along_a_scenario_axis() {
        // vega by spot shift: Σ over tenors per shift (bucket) exists; Σ
        // across shifts and the corner do not.
        let main = Rows {
            grouping: vec!["tenor", "spot_shift"],
            measures: vec!["vega", "vega_usd"],
            rows: vec![
                (
                    vec![Some("1Y"), Some("-5.0")],
                    2,
                    vec![Some(1.0), Some(1.0)],
                ),
                (vec![Some("1Y"), Some("0.0")], 2, vec![Some(2.0), Some(2.0)]),
                (vec![Some("1Y"), None], 1, vec![None, None]),
                (vec![None, None], 0, vec![None, None]),
            ],
        };
        let totals = Rows {
            grouping: vec!["spot_shift"],
            measures: vec!["vega", "vega_usd"],
            rows: vec![
                (vec![Some("-5.0")], 1, vec![Some(1.0), Some(1.0)]),
                (vec![Some("0.0")], 1, vec![Some(2.0), Some(2.0)]),
                (vec![None], 0, vec![None, None]),
            ],
        };
        let p = build_with_totals(
            &vega_spot(),
            &main,
            Some((&totals, Some(1))),
            Some(1),
            native(),
        );
        assert_eq!(p.row_sums, None, "no Σ across spot shifts");
        assert_eq!(p.column_sums, Some(vec![Some(1.0), Some(2.0)]));
        assert_eq!(p.grand_sum, None);

        // The spot ladder: nothing sums at all.
        let p = build(&spot_ladder(), &ladder_main(), native());
        assert_eq!((p.row_sums, p.column_sums, p.grand_sum), (None, None, None));
    }

    #[test]
    fn a_ladders_sum_row_is_the_total_row_per_measure() {
        let main = Rows {
            grouping: vec!["tenor"],
            measures: vec!["repo", "skew"],
            rows: vec![
                (vec![Some("1Y")], 1, vec![Some(1.0), Some(10.0)]),
                (vec![Some("2Y")], 1, vec![Some(2.0), Some(20.0)]),
                (vec![None], 0, vec![Some(3.5), None]),
            ],
        };
        let p = build(&by_maturity(), &main, native());
        assert_eq!(
            p.column_sums,
            Some(vec![Some(3.5), None]),
            "the total row, as it stands — a NULL stays blank"
        );
        assert_eq!(p.row_sums, None, "never across measures");
        assert_eq!(p.grand_sum, None);
    }

    #[test]
    fn diff_subtracts_the_base_row_for_the_configured_measures_only() {
        let p = build(
            &spot_ladder(),
            &ladder_main(),
            Mode {
                currency: Currency::Native,
                diff: true,
            },
        );
        assert_eq!(p.base_row, Some(1));
        assert!(p.columns[0].diffed && !p.columns[1].diffed && !p.columns[2].diffed);
        assert_eq!(p.cell(0, 0), Some(-10.0));
        assert_eq!(p.cell(1, 0), Some(100.0), "the base row stays absolute");
        assert_eq!(p.cell(2, 0), Some(15.0));
        assert_eq!(p.cell(0, 1), Some(1.0), "delta is not a diff measure");
        assert!(!p.diff_base_missing);
    }

    #[test]
    fn diff_off_leaves_absolutes_and_no_base_row() {
        let p = build(&spot_ladder(), &ladder_main(), native());
        assert_eq!(p.base_row, None);
        assert!(!p.columns[0].diffed);
        assert_eq!(p.cell(0, 0), Some(90.0));
        assert!(!p.diff_base_missing);
    }

    #[test]
    fn a_numeric_diff_base_matches_the_label_by_value() {
        for spelling in ["0", "-0.0", "0.00"] {
            let main = Rows {
                rows: ladder_main()
                    .rows
                    .into_iter()
                    .map(|(l, d, v)| {
                        (
                            l.into_iter()
                                .map(|t| t.map(|t| if t == "0.0" { spelling } else { t }))
                                .collect(),
                            d,
                            v,
                        )
                    })
                    .collect(),
                ..ladder_main()
            };
            let p = build(
                &spot_ladder(),
                &main,
                Mode {
                    currency: Currency::Native,
                    diff: true,
                },
            );
            assert_eq!(p.base_row, Some(1), "{spelling}");
            assert_eq!(p.cell(2, 0), Some(15.0), "{spelling}");
        }
    }

    #[test]
    fn a_missing_base_row_paints_absolutes_and_says_so() {
        let main = Rows {
            rows: ladder_main()
                .rows
                .into_iter()
                .filter(|(l, _, _)| l[0] != Some("0.0"))
                .collect(),
            ..ladder_main()
        };
        let p = build(
            &spot_ladder(),
            &main,
            Mode {
                currency: Currency::Native,
                diff: true,
            },
        );
        assert!(p.diff_base_missing);
        assert_eq!(p.base_row, None);
        assert_eq!(p.cell(0, 0), Some(90.0), "absolute");
        assert_eq!(p.cell(1, 0), Some(115.0));
        assert!(!p.columns[0].diffed, "nothing is diffed without a base");
    }

    fn diff_on() -> Mode {
        Mode {
            currency: Currency::Native,
            diff: true,
        }
    }

    #[test]
    fn a_base_label_only_the_domain_carries_is_a_missing_base() {
        // The dataset knows the 0.0 shift, so its row is drawn; no scoped
        // leaf carries it, so there is nothing to subtract.
        let main = Rows {
            rows: ladder_main()
                .rows
                .into_iter()
                .filter(|(l, _, _)| l[0] != Some("0.0"))
                .collect(),
            ..ladder_main()
        };
        let mut domains = AxisDomains::default();
        domains.insert(
            "spot_shift",
            vec!["-5.0".into(), "0.0".into(), "5.0".into()],
        );
        let shape = spot_ladder();
        let p = Pivot::build(
            &main.snapshot(&shape.dataset, Some(1)),
            None,
            &shape,
            diff_on(),
            &domains,
        )
        .unwrap();
        assert_eq!(texts(&p.rows), ["-5.0", "0.0", "5.0"]);
        assert!(p.diff_base_missing, "the notice stands");
        assert_eq!(p.base_row, None);
        assert!(!p.columns[0].diffed);
        assert_eq!(p.cell(0, 0), Some(90.0), "absolute, not blank");
        assert_eq!(p.cell(1, 0), None, "the domain-only row is blank");
        assert_eq!(p.cell(2, 0), Some(115.0));
    }

    #[test]
    fn a_null_base_value_leaves_that_column_absolute_and_says_so() {
        let main = Rows {
            rows: ladder_main()
                .rows
                .into_iter()
                .map(|(l, d, mut v)| {
                    if l[0] == Some("0.0") {
                        v[0] = None;
                    }
                    (l, d, v)
                })
                .collect(),
            ..ladder_main()
        };
        let shape = ShapeSpec {
            diff: Some(Diff {
                base: DiffBase::Number(0.0),
                measures: vec!["npv".into(), "delta".into()],
            }),
            ..spot_ladder()
        };
        let p = build(&shape, &main, diff_on());
        assert_eq!(p.base_row, Some(1));
        assert!(p.diff_base_missing, "npv has no base value");
        assert!(!p.columns[0].diffed, "npv stays absolute");
        assert_eq!(p.cell(0, 0), Some(90.0));
        assert_eq!(p.cell(2, 0), Some(115.0));
        assert!(p.columns[1].diffed, "delta has its base and diffs");
        assert_eq!(p.cell(0, 1), Some(-1.0));
        assert_eq!(p.cell(1, 1), Some(2.0));
        assert_eq!(p.cell(2, 1), Some(1.0));
        assert!(!p.columns[2].diffed);
    }

    #[test]
    fn a_diffed_ladder_column_has_no_sum() {
        let main = Rows {
            grouping: vec!["tenor"],
            measures: vec!["repo", "skew"],
            rows: vec![
                (vec![Some("1Y")], 1, vec![Some(1.0), Some(10.0)]),
                (vec![Some("2Y")], 1, vec![Some(4.0), Some(20.0)]),
                (vec![None], 0, vec![Some(5.0), Some(30.0)]),
            ],
        };
        let shape = ShapeSpec {
            diff: Some(Diff {
                base: DiffBase::Text("1Y".into()),
                measures: vec!["repo".into()],
            }),
            ..by_maturity()
        };
        let p = build(&shape, &main, diff_on());
        assert!(p.columns[0].diffed);
        assert_eq!(p.cell(1, 0), Some(3.0));
        assert_eq!(
            p.column_sums,
            Some(vec![None, Some(30.0)]),
            "an absolute total never sits under value-minus-base cells"
        );
        let absolute = build(&shape, &main, native());
        assert_eq!(absolute.column_sums, Some(vec![Some(5.0), Some(30.0)]));
    }

    #[test]
    fn a_shape_without_diff_ignores_diff_mode() {
        let p = build(
            &vega_kt(),
            &vega_main(),
            Mode {
                currency: Currency::Native,
                diff: true,
            },
        );
        assert_eq!(p.base_row, None);
        assert!(!p.diff_base_missing, "there is no diff to miss a base for");
        assert_eq!(p.cell(0, 0), Some(10.0));
    }

    #[test]
    fn the_heat_domain_excludes_the_base_rows_absolute_cells_in_diff_mode() {
        let p = build(
            &spot_ladder(),
            &ladder_main(),
            Mode {
                currency: Currency::Native,
                diff: true,
            },
        );
        // Deltas are -10 and 15; the base row's npv (100) is left out as a
        // diffed base cell, its untouched delta/theta cells (2, -2) stay
        // in; the largest is 15.
        assert_eq!(p.heat_domain, 15.0);
        let absolute = build(&spot_ladder(), &ladder_main(), native());
        assert_eq!(absolute.heat_domain, 115.0);
    }

    #[test]
    fn a_non_attributable_subtotal_stays_blank() {
        // Attribution NULLs arrive as NULL cells; the pivot never fills them in.
        let main = Rows {
            rows: vega_main()
                .rows
                .into_iter()
                .map(|(l, d, v)| {
                    if d == 1 {
                        (l, d, vec![None, None])
                    } else {
                        (l, d, v)
                    }
                })
                .collect(),
            ..vega_main()
        };
        let p = build(&vega_kt(), &main, native());
        assert_eq!(p.row_sums, Some(vec![None, None, None]));
    }

    /// `rows` as a snapshot without its `row_depth` column: flat.
    fn without_depths(rows: &Rows, dataset: &str) -> Snapshot {
        let depth = rows.grouping.len();
        let mut columns: Vec<(ColumnMeta, TestColumn)> = Vec::new();
        for (i, g) in rows.grouping.iter().enumerate() {
            let values = rows.rows.iter().map(|(labels, _, _)| labels[i]).collect();
            columns.push((dim(g, depth), TestColumn::Str(values)));
        }
        for (i, m) in rows.measures.iter().enumerate() {
            let values = rows.rows.iter().map(|(_, _, v)| v[i]).collect();
            columns.push((sum(m, depth), TestColumn::F64(values)));
        }
        Snapshot::for_tests_with_provenance(columns, depth, provenance(dataset, Some(1)))
    }

    #[test]
    fn a_snapshot_without_depths_is_refused_not_read_as_leaves() {
        let flat = without_depths(&vega_main(), "vega_buckets");
        assert!(!flat.has_depth_column());
        let err =
            Pivot::build(&flat, None, &vega_kt(), native(), &AxisDomains::default()).unwrap_err();
        assert!(err.contains("row_depth"), "{err}");

        let main = vega_main().snapshot("vega_buckets", Some(1));
        let flat_totals = without_depths(&vega_totals(), "vega_buckets");
        let err = Pivot::build(
            &main,
            Some(&flat_totals),
            &vega_kt(),
            native(),
            &AxisDomains::default(),
        )
        .unwrap_err();
        assert!(err.contains("totals") && err.contains("row_depth"), "{err}");
    }

    #[test]
    fn a_shape_painting_no_measure_is_refused_not_a_panic() {
        let shape = ShapeSpec {
            value: None,
            ..vega_kt()
        };
        let err = Pivot::build(
            &vega_main().snapshot("vega_buckets", Some(1)),
            None,
            &shape,
            native(),
            &AxisDomains::default(),
        )
        .unwrap_err();
        assert!(err.contains("measure"), "{err}");
    }

    #[test]
    fn the_heat_domain_ignores_non_finite_cells() {
        let main = Rows {
            rows: vega_main()
                .rows
                .into_iter()
                .map(|(l, d, v)| {
                    if l == [Some("1Y"), Some("90.0")] {
                        (l, d, vec![Some(f64::INFINITY), Some(f64::NEG_INFINITY)])
                    } else {
                        (l, d, v)
                    }
                })
                .collect(),
            ..vega_main()
        };
        assert_eq!(build(&vega_kt(), &main, native()).heat_domain, 30.0);
        assert_eq!(build(&vega_kt(), &main, usd()).heat_domain, 36.0);
    }
}
