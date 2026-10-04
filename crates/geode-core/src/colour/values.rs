//! Value colors: a text dimension's value mapped to a named color, so the
//! cell showing `SPX` paints in SPX's color wherever it appears. The
//! document is `value_colors.toml`; a color is a `colors.toml` name or an
//! inline `{ hue }` / `{ token }` table.
//! Everything here is pure: the reader, the check against the schema and
//! the color definitions, and the layer arithmetic behind the pick list.

use crate::colour::{Base, Definition, NamedColours, RESERVED_PREFIX, Tone};
use crate::config::{Diagnostic, Layer, LayerDoc, MergedDoc, Severity, VALUE_COLORS_DOC};
use crate::dimensions::DerivedDimensions;
use crate::schema::{ColumnRole, ColumnType, SchemaSpec};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// The entry that means "no color": how a higher layer clears a lower
/// layer's color. It reads as an unmapped value.
pub const NO_COLOR: &str = "none";

/// The internal color key of an inline entry. It contains whitespace,
/// which `config::check_object_name` refuses, so no `colors.toml` name can
/// ever equal it.
pub fn inline_key(dimension: &str, value: &str) -> String {
    format!("inline {dimension}.{value}")
}

/// Read one inline entry: [`Definition::from_table`]'s rules, minus
/// `tint_sign`, which is refused because a value has no sign.
pub fn read_inline(
    table: &toml::Table,
    path: &str,
    dimension: &str,
    value: &str,
) -> (Option<Definition>, Vec<Diagnostic>) {
    if table.contains_key("tint_sign") {
        return (
            None,
            vec![refusal(
                path.to_string(),
                format!(
                    "value colors '{dimension}': '{value}': tint_sign is refused, a value has no sign; dropped"
                ),
            )],
        );
    }
    Definition::from_table(
        table,
        path,
        &format!("value colors '{dimension}': '{value}'"),
    )
}

/// An error that drops the entry at `path`.
fn refusal(path: String, message: String) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        layer: None,
        file: None,
        message,
        path: Some(path),
    }
}

/// One layer's entry for a value: a `colors.toml` name (`"none"` included,
/// raw) or an inline definition.
#[derive(Debug, Clone, PartialEq)]
pub enum ValueEntry {
    Named(String),
    Inline(Definition),
}

impl ValueEntry {
    /// `"none"`: the entry by which a higher layer clears a lower one.
    pub fn is_cleared(&self) -> bool {
        matches!(self, ValueEntry::Named(name) if name == NO_COLOR)
    }

    /// How the pick list and the notices name it: the color's name, or
    /// [`inline_label`]'s `hue 210`, `hue 30 light`, or token name.
    pub fn label(&self) -> String {
        match self {
            ValueEntry::Named(name) => name.clone(),
            ValueEntry::Inline(definition) => inline_label(definition),
        }
    }
}

impl From<&str> for ValueEntry {
    fn from(name: &str) -> Self {
        ValueEntry::Named(name.to_string())
    }
}

impl From<String> for ValueEntry {
    fn from(name: String) -> Self {
        ValueEntry::Named(name)
    }
}

/// An inline color's label: `hue 210`, `hue 30 light`, or the token's name.
pub fn inline_label(definition: &Definition) -> String {
    match &definition.base {
        Base::Hue {
            degrees,
            tone: Tone::Normal,
        } => format!("hue {}", degrees.round() as i64),
        Base::Hue {
            degrees,
            tone: Tone::Light,
        } => format!("hue {} light", degrees.round() as i64),
        Base::Token(token) => token.name().to_string(),
    }
}

/// The pick list's twelve presets: a name and a wheel hue, tone normal.
pub const PRESETS: [(&str, u16); 12] = [
    ("red", 0),
    ("orange", 30),
    ("yellow", 60),
    ("lime", 90),
    ("green", 120),
    ("teal", 150),
    ("cyan", 180),
    ("azure", 210),
    ("blue", 240),
    ("violet", 270),
    ("magenta", 300),
    ("rose", 330),
];

/// The preset `definition` is: the same hue, tone normal, untinted.
pub fn preset_of(definition: &Definition) -> Option<&'static str> {
    let Base::Hue {
        degrees,
        tone: Tone::Normal,
    } = &definition.base
    else {
        return None;
    };
    if definition.tint_sign {
        return None;
    }
    PRESETS
        .iter()
        .find(|(_, preset)| f32::from(*preset) == *degrees)
        .map(|(name, _)| *name)
}

/// One dimension's colored values. A color name is an `Arc<str>` so a
/// prepared grid cell shares it rather than copying it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DimensionColors {
    by_value: BTreeMap<String, Arc<str>>,
}

