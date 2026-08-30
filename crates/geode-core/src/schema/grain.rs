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
    fn table_names_are_stable() {
        assert_eq!(Grain::Position.table(), "measures_position");
        assert_eq!(Grain::UnderlyingPair.table(), "measures_underlying_pair");
    }
}
