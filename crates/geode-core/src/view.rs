//! View definitions: dataset, joins, columns, derived expressions, grouping,
//! and sort read from configuration. `geode-data` compiles these values into
//! queries. Presentation overlays are applied separately so personal display
//! changes do not replace the desk's view definition.

use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::config::{Diagnostic, MergedDoc, Severity};
use crate::dimensions::DerivedDimensions;
use crate::schema::{ColumnRole, Grain, SchemaSpec};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinSpec {
    pub dataset: String,
    /// Join key columns declared by the view.
    pub on: Vec<String>,
    /// Whether the view needs this join to mean what it says. A required join
    /// that cannot be honoured refuses the view; an optional one is dropped
    /// with an informational diagnostic naming it. Defaults to true, the same
    /// way `ColumnSpec::required` does, so silence means "I meant this".
    pub required: bool,
}

/// Whether a column needs to be honoured to mean what it says. A required
/// column that cannot be honoured refuses the view; an optional one is
/// dropped with an informational diagnostic naming it. Defaults to true, the
/// same way `ColumnSpec::required` does, so silence means "I meant this".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewColumn {
    Dimension {
        name: String,
        required: bool,
    },
    Measure {
        name: String,
        required: bool,
    },
    /// A SQL expression over other columns of the same view.
    Derived {
        name: String,
        sql: String,
        required: bool,
    },
}

impl ViewColumn {
    pub fn name(&self) -> &str {
        match self {
            ViewColumn::Dimension { name, .. }
            | ViewColumn::Measure { name, .. }
            | ViewColumn::Derived { name, .. } => name,
        }
    }

    /// A required measure. `required` is a field rather than a defaulted
    /// builder step because a declaration that omits it means required.
    pub fn measure(name: impl Into<String>) -> ViewColumn {
        ViewColumn::Measure {
            name: name.into(),
            required: true,
        }
    }

    pub fn dimension(name: impl Into<String>) -> ViewColumn {
        ViewColumn::Dimension {
            name: name.into(),
            required: true,
        }
    }

    pub fn derived(name: impl Into<String>, sql: impl Into<String>) -> ViewColumn {
        ViewColumn::Derived {
            name: name.into(),
            sql: sql.into(),
            required: true,
        }
    }

    pub fn required(&self) -> bool {
        match self {
            ViewColumn::Dimension { required, .. }
            | ViewColumn::Measure { required, .. }
            | ViewColumn::Derived { required, .. } => *required,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortKey {
    pub column: String,
    pub descending: bool,
}

/// How a negative number is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Negative {
    Minus,
    Parens,
}

/// Cell text color: `Sign` uses bullish/bearish theme colors; `Named`
/// references a shared color definition for headers and additive values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Colour {
    None,
    Sign,
    Named(String),
}

/// Divide before display: `k` by a thousand, `M` by a million.
/// `precision` applies to the divided number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    None,
    Thousands,
    Millions,
}

impl Scale {
    pub fn divisor(self) -> f64 {
        match self {
            Scale::None => 1.0,
            Scale::Thousands => 1_000.0,
            Scale::Millions => 1_000_000.0,
        }
    }

    /// What the header shows after the label.
    pub fn suffix(self) -> &'static str {
        match self {
            Scale::None => "",
            Scale::Thousands => "k",
            Scale::Millions => "M",
        }
    }
}

/// A resolved format with a value for every formatting property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnFormat {
    pub precision: u8,
    pub thousands: bool,
    pub negative: Negative,
    pub colour: Colour,
    pub scale: Scale,
}

impl ColumnFormat {
    /// The default for a measure or derived number.
    pub const MEASURE: ColumnFormat = ColumnFormat {
        precision: 2,
        thousands: true,
        negative: Negative::Minus,
        colour: Colour::Sign,
        scale: Scale::None,
    };
    /// The default for a dimension or attribute.
    pub const TEXT: ColumnFormat = ColumnFormat {
        precision: 0,
        thousands: false,
        negative: Negative::Minus,
        colour: Colour::None,
        scale: Scale::None,
    };

    /// This default with the presentation's overrides applied.
    pub fn with(self, p: &ColumnPresentation) -> ColumnFormat {
        ColumnFormat {
            precision: p.precision.unwrap_or(self.precision),
            thousands: p.thousands.unwrap_or(self.thousands),
            negative: p.negative.unwrap_or(self.negative),
            colour: p.colour.clone().unwrap_or(self.colour),
            scale: p.scale.unwrap_or(self.scale),
        }
    }
}

/// Optional display properties keyed by column name on the view. Per-kind
/// defaults fill properties left unset when the query plan is built.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ColumnPresentation {
    pub precision: Option<u8>,
    pub thousands: Option<bool>,
    pub negative: Option<Negative>,
    pub colour: Option<Colour>,
    pub scale: Option<Scale>,
    pub label: Option<String>,
    pub width: Option<f32>,
    /// Set by view presentation only, through a column table or the supported
    /// `hidden` array. A hidden column remains in `ViewSpec.columns` and in
    /// the query result; hiding it changes visibility, not the query shape.
    pub hidden: Option<bool>,
}

/// The old spelling of a column's `color` key, still read with a warning so
/// files written before the rename keep their colors.
pub const LEGACY_COLOR_KEY: &str = "colour";

impl ColumnPresentation {
    /// Read `precision`, `thousands`, `negative`, `color`, and `scale`.
    /// View definitions pass a nested `format` table; presentation overlays
    /// pass the column table itself. `warn` receives a key relative to this
    /// table, and the caller adds the document and column path.
    pub fn parse_format_keys(&mut self, table: &toml::Table, warn: &dyn Fn(&str, String)) {
        match table.get("precision") {
            None => {}
            Some(v) => match v.as_integer() {
                Some(n) if (0..=12).contains(&n) => self.precision = Some(n as u8),
                _ => warn(
                    "precision",
                    format!("'precision' must be an integer 0–12 (got {v})"),
                ),
            },
        }
        match table.get("thousands") {
            None => {}
            Some(v) => match v.as_bool() {
                Some(b) => self.thousands = Some(b),
                None => warn(
                    "thousands",
                    format!("'thousands' must be true or false (got {v})"),
                ),
            },
        }
        match table.get("negative").and_then(|v| v.as_str()) {
            None if table.get("negative").is_none() => {}
            Some("minus") => self.negative = Some(Negative::Minus),
            Some("parens") => self.negative = Some(Negative::Parens),
            other => warn(
                "negative",
                format!("'negative' must be \"minus\" or \"parens\" (got {other:?})"),
            ),
        }
        // `none` and `sign` are built in. Other strings name entries in
        // `colors.toml`; `config::load_views` checks those references because
        // this reader has no color document. `colour` is the old spelling of
        // the key: read when `color` is absent, ignored beside it, and warned
        // about either way so the file gets fixed (dialog writes say `color`).
        let color = match (table.get("color"), table.get(LEGACY_COLOR_KEY)) {
            (Some(v), None) => Some(v),
            (Some(v), Some(_)) => {
                warn(
                    LEGACY_COLOR_KEY,
                    "'colour' is ignored beside 'color' — delete it".into(),
                );
                Some(v)
            }
            (None, Some(v)) => {
                warn(
                    LEGACY_COLOR_KEY,
                    "'colour' is the old spelling — read as 'color'; rename the key".into(),
                );
                Some(v)
            }
            (None, None) => None,
        };
        if let Some(v) = color {
            match v.as_str() {
                Some("none") => self.colour = Some(Colour::None),
                Some("sign") => self.colour = Some(Colour::Sign),
                Some(name) => self.colour = Some(Colour::Named(name.to_string())),
                None => warn("color", format!("'color' must be a string (got {v})")),
            }
        }
        match table.get("scale").and_then(|v| v.as_str()) {
            None if table.get("scale").is_none() => {}
            Some("none") => self.scale = Some(Scale::None),
            Some("k") => self.scale = Some(Scale::Thousands),
            Some("M") => self.scale = Some(Scale::Millions),
            other => warn(
                "scale",
                format!("'scale' must be \"none\", \"k\" or \"M\" (got {other:?})"),
            ),
        }
    }

    /// `label`, `width` (and `hidden` when `read_hidden`) — from a
    /// column's table. The views reader passes `read_hidden: false`
    /// (`hidden` is never a view's own key — only the overlay sets it);
    /// `ViewPresentationSpec::from_doc` passes `true`.
    pub fn parse_column_keys(
        &mut self,
        table: &toml::Table,
        read_hidden: bool,
        warn: &dyn Fn(&str, String),
    ) {
        if let Some(l) = table.get("label") {
            match l.as_str() {
                Some(s) => self.label = Some(s.to_string()),
                None => warn("label", "'label' must be a string".into()),
            }
        }
        if let Some(w) = table.get("width") {
            match w.as_float().or_else(|| w.as_integer().map(|i| i as f64)) {
                Some(x) if x > 0.0 => self.width = Some(x as f32),
                _ => warn(
                    "width",
                    format!("'width' must be a positive number (got {w})"),
                ),
            }
        }
        if read_hidden && let Some(h) = table.get("hidden") {
            match h.as_bool() {
                Some(b) => self.hidden = Some(b),
                None => warn(
                    "hidden",
                    format!("'hidden' must be true or false (got {h})"),
                ),
            }
        }
    }

