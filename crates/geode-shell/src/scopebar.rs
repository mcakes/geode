//! Formatting model for the frame's scope, grouping slot, and as-of.
//! `Frame::bar_model` caches it by versions excluding flip, configured clock,
//! and local date. Formatting receives those inputs explicitly; toolbar
//! rendering clones the prepared strings instead of rebuilding them.

use crate::frame::Frame;
use chrono::NaiveDate;
use geode_core::clock::Clock;
use geode_core::named::{NamedExpr, NamedExpressions};
use geode_core::scope::Scope;
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

/// One expression-term chip: the term's canonical source, elided for the
/// chip and whole for the tooltip, plus the per-index selectors the chip
/// and its `×` paint with. Indexed by position (`Expr::conjuncts` order),
/// which is also what `Frame::drop_expression_term` and the dialog's
/// term mode address — the index IS the term's identity within one scope
/// version, and the model is rebuilt whenever the scope changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprTerm {
    /// Elided source text (≤ 40 chars + `…`).
    pub label: SharedString,
    /// Complete term source for the tooltip.
    pub full: SharedString,
    /// The chip body's debug selector (`"scope-expr-chip-{i}"`).
    pub selector: SharedString,
    /// The chip body's tooltip selector (`"tip-scope-expr-chip-{i}"`).
    pub tip_selector: SharedString,
    /// The `×`'s debug selector (`"scope-expr-chip-close-{i}"`).
    pub close_selector: SharedString,
    /// The `×`'s tooltip selector (`"tip-scope-expr-chip-close-{i}"`).
    pub close_tip_selector: SharedString,
}

/// One named-expression chip: `≡ name`, or `≡ name · missing` /
/// `≡ name · invalid` when the frame's named expressions cannot resolve
/// it. Keyed by the name, which is unique within `Scope.named`, so the
/// chip's element ids and selectors survive reordering and removal of
/// its neighbours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedChip {
    pub name: SharedString,
    /// `≡ name`, with ` · missing` or ` · invalid` when broken.
    pub label: SharedString,
    /// Tooltip title: the expression text, or why the name cannot resolve.
    pub full: SharedString,
    /// Missing or invalid: painted in the danger tone. A tile scoped by it
    /// refuses to query, so the chip must read as an error, not routine state.
    pub broken: bool,
    /// The chip body's element id and debug selector (`"scope-named-chip-{name}"`).
    pub selector: SharedString,
    /// The chip body's tooltip selector (`"tip-scope-named-chip-{name}"`).
    pub tip_selector: SharedString,
    /// The `×`'s element id and debug selector (`"scope-named-chip-close-{name}"`).
    pub close_selector: SharedString,
    /// The `×`'s tooltip selector (`"tip-scope-named-chip-close-{name}"`).
    pub close_tip_selector: SharedString,
    /// The `×`'s tooltip title (`"Remove {name}"`).
    pub close_title: SharedString,
}

/// Elide to 40 characters plus `…`, the scope bar's one rule for
/// expression text.
pub(crate) fn elide(s: &str) -> String {
    if s.chars().count() > 40 {
        format!("{}…", s.chars().take(40).collect::<String>())
    } else {
        s.to_string()
    }
}

/// Preformatted scope-bar labels and tooltip content for one frame state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScopeBarModel {
    pub slot: Option<(u8, String)>,
    /// `n · label` for an active slot, otherwise `view default`.
    pub slot_label: String,
    pub chips: Vec<Chip>,
    /// One chip per `Scope.named` entry, in list order; the toolbar paints
    /// them after the dimension chips and before the expression terms.
    pub named: Vec<NamedChip>,
    /// The scope's text layer. The toolbar shows it in its text Input rather
    /// than a separate chip; this field retains it for model consumers.
    pub text: Option<String>,
    /// One chip per top-level `and` term of the scope's expression
    /// (`Expr::conjuncts`), in order; empty when there is no expression.
    pub terms: Vec<ExprTerm>,
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

