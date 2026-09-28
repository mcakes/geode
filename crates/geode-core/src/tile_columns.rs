//! The columns a tile presents from a named view, and the one at its cursor.
//! The shell reads the focused tile's `TileContent::tile_columns` to offer
//! a column to edit in the Views or Schema dialog; the tile reports names
//! only and the shell resolves them against current configuration, so a
//! tile never needs the dialog's vocabulary.

/// A tile's presented view columns, in display order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TileColumns {
    /// The view the tile shows, by name.
    pub view: String,
    pub columns: Vec<TileColumn>,
    /// Index into `columns`; `None` when the cursor is on no listed column
    /// (a tree column, or no cursor yet).
    pub active: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileColumn {
    /// The view/dataset column name — the identity every lookup uses.
    pub name: String,
    /// The header label as painted.
    pub label: String,
    /// A view-derived column: no dataset declares it, so only Views edits it.
    pub derived: bool,
}

impl TileColumns {
    /// The column at the cursor, if any.
    pub fn active_column(&self) -> Option<&TileColumn> {
        self.active.and_then(|ix| self.columns.get(ix))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str) -> TileColumn {
        TileColumn {
            name: name.into(),
            label: name.into(),
            derived: false,
        }
    }

    #[test]
    fn the_active_column_is_the_indexed_one_or_none() {
        let mut t = TileColumns {
            view: "tree".into(),
            columns: vec![col("npv"), col("delta01")],
            active: Some(1),
        };
        assert_eq!(t.active_column().map(|c| c.name.as_str()), Some("delta01"));
        t.active = None;
        assert_eq!(t.active_column(), None);
        t.active = Some(9);
        assert_eq!(t.active_column(), None, "an out-of-range index names nothing");
    }
}
