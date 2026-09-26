//! The seven package templates (line-pricer spec §6.3): a template is a
//! TABLE — for each leg, its weight, which strike and expiry index it
//! takes and its option kind. The parser expands a template over the
//! typed strikes and expiries; the renderer recognises legs that still
//! match a table and prints the template form back.

use geode_core::config::{Diagnostic, MergedDoc, Severity};
use geode_core::pricing::OptionKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Template {
    /// A `Group` over a run of roots, or a package whose legs no longer
    /// match any table.
    Custom,
    CS,
    PS,
    STRD,
    STRG,
    RR,
    FLY,
    CAL,
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

const fn leg(weight: i64, strike: usize, expiry: usize, kind: OptionKind) -> LegSpec {
    LegSpec {
        weight,
        strike,
        expiry,
        kind,
    }
}

use OptionKind::{Call, Put};

const CS: [LegSpec; 2] = [leg(1, 0, 0, Call), leg(-1, 1, 0, Call)];
const PS: [LegSpec; 2] = [leg(1, 0, 0, Put), leg(-1, 1, 0, Put)];
const STRD: [LegSpec; 2] = [leg(1, 0, 0, Call), leg(1, 0, 0, Put)];
const STRG: [LegSpec; 2] = [leg(1, 0, 0, Put), leg(1, 1, 0, Call)];
const RR: [LegSpec; 2] = [leg(-1, 0, 0, Put), leg(1, 1, 0, Call)];
const FLY: [LegSpec; 3] = [leg(1, 0, 0, Call), leg(-2, 1, 0, Call), leg(1, 2, 0, Call)];
/// `+far −near` calls on one strike; the shorthand's `E1/E2` is
/// near/far, so the far expiry is index 1.
const CAL: [LegSpec; 2] = [leg(1, 0, 1, Call), leg(-1, 0, 0, Call)];

impl Template {
    pub const ALL: [Template; 8] = [
        Template::Custom,
        Template::CS,
        Template::PS,
        Template::STRD,
        Template::STRG,
        Template::RR,
        Template::FLY,
        Template::CAL,
    ];

    /// Case-insensitive. `None` for anything that is not a template
    /// token (`C` and `P` are single legs, not templates).
    pub fn parse(token: &str) -> Option<Template> {
        let upper = token.to_ascii_uppercase();
        Template::ALL.into_iter().find(|t| t.token() == upper)
    }

    pub fn token(self) -> &'static str {
        match self {
            Template::Custom => "CUSTOM",
            Template::CS => "CS",
            Template::PS => "PS",
            Template::STRD => "STRD",
            Template::STRG => "STRG",
            Template::RR => "RR",
            Template::FLY => "FLY",
            Template::CAL => "CAL",
        }
    }

    /// The lower-case spelling `pricer_sheets.template` stores (spec §7.2).
    pub fn storage_name(self) -> &'static str {
        match self {
            Template::Custom => "custom",
            Template::CS => "cs",
            Template::PS => "ps",
            Template::STRD => "strd",
            Template::STRG => "strg",
            Template::RR => "rr",
            Template::FLY => "fly",
            Template::CAL => "cal",
        }
    }

    pub fn legs(self) -> &'static [LegSpec] {
        match self {
            Template::Custom => &[],
            Template::CS => &CS,
            Template::PS => &PS,
            Template::STRD => &STRD,
            Template::STRG => &STRG,
            Template::RR => &RR,
            Template::FLY => &FLY,
            Template::CAL => &CAL,
        }
    }

    /// How many strikes the shorthand takes: one more than the largest
    /// strike index any leg names.
    pub fn strikes(self) -> usize {
        self.legs().iter().map(|l| l.strike + 1).max().unwrap_or(0)
    }

    pub fn expiries(self) -> usize {
        self.legs().iter().map(|l| l.expiry + 1).max().unwrap_or(0)
    }
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

/// A template name as config and storage spell it: 1 to 8 characters, a
/// letter first, then letters or digits. `C` and `P` are single legs and
/// `CUSTOM` is the grouped-package marker, so none of the three names a
/// table. Answers the upper-cased name.
pub fn check_name(name: &str) -> Result<String, String> {
    let upper = name.to_ascii_uppercase();
    let mut chars = upper.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
    if !first_ok || !chars.all(|c| c.is_ascii_alphanumeric()) || upper.len() > 8 {
        return Err(format!(
            "'{name}' is not a template name (1 to 8 letters or digits, a letter first)"
        ));
    }
    if matches!(upper.as_str(), "C" | "P" | "CUSTOM") {
        return Err(format!("'{upper}' is reserved"));
    }
    Ok(upper)
}

