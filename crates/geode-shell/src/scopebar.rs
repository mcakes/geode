//! The scope bar's pure model (Phase 4 spec §3.1, §3.6, §4.4): what the
//! toolbar shows for the frame's active slot, scope, and as-of. Built
//! fresh by [`build_model`] and cached on `Frame::bar_model` (keyed on
//! `(Frame::versions(), clock, today)` — Phase 4b M12 added the date
//! half of the key so the cache doesn't hold a stale `HH:MM` label past
//! midnight, and as-of dialog spec §6.1 added the clock half so a
//! `[time]` reload can't hand a trader a stale-zone label either) — this
//! module has no `gpui` dependency of its own; `shell::toolbar` is the
//! one place that paints it.

use crate::frame::Frame;
use chrono::NaiveDate;
use geode_core::clock::Clock;
use gpui::SharedString;

/// One dimension's chip in the scope bar (Task 4 renders these
/// individually; Task 3 only needs the summary text, which the toolbar
/// joins the way `Frame::readout` used to).
///
/// `full`/`tip_selector`/`close_selector`/`close_title` are
/// `SharedString`, not `String` (fix round 1, Task 3 review): every one
/// of them feeds a `.tooltip(..)` attached inline in `shell::toolbar`'s
/// render path, so a `String` field would mean a fresh heap clone (or,
/// for `tip_selector`/`close_selector`, a `format!`) on every paint of
/// every chip. `build_model` already runs once per frame-version
/// change, not per render (see the module doc), so building these once
/// here and cloning a `SharedString` at the paint site (a refcount
/// bump, or a stack copy for anything under `SmolStr`'s 23-byte inline
/// cap) is the whole fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub column: String,
    pub summary: String,
    /// The whole selection, un-elided — `"{column} ∈ {v1}, {v2}, …"`
    /// with every value — what a hover on the chip shows (Task 3).
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
    /// The scope's text layer, bare. The toolbar paints no chip for it
    /// (toolbar restyle 2026-09-19): the field itself shows the frame's
    /// text while unfocused (`ShellView::on_frame_changed`) and clears it
    /// through its own clear glyph, so a `text "…"` chip only repeated
    /// what sat a few hundred pixels to its right. No painter reads this
    /// field; it is the model's truth, pinned by the model tests.
    pub text: Option<String>,
    /// Elided source text (≤ 40 chars + `…`), or `None` when the scope has
    /// no expression.
    pub expr: Option<String>,
    /// The whole expression source, un-elided — what a hover on the
    /// elided `expr` chip shows. `SharedString` (fix round 1) for the
    /// same reason as `Chip`'s tooltip fields.
    pub expr_full: Option<SharedString>,
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
    /// for the same reason as `slot_label` — distinct from
    /// `as_of` itself, which the status bar reads bare (no "AS OF "
    /// prefix). `SharedString` (fix round 1, Task 3 review): this used to
    /// double as the badge's tooltip TITLE too; since the final review
    /// (spec §5.1) the tooltip title is [`as_of_full`](Self::as_of_full)
    /// and this string moved to the tooltip's detail line instead — a
    /// `SharedString` here still means a refcount bump wherever it feeds
    /// either the painted label or that detail line, never a heap clone.
    pub as_of_badge: Option<SharedString>,
    /// The as-of instant's FULL resolved timestamp, `%Y-%m-%d %H:%M:%S
    /// %Z` (`Clock::full`) on the trader's configured clock, whatever
    /// `as_of`'s own elision does —
    /// `as_of` drops the date on today and always drops seconds, which is
    /// fine for a glance at the badge but not for a hover that exists to
    /// answer "exactly when". Built alongside `as_of` from the same
    /// clock conversion (final review, spec §5.1): the
    /// toolbar's AS OF badge and the status bar's as-of segment both use
    /// this as their tooltip TITLE, with the elided `as_of_badge`/segment
    /// text moved to the tooltip's detail line instead.
    pub as_of_full: Option<SharedString>,
    /// Whether the frame's scope has anything worth saving — `!scope.
    /// is_empty()`. `shell::toolbar` reads this to decide whether the
    /// scope bar's `save` chip paints at all (scope-save spec's
    /// amendment): a `save` chip over an empty scope would either do
    /// nothing (the same "verb that visibly does nothing" defect the
    /// dialog's own `run_overwrite` refuses to ship) or paint the
    /// notice on every click, so the chip is withdrawn instead of
    /// disabled. The `+` pick chip carries no such gate — it is always
    /// useful, empty scope or not.
    pub savable: bool,
}

/// Build the scope bar model for `frame` on `clock` (as-of dialog spec
/// §6.1), given today's date on that clock (for deciding whether an
/// as-of falls on today) — both passed in rather than read from
/// `AppClock` directly, so this stays a pure function callers can test
/// without a `gpui` dependency, and so a caller with its own cached
/// `today` (Phase 4b Task 1 fix round 1, MIN-9 — `ShellView::today`,
/// refreshed once per reload-poll tick) never needs to read the clock
/// again just to call this.
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
        let clock = geode_core::clock::Clock::utc();
        let m = build_model(&f, clock, clock.today(chrono::Utc::now()));
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
        let clock = geode_core::clock::Clock::utc();
        let m = build_model(&f, clock, clock.today(chrono::Utc::now()));
        assert!(m.expr.as_deref().unwrap().ends_with('…'));
        assert_eq!(m.expr_full.as_deref(), Some(expr.to_string().as_str()));
    }

    /// Phase 4b Task 1 fix round 1, MAJ-2: `shell::toolbar` used to
    /// `format!` the (since-retired) text chip, the AS OF badge and the slot readout
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

    /// Final review, spec §5.1: the as-of tooltip must show the FULL
    /// resolved timestamp, not `as_of`'s own elided form (which drops the
    /// date on today and always drops seconds). Built alongside `as_of`
    /// from the same local-time conversion, so a seconds-precision
    /// instant does not silently round to the minute in the one place a
    /// trader hovers to see exactly when.
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

    /// As-of dialog spec §6.1: the chip reads on the CONFIGURED clock, not
    /// hard-coded UTC and not the machine's own zone — `build_model`
    /// takes the clock explicitly (same testability reason `today` is
    /// already a parameter) so this holds for any zone a trader configures.
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
