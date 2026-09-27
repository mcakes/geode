//! Configurable package templates define each leg's weight, strike and expiry index,
//! and option kind. The `pricer_templates` document supplies the tables, with seven
//! templates in its builtin layer. Packages retain their template names independently
//! of the configured tables. Parsing expands the resolved table over typed values;
//! shorthand rendering uses the name only while the legs still match its current
//! table, otherwise printing each leg on a separate line.

use geode_core::config::{Diagnostic, MergedDoc, Severity};
use geode_core::pricing::OptionKind;

/// A package's template name, independent of the configured tables.
/// Names from configuration and stored sheets are interned for the process lifetime,
/// allowing this type and `RowKind` to remain `Copy`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Template(&'static str);

impl std::fmt::Debug for Template {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl Template {
    /// A package formed by grouping root lines, rendered as individual legs.
    /// This reserved name has no template table.
    pub const CUSTOM: Template = Template("CUSTOM");
    pub const CS: Template = Template("CS");
    pub const PS: Template = Template("PS");
    pub const STRD: Template = Template("STRD");
    pub const STRG: Template = Template("STRG");
    pub const RR: Template = Template("RR");
    pub const FLY: Template = Template("FLY");
    pub const CAL: Template = Template("CAL");

    /// Upper-cased and interned; the built-in names allocate nothing.
    pub fn named(name: &str) -> Template {
        use std::sync::{Mutex, OnceLock};
        const KNOWN: [Template; 8] = [
            Template::CUSTOM,
            Template::CS,
            Template::PS,
            Template::STRD,
            Template::STRG,
            Template::RR,
            Template::FLY,
            Template::CAL,
        ];
        if let Some(t) = KNOWN.iter().find(|t| t.0.eq_ignore_ascii_case(name)) {
            return *t;
        }
        static NAMES: OnceLock<Mutex<Vec<&'static str>>> = OnceLock::new();
        let upper = name.to_ascii_uppercase();
        let mut names = NAMES
            .get_or_init(Default::default)
            .lock()
            .expect("the interner never panics while held");
        if let Some(n) = names.iter().find(|n| **n == upper) {
            return Template(n);
        }
        let leaked: &'static str = Box::leak(upper.into_boxed_str());
        names.push(leaked);
        Template(leaked)
    }

    pub fn token(self) -> &'static str {
        self.0
    }

    /// The lower-case spelling `pricer_sheets.template` stores.
    pub fn storage_name(self) -> String {
        self.0.to_ascii_lowercase()
    }

    pub fn is_custom(self) -> bool {
        self == Template::CUSTOM
    }
}

/// One leg of a template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegSpec {
    /// Sign and ratio: `+1`, `-1`, `-2` (the fly's body). Never zero.
    pub weight: i64,
    /// Index into the typed strikes.
    pub strike: usize,
    /// Index into the typed expiries (`0` on every table but `CAL`).
    pub expiry: usize,
    pub kind: OptionKind,
}

pub const PRICER_TEMPLATES_DOC: &str = "pricer_templates";

/// The built-in tables in config form, installed in the builtin layer.
/// A desk or user entry of the same name replaces the whole entry.
pub const BUILTIN_TEMPLATES: &str = r#"[CS]
legs = [ { weight = 1, strike = 1, kind = "C" }, { weight = -1, strike = 2, kind = "C" } ]

[PS]
legs = [ { weight = 1, strike = 1, kind = "P" }, { weight = -1, strike = 2, kind = "P" } ]

[STRD]
legs = [ { weight = 1, strike = 1, kind = "C" }, { weight = 1, strike = 1, kind = "P" } ]

[STRG]
legs = [ { weight = 1, strike = 1, kind = "P" }, { weight = 1, strike = 2, kind = "C" } ]

[RR]
legs = [ { weight = -1, strike = 1, kind = "P" }, { weight = 1, strike = 2, kind = "C" } ]

[FLY]
legs = [
  { weight = 1, strike = 1, kind = "C" },
  { weight = -2, strike = 2, kind = "C" },
  { weight = 1, strike = 3, kind = "C" },
]

# +far -near calls on one strike; E1/E2 is near/far.
[CAL]
legs = [
  { weight = 1, strike = 1, expiry = 2, kind = "C" },
  { weight = -1, strike = 1, expiry = 1, kind = "C" },
]
"#;

