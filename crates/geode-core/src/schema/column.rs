//! Column declarations (spec §3.6). Grain, requiredness, and textual-search
//! participation are all declared, never inferred — spec §3.5 exists because
//! today's grain assignments are informed guesses.

use super::grain::Grain;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    Utf8,
    F64,
    I64,
    Date,
    Timestamp,
    Bool,
}

impl ColumnType {
    /// The DuckDB type this maps to in generated DDL.
    pub fn sql(self) -> &'static str {
        match self {
            ColumnType::Utf8 => "VARCHAR",
            ColumnType::F64 => "DOUBLE",
            ColumnType::I64 => "BIGINT",
            ColumnType::Date => "DATE",
            ColumnType::Timestamp => "TIMESTAMP",
            ColumnType::Bool => "BOOLEAN",
        }
    }

    pub fn parse(s: &str) -> Option<ColumnType> {
        match s {
            "utf8" | "string" => Some(ColumnType::Utf8),
            "f64" | "double" => Some(ColumnType::F64),
            "i64" | "bigint" => Some(ColumnType::I64),
            "date" => Some(ColumnType::Date),
            "timestamp" => Some(ColumnType::Timestamp),
            "bool" => Some(ColumnType::Bool),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aggregate {
    Sum,
    Min,
    Max,
    Any,
}

impl Aggregate {
    pub fn sql(self, expr: &str) -> String {
        match self {
            Aggregate::Sum => format!("sum({expr})"),
            Aggregate::Min => format!("min({expr})"),
            Aggregate::Max => format!("max({expr})"),
            Aggregate::Any => format!("any_value({expr})"),
        }
    }

    pub fn parse(s: &str) -> Option<Aggregate> {
        match s {
            "sum" => Some(Aggregate::Sum),
            "min" => Some(Aggregate::Min),
            "max" => Some(Aggregate::Max),
            "any" => Some(Aggregate::Any),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnRole {
    /// Part of a grain key.
    Key,
    /// Scopeable and groupable. `None` is a column of a built-in grain
    /// key (`Grain::key_columns`); `Some(g)` is a *carried* dimension —
    /// one value per row of `g`'s key, carried by `g` and every finer
    /// grain, stored as a payload column, never added to a key
    /// (spec §3.3). `g` must be a grain whose *dimension* key contains
    /// `g`'s own key (`validate_dataset` rejects it otherwise): the pair
    /// grain fails this — its dimension key collapses to the instrument
    /// key — so `grain = "underlying_pair"` is never carriable by anything.
    Dimension { grain: Option<Grain> },
    /// A number, aggregated at its declared grain.
    Measure { grain: Grain, aggregate: Aggregate },
    /// A non-numeric property. `Some(grain)` on a measure dataset: carried
    /// at that grain. `None` on a document dataset: document-level — one
    /// value per document, repeated on every row of it (market-data
    /// spec §3.1). `validate_dataset` refuses each reading on the other
    /// family, so `None` never reaches the grain tables.
    Attribute { grain: Option<Grain> },
    /// Document family only: identifies a row within a document, in the
    /// dataset's declared `axes` order (market-data spec §3.1).
    Axis,
    /// Document family only: a numeric cell of the document.
    Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSpec {
    pub name: String,
    /// The name in the source file, when it differs (spec §5.1 column map).
    pub source_name: Option<String>,
    pub ty: ColumnType,
    /// Absent-and-required is a health warning; absent-and-optional is
    /// expected and silent (spec §3.6).
    pub required: bool,
    /// Participates in the global text filter (spec §4.1).
    pub textual: bool,
    /// Small enough vocabulary to be an ENUM: interned at ingest, given a
    /// picker, and matched by dictionary in the text filter (spec §3.3).
    /// Defaults to `true` for a dimension, `false` otherwise.
    pub categorical: bool,
    pub role: ColumnRole,
}

impl ColumnSpec {
    pub fn source_name(&self) -> &str {
        self.source_name.as_deref().unwrap_or(&self.name)
    }

    /// The grain a measure or attribute is declared at. `None` for keys
    /// and for every dimension, carried or not: a carried dimension is
    /// reached through [`Self::carried_grain`] so the by-grain payload
    /// paths (`grains()`, `measures_at`, `attributes_at`) keep meaning
    /// "declared at exactly this grain".
    pub fn grain(&self) -> Option<Grain> {
        match self.role {
            ColumnRole::Measure { grain, .. } | ColumnRole::Attribute { grain: Some(grain) } => {
                Some(grain)
            }
            ColumnRole::Attribute { grain: None }
            | ColumnRole::Key
            | ColumnRole::Dimension { .. }
            | ColumnRole::Axis
            | ColumnRole::Value => None,
        }
    }

    /// `Some` for a carried dimension: the grain whose key determines it.
    pub fn carried_grain(&self) -> Option<Grain> {
        match self.role {
            ColumnRole::Dimension { grain } => grain,
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_default() -> ColumnSpec {
        ColumnSpec {
            name: String::new(),
            source_name: None,
            ty: ColumnType::F64,
            required: true,
            textual: false,
            categorical: true,
            role: ColumnRole::Dimension { grain: None },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_name_falls_back_to_canonical_name() {
        let mapped = ColumnSpec {
            name: "delta01".into(),
            source_name: Some("Delta01".into()),
            ..ColumnSpec::test_default()
        };
        let plain = ColumnSpec {
            name: "npv".into(),
            ..ColumnSpec::test_default()
        };
        assert_eq!(mapped.source_name(), "Delta01");
        assert_eq!(plain.source_name(), "npv");
    }

    #[test]
    fn measure_grain_is_reachable_from_role() {
        let m = ColumnSpec {
            name: "daily_trading_pnl".into(),
            role: ColumnRole::Measure {
                grain: Grain::Position,
                aggregate: Aggregate::Sum,
            },
            ..ColumnSpec::test_default()
        };
        assert_eq!(m.grain(), Some(Grain::Position));
        assert_eq!(ColumnSpec::test_default().grain(), None);
    }
}
