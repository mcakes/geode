//! The vol watchlist's tabs, `vol_views.toml`: named column lists over the
//! board's measures. Parsing (`from_doc`) knows nothing of the schema and
//! keeps every column whose text reads; resolution (`resolve`) turns an
//! open-form measure (`ssr 3M`) into a board `Measure::Table` against the
//! history datasets the schema declares, and drops what it cannot. A view
//! with no valid column is kept empty, with a warning: a table with only
//! `cells` is a legal empty tab the dialog fills later.

use std::time::Duration;

use crate::config::{Diagnostic, MergedDoc, Severity};
use crate::query::normalize_key;
use crate::schema::ColumnType;
use crate::source_config::parse_duration;
use crate::volboard::Measure;

/// The spark span when a column names none.
pub const DEFAULT_SPARK_SPAN: Duration = Duration::from_secs(30 * 86_400);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cells {
    Pair,
    Single,
}

/// How a measure column's level cell paints. Absent in the file means the
/// view's default: plain in a pair view, the companion's sign tint in a
/// single view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelGlyph {
    Plain,
    Heat,
    Tint,
}

impl LevelGlyph {
    pub fn key(self) -> &'static str {
        match self {
            LevelGlyph::Plain => "plain",
            LevelGlyph::Heat => "heat",
            LevelGlyph::Tint => "tint",
        }
    }

    /// Exact lower-case keys only: the file is written by the dialog or by
    /// hand, and a near miss is reported, not guessed.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "plain" => Some(LevelGlyph::Plain),
            "heat" => Some(LevelGlyph::Heat),
            "tint" => Some(LevelGlyph::Tint),
            _ => None,
        }
    }
}

/// How a measure column's companion cell paints (the pair's companion
/// column, or a single column after `m`). Absent means a chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompanionGlyph {
    Chip,
    Bar,
    Slider,
}

impl CompanionGlyph {
    pub fn key(self) -> &'static str {
        match self {
            CompanionGlyph::Chip => "chip",
            CompanionGlyph::Bar => "bar",
            CompanionGlyph::Slider => "slider",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "chip" => Some(CompanionGlyph::Chip),
            "bar" => Some(CompanionGlyph::Bar),
            "slider" => Some(CompanionGlyph::Slider),
            _ => None,
        }
    }
}

/// The tenors a picker offers until a surface lists its own; the tile's
/// `order_tenors` falls back to these. Mirrors `demo_hanweck::TENORS`.
pub const ALL_TENORS: [&str; 10] = ["1W", "1M", "2M", "3M", "6M", "1Y", "18M", "2Y", "3Y", "5Y"];
/// The call deltas a surface-point picker offers.
pub const DELTAS: [f64; 5] = [0.10, 0.25, 0.50, 0.75, 0.90];
/// The sparkline spans a picker offers; the reader accepts any duration.
pub const SPANS: [&str; 4] = ["7d", "30d", "90d", "1y"];
pub const SPARK_30D: Duration = Duration::from_secs(30 * 86_400);

