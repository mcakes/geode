//! What a board cell measures, and how a measure lowers to history
//! tuples. Every derived measure is defined once here as arithmetic over
//! named inputs, so the data layer's SQL projection and the reduction
//! agree on the formula by construction.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    Surface,
    Realized,
    Varswap,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::Surface, Role::Realized, Role::Varswap];

    pub fn name(self) -> &'static str {
        match self {
            Role::Surface => "surface",
            Role::Realized => "realized",
            Role::Varswap => "varswap",
        }
    }
}

/// One history tuple a measure reads: the key columns after the member
/// (`[tenor, delta]` on the surface, `[window]` on realized, `[tenor]` on
/// varswap) and the value column. The member is prepended by the reader.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Input {
    pub role: Role,
    pub suffix: Vec<String>,
    pub column: &'static str,
}

/// The spelling `normalize_key` gives an f64 key, so a delta suffix
/// matches the stored key: `0.50` and `0.5` both spell `0.5`, `-0.0` spells
/// `0`.
pub fn delta_text(delta: f64) -> String {
    format!("{}", delta + 0.0)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Measure {
    /// Vol at a surface point.
    Point {
        tenor: String,
        delta: f64,
    },
    Forward {
        tenor: String,
    },
    /// vol(0.75) − vol(0.25): the 25Δ put (call delta 0.75) minus the 25Δ
    /// call; positive for an equity put skew. Deltas are call deltas.
    Skew {
        tenor: String,
    },
    /// vol(0.90) − vol(0.10): the 10Δ put (call delta 0.90) minus the 10Δ
    /// call; positive for an equity put skew.
    WingSkew {
        tenor: String,
    },
    /// atfm(long) − atfm(short).
    Slope {
        long: String,
        short: String,
    },
    Realized {
        window: String,
    },
    /// atfm(tenor) − realized(window).
    IvRv {
        tenor: String,
        window: String,
    },
    Varswap {
        tenor: String,
    },
    /// varswap(tenor) − atfm(tenor).
    VsSpread {
        tenor: String,
    },
}

fn surface(tenor: &str, delta: f64, column: &'static str) -> Input {
    Input {
        role: Role::Surface,
        suffix: vec![tenor.to_string(), delta_text(delta)],
        column,
    }
}

impl Measure {
    pub fn atfm(tenor: &str) -> Measure {
        Measure::Point {
            tenor: tenor.to_string(),
            delta: 0.5,
        }
    }

    /// The tuples this measure reads, in the order `combine` takes them.
    pub fn inputs(&self) -> Vec<Input> {
        match self {
            Measure::Point { tenor, delta } => vec![surface(tenor, *delta, "vol")],
            Measure::Forward { tenor } => vec![surface(tenor, 0.5, "forward")],
            Measure::Skew { tenor } => {
                vec![surface(tenor, 0.75, "vol"), surface(tenor, 0.25, "vol")]
            }
            Measure::WingSkew { tenor } => {
                vec![surface(tenor, 0.9, "vol"), surface(tenor, 0.1, "vol")]
            }
            Measure::Slope { long, short } => {
                vec![surface(long, 0.5, "vol"), surface(short, 0.5, "vol")]
            }
            Measure::Realized { window } => vec![Input {
                role: Role::Realized,
                suffix: vec![window.clone()],
                column: "vol",
            }],
            Measure::IvRv { tenor, window } => vec![
                surface(tenor, 0.5, "vol"),
                Input {
                    role: Role::Realized,
                    suffix: vec![window.clone()],
                    column: "vol",
                },
            ],
            Measure::Varswap { tenor } => vec![Input {
                role: Role::Varswap,
                suffix: vec![tenor.clone()],
                column: "strike",
            }],
            Measure::VsSpread { tenor } => vec![
                Input {
                    role: Role::Varswap,
                    suffix: vec![tenor.clone()],
                    column: "strike",
                },
                surface(tenor, 0.5, "vol"),
            ],
        }
    }

    /// The measure's value from its inputs' values, `v` parallel to
    /// `inputs()`. A derived measure is always first minus second.
    pub fn combine(&self, v: &[f64]) -> f64 {
        let n = self.inputs().len();
        assert_eq!(v.len(), n, "{} takes {n} inputs", self.name());
        match n {
            1 => v[0],
            _ => v[0] - v[1],
        }
    }

    /// The roles `inputs()` touch, distinct, in input order.
    pub fn roles(&self) -> Vec<Role> {
        let mut out = Vec::new();
        for input in self.inputs() {
            if !out.contains(&input.role) {
                out.push(input.role);
            }
        }
        out
    }

    /// For messages and notes.
    pub fn name(&self) -> String {
        match self {
            Measure::Point { tenor, delta } => format!("point {tenor} {}", delta_text(*delta)),
            Measure::Forward { tenor } => format!("forward {tenor}"),
            Measure::Skew { tenor } => format!("skew {tenor}"),
            Measure::WingSkew { tenor } => format!("wing skew {tenor}"),
            Measure::Slope { long, short } => format!("slope {long}-{short}"),
            Measure::Realized { window } => format!("realized {window}"),
            Measure::IvRv { tenor, window } => format!("iv-rv {tenor}/{window}"),
            Measure::Varswap { tenor } => format!("varswap {tenor}"),
            Measure::VsSpread { tenor } => format!("vs spread {tenor}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(tenor: &str, delta: f64) -> Input {
        Input {
            role: Role::Surface,
            suffix: vec![tenor.to_string(), delta_text(delta)],
            column: "vol",
        }
    }

    #[test]
    fn delta_text_spells_a_delta_the_way_normalize_key_does() {
        assert_eq!(delta_text(0.50), "0.5");
        assert_eq!(delta_text(0.1), "0.1");
        assert_eq!(delta_text(-0.0), "0");
        assert_eq!(
            delta_text(0.25),
            crate::query::normalize_key("0.250", crate::schema::ColumnType::F64).unwrap()
        );
    }

    #[test]
    fn every_measure_lowers_to_its_inputs_in_combine_order() {
        assert_eq!(Measure::atfm("3M").inputs(), vec![point("3M", 0.5)]);
        assert_eq!(
            Measure::Forward { tenor: "1M".into() }.inputs(),
            vec![Input {
                role: Role::Surface,
                suffix: vec!["1M".into(), "0.5".into()],
                column: "forward"
            }]
        );
        assert_eq!(
            Measure::Skew { tenor: "3M".into() }.inputs(),
            vec![point("3M", 0.75), point("3M", 0.25)]
        );
        assert_eq!(
            Measure::WingSkew { tenor: "3M".into() }.inputs(),
            vec![point("3M", 0.9), point("3M", 0.1)]
        );
        assert_eq!(
            Measure::Slope {
                long: "1Y".into(),
                short: "1M".into()
            }
            .inputs(),
            vec![point("1Y", 0.5), point("1M", 0.5)]
        );
        let realized = Input {
            role: Role::Realized,
            suffix: vec!["20D".into()],
            column: "vol",
        };
        assert_eq!(
            Measure::Realized {
                window: "20D".into()
            }
            .inputs(),
            vec![realized.clone()]
        );
        assert_eq!(
            Measure::IvRv {
                tenor: "3M".into(),
                window: "20D".into()
            }
            .inputs(),
            vec![point("3M", 0.5), realized]
        );
        let varswap = Input {
            role: Role::Varswap,
            suffix: vec!["3M".into()],
            column: "strike",
        };
        assert_eq!(
            Measure::Varswap { tenor: "3M".into() }.inputs(),
            vec![varswap.clone()]
        );
        assert_eq!(
            Measure::VsSpread { tenor: "3M".into() }.inputs(),
            vec![varswap, point("3M", 0.5)]
        );
    }

    #[test]
    fn combine_is_identity_for_a_point_and_first_minus_second_for_every_derived_measure() {
        assert_eq!(Measure::atfm("3M").combine(&[0.21]), 0.21);
        assert_eq!(
            Measure::Forward { tenor: "3M".into() }.combine(&[4000.0]),
            4000.0
        );
        assert_eq!(
            Measure::Realized {
                window: "5D".into()
            }
            .combine(&[0.12]),
            0.12
        );
        assert_eq!(
            Measure::Varswap { tenor: "3M".into() }.combine(&[0.23]),
            0.23
        );
        // 25Δ put minus 25Δ call in call-delta terms: vol(0.75) − vol(0.25) is positive for a put skew.
        assert_eq!(
            Measure::Skew { tenor: "3M".into() }.combine(&[0.24, 0.20]),
            0.24 - 0.20
        );
        assert_eq!(
            Measure::WingSkew { tenor: "3M".into() }.combine(&[0.30, 0.19]),
            0.30 - 0.19
        );
        assert_eq!(
            Measure::Slope {
                long: "1Y".into(),
                short: "1M".into()
            }
            .combine(&[0.22, 0.18]),
            0.22 - 0.18
        );
        assert_eq!(
            Measure::IvRv {
                tenor: "3M".into(),
                window: "20D".into()
            }
            .combine(&[0.21, 0.15]),
            0.21 - 0.15
        );
        assert_eq!(
            Measure::VsSpread { tenor: "3M".into() }.combine(&[0.23, 0.21]),
            0.23 - 0.21
        );
    }

    #[test]
    #[should_panic(expected = "2 inputs")]
    fn combine_refuses_the_wrong_number_of_values() {
        Measure::Skew { tenor: "3M".into() }.combine(&[0.1]);
    }

    #[test]
    fn roles_are_distinct_and_names_read_as_the_desk_says_them() {
        assert_eq!(
            Measure::IvRv {
                tenor: "3M".into(),
                window: "20D".into()
            }
            .roles(),
            vec![Role::Surface, Role::Realized]
        );
        assert_eq!(
            Measure::Skew { tenor: "3M".into() }.roles(),
            vec![Role::Surface]
        );
        assert_eq!(Measure::atfm("3M").name(), "point 3M 0.5");
        assert_eq!(
            Measure::Slope {
                long: "1Y".into(),
                short: "1M".into()
            }
            .name(),
            "slope 1Y-1M"
        );
        assert_eq!(
            Measure::IvRv {
                tenor: "3M".into(),
                window: "20D".into()
            }
            .name(),
            "iv-rv 3M/20D"
        );
        assert_eq!(
            Measure::VsSpread { tenor: "3M".into() }.name(),
            "vs spread 3M"
        );
    }
}
