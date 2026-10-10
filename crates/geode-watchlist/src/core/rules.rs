//! The rules popup's pure state: one row per rule as the popup lists it,
//! and the cursor over them. The rules themselves are the shown list's
//! (`Watchlist::rules`); the errors are the snapshot's `rule_errors`, by
//! index, so a rule the startup schema cannot read is listed with its
//! reason and can be removed.

use geode_core::watchlist::fold::RuleError;
use geode_core::watchlist::{Rule, Watchlist};

/// What the popup says of a rule over the whole dataset.
pub const WHOLE_DATASET: &str = "whole dataset";

/// One rule as the popup lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleRow {
    /// Its index in the list's rules; shown as `rule <index + 1>`.
    pub index: usize,
    pub dataset: String,
    /// `whole dataset`, `scope <name>`, or the expression as written.
    pub scope_text: String,
    /// Why the fold refused it, when it did.
    pub error: Option<String>,
}

impl RuleRow {
    /// The row's words: `rule <i> · <dataset> · <scope_text>`.
    pub fn text(&self) -> String {
        format!(
            "rule {} \u{b7} {} \u{b7} {}",
            self.index + 1,
            self.dataset,
            self.scope_text
        )
    }
}

/// The scope column of `rule`: a saved scope by name, else the expression
/// as written, else the whole dataset. A rule naming both (which the fold
/// refuses) shows its scope; the error line says the rest.
pub fn scope_text(rule: &Rule) -> String {
    match (&rule.scope, &rule.expression) {
        (Some(name), _) => format!("scope {name}"),
        (None, Some(expr)) => expr.clone(),
        (None, None) => WHOLE_DATASET.to_string(),
    }
}

/// One row per rule, in definition order, each with its error from
/// `errors` by index.
pub fn rule_rows(list: &Watchlist, errors: &[RuleError]) -> Vec<RuleRow> {
    list.rules
        .iter()
        .enumerate()
        .map(|(index, rule)| RuleRow {
            index,
            dataset: rule.dataset.clone(),
            scope_text: scope_text(rule),
            error: errors
                .iter()
                .find(|e| e.index == index)
                .map(|e| e.reason.clone()),
        })
        .collect()
}

/// The open popup: a cursor over the rows, clamped to them (`None` row
/// under it while there are none).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RulesPopup {
    pub cursor: usize,
}

impl RulesPopup {
    /// Open on `at`, clamped to `len` rows.
    pub fn open_at(at: usize, len: usize) -> Self {
        let mut p = RulesPopup { cursor: at };
        p.clamp(len);
        p
    }

    /// Keep the cursor on a row: the last one when the rows shrank under
    /// it, the first when there are none.
    pub fn clamp(&mut self, len: usize) {
        self.cursor = self.cursor.min(len.saturating_sub(1));
    }

    /// `j`/`k`: step `delta` rows, clamped at either end (a short list
    /// has no far side to wrap to that reads as a motion).
    pub fn step(&mut self, delta: i64, len: usize) {
        let next = (self.cursor as i64 + delta).max(0);
        self.cursor = usize::try_from(next).unwrap_or(0);
        self.clamp(len);
    }

    /// The row under the cursor, `None` with no rows.
    pub fn row<'a>(&self, rows: &'a [RuleRow]) -> Option<&'a RuleRow> {
        rows.get(self.cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(dataset: &str, scope: Option<&str>, expression: Option<&str>) -> Rule {
        Rule {
            dataset: dataset.into(),
            scope: scope.map(str::to_string),
            expression: expression.map(str::to_string),
        }
    }

    #[test]
    fn rows_spell_each_rules_scope_and_carry_its_error_by_index() {
        let list = Watchlist {
            rules: vec![
                rule("risk", None, None),
                rule("risk", Some("eu"), None),
                rule("cvi", None, Some("term > 1")),
                rule("gone", Some("eu"), Some("x = 1")),
            ],
            ..Watchlist::default()
        };
        let errors = vec![
            RuleError {
                index: 3,
                reason: "no dataset 'gone'".into(),
            },
            RuleError {
                index: 1,
                reason: "saved scope 'eu' is not defined".into(),
            },
        ];
        let rows = rule_rows(&list, &errors);
        let seen: Vec<(usize, &str, &str, Option<&str>)> = rows
            .iter()
            .map(|r| {
                (
                    r.index,
                    r.dataset.as_str(),
                    r.scope_text.as_str(),
                    r.error.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                (0, "risk", WHOLE_DATASET, None),
                (
                    1,
                    "risk",
                    "scope eu",
                    Some("saved scope 'eu' is not defined")
                ),
                (2, "cvi", "term > 1", None),
                (3, "gone", "scope eu", Some("no dataset 'gone'")),
            ]
        );
        assert_eq!(rows[2].text(), "rule 3 \u{b7} cvi \u{b7} term > 1");
        assert!(rule_rows(&Watchlist::default(), &errors).is_empty());
    }

    #[test]
    fn the_cursor_steps_and_clamps_to_the_rows() {
        let mut p = RulesPopup::open_at(5, 3);
        assert_eq!(p.cursor, 2, "opened past the end: the last row");
        p.step(-1, 3);
        assert_eq!(p.cursor, 1);
        p.step(-5, 3);
        assert_eq!(p.cursor, 0, "clamped, not wrapped");
        p.step(9, 3);
        assert_eq!(p.cursor, 2);
        // The rows shrank under it; then there are none.
        p.clamp(1);
        assert_eq!(p.cursor, 0);
        p.step(1, 0);
        assert_eq!(p.cursor, 0);
        assert_eq!(p.row(&[]), None);
        let rows = rule_rows(
            &Watchlist {
                rules: vec![rule("risk", None, None)],
                ..Watchlist::default()
            },
            &[],
        );
        assert_eq!(p.row(&rows).map(|r| r.index), Some(0));
    }
}
