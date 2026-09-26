# Pricer Templates From Config Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Package templates become data in a layered `pricer_templates`
config document. The seven built-ins ship in the builtin layer, and desk
and user layers add templates or redefine a built-in.

**Architecture:** `core::template` gains a pure `TemplateSet`, read from
the merged document with per-entry validation. `Template` changes from an
enum into a `Copy` name handle (an interned `&'static str`). The parser
resolves type tokens through a set. The `Sheet` carries an
`Arc<TemplateSet>` so `shorthand(row)` keeps its signature. The factory
holds the current set and reloads it the way it reloads `pricer_views`.

**Tech Stack:** Rust, `toml` (workspace `preserve_order`),
`geode_core::config::{MergedDoc, LayerDoc, merge_docs, Diagnostic}`, GPUI.

**Spec:** `docs/superpowers/specs/2026-09-26-pricer-templates-and-completion-design.md` §2 (branch A only; §3 is a later plan).

## Global Constraints

- Document name `pricer_templates`, merged at depth 1 (`"pricer_templates" => Some(1)` in `geode-core/src/config/merge.rs::atomic_depth`).
- Entry shape: `[NAME] legs = [ { weight = <non-zero int>, strike = <int ≥ 1>, expiry = <int ≥ 1, default 1>, kind = "C" | "P" } … ]`. Indices are 1-based in config and 0-based in `LegSpec`.
- Name: 1–8 characters, first a letter, then letters or digits; matched and stored upper-case; `C`, `P`, `CUSTOM` reserved.
- A template has at least 2 legs. The strike indices used must be exactly `1..=n`, and so must the expiry indices.
- A bad entry is dropped with an `Error` diagnostic whose path is `pricer_templates.<NAME>` (or `.legs.<i>` / `.legs.<i>.<key>`). An unknown key in an entry or a leg is a `Warning` and does not drop it.
- The `template` storage column stays lower-case (`cs`); reading is case-insensitive; an unknown name no longer fails a load.
- Built-in tables are exactly today's: `CS PS STRD STRG RR FLY CAL` (see `core/template.rs` before this branch).
- Pure core (`core::*`) does no I/O.
- Every behavior change updates `docs/current/features.md`, `docs/current/configuration.md` and `crates/geode-pricer/README.md` in the same task.
- Mutation harness: commit before running targeted entries; `zsh scripts/mutation-check.sh --anchors-only` must exit 0 at each task end.
- Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Say "color", not "colour", in any new user-facing text.

## Review Focus

1. A stored sheet saved before this branch (`template = "cs"` etc.) must load and print exactly as before.
2. A config entry whose TOML value isn't a table, or whose `legs` isn't an array of tables, is dropped with a diagnostic, never a panic.
3. A desk `RR` override changes how `RR` lines parse. A stored old-convention `RR` package keeps its prices and its `RR` tag, and prints its legs one per line.
4. `Template::named` is called on every parse, so the interner must not leak per call. Repeated calls with one name must return the same pointer.
5. A reload that yields an empty set must still leave `C` and `P` parsing. Only template lines fail, and they say "unknown type".

Tests for 1 and 3 are in Task 2 and Task 3, for 2 in Task 1, for 4 in Task 2, and for 5 in Task 1.

---

### Task 1: `TemplateSet` from a document (additive)

**Files:**
- Modify: `crates/geode-pricer/src/core/template.rs` (add below the existing enum; the enum stays in this task)
- Modify: `crates/geode-pricer/src/core/mod.rs` (re-exports)
- Modify: `crates/geode-core/src/config/merge.rs` (depth rule + test)
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces (in `crate::core::template`, re-exported from `crate::core`):
  - `pub const PRICER_TEMPLATES_DOC: &str = "pricer_templates";`
  - `pub const BUILTIN_TEMPLATES: &str` (TOML, below)
  - `pub struct TemplateDef { pub name: String, pub legs: Vec<LegSpec>, pub strikes: usize, pub expiries: usize }` (`Debug, Clone, PartialEq`)
  - `pub struct TemplateSet { defs: Vec<TemplateDef> }` (`Debug, Clone, Default, PartialEq`), with
    - `pub fn from_doc(doc: &MergedDoc) -> (TemplateSet, Vec<Diagnostic>)`
    - `pub fn builtin() -> TemplateSet`
    - `pub fn resolve(&self, token: &str) -> Option<&TemplateDef>` (case-insensitive)
    - `pub fn iter(&self) -> impl Iterator<Item = &TemplateDef>` (doc order)
  - `pub fn check_name(name: &str) -> Result<String, String>` (returns the upper-cased name)

- [ ] **Step 1: Failing tests** (in `template.rs` `mod tests`):