impl DimensionColors {
    /// The color name for `value`, matched exactly.
    pub fn get(&self, value: &str) -> Option<&Arc<str>> {
        self.by_value.get(value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Arc<str>)> {
        self.by_value.iter().map(|(v, c)| (v.as_str(), c))
    }
}

/// Dimension → value → color name. Holds only colored values: an entry of
/// [`NO_COLOR`] and a refused entry are both absent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ValueColors {
    by_dimension: BTreeMap<String, DimensionColors>,
    /// Inline entries' definitions by internal key ([`inline_key`]); the
    /// key is what `by_dimension` stores for such a value.
    inline: BTreeMap<String, Definition>,
}

impl ValueColors {
    /// Read the merged `value_colors` document. Knows nothing of the schema
    /// or the color definitions; [`check_value_colors`] does.
    pub fn from_doc(doc: &MergedDoc) -> (ValueColors, Vec<Diagnostic>) {
        let mut out = ValueColors::default();
        let mut diags = Vec::new();
        for (dimension, entry) in &doc.value {
            if dimension == "config_version" {
                continue;
            }
            let Some(table) = entry.as_table() else {
                diags.push(refusal(
                    format!("{VALUE_COLORS_DOC}.{dimension}"),
                    format!("value colors '{dimension}': not a table of value entries; dropped"),
                ));
                continue;
            };
            for (value, color) in table {
                let path = format!("{VALUE_COLORS_DOC}.{dimension}.{value}");
                if value.is_empty() {
                    diags.push(refusal(
                        path,
                        format!("value colors '{dimension}': an empty value; dropped"),
                    ));
                    continue;
                }
                if let Some(inline) = color.as_table() {
                    let (definition, inline_diags) = read_inline(inline, &path, dimension, value);
                    diags.extend(inline_diags);
                    if let Some(definition) = definition {
                        out.insert_inline(dimension, value, definition);
                    }
                    continue;
                }
                let Some(name) = color.as_str() else {
                    diags.push(refusal(
                        path,
                        format!(
                            "value colors '{dimension}': '{value}' must name a color or be an inline {{ hue }} or {{ token }} table (got {color}); dropped"
                        ),
                    ));
                    continue;
                };
                if name == "sign" {
                    diags.push(refusal(
                        path,
                        format!(
                            "value colors '{dimension}': '{value}': sign is a column color mode, not a color; dropped"
                        ),
                    ));
                    continue;
                }
                if name.starts_with(RESERVED_PREFIX) {
                    diags.push(refusal(
                        path,
                        format!(
                            "value colors '{dimension}': '{value}': absolute colors are not accepted here, name a color or give an inline hue; dropped"
                        ),
                    ));
                    continue;
                }
                if name == NO_COLOR {
                    continue;
                }
                out.insert(dimension, value, name);
            }
        }
        (out, diags)
    }

    pub fn dimension(&self, name: &str) -> Option<&DimensionColors> {
        self.by_dimension.get(name)
    }

    /// The color name for `value` of `dimension`; `None` when unmapped.
    pub fn get(&self, dimension: &str, value: &str) -> Option<&Arc<str>> {
        self.dimension(dimension)?.get(value)
    }

    /// The dimensions holding at least one colored value.
    pub fn dimensions(&self) -> impl Iterator<Item = &str> {
        self.by_dimension.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.by_dimension.is_empty()
    }

    pub fn insert(&mut self, dimension: &str, value: &str, color: &str) {
        self.inline.remove(&inline_key(dimension, value));
        self.by_dimension
            .entry(dimension.to_string())
            .or_default()
            .by_value
            .insert(value.to_string(), color.into());
    }

    /// Color `value` of `dimension` with an inline definition: the value
    /// maps to its [`inline_key`], whose definition is kept here.
    pub fn insert_inline(&mut self, dimension: &str, value: &str, definition: Definition) {
        let key = inline_key(dimension, value);
        self.by_dimension
            .entry(dimension.to_string())
            .or_default()
            .by_value
            .insert(value.to_string(), key.as_str().into());
        self.inline.insert(key, definition);
    }

    /// The definition behind an inline key; `None` for a `colors.toml` name.
    pub fn inline_definition(&self, key: &str) -> Option<&Definition> {
        self.inline.get(key)
    }

    /// Every inline entry's key and definition.
    pub fn inline(&self) -> impl Iterator<Item = (&str, &Definition)> {
        self.inline
            .iter()
            .map(|(key, definition)| (key.as_str(), definition))
    }
}

/// What a name is, as far as value colors care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DimensionKind {
    /// A utf8 `dimension` or `key` column of some dataset, or a derived
    /// dimension (its values are labels). Values of it can be colored.
    Text,
    /// Declared as a dimension or key, but never as text.
    NotText,
    /// No dataset or derived dimension declares it.
    Undeclared,
}

fn is_dimension_role(role: ColumnRole) -> bool {
    matches!(role, ColumnRole::Dimension { .. } | ColumnRole::Key)
}

