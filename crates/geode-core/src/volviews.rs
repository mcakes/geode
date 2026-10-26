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
use crate::volboard::{Measure, delta_text};

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

/// The picker's families: the grammar's forms, the two non-measure columns
/// and an open-form history dataset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Family {
    Atfm,
    Forward,
    Skew,
    Wing,
    Slope,
    Realized,
    IvRv,
    Varswap,
    Vs,
    Point,
    Spark,
    Vega,
    Table(String),
}

impl Family {
    /// The catalogue in its order: the common form first, the open-form
    /// datasets (schema-dependent) are listed by the caller after these.
    pub fn builtin_families() -> [Family; 12] {
        [
            Family::Atfm,
            Family::Forward,
            Family::Skew,
            Family::Wing,
            Family::Slope,
            Family::Realized,
            Family::IvRv,
            Family::Varswap,
            Family::Vs,
            Family::Point,
            Family::Spark,
            Family::Vega,
        ]
    }

    /// The catalogue entry and the default `group` of a measure column.
    pub fn label(&self) -> String {
        match self {
            Family::Atfm => "ATFM".into(),
            Family::Forward => "Forward".into(),
            Family::Skew => "25Δ skew".into(),
            Family::Wing => "Wing skew".into(),
            Family::Slope => "Slope".into(),
            Family::Realized => "Realized".into(),
            Family::IvRv => "IV − RV".into(),
            Family::Varswap => "Varswap".into(),
            Family::Vs => "VS spread".into(),
            Family::Point => "Surface point".into(),
            Family::Spark => "Sparkline".into(),
            Family::Vega => "Book vega".into(),
            Family::Table(name) => name.to_ascii_uppercase(),
        }
    }

    /// A dataset's one key word after the underlying is held as its tenor.
    pub fn takes_tenor(&self) -> bool {
        matches!(
            self,
            Family::Atfm
                | Family::Forward
                | Family::Skew
                | Family::Wing
                | Family::IvRv
                | Family::Varswap
                | Family::Vs
                | Family::Point
                | Family::Spark
                | Family::Table(_)
        )
    }

    pub fn takes_delta(&self) -> bool {
        matches!(self, Family::Point)
    }

    pub fn takes_span(&self) -> bool {
        matches!(self, Family::Spark)
    }

    pub fn takes_window(&self) -> bool {
        matches!(self, Family::Realized | Family::IvRv)
    }

    pub fn takes_slope(&self) -> bool {
        matches!(self, Family::Slope)
    }

    /// Glyph keys belong to measure columns; the reader refuses them on a
    /// spark or vega column.
    pub fn takes_glyphs(&self) -> bool {
        !matches!(self, Family::Spark | Family::Vega)
    }
}

/// A column as the pickers hold it. `None` in an optional part means the
/// family does not take it; a part the family takes is `Some`, except a
/// dataset keyed by the underlying alone, whose `tenor` is `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnChoice {
    pub family: Family,
    pub tenor: Option<String>,
    pub long: Option<String>,
    pub short: Option<String>,
    pub window: Option<String>,
    pub delta: Option<f64>,
    pub span: Option<Duration>,
    pub label: Option<String>,
    pub group: Option<String>,
    pub level: Option<LevelGlyph>,
    pub companion: Option<CompanionGlyph>,
}

/// A column the pickers cannot express (a dataset with two key words, or a
/// spark over anything but an `atfm`); kept verbatim, movable and deletable
/// only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raw {
    pub text: String,
}

/// The realized windows a picker offers until a source lists its own
/// (the demo's `WINDOWS`); the reader accepts any window text.
pub const WINDOWS: [&str; 3] = ["5D", "20D", "60D"];
/// A new realized or IV − RV column opens at the desk's usual 20-day
/// window, not the shortest offered.
const FIRST_WINDOW: &str = "20D";

impl ColumnChoice {
    /// The family at its firsts: the first tenor, the first delta, the
    /// 30-day span, the 20-day window, and `1Y-1M` for a slope.
    pub fn new(family: Family) -> ColumnChoice {
        let tenor = family.takes_tenor().then(|| ALL_TENORS[0].to_string());
        ColumnChoice {
            long: family.takes_slope().then(|| "1Y".to_string()),
            short: family.takes_slope().then(|| "1M".to_string()),
            window: family.takes_window().then(|| FIRST_WINDOW.to_string()),
            delta: family.takes_delta().then_some(DELTAS[0]),
            span: family.takes_span().then_some(SPARK_30D),
            tenor,
            label: None,
            group: None,
            level: None,
            companion: None,
            family,
        }
    }

