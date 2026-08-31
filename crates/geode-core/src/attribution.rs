//! Whether a measure can be summed at a given grouping level, and how a
//! scope predicate reached it (spec §6.3).
//!
//! Both are decidable from the schema alone — no data is consulted — which
//! is why they live in core beside the grain vocabulary rather than in the
//! compiler.

use crate::dimensions::DerivedDimensions;
use crate::schema::Grain;

/// Whether a measure's value at one grouping level can be summed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    /// Every measure row belongs to exactly one group. Children total to
    /// their parent.
    Additive,
    /// The group names one entity, so the value is real — but it repeats
    /// across sibling groups and must never be totalled.
    DeterminedNonAdditive,
    /// The value would belong to an ancestor row, not this one. NULL.
    NonAttributable,
}

/// How a scope predicate was applied to a measure's grain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeSemantics {
    /// Every predicate names a column present at this grain.
    Direct,
    /// Some predicate names a finer column, applied as a membership test:
    /// "positions that have SPX risk", not "the SPX share".
    SemiJoined { dimensions: Vec<String> },
}

impl ScopeSemantics {
    pub fn is_direct(&self) -> bool {
        matches!(self, ScopeSemantics::Direct)
    }

    pub fn dimensions(&self) -> &[String] {
        match self {
            ScopeSemantics::Direct => &[],
            ScopeSemantics::SemiJoined { dimensions } => dimensions,
        }
    }
}