/// Classify `name`. Text wins when any dataset declares it as a utf8
/// dimension or key, so one numeric spelling elsewhere does not hide it.
pub fn dimension_kind(schema: &SchemaSpec, dims: &DerivedDimensions, name: &str) -> DimensionKind {
    if dims.get(name).is_some() {
        return DimensionKind::Text;
    }
    let mut declared = false;
    for column in schema
        .datasets
        .iter()
        .flat_map(|d| d.columns.iter())
        .filter(|c| c.name == name && is_dimension_role(c.role))
    {
        if column.ty == ColumnType::Utf8 {
            return DimensionKind::Text;
        }
        declared = true;
    }
    if declared {
        DimensionKind::NotText
    } else {
        DimensionKind::Undeclared
    }
}

/// Every name [`dimension_kind`] calls `Text`, so a surface offering to
/// color a dimension and the check agree on which ones can be colored.
pub fn text_dimensions(schema: &SchemaSpec, dims: &DerivedDimensions) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = schema
        .datasets
        .iter()
        .flat_map(|d| d.columns.iter())
        .filter(|c| c.ty == ColumnType::Utf8 && is_dimension_role(c.role))
        .map(|c| c.name.clone())
        .collect();
    out.extend(dims.all().map(|d| d.name.clone()));
    out
}

/// Remove what cannot paint, warning once per removal: a dimension nothing
/// declares, a dimension that is not text, and a value naming a color
/// `named` does not define. An inline entry names none and is kept; a
/// value is inline only when its key is its own [`inline_key`], so a name
/// spelled like another value's key borrows nothing and is warned as
/// unknown. What is returned is exactly what a tile may look up, so paint
/// needs no second validity check.
pub fn check_value_colors(
    values: ValueColors,
    named: &NamedColours,
    kind_of: impl Fn(&str) -> DimensionKind,
) -> (ValueColors, Vec<Diagnostic>) {
    let mut out = ValueColors::default();
    let mut diags = Vec::new();
    let mut warn = |path: String, message: String| {
        diags.push(Diagnostic {
            severity: Severity::Warning,
            layer: None,
            file: None,
            message,
            path: Some(path),
        })
    };
    for (dimension, colors) in &values.by_dimension {
        match kind_of(dimension) {
            DimensionKind::Text => {}
            DimensionKind::NotText => {
                warn(
                    format!("{VALUE_COLORS_DOC}.{dimension}"),
                    format!(
                        "value colors '{dimension}': value colors apply to text dimensions; ignored"
                    ),
                );
                continue;
            }
            DimensionKind::Undeclared => {
                warn(
                    format!("{VALUE_COLORS_DOC}.{dimension}"),
                    format!(
                        "value colors '{dimension}': no dataset declares dimension '{dimension}'; ignored"
                    ),
                );
                continue;
            }
        }
        for (value, color) in colors.iter() {
            let inline = values
                .inline_definition(color)
                .filter(|_| **color == *inline_key(dimension, value));
            if inline.is_none() && named.get(color).is_none() {
                warn(
                    format!("{VALUE_COLORS_DOC}.{dimension}.{value}"),
                    format!(
                        "value colors '{dimension}': '{value}' names unknown color '{color}'; painted without a color"
                    ),
                );
                continue;
            }
            match inline {
                Some(definition) => out.insert_inline(dimension, value, definition.clone()),
                None => out.insert(dimension, value, color),
            }
        }
    }
    (out, diags)
}

/// One value's entries as the layers hold them, raw (`"none"` included).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ValueColorState {
    /// The color in force: the highest layer's entry, unless it is `none`.
    pub effective: Option<ValueEntry>,
    /// The user layer's entry.
    pub user: Option<ValueEntry>,
    /// The highest lower layer's entry (desk over builtin).
    pub lower: Option<ValueEntry>,
}

impl ValueColorState {
    /// The lower layer's entry the user layer overrides with a different
    /// one: what `Follow desk` would return to. A lower entry of
    /// [`NO_COLOR`] is none: following it writes what `None` writes (the
    /// user key removed), so a second row for it would only duplicate that.
    pub fn follow_desk(&self) -> Option<&ValueEntry> {
        match (&self.user, &self.lower) {
            (Some(user), Some(lower)) if user != lower && !lower.is_cleared() => Some(lower),
            _ => None,
        }
    }
}

/// Read `dimension.value` from the unmerged `value_colors` layers, in
/// merge order (`Config::layered_docs`). A string is a name, a table the
/// reader accepts is an inline entry; anything else is no entry.
pub fn value_color_state(layers: &[LayerDoc], dimension: &str, value: &str) -> ValueColorState {
    let mut state = ValueColorState::default();
    for doc in layers {
        let Some(entry) = doc
            .table
            .get(dimension)
            .and_then(|d| d.as_table())
            .and_then(|d| d.get(value))
            .and_then(|entry| entry_of(entry, dimension, value))
        else {
            continue;
        };
        if doc.layer == Layer::User {
            state.user = Some(entry);
        } else {
            state.lower = Some(entry);
        }
    }
    state.effective = state
        .user
        .clone()
        .or_else(|| state.lower.clone())
        .filter(|c| !c.is_cleared());
    state
}

