//! Formatting model for the frame's scope, grouping slot, and as-of.
//! `Frame::bar_model` caches it by versions excluding flip, configured clock,
//! and local date. Formatting receives those inputs explicitly; toolbar
//! rendering clones the prepared strings instead of rebuilding them.

use crate::frame::Frame;
use chrono::NaiveDate;
use geode_core::clock::Clock;
use gpui::SharedString;

/// One dimension chip. Tooltip content and selectors use `SharedString`
/// so rendering can clone cached text without a fresh string allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub column: String,
    pub summary: String,
    /// The complete selection for the hover tooltip: `column ∈ v1, v2, …`.
    pub full: SharedString,
    /// The chip body's own already-prefixed tooltip selector
    /// (`"tip-scope-chip-{column}"`).
    pub tip_selector: SharedString,
    /// The close glyph's own already-prefixed tooltip selector
    /// (`"tip-scope-chip-close-{column}"`).
    pub close_selector: SharedString,
    /// The close glyph's tooltip title (`"Remove {column}"`).
    pub close_title: SharedString,
}

/// Preformatted scope-bar labels and tooltip content for one frame state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScopeBarModel {
    pub slot: Option<(u8, String)>,
    /// `n · label` for an active slot, otherwise `view default`.
    pub slot_label: String,
    pub chips: Vec<Chip>,
    /// The scope's text layer. The toolbar shows it in its text Input rather
    /// than a separate chip; this field retains it for model consumers.
    pub text: Option<String>,
    /// Elided source text (≤ 40 chars + `…`), or `None` when the scope has
    /// no expression.
    pub expr: Option<String>,
    /// Complete expression source for the tooltip, shared across renders.
    pub expr_full: Option<SharedString>,
    /// Contradiction marker, using the first name from `Scope::columns()`.
    /// That name need not identify the dimension that caused the contradiction.
    pub impossible: Option<String>,
    /// `"14:05"` local time when the as-of date is today, otherwise
    /// `"2026-09-05 14:05"`; `None` when live. The status bar's own as-of
    /// segment reads this bare form directly (`shell/render.rs`).
    pub as_of: Option<String>,
    /// `AS OF {as_of}` for the toolbar badge and tooltip detail.
    pub as_of_badge: Option<SharedString>,
    /// Timestamp including seconds and zone on the configured clock
    /// (`Clock::full`). Toolbar and status-bar tooltips use this instead of
    /// the shorter label; fractional seconds are not displayed.
    pub as_of_full: Option<SharedString>,
    /// Whether the scope is nonempty and its save chip should be shown.
    pub savable: bool,
}

