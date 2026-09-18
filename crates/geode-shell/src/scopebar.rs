//! The scope bar's pure model (Phase 4 spec §3.1, §3.6, §4.4): what the
//! toolbar shows for the frame's active slot, scope, and as-of. Built
//! fresh by [`build_model`] and cached on `Frame::bar_model` (keyed on
//! `(Frame::versions(), today)` — Phase 4b M12 added the date half of
//! the key so the cache doesn't hold a stale `HH:MM` label past
//! midnight) — this module has no `gpui` dependency of its own;
//! `shell::toolbar` is the one place that paints it.

use crate::frame::Frame;
use chrono::{Local, NaiveDate};

/// One dimension's chip in the scope bar (Task 4 renders these
/// individually; Task 3 only needs the summary text, which the toolbar
/// joins the way `Frame::readout` used to).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub column: String,
    pub summary: String,
    /// The whole selection, un-elided — `"{column} ∈ {v1}, {v2}, …"`
    /// with every value — what a hover on the chip shows (Task 3).
    pub full: String,
}

/// What the scope bar shows for one frame state — everything already
/// formatted as display strings, so `shell::toolbar` does no formatting
/// of its own (spec §4.4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScopeBarModel {
    pub slot: Option<(u8, String)>,
    /// The slot readout's own display text (Phase 4b Task 1 fix round 1,
    /// MAJ-2): `"{n} · {label}"` when `slot` is `Some`, `"view default"`
    /// otherwise — built here so `shell::toolbar` (the render path)
    /// clones this rather than `format!`ing it fresh every paint.
    pub slot_label: String,
    pub chips: Vec<Chip>,
    pub text: Option<String>,
    /// The scope bar's text chip, in its own display text (Phase 4b
    /// Task 1 fix round 1, MAJ-2): `Some("text \"{t}\"")` when `text` is
    /// `Some`, built here for the same reason as `slot_label`.
    pub text_chip: Option<String>,
    /// Elided source text (≤ 40 chars + `…`), or `None` when the scope has
    /// no expression.
    pub expr: Option<String>,
    /// The whole expression source, un-elided — what a hover on the
    /// elided `expr` chip shows.
    pub expr_full: Option<String>,
    /// `Some("∅ {column}")` when the scope is a contradiction (spec
    /// §4.1's `Scope::impossible`) — named, not merely hidden, per
    /// `Scope::columns`'s own doc comment.
    pub impossible: Option<String>,
    /// `"14:05"` local time when the as-of date is today, otherwise
    /// `"2026-09-05 14:05"`; `None` when live. The status bar's own as-of
    /// segment reads this bare form directly (`shell/render.rs`).
    pub as_of: Option<String>,
    /// The toolbar's AS OF badge, in its own display text (Phase 4b
    /// Task 1 fix round 1, MAJ-2): `Some("AS OF {as_of}")`, built here
    /// for the same reason as `slot_label`/`text_chip` — distinct from
    /// `as_of` itself, which the status bar reads bare (no "AS OF "
    /// prefix).
    pub as_of_badge: Option<String>,
}