    /// The measure the choice names. A spark's measure is the `atfm` at its
    /// tenor; a vega column has none and `to_spec` never asks.
    fn measure(&self) -> MeasureText {
        let t = |s: &Option<String>| s.clone().unwrap_or_default();
        let m = match &self.family {
            Family::Atfm | Family::Spark | Family::Vega => Measure::atfm(&t(&self.tenor)),
            Family::Point => Measure::Point {
                tenor: t(&self.tenor),
                delta: self.delta.unwrap_or(0.5),
            },
            Family::Forward => Measure::Forward {
                tenor: t(&self.tenor),
            },
            Family::Skew => Measure::Skew {
                tenor: t(&self.tenor),
            },
            Family::Wing => Measure::WingSkew {
                tenor: t(&self.tenor),
            },
            Family::Slope => Measure::Slope {
                long: t(&self.long),
                short: t(&self.short),
            },
            Family::Realized => Measure::Realized {
                window: t(&self.window),
            },
            Family::IvRv => Measure::IvRv {
                tenor: t(&self.tenor),
                window: t(&self.window),
            },
            Family::Varswap => Measure::Varswap {
                tenor: t(&self.tenor),
            },
            Family::Vs => Measure::VsSpread {
                tenor: t(&self.tenor),
            },
            Family::Table(dataset) => {
                return MeasureText::Table {
                    dataset: dataset.clone(),
                    suffix: self.tenor.iter().cloned().collect(),
                };
            }
        };
        MeasureText::Builtin(m)
    }

    pub fn to_spec(&self) -> VolColumnSpec {
        match self.family {
            Family::Vega => VolColumnSpec::Vega,
            Family::Spark => VolColumnSpec::Spark {
                measure: self.measure(),
                span: self.span.unwrap_or(SPARK_30D),
            },
            _ => VolColumnSpec::Measure {
                measure: self.measure(),
                label: self.label.clone(),
                group: self.group.clone(),
                level: self.level,
                companion: self.companion,
            },
        }
    }

    /// A choice the pickers can hold, or the column's text as `Raw`.
    pub fn from_spec(spec: &VolColumnSpec) -> Result<ColumnChoice, Raw> {
        let raw = |m: &MeasureText| Raw {
            text: measure_text(m),
        };
        match spec {
            VolColumnSpec::Vega => Ok(ColumnChoice::new(Family::Vega)),
            VolColumnSpec::Spark { measure, span } => {
                let mut c = ColumnChoice::new(Family::Spark);
                match measure {
                    MeasureText::Builtin(Measure::Point { tenor, delta }) if *delta == 0.5 => {
                        c.tenor = Some(tenor.clone());
                    }
                    other => return Err(raw(other)),
                }
                c.span = Some(*span);
                Ok(c)
            }
            VolColumnSpec::Measure {
                measure,
                label,
                group,
                level,
                companion,
            } => {
                let mut c = match measure {
                    MeasureText::Builtin(m) => Self::from_measure(m).ok_or_else(|| raw(measure))?,
                    MeasureText::Table { dataset, suffix } => match suffix.as_slice() {
                        [] | [_] => {
                            let mut c = ColumnChoice::new(Family::Table(dataset.clone()));
                            c.tenor = suffix.first().cloned();
                            c
                        }
                        _ => return Err(raw(measure)),
                    },
                };
                c.label = label.clone();
                c.group = group.clone();
                c.level = *level;
                c.companion = *companion;
                Ok(c)
            }
        }
    }