    /// Merge `other` over `self` per property: each `Some` replaces the
    /// existing value, and each `None` preserves it.
    pub fn merge_over(&mut self, other: &ColumnPresentation) {
        if other.precision.is_some() {
            self.precision = other.precision;
        }
        if other.thousands.is_some() {
            self.thousands = other.thousands;
        }
        if other.negative.is_some() {
            self.negative = other.negative;
        }
        if other.colour.is_some() {
            self.colour = other.colour.clone();
        }
        if other.scale.is_some() {
            self.scale = other.scale;
        }
        if other.label.is_some() {
            self.label = other.label.clone();
        }
        if other.width.is_some() {
            self.width = other.width;
        }
        if other.hidden.is_some() {
            self.hidden = other.hidden;
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ViewSpec {
    pub name: String,
    pub dataset: String,
    pub joins: Vec<JoinSpec>,
    pub columns: Vec<ViewColumn>,
    /// Ordered: each prefix is one level of the rollup tree.
    pub grouping: Vec<String>,
    pub sort: Vec<SortKey>,
    pub presentation: BTreeMap<String, ColumnPresentation>,
    /// Set by a top-level `default = "<name>"` string when the named view
    /// exists. At most one parsed view is marked. Without a string default,
    /// `from_doc` sorts views by name for deterministic fallback selection.
    pub is_default: bool,
}

impl ViewSpec {
    /// Distinct grains of the measures this view selects, coarse first.
    /// The compiler emits one aggregate subquery per grain.
    pub fn measure_grains(&self, schema: &SchemaSpec) -> Vec<Grain> {
        let Some(ds) = schema.dataset(&self.dataset) else {
            return Vec::new();
        };
        let mut out: Vec<Grain> = self
            .columns
            .iter()
            .filter_map(|c| match c {
                ViewColumn::Measure { name, .. } => ds.column(name),
                _ => None,
            })
            .filter(|c| matches!(c.role, ColumnRole::Measure { .. }))
            .filter_map(|c| c.grain())
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The presentation declared for a column, or the empty one.
    pub fn presentation_of(&self, column: &str) -> ColumnPresentation {
        self.presentation.get(column).cloned().unwrap_or_default()
    }

    /// Check that every reference the compiler will resolve can be resolved:
    /// the dataset and each join's dataset exist, each join's keys are carried
    /// by some grain of the dataset it joins and named by the grouping that
    /// puts them on the spine, each selected column exists, a column declared
    /// a measure is a measure of the primary dataset, a column
    /// declared a dimension is reachable through the grouping or a join, and
    /// each grouping column exists. Derived dimensions resolve through their
    /// source column in the primary dataset. Derived SQL and sort keys are
    /// still left to the compiler.
    pub fn validate(&self, schema: &SchemaSpec, dims: &DerivedDimensions) -> Vec<Diagnostic> {
        let mut diags = Vec::new();
        // Addressed to the view, so a dialog can open the object the refusal
        // is about. The message names the column or join within it; carrying a
        // deeper path would mean threading an index through every check here,
        // and the object is enough to reach the place to edit.
        let at = || Some(format!("views.{}", self.name));
        let bad = |m: String| Diagnostic {
            severity: Severity::Error,
            layer: None,
            file: None,
            message: format!("view '{}': {m}", self.name),
            path: at(),
        };
        let warn = |m: String| Diagnostic {
            severity: Severity::Warning,
            layer: None,
            file: None,
            message: format!("view '{}': {m}", self.name),
            path: at(),
        };
        // A required declaration that cannot be honoured refuses the view; an
        // optional one is dropped and said so. The author chose which, so
        // nothing here guesses per kind, and neither case is silent: an
        // optional drop the trader never hears about is the blank column this
        // check exists to remove.
        let report = |required: bool, message: String| {
            if required {
                bad(message)
            } else {
                warn(format!(
                    "{message}; dropped because it is declared optional"
                ))
            }
        };

        let Some(ds) = schema.dataset(&self.dataset) else {
            diags.push(bad(format!("unknown dataset '{}'", self.dataset)));
            return diags;
        };

        for j in &self.joins {
            let Some(joined) = schema.dataset(&j.dataset) else {
                // No second diagnostic about the same line: the keys of a
                // dataset that does not exist cannot be judged. Optional like
                // every other join failure — a views document shared across
                // desks may name a reference dataset only some of them load,
                // and the compiler drops such a join rather than erroring.
                diags.push(report(
                    j.required,
                    format!("join names unknown dataset '{}'", j.dataset),
                ));
                continue;
            };
            // The compiler joins at the first grain of the joined dataset
            // that carries every key. With no such grain there is no table to
            // read, so it drops the join silently and every column the join
            // was to supply is absent from the row rather than NULL.
            let keys =
                j.on.iter()
                    .map(|k| dims.base_column(k))
                    .collect::<Vec<&str>>();
            if !joined
                .grains()
                .into_iter()
                .any(|g| keys.iter().all(|k| joined.carries(g, k)))
            {
                diags.push(report(
                    j.required,
                    format!(
                        "join on dataset '{}' names keys {:?} that no declared grain of it carries",
                        j.dataset, j.on
                    ),
                ));
                // One diagnostic per join: a join no grain can serve is
                // unhonourable whatever the grouping says.
                continue;
            }
            // The join runs only at depths whose spine groups by every key, so
            // a key the grouping never mentions puts it on the spine at no
            // depth at all. A key the grouping does mention but below a
            // query's bound is a depth fact, not a configuration error: the
            // rolled-up row genuinely has no value for it there.
            let Some(ungrouped) = j.on.iter().find(|k| !self.grouping.contains(k)) else {
                continue;
            };
            // Only the columns the compiler would take off this join: a
            // measure must come from the primary dataset, and a grouped name
            // is supplied by the spine itself.
            let starved: Vec<&str> = self
                .columns
                .iter()
                .filter(|c| matches!(c, ViewColumn::Dimension { .. }))
                .map(|c| c.name())
                .filter(|name| {
                    !self.grouping.iter().any(|g| g == name) && joined.column(name).is_some()
                })
                .collect();
            if starved.is_empty() {
                // Dead weight rather than wrong data, so it warns whether or
                // not the join is required: no column reads it, and a join
                // nobody is told about is a join nobody fixes.
                diags.push(warn(format!(
                    "join on dataset '{}' keys on '{ungrouped}', which the grouping does not \
                     include, so the join is never performed; it supplies no column of this view",
                    j.dataset
                )));
            } else {
                diags.push(report(
                    j.required,
                    format!(
                        "join on dataset '{}' keys on '{ungrouped}', which the grouping does not \
                         include, so the join is never performed and column{} {} can never be \
                         supplied",
                        j.dataset,
                        if starved.len() == 1 { "" } else { "s" },
                        starved
                            .iter()
                            .map(|c| format!("'{c}'"))
                            .collect::<Vec<String>>()
                            .join(", ")
                    ),
                ));
            }
        }

        // Derived SQL is checked by the compiler. Other columns may come from
        // the primary dataset or a join; a derived dimension must resolve to a
        // source column in the primary dataset.
        for c in &self.columns {
            match c {
                ViewColumn::Derived { .. } => {}
                other => {
                    let name = other.name();
                    if let Some(d) = dims.get(name) {
                        if ds.column(&d.from).is_none() {
                            diags.push(bad(format!(
                                "column '{name}' is derived from '{}', which dataset '{}' does not have",
                                d.from, self.dataset
                            )));
                        }
                        continue;
                    }
                    if ds.column(name).is_none()
                        && !self.joins.iter().any(|j| {
                            schema
                                .dataset(&j.dataset)
                                .is_some_and(|d| d.column(name).is_some())
                        })
                    {
                        // One diagnostic per column: the role and reachability
                        // rules below have nothing to say about a name that
                        // exists nowhere.
                        diags.push(bad(format!("unknown column '{name}'")));
                        continue;
                    }
                    // A measure is read from the primary dataset alone, and
                    // aggregated by its declared role. A column with any other
                    // role reaches `sum` by default, and an attribute repeats
                    // across the rows of its grain, so the total looks right
                    // and is not.
                    if let ViewColumn::Measure { required, .. } = other
                        && !ds
                            .column(name)
                            .is_some_and(|c| matches!(c.role, ColumnRole::Measure { .. }))
                    {
                        diags.push(report(
                            *required,
                            format!(
                                "column '{name}' is declared a measure, but dataset '{}' \
                                 declares no measure '{name}'",
                                self.dataset
                            ),
                        ));
                    }
                    // A dimension reaches the row only by being grouped or by
                    // coming off a join; the spine selects nothing else. Judged
                    // against the view's own grouping, not a query's bounded
                    // depth: a grouping column below `max_depth` is legitimately
                    // absent at that depth and is not a configuration error.
                    if let ViewColumn::Dimension { required, .. } = other
                        && !self.grouping.iter().any(|g| g == name)
                        && !self.joins.iter().any(|j| {
                            schema
                                .dataset(&j.dataset)
                                .is_some_and(|d| d.column(name).is_some())
                        })
                    {
                        diags.push(report(
                            *required,
                            format!(
                                "column '{name}' is declared a dimension, but it is neither \
                                 in the grouping nor carried by a join, so no row supplies it"
                            ),
                        ));
                    }
                }
            }
        }

        for g in &self.grouping {
            // A derived dimension that shadows a real column is worse than
            // an unknown one: the group-by resolves to whichever the
            // compiler reaches first, so the answer is quietly the wrong
            // one rather than absent. Reported here, at load, because
            // nothing downstream can tell which was meant.
            if let Some(d) = dims.get(g) {
                if ds.column(g).is_some() {
                    diags.push(bad(format!(
                        "grouping '{g}' shadows a real column of dataset '{}': \
                         it is also a derived dimension from '{}'. Rename one.",
                        self.dataset, d.from
                    )));
                } else if ds.column(&d.from).is_none() {
                    diags.push(bad(format!(
                        "grouping '{g}' is derived from '{}', which dataset '{}' does not have",
                        d.from, self.dataset
                    )));
                }
                continue;
            }
            if ds.column(g).is_none() {
                diags.push(bad(format!("grouping names unknown column '{g}'")));
            }
        }

        diags
    }

    pub fn from_doc(doc: &MergedDoc) -> (Vec<ViewSpec>, Vec<Diagnostic>) {
        let mut out = Vec::new();
        let mut diags = Vec::new();
        // A string `default` selects a view; a table named `default` is itself
        // a view. Diagnose other value types once here, then skip them in the
        // object loop so they do not also produce a misleading view error.
        let default_value = doc.value.get("default");
        let default_name = default_value.and_then(|v| v.as_str()).map(str::to_string);
        if let Some(v) = default_value
            && v.as_str().is_none()
            && v.as_table().is_none()
        {
            diags.push(Diagnostic {
                severity: Severity::Warning,
                layer: None,
                file: None,
                message: "top-level 'default' must be a string naming a view".to_string(),
                path: None,
            });
        }

        for (name, value) in &doc.value {
            if name == "config_version" {
                continue;
            }
            if name == "default" && value.as_table().is_none() {
                continue;
            }
            // The object-level path (`views.<name>`) with an optional
            // field suffix appended — every diagnostic below names the
            // deepest key it honestly knows, so a reader that cannot tell
            // which field misbehaved (e.g. "not a table") stays on the
            // object as a whole rather than guessing.
            let at = |suffix: &str| {
                if suffix.is_empty() {
                    format!("views.{name}")
                } else {
                    format!("views.{name}.{suffix}")
                }
            };
            let bad = |suffix: &str, m: String| Diagnostic {
                severity: Severity::Warning,
                layer: None,
                file: None,
                message: format!("view '{name}': {m}"),
                path: Some(at(suffix)),
            };
            let Some(table) = value.as_table() else {
                diags.push(bad("", "not a table".into()));
                continue;
            };

            let mut view = ViewSpec {
                name: name.clone(),
                dataset: table
                    .get("dataset")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                ..ViewSpec::default()
            };
            if view.dataset.is_empty() {
                diags.push(bad("dataset", "missing 'dataset'".into()));
            }

            view.grouping = table
                .get("grouping")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();

            if let Some(joins) = table.get("joins").and_then(|v| v.as_array()) {
                for j in joins.iter().filter_map(|v| v.as_table()) {
                    let Some(dataset) = j.get("dataset").and_then(|v| v.as_str()) else {
                        diags.push(bad("joins", "join missing 'dataset'".into()));
                        continue;
                    };
                    view.joins.push(JoinSpec {
                        dataset: dataset.to_string(),
                        on: j
                            .get("on")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str())
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default(),
                        required: j.get("required").and_then(|v| v.as_bool()).unwrap_or(true),
                    });
                }
            }

            if let Some(cols) = table.get("columns").and_then(|v| v.as_array()) {
                // Enumerate before skipping non-tables: diagnostic indices must match
                // the original array positions, including malformed entries.
                for (i, c) in cols.iter().enumerate() {
                    let Some(c) = c.as_table() else { continue };
                    let Some(col_name) = c.get("name").and_then(|v| v.as_str()) else {
                        diags.push(bad(
                            &format!("columns.{i}.name"),
                            "column missing 'name'".into(),
                        ));
                        continue;
                    };
                    let kind = c.get("kind").and_then(|v| v.as_str()).unwrap_or("measure");
                    let required = c.get("required").and_then(|v| v.as_bool()).unwrap_or(true);
                    let column = match kind {
                        "dimension" => ViewColumn::Dimension {
                            name: col_name.to_string(),
                            required,
                        },
                        "measure" => ViewColumn::Measure {
                            name: col_name.to_string(),
                            required,
                        },
                        "derived" => match c.get("sql").and_then(|v| v.as_str()) {
                            Some(sql) => ViewColumn::Derived {
                                name: col_name.to_string(),
                                sql: sql.to_string(),
                                required,
                            },
                            None => {
                                diags.push(bad(
                                    &format!("columns.{i}.sql"),
                                    format!("derived column '{col_name}' has no 'sql'"),
                                ));
                                continue;
                            }
                        },
                        other => {
                            diags.push(bad(
                                &format!("columns.{i}.kind"),
                                format!("column '{col_name}' has unknown kind '{other}'"),
                            ));
                            continue;
                        }
                    };
                    view.columns.push(column);

                    let mut p = ColumnPresentation::default();
                    // Preserve the original column index in every presentation diagnostic.
                    // The local `RefCell` lets both shared `Fn` callbacks collect warnings
                    // without taking competing mutable borrows of `diags`.
                    let col_diags = RefCell::new(Vec::new());
                    let warn = |key: &str, m: String| {
                        col_diags.borrow_mut().push(bad(
                            &format!("columns.{i}.{key}"),
                            format!("column '{col_name}': {m}"),
                        ));
                    };
                    if let Some(f) = c.get("format") {
                        match f.as_table() {
                            None => warn("format", "'format' is not a table".into()),
                            Some(f) => {
                                p.parse_format_keys(f, &|key, m| warn(&format!("format.{key}"), m))
                            }
                        }
                    }
                    p.parse_column_keys(c, false, &warn);
                    diags.extend(col_diags.into_inner());
                    if p != ColumnPresentation::default() {
                        view.presentation.insert(col_name.to_string(), p);
                    }
                }
            }

            if let Some(sorts) = table.get("sort").and_then(|v| v.as_array()) {
                for s in sorts.iter().filter_map(|v| v.as_table()) {
                    let Some(column) = s.get("column").and_then(|v| v.as_str()) else {
                        diags.push(bad("sort", "sort entry missing 'column'".into()));
                        continue;
                    };
                    view.sort.push(SortKey {
                        column: column.to_string(),
                        descending: s
                            .get("descending")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                    });
                }
            }

            out.push(view);
        }

        // Without a string default, sort by name for deterministic fallback.
        // An explicit name preserves file order, even if the name is unknown;
        // in that case no view is marked and a warning is returned.
        match &default_name {
            Some(default_name) => match out.iter_mut().find(|v| &v.name == default_name) {
                Some(v) => v.is_default = true,
                None => diags.push(Diagnostic {
                    severity: Severity::Warning,
                    layer: None,
                    file: None,
                    message: format!("default view '{default_name}' does not exist"),
                    path: None,
                }),
            },
            None => out.sort_by(|a, b| a.name.cmp(&b.name)),
        }

        (out, diags)
    }
}

/// A view's personal column order and display properties.
///
/// Kept in a separate document so a width, color, or visibility edit does
/// not replace the view's dataset and column definitions. Columns added to
/// the desk view remain available after applying a personal overlay.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ViewPresentation {
    /// Column names, most significant first. Names the view lacks are
    /// warned about and skipped; columns the order omits keep their file
    /// order behind the ones it names (`preserve_order` is on
    /// workspace-wide, so a view's file order is meaningful).
    pub order: Vec<String>,
    /// Per-column display properties, including `hidden`. The reader folds
    /// the supported top-level `hidden` array and `width` map into this map.
    /// An explicitly set column-table property wins over its older spelling,
    /// with a warning when both supply a value.
    pub columns: BTreeMap<String, ColumnPresentation>,
    /// The spelling that first created a column entry from `hidden` or
    /// `width`. `apply` uses it to report an unknown column at a key the file
    /// actually contains. Entries created by a column table are absent here;
    /// when both older spellings name a column, the first one is retained.
    pub legacy_keys: BTreeMap<String, &'static str>,
}

/// `view_presentation.toml`, user layer — one table per view name,
/// atomic at depth one exactly as `views` is
/// (`config::merge::atomic_depth`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ViewPresentationSpec {
    pub views: BTreeMap<String, ViewPresentation>,
}

impl ViewPresentationSpec {
    pub fn from_doc(doc: &MergedDoc) -> (ViewPresentationSpec, Vec<Diagnostic>) {
        let mut spec = ViewPresentationSpec::default();
        let mut diags = Vec::new();

        for (view_name, value) in &doc.value {
            // Every config doc carries this header by convention; it is
            // not a view name.
            if view_name == "config_version" {
                continue;
            }
            // `view_presentation.<view>[.<suffix>]` — the same object-plus-
            // suffix shape as `ViewSpec::from_doc`'s own `at`/`bad` above.
            let at = |suffix: &str| {
                if suffix.is_empty() {
                    format!("view_presentation.{view_name}")
                } else {
                    format!("view_presentation.{view_name}.{suffix}")
                }
            };
            let bad = |suffix: &str, m: String| Diagnostic {
                severity: Severity::Warning,
                layer: None,
                file: None,
                message: format!("view presentation '{view_name}': {m}"),
                path: Some(at(suffix)),
            };
            let Some(table) = value.as_table() else {
                diags.push(bad("", "not a table".into()));
                continue;
            };

            let mut p = ViewPresentation::default();
            if let Some(v) = table.get("order") {
                match v.as_array() {
                    Some(a) => {
                        for item in a {
                            match item.as_str() {
                                Some(s) => p.order.push(s.to_string()),
                                None => diags.push(bad(
                                    "order",
                                    format!("'order' entry is not a string: {item}"),
                                )),
                            }
                        }
                    }
                    None => diags.push(bad(
                        "order",
                        "'order' must be an array of column names".into(),
                    )),
                }
            }
            // The table is read FIRST, before either legacy spelling
            // below, so the conflict rule ("a column set in both wins by
            // the table") can see what the table already holds — folding
            // the legacy keys in ahead of it would have nothing to lose
            // to.
            if let Some(v) = table.get("columns") {
                match v.as_table() {
                    Some(cols) => {
                        for (col, cv) in cols {
                            let Some(ct) = cv.as_table() else {
                                diags.push(bad(
                                    &format!("columns.{col}"),
                                    format!("column '{col}': not a table"),
                                ));
                                continue;
                            };
                            let mut cp = ColumnPresentation::default();
                            // Shared with `parse_column_keys`'s `warn`
                            // below (see the reader's own `col_diags` for
                            // why this collects rather than pushing
                            // straight into `diags`): a `[view.columns.
                            // <col>]` table has no nested `format`
                            // sub-table — its keys sit at the column
                            // table's own top level — so, unlike the
                            // views reader, nothing here prefixes `key`.
                            let col_diags = RefCell::new(Vec::new());
                            let warn = |key: &str, m: String| {
                                col_diags.borrow_mut().push(bad(
                                    &format!("columns.{col}.{key}"),
                                    format!("column '{col}': {m}"),
                                ));
                            };
                            cp.parse_format_keys(ct, &warn);
                            cp.parse_column_keys(ct, true, &warn);
                            diags.extend(col_diags.into_inner());
                            p.columns.insert(col.clone(), cp);
                        }
                    }
                    None => diags.push(bad(
                        "columns",
                        "'columns' must be a table of column tables".into(),
                    )),
                }
            }
            if let Some(v) = table.get("hidden") {
                match v.as_array() {
                    Some(a) => {
                        for item in a {
                            match item.as_str() {
                                Some(col) => {
                                    let is_new = !p.columns.contains_key(col);
                                    let entry = p.columns.entry(col.to_string()).or_default();
                                    if entry.hidden.is_some() {
                                        diags.push(bad(
                                            &format!("columns.{col}.hidden"),
                                            format!(
                                                "column '{col}': 'hidden' is also set in the legacy 'hidden' array — the table wins"
                                            ),
                                        ));
                                        continue;
                                    }
                                    entry.hidden = Some(true);
                                    if is_new {
                                        // Remember the source spelling so unknown-column warnings point to
                                        // a key present in the file.
                                        p.legacy_keys.insert(col.to_string(), "hidden");
                                    }
                                }
                                None => diags.push(bad(
                                    "hidden",
                                    format!("'hidden' entry is not a string: {item}"),
                                )),
                            }
                        }
                    }
                    None => diags.push(bad(
                        "hidden",
                        "'hidden' must be an array of column names".into(),
                    )),
                }
            }
            if let Some(v) = table.get("width") {
                match v.as_table() {
                    Some(t) => {
                        for (col, w) in t {
                            if p.columns.get(col).is_some_and(|e| e.width.is_some()) {
                                diags.push(bad(
                                    &format!("columns.{col}.width"),
                                    format!(
                                        "column '{col}': 'width' is also set in the legacy 'width' map — the table wins"
                                    ),
                                ));
                                continue;
                            }
                            // Validate before creating an entry. An invalid width must not leave
                            // an empty column entry that produces a second unknown-column warning
                            // when the overlay is applied.
                            match w.as_float().or_else(|| w.as_integer().map(|i| i as f64)) {
                                Some(x) if x > 0.0 => {
                                    let is_new = !p.columns.contains_key(col);
                                    let entry = p.columns.entry(col.clone()).or_default();
                                    entry.width = Some(x as f32);
                                    if is_new {
                                        p.legacy_keys.insert(col.clone(), "width");
                                    }
                                }
                                _ => diags.push(bad(
                                    &format!("width.{col}"),
                                    format!(
                                        "column '{col}': 'width' must be a positive number (got {w})"
                                    ),
                                )),
                            }
                        }
                    }
                    None => diags.push(bad(
                        "width",
                        "'width' must be a table of column widths".into(),
                    )),
                }
            }

            spec.views.insert(view_name.clone(), p);
        }

        (spec, diags)
    }

