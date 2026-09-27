//! The blotter's launch context: the underlying of the cursor row, when
//! the grouping carries one and the row is at or below its level.

use super::expansion::Path;

/// The grouping column naming a row's underlying. The desk's underlying
/// identifier; market-data keys use the same one.
pub const UNDERLYING_COLUMN: &str = "underlying_ref";

/// The underlying on `path` (the cursor row's ancestors' tree texts, root
/// excluded) under `grouping`, or `None`:
/// - when the grouping lacks the column;
/// - for a row above that level (a subtotal, or the grand total's empty path);
/// - for a NULL value, which is `None` in the path, never the text "NULL".
pub fn underlying_at(path: &Path, grouping: &[String]) -> Option<String> {
    let level = grouping.iter().position(|g| g == UNDERLYING_COLUMN)?;
    path.get(level)?.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }
    fn p(parts: &[Option<&str>]) -> Path {
        parts.iter().map(|s| s.map(str::to_string)).collect()
    }

    #[test]
    fn an_underlying_row_and_its_descendants_name_it() {
        let grouping = g(&["lhu", "underlying_ref", "position_ref"]);
        assert_eq!(
            underlying_at(&p(&[Some("L1"), Some("SPX")]), &grouping),
            Some("SPX".into())
        );
        assert_eq!(
            underlying_at(&p(&[Some("L1"), Some("SPX"), Some("P7")]), &grouping),
            Some("SPX".into())
        );
    }

    #[test]
    fn rows_above_the_level_and_groupings_without_it_are_empty() {
        let grouping = g(&["lhu", "underlying_ref"]);
        assert_eq!(
            underlying_at(&p(&[Some("L1")]), &grouping),
            None,
            "subtotal"
        );
        assert_eq!(underlying_at(&p(&[]), &grouping), None, "grand total");
        assert_eq!(
            underlying_at(&p(&[Some("L1"), Some("SPX")]), &g(&["lhu", "currency"])),
            None,
            "no underlying column"
        );
    }

    #[test]
    fn a_null_underlying_is_empty_not_a_made_up_key() {
        let grouping = g(&["lhu", "underlying_ref"]);
        assert_eq!(underlying_at(&p(&[Some("L1"), None]), &grouping), None);
    }
}
