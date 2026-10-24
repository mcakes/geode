//! The vol watchlist's tabs, `vol_views.toml`: named column lists over the
//! board's measures. Parsing (`from_doc`) knows nothing of the schema and
//! keeps every column whose text reads; resolution (`resolve`) turns an
//! open-form measure (`ssr 3M`) into a board `Measure::Table` against the
//! history datasets the schema declares, and drops what it cannot. A view
//! with no valid column is skipped, never shown empty.

use std::time::Duration;

use crate::config::{Diagnostic, MergedDoc, Severity};
use crate::query::normalize_key;
use crate::schema::ColumnType;
use crate::source_config::parse_duration;
use crate::volboard::{Measure, delta_text};

/// The spark span when a column names none.
pub const DEFAULT_SPARK_SPAN: Duration = Duration::from_secs(30 * 86_400);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cells {
    Pair,
    Single,
}

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
                let delta: f64 = delta
                    .parse()
                    .map_err(|_| format!("point: '{delta}' is not a delta"))?;
                Measure::Point {
                    tenor: key_text(tenor, ColumnType::Utf8),
                    delta: delta_text(delta).parse().unwrap_or(delta),
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
            match parse_measure(&measure) {
                Ok(measure) => Some(VolColumnSpec::Measure {
                    measure,
                    label: text("label").filter(|s| !s.trim().is_empty()),
                    group: text("group").filter(|s| !s.trim().is_empty()),
                }),
                Err(e) => {
                    diags.push(column(e));
                    None
                }
            }
        }
        (false, true, false) => {
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
        (false, false, true) => match text("kind").as_deref() {
            Some("vega") => Some(VolColumnSpec::Vega),
            _ => {
                diags.push(column("kind is \"vega\"".into()));
                None
            }
        },
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
/// valid column is skipped.
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
        let Some(columns) = table.get("columns").and_then(|v| v.as_array()) else {
            diags.push(warn(
                path,
                format!("vol_views: '{name}' has no columns; skipped"),
            ));
            continue;
        };
        let columns: Vec<VolColumnSpec> = columns
            .iter()
            .enumerate()
            .filter_map(|(i, v)| {
                read_column(v, &format!("{path}.columns.{i}"), name, i, &mut diags)
            })
            .collect();
        if columns.is_empty() {
            diags.push(warn(
                path,
                format!("vol_views: '{name}' has no valid column; skipped"),
            ));
            continue;
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
/// not resolve drops with a diagnostic, a view left with none is skipped,
/// and `default` falls to the first view when it names none.
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
                } => resolve_measure(measure, table).map(|measure| VolColumn::Measure {
                    measure,
                    label: label.clone(),
                    group: group.clone(),
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
        if columns.is_empty() {
            diags.push(warn(
                path,
                format!("vol_views: '{}' has no valid column; skipped", view.name),
            ));
            continue;
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
            }
        );
    }

    #[test]
    fn a_bad_column_drops_alone_and_an_empty_view_is_refused() {
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
        assert_eq!(names, ["vol", "bad"]);
        assert_eq!(d.views[0].columns.len(), 1, "the four bad columns dropped");
        assert_eq!(
            d.views[1].cells,
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
            texts
                .iter()
                .any(|t| t.contains("vol_views: 'empty' has no valid column; skipped")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains(
                "vol_views: bad: cells = \"triple\" is not pair or single; reading pair"
            )),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|t| t.contains("vol_views: 'notatable' has no columns; skipped")),
            "{texts:?}"
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
                group: None
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
                group: None
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
        assert_eq!(v.views.len(), 1);
        assert_eq!(v.views[0].columns.len(), 1);
        assert_eq!(v.default, 0, "an unknown default falls to the first view");
        let texts: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("vol_views: vol column 2: 'two_values' has 2 value columns; a measure dataset has one; dropped")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("vol_views: vol column 3: 'ssr' takes 1 key value after the underlying, not 2; dropped")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("vol_views: vol column 4: 'unknown' is not a history dataset keyed by underlying_ref; dropped")), "{texts:?}");
        assert!(
            texts
                .iter()
                .any(|t| t.contains("vol_views: 'only_bad' has no valid column; skipped")),
            "{texts:?}"
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
}