/// A template as the parser and printer use it: its legs with 0-based
/// indices, and how many strikes and expiries its shorthand takes.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateDef {
    /// Upper-case.
    pub name: String,
    pub legs: Vec<LegSpec>,
    pub strikes: usize,
    pub expiries: usize,
}

/// Every loaded template, in document order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TemplateSet {
    defs: Vec<TemplateDef>,
}

/// The longest template name, in characters. The pricer tree column is
/// sized so a tag this long fits.
pub const MAX_TEMPLATE_NAME: usize = 8;

/// Validate a configured table name and return its uppercase spelling.
/// Names contain 1 to [`MAX_TEMPLATE_NAME`] ASCII letters or digits and start
/// with a letter. `C`, `P`, and `CUSTOM` are reserved for single legs and grouped
/// packages. Storage accepts unresolved names without this validation.
pub fn check_name(name: &str) -> Result<String, String> {
    let upper = name.to_ascii_uppercase();
    let mut chars = upper.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
    if !first_ok || !chars.all(|c| c.is_ascii_alphanumeric()) || upper.len() > MAX_TEMPLATE_NAME {
        return Err(format!(
            "'{name}' is not a template name (1 to {MAX_TEMPLATE_NAME} letters or digits, a letter first)"
        ));
    }
    if matches!(upper.as_str(), "C" | "P" | "CUSTOM") {
        return Err(format!("'{upper}' is reserved"));
    }
    Ok(upper)
}

impl TemplateSet {
    /// Read entries independently, dropping invalid entries with path-specific
    /// errors and ignoring unknown keys with warnings. Names compare without case:
    /// a later valid `rr` replaces an earlier `RR` in its original document position
    /// and emits a warning. The document merge preserves both spellings because
    /// its own keys are case-sensitive.
    pub fn from_doc(doc: &MergedDoc) -> (TemplateSet, Vec<Diagnostic>) {
        TemplateSet::from_doc_over(doc, &TemplateSet::default(), "previous")
    }