    /// Apply order and per-column properties in place after the named-object
    /// merge. Unknown views, unknown columns, and duplicate order entries
    /// produce warnings and are ignored; valid entries still apply.
    pub fn apply(&self, views: &mut [ViewSpec]) -> Vec<Diagnostic> {
        let mut diags = Vec::new();
        for (view_name, p) in &self.views {
            let at = |suffix: &str| {
                if suffix.is_empty() {
                    format!("view_presentation.{view_name}")
                } else {
                    format!("view_presentation.{view_name}.{suffix}")
                }
            };
            let warn = |suffix: &str, m: String| Diagnostic {
                severity: Severity::Warning,
                layer: None,
                file: None,
                message: format!("view presentation '{view_name}': {m}"),
                path: Some(at(suffix)),
            };
            let Some(view) = views.iter_mut().find(|v| &v.name == view_name) else {
                diags.push(warn("", "no view of that name — ignored".into()));
                continue;
            };

            // Order first: `hidden`/`width` only touch presentation, so
            // they neither depend on nor disturb the column sequence.
            let mut taken = vec![false; view.columns.len()];
            let mut permutation: Vec<usize> = Vec::with_capacity(view.columns.len());
            for name in &p.order {
                match view.columns.iter().position(|c| c.name() == name) {
                    Some(i) if !taken[i] => {
                        taken[i] = true;
                        permutation.push(i);
                    }
                    Some(_) => diags.push(warn(
                        "order",
                        format!(
                            "column '{name}' is listed twice in 'order' — the repeat is ignored"
                        ),
                    )),
                    None => diags.push(warn(
                        "order",
                        format!(
                            "'order' names column '{name}', which the view does not have — ignored"
                        ),
                    )),
                }
            }
            // A column the order omits is not dropped: it keeps its file
            // order behind the ones named, so a desk adding a column
            // still reaches a trader whose personal order predates it.
            permutation.extend((0..view.columns.len()).filter(|i| !taken[*i]));
            let mut slots: Vec<Option<ViewColumn>> = view.columns.drain(..).map(Some).collect();
            view.columns = permutation
                .into_iter()
                .map(|i| slots[i].take().expect("each index appears once"))
                .collect();

            // Apply each column once; `from_doc` has already folded the supported
            // `hidden` and `width` spellings into this map.
            for (col, cp) in &p.columns {
                if !view.columns.iter().any(|c| c.name() == col) {
                    // Report the spelling used in the file, including `hidden` or `width`,
                    // so a warning does not point to a nonexistent column table.
                    let (suffix, key) = match p.legacy_keys.get(col).copied() {
                        Some("hidden") => ("hidden".to_string(), "hidden"),
                        Some("width") => (format!("width.{col}"), "width"),
                        _ => (format!("columns.{col}"), "columns"),
                    };
                    diags.push(warn(
                        &suffix,
                        format!(
                            "'{key}' names column '{col}', which the view does not have — ignored"
                        ),
                    ));
                    continue;
                }
                view.presentation
                    .entry(col.clone())
                    .or_default()
                    .merge_over(cp);
            }
        }
        diags
    }
}

/// Dataset-wide column display properties from `dataset_presentation.toml`.
/// Named dataset objects replace whole across configuration layers. Each
/// `[<dataset>.columns.<col>]` table accepts label, width, and format keys;
/// `hidden` and `order` belong in `view_presentation.toml` and warn here.
/// Per-property precedence is kind default, view definition, dataset
/// presentation, then view presentation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DatasetPresentationSpec {
    /// dataset → column → presentation
    pub datasets: BTreeMap<String, BTreeMap<String, ColumnPresentation>>,
}

