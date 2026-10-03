//! Value colors: a text dimension's value mapped to a named color, so the
//! cell showing `SPX` paints in SPX's color wherever it appears. The
//! document is `value_colors.toml`; a color is a `colors.toml` name.
//! Everything here is pure: the reader, the check against the schema and
//! the color definitions, and the layer arithmetic behind the pick list.

use crate::colour::{NamedColours, RESERVED_PREFIX};
use crate::config::{Diagnostic, Layer, LayerDoc, MergedDoc, Severity, VALUE_COLORS_DOC};
use crate::dimensions::DerivedDimensions;
use crate::schema::{ColumnRole, ColumnType, SchemaSpec};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// The entry that means "no color": how a higher layer clears a lower
/// layer's color. It reads as an unmapped value.
pub const NO_COLOR: &str = "none";

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
}

impl ValueColors {
    /// Read the merged `value_colors` document. Knows nothing of the schema
    /// or the color definitions; [`check_value_colors`] does.
    pub fn from_doc(doc: &MergedDoc) -> (ValueColors, Vec<Diagnostic>) {
        let mut out = ValueColors::default();
        let mut diags = Vec::new();
        let mut refuse = |path: String, message: String| {
            diags.push(Diagnostic {
                severity: Severity::Error,
                layer: None,
                file: None,
                message,
                path: Some(path),
            })
        };
        for (dimension, entry) in &doc.value {
            if dimension == "config_version" {
                continue;
            }
            let Some(table) = entry.as_table() else {
                refuse(
                    format!("{VALUE_COLORS_DOC}.{dimension}"),
                    format!(
                        "value colors '{dimension}': not a table of value = \"color\" entries; dropped"
                    ),
                );
                continue;
            };
            for (value, color) in table {
                let path = format!("{VALUE_COLORS_DOC}.{dimension}.{value}");
                let Some(name) = color.as_str() else {
                    refuse(
                        path,
                        format!(
                            "value colors '{dimension}': '{value}' must name a color (got {color}); dropped"
                        ),
                    );
                    continue;
                };
                if name == "sign" {
                    refuse(
                        path,
                        format!(
                            "value colors '{dimension}': '{value}': sign is a column color mode, not a color; dropped"
                        ),
                    );
                    continue;
                }
                if name.starts_with(RESERVED_PREFIX) {
                    refuse(
                        path,
                        format!(
                            "value colors '{dimension}': '{value}': absolute colors are not accepted here, name a color from colors.toml; dropped"
                        ),
                    );
                    continue;
                }
                if value.is_empty() {
                    refuse(
                        path,
                        format!("value colors '{dimension}': an empty value; dropped"),
                    );
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
        self.by_dimension
            .entry(dimension.to_string())
            .or_default()
            .by_value
            .insert(value.to_string(), color.into());
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
/// `named` does not define. What is returned is exactly what a tile may
/// look up, so paint needs no second validity check.
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
            if named.get(color).is_none() {
                warn(
                    format!("{VALUE_COLORS_DOC}.{dimension}.{value}"),
                    format!(
                        "value colors '{dimension}': '{value}' names unknown color '{color}'; painted without a color"
                    ),
                );
                continue;
            }
            out.insert(dimension, value, color);
        }
    }
    (out, diags)
}

/// One value's entries as the layers hold them, raw (`"none"` included).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValueColorState {
    /// The color in force: the highest layer's entry, unless it is `none`.
    pub effective: Option<String>,
    /// The user layer's entry.
    pub user: Option<String>,
    /// The highest lower layer's entry (desk over builtin).
    pub lower: Option<String>,
}

impl ValueColorState {
    /// The lower layer's entry the user layer overrides with a different
    /// one: what `Follow desk` would return to. A lower entry of
    /// [`NO_COLOR`] is none: following it writes what `None` writes (the
    /// user key removed), so a second row for it would only duplicate that.
    pub fn follow_desk(&self) -> Option<&str> {
        match (&self.user, &self.lower) {
            (Some(user), Some(lower)) if user != lower && lower != NO_COLOR => Some(lower),
            _ => None,
        }
    }
}

/// Read `dimension.value` from the unmerged `value_colors` layers, in
/// merge order (`Config::layered_docs`). A non-string entry is no entry.
pub fn value_color_state(layers: &[LayerDoc], dimension: &str, value: &str) -> ValueColorState {
    let mut state = ValueColorState::default();
    for doc in layers {
        let Some(entry) = doc
            .table
            .get(dimension)
            .and_then(|d| d.as_table())
            .and_then(|d| d.get(value))
            .and_then(|c| c.as_str())
        else {
            continue;
        };
        if doc.layer == Layer::User {
            state.user = Some(entry.to_string());
        } else {
            state.lower = Some(entry.to_string());
        }
    }
    state.effective = state
        .user
        .clone()
        .or_else(|| state.lower.clone())
        .filter(|c| c != NO_COLOR);
    state
}

/// What the pick list offers for a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValuePick {
    Color(String),
    None,
    FollowDesk,
}

/// What a pick does to the user layer's entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueWrite {
    Set(String),
    Remove,
    Nothing,
}

/// The smallest user-layer change that makes `pick` the value's state. A
/// pick that changes nothing writes nothing; `None` writes `"none"` only
/// when a lower layer colors the value, since otherwise removing the user
/// entry already clears it.
pub fn value_write(state: &ValueColorState, pick: &ValuePick) -> ValueWrite {
    match pick {
        ValuePick::Color(name) => {
            if state.effective.as_deref() == Some(name.as_str()) {
                ValueWrite::Nothing
            } else {
                ValueWrite::Set(name.clone())
            }
        }
        ValuePick::None => {
            if state.effective.is_none() {
                ValueWrite::Nothing
            } else if state.lower.as_deref().is_some_and(|c| c != NO_COLOR) {
                ValueWrite::Set(NO_COLOR.to_string())
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
        let s = state(Some("blue"), Some("teal"));
        assert_eq!(s.user.as_deref(), Some("teal"));
        assert_eq!(s.lower.as_deref(), Some("blue"));
        assert_eq!(s.effective.as_deref(), Some("teal"));
        assert_eq!(s.follow_desk(), Some("blue"));

        let s = state(Some("blue"), Some("none"));
        assert_eq!(s.effective, None, "the user cleared the desk's color");
        assert_eq!(s.follow_desk(), Some("blue"));

        let s = state(Some("blue"), None);
        assert_eq!(s.effective.as_deref(), Some("blue"));
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
}
