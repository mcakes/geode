//! Axis vocabulary for measures datasets. A bucket axis partitions a total
//! (tenor buckets of vega sum to total vega); a scenario axis enumerates
//! alternative states (spot shifts) and is never summed across. `order`
//! decides how a text axis sorts: lexically, or by tenor length so `18M`
//! lands between `1Y` and `2Y`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisKind {
    /// Partitions a total: sum along it is meaningful.
    Bucket,
    /// Enumerates alternative states: sum along it is refused.
    Scenario,
}

impl AxisKind {
    pub fn parse(s: &str) -> Option<AxisKind> {
        match s {
            "bucket" => Some(AxisKind::Bucket),
            "scenario" => Some(AxisKind::Scenario),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AxisKind::Bucket => "bucket",
            AxisKind::Scenario => "scenario",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AxisOrder {
    #[default]
    Lexical,
    Tenor,
}

impl AxisOrder {
    pub fn parse(s: &str) -> Option<AxisOrder> {
        match s {
            "lexical" => Some(AxisOrder::Lexical),
            "tenor" => Some(AxisOrder::Tenor),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AxisOrder::Lexical => "lexical",
            AxisOrder::Tenor => "tenor",
        }
    }
}

/// `<n>D|W|M|Y` in any case -> days (1, 7, 30, 365 multiples). Anything
/// else, an empty label included, is `None` and sorts after the tenors.
pub fn tenor_days(label: &str) -> Option<u32> {
    let label = label.trim();
    // The last char, not byte: a multi-byte suffix must not split mid-char.
    let (digits, unit) = label.split_at(label.char_indices().next_back()?.0);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u32 = digits.parse().ok()?;
    let per = match unit.to_ascii_uppercase().as_str() {
        "D" => 1,
        "W" => 7,
        "M" => 30,
        "Y" => 365,
        _ => return None,
    };
    n.checked_mul(per)
}

/// Sorts in place per `order`.
pub fn sort_axis_text(values: &mut [String], order: AxisOrder) {
    match order {
        AxisOrder::Lexical => values.sort(),
        // Parsable tenors first by length, ties and the rest lexically.
        AxisOrder::Tenor => values.sort_by(|a, b| match (tenor_days(a), tenor_days(b)) {
            (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.cmp(b)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.cmp(b),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenor_days_reads_day_week_month_and_year_labels() {
        assert_eq!(tenor_days("1D"), Some(1));
        assert_eq!(tenor_days("2W"), Some(14));
        assert_eq!(tenor_days("3M"), Some(90));
        assert_eq!(tenor_days("18M"), Some(540));
        assert_eq!(tenor_days("2Y"), Some(730));
        assert_eq!(tenor_days("ON"), None);
        assert_eq!(tenor_days(""), None);
        assert_eq!(tenor_days("M3"), None);
        assert_eq!(
            tenor_days("4294967295Y"),
            None,
            "overflow is None, not a wrap"
        );
    }

    #[test]
    fn tenor_order_ignores_case_and_puts_unparsable_values_last() {
        let mut v: Vec<String> = ["2Y", "18m", "1y", "3M", "ON", "6M", "1M", "spot"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        sort_axis_text(&mut v, AxisOrder::Tenor);
        assert_eq!(v, ["1M", "3M", "6M", "1y", "18m", "2Y", "ON", "spot"]);
    }

    #[test]
    fn lexical_order_is_plain_string_order() {
        let mut v: Vec<String> = ["RFR", "OIS", "EUR OIS"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        sort_axis_text(&mut v, AxisOrder::Lexical);
        assert_eq!(v, ["EUR OIS", "OIS", "RFR"]);
    }

    #[test]
    fn kinds_and_orders_round_trip_their_config_spelling() {
        assert_eq!(AxisKind::parse("bucket"), Some(AxisKind::Bucket));
        assert_eq!(AxisKind::parse("scenario"), Some(AxisKind::Scenario));
        assert_eq!(
            AxisKind::parse("Bucket"),
            None,
            "config spellings are exact"
        );
        assert_eq!(AxisKind::Scenario.as_str(), "scenario");
        assert_eq!(AxisOrder::parse("tenor"), Some(AxisOrder::Tenor));
        assert_eq!(AxisOrder::parse("lexical"), Some(AxisOrder::Lexical));
        assert_eq!(AxisOrder::default(), AxisOrder::Lexical);
    }
}
