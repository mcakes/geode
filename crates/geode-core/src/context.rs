//! Typed cursor context for acting on the entity at a tile's row: every
//! dimension or key with one value there. The shell pulls it from the
//! focused tile (`TileContent::dimension_context`) and offers what accepts
//! it; the source and target modules share only these types.

/// The context at a source tile's row. A column appears only when the row
/// names a single value for it: an ambiguous (mixed), NULL, or not-yet-grouped
/// value is absent, never a guess, because a panel opened on a made-up key
/// is a plausible wrong answer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DimensionContext {
    /// `(column, value)` in the source's order: grouping path first, then
    /// shown columns, then hidden context columns. No column twice.
    pub values: Vec<(String, String)>,
    /// The clicked column, when it names a dimension or key; its menu
    /// section leads. Set by a right press (Part 2); `None` otherwise.
    pub first: Option<String>,
    /// The column whose value this row stands for: in a grouped grid the
    /// grouping column at the row's depth. `None` when the row stands for
    /// no single dimension value (a grand total). The row menu's `Color…`
    /// row is offered for it when no dimension cell was clicked.
    pub own: Option<String>,
    /// When the target row is inside a selection: each selected top-most
    /// row's values. Empty otherwise.
    pub selection: Vec<Vec<(String, String)>>,
    /// Window-space logical pixels where a key-opened menu hangs (the
    /// cursor row's lower-left), recorded at paint (Part 2). `None` → the
    /// tile's top-left.
    pub anchor: Option<(f32, f32)>,
}

impl DimensionContext {
    /// A context holding just `pairs`, in order. For tests and for sources
    /// that know a fixed set of columns (the pricer).
    pub fn of(pairs: &[(&str, &str)]) -> DimensionContext {
        DimensionContext {
            values: pairs
                .iter()
                .map(|(c, v)| (c.to_string(), v.to_string()))
                .collect(),
            ..DimensionContext::default()
        }
    }

    pub fn get(&self, column: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(c, _)| c == column)
            .map(|(_, v)| v.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Whether a kind accepting `accepts` can open on this context: it
    /// accepts at least one column present here. A row names many columns
    /// (`lhu`, `underlying_ref`, `position_ref`, …) and a kind needs only
    /// its own.
    pub fn offers(&self, accepts: &[&str]) -> bool {
        accepts.iter().any(|c| self.get(c).is_some())
    }

    /// The value a dialog is titled by for kinds accepting `accepts`: the
    /// first value, in context order, of a column one of them accepts.
    pub fn subject(&self, accepts: &[&str]) -> Option<&str> {
        self.values
            .iter()
            .find(|(c, _)| accepts.contains(&c.as_str()))
            .map(|(_, v)| v.as_str())
    }

    /// Every selected row's single value of `column`, in selection order,
    /// or `Err(n)`: the number of selected rows that name none. An empty
    /// selection is `Ok(vec![])`.
    pub fn selection_values(&self, column: &str) -> Result<Vec<String>, usize> {
        let mut values = Vec::new();
        let mut missing = 0;
        for row in &self.selection {
            match row.iter().find(|(c, _)| c == column) {
                Some((_, v)) => values.push(v.clone()),
                None => missing += 1,
            }
        }
        if missing > 0 {
            Err(missing)
        } else {
            Ok(values)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> DimensionContext {
        DimensionContext::of(&[
            ("lhu", "7"),
            ("underlying_ref", "SPX"),
            ("position_ref", "P7"),
        ])
    }

    #[test]
    fn an_empty_context_offers_nothing() {
        let c = DimensionContext::default();
        assert!(c.is_empty());
        assert!(!c.offers(&["underlying_ref"]));
        assert_eq!(c.subject(&["underlying_ref"]), None);
    }

    #[test]
    fn get_reads_a_present_column_only() {
        let c = row();
        assert_eq!(c.get("underlying_ref"), Some("SPX"));
        assert_eq!(c.get("instrument_ref"), None);
    }

    #[test]
    fn a_kind_is_offered_when_it_accepts_any_present_column() {
        let c = row();
        assert!(
            c.offers(&["underlying_ref"]),
            "lhu and position_ref do not block it"
        );
        assert!(c.offers(&["instrument_ref", "position_ref"]));
        assert!(!c.offers(&["instrument_ref"]));
        assert!(!c.offers(&[]), "a kind accepting nothing is never offered");
    }

    #[test]
    fn the_subject_is_the_first_accepted_value_in_context_order() {
        let c = row();
        assert_eq!(c.subject(&["position_ref", "underlying_ref"]), Some("SPX"));
        assert_eq!(c.subject(&["book"]), None);
    }

    #[test]
    fn selection_values_counts_rows_without_the_column() {
        let mut c = row();
        assert_eq!(c.selection_values("position_ref"), Ok(vec![]));
        c.selection = vec![
            vec![("position_ref".into(), "P7".into())],
            vec![("position_ref".into(), "P8".into())],
        ];
        assert_eq!(
            c.selection_values("position_ref"),
            Ok(vec!["P7".into(), "P8".into()])
        );
        c.selection.push(vec![("lhu".into(), "7".into())]);
        assert_eq!(c.selection_values("position_ref"), Err(1));
    }
}
