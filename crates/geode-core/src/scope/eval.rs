//! In-process scope evaluation over one row, for data that never reaches
//! DuckDB (the pricer's sheet lines).
//!
//! The SQL lowering in `geode-data`'s `scope_sql` is the authority: this
//! module copies what DuckDB does with the predicate that lowering emits,
//! and `geode-data`'s `eval_parity` fixture runs every operator through
//! both paths and asserts equal row sets. Where the two could differ, the
//! fixture decides and this module follows the SQL — including DuckDB's
//! cross-type casts, which are not what a reader would guess:
//!
//! - A number against a text column casts the *column* to DOUBLE for `=`,
//!   `!=` and `in`, one row at a time: a row whose text is not a number is
//!   an error. Ordering (`<` …) between text and a number is refused
//!   outright.
//! - Text against an i64 column casts the *literal* to BIGINT, rounding
//!   half away from zero (`qty = '2.5'` is `qty = 3`); a number against an
//!   i64 column compares as DOUBLE (`qty = 2.5` matches nothing).
//! - `like` is DuckDB's `ilike` with no ESCAPE clause: case-insensitive,
//!   `%` any run, `_` one character, and a backslash is an ordinary
//!   character. It is refused on anything but a text column and a text
//!   pattern.
//! - NaN is equal to itself and greater than every other number.
//!
//! Errors come in three kinds, as they do in DuckDB. Compile and bind
//! refusals (an unknown column, an ordering on a derived dimension, a type
//! pair DuckDB will not compare) fail whatever the row. A literal that does
//! not convert fails whatever the row too, *unless* a constant from a
//! derived dimension decides the enclosing `and`/`or` first: DuckDB folds
//! `false and x` before it converts `x`'s literal. A row value that does not
//! convert fails that row only.

use super::{CompareOp, Expr, Literal, Scope, derived_op_error};
use crate::dimensions::{DerivedDimension, DerivedDimensions};
use crate::document::Value;
use crate::schema::{ColumnType, DatasetSpec, Grain};
use chrono::NaiveDate;
use std::cmp::Ordering;

/// A row the evaluator reads by column name. `None` is SQL NULL.
pub trait RowValues {
    fn value(&self, column: &str) -> Option<Value>;
}

impl Expr {
    /// SQL three-valued result: `Ok(None)` is UNKNOWN (a NULL reached a
    /// comparison). `Err` is a comparison DuckDB would reject.
    pub fn evaluate(
        &self,
        row: &dyn RowValues,
        ds: &DatasetSpec,
        dims: &DerivedDimensions,
    ) -> Result<Option<bool>, String> {
        self.bind(ds, dims)?;
        self.eval(row, ds, dims)
    }

    /// The row-independent refusals: what `scope_sql` refuses at compile
    /// time and what DuckDB's binder refuses before it looks at any data.
    /// These are never folded away by a constant.
    fn bind(&self, ds: &DatasetSpec, dims: &DerivedDimensions) -> Result<(), String> {
        match self {
            Expr::And(a, b) | Expr::Or(a, b) => {
                a.bind(ds, dims)?;
                b.bind(ds, dims)
            }
            Expr::Not(e) => e.bind(ds, dims),
            Expr::Compare { column, op, value } => {
                if dims.get(column).is_some() {
                    if let Some(err) = derived_op_error(column, op.grammar()) {
                        return Err(err);
                    }
                    derived_source_type(ds, dims, column)?;
                    return Ok(());
                }
                let ty = scopeable(ds, dims, column)?;
                supported(column, ty)?;
                match (op, ty, value) {
                    (CompareOp::Like, ColumnType::Utf8, Literal::Str(_)) => Ok(()),
                    (CompareOp::Like, ColumnType::Utf8, _) => Err(format!(
                        "'{column}' like needs a text pattern, not {}",
                        literal_kind(value)
                    )),
                    (CompareOp::Like, _, _) => Err(format!(
                        "'{column}' is not a text column, so like has no meaning on it"
                    )),
                    (
                        CompareOp::Lt | CompareOp::Le | CompareOp::Gt | CompareOp::Ge,
                        ColumnType::Utf8,
                        Literal::Num(_) | Literal::Bool(_),
                    ) => Err(format!(
                        "'{column}' is a text column and cannot be ordered against {}",
                        literal_kind(value)
                    )),
                    _ => Ok(()),
                }
            }
            Expr::In { column, .. } => {
                if dims.get(column).is_some() {
                    derived_source_type(ds, dims, column)?;
                    return Ok(());
                }
                supported(column, scopeable(ds, dims, column)?)
            }
        }
    }