pub const DATASET_PRESENTATION_DOC: &str = "dataset_presentation";

impl DatasetPresentationSpec {
    pub fn from_doc(doc: &MergedDoc) -> (DatasetPresentationSpec, Vec<Diagnostic>) {
        let mut spec = DatasetPresentationSpec::default();
        let mut diags = Vec::new();
        for (dataset, value) in &doc.value {
            if dataset == "config_version" {
                continue;
            }
            let at = |suffix: &str| {
                if suffix.is_empty() {
                    format!("{DATASET_PRESENTATION_DOC}.{dataset}")
                } else {
                    format!("{DATASET_PRESENTATION_DOC}.{dataset}.{suffix}")
                }
            };
            let bad = |suffix: &str, m: String| Diagnostic {
                severity: Severity::Warning,
                layer: None,
                file: None,
                message: format!("dataset presentation '{dataset}': {m}"),
                path: Some(at(suffix)),
            };
            let Some(table) = value.as_table() else {
                diags.push(bad("", "not a table".into()));
                continue;
            };
            let mut columns = BTreeMap::new();
            for (key, v) in table {
                match key.as_str() {
                    "columns" => {
                        let Some(cols) = v.as_table() else {
                            diags.push(bad(
                                "columns",
                                "'columns' must be a table of column tables".into(),
                            ));
                            continue;
                        };
                        for (col, cv) in cols {
                            let Some(ct) = cv.as_table() else {
                                diags.push(bad(
                                    &format!("columns.{col}"),
                                    format!("column '{col}': not a table"),
                                ));
                                continue;
                            };
                            let mut cp = ColumnPresentation::default();
                            let col_diags = RefCell::new(Vec::new());
                            let warn = |k: &str, m: String| {
                                col_diags.borrow_mut().push(bad(
                                    &format!("columns.{col}.{k}"),
                                    format!("column '{col}': {m}"),
                                ));
                            };
                            cp.parse_format_keys(ct, &warn);
                            // `read_hidden: false`: the dataset overlay
                            // never carries membership.
                            cp.parse_column_keys(ct, false, &warn);
                            if ct.contains_key("hidden") {
                                warn(
                                    "hidden",
                                    "'hidden' belongs to a view — set it in view_presentation.toml"
                                        .into(),
                                );
                            }
                            // Warn on unknown column keys as well as malformed known values.
                            // The old `colour` spelling has its own warning in
                            // `parse_format_keys`; `hidden` has its own warning above.
                            const COLUMN_KEYS: [&str; 9] = [
                                "label",
                                "width",
                                "scale",
                                "precision",
                                "thousands",
                                "negative",
                                "color",
                                LEGACY_COLOR_KEY,
                                "hidden",
                            ];
                            for k in ct.keys() {
                                if !COLUMN_KEYS.contains(&k.as_str()) {
                                    warn(k, format!("unknown key '{k}' — ignored"));
                                }
                            }
                            diags.extend(col_diags.into_inner());
                            columns.insert(col.clone(), cp);
                        }
                    }
                    "order" => diags.push(bad(
                        "order",
                        "'order' belongs to a view — set it in view_presentation.toml".into(),
                    )),
                    other => diags.push(bad(other, format!("unknown key '{other}' — ignored"))),
                }
            }
            spec.datasets.insert(dataset.clone(), columns);
        }
        (spec, diags)
    }

    /// The first dataset declaring `column`: the primary dataset, then
    /// joins in declaration order, matching compiler name resolution.
    /// Returns `None` when none declares the name.
    pub fn owner_of<'a>(view: &'a ViewSpec, column: &str, schema: &SchemaSpec) -> Option<&'a str> {
        std::iter::once(view.dataset.as_str())
            .chain(view.joins.iter().map(|j| j.dataset.as_str()))
            .find(|ds| {
                schema
                    .dataset(ds)
                    .is_some_and(|d| d.column(column).is_some())
            })
    }

    /// Apply each dataset's properties to selected columns it owns.
    /// `load_views` calls this after parsing view definitions and before
    /// applying view presentation. Unknown datasets and columns warn and
    /// are skipped; ownership uses `owner_of` to resolve duplicate names.
    pub fn apply(&self, views: &mut [ViewSpec], schema: &SchemaSpec) -> Vec<Diagnostic> {
        let mut diags = Vec::new();
        let warn = |path: String, m: String| Diagnostic {
            severity: Severity::Warning,
            layer: None,
            file: None,
            message: m,
            path: Some(path),
        };
        for (dataset, columns) in &self.datasets {
            let Some(spec) = schema.dataset(dataset) else {
                diags.push(warn(
                    format!("{DATASET_PRESENTATION_DOC}.{dataset}"),
                    format!("dataset presentation '{dataset}': names dataset '{dataset}', which no schema declares — ignored"),
                ));
                continue;
            };
            for (col, cp) in columns {
                if spec.column(col).is_none() {
                    diags.push(warn(
                        format!("{DATASET_PRESENTATION_DOC}.{dataset}.columns.{col}"),
                        format!("dataset presentation '{dataset}': names column '{col}', which dataset '{dataset}' does not have — ignored"),
                    ));
                    continue;
                }
                for view in views.iter_mut() {
                    let owned_here = view.columns.iter().any(|c| c.name() == col)
                        && Self::owner_of(view, col, schema) == Some(dataset.as_str());
                    if owned_here {
                        view.presentation
                            .entry(col.clone())
                            .or_default()
                            .merge_over(cp);
                    }
                }
            }
        }
        diags
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};
    use crate::schema::SchemaSpec;

    fn doc(text: &str) -> crate::config::MergedDoc {
        merge_docs("views", &[LayerDoc::builtin("views", text).unwrap()])
    }

    fn dimensions(text: &str) -> DerivedDimensions {
        let doc = merge_docs(
            "dimensions",
            &[LayerDoc::builtin("dimensions", text).unwrap()],
        );
        DerivedDimensions::from_doc(&doc).0
    }

    const SAMPLE: &str = r#"
[desk_risk]
dataset = "risk_snapshot"
grouping = ["book", "lhu", "position_ref"]

[[desk_risk.joins]]
dataset = "instrument_ref"
on = ["instrument_ref"]

[[desk_risk.columns]]
name = "book"
kind = "dimension"

[[desk_risk.columns]]
name = "delta01"
kind = "measure"

[[desk_risk.columns]]
name = "delta_per_vega"
kind = "derived"
sql = "delta01 / nullif(vega01, 0)"

[[desk_risk.sort]]
column = "delta01"
descending = true
"#;

    fn schema() -> SchemaSpec {
        let text = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.lhu]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.counterparty]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.instrument_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