```rust
    fn set(toml: &str) -> (TemplateSet, Vec<geode_core::config::Diagnostic>) {
        let doc = geode_core::config::LayerDoc::builtin(PRICER_TEMPLATES_DOC, toml).unwrap();
        TemplateSet::from_doc(&geode_core::config::merge_docs(PRICER_TEMPLATES_DOC, &[doc]))
    }

    #[test]
    fn the_builtin_document_is_exactly_the_seven_tables() {
        let (s, diags) = set(BUILTIN_TEMPLATES);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(s, TemplateSet::builtin());
        let names: Vec<&str> = s.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["CS", "PS", "STRD", "STRG", "RR", "FLY", "CAL"]);
        for t in [Template::CS, Template::PS, Template::STRD, Template::STRG, Template::RR, Template::FLY, Template::CAL] {
            let d = s.resolve(t.token()).unwrap();
            assert_eq!(d.legs, t.legs(), "{t:?}");
            assert_eq!((d.strikes, d.expiries), (t.strikes(), t.expiries()), "{t:?}");
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
        assert_eq!(d.legs[1], LegSpec { weight: -1, strike: 1, expiry: 0, kind: OptionKind::Call });
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
legs = [ { weight = 1, strike = 1, kind = "C" }, { weight = -1, strike = 3, kind = "C" } ]
[EGAP]
legs = [ { weight = 1, strike = 1, expiry = 2, kind = "C" }, { weight = -1, strike = 1, expiry = 2, kind = "C" } ]
[KIND]
legs = [ { weight = 1, strike = 1, kind = "X" }, { weight = -1, strike = 2, kind = "C" } ]
[NOTTABLE]
legs = 3
[LEGNOTTABLE]
legs = [ 1, 2 ]
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
            "pricer_templates.LEGNOTTABLE.legs.0",
        ] {
            assert!(paths.iter().any(|p| p == want), "{want} in {paths:?}");
        }
        assert!(diags.iter().all(|d| d.severity == geode_core::config::Severity::Error));
    }

    #[test]
    fn an_unknown_key_warns_and_keeps_the_entry() {
        let (s, diags) = set(r#"[W]
legs = [ { weight = 1, strike = 1, kind = "C", offset = 5 }, { weight = -1, strike = 2, kind = "C" } ]
note = "x"
"#);
        assert!(s.resolve("W").is_some());
        assert_eq!(diags.len(), 2, "{diags:?}");
        assert!(diags.iter().all(|d| d.severity == geode_core::config::Severity::Warning));
    }

    #[test]
    fn an_empty_set_resolves_nothing() {
        assert!(TemplateSet::default().resolve("CS").is_none());
    }
```

In `geode-core/src/config/merge.rs` tests, add next to the `pricer_views` merge test:

```rust
    #[test]
    fn a_user_pricer_template_replaces_the_whole_builtin_entry() {
        let builtin = LayerDoc::builtin(
            "pricer_templates",
            "[RR]\nlegs = [ { weight = -1, strike = 1, kind = \"P\" }, { weight = 1, strike = 2, kind = \"C\" } ]\n",
        )
        .unwrap();
        let user = LayerDoc::builtin(
            "pricer_templates",
            "[RR]\nlegs = [ { weight = 1, strike = 1, kind = \"P\" }, { weight = -1, strike = 2, kind = \"C\" } ]\n",
        )
        .unwrap();
        let merged = merge_docs("pricer_templates", &[builtin, user]);
        let legs = merged.value["RR"]["legs"].as_array().unwrap();
        assert_eq!(legs.len(), 2);
        assert_eq!(legs[0]["weight"].as_integer(), Some(1), "the user's entry, whole");
    }
```

(Adapt `LayerDoc` construction to the neighbouring test's exact style
if `builtin` is not how that test builds a user layer.)

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-pricer core::template` and `cargo test -p geode-core a_user_pricer_template`
Expected: compile errors (`TemplateSet`, `BUILTIN_TEMPLATES` missing) and, for the merge test, a failure showing a leg-wise merge.

- [ ] **Step 3: Implement**

Merge depth: add `"pricer_templates" => Some(1),` with the comment `// One complete definition per pricer template name.` after the `pricer_views` line.

In `template.rs`, below the existing `impl Template`:

```rust
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
    /// refuse the template.
    pub fn from_doc(doc: &MergedDoc) -> (TemplateSet, Vec<Diagnostic>) {
        let mut out = TemplateSet::default();
        let mut diags = Vec::new();
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
                diags.push(report(Severity::Warning, key, format!("unknown key '{key}' ignored")));
            }
            let Some(raw) = table.get("legs").and_then(|v| v.as_array()) else {
                diags.push(report(Severity::Error, "legs", "missing 'legs' array; dropped".into()));
                continue;
            };
            let mut legs = Vec::with_capacity(raw.len());
            let mut bad = false;
            for (i, leg) in raw.iter().enumerate() {
                match read_leg(leg) {
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
                diags.push(report(Severity::Error, "legs", "needs at least two legs; dropped".into()));
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
            out.defs.push(TemplateDef { name: upper, legs, strikes, expiries });
        }
        (out, diags)
    }

    /// `BUILTIN_TEMPLATES` parsed; `the_builtin_document_is_exactly_the_seven_tables`
    /// pins that it loads clean, so the `expect` cannot fire in a shipped build.
    pub fn builtin() -> TemplateSet {
        let doc = geode_core::config::LayerDoc::builtin(PRICER_TEMPLATES_DOC, BUILTIN_TEMPLATES)
            .expect("BUILTIN_TEMPLATES is well-formed TOML");
        TemplateSet::from_doc(&geode_core::config::merge_docs(PRICER_TEMPLATES_DOC, &[doc])).0
    }

    pub fn resolve(&self, token: &str) -> Option<&TemplateDef> {
        self.defs.iter().find(|d| d.name.eq_ignore_ascii_case(token))
    }

    pub fn iter(&self) -> impl Iterator<Item = &TemplateDef> {
        self.defs.iter()
    }
}

/// One leg: the spec with 0-based indices and the unknown keys, or the
/// offending key (`None`: the leg itself) and why.
fn read_leg(v: &toml::Value) -> Result<(LegSpec, Vec<String>), (Option<&'static str>, String)> {
    let t = v.as_table().ok_or((None, "a leg must be a table".to_string()))?;
    let weight = t
        .get("weight")
        .and_then(|w| w.as_integer())
        .ok_or((Some("weight"), "missing or not an integer".to_string()))?;
    if weight == 0 {
        return Err((Some("weight"), "must not be zero".into()));
    }
    let index = |key: &'static str, default: Option<i64>| -> Result<usize, (Option<&'static str>, String)> {
        let n = match t.get(key) {
            Some(v) => v.as_integer().ok_or((Some(key), "not an integer".to_string()))?,
            None => default.ok_or((Some(key), "missing".to_string()))?,
        };
        if n < 1 {
            return Err((Some(key), "must be 1 or more".into()));
        }
        Ok((n - 1) as usize)
    };
    let strike = index("strike", None)?;
    let expiry = index("expiry", Some(1))?;
    let kind = match t.get("kind").and_then(|k| k.as_str()).map(str::to_ascii_uppercase).as_deref() {
        Some("C") => OptionKind::Call,
        Some("P") => OptionKind::Put,
        _ => return Err((Some("kind"), "must be \"C\" or \"P\"".into())),
    };
    let unknown = t
        .keys()
        .filter(|k| !matches!(k.as_str(), "weight" | "strike" | "expiry" | "kind"))
        .cloned()
        .collect();
    Ok((LegSpec { weight, strike, expiry, kind }, unknown))
}
```

Imports: `use geode_core::config::{Diagnostic, MergedDoc, Severity};`.
The leg test for `NOTTABLE` expects path `…NOTTABLE.legs`, which the
"missing 'legs' array" arm gives. Adjust the arm order only if a test
path disagrees, and keep the test's expected paths.

`core/mod.rs`: `pub use template::{BUILTIN_TEMPLATES, LegSpec, PRICER_TEMPLATES_DOC, Template, TemplateDef, TemplateSet, check_name};`.

- [ ] **Step 4: Run to see them pass**

Run: `cargo test -p geode-pricer core::template` and `cargo test -p geode-core merge`. All pass.

- [ ] **Step 5: Mutation entries** (append near the other pricer core entries):

```zsh
# Strike numbers with a gap make `K1/K2/K3` ambiguous; such a template
# must be dropped.
run_mutation "pricer templates: a strike-number gap loads" \
  crates/geode-pricer/src/core/template.rs \
  '            if !covers(strikes, &|l| l.strike) || !covers(expiries, &|l| l.expiry) {' \
  '            if !covers(expiries, &|l| l.expiry) {' \
  geode-pricer each_rule_drops_only_its_own_entry_with_a_path

# `C`, `P` and `CUSTOM` can never name a table.
run_mutation "pricer templates: a reserved name loads" \
  crates/geode-pricer/src/core/template.rs \
  '    if matches!(upper.as_str(), "C" | "P" | "CUSTOM") {' \
  '    if false {' \
  geode-pricer each_rule_drops_only_its_own_entry_with_a_path
```

Commit first, then run `zsh scripts/mutation-check.sh "pricer templates"`; both must be caught. Run `--anchors-only` and check it exits 0.

- [ ] **Step 6: Commit**

