//! Whether a measure can be summed at a given grouping level, and how a
//! scope predicate reached it (spec §6.3).
//!
//! Both are decidable from the schema alone — no data is consulted — which
//! is why they live in core beside the grain vocabulary rather than in the
//! compiler.

use crate::dimensions::DerivedDimensions;
use crate::schema::{DatasetSpec, Grain};

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

impl Attribution {
    /// The weaker of two claims, for a value computed from both.
    ///
    /// A derived column is only as attributable as its inputs: an
    /// expression over a `NonAttributable` measure is itself
    /// non-attributable, because the number it is built from does not
    /// belong to this row. Ordered `Additive` < `DeterminedNonAdditive` <
    /// `NonAttributable`, weakest wins.
    pub fn meet(self, other: Attribution) -> Attribution {
        use Attribution::*;
        match (self, other) {
            (NonAttributable, _) | (_, NonAttributable) => NonAttributable,
            (DeterminedNonAdditive, _) | (_, DeterminedNonAdditive) => DeterminedNonAdditive,
            (Additive, Additive) => Additive,
        }
    }
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

    /// The weaker of two, for a value computed from both: semi-joined if
    /// either input was, over the union of the dimensions responsible.
    /// "Positions that have SPX risk" does not become direct by being
    /// divided by something that is.
    pub fn meet(&self, other: &ScopeSemantics) -> ScopeSemantics {
        match (self, other) {
            (ScopeSemantics::Direct, ScopeSemantics::Direct) => ScopeSemantics::Direct,
            _ => {
                let mut dimensions: Vec<String> = [self, other]
                    .iter()
                    .filter_map(|s| match s {
                        ScopeSemantics::SemiJoined { dimensions } => Some(dimensions.clone()),
                        ScopeSemantics::Direct => None,
                    })
                    .flatten()
                    .collect();
                dimensions.sort();
                dimensions.dedup();
                ScopeSemantics::SemiJoined { dimensions }
            }
        }
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
pub fn attribution_of(
    ds: &DatasetSpec,
    grain: Grain,
    grouping: &[String],
    dims: &DerivedDimensions,
) -> Attribution {
    // Dimension keys plus carried dimensions (spec §3.3): a carried
    // dimension is functionally determined by this grain's key, so
    // grouping by it partitions the rows exactly as a key does. The pair
    // grain's canonicalised underlyings are still excluded, for the
    // reason `dimension_key_columns` gives.
    let key = ds.dimensions_at(grain);

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

    #[test]
    fn the_attribution_meet_takes_the_weaker_claim() {
        use Attribution::*;
        // A value computed from both is only as attributable as the
        // weaker input: dividing a blanked number by a good one does not
        // produce a number that belongs to this row.
        assert_eq!(Additive.meet(Additive), Additive);
        assert_eq!(Additive.meet(NonAttributable), NonAttributable);
        assert_eq!(NonAttributable.meet(Additive), NonAttributable);
        assert_eq!(Additive.meet(DeterminedNonAdditive), DeterminedNonAdditive);
        assert_eq!(
            DeterminedNonAdditive.meet(NonAttributable),
            NonAttributable,
            "non-attributable is weaker than merely non-additive"
        );
        // Commutative, or the answer would depend on column order.
        for (a, b) in [
            (Additive, NonAttributable),
            (Additive, DeterminedNonAdditive),
            (DeterminedNonAdditive, NonAttributable),
        ] {
            assert_eq!(a.meet(b), b.meet(a), "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn the_scope_semantics_meet_unions_the_dimensions_responsible() {
        let semi = |d: &[&str]| ScopeSemantics::SemiJoined {
            dimensions: d.iter().map(|s| s.to_string()).collect(),
        };
        assert_eq!(
            ScopeSemantics::Direct.meet(&ScopeSemantics::Direct),
            ScopeSemantics::Direct
        );
        // "Positions that have SPX risk" does not become direct by being
        // divided by something that is.
        assert_eq!(
            ScopeSemantics::Direct.meet(&semi(&["underlying_ref"])),
            semi(&["underlying_ref"])
        );
        assert_eq!(
            semi(&["underlying_ref"]).meet(&semi(&["lhu"])),
            semi(&["lhu", "underlying_ref"]),
            "both dimensions are responsible, named once each"
        );
        assert_eq!(
            semi(&["lhu"]).meet(&semi(&["lhu"])),
            semi(&["lhu"]),
            "the same dimension twice is still one dimension"
        );
    }

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

    /// No carried dimensions: `dimensions_at` reduces to
    /// `grain.dimension_key_columns()`, so an empty dataset is a drop-in
    /// stand-in for the tests below that only exercise the built-in
    /// vocabulary.
    fn dataset() -> DatasetSpec {
        DatasetSpec::default()
    }

    fn attribution(grain: Grain, grouping: &[&str]) -> Attribution {
        let g: Vec<String> = grouping.iter().map(|s| s.to_string()).collect();
        attribution_of(&dataset(), grain, &g, &dims())
    }

    /// The Phase 4 §3.3 carried-dimension fixture (`currency` carried by
    /// the instrument grain), duplicated from `schema::mod::tests::CARRIED`
    /// — attribution and schema parsing are tested separately even though
    /// they share a fixture shape.
    fn carried_dataset() -> DatasetSpec {
        let text = r#"
[risk.columns.book]
type = "utf8"
role = "dimension"
[risk.columns.lhu]
type = "utf8"
role = "dimension"
[risk.columns.position_ref]
type = "utf8"
role = "key"
[risk.columns.counterparty]
type = "utf8"
role = "dimension"
[risk.columns.instrument_ref]
type = "utf8"
role = "key"
[risk.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk.columns.currency]
type = "utf8"
role = "dimension"
grain = "instrument"
[risk.columns.npv]
type = "f64"
role = "measure"
grain = "position"
[risk.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        crate::schema::SchemaSpec::from_doc(&doc)
            .0
            .dataset("risk")
            .expect("dataset")
            .clone()
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

    #[test]
    fn grouping_by_a_carried_dimension_is_additive_where_carried_and_non_attributable_where_not() {
        let ds = carried_dataset(); // the CARRIED fixture from schema tests, duplicated here
        let g = |cols: &[&str]| cols.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            attribution_of(&ds, Grain::Underlying, &g(&["currency"]), &dims()),
            Attribution::Additive
        );
        assert_eq!(
            attribution_of(
                &ds,
                Grain::Instrument,
                &g(&["currency", "instrument_ref"]),
                &dims()
            ),
            Attribution::Additive
        );
        assert_eq!(
            attribution_of(&ds, Grain::Position, &g(&["currency"]), &dims()),
            Attribution::NonAttributable,
            "a position spans currencies and nothing names the position"
        );
        assert_eq!(
            attribution_of(
                &ds,
                Grain::Position,
                &g(&["position_ref", "currency"]),
                &dims()
            ),
            Attribution::DeterminedNonAdditive
        );
    }
}
