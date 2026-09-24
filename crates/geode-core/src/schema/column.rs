//! Column declarations read from dataset configuration. Type, role, grain,
//! requiredness, and text-search participation determine ingestion and query
//! behavior; the reader applies defaults only where the format allows them.

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
    /// Scopeable and groupable. On measures, `None` identifies a built-in
    /// key dimension; `Some(g)` is a payload dimension carried wherever the
    /// dimension key contains `g`'s key. The pair grain cannot declare one:
    /// its dimension key stops at instrument and excludes its pair identity.
    /// Document dimensions are grainless and must belong to the document key.
    Dimension { grain: Option<Grain> },
    /// A number, aggregated at its declared grain.
    Measure { grain: Grain, aggregate: Aggregate },
    /// A property stored at `Some(grain)` for measures or at document level
    /// for `None`, repeated on its rows. Validators reject grainless measure
    /// attributes and grain-bearing document attributes. Supported types are
    /// checked by dataset family; attributes need not be non-numeric.
    Attribute { grain: Option<Grain> },
    /// Document row identity, in the dataset's declared `axes` order.
    Axis,
    /// Document family only: a per-row f64, i64, date, or text value.
    Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSpec {
    pub name: String,
    /// The source-file column name, when different from the schema name.
    pub source_name: Option<String>,
    pub ty: ColumnType,
    /// Missing required columns produce health warnings; optional omissions
    /// are expected and silent.
    pub required: bool,
    /// Participates in the text filter.
    pub textual: bool,
    /// A string vocabulary suitable for interning, pickers, and dictionary
    /// text matching. Defaults to true for utf8 dimensions and false otherwise.
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