    /// The built-in measure as a choice; a `Table` measure is the open form
    /// and reads through `MeasureText::Table`, never here.
    fn from_measure(m: &Measure) -> Option<ColumnChoice> {
        let with_tenor = |family: Family, tenor: &String| {
            let mut c = ColumnChoice::new(family);
            c.tenor = Some(tenor.clone());
            c
        };
        let c = match m {
            Measure::Point { tenor, delta } if *delta == 0.5 => with_tenor(Family::Atfm, tenor),
            Measure::Point { tenor, delta } => {
                let mut c = with_tenor(Family::Point, tenor);
                c.delta = Some(*delta);
                c
            }
            Measure::Forward { tenor } => with_tenor(Family::Forward, tenor),
            Measure::Skew { tenor } => with_tenor(Family::Skew, tenor),
            Measure::WingSkew { tenor } => with_tenor(Family::Wing, tenor),
            Measure::Slope { long, short } => {
                let mut c = ColumnChoice::new(Family::Slope);
                c.long = Some(long.clone());
                c.short = Some(short.clone());
                c
            }
            Measure::Realized { window } => {
                let mut c = ColumnChoice::new(Family::Realized);
                c.window = Some(window.clone());
                c
            }
            Measure::IvRv { tenor, window } => {
                let mut c = with_tenor(Family::IvRv, tenor);
                c.window = Some(window.clone());
                c
            }
            Measure::Varswap { tenor } => with_tenor(Family::Varswap, tenor),
            Measure::VsSpread { tenor } => with_tenor(Family::Vs, tenor),
            Measure::Table { .. } => return None,
        };
        Some(c)
    }

    /// The group the tile shows when the column names none.
    pub fn default_group(&self) -> String {
        self.family.label()
    }

    /// The label the tile shows when the column names none.
    pub fn default_label(&self) -> String {
        let t = |s: &Option<String>| s.clone().unwrap_or_default();
        match &self.family {
            Family::Point => format!(
                "{} {}",
                t(&self.tenor),
                self.delta.map(delta_text).unwrap_or_default()
            ),
            Family::Slope => format!("{}-{}", t(&self.long), t(&self.short)),
            Family::Realized => t(&self.window),
            Family::IvRv => format!("{}/{}", t(&self.tenor), t(&self.window)),
            _ => t(&self.tenor),
        }
    }

    /// The list row's text, as the tile's header reads it, with any glyph
    /// the file names appended (`plain` included: naming it on a single
    /// view is a deliberate override).
    pub fn head(&self) -> String {
        let mut s = match &self.family {
            Family::Vega => "Book vega".to_string(),
            Family::Spark => format!(
                "ATFM {} · {} spark",
                self.tenor.clone().unwrap_or_default(),
                span_text(self.span.unwrap_or(SPARK_30D))
            ),
            Family::Table(_) if self.tenor.is_none() => self.family.label(),
            _ => format!(
                "{} {}",
                self.group.clone().unwrap_or_else(|| self.default_group()),
                self.label.clone().unwrap_or_else(|| self.default_label())
            ),
        };
        if let Some(l) = self.level {
            s.push_str(" · ");
            s.push_str(l.key());
        }
        if let Some(c) = self.companion {
            s.push_str(" · ");
            s.push_str(c.key());
        }
        s
    }
}

/// `7d`, `30d`, `90d`, `1y` for the spans the picker offers; otherwise the
/// coarsest unit of `parse_duration`'s grammar that spells the duration
/// exactly (days, hours, seconds, milliseconds), so a hand-written `12h`
/// is never rounded to a `0d` the reader refuses.
pub fn span_text(d: Duration) -> String {
    let ms = d.as_millis();
    if !ms.is_multiple_of(1_000) {
        return format!("{ms}ms");
    }
    match d.as_secs() {
        s if s == 365 * 86_400 => "1y".to_string(),
        s if s.is_multiple_of(86_400) => format!("{}d", s / 86_400),
        s if s.is_multiple_of(3_600) => format!("{}h", s / 3_600),
        s => format!("{s}s"),
    }
}