/// A raw entry as a [`ValueEntry`]. A table the reader refuses paints
/// nothing, so it is no entry, like a number.
fn entry_of(entry: &toml::Value, dimension: &str, value: &str) -> Option<ValueEntry> {
    match entry {
        toml::Value::String(name) => Some(ValueEntry::Named(name.clone())),
        toml::Value::Table(table) => read_inline(
            table,
            &format!("{VALUE_COLORS_DOC}.{dimension}.{value}"),
            dimension,
            value,
        )
        .0
        .map(ValueEntry::Inline),
        _ => None,
    }
}

/// What the pick list offers for a value.
#[derive(Debug, Clone, PartialEq)]
pub enum ValuePick {
    Color(String),
    Inline(Definition),
    None,
    FollowDesk,
}

/// What a pick does to the user layer's entry.
#[derive(Debug, Clone, PartialEq)]
pub enum ValueWrite {
    Set(ValueEntry),
    Remove,
    Nothing,
}

/// The smallest user-layer change that makes `pick` the value's state. A
/// pick that changes nothing writes nothing; `None` writes `"none"` only
/// when a lower layer colors the value, since otherwise removing the user
/// entry already clears it.
pub fn value_write(state: &ValueColorState, pick: &ValuePick) -> ValueWrite {
    match pick {
        ValuePick::Color(name) => set_unless_in_force(state, ValueEntry::Named(name.clone())),
        ValuePick::Inline(definition) => {
            set_unless_in_force(state, ValueEntry::Inline(definition.clone()))
        }
        ValuePick::None => {
            if state.effective.is_none() {
                ValueWrite::Nothing
            } else if state.lower.as_ref().is_some_and(|c| !c.is_cleared()) {
                ValueWrite::Set(NO_COLOR.into())
            } else {
                ValueWrite::Remove
            }
        }
        ValuePick::FollowDesk => {
            if state.user.is_some() {
                ValueWrite::Remove
            } else {
                ValueWrite::Nothing
            }
        }
    }
}

