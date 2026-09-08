//! The `Domain::Views` adapter (spec §8.1): the doc views live in, and
//! the one-line summary a view's browse row shows.
//!
//! One module per domain, holding that domain's own functions and
//! nothing else (spec §4) — the scaffold matches on `Domain` exactly
//! once per function, so an adapter never learns about the stage
//! machine, the key vocabulary or the markers, and the markers never
//! learn about views.
//!
//! Views goes first on purpose (spec §14). It is the only domain whose
//! edit stage has to split one list of columns across two destinations —
//! order, inclusion and width to `view_presentation.toml`, the column
//! set itself to `views.toml` — so building it first settles the
//! vocabulary before three thinner adapters depend on it.

/// The config doc name (file stem), as `Config::layered_docs` keys it.
pub const DOC: &str = "views";

/// The muted second line of a view's browse row: what the view selects,
/// in the order a trader would ask it — which dataset, how many columns,
/// and the rollup it builds.
///
/// Read straight off the TOML value rather than through `ViewSpec` on
/// purpose. `ViewSpec::from_doc` reads the *merged* doc and would need
/// the whole document to produce one view, while this is handed exactly
/// the one table whose provenance the row is reporting — and it must
/// keep describing a view even when that view is malformed, because a
/// browse row a user cannot see is a view they cannot open and fix.
/// That is also why a non-table value still produces a row: `views.toml`
/// with `tree = "oops"` in it is precisely when the dialog needs to show
/// `tree`.
pub fn summary(value: &toml::Value) -> String {
    let Some(table) = value.as_table() else {
        return "not a table".to_string();
    };
    let dataset = table
        .get("dataset")
        .and_then(|v| v.as_str())
        .unwrap_or("no dataset");
    let columns = table
        .get("columns")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let mut out = match columns {
        1 => format!("{dataset} · 1 column"),
        n => format!("{dataset} · {n} columns"),
    };
    // The grouping is the view's shape, not decoration: two views over
    // the same dataset and the same columns are told apart by nothing
    // else. Named rather than counted for that reason, and with the same
    // arrow the blotter's own header uses for a rollup path.
    let grouping: Vec<&str> = table
        .get("grouping")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    if !grouping.is_empty() {
        out.push_str(" · grouped by ");
        out.push_str(&grouping.join(" → "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str) -> toml::Value {
        toml::Value::Table(text.parse::<toml::Table>().expect("fixture parses"))
    }

    #[test]
    fn the_summary_names_the_dataset_the_columns_and_the_rollup() {
        let v = value(
            "dataset = \"risk_snapshot\"\ngrouping = [\"lhu\", \"position_ref\"]\n\
             [[columns]]\nname = \"npv\"\n[[columns]]\nname = \"delta01\"\n",
        );
        assert_eq!(
            summary(&v),
            "risk_snapshot · 2 columns · grouped by lhu → position_ref"
        );
    }

    /// A flat view has no rollup to name, and one column is not "1
    /// columns" — the row is read at a glance, so the grammar matters.
    #[test]
    fn a_flat_single_column_view_reads_as_prose() {
        let v = value("dataset = \"risk\"\n[[columns]]\nname = \"npv\"\n");
        assert_eq!(summary(&v), "risk · 1 column");
    }

    /// A malformed view is exactly the one a user opens the dialog to
    /// fix, so it still gets a row and the row still says what is wrong
    /// with it rather than showing an empty line.
    #[test]
    fn a_malformed_view_still_describes_itself() {
        assert_eq!(summary(&toml::Value::String("oops".into())), "not a table");
        assert_eq!(summary(&value("columns = []\n")), "no dataset · 0 columns");
    }
}
