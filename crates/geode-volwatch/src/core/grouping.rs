//! Grouping over the member rows: which of the frame's chain applies to a
//! list of underlyings, how the header names it, the pin a `:group` holds,
//! and the preorder tree of averaged group rows the grid shows.
//!
//! A vol row is one underlying, so the only levels that can group it are
//! `underlying_ref` itself (the leaf) and a classification over it. The
//! rest of a chain (`lhu`, `position_ref`, a classification over `book`)
//! is skipped and named, never silently dropped: a header reading
//! `region` while the frame says `region / lhu` would be a plausible wrong
//! grouping.

use geode_core::dimensions::DerivedDimensions;
use geode_core::link::UNDERLYING;

/// The group a member the classification does not map falls under: the
/// blotter's absent group, last among its siblings.
pub const UNMAPPED: &str = "\u{2014}";

/// The prefix of a group row's name (its `GridRow` identity): no
/// underlying is spelled `group:…`, so a group never collides with a
/// member in the cursor's or the collapsed set's names.
pub const GROUP_PREFIX: &str = "group:";

/// The chain after dropping what cannot group a vol row: `kept` in chain
/// order, `skipped` in chain order, for the header.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Applicable {
    pub kept: Vec<String>,
    pub skipped: Vec<String>,
}

/// Keep `underlying_ref` (the leaf, which closes the chain: nothing after
/// it applies) and every classification whose `from` is `underlying_ref`,
/// each once; skip the rest and name it. A chain of nothing applicable, or
/// of `underlying_ref` alone, is the flat list.
pub fn applicable(chain: &[String], dims: &DerivedDimensions) -> Applicable {
    let mut out = Applicable::default();
    let mut ended = false;
    for name in chain {
        if ended {
            out.skipped.push(name.clone());
            continue;
        }
        if name == UNDERLYING {
            ended = true;
            continue;
        }
        let over_underlying = dims.get(name).is_some_and(|d| d.from == UNDERLYING);
        if over_underlying && !out.kept.contains(name) {
            out.kept.push(name.clone());
        } else {
            out.skipped.push(name.clone());
        }
    }
    out
}

/// What `:group` completes: the leaf first, then every classification over
/// it, sorted.
pub fn vocabulary(dims: &DerivedDimensions) -> Vec<String> {
    let mut classes: Vec<String> = dims
        .all()
        .filter(|d| d.from == UNDERLYING)
        .map(|d| d.name.clone())
        .collect();
    classes.sort();
    classes.dedup();
    let mut out = vec![UNDERLYING.to_string()];
    out.extend(classes);
    out
}

/// The tile's grouping override. `None` follows the frame; `Grouping` and
/// `Flat` stand whatever the frame does; `Slot` re-reads its slot when the
/// slot changes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Pin {
    #[default]
    None,
    Grouping(Vec<String>),
    Slot(u8),
    Flat,
}

/// The header's grouping chip: the kept levels joined by ` · `, the
/// skipped ones named (`lhu skipped` beside a grouping, `lhu not
/// applicable` under `ungrouped`), `pinned ` ahead while a pin holds.
pub fn chip_text(pin: &Pin, a: &Applicable) -> String {
    let kept = a.kept.join(" \u{b7} ");
    let skipped = a.skipped.join(", ");
    let body = match (a.kept.is_empty(), a.skipped.is_empty()) {
        (false, true) => kept,
        (false, false) => format!("{kept} \u{b7} {skipped} skipped"),
        (true, true) => "ungrouped".to_string(),
        (true, false) => format!("ungrouped \u{b7} {skipped} not applicable"),
    };
    match pin {
        Pin::None => body,
        _ => format!("pinned {body}"),
    }
}