    /// `Some` when the SQL lowering makes this a constant: a derived value
    /// no source maps to, and whatever that constant decides through
    /// `and`, `or` and `not`. DuckDB folds these before it converts any
    /// literal, so a constant hides a conversion error beside it.
    fn constant(&self, dims: &DerivedDimensions) -> Option<bool> {
        match self {
            Expr::And(a, b) => match (a.constant(dims), b.constant(dims)) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            },
            Expr::Or(a, b) => match (a.constant(dims), b.constant(dims)) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            },
            Expr::Not(e) => e.constant(dims).map(|c| !c),
            Expr::Compare { column, op, value } => {
                let d = dims.get(column)?;
                let negated = match op {
                    CompareOp::Eq => false,
                    CompareOp::Ne => true,
                    _ => return None,
                };
                has_no_source(d, std::slice::from_ref(value)).then_some(negated)
            }
            Expr::In { column, values } => {
                let d = dims.get(column)?;
                has_no_source(d, values).then_some(false)
            }
        }
    }

    /// Evaluate a bound expression. Both sides of `and`/`or` are evaluated
    /// unless a constant decides the node, so an error on either side is
    /// reported the way DuckDB reports a literal it cannot convert.
    fn eval(
        &self,
        row: &dyn RowValues,
        ds: &DatasetSpec,
        dims: &DerivedDimensions,
    ) -> Result<Option<bool>, String> {
        if let Some(c) = self.constant(dims) {
            return Ok(Some(c));
        }
        Ok(match self {
            Expr::And(a, b) => and(a.eval(row, ds, dims)?, b.eval(row, ds, dims)?),
            Expr::Or(a, b) => or(a.eval(row, ds, dims)?, b.eval(row, ds, dims)?),
            Expr::Not(e) => e.eval(row, ds, dims)?.map(|v| !v),
            Expr::Compare { column, op, value } => match dims.get(column) {
                Some(d) => {
                    let member = derived_member(d, std::slice::from_ref(value), row)?;
                    if *op == CompareOp::Ne {
                        member.map(|m| !m)
                    } else {
                        member
                    }
                }
                None if *op == CompareOp::Like => {
                    let Literal::Str(pattern) = value else {
                        unreachable!("bind refuses a like with a non-text pattern")
                    };
                    match row.value(column) {
                        None => None,
                        Some(Value::Utf8(s)) => Some(like(&s, pattern)),
                        Some(other) => return Err(mismatch(column, &other)),
                    }
                }
                None => {
                    let ty = column_type(ds, dims, column);
                    let domain = Domain::of(ty, std::slice::from_ref(value));
                    let lit = domain.literal(column, value)?;
                    match row.value(column) {
                        None => None,
                        Some(v) => {
                            let v = domain.row(column, &v)?;
                            Some(test(*op, v.cmp_sql(&lit)))
                        }
                    }
                }
            },
            Expr::In { column, values } => match dims.get(column) {
                Some(d) => derived_member(d, values, row)?,
                None => {
                    let ty = column_type(ds, dims, column);
                    let domain = Domain::of(ty, values);
                    let lits = values
                        .iter()
                        .map(|l| domain.literal(column, l))
                        .collect::<Result<Vec<_>, _>>()?;
                    match row.value(column) {
                        None => None,
                        Some(v) => {
                            let v = domain.row(column, &v)?;
                            Some(lits.iter().any(|l| v.cmp_sql(l) == Ordering::Equal))
                        }
                    }
                }
            },
        })
    }
}

