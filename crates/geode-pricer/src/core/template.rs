//! The seven package templates (line-pricer spec §6.3): a template is a
//! TABLE — for each leg, its weight, which strike and expiry index it
//! takes and its option kind. The parser expands a template over the
//! typed strikes and expiries; the renderer recognises legs that still
//! match a table and prints the template form back.

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

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::pricing::OptionKind;

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
