//! Text ordering for a dimension column: lexical, or by tenor length so
//! `18M` lands between `1Y` and `2Y`. Used by history datasets to sort key
//! values the way a desk reads them; measures-family axes order by
//! `axis::AxisOrder`, and both parse tenors with `axis::tenor_days`.

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

/// The one tenor parser, shared with measures-family axes: `<n>D|W|M|Y`
/// in any case -> days; anything else is `None` and sorts last.
pub use crate::schema::axis::tenor_days;

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
