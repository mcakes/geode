//! Measure grains ordered from coarse to fine. Their storage keys form a
//! prefix chain. Attribution also uses each grain's dimension key: the pair
//! grain's ordered pair identifiers do not carry underlying-dimension meaning.

/// The identity columns a measure is keyed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Grain {
    Position,
    Instrument,
    Underlying,
    UnderlyingPair,
}

const K: [&str; 4] = ["book", "lhu", "position_ref", "counterparty"];
const K_INSTRUMENT: [&str; 5] = [
    "book",
    "lhu",
    "position_ref",
    "counterparty",
    "instrument_ref",
];
const K_UNDERLYING: [&str; 6] = [
    "book",
    "lhu",
    "position_ref",
    "counterparty",
    "instrument_ref",
    "underlying_ref",
];
const K_PAIR: [&str; 7] = [
    "book",
    "lhu",
    "position_ref",
    "counterparty",
    "instrument_ref",
    "underlying_ref",
    "underlying2_ref",
];

const ID_POSITION: [&str; 1] = ["position_ref"];
const ID_INSTRUMENT: [&str; 1] = ["instrument_ref"];
const ID_UNDERLYING: [&str; 2] = ["instrument_ref", "underlying_ref"];
const ID_PAIR: [&str; 3] = ["instrument_ref", "underlying_ref", "underlying2_ref"];

impl Grain {
    pub const ALL: [Grain; 4] = [
        Grain::Position,
        Grain::Instrument,
        Grain::Underlying,
        Grain::UnderlyingPair,
    ];

    pub fn key_columns(self) -> &'static [&'static str] {
        match self {
            Grain::Position => &K,
            Grain::Instrument => &K_INSTRUMENT,
            Grain::Underlying => &K_UNDERLYING,
            Grain::UnderlyingPair => &K_PAIR,
        }
    }

    /// Key columns that retain dimension meaning for grouping and scope.
    /// The pair grain stores canonical `(least, greatest)` underlying IDs;
    /// filtering either as an ordinary underlying would miss pairs where that
    /// underlying occupies the other position. Its dimension key therefore
    /// stops at instrument, making pair measures non-attributable to an
    /// underlying-level grouping.
    pub fn dimension_key_columns(self) -> &'static [&'static str] {
        match self {
            Grain::UnderlyingPair => &K_INSTRUMENT,
            other => other.key_columns(),
        }
    }

    /// Entity-identifying columns used to determine non-additive values.
    /// `book` and `lhu` are containers; `counterparty` subdivides a position.
    /// They remain storage keys but do not identify the measured entity.
    pub fn identity_columns(self) -> &'static [&'static str] {
        match self {
            Grain::Position => &ID_POSITION,
            Grain::Instrument => &ID_INSTRUMENT,
            Grain::Underlying => &ID_UNDERLYING,
            Grain::UnderlyingPair => &ID_PAIR,
        }
    }

    /// Short name used to build per-dataset table names. Table names must
    /// carry the dataset too: two datasets can declare columns at the same
    /// grain, and a grain-only name would silently make them share a table.
    pub fn short(self) -> &'static str {
        match self {
            Grain::Position => "position",
            Grain::Instrument => "instrument",
            Grain::Underlying => "underlying",
            Grain::UnderlyingPair => "underlying_pair",
        }
    }

    pub fn table(self) -> &'static str {
        match self {
            Grain::Position => "measures_position",
            Grain::Instrument => "measures_instrument",
            Grain::Underlying => "measures_underlying",
            Grain::UnderlyingPair => "measures_underlying_pair",
        }
    }

    pub fn parse(s: &str) -> Option<Grain> {
        match s {
            "position" => Some(Grain::Position),
            "instrument" => Some(Grain::Instrument),
            "underlying" => Some(Grain::Underlying),
            "underlying_pair" => Some(Grain::UnderlyingPair),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_columns_nest_from_coarse_to_fine() {
        assert_eq!(
            Grain::Position.key_columns(),
            &["book", "lhu", "position_ref", "counterparty"]
        );
        assert_eq!(
            Grain::UnderlyingPair.key_columns(),
            &[
                "book",
                "lhu",
                "position_ref",
                "counterparty",
                "instrument_ref",
                "underlying_ref",
                "underlying2_ref"
            ]
        );
        // Each grain's key is a prefix-extension of the coarser one.
        for (coarse, fine) in [
            (Grain::Position, Grain::Instrument),
            (Grain::Instrument, Grain::Underlying),
            (Grain::Underlying, Grain::UnderlyingPair),
        ] {
            assert!(fine.key_columns().starts_with(coarse.key_columns()));
            assert!(coarse < fine, "Ord must read coarse < fine");
        }
    }

    #[test]
    fn identity_columns_name_the_entity_not_the_whole_key() {
        // The entity a measure belongs to. `book`/`lhu` are containers and
        // `counterparty` subdivides a position, so none of them identify.
        assert_eq!(Grain::Position.identity_columns(), &["position_ref"]);
        assert_eq!(Grain::Instrument.identity_columns(), &["instrument_ref"]);
        assert_eq!(
            Grain::Underlying.identity_columns(),
            &["instrument_ref", "underlying_ref"]
        );
        assert_eq!(
            Grain::UnderlyingPair.identity_columns(),
            &["instrument_ref", "underlying_ref", "underlying2_ref"]
        );

        // Identity is always a subset of the key.
        for g in Grain::ALL {
            for c in g.identity_columns() {
                assert!(g.key_columns().contains(c), "{g:?} / {c}");
            }
        }
    }

    #[test]
    fn the_pair_grain_does_not_carry_the_underlying_dimension() {
        // `underlying_ref` on the pair table is `least(u1, u2)`, not the
        // underlying: a view must never group or scope the pair table by it.
        assert_eq!(
            Grain::UnderlyingPair.dimension_key_columns(),
            Grain::Instrument.key_columns()
        );
        for g in [Grain::Position, Grain::Instrument, Grain::Underlying] {
            assert_eq!(g.dimension_key_columns(), g.key_columns(), "{g:?}");
        }
        // Dimension keys are always a prefix of the key, so the join key
        // between any two grains is the shorter list.
        for g in Grain::ALL {
            assert!(g.key_columns().starts_with(g.dimension_key_columns()));
        }
    }

    #[test]
    fn table_names_are_stable() {
        assert_eq!(Grain::Position.table(), "measures_position");
        assert_eq!(Grain::UnderlyingPair.table(), "measures_underlying_pair");
    }
}
