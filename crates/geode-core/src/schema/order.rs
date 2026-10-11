//! Text ordering for a dimension column: lexical, or by tenor length so
//! `18M` lands between `1Y` and `2Y`. Used by history datasets (and later
//! by measure axes) to sort key values the way a desk reads them.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextOrder {
    #[default]
    Lexical,
    Tenor,
}

impl TextOrder {
    pub fn parse(s: &str) -> Option<TextOrder> {
        match s {
            "lexical" => Some(TextOrder::Lexical),
            "tenor" => Some(TextOrder::Tenor),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TextOrder::Lexical => "lexical",
            TextOrder::Tenor => "tenor",
        }
    }
}

/// `<n>D|W|M|Y` in any case -> days (1, 7, 30, 365 multiples). Anything
/// else, an empty label included, is `None` and sorts after the tenors.
pub fn tenor_days(label: &str) -> Option<u32> {
    let label = label.trim();
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

/// Sorts in place per `order`. Tenor: parsable tenors first by length,
/// ties and the rest lexically.
pub fn sort_text(values: &mut [String], order: TextOrder) {
    match order {
        TextOrder::Lexical => values.sort(),
        TextOrder::Tenor => values.sort_by(|a, b| match (tenor_days(a), tenor_days(b)) {
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
        sort_text(&mut v, TextOrder::Tenor);
        assert_eq!(v, ["1M", "3M", "6M", "1y", "18m", "2Y", "ON", "spot"]);
    }

    #[test]
    fn lexical_order_is_plain_string_order_and_parse_round_trips() {
        let mut v: Vec<String> = ["b", "a", "10", "9"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        sort_text(&mut v, TextOrder::Lexical);
        assert_eq!(v, ["10", "9", "a", "b"]);
        for o in [TextOrder::Lexical, TextOrder::Tenor] {
            assert_eq!(TextOrder::parse(o.as_str()), Some(o));
        }
        assert_eq!(TextOrder::parse("alphabetical"), None);
        assert_eq!(TextOrder::default(), TextOrder::Lexical);
    }
}