```bash
cargo fmt && cargo clippy -p geode-pricer -p geode-core --all-targets -- -D warnings
git add -A crates/geode-pricer/src/core crates/geode-core/src/config/merge.rs scripts/mutation-check.sh
git commit -m "feat(pricer): TemplateSet read from a pricer_templates document

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: `Template` is a name; parse, print and store through a set

**Files:**
- Modify: `crates/geode-pricer/src/core/template.rs` (enum → handle; delete the static tables)
- Modify: `crates/geode-pricer/src/core/shorthand.rs` (`parse`, `render_package`)
- Modify: `crates/geode-pricer/src/core/sheet.rs` (`templates` field, `shorthand`)
- Modify: `crates/geode-pricer/src/core/storage.rs` (lenient template read)
- Modify: every other `Template::…`/`parse(` caller: `core/edit.rs`, `core/entry.rs`, `core/clip.rs`, `core/mod.rs`, `grid.rs`, `delegate.rs`, `tile.rs`, `benches/core.rs`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `TemplateSet`, `TemplateDef`, `check_name` (Task 1).
- Produces:
  - `#[derive(Clone, Copy, PartialEq, Eq, Hash)] pub struct Template(&'static str)` with
    `pub const CUSTOM`, `pub const CS`, `PS`, `STRD`, `STRG`, `RR`, `FLY`, `CAL`;
    `pub fn named(name: &str) -> Template` (upper-cases, interns);
    `pub fn token(self) -> &'static str`; `pub fn storage_name(self) -> String` (lower-case);
    `pub fn is_custom(self) -> bool`. A `Debug` impl printing the name.
  - `pub fn parse(text: &str, templates: &TemplateSet) -> Result<RowSpec, ParseError>`
  - `pub fn render_package(def: &TemplateDef, legs: &[(i64, &Instrument)]) -> Option<String>`
  - `Sheet::set_templates(&mut self, templates: Arc<TemplateSet>)`, `Sheet::templates(&self) -> &Arc<TemplateSet>`
  - `#[cfg(test)] pub(crate) fn parse_builtin(text: &str) -> Result<RowSpec, ParseError>` in `shorthand.rs` for tests (parses against `TemplateSet::builtin()`)

- [ ] **Step 1: Failing tests** (add to `template.rs` tests):

```rust
    #[test]
    fn a_name_interns_once_and_compares_by_name() {
        let a = Template::named("condor");
        let b = Template::named("CONDOR");
        assert_eq!(a, b);
        assert!(std::ptr::eq(a.token(), b.token()), "interned: one allocation per name");
        assert_eq!(a.token(), "CONDOR");
        assert_eq!(a.storage_name(), "condor");
        assert_eq!(Template::named("cs"), Template::CS);
        assert!(Template::named("custom").is_custom());
    }
```

Add to `shorthand.rs` tests:

```rust
    #[test]
    fn a_config_template_parses_and_prints_back() {
        let doc = geode_core::config::LayerDoc::builtin(
            crate::core::PRICER_TEMPLATES_DOC,
            "[CONDOR]\nlegs = [ { weight = 1, strike = 1, kind = \"C\" }, { weight = -1, strike = 2, kind = \"C\" }, { weight = -1, strike = 3, kind = \"C\" }, { weight = 1, strike = 4, kind = \"C\" } ]\n",
        )
        .unwrap();
        let set = TemplateSet::from_doc(&geode_core::config::merge_docs(crate::core::PRICER_TEMPLATES_DOC, &[doc])).0;
        let spec = parse("-2 SPX Z26 4800/4900/5100/5200 condor", &set).unwrap();
        let RowSpec::Package { template, legs } = spec else { panic!("a package") };
        assert_eq!(template, Template::named("CONDOR"));
        assert_eq!(legs.iter().map(|l| l.qty).collect::<Vec<_>>(), [-2, 2, 2, -2]);
        let pairs: Vec<(i64, &Instrument)> = legs.iter().map(|l| (l.qty, &l.instrument)).collect();
        assert_eq!(
            render_package(set.resolve("CONDOR").unwrap(), &pairs).as_deref(),
            Some("-2 SPX Z26 4800/4900/5100/5200 CONDOR")
        );
        assert!(parse("SPX Z26 5000 CS", &set).is_err(), "only the set's names parse");
        assert!(parse("SPX Z26 5000 C", &TemplateSet::default()).is_ok(), "C and P need no set");
    }
```

(`render_expiry` prints a third-Friday month back as `Z26`; the grid tests pin `SPX Z26 4800/5200 CS`.)

Add to `storage.rs` tests:

```rust
    #[test]
    fn a_package_whose_template_is_unknown_loads_and_prints_its_legs() {
        let mut s = Sheet::new("t");
        s.apply(Edit::Insert {
            place: Place::Root { at: 0 },
            rows: vec![crate::core::shorthand::parse_builtin("SPX Z26 4800/5200 CS").unwrap()],
        })
        .unwrap();
        let mut rows = to_rows(&s).unwrap();
        // Rename the stored template to one no set defines.
        if let Some((_, Column::Utf8(t))) = rows.columns.iter_mut().find(|(n, _)| n == "template") {
            for v in t.iter_mut().filter(|v| v.as_str() == "cs") {
                *v = "gone".into();
            }
        }
        let back = from_rows("t", &rows).expect("an unknown template no longer refuses the load");
        assert_eq!(back.kind(0), RowKind::Package { template: Template::named("GONE") });
        assert!(back.shorthand(0).contains('\n'), "legs one per line");
    }
```

Adapt the column access to `DocumentRows`' real field names (read `storage.rs` `to_rows`). The assertions stay.

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-pricer`. Expected: compile errors.

- [ ] **Step 3: Implement `Template` as a handle**

Replace the `enum Template`, the static tables (`CS`, `PS`… consts of `LegSpec` arrays), `Template::ALL`, `parse`, `legs`, `strikes`, `expiries` with:

```rust
/// A package's template, by name. `Copy` so `RowKind` stays `Copy`; the
/// name is interned, so each distinct name is allocated once for the
/// process (names come from config and stored sheets and are few).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Template(&'static str);

impl std::fmt::Debug for Template {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl Template {
    /// A `Group` over a run of roots; never a table.
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
            Template::CUSTOM, Template::CS, Template::PS, Template::STRD,
            Template::STRG, Template::RR, Template::FLY, Template::CAL,
        ];
        if let Some(t) = KNOWN.iter().find(|t| t.0.eq_ignore_ascii_case(name)) {
            return *t;
        }
        static NAMES: OnceLock<Mutex<Vec<&'static str>>> = OnceLock::new();
        let upper = name.to_ascii_uppercase();
        let mut names = NAMES.get_or_init(Default::default).lock().expect("the interner never panics while held");
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
```

Update the module doc: templates are tables loaded from `pricer_templates`, a package names its template, and `TemplateSet` resolves the name.

Rewrite the old enum-based tests:
- `the_seven_tables_have_the_documented_legs` and `strike_and_expiry_counts_follow_the_tables` read `TemplateSet::builtin().resolve(…)` and keep their assertions.
- `every_template_token_parses_case_insensitively_and_round_trips` goes through `Template::named` and `TemplateSet::resolve`.
- Task 1's `the_builtin_document_is_exactly_the_seven_tables` compared against `t.legs()`. Replace that loop with literal expectations, copied from the old tables before you delete them.

Replace `Template::Custom` with `Template::CUSTOM` everywhere (`edit.rs`, `tile.rs`, `grid.rs` tests, …).

- [ ] **Step 4: Parser and printer**

In `shorthand.rs`:
- `pub fn parse(text: &str, templates: &TemplateSet) -> Result<RowSpec, ParseError>`.
- Replace the `Template::parse(&type_upper).filter(...)` block with:

```rust
    let def = templates.resolve(&type_upper).ok_or_else(|| {
        let names: Vec<&str> = templates.iter().map(|d| d.name.as_str()).collect();
        err(
            type_tok.offset,
            format!("unknown type '{}': C P {}", type_tok.text, names.join(" ")),
        )
    })?;
    let template = Template::named(&def.name);
```

  Then use `def.expiries`, `def.strikes`, `def.legs.iter()` and `def.name` where the code used `template.expiries()`, `template.strikes()`, `template.legs()` and `template.token()`.
- `render_package(def: &TemplateDef, legs)`: `def.legs`, `def.strikes`, `def.expiries`, `def.name`. Keep the logic as it is.
- Add `#[cfg(test)] pub(crate) fn parse_builtin(text: &str) -> Result<RowSpec, ParseError> { parse(text, &TemplateSet::builtin()) }`. Replace test-only `parse(x)` calls across the crate with `parse_builtin(x)`. `TemplateSet::builtin()` parses TOML, so cache it in a `thread_local!` inside `parse_builtin` if the suite slows.
- `core/mod.rs` keeps `pub use shorthand::parse` (the two-argument form).

- [ ] **Step 5: The sheet carries its set**

In `sheet.rs`:
- Add the field `templates: Arc<TemplateSet>` with the doc "The template tables `shorthand` prints against; not persisted. The builtin set until the tile sets the configured one."
- `Sheet::new` initialises it from a thread-local cached `Arc<TemplateSet>` of `TemplateSet::builtin()`.
- Add:

```rust
    pub fn set_templates(&mut self, templates: Arc<TemplateSet>) {
        self.templates = templates;
    }

    pub fn templates(&self) -> &Arc<TemplateSet> {
        &self.templates
    }
```

- In `shorthand`, the `Package` arm becomes:

```rust
            RowKind::Package { template } => {
                let legs: Vec<(i64, &Instrument)> = …; // unchanged
                self.templates
                    .resolve(template.token())
                    .and_then(|def| render_package(def, &legs))
                    .unwrap_or_else(|| /* legs one per line, unchanged */)
            }
```

Anywhere a new `Sheet` replaces the tile's sheet in production (`from_rows` in `storage.rs`, and the tile's load and switch paths), the sheet keeps the builtin set until Task 3 wires the factory's set. That is correct for this task.

- [ ] **Step 6: Storage reads leniently**

In `from_rows`, the package arm becomes:

```rust
            "package" => {
                let name = template[i].trim();
                if name.is_empty() {
                    return Err(format!("line {}: a package has no template", id.0));
                }
                // An unknown or since-removed name still loads: the legs
                // are stored, so the package keeps its prices, and it
                // prints its legs until a table of that name fits them.
                RowKind::Package { template: Template::named(name) }
            }
```

`to_rows` writes `template.storage_name()`. The `(k, t)` tuple becomes `(&str, String)`; adjust the push.

- [ ] **Step 7: Remaining callers**

- **Production `parse` callers:** `tile.rs` in `commit_entry`, and any others the compiler lists. Pass `self.sheet.templates()`.
- **`grid.rs`:** the tag stays `SharedString::new_static(template.token())`. `package_search` uses `template.token()`.
- **`delegate.rs` width test:** replace the `Template::ALL` widest-token computation with the constant `8` (the longest allowed name). Keep `TREE_WIDTH` unchanged in this task; Task 3 re-derives it.
- **`core/entry.rs` `describe`/`target_label`:** `template.token()`, unchanged.
- **`benches/core.rs`:** parse against `TemplateSet::builtin()` built once outside the timed loop.

- [ ] **Step 8: Run**

Run: `cargo test -p geode-pricer` and `cargo test -p geode-app pricer`, then `cargo bench -p geode-pricer --no-run`. All pass.

- [ ] **Step 9: Mutation entries**

- Re-anchor any existing `crates/geode-pricer/src/core/template.rs` or `shorthand.rs` entry whose anchor text changed: run `--anchors-only`, and fix each stale anchor to the new code, keeping the behavior it guards.
- Add:

```zsh
# A stored package whose template name is gone must still load; the
# old code refused the whole sheet.
run_mutation "pricer storage: an unknown template refuses the load" \
  crates/geode-pricer/src/core/storage.rs \
  '                RowKind::Package { template: Template::named(name) }' \
  '                return Err(format!("unknown template {name}"));' \
  geode-pricer a_package_whose_template_is_unknown_loads_and_prints_its_legs

# The interner must hand back the same name, or every parse leaks.
run_mutation "pricer templates: the interner leaks a copy per call" \
  crates/geode-pricer/src/core/template.rs \
  '        if let Some(n) = names.iter().find(|n| **n == upper) {' \
  '        if let Some(n) = names.iter().find(|_| false) {' \
  geode-pricer a_name_interns_once_and_compares_by_name
```

Commit first, then run each by name; all must be caught. `--anchors-only` must exit 0.

- [ ] **Step 10: Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add -A crates/geode-pricer scripts/mutation-check.sh
git commit -m "refactor(pricer): a package names its template; parse and print through a TemplateSet

An unknown stored template no longer refuses the sheet: it loads and
prints its legs.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: The app loads `pricer_templates` and reloads it

**Files:**
- Modify: `crates/geode-pricer/src/content.rs` (`Shared.templates`, `PricerFactory::new`, `reload`)
- Modify: `crates/geode-pricer/src/tile.rs` (set the sheet's templates on load, switch, new and reload; tests)
- Modify: `crates/geode-pricer/src/delegate.rs` (`TREE_WIDTH` for 8-character names)
- Modify: `crates/geode-app/src/main.rs` (builtin layer), `crates/geode-app/src/bridge.rs` (setup, key, reload)
- Modify: `docs/current/features.md`, `docs/current/configuration.md`, `crates/geode-pricer/README.md`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `TemplateSet::from_doc`, `BUILTIN_TEMPLATES`, `PRICER_TEMPLATES_DOC`, `Sheet::set_templates` (Tasks 1–2).
- Produces:
  - `PricerFactory::new(data, store, views: Views, templates: TemplateSet, settings)`
  - `PricerFactory::reload(&self, views, templates: TemplateSet, refresh, stale_after, cx)`
  - `pub fn pricer_templates_from_config(config: &Config) -> (TemplateSet, Vec<Diagnostic>)` in `bridge.rs`
  - `PricerConfigKey` gains `templates: Option<toml::Table>`

- [ ] **Step 1: Failing tests**

`tile.rs` tests. Use the existing `open_full` harness. Build a factory with a custom set by adding an optional `templates: TemplateSet` parameter to a new `open_with_templates` helper beside `open_full`; `open_full` passes `TemplateSet::builtin()`.

```rust
    fn condor_set() -> TemplateSet {
        let doc = geode_core::config::LayerDoc::builtin(
            crate::core::PRICER_TEMPLATES_DOC,
            &format!(
                "{}\n[CONDOR]\nlegs = [ {{ weight = 1, strike = 1, kind = \"C\" }}, {{ weight = -1, strike = 2, kind = \"C\" }}, {{ weight = -1, strike = 3, kind = \"C\" }}, {{ weight = 1, strike = 4, kind = \"C\" }} ]\n",
                crate::core::BUILTIN_TEMPLATES
            ),
        )
        .unwrap();
        TemplateSet::from_doc(&geode_core::config::merge_docs(crate::core::PRICER_TEMPLATES_DOC, &[doc])).0
    }

    #[gpui::test]
    fn a_config_template_is_typed_in_the_bar_tagged_and_found(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_with_templates(cx, condor_set());
        h.visible(&mut vcx, true);
        h.dispatch(&mut vcx, "add_below", None);
        typed(&h, &mut vcx, "SPX Z26 4800/4900/5100/5200 CONDOR");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.entry_error(&vcx), None);
        assert_eq!(h.sheet_len(&vcx), 5);
        assert_eq!(h.tags(&vcx)[0], "CONDOR");
        assert_eq!(h.tree(&vcx)[0], "SPX Z26 4800/4900/5100/5200 CONDOR");
    }

    #[gpui::test]
    fn a_reload_that_redefines_rr_changes_parsing_and_keeps_stored_rr_legs(
        cx: &mut gpui::TestAppContext,
    ) {
        // A package, then a line: `o` from the line lands a root.
        let (h, mut vcx) = open_seeded(cx, &["SPX Z26 4800/5200 RR", "SPX Z26 5000 C"]);
        let flipped = {
            let builtin = geode_core::config::LayerDoc::builtin(crate::core::PRICER_TEMPLATES_DOC, crate::core::BUILTIN_TEMPLATES).unwrap();
            let user = geode_core::config::LayerDoc::builtin(
                crate::core::PRICER_TEMPLATES_DOC,
                "[RR]\nlegs = [ { weight = 1, strike = 1, kind = \"P\" }, { weight = -1, strike = 2, kind = \"C\" } ]\n",
            )
            .unwrap();
            TemplateSet::from_doc(&geode_core::config::merge_docs(crate::core::PRICER_TEMPLATES_DOC, &[builtin, user])).0
        };
        let (views, settings) = (h.factory.views_for_tests(), h.factory.settings());
        vcx.update(|_, cx| h.factory.reload(views, flipped, settings.refresh, settings.stale_after, cx));
        h.draw(&mut vcx);
        assert_eq!(h.tags(&vcx)[0], "RR", "the stored package keeps its name");
        let stored = h.tile.read_with(&vcx, |t, _| t.sheet.shorthand(0));
        assert!(stored.contains('\n'), "its legs no longer fit the new RR: one per line, got {stored}");
        h.dispatch(&mut vcx, "bottom", None); // the line
        h.dispatch(&mut vcx, "add_below", None);
        typed(&h, &mut vcx, "SPX Z26 4800/5200 RR");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.entry_error(&vcx), None);
        let first_leg_qty = h.tile.read_with(&vcx, |t, _| {
            let p = t.sheet.roots().nth(2).expect("the typed package is the third root");
            t.sheet.qty(p + 1)
        });
        assert_eq!(first_leg_qty, 1, "the new RR is long the put");
    }
```

If the factory has no `views_for_tests`, add a `#[cfg(test)]` accessor that clones `shared.views`. A package's first leg is the flat row after it (`p + 1`).

`bridge.rs` tests: extend `the_pricer_config_key_changes_only_with_what_the_pricer_reads` so that a `pricer_templates` document change changes the key. Add
`fn pricer_templates_from_config_falls_back_to_the_builtin_set_without_a_doc()`.

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-pricer a_config_template a_reload_that_redefines` and `cargo test -p geode-app pricer_templates pricer_config_key`. They fail to compile.

- [ ] **Step 3: Implement**

`content.rs`:
- Add `pub(crate) templates: RefCell<Arc<TemplateSet>>` to `Shared`.
- `new` takes `templates: TemplateSet` and wraps it in an `Arc`.
- `reload` takes `templates: TemplateSet` after `views` and stores `Arc::new(templates)` before notifying tiles.
- Update the doc comments.

`tile.rs`:
- Add a helper that is the only place a sheet gets its set:

```rust
    /// The factory's current template tables onto the sheet. Called
    /// wherever a sheet is installed (load, switch, new) and on reload,
    /// so parsing and printing always use the configured set.
    fn adopt_templates(&mut self) {
        let set = self.shared.templates.borrow().clone();
        self.sheet.set_templates(set);
    }
```

- Call it after every assignment to `self.sheet` in production code (grep `self.sheet =`), and at the start of `config_changed`, before `resolve_plan`.
- `commit_entry` already parses with `self.sheet.templates()` (Task 2), so the bar uses the configured set.

`delegate.rs`: re-derive `TREE_WIDTH` exactly as the previous branch did. The width test now uses 8 characters; set `TREE_WIDTH` to the smallest multiple of 4 that passes both of its bounds, and update its doc to say "the widest allowed template name (8 characters)".

`geode-app/src/main.rs` `builtin_layer`: add the document after the views one:

```rust
        LayerDoc::builtin(
            geode_pricer::core::PRICER_TEMPLATES_DOC,
            geode_pricer::core::BUILTIN_TEMPLATES,
        )
        .expect("BUILTIN_TEMPLATES is well-formed TOML"),
```

and update that function's doc line to mention the templates.

`bridge.rs`:

```rust
/// The `pricer_templates` doc, or the built-in set when no layer has one
/// (the builtin layer always does in the app; a test config may not).
pub fn pricer_templates_from_config(config: &Config) -> (TemplateSet, Vec<Diagnostic>) {
    match config.doc(PRICER_TEMPLATES_DOC) {
        Some(doc) => TemplateSet::from_doc(doc),
        None => (TemplateSet::builtin(), Vec::new()),
    }
}
```

- Setup reads it beside `pricer_views`, extends `diagnostics`, and stores `pricer_templates` on `DataSetup`, which is passed to `PricerFactory::new`.
- `PricerConfigKey` gains `templates: config.doc(PRICER_TEMPLATES_DOC).map(|d| d.value.clone())`.
- The reload observer reads `pricer_templates_from_config` beside the views, merges its diagnostics, and calls `pricer.reload(views, templates, refresh, stale_after, cx)`.
- Update every other `PricerFactory::new` and `reload` call (tests in `geode-app` and `geode-pricer`).

- [ ] **Step 4: Run**

Run: `cargo test -p geode-pricer`, then `cargo test -p geode-app`. All pass.

- [ ] **Step 5: Mutation entries**

```zsh
# A reload must reach the open sheet's tables, or the bar keeps parsing
# with the templates the tile opened with.
run_mutation "pricer templates: a reload leaves the sheet's tables stale" \
  crates/geode-pricer/src/tile.rs \
  '    pub(crate) fn config_changed(&mut self, cx: &mut Context<Self>) {
        self.adopt_templates();' \
  '    pub(crate) fn config_changed(&mut self, cx: &mut Context<Self>) {' \
  geode-pricer a_reload_that_redefines_rr_changes_parsing_and_keeps_stored_rr_legs

# The reload key must see the templates doc, or an edit to it never
# reaches the pricer.
run_mutation "pricer config key: templates are not part of the key" \
  crates/geode-app/src/bridge.rs \
  '        templates: config.doc(PRICER_TEMPLATES_DOC).map(|d| d.value.clone()),' \
  '        templates: None,' \
  geode-app the_pricer_config_key_changes_only_with_what_the_pricer_reads
```

Commit first, then run each by name; all must be caught. `--anchors-only` must exit 0.

- [ ] **Step 6: Docs**

- `docs/current/configuration.md`:
  - Line 25's list of whole-object documents gains `pricer_templates`.
  - After the `pricer_views` section (around line 388), add a `pricer_templates` section:
    - the entry shape with the CONDOR example;
    - 1-based `strike`/`expiry` with `expiry` defaulting to 1;
    - the name rule and reserved names;
    - the validation rules, and that a bad entry is dropped with a diagnostic while unknown keys warn;
    - that a same-named entry in a higher layer replaces a built-in, e.g. a desk `RR`.
- `docs/current/features.md` pricer section, where the shorthand grammar and templates are described:
  - templates come from `pricer_templates`, the seven built-ins plus the desk's own;
  - an unknown type names the available templates;
  - a stored package whose template was removed, or whose legs no longer fit a redefined table, loads with its name as its tag and prints its legs one per line; its prices are unchanged.
- `crates/geode-pricer/README.md`:
  - the `template` row: "Template names, the `pricer_templates` reader and `TemplateSet`";
  - the shorthand invariant line: "…uses a template only while the legs still match its current table".

- [ ] **Step 7: Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add -A crates/geode-pricer crates/geode-app scripts/mutation-check.sh docs/current
git commit -m "feat(pricer): templates load from pricer_templates and reload live

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Branch verification (controller)

- [ ] Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo check -p geode-shell --features test-support --all-targets`, `zsh scripts/mutation-check.sh --anchors-only`, and `zsh scripts/mutation-check.sh --changed` (run detached).
- [ ] Display check list for the user: a user template typed in the bar, its tag in column 0 at the new `TREE_WIDTH` with line numbers on and off, a template edit in the desk file reaching an open tile, and an invalid entry's diagnostic in the diagnostics tile.