/// Build labels using the supplied clock and local date. Explicit inputs
/// keep formatting deterministic and avoid clock reads during render.
pub fn build_model(frame: &Frame, clock: Clock, today: NaiveDate) -> ScopeBarModel {
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
            full: format!("{} ∈ {}", d.column, d.values.join(", ")).into(),
            tip_selector: format!("tip-scope-chip-{}", d.column).into(),
            close_selector: format!("tip-scope-chip-close-{}", d.column).into(),
            close_title: format!("Remove {}", d.column).into(),
        })
        .collect();
    let expr_full: Option<SharedString> = scope.expression.as_ref().map(|e| e.to_string().into());
    let expr = expr_full.as_ref().map(|s| {
        if s.chars().count() > 40 {
            format!("{}…", s.chars().take(40).collect::<String>())
        } else {
            s.to_string()
        }
    });
    let impossible = scope.impossible.then(|| {
        let named = scope.columns().into_iter().next().unwrap_or_default();
        format!("∅ {named}")
    });
    let (as_of, as_of_full) = match frame.as_of() {
        geode_core::query::AsOf::Live => (None, None),
        geode_core::query::AsOf::At(t) => {
            let local = clock.local(*t);
            let elided = if local.date_naive() == today {
                local.format("%H:%M").to_string()
            } else {
                local.format("%Y-%m-%d %H:%M").to_string()
            };
            let full: SharedString = clock.full(*t).into();
            (Some(elided), Some(full))
        }
    };
    let slot_label = match &slot {
        Some((n, label)) => format!("{n} · {label}"),
        None => "view default".to_string(),
    };
    let as_of_badge: Option<SharedString> = as_of.as_ref().map(|t| format!("AS OF {t}").into());
    let savable = !scope.is_empty();
    ScopeBarModel {
        slot,
        slot_label,
        chips,
        text: scope.text.clone(),
        expr,
        expr_full,
        impossible,
        as_of,
        as_of_badge,
        as_of_full,
        savable,
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

    /// A chip tooltip retains every value even when its summary uses a count.
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
        let clock = geode_core::clock::Clock::utc();
        let m = build_model(&f, clock, clock.today(chrono::Utc::now()));
        assert_eq!(m.chips[0].summary, "book ∈ {3}");
        assert_eq!(m.chips[0].full, "book ∈ A, B, C");
    }

    /// The expression label elides after 40 characters; its tooltip keeps the
    /// complete canonical Display form, including explicit parentheses.
    #[test]
    fn expr_full_is_the_whole_expression_while_expr_is_elided() {
        let mut f = Frame::new(GroupingSlots::default(), SavedScopes::new(), None);
        let long = "npv > 1000000 and delta < -50000 and book = 'ABCDEFGH'";
        let expr = parse_expr(long).unwrap();
        f.set_scope(Scope {
            expression: Some(expr.clone()),
            ..Scope::default()
        });
        let clock = geode_core::clock::Clock::utc();
        let m = build_model(&f, clock, clock.today(chrono::Utc::now()));
        assert!(m.expr.as_deref().unwrap().ends_with('…'));
        assert_eq!(m.expr_full.as_deref(), Some(expr.to_string().as_str()));
    }

    /// The model carries finished slot and as-of strings for rendering.
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
        let clock = geode_core::clock::Clock::utc();
        let at = chrono::Utc.with_ymd_and_hms(2026, 9, 8, 14, 5, 0).unwrap();
        f.set_as_of(AsOf::At(at));
        let today = clock.today(at);

        let m = build_model(&f, clock, today);
        assert_eq!(
            m.text.as_deref(),
            Some("spx"),
            "the text layer rides on the model bare; the field shows it (no text chip since 2026-09-19)"
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
        assert!(
            m.savable,
            "a non-empty scope is savable — the toolbar's save chip paints"
        );
    }

    /// As-of tooltips retain date and seconds that the compact label omits.
    #[test]
    fn as_of_full_is_the_unelided_local_timestamp_while_as_of_elides_it() {
        let mut f = Frame::new(GroupingSlots::default(), SavedScopes::new(), None);
        let clock = geode_core::clock::Clock::utc();
        let at = chrono::Utc.with_ymd_and_hms(2026, 9, 8, 14, 5, 30).unwrap();
        f.set_as_of(AsOf::At(at));
        let today = clock.today(at);

        let m = build_model(&f, clock, today);
        assert_eq!(m.as_of.as_deref(), Some("14:05"), "the elided badge form");
        assert_eq!(
            m.as_of_full.as_deref(),
            Some("2026-09-08 14:05:30 UTC"),
            "the full resolved timestamp, seconds included"
        );
    }

    #[test]
    fn no_active_slot_labels_as_view_default() {
        let f = Frame::new(GroupingSlots::default(), SavedScopes::new(), None);
        let clock = geode_core::clock::Clock::utc();
        let m = build_model(&f, clock, clock.today(chrono::Utc::now()));
        assert_eq!(m.slot, None);
        assert_eq!(m.slot_label, "view default");
        assert_eq!(m.text, None);
        assert_eq!(m.as_of_badge, None);
        assert_eq!(m.as_of_full, None);
        assert!(!m.savable, "an empty scope has nothing to save");
    }

    /// As-of labels use the configured clock, including non-UTC zones.
    #[test]
    fn the_as_of_chip_reads_on_the_clock_not_utc() {
        use chrono::TimeZone;
        let mut f = Frame::new(GroupingSlots::default(), SavedScopes::new(), None);
        let t = chrono::Utc.with_ymd_and_hms(2026, 9, 18, 22, 0, 0).unwrap();
        f.set_as_of(geode_core::query::AsOf::At(t));
        let utc = geode_core::clock::Clock::utc();
        let m = build_model(&f, utc, utc.today(t));
        assert_eq!(
            m.as_of.as_deref(),
            Some("22:00"),
            "today on the clock: HH:MM alone"
        );
        let m = build_model(&f, utc, utc.today(t).succ_opt().unwrap());
        assert_eq!(
            m.as_of.as_deref(),
            Some("2026-09-18 22:00"),
            "another day: dated"
        );
        assert_eq!(m.as_of_full.as_deref(), Some("2026-09-18 22:00:00 UTC"));

        // UTC alone cannot see a zone — the same instant on a real
        // non-UTC clock (Tokyo, UTC+9) must read a different wall-clock
        // time, by hand here, not through `clock.local` (which would
        // just prove the arithmetic agrees with itself).
        let tokyo = geode_core::clock::Clock::in_zone_named("Asia/Tokyo");
        let m = build_model(&f, tokyo, tokyo.today(t));
        assert_eq!(
            m.as_of.as_deref(),
            Some("07:00"),
            "22:00 UTC is 07:00 the next day in Tokyo — today on the clock"
        );
        let m = build_model(&f, tokyo, tokyo.today(t).succ_opt().unwrap());
        assert_eq!(
            m.as_of.as_deref(),
            Some("2026-09-19 07:00"),
            "another day on the clock: dated"
        );
        assert_eq!(m.as_of_full.as_deref(), Some("2026-09-19 07:00:00 JST"));
    }
}