/// The chip for one name in `Scope.named`. A broken name's tooltip is
/// `Scope::resolve`'s own refusal for it, the same words the tile that
/// refuses to query shows.
fn named_chip(name: &str, defined: &NamedExpressions) -> NamedChip {
    let (label, full, broken) = match defined.get(name) {
        Some(NamedExpr::Valid { text, .. }) => (format!("≡ {name}"), text.clone(), false),
        found => {
            let state = if found.is_none() {
                "missing"
            } else {
                "invalid"
            };
            let reason = Scope {
                named: vec![name.to_string()],
                ..Scope::default()
            }
            .resolve(defined)
            .err()
            .unwrap_or_default();
            (format!("≡ {name} · {state}"), reason, true)
        }
    };
    NamedChip {
        name: name.to_string().into(),
        label: label.into(),
        full: full.into(),
        broken,
        selector: format!("scope-named-chip-{name}").into(),
        tip_selector: format!("tip-scope-named-chip-{name}").into(),
        close_selector: format!("scope-named-chip-close-{name}").into(),
        close_tip_selector: format!("tip-scope-named-chip-close-{name}").into(),
        close_title: format!("Remove {name}").into(),
    }
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
    let named = scope
        .named
        .iter()
        .map(|name| named_chip(name, frame.named_expressions()))
        .collect();
    let terms = scope
        .expression
        .as_ref()
        .map(|e| {
            e.conjuncts()
                .into_iter()
                .enumerate()
                .map(|(i, term)| {
                    let full = term.to_string();
                    ExprTerm {
                        label: elide(&full).into(),
                        full: full.into(),
                        selector: format!("scope-expr-chip-{i}").into(),
                        tip_selector: format!("tip-scope-expr-chip-{i}").into(),
                        close_selector: format!("scope-expr-chip-close-{i}").into(),
                        close_tip_selector: format!("tip-scope-expr-chip-close-{i}").into(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
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
        named,
        text: scope.text.clone(),
        terms,
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

    /// One chip per top-level `and` term, in order: each label elides
    /// after 40 characters while its tooltip keeps the whole canonical
    /// term, and each carries its own per-index selectors.
    #[test]
    fn each_and_term_is_its_own_chip_elided_with_its_full_text() {
        let mut f = Frame::new(GroupingSlots::default(), SavedScopes::new(), None);
        let long = "npv > 1000000 and (delta < -50000 or book = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ')";
        f.set_scope(Scope {
            expression: Some(parse_expr(long).unwrap()),
            ..Scope::default()
        });
        let clock = geode_core::clock::Clock::utc();
        let m = build_model(&f, clock, clock.today(chrono::Utc::now()));
        assert_eq!(m.terms.len(), 2, "an or inside an and is one term");
        assert_eq!(m.terms[0].label, "npv > 1000000");
        assert_eq!(m.terms[0].full, "npv > 1000000");
        let or = "(delta < -50000) or (book = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ')";
        assert_eq!(m.terms[1].full, or);
        assert!(m.terms[1].label.ends_with('…'));
        assert_eq!(m.terms[1].label.chars().count(), 41);
        assert_eq!(m.terms[1].selector, "scope-expr-chip-1");
        assert_eq!(m.terms[1].tip_selector, "tip-scope-expr-chip-1");
        assert_eq!(m.terms[1].close_selector, "scope-expr-chip-close-1");
        assert_eq!(m.terms[1].close_tip_selector, "tip-scope-expr-chip-close-1");
    }

    /// Named expressions defined by `text` (an `expressions` doc body);
    /// invalid entries are kept, so their diagnostics are not asserted.
    fn named_exprs(text: &str) -> geode_core::named::NamedExpressions {
        use geode_core::config::{EXPRESSIONS_DOC, LayerDoc, merge_docs};
        let doc = LayerDoc::builtin(EXPRESSIONS_DOC, text).unwrap();
        let merged = merge_docs(EXPRESSIONS_DOC, &[doc]);
        geode_core::named::NamedExpressions::from_doc(
            &merged,
            &geode_core::scope::complete::ExprVocab::default(),
        )
        .0
    }

    /// One chip per name in `Scope.named`, in list order, between the
    /// dimension chips and the expression terms: a defined name reads
    /// `≡ name` with its expression text as the tooltip; a missing or
    /// invalid one is flagged broken and says why. Names alone make the
    /// scope savable.
    #[test]
    fn each_named_expression_is_a_chip_and_a_broken_one_is_flagged() {
        let mut f = Frame::new(GroupingSlots::default(), SavedScopes::new(), None);
        f.replace_named_expressions(named_exprs(
            "[liq]\nexpression = \"npv > 0\"\n[bad]\nexpression = \"npv >\"\n",
        ));
        f.set_scope(Scope {
            named: vec!["liq".into(), "gone".into(), "bad".into()],
            ..Scope::default()
        });
        let clock = geode_core::clock::Clock::utc();
        let m = build_model(&f, clock, clock.today(chrono::Utc::now()));
        let labels: Vec<&str> = m.named.iter().map(|c| c.label.as_ref()).collect();
        assert_eq!(
            labels,
            ["≡ liq", "≡ gone · missing", "≡ bad · invalid"],
            "one chip per name, in list order"
        );
        assert!(!m.named[0].broken);
        assert!(m.named[1].broken && m.named[2].broken);
        assert_eq!(m.named[0].full, "npv > 0", "tooltip: the expression text");
        assert_eq!(m.named[1].full, "named expression 'gone' is missing");
        assert!(
            m.named[2]
                .full
                .starts_with("named expression 'bad' is invalid: "),
            "{}",
            m.named[2].full
        );
        assert_eq!(m.named[0].name, "liq");
        assert_eq!(m.named[0].selector, "scope-named-chip-liq");
        assert_eq!(m.named[0].close_selector, "scope-named-chip-close-liq");
        assert_eq!(m.named[0].tip_selector, "tip-scope-named-chip-liq");
        assert_eq!(
            m.named[0].close_tip_selector,
            "tip-scope-named-chip-close-liq"
        );
        assert_eq!(m.named[0].close_title, "Remove liq");
        assert!(m.savable, "a scope of names alone is not empty");
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
