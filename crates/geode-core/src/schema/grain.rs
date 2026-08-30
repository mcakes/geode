//! Measure grain (spec §3.2). Ord reads coarse < fine: a coarser grain's
//! key is a prefix of every finer one's, which is what makes
//! attributability decidable from the schema alone (spec §6.3).

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

    /// The columns naming the entity a measure belongs to — a subset of
    /// the key. `book` and `lhu` are containers, and `counterparty`
    /// subdivides a position rather than naming it, so none of them
    /// identify. This is what decides `DeterminedNonAdditive` (spec §6.3).
    pub fn identity_columns(self) -> &'static [&'static str] {
        match self {
            Grain::Position => &ID_POSITION,
            Grain::Instrument => &ID_INSTRUMENT,
            Grain::Underlying => &ID_UNDERLYING,
            Grain::UnderlyingPair => &ID_PAIR,
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
    fn table_names_are_stable() {
        assert_eq!(Grain::Position.table(), "measures_position");
        assert_eq!(Grain::UnderlyingPair.table(), "measures_underlying_pair");
    }
}