impl Scope {
    /// Whether `row` is kept: every part must be TRUE (UNKNOWN drops the
    /// row, as a WHERE clause does). `impossible` keeps nothing. A scope
    /// still carrying `named` is an error — resolve it first.
    ///
    /// Refusals and constants are decided in `scope_sql`'s order: a
    /// selection of a derived value nothing maps to keeps nothing even when
    /// a later part would be refused, because the SQL returns `false`
    /// before compiling that part.
    pub fn matches(
        &self,
        row: &dyn RowValues,
        ds: &DatasetSpec,
        dims: &DerivedDimensions,
    ) -> Result<bool, String> {
        if !self.named.is_empty() {
            return Err("scope carries unresolved named expressions".into());
        }
        if self.impossible {
            return Ok(false);
        }
        let selections: Vec<_> = self
            .dimensions
            .iter()
            .filter(|s| !s.values.is_empty())
            .collect();
        for sel in &selections {
            if let Some(d) = dims.get(&sel.column)
                && !d.values.values().any(|v| sel.values.contains(v))
            {
                return Ok(false);
            }
            scopeable(ds, dims, &sel.column)?;
        }
        let textual: Vec<&str> = ds.textual_columns().map(|c| c.name.as_str()).collect();
        if self.text.is_some() {
            for name in &textual {
                scopeable(ds, dims, name)?;
            }
        }
        if let Some(e) = &self.expression {
            e.bind(ds, dims)?;
        }
        // Binder refusals: a selection or a text search compares text with
        // the stored column, which DuckDB refuses for any other type.
        for sel in &selections {
            let base = dims.base_column(&sel.column);
            let ty = column_type(ds, dims, base);
            if ty != ColumnType::Utf8 {
                return Err(format!(
                    "'{}' is not a text column, so it cannot be selected by value",
                    sel.column
                ));
            }
        }
        if self.text.is_some() {
            for name in &textual {
                if column_type(ds, dims, name) != ColumnType::Utf8 {
                    return Err(format!(
                        "'{name}' is textual but not a text column, so the text filter cannot search it"
                    ));
                }
            }
        }
        // Constants fold the whole predicate before any literal converts:
        // a text filter with nothing to search is `false`.
        if self.text.is_some() && textual.is_empty() {
            return Ok(false);
        }
        if let Some(e) = &self.expression
            && e.constant(dims) == Some(false)
        {
            return Ok(false);
        }

        let mut kept = Some(true);
        for sel in &selections {
            let member = match dims.get(&sel.column) {
                Some(d) => match row.value(&d.from) {
                    None => None,
                    Some(Value::Utf8(s)) => Some(
                        d.values
                            .get(&s)
                            .is_some_and(|derived| sel.values.iter().any(|w| w == derived)),
                    ),
                    Some(other) => return Err(mismatch(&d.from, &other)),
                },
                None => match row.value(&sel.column) {
                    None => None,
                    Some(Value::Utf8(s)) => Some(sel.values.contains(&s)),
                    Some(other) => return Err(mismatch(&sel.column, &other)),
                },
            };
            kept = and(kept, member);
        }
        if let Some(needle) = &self.text {
            let needle = needle.to_lowercase();
            let mut found = Some(false);
            for name in &textual {
                let hit = match row.value(name) {
                    None => None,
                    Some(Value::Utf8(s)) => Some(s.to_lowercase().contains(&needle)),
                    Some(other) => return Err(mismatch(name, &other)),
                };
                found = or(found, hit);
            }
            kept = and(kept, found);
        }
        if let Some(e) = &self.expression {
            kept = and(kept, e.eval(row, ds, dims)?);
        }
        Ok(kept == Some(true))
    }
}

/// Kleene AND: FALSE wins, then UNKNOWN.
fn and(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    }
}

/// Kleene OR: TRUE wins, then UNKNOWN.
fn or(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    }
}

/// The type of a column `scope_sql` can scope by, or its refusal: the
/// same test `route` applies. A grain key the dataset does not declare is
/// scopeable and is text.
fn scopeable(
    ds: &DatasetSpec,
    dims: &DerivedDimensions,
    column: &str,
) -> Result<ColumnType, String> {
    let base = dims.base_column(column);
    let carried = Grain::ALL.iter().any(|g| ds.carries(*g, base));
    match ds.column(base) {
        None if !carried => Err(format!(
            "'{column}' is not a column in dataset '{}'",
            ds.name
        )),
        Some(col) if col.grain().is_none() && !carried => Err(format!(
            "'{column}' is not carried as a dimension by any grain, so it cannot be scoped"
        )),
        Some(col) => Ok(col.ty),
        None => Ok(ColumnType::Utf8),
    }
}

/// The declared type of an already-scopeable column.
fn column_type(ds: &DatasetSpec, dims: &DerivedDimensions, column: &str) -> ColumnType {
    ds.column(dims.base_column(column))
        .map_or(ColumnType::Utf8, |c| c.ty)
}

/// Row values carry only the document value types; a timestamp or boolean
/// column has no row representation to compare.
fn supported(column: &str, ty: ColumnType) -> Result<(), String> {
    match ty {
        ColumnType::Timestamp | ColumnType::Bool => Err(format!(
            "'{column}' has a type the in-process scope cannot compare"
        )),
        _ => Ok(()),
    }
}