impl TemplateSet {
    /// Each entry is checked alone: a bad one is dropped with an error
    /// naming its path, the rest load. Unknown keys warn and are ignored,
    /// so a later key (strike arithmetic) does not make an older binary
    /// refuse the template. A name that repeats an earlier one case-
    /// insensitively (the merge matches document keys case-sensitively, so
    /// `RR` and `rr` both reach here) replaces the earlier entry in place,
    /// keeping its doc-order position, with a warning naming both spellings;
    /// this is layer-override semantics for names TOML itself cannot fold
    /// together.
    pub fn from_doc(doc: &MergedDoc) -> (TemplateSet, Vec<Diagnostic>) {
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
            let Some(table) = value.as_table() else {
                diags.push(report(Severity::Error, "", "not a table; dropped".into()));
                continue;
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
                continue;
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
                continue;
            }
            if legs.len() < 2 {
                diags.push(report(
                    Severity::Error,
                    "legs",
                    "needs at least two legs; dropped".into(),
                ));
                continue;
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
                continue;
            }
            let def = TemplateDef {
                name: upper.clone(),
                legs,
                strikes,
                expiries,
            };
            if let Some((idx, earlier)) = seen.get(&upper) {
                diags.push(Diagnostic {
                    severity: Severity::Warning,
                    layer: None,
                    file: None,
                    message: format!(
                        "'{name}' replaces '{earlier}' (template names are case-insensitive)"
                    ),
                    path: Some(path("")),
                });
                out.defs[*idx] = def;
                seen.insert(upper, (*idx, name.clone()));
            } else {
                seen.insert(upper, (out.defs.len(), name.clone()));
                out.defs.push(def);
            }
        }
        (out, diags)
    }

    /// `BUILTIN_TEMPLATES` parsed; `the_builtin_document_is_exactly_the_seven_tables`
    /// pins that it loads clean, so the `expect` cannot fire in a shipped build.
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

    fn set(toml: &str) -> (TemplateSet, Vec<geode_core::config::Diagnostic>) {
        let doc = geode_core::config::LayerDoc::builtin(PRICER_TEMPLATES_DOC, toml).unwrap();
        TemplateSet::from_doc(&geode_core::config::merge_docs(
            PRICER_TEMPLATES_DOC,
            &[doc],
        ))
    }

    #[test]
    fn the_builtin_document_is_exactly_the_seven_tables() {
        let (s, diags) = set(BUILTIN_TEMPLATES);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(s, TemplateSet::builtin());
        let names: Vec<&str> = s.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["CS", "PS", "STRD", "STRG", "RR", "FLY", "CAL"]);
        for t in [
            Template::CS,
            Template::PS,
            Template::STRD,
            Template::STRG,
            Template::RR,
            Template::FLY,
            Template::CAL,
        ] {
            let d = s.resolve(t.token()).unwrap();
            assert_eq!(d.legs, t.legs(), "{t:?}");
            assert_eq!(
                (d.strikes, d.expiries),
                (t.strikes(), t.expiries()),
                "{t:?}"
            );
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
    fn every_template_token_parses_case_insensitively_and_round_trips() {
        for t in Template::ALL {
            assert_eq!(Template::parse(t.token()), Some(t), "{t:?}");
            assert_eq!(Template::parse(&t.token().to_lowercase()), Some(t), "{t:?}");
            assert_eq!(t.storage_name(), t.token().to_lowercase());
        }
        assert_eq!(Template::parse("C"), None, "a single leg is not a template");
        assert_eq!(Template::parse("BUTTERFLY"), None);
    }

    #[test]
    fn the_seven_tables_have_the_documented_legs() {
        let cs = Template::CS.legs();
        assert_eq!(cs.len(), 2);
        assert_eq!(
            (cs[0].weight, cs[0].strike, cs[0].kind),
            (1, 0, OptionKind::Call)
        );
        assert_eq!(
            (cs[1].weight, cs[1].strike, cs[1].kind),
            (-1, 1, OptionKind::Call)
        );
        let ps = Template::PS.legs();
        assert_eq!((ps[0].weight, ps[0].kind), (1, OptionKind::Put));
        assert_eq!((ps[1].weight, ps[1].kind), (-1, OptionKind::Put));
        let strd = Template::STRD.legs();
        assert_eq!(strd.len(), 2);
        assert!(strd.iter().all(|l| l.weight == 1 && l.strike == 0));
        assert_eq!(
            (strd[0].kind, strd[1].kind),
            (OptionKind::Call, OptionKind::Put)
        );
        let strg = Template::STRG.legs();
        assert_eq!(
            (strg[0].weight, strg[0].strike, strg[0].kind),
            (1, 0, OptionKind::Put)
        );
        assert_eq!(
            (strg[1].weight, strg[1].strike, strg[1].kind),
            (1, 1, OptionKind::Call)
        );
        let rr = Template::RR.legs();
        assert_eq!(
            (rr[0].weight, rr[0].strike, rr[0].kind),
            (-1, 0, OptionKind::Put)
        );
        assert_eq!(
            (rr[1].weight, rr[1].strike, rr[1].kind),
            (1, 1, OptionKind::Call)
        );
        let fly = Template::FLY.legs();
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
        let cal = Template::CAL.legs();
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
        assert_eq!((Template::CS.strikes(), Template::CS.expiries()), (2, 1));
        assert_eq!((Template::PS.strikes(), Template::PS.expiries()), (2, 1));
        assert_eq!(
            (Template::STRD.strikes(), Template::STRD.expiries()),
            (1, 1)
        );
        assert_eq!(
            (Template::STRG.strikes(), Template::STRG.expiries()),
            (2, 1)
        );
        assert_eq!((Template::RR.strikes(), Template::RR.expiries()), (2, 1));
        assert_eq!((Template::FLY.strikes(), Template::FLY.expiries()), (3, 1));
        assert_eq!((Template::CAL.strikes(), Template::CAL.expiries()), (1, 2));
        assert_eq!(
            (Template::Custom.strikes(), Template::Custom.expiries()),
            (0, 0)
        );
        assert!(Template::Custom.legs().is_empty());
        // Every table's indices are in range of its own counts.
        for t in Template::ALL {
            for l in t.legs() {
                assert!(l.strike < t.strikes(), "{t:?}");
                assert!(l.expiry < t.expiries(), "{t:?}");
                assert_ne!(l.weight, 0, "{t:?}");
            }
        }
    }
}