/// Build the scope bar model for `frame`, given today's local date (for
/// deciding whether an as-of falls on today) — passed in rather than
/// read from the clock, both so this stays a pure function callers can
/// test without a wall-clock dependency and so a caller with its own
/// cached `today` (Phase 4b Task 1 fix round 1, MIN-9 — `ShellView::
/// today`, refreshed once per reload-poll tick) never needs to read the
/// clock again just to call this.
pub fn build_model(frame: &Frame, today: NaiveDate) -> ScopeBarModel {
    let scope = frame.scope();
    let slot = frame
        .active_slot()
        .and_then(|n| frame.slots().label(n).map(|l| (n, l)));
    let chips = scope
        .dimensions
        .iter()
        .filter(|d| !d.values.is_empty())
        .map(|d| Chip {
            column: d.column.clone(),
            summary: if d.values.len() <= 2 {
                format!("{} ∈ {}", d.column, d.values.join(", "))
            } else {
                format!("{} ∈ {{{}}}", d.column, d.values.len())
            },
            full: format!("{} ∈ {}", d.column, d.values.join(", ")),
        })
        .collect();
    let expr_full = scope.expression.as_ref().map(|e| e.to_string());
    let expr = expr_full.as_ref().map(|s| {
        if s.chars().count() > 40 {
            format!("{}…", s.chars().take(40).collect::<String>())
        } else {
            s.clone()
        }
    });
    let impossible = scope.impossible.then(|| {
        let named = scope.columns().into_iter().next().unwrap_or_default();
        format!("∅ {named}")
    });
    let as_of = match frame.as_of() {
        geode_core::query::AsOf::Live => None,
        geode_core::query::AsOf::At(t) => {
            let local = t.with_timezone(&Local);
            Some(if local.date_naive() == today {
                local.format("%H:%M").to_string()
            } else {
                local.format("%Y-%m-%d %H:%M").to_string()
            })
        }
    };
    let slot_label = match &slot {
        Some((n, label)) => format!("{n} · {label}"),
        None => "view default".to_string(),
    };
    let text_chip = scope.text.as_ref().map(|t| format!("text \"{t}\""));
    let as_of_badge = as_of.as_ref().map(|t| format!("AS OF {t}"));
    ScopeBarModel {
        slot,
        slot_label,
        chips,
        text: scope.text.clone(),
        text_chip,
        expr,
        expr_full,
        impossible,
        as_of,
        as_of_badge,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Frame;
    use chrono::TimeZone;
    use geode_core::groupings::GroupingSlots;
    use geode_core::query::AsOf;
    use geode_core::scope::{DimensionSelection, Scope, parse_expr};
    use geode_core::scopes::SavedScopes;

    /// Task 3 (tooltips): a chip's hover wants the full selection, not
    /// the elided `summary` a trader sees on the bar itself.
    #[test]
    fn a_chip_carries_the_full_selection_beside_its_elided_summary() {
        let mut f = Frame::new(GroupingSlots::default(), SavedScopes::new(), None);
        f.set_scope(Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["A".into(), "B".into(), "C".into()],
            }],
            ..Scope::default()
        });
        let m = build_model(&f, chrono::Local::now().date_naive());
        assert_eq!(m.chips[0].summary, "book ∈ {3}");
        assert_eq!(m.chips[0].full, "book ∈ A, B, C");
    }

    /// Task 3 (tooltips): the expr chip elides past 40 chars, but a
    /// hover wants the whole expression source. `Expr::Display` fully
    /// parenthesises every `and`/`or` operand (round-trip grammar, not
    /// brevity — see its own doc comment) so it does not reproduce the
    /// typed text verbatim; comparing against `expr.to_string()` is the
    /// brief's own documented fallback for that case.
    #[test]
    fn expr_full_is_the_whole_expression_while_expr_is_elided() {
        let mut f = Frame::new(GroupingSlots::default(), SavedScopes::new(), None);
        let long = "npv > 1000000 and delta < -50000 and book = 'ABCDEFGH'";
        let expr = parse_expr(long).unwrap();
        f.set_scope(Scope {
            expression: Some(expr.clone()),
            ..Scope::default()
        });
        let m = build_model(&f, chrono::Local::now().date_naive());
        assert!(m.expr.as_deref().unwrap().ends_with('…'));
        assert_eq!(m.expr_full.as_deref(), Some(expr.to_string().as_str()));
    }

    /// Phase 4b Task 1 fix round 1, MAJ-2: `shell::toolbar` used to
    /// `format!` the text chip, the AS OF badge and the slot readout
    /// fresh every paint even though `build_model` already had
    /// everything each of those three needs — this pins the model
    /// itself carrying the finished strings, so the render path has
    /// nothing left to format.
    #[test]
    fn build_model_carries_the_toolbars_own_finished_strings() {
        let mut slots = GroupingSlots::default();
        slots.set(1, vec!["book".into(), "lhu".into()]);
        let mut f = Frame::new(slots, SavedScopes::new(), None);
        f.set_active_slot(Some(1));
        let s = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into(), "BK001".into()],
            }],
            text: Some("spx".into()),
            ..Scope::default()
        };
        f.set_scope(s);
        let at = Local.with_ymd_and_hms(2026, 9, 8, 14, 5, 0).unwrap();
        f.set_as_of(AsOf::At(at.with_timezone(&chrono::Utc)));
        let today = at.date_naive();

        let m = build_model(&f, today);
        assert_eq!(
            m.text_chip.as_deref(),
            Some("text \"spx\""),
            "the text chip's own display string"
        );
        assert_eq!(
            m.as_of_badge.as_deref(),
            Some("AS OF 14:05"),
            "the AS OF badge's own display string"
        );
        assert_eq!(
            m.slot_label, "1 · book / lhu",
            "the slot readout's own display string"
        );
    }

    #[test]
    fn no_active_slot_labels_as_view_default() {
        let f = Frame::new(GroupingSlots::default(), SavedScopes::new(), None);
        let m = build_model(&f, chrono::Local::now().date_naive());
        assert_eq!(m.slot, None);
        assert_eq!(m.slot_label, "view default");
        assert_eq!(m.text_chip, None);
        assert_eq!(m.as_of_badge, None);
    }
}
