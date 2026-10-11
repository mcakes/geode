//! The words the header and footer carry: the scope readout (`L1 / SPX`,
//! `· expr`, a muted `all`), the tile title, a column head's text and the
//! cursor readout (`1Y × 100 · 254.5`).

use crate::core::pivot::{AxisLabel, Head};
use geode_core::scope::Scope;

/// What an empty scope reads as, painted in the muted tone.
pub const ALL: &str = "all";

/// The effective scope's dimension selections as their values in
/// dimension order joined by ` / `, then `· <expr>`, `· "text"` and
/// `· ≡ name` per layer; `None` for an empty scope.
pub fn scope_readout(scope: &Scope) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let values: Vec<String> = scope
        .dimensions
        .iter()
        .filter(|d| !d.values.is_empty())
        .map(|d| d.values.join(", "))
        .collect();
    if !values.is_empty() {
        parts.push(values.join(" / "));
    }
    if let Some(e) = &scope.expression {
        parts.push(e.to_string());
    }
    if let Some(t) = &scope.text {
        parts.push(format!("\"{t}\""));
    }
    parts.extend(scope.named.iter().map(|n| format!("≡ {n}")));
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

/// `{title} · {readout}`, the stack list's row and the window's title.
pub fn tile_title(title: &str, scope: &Scope) -> String {
    format!(
        "{title} · {}",
        scope_readout(scope).unwrap_or_else(|| ALL.to_string())
    )
}

/// A column head as one line: the axis value, `outer › inner` for a
/// two-level header, a ladder's label.
pub fn head_text(head: &Head) -> String {
    match head {
        Head::Axis { outer: None, label } => label.text.clone(),
        Head::Axis {
            outer: Some(outer),
            label,
        } => format!("{} › {}", outer.text, label.text),
        Head::Measure { label, .. } => label.clone(),
    }
}

/// `1Y × 100 · 254.5`; a blank cell reads `—`.
pub fn cursor_readout(row: &AxisLabel, head: &Head, text: &str) -> String {
    let value = if text.is_empty() { "—" } else { text };
    format!("{} × {} · {value}", row.text, head_text(head))
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::scope::{DimensionSelection, parse_expr};

    fn label(text: &str) -> AxisLabel {
        AxisLabel {
            text: text.into(),
            number: text.parse().ok(),
        }
    }

    #[test]
    fn the_readout_is_the_selections_values_in_order_then_the_layers() {
        let mut scope = Scope::default();
        assert_eq!(scope_readout(&scope), None, "empty is `all`, painted muted");
        scope.dimensions.push(DimensionSelection {
            column: "lhu".into(),
            values: vec!["L1".into()],
        });
        scope.dimensions.push(DimensionSelection {
            column: "underlying_ref".into(),
            values: vec!["SPX".into(), "NDX".into()],
        });
        assert_eq!(scope_readout(&scope).as_deref(), Some("L1 / SPX, NDX"));
        scope.expression = Some(parse_expr("vega > 0").unwrap());
        scope.text = Some("call".into());
        scope.named = vec!["big".into()];
        assert_eq!(
            scope_readout(&scope).as_deref(),
            Some("L1 / SPX, NDX · vega > 0 · \"call\" · ≡ big")
        );
        assert_eq!(
            tile_title("Vega K/T", &scope),
            "Vega K/T · L1 / SPX, NDX · vega > 0 · \"call\" · ≡ big"
        );
        assert_eq!(tile_title("Vega K/T", &Scope::default()), "Vega K/T · all");
    }

    #[test]
    fn heads_and_the_cursor_readout_spell_the_cell() {
        let one = Head::Axis {
            outer: None,
            label: label("100"),
        };
        let two = Head::Axis {
            outer: Some(label("USD")),
            label: label("OIS"),
        };
        let m = Head::Measure {
            label: "P&L vs base".into(),
            unit: Some("USD".into()),
        };
        assert_eq!(head_text(&one), "100");
        assert_eq!(head_text(&two), "USD › OIS");
        assert_eq!(head_text(&m), "P&L vs base");
        assert_eq!(
            cursor_readout(&label("1Y"), &one, "254.5"),
            "1Y × 100 · 254.5"
        );
        assert_eq!(
            cursor_readout(&label("1Y"), &m, "8,240.0"),
            "1Y × P&L vs base · 8,240.0"
        );
        assert_eq!(cursor_readout(&label("1Y"), &two, ""), "1Y × USD › OIS · —");
    }
}