/// A column's table: only the keys the spec sets, so a default label,
/// group or glyph is never written and the builtin tabs round-trip as
/// they are.
pub fn column_table(spec: &VolColumnSpec) -> toml_edit::Table {
    let mut t = toml_edit::Table::new();
    match spec {
        VolColumnSpec::Measure {
            measure,
            label,
            group,
            level,
            companion,
        } => {
            t["measure"] = toml_edit::value(measure_text(measure));
            if let Some(l) = label {
                t["label"] = toml_edit::value(l.as_str());
            }
            if let Some(g) = group {
                t["group"] = toml_edit::value(g.as_str());
            }
            if let Some(l) = level {
                t["level"] = toml_edit::value(l.key());
            }
            if let Some(c) = companion {
                t["companion"] = toml_edit::value(c.key());
            }
        }
        VolColumnSpec::Spark { measure, span } => {
            t["spark"] = toml_edit::value(measure_text(measure));
            if *span != DEFAULT_SPARK_SPAN {
                t["span"] = toml_edit::value(span_text(*span));
            }
        }
        VolColumnSpec::Vega => {
            t["kind"] = toml_edit::value("vega");
        }
    }
    t
}

/// The view's table for `vol_views.toml`: the source table when given (so
/// its comments and any key this writer does not model survive) with
/// `cells` and `columns` replaced; a fresh table otherwise. `cells` is
/// written only for a single view, since pair is the reader's default.
pub fn to_table(view: &VolViewSpec, source: Option<&toml_edit::Table>) -> toml_edit::Item {
    to_table_from(view.cells, view.columns.iter().map(column_table), source)
}

/// [`to_table`] with the columns given as tables, in order: each the
/// writer's rendering of a spec ([`column_table`]) or a table copied as it
/// was read — one the reader refused, which the dialog carries so that a
/// save keeps it where the reader will go on warning about it rather than
/// deleting what was written by hand.
pub fn to_table_from(
    cells: Cells,
    columns: impl IntoIterator<Item = toml_edit::Table>,
    source: Option<&toml_edit::Table>,
) -> toml_edit::Item {
    let mut table = source.cloned().unwrap_or_default();
    match cells {
        Cells::Single => table["cells"] = toml_edit::value("single"),
        Cells::Pair => {
            table.remove("cells");
        }
    }
    let mut array = toml_edit::ArrayOfTables::new();
    for c in columns {
        array.push(c);
    }
    table["columns"] = toml_edit::Item::ArrayOfTables(array);
    toml_edit::Item::Table(table)
}

/// Give every table its serialized position in iteration order, depth
/// first. `Display` orders tables by the position parsing gave them (a
/// fresh table inherits its predecessor's), so a table cloned or moved from
/// a source document would otherwise print where it was read, not where
/// the doc puts it.
fn renumber(table: &mut toml_edit::Table, next: &mut isize) {
    for (_, item) in table.iter_mut() {
        match item {
            toml_edit::Item::Table(t) => {
                t.set_position(Some(*next));
                *next += 1;
                renumber(t, next);
            }
            toml_edit::Item::ArrayOfTables(a) => {
                for t in a.iter_mut() {
                    t.set_position(Some(*next));
                    *next += 1;
                    renumber(t, next);
                }
            }
            _ => {}
        }
    }
}

/// The whole document: `config_version`, `default`, the views in order,
/// then any top-level value the source held that this writer does not
/// model. A source table the doc no longer lists is a deleted view and is
/// not copied: the reader takes every table for a view.
///
/// The whole-document rendering, which the tests use to prove the writer
/// round-trips a file; the VolMonitor views dialog writes one view at a time
/// through [`to_table_from`] and reorders the file in place through
/// [`reorder`], so no running path renders a document whole.
pub fn to_document(
    doc: &VolViewsDoc,
    source: Option<&toml_edit::DocumentMut>,
) -> toml_edit::DocumentMut {
    let mut out = toml_edit::DocumentMut::new();
    out["config_version"] = toml_edit::value(1);
    if let Some(d) = &doc.default {
        out["default"] = toml_edit::value(d.as_str());
    }
    for view in &doc.views {
        let src = source
            .and_then(|s| s.get(&view.name))
            .and_then(|i| i.as_table());
        out[&view.name] = to_table(view, src);
    }
    if let Some(s) = source {
        for (key, item) in s.as_table().iter() {
            if key == "config_version"
                || key == "default"
                || item.is_table_like()
                || item.is_array_of_tables()
            {
                continue;
            }
            out[key] = item.clone();
        }
    }
    renumber(out.as_table_mut(), &mut 0);
    out
}