/// A measure as the document spells it, before the schema is consulted.
#[derive(Debug, Clone, PartialEq)]
pub enum MeasureText {
    Builtin(Measure),
    Table {
        dataset: String,
        suffix: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum VolColumnSpec {
    Measure {
        measure: MeasureText,
        label: Option<String>,
        group: Option<String>,
        level: Option<LevelGlyph>,
        companion: Option<CompanionGlyph>,
    },
    Spark {
        measure: MeasureText,
        span: Duration,
    },
    Vega,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VolViewSpec {
    pub name: String,
    pub cells: Cells,
    pub columns: Vec<VolColumnSpec>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct VolViewsDoc {
    pub views: Vec<VolViewSpec>,
    pub default: Option<String>,
}

/// What the resolver needs of a declared history dataset: its key length
/// (`underlying_ref` included) and its value column count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableShape {
    pub key_len: usize,
    pub values: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VolColumn {
    Measure {
        measure: Measure,
        label: Option<String>,
        group: Option<String>,
        level: Option<LevelGlyph>,
        companion: Option<CompanionGlyph>,
    },
    Spark {
        measure: Measure,
        span: Duration,
    },
    Vega,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VolView {
    pub name: String,
    pub cells: Cells,
    pub columns: Vec<VolColumn>,
}

/// The resolved tabs in document order; `default` indexes `views` and is
/// `0` when `views` is empty.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VolViews {
    pub views: Vec<VolView>,
    pub default: usize,
}

impl VolViews {
    pub fn get(&self, name: &str) -> Option<&VolView> {
        self.views.iter().find(|v| v.name == name)
    }

    pub fn default_view(&self) -> Option<&VolView> {
        self.views.get(self.default)
    }

    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }
}

fn warn(path: String, message: String) -> Diagnostic {
    Diagnostic {
        severity: Severity::Warning,
        layer: None,
        file: None,
        message,
        path: Some(path),
    }
}

/// The keywords of the built-in forms; a dataset by one of these names is
/// unreachable by the open form.
const KEYWORDS: [&str; 10] = [
    "point", "atfm", "forward", "skew", "wing", "slope", "realized", "iv-rv", "varswap", "vs",
];

fn key_text(word: &str, ty: ColumnType) -> String {
    normalize_key(word, ty)
        .ok()
        .unwrap_or_else(|| word.to_string())
}

/// `point <tenor> <delta>`, `atfm <tenor>`, `forward <tenor>`, `skew
/// <tenor>`, `wing skew <tenor>`, `slope <long>-<short>`, `realized
/// <window>`, `iv-rv <tenor>/<window>`, `varswap <tenor>`, `vs spread
/// <tenor>`, else `<dataset> [k1] [k2]`. Keywords read case-insensitively;
/// key texts are normalized as the surface reader normalizes them.
pub fn parse_measure(text: &str) -> Result<MeasureText, String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let Some((&head, rest)) = words.split_first() else {
        return Err("a measure names something".into());
    };
    let head_lc = head.to_ascii_lowercase();
    let one = |what: &str, example: &str| -> Result<String, String> {
        match rest {
            [t] => Ok(key_text(t, ColumnType::Utf8)),
            _ => Err(format!("{what} needs a tenor: `{example}`")),
        }
    };
    let m = match head_lc.as_str() {
        "point" => match rest {
            [tenor, delta] => {
                // The surface key's own normalization: finite, and spelled
                // as the stored key is, so `0.50` reads as the `0.5` point.
                // A NaN or infinite delta would be a kept column that never
                // matches a key and a `Measure` unequal to itself.
                let canonical = normalize_key(delta, ColumnType::F64)
                    .map_err(|_| format!("point: '{delta}' is not a delta"))?;
                let value: f64 = canonical
                    .parse()
                    .map_err(|_| format!("point: '{delta}' is not a delta"))?;
                if !(value > 0.0 && value < 1.0) {
                    return Err(format!("point: '{delta}' is not a delta between 0 and 1"));
                }
                Measure::Point {
                    tenor: key_text(tenor, ColumnType::Utf8),
                    delta: value,
                }
            }
            _ => return Err("point needs a tenor and a delta: `point 3M 0.25`".into()),
        },
        "atfm" => Measure::atfm(&one("atfm", "atfm 3M")?),
        "forward" => Measure::Forward {
            tenor: one("forward", "forward 3M")?,
        },
        "skew" => Measure::Skew {
            tenor: one("skew", "skew 3M")?,
        },
        "wing" => match rest {
            [w, t] if w.eq_ignore_ascii_case("skew") => Measure::WingSkew {
                tenor: key_text(t, ColumnType::Utf8),
            },
            _ => return Err("wing skew needs a tenor: `wing skew 3M`".into()),
        },
        "slope" => match rest {
            [pair] => match pair.split_once('-') {
                Some((long, short)) if !long.is_empty() && !short.is_empty() => Measure::Slope {
                    long: key_text(long, ColumnType::Utf8),
                    short: key_text(short, ColumnType::Utf8),
                },
                _ => return Err("slope needs `<long>-<short>`: `slope 1Y-1M`".into()),
            },
            _ => return Err("slope needs `<long>-<short>`: `slope 1Y-1M`".into()),
        },
        "realized" => Measure::Realized {
            window: one("realized", "realized 20D")?,
        },
        "iv-rv" => match rest {
            [pair] => match pair.split_once('/') {
                Some((tenor, window)) if !tenor.is_empty() && !window.is_empty() => Measure::IvRv {
                    tenor: key_text(tenor, ColumnType::Utf8),
                    window: key_text(window, ColumnType::Utf8),
                },
                _ => return Err("iv-rv needs `<tenor>/<window>`: `iv-rv 3M/20D`".into()),
            },
            _ => return Err("iv-rv needs `<tenor>/<window>`: `iv-rv 3M/20D`".into()),
        },
        "varswap" => Measure::Varswap {
            tenor: one("varswap", "varswap 3M")?,
        },
        "vs" => match rest {
            [s, t] if s.eq_ignore_ascii_case("spread") => Measure::VsSpread {
                tenor: key_text(t, ColumnType::Utf8),
            },
            _ => return Err("vs spread needs a tenor: `vs spread 3M`".into()),
        },
        _ => {
            if rest.len() > 2 {
                return Err("a dataset takes at most two key values after its name".into());
            }
            return Ok(MeasureText::Table {
                dataset: head.to_string(),
                suffix: rest.iter().map(|w| w.to_string()).collect(),
            });
        }
    };
    Ok(MeasureText::Builtin(m))
}

/// The text `parse_measure` reads back to the same measure.
pub fn measure_text(m: &MeasureText) -> String {
    match m {
        MeasureText::Builtin(Measure::Point { tenor, delta }) if *delta == 0.5 => {
            format!("atfm {tenor}")
        }
        MeasureText::Builtin(m) => m.name(),
        MeasureText::Table { dataset, suffix } => {
            let mut s = dataset.clone();
            for k in suffix {
                s.push(' ');
                s.push_str(k);
            }
            s
        }
    }
}

fn read_column(
    value: &toml::Value,
    path: &str,
    view: &str,
    index: usize,
    diags: &mut Vec<Diagnostic>,
) -> Option<VolColumnSpec> {
    let column = |why: String| {
        warn(
            path.to_string(),
            format!("vol_views: {view} column {}: {why}; dropped", index + 1),
        )
    };
    let Some(t) = value.as_table() else {
        diags.push(column("a column is a table".into()));
        return None;
    };
    let text = |k: &str| t.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let set = |k: &str| t.contains_key(k);
    match (set("measure"), set("spark"), set("kind")) {
        (true, false, false) => {
            let Some(measure) = text("measure") else {
                diags.push(column("measure is a string".into()));
                return None;
            };
            let measure = match parse_measure(&measure) {
                Ok(m) => m,
                Err(e) => {
                    diags.push(column(e));
                    return None;
                }
            };
            let level = match t.get("level") {
                None => None,
                Some(v) => match v.as_str().and_then(LevelGlyph::parse) {
                    Some(g) => Some(g),
                    None => {
                        diags.push(column(format!("level = {v} is not plain, heat or tint")));
                        return None;
                    }
                },
            };
            let companion = match t.get("companion") {
                None => None,
                Some(v) => match v.as_str().and_then(CompanionGlyph::parse) {
                    Some(g) => Some(g),
                    None => {
                        diags.push(column(format!(
                            "companion = {v} is not chip, bar or slider"
                        )));
                        return None;
                    }
                },
            };
            Some(VolColumnSpec::Measure {
                measure,
                label: text("label").filter(|s| !s.trim().is_empty()),
                group: text("group").filter(|s| !s.trim().is_empty()),
                level,
                companion,
            })
        }
        (false, true, false) => {
            for key in ["level", "companion"] {
                if set(key) {
                    diags.push(column(format!("{key} applies to a measure column")));
                    return None;
                }
            }
            let Some(measure) = text("spark") else {
                diags.push(column("spark is a string".into()));
                return None;
            };
            let measure = match parse_measure(&measure) {
                Ok(m) => m,
                Err(e) => {
                    diags.push(column(e));
                    return None;
                }
            };
            let span = match t.get("span") {
                None => DEFAULT_SPARK_SPAN,
                Some(v) => match v.as_str().and_then(parse_duration).filter(|d| !d.is_zero()) {
                    Some(d) => d,
                    None => {
                        diags.push(column(format!("span = {v} is not a duration")));
                        return None;
                    }
                },
            };
            Some(VolColumnSpec::Spark { measure, span })
        }
        (false, false, true) => {
            for key in ["level", "companion"] {
                if set(key) {
                    diags.push(column(format!("{key} applies to a measure column")));
                    return None;
                }
            }
            match text("kind").as_deref() {
                Some("vega") => Some(VolColumnSpec::Vega),
                _ => {
                    diags.push(column("kind is \"vega\"".into()));
                    None
                }
            }
        }
        _ => {
            diags.push(column(
                "a column is one of measure, spark or kind = \"vega\"".into(),
            ));
            None
        }
    }
}

/// Views in document order, each a table with a `columns` array; a
/// top-level string `default` names the opening view. Problems warn at
/// their key; a column that does not read drops alone; a view with no
/// valid column is kept empty, with a warning. A second view whose name
/// matches an earlier one case-insensitively is skipped: the tile's
/// `view <name>` lookup and the dialog's name prompt would otherwise hold
/// two tabs a reader cannot tell apart.
pub fn from_doc(doc: &MergedDoc) -> (VolViewsDoc, Vec<Diagnostic>) {
    let mut out = VolViewsDoc::default();
    let mut diags = Vec::new();
    for (name, value) in &doc.value {
        let path = format!("vol_views.{name}");
        if name == "config_version" {
            continue;
        }
        if name == "default" {
            match value.as_str() {
                Some(s) => out.default = Some(s.to_string()),
                None => diags.push(warn(path, "vol_views: default is a view name".into())),
            }
            continue;
        }
        let Some(table) = value.as_table() else {
            diags.push(warn(
                path,
                format!("vol_views: '{name}' must be a table; skipped"),
            ));
            continue;
        };
        let cells = match table.get("cells") {
            None => Cells::Pair,
            Some(v) => match v.as_str() {
                Some("pair") => Cells::Pair,
                Some("single") => Cells::Single,
                _ => {
                    diags.push(warn(
                        format!("{path}.cells"),
                        format!(
                            "vol_views: {name}: cells = {v} is not pair or single; reading pair"
                        ),
                    ));
                    Cells::Pair
                }
            },
        };
        if out.views.iter().any(|v| v.name.eq_ignore_ascii_case(name)) {
            diags.push(warn(
                path,
                format!("vol_views: '{name}' is defined twice; the second is skipped"),
            ));
            continue;
        }
        let columns: Vec<VolColumnSpec> = match table.get("columns").and_then(|v| v.as_array()) {
            None => Vec::new(),
            Some(columns) => columns
                .iter()
                .enumerate()
                .filter_map(|(i, v)| {
                    read_column(v, &format!("{path}.columns.{i}"), name, i, &mut diags)
                })
                .collect(),
        };
        if columns.is_empty() {
            diags.push(warn(path, format!("vol_views: '{name}' has no columns")));
        }
        out.views.push(VolViewSpec {
            name: name.clone(),
            cells,
            columns,
        });
    }
    (out, diags)
}

fn resolve_measure(
    m: &MeasureText,
    table: &dyn Fn(&str) -> Option<TableShape>,
) -> Result<Measure, String> {
    match m {
        MeasureText::Builtin(m) => Ok(m.clone()),
        MeasureText::Table { dataset, suffix } => {
            if KEYWORDS.contains(&dataset.to_ascii_lowercase().as_str()) {
                return Err(format!("'{dataset}' is a measure keyword, not a dataset"));
            }
            let Some(shape) = table(dataset) else {
                return Err(format!(
                    "'{dataset}' is not a history dataset keyed by underlying_ref"
                ));
            };
            if shape.values != 1 {
                return Err(format!(
                    "'{dataset}' has {} value columns; a measure dataset has one",
                    shape.values
                ));
            }
            let wanted = shape.key_len.saturating_sub(1);
            if suffix.len() != wanted {
                return Err(format!(
                    "'{dataset}' takes {wanted} key value{} after the underlying, not {}",
                    if wanted == 1 { "" } else { "s" },
                    suffix.len()
                ));
            }
            Ok(Measure::Table {
                dataset: dataset.clone(),
                suffix: suffix.clone(),
            })
        }
    }
}

/// Resolve every open-form measure against `table`; a column that does
/// not resolve drops with a diagnostic, a view left with none stays as an
/// empty tab, and `default` falls to the first view when it names none.
pub fn resolve(
    doc: &VolViewsDoc,
    table: &dyn Fn(&str) -> Option<TableShape>,
) -> (VolViews, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    let mut views = Vec::new();
    for view in &doc.views {
        let path = format!("vol_views.{}", view.name);
        let mut columns = Vec::new();
        for (i, c) in view.columns.iter().enumerate() {
            let col_path = format!("{path}.columns.{i}");
            let resolved = match c {
                VolColumnSpec::Measure {
                    measure,
                    label,
                    group,
                    level,
                    companion,
                } => resolve_measure(measure, table).map(|measure| VolColumn::Measure {
                    measure,
                    label: label.clone(),
                    group: group.clone(),
                    level: *level,
                    companion: *companion,
                }),
                VolColumnSpec::Spark { measure, span } => {
                    resolve_measure(measure, table).map(|measure| VolColumn::Spark {
                        measure,
                        span: *span,
                    })
                }
                VolColumnSpec::Vega => Ok(VolColumn::Vega),
            };
            match resolved {
                Ok(c) => columns.push(c),
                Err(e) => diags.push(warn(
                    col_path,
                    format!("vol_views: {} column {}: {e}; dropped", view.name, i + 1),
                )),
            }
        }
        views.push(VolView {
            name: view.name.clone(),
            cells: view.cells,
            columns,
        });
    }
    let default = match &doc.default {
        Some(name) => match views.iter().position(|v| &v.name == name) {
            Some(i) => i,
            None => {
                if let Some(first) = views.first() {
                    diags.push(warn(
                        "vol_views.default".into(),
                        format!(
                            "vol_views: default = \"{name}\" names no view; opening '{}'",
                            first.name
                        ),
                    ));
                }
                0
            }
        },
        None => 0,
    };
    (VolViews { views, default }, diags)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};

    fn doc(text: &str) -> MergedDoc {
        merge_docs(
            "vol_views",
            &[LayerDoc::builtin("vol_views", text).unwrap()],
        )
    }

    fn surface_only(name: &str) -> Option<TableShape> {
        match name {
            "ssr" => Some(TableShape {
                key_len: 2,
                values: 1,
            }),
            "spotvol_cov" => Some(TableShape {
                key_len: 1,
                values: 1,
            }),
            "two_values" => Some(TableShape {
                key_len: 2,
                values: 2,
            }),
            _ => None,
        }
    }

    #[test]
    fn every_builtin_form_round_trips_and_atfm_is_the_half_delta_point() {
        for text in [
            "point 3M 0.25",
            "atfm 3M",
            "forward 1M",
            "skew 3M",
            "wing skew 3M",
            "slope 1Y-1M",
            "realized 20D",
            "iv-rv 3M/20D",
            "varswap 3M",
            "vs spread 3M",
        ] {
            let m = parse_measure(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(measure_text(&m), text, "{text}");
        }
        assert_eq!(
            parse_measure("atfm 3M").unwrap(),
            MeasureText::Builtin(Measure::atfm("3M"))
        );
        assert_eq!(
            parse_measure("point 3M 0.50").unwrap(),
            MeasureText::Builtin(Measure::atfm("3M")),
            "0.50 normalizes to 0.5"
        );
        assert_eq!(
            measure_text(&parse_measure("POINT 3M 0.5").unwrap()),
            "atfm 3M"
        );
    }

    #[test]
    fn a_point_delta_is_a_finite_number_strictly_between_0_and_1() {
        for (text, err) in [
            ("point 3M NaN", "point: 'NaN' is not a delta"),
            ("point 3M inf", "point: 'inf' is not a delta"),
            ("point 3M 25", "point: '25' is not a delta between 0 and 1"),
            ("point 3M 0", "point: '0' is not a delta between 0 and 1"),
            ("point 3M 1", "point: '1' is not a delta between 0 and 1"),
        ] {
            assert_eq!(parse_measure(text).unwrap_err(), err, "{text}");
        }
        let m = parse_measure("point 3M 0.250").unwrap();
        assert_eq!(
            m,
            MeasureText::Builtin(Measure::Point {
                tenor: "3M".into(),
                delta: 0.25
            })
        );
        assert_eq!(measure_text(&m), "point 3M 0.25");
    }

    #[test]
    fn the_open_form_names_a_dataset_and_a_keyword_is_never_a_dataset() {
        assert_eq!(
            parse_measure("ssr 3M").unwrap(),
            MeasureText::Table {
                dataset: "ssr".into(),
                suffix: vec!["3M".into()]
            }
        );
        assert_eq!(
            parse_measure("spotvol_cov").unwrap(),
            MeasureText::Table {
                dataset: "spotvol_cov".into(),
                suffix: vec![]
            }
        );
        assert_eq!(
            parse_measure("smile_curv 3M 0.25").unwrap(),
            MeasureText::Table {
                dataset: "smile_curv".into(),
                suffix: vec!["3M".into(), "0.25".into()]
            }
        );
        assert_eq!(parse_measure("").unwrap_err(), "a measure names something");
        assert_eq!(
            parse_measure("skew").unwrap_err(),
            "skew needs a tenor: `skew 3M`"
        );
        assert_eq!(
            parse_measure("slope 1Y 1M").unwrap_err(),
            "slope needs `<long>-<short>`: `slope 1Y-1M`"
        );
        assert_eq!(
            parse_measure("a b c d").unwrap_err(),
            "a dataset takes at most two key values after its name"
        );
    }

    #[test]
    fn a_document_reads_views_in_order_with_cells_and_a_default() {
        let (d, diags) = from_doc(&doc("config_version = 1\ndefault = \"skew\"\n\
             [vol]\n[[vol.columns]]\nmeasure = \"atfm 1M\"\n\
             [[vol.columns]]\nspark = \"atfm 3M\"\nspan = \"30d\"\n\
             [[vol.columns]]\nkind = \"vega\"\n\
             [skew]\ncells = \"single\"\n[[skew.columns]]\nmeasure = \"skew 3M\"\nlabel = \"3M\"\ngroup = \"25Δ skew\"\n"));
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(d.default.as_deref(), Some("skew"));
        assert_eq!(d.views.len(), 2);
        assert_eq!(d.views[0].name, "vol");
        assert_eq!(d.views[0].cells, Cells::Pair);
        assert_eq!(d.views[0].columns.len(), 3);
        assert_eq!(
            d.views[0].columns[1],
            VolColumnSpec::Spark {
                measure: MeasureText::Builtin(Measure::atfm("3M")),
                span: Duration::from_secs(30 * 86_400)
            }
        );
        assert_eq!(d.views[0].columns[2], VolColumnSpec::Vega);
        assert_eq!(d.views[1].cells, Cells::Single);
        assert_eq!(
            d.views[1].columns[0],
            VolColumnSpec::Measure {
                measure: MeasureText::Builtin(Measure::Skew { tenor: "3M".into() }),
                label: Some("3M".into()),
                group: Some("25Δ skew".into()),
                level: None,
                companion: None,
            }
        );
    }

    #[test]
    fn a_bad_column_drops_alone_and_an_empty_view_is_kept() {
        let (d, diags) = from_doc(&doc("config_version = 1\n\
             [vol]\n[[vol.columns]]\nmeasure = \"atfm 1M\"\n\
             [[vol.columns]]\nmeasure = \"nonsense here now four\"\n\
             [[vol.columns]]\nmeasure = \"atfm 3M\"\nkind = \"vega\"\n\
             [[vol.columns]]\nlabel = \"x\"\n\
             [[vol.columns]]\nspark = \"atfm 3M\"\nspan = \"soon\"\n\
             [empty]\n[[empty.columns]]\nmeasure = \"\"\n\
             [bad]\ncells = \"triple\"\n[[bad.columns]]\nmeasure = \"atfm 6M\"\n\
             [notatable]\n"));
        let names: Vec<&str> = d.views.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["vol", "empty", "bad", "notatable"]);
        assert_eq!(d.views[0].columns.len(), 1, "the four bad columns dropped");
        assert!(d.views[1].columns.is_empty(), "its one column dropped");
        assert!(d.views[3].columns.is_empty(), "no columns key at all");
        assert_eq!(
            d.views[2].cells,
            Cells::Pair,
            "an unknown cells reads as pair"
        );
        let texts: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("vol_views: vol column 2: a dataset takes at most two key values after its name; dropped")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("vol_views: vol column 3: a column is one of measure, spark or kind = \"vega\"; dropped")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("vol_views: vol column 4: a column is one of measure, spark or kind = \"vega\"; dropped")), "{texts:?}");
        assert!(
            texts.iter().any(|t| t
                .contains("vol_views: vol column 5: span = \"soon\" is not a duration; dropped")),
            "{texts:?}"
        );
        assert!(
            texts.contains(&"vol_views: 'empty' has no columns"),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains(
                "vol_views: bad: cells = \"triple\" is not pair or single; reading pair"
            )),
            "{texts:?}"
        );
        assert!(
            texts.contains(&"vol_views: 'notatable' has no columns"),
            "{texts:?}"
        );
        assert_eq!(
            diags
                .iter()
                .find(|d| d.message.ends_with("'notatable' has no columns"))
                .unwrap()
                .path
                .as_deref(),
            Some("vol_views.notatable")
        );
        assert_eq!(
            diags
                .iter()
                .find(|d| d.message.contains("column 2"))
                .unwrap()
                .path
                .as_deref(),
            Some("vol_views.vol.columns.1")
        );
    }

    #[test]
    fn resolve_keeps_builtins_resolves_tables_and_picks_the_default() {
        let (d, _) = from_doc(&doc("config_version = 1\ndefault = \"ssr\"\n\
             [vol]\n[[vol.columns]]\nmeasure = \"atfm 1M\"\n\
             [ssr]\n[[ssr.columns]]\nmeasure = \"ssr 3M\"\n[[ssr.columns]]\nmeasure = \"spotvol_cov\"\n"));
        let (v, diags) = resolve(&d, &surface_only);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(v.default, 1);
        assert_eq!(v.default_view().unwrap().name, "ssr");
        assert_eq!(
            v.get("ssr").unwrap().columns[0],
            VolColumn::Measure {
                measure: Measure::Table {
                    dataset: "ssr".into(),
                    suffix: vec!["3M".into()]
                },
                label: None,
                group: None,
                level: None,
                companion: None,
            }
        );
        assert_eq!(
            v.get("ssr").unwrap().columns[1],
            VolColumn::Measure {
                measure: Measure::Table {
                    dataset: "spotvol_cov".into(),
                    suffix: vec![]
                },
                label: None,
                group: None,
                level: None,
                companion: None,
            }
        );
    }

    #[test]
    fn resolve_refuses_a_table_of_the_wrong_shape() {
        let (d, _) = from_doc(&doc("config_version = 1\ndefault = \"gone\"\n\
             [vol]\n[[vol.columns]]\nmeasure = \"atfm 1M\"\n[[vol.columns]]\nmeasure = \"two_values 3M\"\n\
             [[vol.columns]]\nmeasure = \"ssr 3M 0.5\"\n[[vol.columns]]\nmeasure = \"unknown 3M\"\n\
             [only_bad]\n[[only_bad.columns]]\nmeasure = \"unknown\"\n"));
        let (v, diags) = resolve(&d, &surface_only);
        assert_eq!(
            v.views.len(),
            2,
            "a view whose columns all drop stays, empty"
        );
        assert_eq!(v.views[0].columns.len(), 1);
        assert!(v.views[1].columns.is_empty());
        assert_eq!(v.default, 0, "an unknown default falls to the first view");
        let texts: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("vol_views: vol column 2: 'two_values' has 2 value columns; a measure dataset has one; dropped")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("vol_views: vol column 3: 'ssr' takes 1 key value after the underlying, not 2; dropped")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("vol_views: vol column 4: 'unknown' is not a history dataset keyed by underlying_ref; dropped")), "{texts:?}");
        assert!(
            !texts.iter().any(|t| t.contains("only_bad' has no")),
            "resolve says nothing of an empty view beyond its dropped columns: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|t| t.contains("vol_views: default = \"gone\" names no view; opening 'vol'")),
            "{texts:?}"
        );
    }

    #[test]
    fn an_empty_document_resolves_to_no_views() {
        let (d, diags) = from_doc(&doc("config_version = 1\n"));
        assert!(diags.is_empty());
        let (v, diags) = resolve(&d, &|_| None);
        assert!(v.is_empty());
        assert!(diags.is_empty());
        assert!(v.default_view().is_none());
    }

    #[test]
    fn glyph_keys_read_on_a_measure_column_and_drop_elsewhere() {
        let (d, diags) = from_doc(&doc(
            "config_version = 1\n[vol]\n[[vol.columns]]\nmeasure = \"atfm 3M\"\nlevel = \"heat\"\ncompanion = \"bar\"\n\
             [[vol.columns]]\nmeasure = \"atfm 1Y\"\n\
             [[vol.columns]]\nspark = \"atfm 3M\"\nlevel = \"heat\"\n\
             [[vol.columns]]\nkind = \"vega\"\ncompanion = \"bar\"\n\
             [[vol.columns]]\nmeasure = \"atfm 6M\"\nlevel = \"loud\"\n\
             [[vol.columns]]\nmeasure = \"atfm 2Y\"\ncompanion = \"gauge\"\n",
        ));
        let v = &d.views[0];
        assert_eq!(v.columns.len(), 2, "{:?}", v.columns);
        assert_eq!(
            v.columns[0],
            VolColumnSpec::Measure {
                measure: parse_measure("atfm 3M").unwrap(),
                label: None,
                group: None,
                level: Some(LevelGlyph::Heat),
                companion: Some(CompanionGlyph::Bar),
            }
        );
        assert!(matches!(
            &v.columns[1],
            VolColumnSpec::Measure {
                level: None,
                companion: None,
                ..
            }
        ));
        let texts: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert!(
            texts.contains(&"vol_views: vol column 3: level applies to a measure column; dropped"),
            "{texts:?}"
        );
        assert!(
            texts.contains(
                &"vol_views: vol column 4: companion applies to a measure column; dropped"
            ),
            "{texts:?}"
        );
        assert!(
            texts.contains(
                &"vol_views: vol column 5: level = \"loud\" is not plain, heat or tint; dropped"
            ),
            "{texts:?}"
        );
        assert!(texts.contains(&"vol_views: vol column 6: companion = \"gauge\" is not chip, bar or slider; dropped"), "{texts:?}");
    }

    #[test]
    fn a_view_defined_twice_keeps_the_first_and_says_so() {
        // `merge_docs` keeps TOML order; a second `[vol]` table in one layer is a
        // parse error in TOML itself, so the duplicate arrives through two
        // layers merged into one doc — model that with the merged doc's own
        // `value` holding the name once per layer is impossible; instead the
        // reader sees a duplicate only via the case-insensitive match below.
        let (d, diags) = from_doc(&doc(
            "config_version = 1\n[vol]\n[[vol.columns]]\nmeasure = \"atfm 3M\"\n\
             [Vol]\n[[Vol.columns]]\nmeasure = \"atfm 1Y\"\n",
        ));
        assert_eq!(d.views.len(), 1);
        assert_eq!(d.views[0].name, "vol");
        assert!(
            diags
                .iter()
                .any(|d| d.message == "vol_views: 'Vol' is defined twice; the second is skipped"),
            "{diags:?}"
        );
    }

    #[test]
    fn an_empty_view_is_kept_with_a_warning() {
        let (d, diags) = from_doc(&doc(
            "config_version = 1\n[vol]\n[[vol.columns]]\nmeasure = \"atfm 3M\"\n\
             [blank]\ncells = \"single\"\ncolumns = []\n[bare]\n",
        ));
        let names: Vec<&str> = d.views.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["vol", "blank", "bare"]);
        assert!(d.views[1].columns.is_empty() && d.views[2].columns.is_empty());
        assert_eq!(d.views[1].cells, Cells::Single);
        let texts: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert!(
            texts.contains(&"vol_views: 'blank' has no columns"),
            "{texts:?}"
        );
        assert!(
            texts.contains(&"vol_views: 'bare' has no columns"),
            "{texts:?}"
        );
        let (views, _) = resolve(&d, &surface_only);
        assert_eq!(views.views.len(), 3);
        assert!(views.views[1].columns.is_empty());
    }

    #[test]
    fn the_glyph_enums_round_trip_their_keys() {
        for g in [LevelGlyph::Plain, LevelGlyph::Heat, LevelGlyph::Tint] {
            assert_eq!(LevelGlyph::parse(g.key()), Some(g));
        }
        for g in [
            CompanionGlyph::Chip,
            CompanionGlyph::Bar,
            CompanionGlyph::Slider,
        ] {
            assert_eq!(CompanionGlyph::parse(g.key()), Some(g));
        }
        assert_eq!(LevelGlyph::parse("HEAT"), None);
        assert_eq!(SPANS, ["7d", "30d", "90d", "1y"]);
        assert_eq!(ALL_TENORS[0], "1W");
    }
}