/// A derived dimension's source column, which must be text: its map binds
/// text source values against it.
fn derived_source_type(
    ds: &DatasetSpec,
    dims: &DerivedDimensions,
    column: &str,
) -> Result<(), String> {
    match scopeable(ds, dims, column)? {
        ColumnType::Utf8 => Ok(()),
        _ => Err(format!(
            "'{column}' is derived from a column that is not text, which the in-process scope cannot compare"
        )),
    }
}

/// Whether no source value maps to any wanted derived value. Only text
/// literals name a derived value; a number names none, as in
/// `derived_membership`.
fn has_no_source(d: &DerivedDimension, wanted: &[Literal]) -> bool {
    !d.values.values().any(|derived| wanted_by(wanted, derived))
}

fn wanted_by(wanted: &[Literal], derived: &str) -> bool {
    wanted
        .iter()
        .any(|w| matches!(w, Literal::Str(s) if s == derived))
}

/// Source-value membership: the row's source value maps to a wanted
/// derived value. A source value the map does not know is not a member
/// (and under `!=` is kept), exactly as `"und" not in (…)` treats it.
fn derived_member(
    d: &DerivedDimension,
    wanted: &[Literal],
    row: &dyn RowValues,
) -> Result<Option<bool>, String> {
    Ok(match row.value(&d.from) {
        None => None,
        Some(Value::Utf8(s)) => Some(
            d.values
                .get(&s)
                .is_some_and(|derived| wanted_by(wanted, derived)),
        ),
        Some(other) => return Err(mismatch(&d.from, &other)),
    })
}

fn literal_kind(l: &Literal) -> &'static str {
    match l {
        Literal::Str(_) => "text",
        Literal::Num(_) => "a number",
        Literal::Bool(_) => "a boolean",
    }
}

fn mismatch(column: &str, v: &Value) -> String {
    format!(
        "'{column}' holds a {:?} value where its declared type differs",
        v.column_type()
    )
}

/// The type DuckDB compares in, chosen from the column type and every
/// literal of the comparison (one for `Compare`, the list for `In`).
#[derive(Clone, Copy)]
enum Domain {
    Text,
    F64,
    I64,
    Bool,
    Date,
}

/// A value in its comparison domain.
enum Scalar<'a> {
    Text(std::borrow::Cow<'a, str>),
    F64(f64),
    I64(i64),
    Bool(bool),
    Date(NaiveDate),
}

impl Domain {
    fn of(ty: ColumnType, literals: &[Literal]) -> Domain {
        let any_num = literals.iter().any(|l| matches!(l, Literal::Num(_)));
        let any_bool = literals.iter().any(|l| matches!(l, Literal::Bool(_)));
        match ty {
            // The column is cast to the literal's type, one row at a time.
            ColumnType::Utf8 if any_num => Domain::F64,
            ColumnType::Utf8 if any_bool => Domain::Bool,
            ColumnType::Utf8 => Domain::Text,
            ColumnType::F64 => Domain::F64,
            // A number widens the comparison to DOUBLE; text alone is cast
            // to the column's BIGINT.
            ColumnType::I64 if any_num => Domain::F64,
            ColumnType::I64 => Domain::I64,
            ColumnType::Date => Domain::Date,
            // `bind` refuses these before evaluation.
            ColumnType::Timestamp | ColumnType::Bool => Domain::Text,
        }
    }