/// Set `entry` unless it is the entry in force. An inline entry compares
/// by definition, so `{ hue = 210 }` over `{ hue = 210 }` writes nothing.
fn set_unless_in_force(state: &ValueColorState, entry: ValueEntry) -> ValueWrite {
    if state.effective.as_ref() == Some(&entry) {
        ValueWrite::Nothing
    } else {
        ValueWrite::Set(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, Severity, merge_docs};

    use crate::colour::{Definition, NamedColours, Tone};
    use crate::dimensions::DerivedDimensions;
    use crate::schema::SchemaSpec;

    const DEMO_DATASETS: &str = include_str!("../../../../examples/demo-config/datasets.toml");
    const DEMO_DIMENSIONS: &str = include_str!("../../../../examples/demo-config/dimensions.toml");

    fn doc(text: &str) -> crate::config::MergedDoc {
        merge_docs(
            VALUE_COLORS_DOC,
            &[LayerDoc::builtin(VALUE_COLORS_DOC, text).expect("fixture parses")],
        )
    }

    #[test]
    fn a_value_names_its_color() {
        let (values, diags) = ValueColors::from_doc(&doc(
            "[underlying_ref]\nSPX = \"blue\"\n\"SX5E Index\" = \"teal\"\n",
        ));
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(
            values.get("underlying_ref", "SPX").map(|c| &**c),
            Some("blue")
        );
        assert_eq!(
            values.get("underlying_ref", "SX5E Index").map(|c| &**c),
            Some("teal")
        );
        assert_eq!(values.get("underlying_ref", "NDX"), None, "unmapped");
        assert_eq!(values.get("underlying_ref", "spx"), None, "case-sensitive");
        assert_eq!(values.get("book", "SPX"), None, "another dimension");
        assert_eq!(values.dimensions().collect::<Vec<_>>(), ["underlying_ref"]);
    }

    #[test]
    fn none_reads_as_unmapped_without_a_diagnostic() {
        let (values, diags) =
            ValueColors::from_doc(&doc("[underlying_ref]\nSPX = \"none\"\nNDX = \"amber\"\n"));
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(values.get("underlying_ref", "SPX"), None);
        assert!(values.get("underlying_ref", "NDX").is_some());
    }

    #[test]
    fn refused_entries_are_dropped_with_an_error_at_their_path() {
        let (values, diags) = ValueColors::from_doc(&doc("config_version = 1\n\
             book = \"blue\"\n\
             [underlying_ref]\n\
             SPX = 3\n\
             NDX = \"sign\"\n\
             RUT = \"#ff0000\"\n\
             \"\" = \"blue\"\n\
             DAX = \"blue\"\n"));
        assert_eq!(
            values.get("underlying_ref", "DAX").map(|c| &**c),
            Some("blue")
        );
        for dropped in ["SPX", "NDX", "RUT", ""] {
            assert_eq!(values.get("underlying_ref", dropped), None, "{dropped:?}");
        }
        assert!(values.dimension("book").is_none());
        let paths: Vec<_> = diags.iter().map(|d| d.path.clone().unwrap()).collect();
        assert_eq!(
            paths,
            [
                "value_colors.book",
                "value_colors.underlying_ref.SPX",
                "value_colors.underlying_ref.NDX",
                "value_colors.underlying_ref.RUT",
                "value_colors.underlying_ref.",
            ]
        );
        assert!(diags.iter().all(|d| d.severity == Severity::Error));
        assert!(
            diags[2].message.contains("sign is a column color mode"),
            "{}",
            diags[2].message
        );
    }

    #[test]
    fn a_dimension_with_only_cleared_values_is_absent() {
        let (values, _) = ValueColors::from_doc(&doc("[underlying_ref]\nSPX = \"none\"\n"));
        assert!(values.is_empty());
        assert!(values.dimension("underlying_ref").is_none());
    }

    #[test]
    fn inline_entries_parse_and_refuse_at_their_path() {
        let (values, diags) = ValueColors::from_doc(&doc("[underlying_ref]\n\
             NDX = { hue = 210 }\n\
             RUT = { hue = 30, tone = \"light\" }\n\
             DAX = { token = \"warning\" }\n\
             WRAP = { hue = 360 }\n\
             ODD = { hue = 90, tone = \"pale\" }\n\
             BOTH = { hue = 1, token = \"danger\" }\n\
             NEITHER = { tone = \"light\" }\n\
             FAR = { hue = 400 }\n\
             SIGNED = { hue = 10, tint_sign = true }\n\
             TOK = { token = \"nope\" }\n"));
        let def = |value: &str| {
            values
                .get("underlying_ref", value)
                .and_then(|key| values.inline_definition(key))
                .cloned()
        };
        assert_eq!(def("NDX"), Some(Definition::hue(210.0, Tone::Normal)));
        assert_eq!(def("RUT"), Some(Definition::hue(30.0, Tone::Light)));
        assert_eq!(
            def("DAX"),
            Some(Definition::token(crate::colour::Token::Warning))
        );
        assert_eq!(
            def("WRAP"),
            Some(Definition::hue(0.0, Tone::Normal)),
            "360 is 0"
        );
        assert_eq!(
            def("ODD"),
            Some(Definition::hue(90.0, Tone::Normal)),
            "a bad tone warns and falls back"
        );
        assert_eq!(
            values.get("underlying_ref", "NDX").map(|k| &**k),
            Some("inline underlying_ref.NDX")
        );
        for dropped in ["BOTH", "NEITHER", "FAR", "SIGNED", "TOK"] {
            assert_eq!(values.get("underlying_ref", dropped), None, "{dropped}");
        }
        let errors: Vec<&str> = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .filter_map(|d| d.path.as_deref())
            .collect();
        assert_eq!(
            errors,
            [
                "value_colors.underlying_ref.BOTH",
                "value_colors.underlying_ref.NEITHER",
                "value_colors.underlying_ref.FAR.hue",
                "value_colors.underlying_ref.SIGNED",
                "value_colors.underlying_ref.TOK.token",
            ]
        );
        let warnings: Vec<&str> = diags
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .filter_map(|d| d.path.as_deref())
            .collect();
        assert_eq!(warnings, ["value_colors.underlying_ref.ODD.tone"]);
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("tint_sign is refused")),
            "{diags:?}"
        );
    }

    #[test]
    fn the_inline_key_can_never_be_a_color_name() {
        let key = inline_key("underlying_ref", "SPX");
        assert_eq!(key, "inline underlying_ref.SPX");
        assert!(crate::config::check_object_name(&key).is_err());
        let (colors, diags) = NamedColours::from_doc(&merge_docs(
            "colors",
            &[LayerDoc::builtin("colors", "[\"inline underlying_ref.SPX\"]\nhue = 1\n").unwrap()],
        ));
        assert!(colors.get(&key).is_none());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn the_check_keeps_an_inline_entry_and_prunes_its_dimension_by_kind() {
        let mut values = ValueColors::default();
        values.insert_inline(
            "underlying_ref",
            "NDX",
            Definition::hue(210.0, Tone::Normal),
        );
        values.insert_inline("strike", "5000", Definition::hue(30.0, Tone::Normal));
        let kind_of = |name: &str| match name {
            "underlying_ref" => DimensionKind::Text,
            _ => DimensionKind::NotText,
        };
        let (checked, diags) = check_value_colors(values, &NamedColours::default(), kind_of);
        let key = checked
            .get("underlying_ref", "NDX")
            .expect("an inline entry names no colors.toml color and is kept");
        assert_eq!(&**key, "inline underlying_ref.NDX");
        assert_eq!(
            checked.inline_definition(key),
            Some(&Definition::hue(210.0, Tone::Normal))
        );
        assert!(checked.dimension("strike").is_none());
        assert_eq!(
            checked.inline_definition("inline strike.5000"),
            None,
            "a pruned dimension takes its definitions with it"
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(
            diags[0]
                .message
                .contains("value colors apply to text dimensions")
        );
    }

    #[test]
    fn inline_keys_resolve_through_get_and_never_list() {
        let config = crate::config::Config::from_docs(vec![
            LayerDoc::builtin("datasets", DEMO_DATASETS).unwrap(),
            LayerDoc::builtin("colors", "[blue]\nhue = 240\n").unwrap(),
            LayerDoc::builtin(
                "value_colors",
                "[underlying_ref]\nSPX = \"blue\"\nNDX = { hue = 210 }\n",
            )
            .unwrap(),
        ]);
        let (colors, diags) = NamedColours::from_config(&config);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(
            colors.names().collect::<Vec<_>>(),
            ["blue"],
            "inline keys are not names"
        );
        let key = colors
            .values()
            .get("underlying_ref", "NDX")
            .expect("NDX is colored");
        assert_eq!(colors.get(key), Some(&Definition::hue(210.0, Tone::Normal)));
        assert_eq!(
            colors.values().get("underlying_ref", "SPX").map(|c| &**c),
            Some("blue")
        );
    }

    #[test]
    fn a_user_inline_hue_over_a_desk_inline_token_replaces_it_whole() {
        let config = crate::config::Config::from_docs(vec![
            LayerDoc::builtin("datasets", DEMO_DATASETS).unwrap(),
            layer(
                Layer::Desk,
                "[underlying_ref]\nNDX = { token = \"warning\" }\n",
            ),
            layer(Layer::User, "[underlying_ref]\nNDX = { hue = 30 }\n"),
        ]);
        let (colors, diags) = NamedColours::from_config(&config);
        assert!(diags.is_empty(), "{diags:?}");
        let key = colors
            .values()
            .get("underlying_ref", "NDX")
            .expect("NDX stays colored");
        assert_eq!(
            colors.get(key),
            Some(&Definition::hue(30.0, Tone::Normal)),
            "the user's hue, not a hue-and-token table refused as both"
        );
    }

    fn demo() -> (SchemaSpec, DerivedDimensions) {
        let schema = SchemaSpec::from_doc(&merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", DEMO_DATASETS).unwrap()],
        ))
        .0;
        let dims = DerivedDimensions::from_doc(&merge_docs(
            "dimensions",
            &[LayerDoc::builtin("dimensions", DEMO_DIMENSIONS).unwrap()],
        ))
        .0;
        (schema, dims)
    }

    fn named(names: &[&str]) -> NamedColours {
        let mut out = NamedColours::default();
        for name in names {
            out.insert(name.to_string(), Definition::hue(240.0, Tone::Normal));
        }
        out
    }

    #[test]
    fn a_dimension_is_text_not_text_or_undeclared() {
        let (schema, dims) = demo();
        // utf8, role = dimension.
        assert_eq!(
            dimension_kind(&schema, &dims, "underlying_ref"),
            DimensionKind::Text
        );
        // f64, role = dimension.
        assert_eq!(
            dimension_kind(&schema, &dims, "strike"),
            DimensionKind::NotText
        );
        // A derived dimension's values are labels.
        assert_eq!(dimension_kind(&schema, &dims, "desk"), DimensionKind::Text);
        assert_eq!(
            dimension_kind(&schema, &dims, "nonsense"),
            DimensionKind::Undeclared
        );
        let text = text_dimensions(&schema, &dims);
        assert!(text.contains("underlying_ref") && text.contains("desk"));
        assert!(!text.contains("strike"));
    }

    #[test]
    fn the_check_prunes_what_cannot_paint_and_says_why() {
        let mut values = ValueColors::default();
        values.insert("underlying_ref", "SPX", "blue");
        values.insert("underlying_ref", "NDX", "missing");
        values.insert("strike", "5000", "blue");
        values.insert("nonsense", "x", "blue");
        let kind_of = |name: &str| match name {
            "underlying_ref" => DimensionKind::Text,
            "strike" => DimensionKind::NotText,
            _ => DimensionKind::Undeclared,
        };
        let (checked, diags) = check_value_colors(values, &named(&["blue"]), kind_of);
        assert_eq!(
            checked.get("underlying_ref", "SPX").map(|c| &**c),
            Some("blue")
        );
        assert_eq!(checked.get("underlying_ref", "NDX"), None, "unknown color");
        assert!(
            checked.dimension("strike").is_none(),
            "not a text dimension"
        );
        assert!(checked.dimension("nonsense").is_none(), "undeclared");
        assert!(diags.iter().all(|d| d.severity == Severity::Warning));
        let said: Vec<(String, &str)> = diags
            .iter()
            .map(|d| (d.path.clone().unwrap(), d.message.as_str()))
            .collect();
        assert_eq!(said.len(), 3, "{said:?}");
        assert!(said.iter().any(|(p, m)| p == "value_colors.nonsense"
            && m.contains("no dataset declares dimension 'nonsense'")));
        assert!(said.iter().any(|(p, m)| p == "value_colors.strike"
            && m.contains("value colors apply to text dimensions")));
        assert!(
            said.iter()
                .any(|(p, m)| p == "value_colors.underlying_ref.NDX"
                    && m.contains("unknown color 'missing'"))
        );
    }

    #[test]
    fn from_config_carries_the_checked_mapping_with_the_definitions() {
        let config = crate::config::Config::from_docs(vec![
            LayerDoc::builtin("datasets", DEMO_DATASETS).unwrap(),
            LayerDoc::builtin("colors", "[blue]\nhue = 240\n").unwrap(),
            LayerDoc::builtin(
                "value_colors",
                "[underlying_ref]\nSPX = \"blue\"\nNDX = \"missing\"\n",
            )
            .unwrap(),
        ]);
        let (colors, diags) = NamedColours::from_config(&config);
        assert!(colors.get("blue").is_some());
        assert_eq!(
            colors.values().get("underlying_ref", "SPX").map(|c| &**c),
            Some("blue")
        );
        assert_eq!(colors.values().get("underlying_ref", "NDX"), None);
        assert_eq!(diags.len(), 1, "{diags:?}");
        // Without the documents, both are empty and nothing is said.
        let (empty, diags) = NamedColours::from_config(&crate::config::Config::default());
        assert!(empty.is_empty() && empty.values().is_empty() && diags.is_empty());
    }

    use crate::config::Layer;

    fn layer(layer: Layer, text: &str) -> LayerDoc {
        LayerDoc {
            layer,
            name: VALUE_COLORS_DOC.to_string(),
            file: "value_colors.toml".into(),
            table: text.parse().unwrap(),
        }
    }

    fn state(desk: Option<&str>, user: Option<&str>) -> ValueColorState {
        let entry = |c: &str| format!("[underlying_ref]\nSPX = \"{c}\"\n");
        let mut layers = Vec::new();
        if let Some(c) = desk {
            layers.push(layer(Layer::Desk, &entry(c)));
        }
        if let Some(c) = user {
            layers.push(layer(Layer::User, &entry(c)));
        }
        value_color_state(&layers, "underlying_ref", "SPX")
    }

    #[test]
    fn the_state_separates_the_user_entry_from_the_layers_below() {
        let named = |c: &str| Some(ValueEntry::from(c));
        let s = state(Some("blue"), Some("teal"));
        assert_eq!(s.user, named("teal"));
        assert_eq!(s.lower, named("blue"));
        assert_eq!(s.effective, named("teal"));
        assert_eq!(s.follow_desk(), Some(&ValueEntry::from("blue")));

        let s = state(Some("blue"), Some("none"));
        assert_eq!(s.effective, None, "the user cleared the desk's color");
        assert_eq!(s.follow_desk(), Some(&ValueEntry::from("blue")));

        let s = state(Some("blue"), None);
        assert_eq!(s.effective, named("blue"));
        assert_eq!(s.follow_desk(), None, "nothing to follow back to");

        let s = state(None, Some("teal"));
        assert_eq!(s.follow_desk(), None, "no lower entry");
        let s = state(Some("teal"), Some("teal"));
        assert_eq!(s.follow_desk(), None, "the same as the desk");
        let s = state(Some("none"), Some("teal"));
        assert_eq!(
            s.follow_desk(),
            None,
            "a desk with no color is the None row's own write, not a second row"
        );
        assert_eq!(state(None, None), ValueColorState::default());
    }

    #[test]
    fn a_pick_becomes_the_smallest_user_layer_write() {
        use ValuePick as P;
        use ValueWrite as W;
        let blue = || P::Color("blue".into());
        // A color: set it, unless it is already the effective one.
        assert_eq!(
            value_write(&state(None, None), &blue()),
            W::Set("blue".into())
        );
        assert_eq!(
            value_write(&state(Some("teal"), None), &blue()),
            W::Set("blue".into())
        );
        assert_eq!(value_write(&state(Some("blue"), None), &blue()), W::Nothing);
        assert_eq!(value_write(&state(None, Some("blue")), &blue()), W::Nothing);
        // None: `none` only when a lower layer colors it; else just remove.
        assert_eq!(
            value_write(&state(Some("blue"), None), &P::None),
            W::Set("none".into())
        );
        assert_eq!(
            value_write(&state(Some("blue"), Some("teal")), &P::None),
            W::Set("none".into())
        );
        assert_eq!(value_write(&state(None, Some("teal")), &P::None), W::Remove);
        assert_eq!(
            value_write(&state(Some("none"), Some("teal")), &P::None),
            W::Remove
        );
        assert_eq!(value_write(&state(None, None), &P::None), W::Nothing);
        assert_eq!(
            value_write(&state(Some("blue"), Some("none")), &P::None),
            W::Nothing
        );
        // Follow desk: drop the user entry, if there is one.
        assert_eq!(
            value_write(&state(Some("blue"), Some("teal")), &P::FollowDesk),
            W::Remove
        );
        assert_eq!(
            value_write(&state(Some("blue"), None), &P::FollowDesk),
            W::Nothing
        );
    }

    fn raw_state(desk: Option<&str>, user: Option<&str>) -> ValueColorState {
        let entry = |c: &str| format!("[underlying_ref]\nSPX = {c}\n");
        let mut layers = Vec::new();
        if let Some(c) = desk {
            layers.push(layer(Layer::Desk, &entry(c)));
        }
        if let Some(c) = user {
            layers.push(layer(Layer::User, &entry(c)));
        }
        value_color_state(&layers, "underlying_ref", "SPX")
    }

    #[test]
    fn an_inline_entry_reads_into_the_layer_state() {
        let s = raw_state(
            Some("{ token = \"warning\" }"),
            Some("{ hue = 30, tone = \"light\" }"),
        );
        let light = ValueEntry::Inline(Definition::hue(30.0, Tone::Light));
        let warning = ValueEntry::Inline(Definition::token(crate::colour::Token::Warning));
        assert_eq!(s.user, Some(light.clone()));
        assert_eq!(s.lower, Some(warning.clone()));
        assert_eq!(s.effective, Some(light));
        assert_eq!(s.follow_desk(), Some(&warning));
        // A table the reader refuses, and a number, are no entry.
        let s = raw_state(Some("\"blue\""), Some("{ hue = 1, token = \"danger\" }"));
        assert_eq!(s.user, None);
        assert_eq!(s.effective, Some(ValueEntry::from("blue")));
        assert_eq!(raw_state(None, Some("3")), ValueColorState::default());
    }

    #[test]
    fn an_inline_pick_writes_unless_it_is_the_entry_in_force() {
        use ValuePick as P;
        use ValueWrite as W;
        let hue = |d: f32| Definition::hue(d, Tone::Normal);
        let user_210 = raw_state(None, Some("{ hue = 210 }"));
        assert_eq!(value_write(&user_210, &P::Inline(hue(210.0))), W::Nothing);
        assert_eq!(
            value_write(&user_210, &P::Inline(hue(211.0))),
            W::Set(ValueEntry::Inline(hue(211.0)))
        );
        assert_eq!(
            value_write(&user_210, &P::Color("blue".into())),
            W::Set("blue".into())
        );
        assert_eq!(value_write(&user_210, &P::None), W::Remove);
        let desk_210 = raw_state(Some("{ hue = 210 }"), None);
        assert_eq!(value_write(&desk_210, &P::None), W::Set("none".into()));
        assert_eq!(
            value_write(&raw_state(None, None), &P::Inline(hue(0.0))),
            W::Set(ValueEntry::Inline(hue(0.0)))
        );
    }

    #[test]
    fn labels_name_a_color_or_its_hue_or_token() {
        assert_eq!(ValueEntry::from("blue").label(), "blue");
        assert_eq!(
            inline_label(&Definition::hue(210.0, Tone::Normal)),
            "hue 210"
        );
        assert_eq!(
            inline_label(&Definition::hue(30.0, Tone::Light)),
            "hue 30 light"
        );
        assert_eq!(
            inline_label(&Definition::token(crate::colour::Token::Warning)),
            "warning"
        );
        assert_eq!(
            ValueEntry::Inline(Definition::hue(210.0, Tone::Normal)).label(),
            "hue 210"
        );
    }

    #[test]
    fn preset_matching_is_hue_and_normal_tone_only() {
        assert_eq!(
            PRESETS.map(|(_, d)| d),
            [0, 30, 60, 90, 120, 150, 180, 210, 240, 270, 300, 330]
        );
        assert_eq!(
            preset_of(&Definition::hue(210.0, Tone::Normal)),
            Some("azure")
        );
        assert_eq!(preset_of(&Definition::hue(0.0, Tone::Normal)), Some("red"));
        assert_eq!(
            preset_of(&Definition::hue(330.0, Tone::Normal)),
            Some("rose")
        );
        assert_eq!(preset_of(&Definition::hue(215.0, Tone::Normal)), None);
        assert_eq!(preset_of(&Definition::hue(210.0, Tone::Light)), None);
        assert_eq!(
            preset_of(&Definition::hue(210.0, Tone::Normal).tinted()),
            None
        );
        assert_eq!(
            preset_of(&Definition::token(crate::colour::Token::Info)),
            None
        );
    }

    #[test]
    fn a_name_spelled_like_another_values_inline_key_borrows_nothing() {
        let mut values = ValueColors::default();
        values.insert_inline(
            "underlying_ref",
            "SPX",
            Definition::hue(210.0, Tone::Normal),
        );
        values.insert("underlying_ref", "NDX", "inline underlying_ref.SPX");
        let kind_of = |_: &str| DimensionKind::Text;
        let (checked, diags) = check_value_colors(values, &NamedColours::default(), kind_of);
        assert!(checked.get("underlying_ref", "SPX").is_some(), "SPX's own");
        assert_eq!(
            checked.get("underlying_ref", "NDX"),
            None,
            "NDX names a color, not SPX's inline entry"
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(
            diags[0].path.as_deref(),
            Some("value_colors.underlying_ref.NDX")
        );
        assert!(diags[0].message.contains("unknown color"), "{diags:?}");
    }
}