    /// [`from_doc`](Self::from_doc), keeping the last valid state per
    /// name: an entry dropped with an Error keeps `previous`'s definition
    /// of that name, if it has one, in the entry's doc-order position,
    /// with a Warning at the entry's path. A name absent from `doc` is
    /// removed as usual. `previous` is the running set on a reload and
    /// the builtin set at startup, so a bad desk `RR` falls back to the
    /// builtin one instead of disappearing. `previous_is` names that set
    /// in the Warning ("keeping the {previous_is} definition"): "previous"
    /// on a reload, "built-in" at startup.
    pub fn from_doc_over(
        doc: &MergedDoc,
        previous: &TemplateSet,
        previous_is: &str,
    ) -> (TemplateSet, Vec<Diagnostic>) {
        let mut out = TemplateSet::default();
        let mut diags = Vec::new();
        // Upper-cased name -> (its position in `out.defs`, the spelling on
        // record), so a later case-insensitive repeat can replace in place.
        let mut seen: std::collections::HashMap<String, (usize, String)> =
            std::collections::HashMap::new();
        for (name, value) in &doc.value {
            if name == "config_version" {
                continue;
            }
            let path = |suffix: &str| {
                if suffix.is_empty() {
                    format!("{PRICER_TEMPLATES_DOC}.{name}")
                } else {
                    format!("{PRICER_TEMPLATES_DOC}.{name}.{suffix}")
                }
            };
            let report = |severity: Severity, suffix: &str, m: String| Diagnostic {
                severity,
                layer: None,
                file: None,
                message: format!("pricer template '{name}': {m}"),
                path: Some(path(suffix)),
            };
            let upper = match check_name(name) {
                Ok(u) => u,
                Err(m) => {
                    diags.push(report(Severity::Error, "", format!("{m}; dropped")));
                    continue;
                }
            };
            // `None`: the entry was dropped with an Error.
            let read: Option<TemplateDef> = 'entry: {
                let Some(table) = value.as_table() else {
                    diags.push(report(Severity::Error, "", "not a table; dropped".into()));
                    break 'entry None;
                };
                for key in table.keys().filter(|k| *k != "legs") {
                    diags.push(report(
                        Severity::Warning,
                        key,
                        format!("unknown key '{key}' ignored"),
                    ));
                }
                let Some(raw) = table.get("legs").and_then(|v| v.as_array()) else {
                    diags.push(report(
                        Severity::Error,
                        "legs",
                        "missing 'legs' array; dropped".into(),
                    ));
                    break 'entry None;
                };
                let mut legs = Vec::with_capacity(raw.len());
                let mut bad = false;
                for (i, leg) in raw.iter().enumerate() {
                    match read_leg(leg, raw.len()) {
                        Ok((spec, unknown)) => {
                            for key in unknown {
                                diags.push(report(
                                    Severity::Warning,
                                    &format!("legs.{i}.{key}"),
                                    format!("unknown key '{key}' ignored"),
                                ));
                            }
                            legs.push(spec);
                        }
                        Err((key, m)) => {
                            let suffix = match key {
                                Some(k) => format!("legs.{i}.{k}"),
                                None => format!("legs.{i}"),
                            };
                            diags.push(report(Severity::Error, &suffix, format!("{m}; dropped")));
                            bad = true;
                            break;
                        }
                    }
                }
                if bad {
                    break 'entry None;
                }
                if legs.len() < 2 {
                    diags.push(report(
                        Severity::Error,
                        "legs",
                        "needs at least two legs; dropped".into(),
                    ));
                    break 'entry None;
                }
                let strikes = legs.iter().map(|l| l.strike + 1).max().unwrap_or(0);
                let expiries = legs.iter().map(|l| l.expiry + 1).max().unwrap_or(0);
                let covers = |n: usize, used: &dyn Fn(&LegSpec) -> usize| {
                    (0..n).all(|k| legs.iter().any(|l| used(l) == k))
                };
                if !covers(strikes, &|l| l.strike) || !covers(expiries, &|l| l.expiry) {
                    diags.push(report(
                        Severity::Error,
                        "legs",
                        "strike and expiry numbers must run 1, 2, … with no gap; dropped".into(),
                    ));
                    break 'entry None;
                }
                Some(TemplateDef {
                    name: upper.clone(),
                    legs,
                    strikes,
                    expiries,
                })
            };
            // Keep-last-valid, per name: a bad entry keeps the name's
            // previous definition rather than vanishing (the merge
            // replaced the lower layer's whole entry with this one).
            let def = match read {
                Some(def) => def,
                None => match previous.resolve(&upper) {
                    Some(kept) => {
                        diags.push(report(
                            Severity::Warning,
                            "",
                            format!("keeping the {previous_is} definition"),
                        ));
                        kept.clone()
                    }
                    None => continue,
                },
            };
            if let Some((idx, earlier)) = seen.get(&upper) {
                diags.push(report(
                    Severity::Warning,
                    "",
                    format!("replaces '{earlier}' (template names are case-insensitive)"),
                ));
                out.defs[*idx] = def;
                seen.insert(upper, (*idx, name.clone()));
            } else {
                seen.insert(upper, (out.defs.len(), name.clone()));
                out.defs.push(def);
            }
        }
        (out, diags)
    }

    /// Parse the seven builtin tables from [`BUILTIN_TEMPLATES`].
    /// Malformed embedded TOML is a programming error and panics.
    pub fn builtin() -> TemplateSet {
        let doc = geode_core::config::LayerDoc::builtin(PRICER_TEMPLATES_DOC, BUILTIN_TEMPLATES)
            .expect("BUILTIN_TEMPLATES is well-formed TOML");
        TemplateSet::from_doc(&geode_core::config::merge_docs(
            PRICER_TEMPLATES_DOC,
            &[doc],
        ))
        .0
    }

    pub fn resolve(&self, token: &str) -> Option<&TemplateDef> {
        self.defs
            .iter()
            .find(|d| d.name.eq_ignore_ascii_case(token))
    }

    pub fn iter(&self) -> impl Iterator<Item = &TemplateDef> {
        self.defs.iter()
    }
}