    /// Convert a literal. A failure is row-independent: DuckDB converts a
    /// bound constant once, before it reads data.
    fn literal<'a>(self, column: &str, l: &'a Literal) -> Result<Scalar<'a>, String> {
        let refuse = || format!("{l} cannot be compared with '{column}'");
        Ok(match (self, l) {
            (Domain::Text, Literal::Str(s)) => Scalar::Text(s.as_str().into()),
            (Domain::F64, Literal::Num(n)) => Scalar::F64(*n),
            (Domain::F64, Literal::Str(s)) => Scalar::F64(text_to_f64(s).ok_or_else(refuse)?),
            (Domain::F64, Literal::Bool(b)) => Scalar::F64(if *b { 1.0 } else { 0.0 }),
            (Domain::I64, Literal::Str(s)) => Scalar::I64(text_to_i64(s).ok_or_else(refuse)?),
            (Domain::I64, Literal::Bool(b)) => Scalar::I64(i64::from(*b)),
            (Domain::Bool, Literal::Bool(b)) => Scalar::Bool(*b),
            (Domain::Bool, Literal::Str(s)) => Scalar::Bool(text_to_bool(s).ok_or_else(refuse)?),
            (Domain::Date, Literal::Str(s)) => Scalar::Date(text_to_date(s).ok_or_else(refuse)?),
            _ => return Err(refuse()),
        })
    }

    /// Convert a row value. A failure belongs to this row alone: DuckDB
    /// casts a text column value by value and fails on the first that
    /// does not convert.
    fn row<'a>(self, column: &str, v: &'a Value) -> Result<Scalar<'a>, String> {
        let refuse = |s: &str| {
            format!(
                "'{column}' value '{s}' cannot be compared as {}",
                self.name()
            )
        };
        Ok(match (self, v) {
            (Domain::Text, Value::Utf8(s)) => Scalar::Text(s.as_str().into()),
            (Domain::F64, Value::F64(n)) => Scalar::F64(*n),
            (Domain::F64, Value::I64(n)) => Scalar::F64(*n as f64),
            (Domain::F64, Value::Utf8(s)) => Scalar::F64(text_to_f64(s).ok_or_else(|| refuse(s))?),
            (Domain::I64, Value::I64(n)) => Scalar::I64(*n),
            (Domain::Bool, Value::Utf8(s)) => {
                Scalar::Bool(text_to_bool(s).ok_or_else(|| refuse(s))?)
            }
            (Domain::Date, Value::Date(d)) => Scalar::Date(*d),
            (_, other) => return Err(mismatch(column, other)),
        })
    }

    fn name(self) -> &'static str {
        match self {
            Domain::Text => "text",
            Domain::F64 | Domain::I64 => "a number",
            Domain::Bool => "a boolean",
            Domain::Date => "a date",
        }
    }
}

impl Scalar<'_> {
    /// DuckDB's ordering: text by bytes (its default binary collation),
    /// NaN equal to itself and above every other number.
    fn cmp_sql(&self, other: &Scalar<'_>) -> Ordering {
        match (self, other) {
            (Scalar::Text(a), Scalar::Text(b)) => a.as_bytes().cmp(b.as_bytes()),
            (Scalar::F64(a), Scalar::F64(b)) => match (a.is_nan(), b.is_nan()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => a.partial_cmp(b).expect("neither is NaN"),
            },
            (Scalar::I64(a), Scalar::I64(b)) => a.cmp(b),
            (Scalar::Bool(a), Scalar::Bool(b)) => a.cmp(b),
            (Scalar::Date(a), Scalar::Date(b)) => a.cmp(b),
            _ => unreachable!("both sides are converted into one domain"),
        }
    }
}

fn test(op: CompareOp, ord: Ordering) -> bool {
    match op {
        CompareOp::Eq => ord == Ordering::Equal,
        CompareOp::Ne => ord != Ordering::Equal,
        CompareOp::Lt => ord == Ordering::Less,
        CompareOp::Le => ord != Ordering::Greater,
        CompareOp::Gt => ord == Ordering::Greater,
        CompareOp::Ge => ord != Ordering::Less,
        CompareOp::Like => unreachable!("like is matched, not ordered"),
    }
}

/// VARCHAR → DOUBLE: surrounding whitespace is ignored; exponents, `inf`
/// and `nan` are accepted.
fn text_to_f64(s: &str) -> Option<f64> {
    s.trim().parse().ok()
}

/// VARCHAR → BIGINT: an integer, or a decimal rounded half away from zero
/// (`'2.5'` is 3, `'-2.5'` is -3, `'1e1'` is 10).
fn text_to_i64(s: &str) -> Option<i64> {
    let t = s.trim();
    if let Ok(n) = t.parse::<i64>() {
        return Some(n);
    }
    let f: f64 = t.parse().ok()?;
    let r = f.round();
    (r.is_finite() && r >= i64::MIN as f64 && r < i64::MAX as f64).then_some(r as i64)
}

/// VARCHAR → BOOLEAN. Not pinned by the parity fixture beyond refusing
/// non-boolean text; these are DuckDB's documented spellings.
fn text_to_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "t" | "1" => Some(true),
        "false" | "f" | "0" => Some(false),
        _ => None,
    }
}

/// VARCHAR → DATE in ISO form.
fn text_to_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

