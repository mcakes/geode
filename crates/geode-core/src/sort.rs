//! Sibling sort vocabulary shared by the grid tiles: the four orders, the
//! `s`/`shift+s` key cycles and the header click cycle, and the `:sort`
//! argument grammar with its completions. Pure: each tile holds its own
//! sort state and ranks its own rows.

/// Sibling ordering on a named column. Absolute orders compare measure
/// magnitudes so large exposures rank independently of sign; text columns
/// use the corresponding ascending or descending order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortOrder {
    #[default]
    Asc,
    Desc,
    AbsDesc,
    AbsAsc,
}

impl SortOrder {
    pub fn descending(self) -> bool {
        matches!(self, SortOrder::Desc | SortOrder::AbsDesc)
    }

    pub fn absolute(self) -> bool {
        matches!(self, SortOrder::AbsDesc | SortOrder::AbsAsc)
    }

    /// The order a column of the given kind can actually show: a text
    /// column has no magnitude, so an absolute order asked of it is its
    /// signed direction. `:sort <textcol> abs` lands here so the tile's
    /// state, the header and the rows all say the same thing.
    pub fn on_column(self, measure: bool) -> SortOrder {
        match (self, measure) {
            (SortOrder::AbsDesc, false) => SortOrder::Desc,
            (SortOrder::AbsAsc, false) => SortOrder::Asc,
            (order, _) => order,
        }
    }

    /// What `s` (`absolute == false`) or `S` (`absolute == true`) does to
    /// a column whose current order is `current`: `s` walks asc → desc →
    /// clear, `S` walks abs desc → abs asc → clear, and either key pressed
    /// while the other's order is showing starts its own cycle afresh
    /// rather than continuing a cycle the trader did not choose. `S` on a
    /// column with no magnitude (`measure == false`) leaves `current` as
    /// it is — a sort it did apply would just be `s`'s.
    pub fn cycle(current: Option<SortOrder>, absolute: bool, measure: bool) -> Option<SortOrder> {
        if absolute && !measure {
            return current;
        }
        if absolute {
            match current {
                Some(SortOrder::AbsDesc) => Some(SortOrder::AbsAsc),
                Some(SortOrder::AbsAsc) => None,
                _ => Some(SortOrder::AbsDesc),
            }
        } else {
            match current {
                Some(SortOrder::Asc) => Some(SortOrder::Desc),
                Some(SortOrder::Desc) => None,
                _ => Some(SortOrder::Asc),
            }
        }
    }

    /// Header clicks cycle a measure through desc → asc → abs desc → abs asc
    /// → clear. Text columns skip the absolute orders. Clicking a different
    /// column starts its cycle at desc.
    pub fn click_cycle(current: Option<SortOrder>, measure: bool) -> Option<SortOrder> {
        match current {
            Some(SortOrder::Desc) => Some(SortOrder::Asc),
            Some(SortOrder::Asc) if measure => Some(SortOrder::AbsDesc),
            Some(SortOrder::Asc) => None,
            Some(SortOrder::AbsDesc) => Some(SortOrder::AbsAsc),
            Some(SortOrder::AbsAsc) => None,
            None => Some(SortOrder::Desc),
        }
    }
}

/// A parsed `:sort` argument list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SortArg {
    /// `:sort <column> [asc|desc|abs [asc|desc]]`. The column is only a
    /// word here: the tile resolves it against its own columns.
    Column { column: String, order: SortOrder },
    /// `:sort clear`.
    Clear,
}

/// Parse the words after `sort`. A bare column is ascending; a bare `abs`
/// is `abs desc`, the biggest exposures first. Errors name the grammar.
pub fn parse_args(rest: &str) -> Result<SortArg, String> {
    let mut words = rest.split_whitespace();
    let column = match words.next() {
        None => return Err("sort needs a column, or `clear`".into()),
        Some("clear") => {
            return if words.next().is_none() {
                Ok(SortArg::Clear)
            } else {
                Err("sort clear takes nothing after it".into())
            };
        }
        Some(c) => c,
    };
    let order = match (words.next(), words.next(), words.next()) {
        (None, _, _) => SortOrder::Asc,
        (Some("asc"), None, _) => SortOrder::Asc,
        (Some("desc"), None, _) => SortOrder::Desc,
        (Some("abs"), None, _) | (Some("abs"), Some("desc"), None) => SortOrder::AbsDesc,
        (Some("abs"), Some("asc"), None) => SortOrder::AbsAsc,
        _ => {
            return Err(
                "sort takes a column and optionally `asc`, `desc`, `abs`, `abs asc` or `abs desc`"
                    .into(),
            );
        }
    };
    Ok(SortArg::Column {
        column: column.into(),
        order,
    })
}