/// The offending key (`None`: the leg itself) and why reading it failed.
type LegError = (Option<&'static str>, String);

/// One leg: the spec with 0-based indices and the unknown keys, or the
/// offending key (`None`: the leg itself) and why. `leg_count` bounds a
/// strike or expiry number: covering `1..=n` with no gap needs at least `n`
/// legs, so a number above the entry's own leg count can never be valid and
/// is rejected here rather than walking the gap-check loop up to it.
fn read_leg(v: &toml::Value, leg_count: usize) -> Result<(LegSpec, Vec<String>), LegError> {
    let t = v
        .as_table()
        .ok_or((None, "a leg must be a table".to_string()))?;
    let weight = t
        .get("weight")
        .and_then(|w| w.as_integer())
        .ok_or((Some("weight"), "missing or not an integer".to_string()))?;
    if weight == 0 {
        return Err((Some("weight"), "must not be zero".into()));
    }
    let index = |key: &'static str, default: Option<i64>| -> Result<usize, LegError> {
        let n = match t.get(key) {
            Some(v) => v
                .as_integer()
                .ok_or((Some(key), "not an integer".to_string()))?,
            None => default.ok_or((Some(key), "missing".to_string()))?,
        };
        if n < 1 {
            return Err((Some(key), "must be 1 or more".into()));
        }
        if n as usize > leg_count {
            return Err((Some(key), "above the number of legs".into()));
        }
        Ok((n - 1) as usize)
    };
    let strike = index("strike", None)?;
    let expiry = index("expiry", Some(1))?;
    let kind = match t
        .get("kind")
        .and_then(|k| k.as_str())
        .map(str::to_ascii_uppercase)
        .as_deref()
    {
        Some("C") => OptionKind::Call,
        Some("P") => OptionKind::Put,
        _ => return Err((Some("kind"), "must be \"C\" or \"P\"".into())),
    };
    let unknown = t
        .keys()
        .filter(|k| !matches!(k.as_str(), "weight" | "strike" | "expiry" | "kind"))
        .cloned()
        .collect();
    Ok((
        LegSpec {
            weight,
            strike,
            expiry,
            kind,
        },
        unknown,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::pricing::OptionKind;

    fn leg(weight: i64, strike: usize, expiry: usize, kind: OptionKind) -> LegSpec {
        LegSpec {
            weight,
            strike,
            expiry,
            kind,
        }
    }

    fn set(toml: &str) -> (TemplateSet, Vec<geode_core::config::Diagnostic>) {
        let doc = geode_core::config::LayerDoc::builtin(PRICER_TEMPLATES_DOC, toml).unwrap();
        TemplateSet::from_doc(&geode_core::config::merge_docs(
            PRICER_TEMPLATES_DOC,
            &[doc],
        ))
    }

    /// Keep-last-valid, per name: a bad `RR` keeps the previous `RR` in
    /// its own doc-order slot; a doc with no `RR` removes it.
    #[test]
    fn a_bad_entry_keeps_the_previous_definition_and_an_absent_one_is_removed() {
        use geode_core::config::{LayerDoc, Severity, merge_docs};
        let previous = TemplateSet::builtin();
        let doc = |toml: &str| {
            merge_docs(
                PRICER_TEMPLATES_DOC,
                &[LayerDoc::builtin(PRICER_TEMPLATES_DOC, toml).unwrap()],
            )
        };
        let bad = doc(
            "[CS]\nlegs = [ { weight = 1, strike = 1, kind = \"C\" }, { weight = -1, strike = 2, kind = \"C\" } ]\n\
             [RR]\nlegs = [ { weight = 0, strike = 1, kind = \"P\" }, { weight = 1, strike = 2, kind = \"C\" } ]\n\
             [PS]\nlegs = [ { weight = 1, strike = 1, kind = \"P\" }, { weight = -1, strike = 2, kind = \"P\" } ]\n",
        );
        let (s, diags) = TemplateSet::from_doc_over(&bad, &previous, "previous");
        let names: Vec<&str> = s.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            ["CS", "RR", "PS"],
            "the kept RR takes its entry's slot"
        );
        assert_eq!(s.resolve("RR"), previous.resolve("RR"));
        let errors: Vec<_> = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        let warnings: Vec<_> = diags
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .collect();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(warnings.len(), 1, "{diags:?}");
        assert!(
            warnings[0]
                .message
                .contains("keeping the previous definition"),
            "{diags:?}"
        );
        assert_eq!(warnings[0].path.as_deref(), Some("pricer_templates.RR"));

        // Without a previous RR, the bad one is simply dropped.
        let (s, _) = TemplateSet::from_doc(&bad);
        assert!(s.resolve("RR").is_none());

        // Absent from the doc: removed, whatever the previous set held.
        let (s, diags) = TemplateSet::from_doc_over(
            &doc(
                "[CS]\nlegs = [ { weight = 1, strike = 1, kind = \"C\" }, { weight = -1, strike = 2, kind = \"C\" } ]\n",
            ),
            &previous,
            "previous",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(s.resolve("RR").is_none());
    }

    #[test]
    fn the_builtin_document_is_exactly_the_seven_tables() {
        let (s, diags) = set(BUILTIN_TEMPLATES);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(s, TemplateSet::builtin());
        let names: Vec<&str> = s.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["CS", "PS", "STRD", "STRG", "RR", "FLY", "CAL"]);
        // Builtin definitions: (name, legs, strikes, expiries).
        use OptionKind::{Call, Put};
        let want: [(&str, Vec<LegSpec>, usize, usize); 7] = [
            ("CS", vec![leg(1, 0, 0, Call), leg(-1, 1, 0, Call)], 2, 1),
            ("PS", vec![leg(1, 0, 0, Put), leg(-1, 1, 0, Put)], 2, 1),
            ("STRD", vec![leg(1, 0, 0, Call), leg(1, 0, 0, Put)], 1, 1),
            ("STRG", vec![leg(1, 0, 0, Put), leg(1, 1, 0, Call)], 2, 1),
            ("RR", vec![leg(-1, 0, 0, Put), leg(1, 1, 0, Call)], 2, 1),
            (
                "FLY",
                vec![leg(1, 0, 0, Call), leg(-2, 1, 0, Call), leg(1, 2, 0, Call)],
                3,
                1,
            ),
            ("CAL", vec![leg(1, 0, 1, Call), leg(-1, 0, 0, Call)], 1, 2),
        ];
        for (name, legs, strikes, expiries) in want {
            let d = s.resolve(name).unwrap();
            assert_eq!(d.legs, legs, "{name}");
            assert_eq!((d.strikes, d.expiries), (strikes, expiries), "{name}");
        }
    }

    #[test]
    fn a_user_condor_loads_with_one_based_indices_made_zero_based() {
        let (s, diags) = set(r#"[condor]
legs = [
  { weight = 1, strike = 1, kind = "C" },
  { weight = -1, strike = 2, kind = "c" },
  { weight = -1, strike = 3, kind = "C" },
  { weight = 1, strike = 4, kind = "C" },
]"#);
        assert!(diags.is_empty(), "{diags:?}");
        let d = s.resolve("Condor").unwrap();
        assert_eq!(d.name, "CONDOR");
        assert_eq!((d.strikes, d.expiries), (4, 1));
        assert_eq!(
            d.legs[1],
            LegSpec {
                weight: -1,
                strike: 1,
                expiry: 0,
                kind: OptionKind::Call
            }
        );
    }

    #[test]
    fn each_rule_drops_only_its_own_entry_with_a_path() {
        let (s, diags) = set(r#"[OK]
legs = [ { weight = 1, strike = 1, kind = "C" }, { weight = -1, strike = 2, kind = "C" } ]
[C]
legs = [ { weight = 1, strike = 1, kind = "C" }, { weight = -1, strike = 2, kind = "C" } ]
[TOOLONGNAME]
legs = [ { weight = 1, strike = 1, kind = "C" }, { weight = -1, strike = 2, kind = "C" } ]
[ONE]
legs = [ { weight = 1, strike = 1, kind = "C" } ]
[ZERO]
legs = [ { weight = 0, strike = 1, kind = "C" }, { weight = 1, strike = 2, kind = "C" } ]
[GAP]
legs = [ { weight = 1, strike = 1, kind = "C" }, { weight = -1, strike = 1, kind = "C" }, { weight = 1, strike = 3, kind = "C" } ]
[EGAP]
legs = [ { weight = 1, strike = 1, expiry = 2, kind = "C" }, { weight = -1, strike = 1, expiry = 2, kind = "C" } ]
[KIND]
legs = [ { weight = 1, strike = 1, kind = "X" }, { weight = -1, strike = 2, kind = "C" } ]
[NOTTABLE]
legs = 3
[LEGBAD]
legs = [ 1, 2 ]
[BIG]
legs = [ { weight = 1, strike = 100000000, kind = "C" }, { weight = -1, strike = 2, kind = "C" } ]
"#);
        let names: Vec<&str> = s.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["OK"]);
        let paths: Vec<String> = diags.iter().filter_map(|d| d.path.clone()).collect();
        for want in [
            "pricer_templates.C",
            "pricer_templates.TOOLONGNAME",
            "pricer_templates.ONE.legs",
            "pricer_templates.ZERO.legs.0.weight",
            "pricer_templates.GAP.legs",
            "pricer_templates.EGAP.legs",
            "pricer_templates.KIND.legs.0.kind",
            "pricer_templates.NOTTABLE.legs",
            "pricer_templates.LEGBAD.legs.0",
            "pricer_templates.BIG.legs.0.strike",
        ] {
            assert!(paths.iter().any(|p| p == want), "{want} in {paths:?}");
        }
        assert!(
            diags
                .iter()
                .all(|d| d.severity == geode_core::config::Severity::Error)
        );
    }

    #[test]
    fn a_case_insensitive_repeat_replaces_the_earlier_entry_in_place() {
        // The merge matches document keys case-sensitively, so a builtin
        // `[RR]` and a user `[rr]` both reach `from_doc`; the later one must
        // win, in the earlier one's doc-order slot, with a warning.
        let builtin = geode_core::config::LayerDoc::builtin(
            PRICER_TEMPLATES_DOC,
            "[RR]\nlegs = [ { weight = -1, strike = 1, kind = \"P\" }, { weight = 1, strike = 2, kind = \"C\" } ]\n",
        )
        .unwrap();
        let mut user = geode_core::config::LayerDoc::builtin(
            PRICER_TEMPLATES_DOC,
            "[rr]\nlegs = [ { weight = 1, strike = 1, kind = \"P\" }, { weight = -1, strike = 2, kind = \"C\" } ]\n",
        )
        .unwrap();
        user.layer = geode_core::config::Layer::User;
        let merged = geode_core::config::merge_docs(PRICER_TEMPLATES_DOC, &[builtin, user]);
        let (s, diags) = TemplateSet::from_doc(&merged);
        let names: Vec<&str> = s.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["RR"], "one entry, in the earlier slot");
        let d = s.resolve("RR").unwrap();
        assert_eq!(
            d.legs[0],
            LegSpec {
                weight: 1,
                strike: 0,
                expiry: 0,
                kind: OptionKind::Put
            },
            "the later (user) legs win"
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].severity, geode_core::config::Severity::Warning);
        assert_eq!(diags[0].path.as_deref(), Some("pricer_templates.rr"));
        assert_eq!(
            diags[0].message,
            "pricer template 'rr': replaces 'RR' (template names are case-insensitive)",
            "the same prefix as every other template diagnostic"
        );
    }

    #[test]
    fn an_unknown_key_warns_and_keeps_the_entry() {
        let (s, diags) = set(r#"[W]
legs = [ { weight = 1, strike = 1, kind = "C", offset = 5 }, { weight = -1, strike = 2, kind = "C" } ]
note = "x"
"#);
        assert!(s.resolve("W").is_some());
        assert_eq!(diags.len(), 2, "{diags:?}");
        assert!(
            diags
                .iter()
                .all(|d| d.severity == geode_core::config::Severity::Warning)
        );
    }

    #[test]
    fn an_empty_set_resolves_nothing() {
        assert!(TemplateSet::default().resolve("CS").is_none());
    }

    #[test]
    fn a_name_interns_once_and_compares_by_name() {
        let a = Template::named("condor");
        let b = Template::named("CONDOR");
        assert_eq!(a, b);
        assert!(
            std::ptr::eq(a.token(), b.token()),
            "interned: one allocation per name"
        );
        assert_eq!(a.token(), "CONDOR");
        assert_eq!(a.storage_name(), "condor");
        assert_eq!(Template::named("cs"), Template::CS);
        assert!(Template::named("custom").is_custom());
    }

    #[test]
    fn every_template_token_parses_case_insensitively_and_round_trips() {
        let builtin = TemplateSet::builtin();
        for d in builtin.iter() {
            let t = Template::named(&d.name);
            assert_eq!(Template::named(&d.name.to_lowercase()), t, "{t:?}");
            assert_eq!(t.token(), d.name, "{t:?}");
            assert_eq!(t.storage_name(), t.token().to_lowercase());
            assert_eq!(builtin.resolve(t.token()), Some(d), "{t:?}");
            assert_eq!(builtin.resolve(&t.storage_name()), Some(d), "{t:?}");
            assert!(!t.is_custom());
        }
        assert!(
            builtin.resolve("C").is_none(),
            "a single leg is not a template"
        );
        assert!(builtin.resolve("BUTTERFLY").is_none());
        assert!(
            builtin.resolve("CUSTOM").is_none(),
            "CUSTOM is never a table"
        );
    }

    #[test]
    fn the_seven_tables_have_the_documented_legs() {
        let b = TemplateSet::builtin();
        let legs = |n: &str| b.resolve(n).unwrap().legs.clone();
        let cs = legs("CS");
        assert_eq!(cs.len(), 2);
        assert_eq!(
            (cs[0].weight, cs[0].strike, cs[0].kind),
            (1, 0, OptionKind::Call)
        );
        assert_eq!(
            (cs[1].weight, cs[1].strike, cs[1].kind),
            (-1, 1, OptionKind::Call)
        );
        let ps = legs("PS");
        assert_eq!((ps[0].weight, ps[0].kind), (1, OptionKind::Put));
        assert_eq!((ps[1].weight, ps[1].kind), (-1, OptionKind::Put));
        let strd = legs("STRD");
        assert_eq!(strd.len(), 2);
        assert!(strd.iter().all(|l| l.weight == 1 && l.strike == 0));
        assert_eq!(
            (strd[0].kind, strd[1].kind),
            (OptionKind::Call, OptionKind::Put)
        );
        let strg = legs("STRG");
        assert_eq!(
            (strg[0].weight, strg[0].strike, strg[0].kind),
            (1, 0, OptionKind::Put)
        );
        assert_eq!(
            (strg[1].weight, strg[1].strike, strg[1].kind),
            (1, 1, OptionKind::Call)
        );
        let rr = legs("RR");
        assert_eq!(
            (rr[0].weight, rr[0].strike, rr[0].kind),
            (-1, 0, OptionKind::Put)
        );
        assert_eq!(
            (rr[1].weight, rr[1].strike, rr[1].kind),
            (1, 1, OptionKind::Call)
        );
        let fly = legs("FLY");
        assert_eq!(
            fly.iter().map(|l| l.weight).collect::<Vec<_>>(),
            vec![1, -2, 1]
        );
        assert_eq!(
            fly.iter().map(|l| l.strike).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(fly.iter().all(|l| l.kind == OptionKind::Call));
        // CAL: +far −near calls on one strike; the far expiry is index 1.
        let cal = legs("CAL");
        assert_eq!(
            (cal[0].weight, cal[0].expiry, cal[0].kind),
            (1, 1, OptionKind::Call)
        );
        assert_eq!(
            (cal[1].weight, cal[1].expiry, cal[1].kind),
            (-1, 0, OptionKind::Call)
        );
        assert!(cal.iter().all(|l| l.strike == 0));
    }

    #[test]
    fn strike_and_expiry_counts_follow_the_tables() {
        let b = TemplateSet::builtin();
        let counts = |n: &str| {
            let d = b.resolve(n).unwrap();
            (d.strikes, d.expiries)
        };
        assert_eq!(counts("CS"), (2, 1));
        assert_eq!(counts("PS"), (2, 1));
        assert_eq!(counts("STRD"), (1, 1));
        assert_eq!(counts("STRG"), (2, 1));
        assert_eq!(counts("RR"), (2, 1));
        assert_eq!(counts("FLY"), (3, 1));
        assert_eq!(counts("CAL"), (1, 2));
        assert!(b.resolve(Template::CUSTOM.token()).is_none());
        // Every table's indices are in range of its own counts.
        for d in b.iter() {
            for l in &d.legs {
                assert!(l.strike < d.strikes, "{}", d.name);
                assert!(l.expiry < d.expiries, "{}", d.name);
                assert_ne!(l.weight, 0, "{}", d.name);
            }
        }
    }
}