/// `value ilike pattern` with no ESCAPE clause: case-insensitive, `%`
/// matches any run of characters, `_` exactly one, and every other
/// character (a backslash included) matches itself.
fn like(value: &str, pattern: &str) -> bool {
    let v: Vec<char> = value.to_lowercase().chars().collect();
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    // Greedy wildcard match with one backtrack point, the last `%` seen.
    let (mut i, mut j) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while i < v.len() {
        if j < p.len() && (p[j] == '_' || (p[j] != '%' && p[j] == v[i])) {
            i += 1;
            j += 1;
        } else if j < p.len() && p[j] == '%' {
            star = Some((j, i));
            j += 1;
        } else if let Some((sj, si)) = star {
            j = sj + 1;
            i = si + 1;
            star = Some((sj, si + 1));
        } else {
            return false;
        }
    }
    p[j..].iter().all(|c| *c == '%')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ColumnRole, ColumnSpec};
    use crate::scope::{DimensionSelection, parse_expr};
    use std::collections::HashMap;

    struct Row(HashMap<&'static str, Value>);

    impl RowValues for Row {
        fn value(&self, column: &str) -> Option<Value> {
            self.0.get(column).cloned()
        }
    }

    fn ds() -> DatasetSpec {
        let col = |name: &str, ty| ColumnSpec {
            name: name.into(),
            source_name: None,
            ty,
            required: false,
            textual: ty == ColumnType::Utf8,
            categorical: false,
            role: ColumnRole::Dimension {
                grain: Some(Grain::Underlying),
            },
        };
        DatasetSpec {
            name: "t".into(),
            columns: vec![col("a", ColumnType::Utf8), col("n", ColumnType::F64)],
            ..DatasetSpec::default()
        }
    }

    fn row(a: Option<&str>, n: Option<f64>) -> Row {
        let mut m = HashMap::new();
        if let Some(a) = a {
            m.insert("a", Value::Utf8(a.into()));
        }
        if let Some(n) = n {
            m.insert("n", Value::F64(n));
        }
        Row(m)
    }

    fn eval(text: &str, r: &Row) -> Result<Option<bool>, String> {
        parse_expr(text)
            .unwrap()
            .evaluate(r, &ds(), &DerivedDimensions::default())
    }

    /// `a = 'x'` is TRUE, `a = 'y'` FALSE, `n > 0` UNKNOWN (n is NULL).
    #[test]
    fn and_and_or_follow_kleene_logic() {
        let r = row(Some("x"), None);
        for (text, want) in [
            ("a = 'x' and a = 'x'", Some(true)),
            ("a = 'x' and a = 'y'", Some(false)),
            ("a = 'x' and n > 0", None),
            ("a = 'y' and n > 0", Some(false)),
            ("a = 'x' or a = 'y'", Some(true)),
            ("a = 'y' or a = 'y'", Some(false)),
            ("a = 'x' or n > 0", Some(true)),
            ("a = 'y' or n > 0", None),
        ] {
            assert_eq!(eval(text, &r), Ok(want), "{text}");
        }
    }

    #[test]
    fn not_over_a_null_comparison_is_unknown_and_drops_the_row() {
        let r = row(None, Some(1.0));
        assert_eq!(eval("not (a = 'x')", &r), Ok(None));
        let scope = Scope {
            expression: Some(parse_expr("not (a = 'x')").unwrap()),
            ..Scope::default()
        };
        assert_eq!(
            scope.matches(&r, &ds(), &DerivedDimensions::default()),
            Ok(false)
        );
    }

    #[test]
    fn an_impossible_scope_keeps_nothing() {
        let scope = Scope {
            impossible: true,
            ..Scope::default()
        };
        let r = row(Some("x"), Some(1.0));
        assert_eq!(
            scope.matches(&r, &ds(), &DerivedDimensions::default()),
            Ok(false)
        );
        assert_eq!(
            Scope::default().matches(&r, &ds(), &DerivedDimensions::default()),
            Ok(true)
        );
    }

    #[test]
    fn a_scope_with_unresolved_names_is_refused() {
        let scope = Scope {
            named: vec!["liq".into()],
            dimensions: vec![DimensionSelection {
                column: "a".into(),
                values: vec!["x".into()],
            }],
            ..Scope::default()
        };
        assert!(
            scope
                .matches(&row(Some("x"), None), &ds(), &DerivedDimensions::default())
                .is_err()
        );
    }

    #[test]
    fn like_matches_wildcards_case_insensitively_with_no_escape() {
        assert!(like("SPX", "s%"));
        assert!(like("NDX", "_dx"));
        assert!(!like("NDX", "_x"));
        assert!(like("a\\b", "a\\b"));
        assert!(!like("50%off", "%\\%%"));
        assert!(like("abcabd", "%ab_"));
        assert!(like("", "%"));
        assert!(!like("", "_"));
        assert!(like("Ärger", "_rger"));
    }
}