/// Move the named view tables to the front in `order`; tables not named
/// keep their relative order after them; `config_version` and `default`
/// stay first. Removing and re-inserting keeps each table's decor; the
/// tables are then renumbered so the order reaches the file. The shell's
/// `objectdialog::apply::docs_reordered` is the same rule over the
/// `toml::Table` memory holds, so what a batch does to memory and to the
/// file agree.
pub fn reorder(document: &mut toml_edit::DocumentMut, order: &[String]) {
    let table = document.as_table_mut();
    let keys: Vec<String> = table.iter().map(|(k, _)| k.to_string()).collect();
    let mut head = Vec::new();
    let mut named = Vec::new();
    let mut rest = Vec::new();
    for k in keys {
        let item = table.remove(&k).expect("key listed");
        if k == "config_version" || k == "default" {
            head.push((k, item));
        } else if order.contains(&k) {
            named.push((k, item));
        } else {
            rest.push((k, item));
        }
    }
    named.sort_by_key(|(k, _)| order.iter().position(|o| o == k));
    for (k, item) in head.into_iter().chain(named).chain(rest) {
        table.insert(&k, item);
    }
    renumber(table, &mut 0);
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

/// A test-only copy of the tile's `BUILTIN_VOL_VIEWS` (`geode-volwatch`):
/// core cannot import the tile, and the writer's round-trip test needs the
/// shipped tabs verbatim. Keep the two texts equal.
#[cfg(test)]
mod builtin_text {
    pub(crate) const BUILTIN_VOL_VIEWS: &str = r#"config_version = 1
default = "vol"

# ATFM vol across the tenor ladder, a 30-day sparkline and book vega.
[vol]
[[vol.columns]]
measure = "atfm 1W"
[[vol.columns]]
measure = "atfm 1M"
[[vol.columns]]
measure = "atfm 3M"
[[vol.columns]]
measure = "atfm 6M"
[[vol.columns]]
measure = "atfm 1Y"
[[vol.columns]]
measure = "atfm 2Y"
[[vol.columns]]
measure = "atfm 3Y"
[[vol.columns]]
measure = "atfm 5Y"
[[vol.columns]]
spark = "atfm 3M"
span = "30d"
[[vol.columns]]
kind = "vega"

# 25Δ skew across the ladder and the 3M wing.
[skew]
[[skew.columns]]
measure = "skew 1W"
[[skew.columns]]
measure = "skew 1M"
[[skew.columns]]
measure = "skew 3M"
[[skew.columns]]
measure = "skew 6M"
[[skew.columns]]
measure = "skew 1Y"
[[skew.columns]]
measure = "skew 2Y"
[[skew.columns]]
measure = "skew 3Y"
[[skew.columns]]
measure = "skew 5Y"
[[skew.columns]]
measure = "wing skew 3M"

# Varswap strikes across the ladder.
[varswap]
[[varswap.columns]]
measure = "varswap 1W"
[[varswap.columns]]
measure = "varswap 1M"
[[varswap.columns]]
measure = "varswap 3M"
[[varswap.columns]]
measure = "varswap 6M"
[[varswap.columns]]
measure = "varswap 1Y"
[[varswap.columns]]
measure = "varswap 2Y"
[[varswap.columns]]
measure = "varswap 3Y"
[[varswap.columns]]
measure = "varswap 5Y"

# The surface in one line: five deltas at five tenors, one number per
# cell; `m` swaps the level for its companion.
[surface]
cells = "single"
[[surface.columns]]
measure = "point 1M 0.05"
[[surface.columns]]
measure = "point 1M 0.25"
[[surface.columns]]
measure = "point 1M 0.5"
[[surface.columns]]
measure = "point 1M 0.75"
[[surface.columns]]
measure = "point 1M 0.95"
[[surface.columns]]
measure = "point 3M 0.05"
[[surface.columns]]
measure = "point 3M 0.25"
[[surface.columns]]
measure = "point 3M 0.5"
[[surface.columns]]
measure = "point 3M 0.75"
[[surface.columns]]
measure = "point 3M 0.95"
[[surface.columns]]
measure = "point 6M 0.05"
[[surface.columns]]
measure = "point 6M 0.25"
[[surface.columns]]
measure = "point 6M 0.5"
[[surface.columns]]
measure = "point 6M 0.75"
[[surface.columns]]
measure = "point 6M 0.95"
[[surface.columns]]
measure = "point 1Y 0.05"
[[surface.columns]]
measure = "point 1Y 0.25"
[[surface.columns]]
measure = "point 1Y 0.5"
[[surface.columns]]
measure = "point 1Y 0.75"
[[surface.columns]]
measure = "point 1Y 0.95"
[[surface.columns]]
measure = "point 2Y 0.05"
[[surface.columns]]
measure = "point 2Y 0.25"
[[surface.columns]]
measure = "point 2Y 0.5"
[[surface.columns]]
measure = "point 2Y 0.75"
[[surface.columns]]
measure = "point 2Y 0.95"
"#;
}

#[cfg(test)]
pub(crate) fn tests_builtin_text() -> &'static str {
    builtin_text::BUILTIN_VOL_VIEWS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Layer, LayerDoc, merge_docs};

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
                key_len: 2,
                values: 1,
            }),
            // Keyed by `underlying_ref` alone: the zero-suffix open form.
            "term_slope" => Some(TableShape {
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
            parse_measure("spotvol_cov 3M").unwrap(),
            MeasureText::Table {
                dataset: "spotvol_cov".into(),
                suffix: vec!["3M".into()]
            }
        );
        assert_eq!(
            parse_measure("term_slope").unwrap(),
            MeasureText::Table {
                dataset: "term_slope".into(),
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
             [ssr]\n[[ssr.columns]]\nmeasure = \"ssr 3M\"\n[[ssr.columns]]\nmeasure = \"spotvol_cov 3M\"\n\
             [[ssr.columns]]\nmeasure = \"term_slope\"\n"));
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
                    suffix: vec!["3M".into()]
                },
                label: None,
                group: None,
                level: None,
                companion: None,
            }
        );
        // A dataset keyed by `underlying_ref` alone resolves with no suffix.
        assert_eq!(
            v.get("ssr").unwrap().columns[2],
            VolColumn::Measure {
                measure: Measure::Table {
                    dataset: "term_slope".into(),
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
        // A TOML layer cannot hold `[vol]` twice and `merge_docs` collapses a
        // cross-layer duplicate, so the one duplicate that can reach the
        // reader is a case collision.
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

    fn spec(text: &str) -> VolColumnSpec {
        VolColumnSpec::Measure {
            measure: parse_measure(text).unwrap(),
            label: None,
            group: None,
            level: None,
            companion: None,
        }
    }

    #[test]
    fn every_builtin_column_reads_into_a_choice_and_writes_back_the_same_text() {
        let (d, _) = from_doc(&doc(crate::volviews::tests_builtin_text()));
        for v in &d.views {
            for c in &v.columns {
                let choice =
                    ColumnChoice::from_spec(c).unwrap_or_else(|r| panic!("{}: {}", v.name, r.text));
                assert_eq!(&choice.to_spec(), c, "{} {:?}", v.name, c);
            }
        }
    }

    #[test]
    fn a_choice_at_its_firsts_names_the_first_tenor_and_delta() {
        let c = ColumnChoice::new(Family::Point);
        assert_eq!(c.tenor.as_deref(), Some("1W"));
        assert_eq!(c.delta, Some(0.10));
        assert_eq!(
            measure_text(&match c.to_spec() {
                VolColumnSpec::Measure { measure, .. } => measure,
                _ => unreachable!(),
            }),
            "point 1W 0.1"
        );
        let s = ColumnChoice::new(Family::Spark);
        assert_eq!(s.span, Some(SPARK_30D));
        assert!(matches!(s.to_spec(), VolColumnSpec::Spark { .. }));
        assert!(matches!(
            ColumnChoice::new(Family::Vega).to_spec(),
            VolColumnSpec::Vega
        ));
        let t = ColumnChoice::new(Family::Table("ssr".into()));
        assert_eq!(t.tenor.as_deref(), Some("1W"));
        assert_eq!(t.head(), "SSR 1W");
        assert_eq!(WINDOWS, ["5D", "20D", "60D"]);
        assert_eq!(
            ColumnChoice::new(Family::Realized).window.as_deref(),
            Some("20D")
        );
        assert_eq!(
            ColumnChoice::new(Family::IvRv).window.as_deref(),
            Some("20D")
        );
    }

    #[test]
    fn heads_read_as_the_tile_would_and_name_a_non_default_glyph() {
        let mut c = ColumnChoice::from_spec(&spec("skew 1Y")).unwrap();
        assert_eq!(c.head(), "25Δ skew 1Y");
        c.level = Some(LevelGlyph::Heat);
        c.companion = Some(CompanionGlyph::Slider);
        assert_eq!(c.head(), "25Δ skew 1Y · heat · slider");
        assert_eq!(
            ColumnChoice::from_spec(&spec("point 3M 0.25"))
                .unwrap()
                .head(),
            "Surface point 3M 0.25"
        );
        assert_eq!(
            ColumnChoice::from_spec(&spec("atfm 3M"))
                .unwrap()
                .default_group(),
            "ATFM"
        );
        assert_eq!(
            ColumnChoice::from_spec(&spec("atfm 3M"))
                .unwrap()
                .default_label(),
            "3M"
        );
    }

    #[test]
    fn a_text_the_pickers_cannot_express_is_raw() {
        let r = ColumnChoice::from_spec(&spec("smile_curv 3M 0.25")).unwrap_err();
        assert_eq!(r.text, "smile_curv 3M 0.25");
    }

    #[test]
    fn the_writer_keeps_unknown_keys_and_comments_and_round_trips_the_builtin_tabs() {
        let text = "config_version = 1\ndefault = \"vol\"\n\n# the desk's tabs\n[vol]\nnote = \"kept\"\n[[vol.columns]]\nmeasure = \"atfm 3M\"\n[[vol.columns]]\nkind = \"vega\"\n\n[skew]\ncells = \"single\"\n[[skew.columns]]\nmeasure = \"skew 3M\"\nlabel = \"3M\"\ngroup = \"25Δ skew\"\nlevel = \"heat\"\n";
        let source: toml_edit::DocumentMut = text.parse().unwrap();
        let (d, _) = from_doc(&doc(text));
        let out = to_document(&d, Some(&source)).to_string();
        assert!(out.contains("# the desk's tabs"), "{out}");
        assert!(out.contains("note = \"kept\""), "{out}");
        assert!(out.contains("level = \"heat\""), "{out}");
        let (again, _) = from_doc(&doc(&out));
        assert_eq!(again, d);
        // The doc's order wins over the source's: a source table keeps its
        // parsed position unless the writer renumbers it.
        let mut swapped = d.clone();
        swapped.views.reverse();
        let out = to_document(&swapped, Some(&source)).to_string();
        let at = |needle: &str| {
            out.find(needle)
                .unwrap_or_else(|| panic!("{needle}: {out}"))
        };
        assert!(at("[skew]") < at("[[skew.columns]]"), "{out}");
        assert!(at("[[skew.columns]]") < at("[vol]"), "{out}");
        assert!(at("[vol]") < at("[[vol.columns]]"), "{out}");
        let (again, _) = from_doc(&doc(&out));
        assert_eq!(again, swapped);
        // The builtin tabs, read and written, read identically.
        let builtin = crate::volviews::tests_builtin_text();
        let (b, _) = from_doc(&doc(builtin));
        let (b2, _) = from_doc(&doc(&to_document(&b, None).to_string()));
        assert_eq!(b2, b);
    }

    #[test]
    fn to_table_writes_cells_only_for_single_and_no_default_label_or_group() {
        let (d, _) = from_doc(&doc(
            "config_version = 1\n[vol]\n[[vol.columns]]\nmeasure = \"atfm 3M\"\n[s]\ncells = \"single\"\n[[s.columns]]\nmeasure = \"atfm 3M\"\n",
        ));
        let pair = to_table(&d.views[0], None).to_string();
        assert!(!pair.contains("cells"), "{pair}");
        assert!(!pair.contains("label") && !pair.contains("group"), "{pair}");
        let single = to_table(&d.views[1], None).to_string();
        assert!(single.contains("cells = \"single\""), "{single}");
    }

    #[test]
    fn reorder_moves_the_named_tables_and_keeps_the_rest_after() {
        let mut document: toml_edit::DocumentMut =
            "config_version = 1\n[a]\n[[a.columns]]\nkind = \"vega\"\n[b]\n[[b.columns]]\nkind = \"vega\"\n[c]\n[[c.columns]]\nkind = \"vega\"\n"
                .parse()
                .unwrap();
        reorder(&mut document, &["c".into(), "a".into()]);
        let names: Vec<&str> = document.as_table().iter().map(|(k, _)| k).collect();
        assert_eq!(names, ["config_version", "c", "a", "b"]);
        let out = document.to_string();
        let at = |needle: &str| {
            out.find(needle)
                .unwrap_or_else(|| panic!("{needle}: {out}"))
        };
        assert!(at("[c]") < at("[[c.columns]]"), "{out}");
        assert!(at("[[c.columns]]") < at("[a]"), "{out}");
        assert!(at("[[a.columns]]") < at("[b]"), "{out}");
        assert!(at("[b]") < at("[[b.columns]]"), "{out}");
    }

    /// A user file that forks `surface` and then `vol` holds them in that
    /// order, which the merge reads as the tab order; put in the merged
    /// order after each write, the file leaves the tabs where they were.
    #[test]
    fn a_user_file_in_merged_order_leaves_the_merged_tabs_in_place() {
        let shipped: Vec<String> = ["vol", "skew", "varswap", "surface"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let builtin: toml::Table = "config_version = 1\n[vol]\n[skew]\n[varswap]\n[surface]\n"
            .parse()
            .unwrap();
        let mut user: toml_edit::DocumentMut = "config_version = 1\n".parse().unwrap();
        let merged = |user: &toml_edit::DocumentMut| {
            let docs = [
                LayerDoc {
                    layer: Layer::Builtin,
                    name: "vol_views".into(),
                    file: "builtin".into(),
                    table: builtin.clone(),
                },
                LayerDoc {
                    layer: Layer::User,
                    name: "vol_views".into(),
                    file: "user".into(),
                    table: user.to_string().parse().unwrap(),
                },
            ];
            merge_docs("vol_views", &docs)
                .value
                .keys()
                .filter(|k| *k != "config_version")
                .cloned()
                .collect::<Vec<_>>()
        };
        user["surface"] = toml_edit::table();
        reorder(&mut user, &shipped);
        assert_eq!(merged(&user), shipped);
        user["vol"] = toml_edit::table();
        assert_eq!(
            merged(&user),
            ["surface", "skew", "varswap", "vol"],
            "appended as written, the second fork takes the first's slot"
        );
        reorder(&mut user, &shipped);
        assert_eq!(merged(&user), shipped);
        let keys: Vec<&str> = user.as_table().iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["config_version", "vol", "surface"]);
    }

    #[test]
    fn a_view_the_doc_dropped_is_not_written_back_from_the_source() {
        let text = "config_version = 1\nnote = \"kept\"\n[vol]\n[[vol.columns]]\nkind = \"vega\"\n[gone]\n[[gone.columns]]\nkind = \"vega\"\n";
        let source: toml_edit::DocumentMut = text.parse().unwrap();
        let (mut d, _) = from_doc(&doc(text));
        d.views.retain(|v| v.name == "vol");
        let out = to_document(&d, Some(&source)).to_string();
        assert!(!out.contains("gone"), "{out}");
        assert!(out.contains("note = \"kept\""), "{out}");
        let (again, _) = from_doc(&doc(&out));
        assert_eq!(again.views.len(), 1);
    }

    #[test]
    fn a_span_is_written_at_the_grain_the_reader_takes_and_reads_back_equal() {
        for text in ["7d", "30d", "90d", "1y", "12h", "36h", "90s", "500ms"] {
            let d = parse_duration(text).unwrap();
            assert_eq!(span_text(d), text, "{text}");
        }
        let text = "config_version = 1\n[vol]\n[[vol.columns]]\nspark = \"atfm 3M\"\nspan = \"12h\"\n[[vol.columns]]\nspark = \"atfm 1M\"\nspan = \"36h\"\n";
        let (d, _) = from_doc(&doc(text));
        let out = to_document(&d, None).to_string();
        assert!(out.contains("span = \"12h\""), "{out}");
        assert!(out.contains("span = \"36h\""), "{out}");
        let (again, diags) = from_doc(&doc(&out));
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(again, d);
    }
}