[risk_snapshot.columns.vega01]
type = "f64"
role = "measure"
grain = "underlying"
[risk_snapshot.columns.desk_name]
type = "utf8"
role = "attribute"
grain = "underlying"
[instrument_ref.columns.instrument_ref]
type = "utf8"
role = "key"
[instrument_ref.columns.strike]
type = "f64"
role = "attribute"
grain = "instrument"
"#;
        let d = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&d).0
    }

    #[test]
    fn parses_a_view_with_joins_columns_grouping_and_sort() {
        let (views, diags) = ViewSpec::from_doc(&doc(SAMPLE));
        assert!(diags.is_empty(), "{diags:?}");
        let v = views.iter().find(|v| v.name == "desk_risk").expect("view");
        assert_eq!(v.dataset, "risk_snapshot");
        assert_eq!(v.grouping, vec!["book", "lhu", "position_ref"]);
        assert_eq!(v.joins.len(), 1);
        assert_eq!(v.joins[0].dataset, "instrument_ref");
        assert_eq!(v.joins[0].on, vec!["instrument_ref"]);
        assert_eq!(
            v.sort,
            vec![SortKey {
                column: "delta01".into(),
                descending: true
            }]
        );
    }

    #[test]
    fn required_defaults_true_and_is_read_from_a_join_and_a_column() {
        let text = r#"
[risk]
dataset = "risk_snapshot"
grouping = ["book"]

[[risk.joins]]
dataset = "instrument_ref"
on = ["instrument_ref"]

[[risk.joins]]
dataset = "optional_ref"
on = ["instrument_ref"]
required = false

[[risk.columns]]
name = "delta01"

[[risk.columns]]
name = "maybe_missing"
required = false
"#;
        let (views, _) = ViewSpec::from_doc(&merge_docs(
            "views",
            &[LayerDoc::builtin("views", text).unwrap()],
        ));
        let v = views.iter().find(|v| v.name == "risk").expect("risk");
        assert!(v.joins[0].required, "a join defaults to required");
        assert!(!v.joins[1].required, "required = false is read");
        assert!(v.columns[0].required(), "a column defaults to required");
        assert!(!v.columns[1].required(), "required = false is read");
    }

    #[test]
    fn a_view_config_version_header_is_not_a_spurious_diagnostic() {
        // Every config doc carries this header by convention
        // (`groupings.toml`'s own `GroupingSlots::from_doc` already
        // skips it) — it must not be treated as a malformed view.
        let (views, diags) = ViewSpec::from_doc(&doc(&format!("config_version = 1\n{SAMPLE}")));
        assert!(diags.is_empty(), "{diags:?}");
        assert!(views.iter().any(|v| v.name == "desk_risk"));
    }

    #[test]
    fn views_with_no_default_key_come_out_sorted_by_name() {
        // File order is deliberately the reverse of name order to verify the
        // deterministic fallback when no default is named.
        let text = r#"
[b]
dataset = "risk_snapshot"

[a]
dataset = "risk_snapshot"
"#;
        let (views, diags) = ViewSpec::from_doc(&doc(text));
        assert!(diags.is_empty(), "{diags:?}");
        let names: Vec<&str> = views.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert!(views.iter().all(|v| !v.is_default));
    }

    #[test]
    fn a_top_level_default_key_flags_that_view_and_leaves_file_order_alone() {
        let text = r#"
default = "b"

[b]
dataset = "risk_snapshot"

[a]
dataset = "risk_snapshot"
"#;
        let (views, diags) = ViewSpec::from_doc(&doc(text));
        assert!(diags.is_empty(), "{diags:?}");
        let b = views.iter().find(|v| v.name == "b").expect("view b");
        assert!(b.is_default);
        let a = views.iter().find(|v| v.name == "a").expect("view a");
        assert!(!a.is_default);
    }

    #[test]
    fn a_view_literally_named_default_is_not_silently_dropped() {
        // A table named `default` is an ordinary view, not the string header.
        let text = r#"
[default]
dataset = "risk_snapshot"
"#;
        let (views, diags) = ViewSpec::from_doc(&doc(text));
        assert!(diags.is_empty(), "{diags:?}");
        assert!(
            views.iter().any(|v| v.name == "default"),
            "a view named 'default' must still become a ViewSpec: {views:?}"
        );
    }

    #[test]
    fn a_non_string_default_value_warns_instead_of_being_silently_ignored() {
        // A malformed default header warns once and leaves name-sorted fallback.
        let text = r#"
default = 3

[a]
dataset = "risk_snapshot"
"#;
        let (views, diags) = ViewSpec::from_doc(&doc(text));
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("default")),
            "a non-string `default` must warn: {diags:?}"
        );
        assert!(views.iter().all(|v| !v.is_default));
    }

    #[test]
    fn distinguishes_the_three_column_kinds() {
        let (views, _) = ViewSpec::from_doc(&doc(SAMPLE));
        let v = &views[0];
        assert_eq!(
            v.columns,
            vec![
                ViewColumn::dimension("book"),
                ViewColumn::measure("delta01"),
                ViewColumn::derived("delta_per_vega", "delta01 / nullif(vega01, 0)"),
            ]
        );
    }

    #[test]
    fn a_derived_column_without_sql_is_a_diagnostic() {
        let (_v, diags) = ViewSpec::from_doc(&doc(
            "[v]\ndataset = \"d\"\n[[v.columns]]\nname = \"x\"\nkind = \"derived\"\n",
        ));
        assert!(diags.iter().any(|d| d.message.contains("sql")), "{diags:?}");
    }

    #[test]
    fn validation_catches_unknown_columns_grouping_and_datasets() {
        let (views, _) = ViewSpec::from_doc(&doc(
            "[v]\ndataset = \"nosuch\"\ngrouping = [\"nocolumn\"]\n",
        ));
        let diags = views[0].validate(&schema(), &DerivedDimensions::default());
        assert!(
            diags.iter().any(|d| d.message.contains("nosuch")),
            "{diags:?}"
        );
    }

    /// A join key no grain of the joined dataset carries cannot be honoured:
    /// the compiler finds no grain to read and drops the whole join, so every
    /// column it was to supply paints blank.
    #[test]
    fn a_join_whose_keys_no_grain_carries_refuses_the_view() {
        let text = r#"
[v]
dataset = "risk_snapshot"
grouping = ["book"]

[[v.joins]]
dataset = "instrument_ref"
on = ["strike"]

[[v.columns]]
name = "book"
kind = "dimension"
"#;
        let (views, _) = ViewSpec::from_doc(&doc(text));
        let diags = views[0].validate(&schema(), &dimensions(""));
        let errors: Vec<&Diagnostic> = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert!(
            errors[0].message.contains("strike") && errors[0].message.contains("instrument_ref"),
            "the diagnostic must name the key and the dataset: {}",
            errors[0].message
        );
    }

    /// A views document shared across desks may name a reference dataset only
    /// some of them load. Declaring the join optional drops it, as it does for
    /// every other join failure, and the compiler drops it too.
    #[test]
    fn an_optional_join_naming_an_unknown_dataset_is_dropped_not_refused() {
        let text = r#"
[v]
dataset = "risk_snapshot"
grouping = ["book"]

[[v.joins]]
dataset = "no_such_dataset"
on = ["book"]
required = false

[[v.columns]]
name = "book"
kind = "dimension"
"#;
        let (views, _) = ViewSpec::from_doc(&doc(text));
        let diags = views[0].validate(&schema(), &dimensions(""));
        assert!(
            !diags.iter().any(|d| d.severity == Severity::Error),
            "an optional join must not refuse the view: {diags:?}"
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(
            diags[0].message.contains("no_such_dataset") && diags[0].message.contains("dropped"),
            "the diagnostic must name the dataset and say it was dropped: {}",
            diags[0].message
        );
    }

    /// The same join declared optional is dropped and said so, never refused.
    #[test]
    fn an_optional_join_whose_keys_no_grain_carries_is_dropped_with_a_warning() {
        let text = r#"
[v]
dataset = "risk_snapshot"
grouping = ["book"]

[[v.joins]]
dataset = "instrument_ref"
on = ["strike"]
required = false

[[v.columns]]
name = "book"
kind = "dimension"
"#;
        let (views, _) = ViewSpec::from_doc(&doc(text));
        let diags = views[0].validate(&schema(), &dimensions(""));
        assert!(
            !diags.iter().any(|d| d.severity == Severity::Error),
            "an optional join must not refuse the view: {diags:?}"
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(
            diags[0].message.contains("strike")
                && diags[0].message.contains("instrument_ref")
                && diags[0].message.contains("dropped"),
            "the diagnostic must name the join and say it was dropped: {}",
            diags[0].message
        );
    }

    /// An attribute repeats across every row of its grain, so summing it
    /// yields a total that looks right and is not. This is the reason the
    /// `kind` default of "measure" is the dangerous one.
    #[test]
    fn a_measure_column_over_an_attribute_refuses_the_view() {
        let text = r#"
[v]
dataset = "risk_snapshot"
grouping = ["book"]

[[v.columns]]
name = "desk_name"
kind = "measure"
"#;
        let (views, _) = ViewSpec::from_doc(&doc(text));
        let diags = views[0].validate(&schema(), &dimensions(""));
        let errors: Vec<&Diagnostic> = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert!(
            errors[0].message.contains("desk_name"),
            "the diagnostic must name the column: {}",
            errors[0].message
        );
    }

    /// Selected nowhere: the spine carries only grouping columns, aggregates,
    /// joins and derived expressions, so this column is absent, not NULL.
    #[test]
    fn a_dimension_column_neither_grouped_nor_joined_refuses_the_view() {
        let text = r#"
[v]
dataset = "risk_snapshot"
grouping = ["book"]

[[v.columns]]
name = "counterparty"
kind = "dimension"
"#;
        let (views, _) = ViewSpec::from_doc(&doc(text));
        let diags = views[0].validate(&schema(), &dimensions(""));
        let errors: Vec<&Diagnostic> = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert!(
            errors[0].message.contains("counterparty"),
            "the diagnostic must name the column: {}",
            errors[0].message
        );
    }

    /// The opt-out on a column, with the drop named.
    #[test]
    fn an_optional_unreachable_column_is_dropped_with_a_warning_naming_it() {
        let text = r#"
[v]
dataset = "risk_snapshot"
grouping = ["book"]

[[v.columns]]
name = "counterparty"
kind = "dimension"
required = false
"#;
        let (views, _) = ViewSpec::from_doc(&doc(text));
        let diags = views[0].validate(&schema(), &dimensions(""));
        assert!(
            !diags.iter().any(|d| d.severity == Severity::Error),
            "an optional column must not refuse the view: {diags:?}"
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(
            diags[0].message.contains("counterparty") && diags[0].message.contains("dropped"),
            "the diagnostic must name the column and say it was dropped: {}",
            diags[0].message
        );
    }

    /// A join keyed outside the grouping cannot be performed at any depth, and
    /// every column it was to supply paints blank for the life of the view.
    #[test]
    fn a_join_keyed_outside_the_grouping_refuses_the_view_when_it_supplies_a_column() {
        let text = r#"
[v]
dataset = "risk_snapshot"
grouping = ["book"]

[[v.joins]]
dataset = "instrument_ref"
on = ["instrument_ref"]

[[v.columns]]
name = "strike"
kind = "dimension"
"#;
        let (views, _) = ViewSpec::from_doc(&doc(text));
        let diags = views[0].validate(&schema(), &dimensions(""));
        let errors: Vec<&Diagnostic> = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert!(
            errors[0].message.contains("instrument_ref") && errors[0].message.contains("strike"),
            "the diagnostic must name the join, the ungrouped key and the starved column: {}",
            errors[0].message
        );
    }

    /// The same join supplying nothing is dead weight, not wrong data: it is
    /// named so it can be removed, and it refuses nothing.
    #[test]
    fn a_join_keyed_outside_the_grouping_that_supplies_nothing_only_warns() {
        let text = r#"
[v]
dataset = "risk_snapshot"
grouping = ["book"]

[[v.joins]]
dataset = "instrument_ref"
on = ["instrument_ref"]

[[v.columns]]
name = "delta01"
kind = "measure"
"#;
        let (views, _) = ViewSpec::from_doc(&doc(text));
        let diags = views[0].validate(&schema(), &dimensions(""));
        assert!(
            !diags.iter().any(|d| d.severity == Severity::Error),
            "a join that supplies no column must not refuse the view: {diags:?}"
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(
            diags[0].message.contains("instrument_ref")
                && diags[0].message.contains("supplies no column"),
            "the diagnostic must name the join and say it supplies nothing: {}",
            diags[0].message
        );
    }

    /// The opt-out on a join whose columns are starved.
    #[test]
    fn an_optional_join_keyed_outside_the_grouping_is_dropped_with_a_warning() {
        let text = r#"
[v]
dataset = "risk_snapshot"
grouping = ["book"]

[[v.joins]]
dataset = "instrument_ref"
on = ["instrument_ref"]
required = false

[[v.columns]]
name = "strike"
kind = "dimension"
"#;
        let (views, _) = ViewSpec::from_doc(&doc(text));
        let diags = views[0].validate(&schema(), &dimensions(""));
        assert!(
            !diags.iter().any(|d| d.severity == Severity::Error),
            "an optional join must not refuse the view: {diags:?}"
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(
            diags[0].message.contains("strike") && diags[0].message.contains("dropped"),
            "the diagnostic must name the starved column and say it was dropped: {}",
            diags[0].message
        );
    }

    #[test]
    fn a_well_formed_view_validates_clean() {
        let (views, _) = ViewSpec::from_doc(&doc(SAMPLE));
        let diags = views[0].validate(&schema(), &DerivedDimensions::default());
        // `desk_risk` joins `instrument_ref` on a key its grouping does not
        // include and selects no column from it: dead weight, which warns and
        // refuses nothing. Nothing about the view is unhonourable.
        assert!(
            !diags.iter().any(|d| d.severity == Severity::Error),
            "{diags:?}"
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(
            diags[0].message.contains("supplies no column of this view"),
            "{}",
            diags[0].message
        );
    }

    #[test]
    fn grouping_by_a_derived_dimension_is_not_an_unknown_column() {
        // A derived dimension resolves through its source column during validation.
        let dims = dimensions("[desk]\nfrom = \"book\"\n[desk.values]\nBK000 = \"Flow\"\n");
        let (views, _) = ViewSpec::from_doc(&doc(
            "[v]\ndataset = \"risk_snapshot\"\ngrouping = [\"desk\"]\n\
             [[v.columns]]\nname = \"delta01\"\nkind = \"measure\"\n",
        ));
        let diags = views[0].validate(&schema(), &dims);
        assert!(
            diags.is_empty(),
            "a derived grouping is not an unknown column: {diags:?}"
        );
    }

    #[test]
    fn a_derived_dimension_is_accepted_as_a_column_not_only_as_a_grouping() {
        // Validate a derived dimension in both selected columns and grouping.
        let dims = dimensions("[desk]\nfrom = \"book\"\n[desk.values]\nBK000 = \"Flow\"\n");
        let (views, _) = ViewSpec::from_doc(&doc(
            "[v]\ndataset = \"risk_snapshot\"\ngrouping = [\"desk\"]\n\
             [[v.columns]]\nname = \"desk\"\nkind = \"dimension\"\n\
             [[v.columns]]\nname = \"delta01\"\nkind = \"measure\"\n",
        ));
        let diags = views[0].validate(&schema(), &dims);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn a_derived_column_whose_source_is_absent_is_reported() {
        let dims = dimensions("[desk]\nfrom = \"nosuch\"\n[desk.values]\nX = \"Flow\"\n");
        let (views, _) = ViewSpec::from_doc(&doc("[v]\ndataset = \"risk_snapshot\"\n\
             [[v.columns]]\nname = \"desk\"\nkind = \"dimension\"\n"));
        let diags = views[0].validate(&schema(), &dims);
        assert!(
            diags.iter().any(|d| d.message.contains("nosuch")),
            "{diags:?}"
        );
    }

    #[test]
    fn a_derived_dimension_that_shadows_a_real_column_is_reported() {
        // Accepted silently, the group-by then runs against whichever the
        // compiler resolves first — the derived mapping or the column of
        // the same name in the data. Both are plausible and only one is
        // meant, so the answer is quietly wrong rather than absent.
        let dims = dimensions("[book]\nfrom = \"lhu\"\n[book.values]\nL0 = \"Flow\"\n");
        let (views, _) = ViewSpec::from_doc(&doc(
            "[v]\ndataset = \"risk_snapshot\"\ngrouping = [\"book\"]\n\
             [[v.columns]]\nname = \"delta01\"\nkind = \"measure\"\n",
        ));
        let diags = views[0].validate(&schema(), &dims);
        assert!(
            diags.iter().any(|d| d.message.contains("shadows")),
            "expected a shadowing diagnostic, got {diags:?}"
        );
    }

    #[test]
    fn measure_grains_reports_the_distinct_grains_the_view_touches() {
        // Each distinct measure grain requires one aggregate subquery.
        let (views, _) = ViewSpec::from_doc(&doc(SAMPLE));
        let ds = schema();
        assert_eq!(
            views[0].measure_grains(&ds),
            vec![crate::schema::Grain::Underlying]
        );
    }

    #[test]
    fn presentation_is_parsed_per_column_and_defaults_are_per_kind() {
        let (views, diags) = ViewSpec::from_doc(&doc(r#"
[tree]
dataset = "risk_snapshot"
grouping = ["lhu"]
[[tree.columns]]
name = "npv"
format = { precision = 0, thousands = true, negative = "parens", color = "sign", scale = "k" }
label = "NPV"
width = 110
[[tree.columns]]
name = "delta01"
format = { precision = 4 }
[[tree.columns]]
name = "lhu"
kind = "dimension"
"#));
        assert!(diags.is_empty(), "{diags:?}");
        let v = &views[0];
        let npv = v.presentation_of("npv");
        assert_eq!(npv.label.as_deref(), Some("NPV"));
        assert_eq!(npv.width, Some(110.0));
        let f = ColumnFormat::MEASURE.with(&npv);
        assert_eq!(f.precision, 0);
        assert!(f.thousands);
        assert_eq!(f.negative, Negative::Parens);
        assert_eq!(f.colour, Colour::Sign);
        assert_eq!(f.scale, Scale::Thousands);
        assert_eq!(Scale::Thousands.divisor(), 1_000.0);
        assert_eq!(Scale::Millions.divisor(), 1_000_000.0);
        assert_eq!(Scale::None.divisor(), 1.0);
        assert_eq!(Scale::Thousands.suffix(), "k");
        assert_eq!(Scale::Millions.suffix(), "M");
        assert_eq!(Scale::None.suffix(), "");

        let d = ColumnFormat::MEASURE.with(&v.presentation_of("delta01"));
        assert_eq!(d.precision, 4, "one field overrides, the rest default");
        assert!(d.thousands);
        assert_eq!(d.negative, Negative::Minus);
        assert_eq!(d.scale, Scale::None, "unscaled by default");

        let l = ColumnFormat::TEXT.with(&v.presentation_of("lhu"));
        assert_eq!(l.colour, Colour::None);
        assert_eq!(v.presentation_of("nonesuch"), ColumnPresentation::default());
    }

    #[test]
    fn bad_presentation_values_warn_and_are_ignored() {
        let (views, diags) = ViewSpec::from_doc(&doc(r#"
[tree]
dataset = "risk_snapshot"
grouping = ["lhu"]
[[tree.columns]]
name = "npv"
format = { precision = 40, negative = "red", color = 42, thousands = "yes", scale = "bn" }
width = -5
"#));
        let p = views[0].presentation_of("npv");
        assert_eq!(p, ColumnPresentation::default(), "{p:?}");
        assert_eq!(diags.len(), 6, "{diags:?}");
        assert!(diags.iter().all(|d| d.message.contains("npv")));
    }

    #[test]
    fn color_is_accepted_as_a_spelling_of_colour() {
        let (views, diags) = ViewSpec::from_doc(&doc(r#"
[tree]
dataset = "risk_snapshot"
grouping = ["lhu"]
[[tree.columns]]
name = "npv"
format = { color = "none" }
"#));
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(views[0].presentation_of("npv").colour, Some(Colour::None));
    }

    /// Files written before the rename still say `colour`: the value is read,
    /// and a warning at the old key tells the author to rename it.
    #[test]
    fn the_old_colour_key_is_read_with_a_warning_naming_color() {
        let (views, diags) = ViewSpec::from_doc(&doc(r#"
[tree]
dataset = "risk_snapshot"
grouping = ["lhu"]
[[tree.columns]]
name = "npv"
format = { colour = "sign" }
"#));
        assert_eq!(views[0].presentation_of("npv").colour, Some(Colour::Sign));
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].severity, Severity::Warning);
        assert!(
            diags[0]
                .path
                .as_deref()
                .is_some_and(|p| p.ends_with("format.colour")),
            "{diags:?}"
        );
        assert!(diags[0].message.contains("'color'"), "{}", diags[0].message);
    }

    /// Beside `color`, the old key is ignored — never merged over it — and
    /// warned about once, not also reported as an unknown key.
    #[test]
    fn color_wins_over_the_old_colour_key() {
        let (spec, diags) = DatasetPresentationSpec::from_doc(&dataset_doc(
            "[risk.columns.npv]\ncolor = \"sign\"\ncolour = \"none\"\n",
        ));
        assert_eq!(spec.datasets["risk"]["npv"].colour, Some(Colour::Sign));
        let paths: Vec<&str> = diags.iter().filter_map(|d| d.path.as_deref()).collect();
        assert_eq!(
            paths,
            vec!["dataset_presentation.risk.columns.npv.colour"],
            "{diags:?}"
        );
        assert!(diags[0].message.contains("ignored"), "{}", diags[0].message);
    }

    #[test]
    fn a_column_colour_may_name_a_named_colour() {
        let doc = merge_docs(
            "views",
            &[
                LayerDoc::builtin(
                    "views",
                    "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\nformat = { color = \"delta\" }\n",
                )
                .unwrap(),
            ],
        );
        let (views, diags) = ViewSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(
            views[0].presentation_of("npv").colour,
            Some(Colour::Named("delta".to_string()))
        );
    }
    /// Presentation lives in its own doc, so its merged form must be
    /// built under its own name — `atomic_depth` keys off the doc name,
    /// and a per-view table is replaced whole exactly as `views` is.
    fn presentation_doc(text: &str) -> crate::config::MergedDoc {
        merge_docs(
            "view_presentation",
            &[LayerDoc::builtin("view_presentation", text).unwrap()],
        )
    }

    #[test]
    fn presentation_reads_order_hidden_and_width_per_view() {
        let doc = presentation_doc(
            r#"
config_version = 1
[tree]
order = ["book", "npv", "delta01"]
hidden = ["cross_gamma02"]
[tree.width]
npv = 120
"#,
        );
        let (spec, diags) = ViewPresentationSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        let tree = spec.views.get("tree").expect("tree");
        assert_eq!(tree.order, vec!["book", "npv", "delta01"]);
        assert_eq!(tree.columns["cross_gamma02"].hidden, Some(true));
        assert_eq!(tree.columns["npv"].width, Some(120.0));
    }

    fn overlay(text: &str) -> (ViewPresentationSpec, Vec<Diagnostic>) {
        let doc = merge_docs(
            "view_presentation",
            &[LayerDoc::builtin("view_presentation", text).unwrap()],
        );
        ViewPresentationSpec::from_doc(&doc)
    }

    #[test]
    fn the_overlay_reads_a_column_table_with_every_presentation_key() {
        let (spec, diags) = overlay(
            "[tree]\norder = [\"npv\"]\n[tree.columns.npv]\nscale = \"k\"\nprecision = 0\nthousands = false\nnegative = \"parens\"\ncolor = \"delta\"\nlabel = \"NPV\"\nwidth = 120\nhidden = true\n",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let p = &spec.views["tree"].columns["npv"];
        assert_eq!(p.scale, Some(Scale::Thousands));
        assert_eq!(p.precision, Some(0));
        assert_eq!(p.thousands, Some(false));
        assert_eq!(p.negative, Some(Negative::Parens));
        assert_eq!(p.colour, Some(Colour::Named("delta".to_string())));
        assert_eq!(p.label.as_deref(), Some("NPV"));
        assert_eq!(p.width, Some(120.0));
        assert_eq!(p.hidden, Some(true));
        assert_eq!(spec.views["tree"].order, vec!["npv".to_string()]);
    }

    #[test]
    fn the_legacy_hidden_and_width_keys_still_load_and_the_table_wins_a_conflict() {
        let (spec, diags) = overlay(
            "[tree]\nhidden = [\"book\"]\n[tree.width]\nnpv = 140\nbook = 90\n[tree.columns.npv]\nwidth = 120\n",
        );
        let cols = &spec.views["tree"].columns;
        assert_eq!(cols["book"].hidden, Some(true));
        assert_eq!(cols["book"].width, Some(90.0));
        assert_eq!(cols["npv"].width, Some(120.0), "the table wins");
        assert!(
            diags.iter().any(|d| d.path.as_deref()
                == Some("view_presentation.tree.columns.npv.width")
                && d.message.contains("table wins")),
            "{diags:?}"
        );
    }

    #[test]
    fn apply_merges_a_column_table_over_the_view() {
        let views_doc = merge_docs("views", &[LayerDoc::builtin("views",
            "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\nformat = { scale = \"k\", precision = 2 }\n").unwrap()]);
        let (mut views, _) = ViewSpec::from_doc(&views_doc);
        let (spec, _) = overlay("[tree.columns.npv]\nprecision = 0\ncolor = \"delta\"\n");
        let diags = spec.apply(&mut views);
        assert!(diags.is_empty(), "{diags:?}");
        let p = views[0].presentation_of("npv");
        assert_eq!(p.scale, Some(Scale::Thousands), "the desk's key survives");
        assert_eq!(p.precision, Some(0), "the trader's key wins");
        assert_eq!(p.colour, Some(Colour::Named("delta".to_string())));
    }

    /// An invalid width leaves no overlay entry. Applying that overlay must
    /// not produce an additional unknown-column warning.
    #[test]
    fn an_invalid_legacy_width_on_an_unknown_column_is_one_diagnostic() {
        let (mut views, _) = ViewSpec::from_doc(&doc("[tree]\ndataset = \"risk_snapshot\"\n\
             [[tree.columns]]\nname = \"npv\"\nkind = \"measure\"\n"));
        let (pres, diags) =
            ViewPresentationSpec::from_doc(&presentation_doc("[tree.width]\nghost = 0\n"));
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(
            diags[0].message.contains("must be a positive number"),
            "{diags:?}"
        );
        assert!(
            !pres.views["tree"].columns.contains_key("ghost"),
            "a refused width must leave no column entry behind"
        );
        let warnings = pres.apply(&mut views);
        assert!(
            warnings.is_empty(),
            "apply must not warn a second time about a column the reader \
             already refused: {warnings:?}"
        );
    }

    /// Unknown-column warnings preserve the spelling from the file, whether
    /// it used a column table, the hidden array, or the width map.
    #[test]
    fn the_column_not_in_view_warning_names_the_spelling_it_was_read_under() {
        let view = "[tree]\ndataset = \"risk_snapshot\"\n\
                    [[tree.columns]]\nname = \"npv\"\nkind = \"measure\"\n";
        let case = |text: &str| {
            let (mut views, _) = ViewSpec::from_doc(&doc(view));
            let (pres, diags) = ViewPresentationSpec::from_doc(&presentation_doc(text));
            assert!(diags.is_empty(), "{diags:?}");
            let warnings = pres.apply(&mut views);
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            (
                warnings[0].message.clone(),
                warnings[0].path.clone().unwrap_or_default(),
            )
        };

        let (message, path) = case("[tree]\nhidden = [\"ghost\"]\n");
        assert!(
            message.contains("'hidden' names column 'ghost'"),
            "{message}"
        );
        assert_eq!(path, "view_presentation.tree.hidden");

        let (message, path) = case("[tree.width]\nghost = 90\n");
        assert!(
            message.contains("'width' names column 'ghost'"),
            "{message}"
        );
        assert_eq!(path, "view_presentation.tree.width.ghost");

        let (message, path) = case("[tree.columns.ghost]\nhidden = true\n");
        assert!(
            message.contains("'columns' names column 'ghost'"),
            "{message}"
        );
        assert_eq!(path, "view_presentation.tree.columns.ghost");
    }

    /// A desk that renames a column must not break a personal file. The
    /// name is warned about and ignored, never an error.
    #[test]
    fn a_column_the_view_lacks_is_a_warning_not_an_error() {
        let (mut views, _) = ViewSpec::from_doc(&doc("[tree]\ndataset = \"risk_snapshot\"\n\
             [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
             [[tree.columns]]\nname = \"npv\"\nkind = \"measure\"\n"));
        let before = views[0].columns.clone();
        let (pres, diags) = ViewPresentationSpec::from_doc(&presentation_doc(
            "[tree]\norder = [\"gone\"]\nhidden = [\"gone\"]\n[tree.width]\ngone = 90\n",
        ));
        assert!(diags.is_empty(), "{diags:?}");

        let warnings = pres.apply(&mut views);
        assert!(
            !warnings.is_empty() && warnings.iter().all(|d| d.severity == Severity::Warning),
            "an unknown column is a warning, never an error: {warnings:?}"
        );
        assert!(
            warnings.iter().all(|d| d.message.contains("gone")),
            "{warnings:?}"
        );
        assert_eq!(
            views[0].columns, before,
            "the view's own columns are untouched"
        );
        assert!(
            !views[0].presentation.contains_key("gone"),
            "a name the view lacks invents no column presentation"
        );
    }
    /// A personal overlay for an unknown view is skipped with a warning,
    /// explaining why its settings do not apply. Other views remain unchanged.
    #[test]
    fn a_view_the_config_lacks_is_a_warning_not_an_error() {
        let (mut views, _) = ViewSpec::from_doc(&doc("[tree]\ndataset = \"risk_snapshot\"\n\
             [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
             [[tree.columns]]\nname = \"npv\"\nkind = \"measure\"\n"));
        let columns_before = views[0].columns.clone();
        let presentation_before = views[0].presentation.clone();

        let (pres, diags) = ViewPresentationSpec::from_doc(&presentation_doc(
            "[retired_view]\norder = [\"book\"]\nhidden = [\"npv\"]\n",
        ));
        assert!(diags.is_empty(), "{diags:?}");

        let warnings = pres.apply(&mut views);
        assert_eq!(
            warnings.len(),
            1,
            "one warning for the one stale name: {warnings:?}"
        );
        assert_eq!(
            warnings[0].severity,
            Severity::Warning,
            "a stale view name is a warning, never an error: {warnings:?}"
        );
        assert!(
            warnings[0].message.contains("retired_view"),
            "the warning must name the view it skipped: {warnings:?}"
        );
        assert_eq!(views.len(), 1, "no view is invented for the stale name");
        assert_eq!(
            views[0].columns, columns_before,
            "the real view's columns are untouched"
        );
        assert_eq!(
            views[0].presentation, presentation_before,
            "the real view's presentation is untouched"
        );
    }

    #[test]
    fn a_column_format_diagnostic_carries_its_indexed_path() {
        let doc = merge_docs(
            "views",
            &[LayerDoc::builtin(
                "views",
                "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n\
                 [[tree.columns]]\nname = \"delta\"\nformat = { precision = 99 }\n",
            )
            .unwrap()],
        );
        let (_, diags) = ViewSpec::from_doc(&doc);
        assert_eq!(
            diags[0].path.as_deref(),
            Some("views.tree.columns.1.format.precision"),
            "{diags:?}"
        );
        assert!(
            diags[0]
                .to_string()
                .ends_with(" (at views.tree.columns.1.format.precision)")
        );
    }

    #[test]
    fn a_missing_dataset_diagnostic_carries_its_field_path() {
        let (_, diags) = ViewSpec::from_doc(&doc("[tree]\n"));
        assert_eq!(
            diags[0].path.as_deref(),
            Some("views.tree.dataset"),
            "{diags:?}"
        );
    }

    #[test]
    fn a_presentation_order_diagnostic_carries_its_field_path() {
        let (mut views, _) = ViewSpec::from_doc(&doc(
            "[tree]\ndataset = \"risk_snapshot\"\n[[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n",
        ));
        let (pres, _) = ViewPresentationSpec::from_doc(&presentation_doc(
            "[tree]\norder = [\"missing_col\"]\n",
        ));
        let diags = pres.apply(&mut views);
        assert_eq!(
            diags[0].path.as_deref(),
            Some("view_presentation.tree.order"),
            "{diags:?}"
        );
    }

    fn dataset_doc(text: &str) -> crate::config::MergedDoc {
        merge_docs(
            "dataset_presentation",
            &[LayerDoc::builtin("dataset_presentation", text).unwrap()],
        )
    }

    #[test]
    fn dataset_presentation_reads_one_table_per_column_with_paths() {
        let (spec, diags) = DatasetPresentationSpec::from_doc(&dataset_doc(
            "[risk.columns.delta01]\nlabel = \"Δ\"\nwidth = 90\nscale = \"k\"\nprecision = 0\n\
             thousands = true\nnegative = \"parens\"\ncolor = \"delta\"\n\
             [risk.columns.npv]\nwidth = \"wide\"\n",
        ));
        let delta = &spec.datasets["risk"]["delta01"];
        assert_eq!(delta.label.as_deref(), Some("Δ"));
        assert_eq!(delta.width, Some(90.0));
        assert_eq!(delta.scale, Some(Scale::Thousands));
        assert_eq!(delta.precision, Some(0));
        assert_eq!(delta.thousands, Some(true));
        assert_eq!(delta.negative, Some(Negative::Parens));
        assert_eq!(delta.colour, Some(Colour::Named("delta".into())));
        assert_eq!(
            delta.hidden, None,
            "the dataset overlay never carries hidden"
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(
            diags[0].path.as_deref(),
            Some("dataset_presentation.risk.columns.npv.width")
        );
        assert!(
            spec.datasets["risk"].contains_key("npv"),
            "a bad key skips the key, not the column"
        );
    }

    #[test]
    fn dataset_presentation_refuses_hidden_and_order_naming_the_view_overlay() {
        let (spec, diags) = DatasetPresentationSpec::from_doc(&dataset_doc(
            "[risk]\norder = [\"npv\"]\n[risk.columns.delta01]\nhidden = true\nscale = \"k\"\n",
        ));
        assert_eq!(
            spec.datasets["risk"]["delta01"].scale,
            Some(Scale::Thousands)
        );
        assert_eq!(spec.datasets["risk"]["delta01"].hidden, None);
        let messages: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(diags.len(), 2, "{messages:?}");
        assert!(
            messages
                .iter()
                .all(|m| m.contains("view_presentation.toml")),
            "{messages:?}"
        );
        assert_eq!(
            diags[0].path.as_deref(),
            Some("dataset_presentation.risk.order")
        );
        assert_eq!(
            diags[1].path.as_deref(),
            Some("dataset_presentation.risk.columns.delta01.hidden")
        );
    }

    /// Unknown column keys warn. `color` is accepted without a warning;
    /// malformed values still use the shared property reader's diagnostics.
    #[test]
    fn dataset_presentation_warns_for_an_unknown_key_inside_a_column_table() {
        let (spec, diags) = DatasetPresentationSpec::from_doc(&dataset_doc(
            "[risk.columns.npv]\nprecison = 3\ncolor = \"sign\"\nwidth = 80\n",
        ));
        assert_eq!(
            spec.datasets["risk"]["npv"].width,
            Some(80.0),
            "an unknown key skips the key, not the column"
        );
        assert_eq!(
            spec.datasets["risk"]["npv"].colour,
            Some(Colour::Sign),
            "`color` is the reader's own alias, not an unknown key"
        );
        let paths: Vec<&str> = diags.iter().filter_map(|d| d.path.as_deref()).collect();
        assert_eq!(
            paths,
            vec!["dataset_presentation.risk.columns.npv.precison"],
            "{diags:?}"
        );
        assert!(
            diags[0]
                .message
                .contains("column 'npv': unknown key 'precison' — ignored"),
            "{}",
            diags[0].message
        );
    }

    #[test]
    fn dataset_presentation_skips_a_non_table_dataset_and_column() {
        let (spec, diags) = DatasetPresentationSpec::from_doc(&dataset_doc(
            "config_version = 1\nrisk = 3\n[vol.columns]\nstrike = \"no\"\n[vol.columns.spot]\nwidth = 80\n",
        ));
        assert!(!spec.datasets.contains_key("risk"));
        assert_eq!(spec.datasets["vol"]["spot"].width, Some(80.0));
        assert!(!spec.datasets["vol"].contains_key("strike"));
        let paths: Vec<&str> = diags.iter().filter_map(|d| d.path.as_deref()).collect();
        assert_eq!(
            paths,
            vec![
                "dataset_presentation.risk",
                "dataset_presentation.vol.columns.strike"
            ]
        );
    }

    fn schema_with(text: &str) -> SchemaSpec {
        SchemaSpec::from_doc(&merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", text).unwrap()],
        ))
        .0
    }

    const RISK_SCHEMA: &str = "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
        [risk.columns.delta01]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
        [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n";

    #[test]
    fn dataset_level_beats_the_desk_view_and_loses_to_the_view_level() {
        let schema = schema_with(RISK_SCHEMA);
        let (mut views, _) = ViewSpec::from_doc(&doc(
            "[tree]\ndataset = \"risk\"\ngrouping = [\"book\"]\n\
             [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
             [[tree.columns]]\nname = \"delta01\"\nkind = \"measure\"\nlabel = \"desk\"\nwidth = 50\n\
             format = { scale = \"m\", precision = 2 }\n",
        ));
        let (dataset, d) = DatasetPresentationSpec::from_doc(&dataset_doc(
            "[risk.columns.delta01]\nlabel = \"dataset\"\nscale = \"k\"\ncolor = \"delta\"\n",
        ));
        assert!(d.is_empty(), "{d:?}");
        assert!(dataset.apply(&mut views, &schema).is_empty());
        let p = views[0].presentation_of("delta01");
        assert_eq!(
            p.label.as_deref(),
            Some("dataset"),
            "dataset beats the desk view"
        );
        assert_eq!(p.scale, Some(Scale::Thousands));
        assert_eq!(p.colour, Some(Colour::Named("delta".into())));
        assert_eq!(p.width, Some(50.0), "an unset dataset key keeps the desk's");
        assert_eq!(p.precision, Some(2));

        let view_doc = merge_docs(
            "view_presentation",
            &[LayerDoc::builtin(
                "view_presentation",
                "[tree.columns.delta01]\nlabel = \"view\"\nscale = \"none\"\n",
            )
            .unwrap()],
        );
        let (view_overlay, _) = ViewPresentationSpec::from_doc(&view_doc);
        assert!(view_overlay.apply(&mut views).is_empty());
        let p = views[0].presentation_of("delta01");
        assert_eq!(
            p.label.as_deref(),
            Some("view"),
            "the view level beats the dataset level"
        );
        assert_eq!(p.scale, Some(Scale::None));
        assert_eq!(
            p.colour,
            Some(Colour::Named("delta".into())),
            "an unset view key keeps the dataset's"
        );
    }

    #[test]
    fn a_joined_column_takes_its_own_datasets_entry_and_the_view_dataset_wins_a_tie() {
        let schema = schema_with(
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
             [risk.columns.instrument_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
             [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
             [ref.columns.instrument_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
             [ref.columns.spot]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"instrument\"\n\
             [ref.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"instrument\"\n",
        );
        let (mut views, _) =
            ViewSpec::from_doc(&doc("[j]\ndataset = \"risk\"\ngrouping = [\"book\"]\n\
             [[j.joins]]\ndataset = \"ref\"\non = [\"instrument_ref\"]\n\
             [[j.columns]]\nname = \"npv\"\nkind = \"measure\"\n\
             [[j.columns]]\nname = \"spot\"\nkind = \"measure\"\n\
             [[j.columns]]\nname = \"vega\"\nkind = \"derived\"\nsql = \"npv * 2\"\n"));
        assert_eq!(
            DatasetPresentationSpec::owner_of(&views[0], "spot", &schema),
            Some("ref")
        );
        assert_eq!(
            DatasetPresentationSpec::owner_of(&views[0], "npv", &schema),
            Some("risk")
        );
        assert_eq!(
            DatasetPresentationSpec::owner_of(&views[0], "vega", &schema),
            None
        );
        let (dataset, _) = DatasetPresentationSpec::from_doc(&dataset_doc(
            "[ref.columns.spot]\nwidth = 70\n[ref.columns.npv]\nwidth = 99\n[risk.columns.npv]\nwidth = 42\n",
        ));
        assert!(dataset.apply(&mut views, &schema).is_empty());
        assert_eq!(
            views[0].presentation_of("spot").width,
            Some(70.0),
            "a joined column takes the join's entry"
        );
        assert_eq!(
            views[0].presentation_of("npv").width,
            Some(42.0),
            "the view's own dataset wins a tie"
        );
    }

    #[test]
    fn an_unknown_dataset_or_column_warns_with_its_path_and_is_skipped() {
        let schema = schema_with(RISK_SCHEMA);
        let (mut views, _) = ViewSpec::from_doc(&doc(
            "[tree]\ndataset = \"risk\"\ngrouping = [\"book\"]\n[[tree.columns]]\nname = \"delta01\"\nkind = \"measure\"\n",
        ));
        let (dataset, _) = DatasetPresentationSpec::from_doc(&dataset_doc(
            "[ghost.columns.x]\nwidth = 1\n[risk.columns.nope]\nwidth = 2\n[risk.columns.delta01]\nwidth = 3\n",
        ));
        let diags = dataset.apply(&mut views, &schema);
        let paths: Vec<&str> = diags.iter().filter_map(|d| d.path.as_deref()).collect();
        assert_eq!(
            paths,
            vec![
                "dataset_presentation.ghost",
                "dataset_presentation.risk.columns.nope"
            ]
        );
        assert_eq!(views[0].presentation_of("delta01").width, Some(3.0));
    }

    /// Strictness is only safe if what we ship is already clean: an error
    /// diagnostic refuses its view, so a shipped view that fails to validate
    /// is a `--demo` that opens onto a blotter which will not load. Every
    /// views document in the repo is checked against its own datasets
    /// document.
    #[test]
    fn every_shipped_views_document_validates_clean() {
        let views_text = include_str!("../../../examples/demo-config/views.toml");
        let datasets_text = include_str!("../../../examples/demo-config/datasets.toml");
        let dims_text = include_str!("../../../examples/demo-config/dimensions.toml");

        let (views, read_diags) = ViewSpec::from_doc(&merge_docs(
            "views",
            &[LayerDoc::builtin("views", views_text).unwrap()],
        ));
        let errors: Vec<&Diagnostic> = read_diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(errors.is_empty(), "reading the shipped views: {errors:?}");

        let (schema, _) = SchemaSpec::from_doc(&merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", datasets_text).unwrap()],
        ));
        let (dims, _) = DerivedDimensions::from_doc(&merge_docs(
            "dimensions",
            &[LayerDoc::builtin("dimensions", dims_text).unwrap()],
        ));

        assert!(!views.is_empty(), "the fixture must actually load views");
        for view in &views {
            let errors: Vec<String> = view
                .validate(&schema, &dims)
                .into_iter()
                .filter(|d| d.severity == Severity::Error)
                .map(|d| d.message)
                .collect();
            assert!(
                errors.is_empty(),
                "shipped view '{}' does not validate: {errors:?}",
                view.name
            );
        }
    }
}