/// Decide a measure's attribution at one grouping level.
///
/// `grouping` is the prefix of the view's grouping tuple for this level,
/// so a three-level tree calls this three times with growing slices.
pub fn attribution_of(grain: Grain, grouping: &[String], dims: &DerivedDimensions) -> Attribution {
    // Dimension keys, not the raw key: the pair grain's `underlying_ref`
    // is `least(u1, u2)`, so grouping by the underlying does not partition
    // its rows — an SPX-RUT pair belongs to both. That is spec §6.3's
    // "non-attributable at an underlying-level grouping" rule, and it
    // falls out of the vocabulary rather than needing a special case.
    let key = grain.dimension_key_columns();

    // Resolve derived dimensions to their source before testing: `desk`
    // counts as `book`, which is what makes a desk rollup additive.
    let base: Vec<&str> = grouping
        .iter()
        .map(|c| dims.base_column(c.as_str()))
        .collect();

    // A = determined by the measure's key, E = everything else.
    let has_extra = base.iter().any(|c| !key.contains(c));
    if !has_extra {
        return Attribution::Additive;
    }

    // Not additive. Does what *is* attributable still name the entity?
    let attributable: Vec<&&str> = base.iter().filter(|c| key.contains(c)).collect();
    let names_entity = grain
        .identity_columns()
        .iter()
        .all(|id| attributable.iter().any(|c| **c == *id));

    if names_entity {
        Attribution::DeterminedNonAdditive
    } else {
        Attribution::NonAttributable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};
    use crate::dimensions::DerivedDimensions;
    use crate::schema::Grain;

    fn dims() -> DerivedDimensions {
        let text = r#"
[desk]
from = "book"
[desk.values]
IDX_EXO_EU = ["BK000", "BK001"]
"#;
        let doc = merge_docs(
            "dimensions",
            &[LayerDoc::builtin("dimensions", text).unwrap()],
        );
        DerivedDimensions::from_doc(&doc).0
    }

    fn attribution(grain: Grain, grouping: &[&str]) -> Attribution {
        let g: Vec<String> = grouping.iter().map(|s| s.to_string()).collect();
        attribution_of(grain, &g, &dims())
    }

    #[test]
    fn the_specs_worked_example_reads_additive_blank_determined() {
        // spec §6.3: grouping lhu > underlying > position, trading PnL,
        // which is position grain. Each level is a prefix of the grouping.
        assert_eq!(
            attribution(Grain::Position, &["lhu"]),
            Attribution::Additive,
            "level 1: every position sits in exactly one LHU"
        );
        assert_eq!(
            attribution(Grain::Position, &["lhu", "underlying_ref"]),
            Attribution::NonAttributable,
            "level 2: a position has several underlyings"
        );
        assert_eq!(
            attribution(Grain::Position, &["lhu", "underlying_ref", "position_ref"]),
            Attribution::DeterminedNonAdditive,
            "level 3: the position is named, but repeats across underlyings"
        );
    }

    #[test]
    fn greeks_are_additive_at_every_level_of_that_same_grouping() {
        // Underlying-grain measures have no mismatch with this grouping.
        for level in [
            &["lhu"][..],
            &["lhu", "underlying_ref"],
            &["lhu", "underlying_ref", "position_ref"],
        ] {
            assert_eq!(
                attribution(Grain::Underlying, level),
                Attribution::Additive,
                "{level:?}"
            );
        }
    }

    #[test]
    fn a_derived_dimension_is_additive_through_its_source_column() {
        // desk is not a key column, but book determines it (spec §6.8), so
        // every position sits in exactly one desk.
        assert_eq!(
            attribution(Grain::Position, &["desk"]),
            Attribution::Additive
        );
        assert_eq!(
            attribution(Grain::Underlying, &["desk", "book"]),
            Attribution::Additive
        );
    }

    #[test]
    fn an_undeclared_outside_column_is_not_attributable() {
        // Without a declared dependency the compiler cannot know a
        // position sits in exactly one of these.
        assert_eq!(
            attribution(Grain::Position, &["region"]),
            Attribution::NonAttributable
        );
    }

    #[test]
    fn grouping_by_an_instrument_attribute_cannot_attribute_position_pnl() {
        // A position's legs may carry different model codes.
        assert_eq!(
            attribution(Grain::Position, &["book", "model_code"]),
            Attribution::NonAttributable
        );
        // Adding the position back makes it determined, not additive.
        assert_eq!(
            attribution(Grain::Position, &["book", "model_code", "position_ref"]),
            Attribution::DeterminedNonAdditive
        );
    }

    #[test]
    fn cross_gamma_is_not_attributable_at_an_underlying_level_grouping() {
        // spec §6.3: the pair is canonicalized, so an SPX-RUT pair would
        // land under whichever name sorts first — arbitrary. Additive at
        // or coarser than instrument, blank below.
        assert_eq!(
            attribution(Grain::UnderlyingPair, &["book", "instrument_ref"]),
            Attribution::Additive
        );
        assert_eq!(
            attribution(Grain::UnderlyingPair, &["lhu", "underlying_ref"]),
            Attribution::NonAttributable
        );
        assert_eq!(
            attribution(
                Grain::UnderlyingPair,
                &["lhu", "underlying_ref", "position_ref"]
            ),
            Attribution::NonAttributable,
            "naming the position does not name the pair"
        );
    }

    #[test]
    fn an_empty_grouping_is_additive_for_every_grain() {
        // The grand total. Nothing outside the key, so nothing to break.
        for g in Grain::ALL {
            assert_eq!(attribution(g, &[]), Attribution::Additive, "{g:?}");
        }
    }

    #[test]
    fn counterparty_never_needs_special_handling() {
        // It is a key column, so grouping by it is additive; omitting it
        // just sums across it, which is ordinary aggregation.
        assert_eq!(
            attribution(Grain::Position, &["book", "counterparty"]),
            Attribution::Additive
        );
        assert_eq!(
            attribution(Grain::Position, &["book", "position_ref"]),
            Attribution::Additive
        );
    }

    #[test]
    fn scope_semantics_names_the_dimensions_applied_by_membership() {
        let direct = ScopeSemantics::Direct;
        assert!(direct.is_direct());
        let semi = ScopeSemantics::SemiJoined {
            dimensions: vec!["underlying_ref".into()],
        };
        assert!(!semi.is_direct());
        assert_eq!(semi.dimensions(), &["underlying_ref".to_string()]);
    }
}
