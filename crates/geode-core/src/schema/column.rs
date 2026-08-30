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
    /// Scopeable and groupable.
    Dimension,
    /// A number, aggregated at its declared grain.
    Measure { grain: Grain, aggregate: Aggregate },
    /// A non-numeric property carried at a grain (strike, expiry, currency).
    Attribute { grain: Grain },
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
    pub role: ColumnRole,
}

impl ColumnSpec {
    pub fn source_name(&self) -> &str {
        self.source_name.as_deref().unwrap_or(&self.name)
    }

    pub fn grain(&self) -> Option<Grain> {
        match self.role {
            ColumnRole::Measure { grain, .. } | ColumnRole::Attribute { grain } => Some(grain),
            ColumnRole::Key | ColumnRole::Dimension => None,
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
            role: ColumnRole::Dimension,
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
