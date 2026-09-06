//! The scope bar's pure model (Phase 4 spec §3.1, §3.6, §4.4): what the
//! toolbar shows for the frame's active slot, scope, and as-of. Built
//! fresh by [`build_model`] and cached on `Frame::bar_model` (keyed on
//! `Frame::versions()`) — this module has no `gpui` dependency of its
//! own; `shell::toolbar` is the one place that paints it.

use crate::frame::Frame;
use chrono::{DateTime, Local};

/// One dimension's chip in the scope bar (Task 4 renders these
/// individually; Task 3 only needs the summary text, which the toolbar
/// joins the way `Frame::readout` used to).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub column: String,
    pub summary: String,
}

/// What the scope bar shows for one frame state — everything already
/// formatted as display strings, so `shell::toolbar` does no formatting
/// of its own (spec §4.4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScopeBarModel {
    pub slot: Option<(u8, String)>,
    pub chips: Vec<Chip>,
    pub text: Option<String>,
    /// Elided source text (≤ 40 chars + `…`), or `None` when the scope has
    /// no expression.
    pub expr: Option<String>,
    /// `Some("∅ {column}")` when the scope is a contradiction (spec
    /// §4.1's `Scope::impossible`) — named, not merely hidden, per
    /// `Scope::columns`'s own doc comment.
    pub impossible: Option<String>,
    /// `"14:05"` local time when the as-of date is today, otherwise
    /// `"2026-09-05 14:05"`; `None` when live.
    pub as_of: Option<String>,
}

/// Build the scope bar model for `frame` as of `now` (local time, for
/// deciding whether an as-of falls on today — passed in rather than read
/// from the clock so this stays a pure function callers can test without
/// a wall-clock dependency).
pub fn build_model(frame: &Frame, now: DateTime<Local>) -> ScopeBarModel {
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
        })
        .collect();
    let expr = scope.expression.as_ref().map(|e| {
        let s = e.to_string();
        if s.chars().count() > 40 {
            format!("{}…", s.chars().take(40).collect::<String>())
        } else {
            s
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
            Some(if local.date_naive() == now.date_naive() {
                local.format("%H:%M").to_string()
            } else {
                local.format("%Y-%m-%d %H:%M").to_string()
            })
        }
    };
    ScopeBarModel {
        slot,
        chips,
        text: scope.text.clone(),
        expr,
        impossible,
        as_of,
    }
}