/// A group row's name from its path (one label per kept level, the
/// group's own last).
pub fn group_name(path: &[String]) -> String {
    format!("{GROUP_PREFIX}{}", path.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{LayerDoc, merge_docs};

    fn dims(toml: &str) -> DerivedDimensions {
        let doc = merge_docs(
            "dimensions",
            &[LayerDoc::builtin("dimensions", toml).unwrap()],
        );
        let (dims, diags) = DerivedDimensions::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        dims
    }

    /// `region` and `sector` over the underlying; `desk` over `book`.
    pub(super) fn europe() -> DerivedDimensions {
        dims(
            r#"
[region]
from = "underlying_ref"
[region.values]
Core = ["DAX", "SX5E"]
Isles = ["UKX"]

[sector]
from = "underlying_ref"
[sector.values]
Index = ["DAX", "SX5E", "UKX"]

[desk]
from = "book"
[desk.values]
Flow = ["b1"]
"#,
        )
    }

    fn chain(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn the_chain_keeps_only_underlying_ref_classifications_in_order() {
        let a = applicable(&chain(&["region", "lhu", "sector"]), &europe());
        assert_eq!(a.kept, ["region", "sector"]);
        assert_eq!(a.skipped, ["lhu"]);
        let a = applicable(&chain(&["desk", "position_ref"]), &europe());
        assert!(a.kept.is_empty());
        assert_eq!(a.skipped, ["desk", "position_ref"]);
    }

    #[test]
    fn underlying_ref_closes_the_chain_and_a_repeat_is_skipped() {
        let a = applicable(&chain(&["region", "underlying_ref", "sector"]), &europe());
        assert_eq!(a.kept, ["region"]);
        assert_eq!(a.skipped, ["sector"], "after the leaf nothing applies");
        let a = applicable(&chain(&["region", "region"]), &europe());
        assert_eq!(a.kept, ["region"]);
        assert_eq!(a.skipped, ["region"]);
        let a = applicable(&chain(&["underlying_ref"]), &europe());
        assert!(a.kept.is_empty() && a.skipped.is_empty(), "the flat list");
    }

    #[test]
    fn a_dimension_the_classifications_no_longer_hold_is_skipped() {
        let a = applicable(&chain(&["region", "country"]), &europe());
        assert_eq!(a.kept, ["region"]);
        assert_eq!(a.skipped, ["country"]);
    }

    #[test]
    fn the_vocabulary_is_underlying_ref_then_the_classifications_over_it() {
        assert_eq!(
            vocabulary(&europe()),
            ["underlying_ref", "region", "sector"]
        );
        assert_eq!(
            vocabulary(&DerivedDimensions::default()),
            ["underlying_ref"]
        );
    }

    #[test]
    fn the_chip_names_the_kept_the_skipped_and_the_pin() {
        let a = |k: &[&str], s: &[&str]| Applicable {
            kept: chain(k),
            skipped: chain(s),
        };
        assert_eq!(chip_text(&Pin::None, &a(&["region"], &[])), "region");
        assert_eq!(
            chip_text(&Pin::None, &a(&["region", "sector"], &[])),
            "region \u{b7} sector"
        );
        assert_eq!(
            chip_text(&Pin::None, &a(&["region"], &["lhu"])),
            "region \u{b7} lhu skipped"
        );
        assert_eq!(
            chip_text(&Pin::None, &a(&[], &["lhu", "position_ref"])),
            "ungrouped \u{b7} lhu, position_ref not applicable"
        );
        assert_eq!(chip_text(&Pin::None, &a(&[], &[])), "ungrouped");
        assert_eq!(
            chip_text(&Pin::Grouping(chain(&["region"])), &a(&["region"], &[])),
            "pinned region"
        );
        assert_eq!(
            chip_text(&Pin::Slot(2), &a(&["region"], &[])),
            "pinned region"
        );
        assert_eq!(chip_text(&Pin::Flat, &a(&[], &[])), "pinned ungrouped");
    }

    #[test]
    fn a_group_name_is_its_path_behind_the_prefix() {
        assert_eq!(group_name(&chain(&["Core"])), "group:Core");
        assert_eq!(group_name(&chain(&["Core", "Index"])), "group:Core/Index");
        assert_eq!(group_name(&chain(&[UNMAPPED])), "group:\u{2014}");
    }
}