/// The candidates for the word after `sort` and the completed words
/// `after` it (`after` excludes `sort` itself and the word under the
/// cursor), unsorted: the tile's column names and `clear`, then the
/// order words. Empty past the end of the grammar.
pub fn completions(after: &[&str], columns: &[String]) -> Vec<String> {
    let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect();
    match after {
        [] => columns
            .iter()
            .cloned()
            .chain(["clear".to_string()])
            .collect(),
        ["clear"] => Vec::new(),
        [_] => words(&["abs", "asc", "desc"]),
        [_, "abs"] => words(&["asc", "desc"]),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_cycles_walk_their_own_orders_and_restart_from_the_others() {
        use SortOrder::*;
        let s = |current| SortOrder::cycle(current, false, true);
        let big_s = |current| SortOrder::cycle(current, true, true);
        assert_eq!(s(None), Some(Asc));
        assert_eq!(s(Some(Asc)), Some(Desc));
        assert_eq!(s(Some(Desc)), None);
        assert_eq!(big_s(None), Some(AbsDesc));
        assert_eq!(big_s(Some(AbsDesc)), Some(AbsAsc));
        assert_eq!(big_s(Some(AbsAsc)), None);
        // Crossing over restarts the pressed key's own cycle.
        assert_eq!(s(Some(AbsDesc)), Some(Asc));
        assert_eq!(s(Some(AbsAsc)), Some(Asc));
        assert_eq!(big_s(Some(Asc)), Some(AbsDesc));
        assert_eq!(big_s(Some(Desc)), Some(AbsDesc));
    }

    #[test]
    fn a_header_click_walks_every_order_a_measure_can_show_desc_first() {
        use SortOrder::*;
        let mut current = None;
        let mut seen = Vec::new();
        for _ in 0..5 {
            current = SortOrder::click_cycle(current, true);
            seen.push(current);
        }
        assert_eq!(
            seen,
            vec![Some(Desc), Some(Asc), Some(AbsDesc), Some(AbsAsc), None]
        );
    }

    #[test]
    fn a_header_click_on_a_text_column_skips_the_absolute_pair() {
        use SortOrder::*;
        let mut current = None;
        let mut seen = Vec::new();
        for _ in 0..3 {
            current = SortOrder::click_cycle(current, false);
            seen.push(current);
        }
        assert_eq!(seen, vec![Some(Desc), Some(Asc), None]);
        // A text column can never hold an absolute order (`on_column`),
        // but were one there, a click still ends the cycle rather than
        // looping inside the pair.
        assert_eq!(SortOrder::click_cycle(Some(AbsDesc), false), Some(AbsAsc));
        assert_eq!(SortOrder::click_cycle(Some(AbsAsc), false), None);
    }

    #[test]
    fn shift_s_is_inert_on_a_column_with_no_magnitude_where_s_is_not() {
        use SortOrder::*;
        for current in [None, Some(Asc), Some(Desc)] {
            assert_eq!(
                SortOrder::cycle(current, true, false),
                current,
                "{current:?}"
            );
        }
        assert_eq!(SortOrder::cycle(None, false, false), Some(Asc));
        assert_eq!(SortOrder::cycle(Some(Asc), false, false), Some(Desc));
    }

    #[test]
    fn an_absolute_order_asked_of_a_text_column_becomes_its_signed_direction() {
        use SortOrder::*;
        assert_eq!(AbsDesc.on_column(false), Desc);
        assert_eq!(AbsAsc.on_column(false), Asc);
        assert_eq!(Desc.on_column(false), Desc);
        for order in [Asc, Desc, AbsDesc, AbsAsc] {
            assert_eq!(order.on_column(true), order, "a measure keeps {order:?}");
        }
    }

    #[test]
    fn the_sort_grammar_parses_every_order_and_names_itself_on_error() {
        let col = |order| {
            Ok(SortArg::Column {
                column: "delta01".into(),
                order,
            })
        };
        assert_eq!(parse_args("delta01"), col(SortOrder::Asc));
        assert_eq!(parse_args("delta01 asc"), col(SortOrder::Asc));
        assert_eq!(parse_args("delta01 desc"), col(SortOrder::Desc));
        assert_eq!(parse_args("delta01 abs"), col(SortOrder::AbsDesc));
        assert_eq!(parse_args("delta01 abs desc"), col(SortOrder::AbsDesc));
        assert_eq!(parse_args("delta01 abs asc"), col(SortOrder::AbsAsc));
        assert_eq!(parse_args("  delta01  "), col(SortOrder::Asc));
        assert_eq!(parse_args("clear"), Ok(SortArg::Clear));
        assert!(parse_args("").unwrap_err().contains("column"));
        assert!(parse_args("delta01 up").unwrap_err().contains("abs"));
        assert!(parse_args("clear extra").unwrap_err().contains("clear"));
        assert!(parse_args("delta01 abs up").unwrap_err().contains("abs"));
        assert!(parse_args("delta01 desc abs").unwrap_err().contains("abs"));
    }

    #[test]
    fn sort_completions_offer_columns_then_orders() {
        let cols = vec!["npv".to_string(), "qty".to_string()];
        assert_eq!(completions(&[], &cols), vec!["npv", "qty", "clear"]);
        assert_eq!(completions(&["npv"], &cols), vec!["abs", "asc", "desc"]);
        assert_eq!(completions(&["npv", "abs"], &cols), vec!["asc", "desc"]);
        assert!(completions(&["clear"], &cols).is_empty());
        assert!(completions(&["npv", "desc"], &cols).is_empty());
    }
}
