//! The panel's header row (spec 2026-09-14 §4): prepared once per change
//! by [`HeaderModel::prepare`] — the tile's `changed()` door — and painted
//! by [`render`] with no formatting of its own.

use crate::core::draft::{DraftBadge, local_hhmm};
use crate::core::matrix::{HeaderCell, MatrixModel};
use crate::core::spec::PanelSpec;
use crate::delegate::{CellPaint, cell_paint};
use crate::tile::{FlooredTones, MarketDataTile, display_key};
use chrono::{DateTime, Utc};
use geode_shell::fonts;
use gpui::prelude::*;
use gpui::{Entity, SharedString, div, px};
use gpui_component::input::{Input, InputState};
use gpui_component::{Theme, h_flex};

/// What one prepared header run is painted as. The tone is resolved to a
/// theme colour at paint (never a stored colour, so a theme switch needs
/// no rebuild), and `Time` is the one tone whose colour depends on the
/// clock — the staleness reading, which is a comparison per frame and not
/// a format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    Plain,
    Key,
    Time,
    Warn,
    Error,
}

/// One run's text colour: the theme's own secondary/primary text for the
/// quiet tones, the floored `warning` for `Warn` (and for `Time` once the
/// document is stale), the floored `danger` for `Error`. Never
/// `warning_foreground` — that is the token for text on a SOLID warning
/// fill, and a run has no fill.
pub(crate) fn tone_colour(
    tone: Tone,
    stale: bool,
    theme: &Theme,
    floored: &FlooredTones,
) -> gpui::Hsla {
    match tone {
        Tone::Plain => theme.muted_foreground,
        Tone::Key => theme.foreground,
        Tone::Time if stale => floored.warn,
        Tone::Time => theme.muted_foreground,
        Tone::Warn => floored.warn,
        Tone::Error => floored.error,
    }
}

/// Everything [`HeaderModel::prepare`] needs, gathered so the tile's own
/// `rebuild_chrome` reads as one call rather than seven positional
/// arguments.
pub(crate) struct HeaderInputs<'a> {
    pub spec: &'a PanelSpec,
    pub key: Option<&'a [String]>,
    pub model: &'a MatrixModel,
    pub badge: DraftBadge,
    pub unresolved_restore: bool,
    pub notice: Option<&'a SharedString>,
    pub source_at: Option<DateTime<Utc>>,
}

/// The header row, prepared once per change: every string already
/// formatted, so `render` clones refcounts and copies a `Copy` flag.
#[derive(Debug, Clone)]
pub(crate) struct HeaderModel {
    /// `spec.title`, the kind badge.
    pub title: SharedString,
    /// `display_key(key)`, or `None` with no underlying loaded at all.
    pub underlying: Option<SharedString>,
    /// Whether the draft has any edit, cell or attribute — the dirty dot.
    pub dirty: bool,
    /// A clone of `model.header` — `Rc`-cheap `SharedString`s, prepared by
    /// [`crate::core::matrix::MatrixModel::build`] already.
    pub attrs: Vec<HeaderCell>,
    /// The one short state run (spec §4 item 5), if any.
    pub state: Option<(SharedString, Tone)>,
    pub notice: Option<SharedString>,
    /// The generation's source time, `HH:MM:SS` on the trader's own clock.
    pub time: Option<SharedString>,
    /// The tile's own clock reading, applied at paint (`render`'s job,
    /// never `prepare`'s — a staleness comparison is per frame, not per
    /// change).
    pub stale: bool,
}

impl HeaderModel {
    /// Build the header from the tile's current state. Called from
    /// `rebuild_chrome` — the one door every mutation on the tile ends
    /// at — so `render` never formats.
    pub(crate) fn prepare(i: HeaderInputs) -> HeaderModel {
        let underlying = i.key.map(|k| SharedString::from(display_key(k)));
        let (dirty, mut state) = match i.badge {
            DraftBadge::Clean => (false, None),
            DraftBadge::Dirty => (true, None),
            DraftBadge::Behind { newer } => (
                true,
                Some((format!("update {}", local_hhmm(&newer)).into(), Tone::Warn)),
            ),
            DraftBadge::Sent => (false, Some(("sent".into(), Tone::Time))),
        };
        if i.key.is_some() && i.model.rows.is_empty() {
            state = Some(if i.unresolved_restore && dirty {
                ("edits await a document".into(), Tone::Warn)
            } else {
                ("no document yet".into(), Tone::Warn)
            });
        }
        HeaderModel {
            title: i.spec.title.into(),
            underlying,
            dirty,
            attrs: i.model.header.clone(),
            state,
            notice: i.notice.cloned(),
            time: i.source_at.map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%H:%M:%S")
                    .to_string()
                    .into()
            }),
            stale: false,
        }
    }

    /// Every painted string, in order — the test door: a test asserts on
    /// what a trader reads, not on the fields behind it. Test-only: no
    /// production code reads a formatted-string form of the header, only
    /// its fields (`render`'s job).
    #[cfg(test)]
    pub(crate) fn texts(&self) -> Vec<String> {
        let mut out = vec![self.title.to_string()];
        match &self.underlying {
            Some(u) => out.push(u.to_string()),
            None => out.push("no underlying — load…".to_string()),
        }
        for a in &self.attrs {
            out.push(format!("{} {}", a.label, a.text));
        }
        if let Some((text, _)) = &self.state {
            out.push(text.to_string());
        }
        if let Some(n) = &self.notice {
            out.push(n.to_string());
        }
        if let Some(t) = &self.time {
            out.push(if self.stale {
                format!("{t} stale")
            } else {
                t.to_string()
            });
        }
        out
    }
}

/// Paint the header row (spec §4): kind badge, bold underlying, dirty
/// dot, inline attribute strip, spacer, state, notice, time, `⋯`.
///
/// `cursor_attr`/`editor` are `None` until Task 5 wires the strip's own
/// cursor and cell editor; `menu_open` is `false` until Task 6 wires the
/// action list `⋯` opens. `tile` is accepted, unused, for the mouse
/// handlers those tasks add.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render(
    h: &HeaderModel,
    cursor_attr: Option<usize>,
    editor: Option<(usize, &Entity<InputState>)>,
    menu_open: bool,
    theme: &Theme,
    tones: &FlooredTones,
    tile: &Entity<MarketDataTile>,
    tile_id: u64,
) -> impl IntoElement {
    let muted = theme.muted_foreground;
    let mut row = h_flex()
        .w_full()
        .h(px(22.))
        .items_center()
        .gap_3()
        .px_2()
        .text_sm()
        .text_color(muted)
        .border_b_1()
        .border_color(theme.border)
        .debug_selector(move || format!("marketdata-header-{tile_id}"));

    // 1. Kind badge.
    row = row.child(
        div()
            .px_1p5()
            .rounded_sm()
            .bg(theme.secondary)
            .text_color(theme.secondary_foreground)
            .text_xs()
            .child(h.title.clone()),
    );

    // 2. Underlying, bold, plus the dirty dot.
    match &h.underlying {
        Some(u) => {
            row = row.child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(tone_colour(Tone::Key, false, theme, tones))
                    .child(u.clone()),
            );
            if h.dirty {
                row = row.child(
                    div()
                        .size(px(8.))
                        .rounded_full()
                        .bg(tones.warn)
                        .debug_selector(move || format!("marketdata-dirty-{tile_id}")),
                );
            }
        }
        None => {
            row = row.child(div().text_color(muted).child("no underlying — load…"));
        }
    }

    // 3. Attribute strip, inline.
    for (i, attr) in h.attrs.iter().enumerate() {
        let CellPaint { fill, text } = cell_paint(theme, false, attr.edited);
        let at_cursor = cursor_attr == Some(i);
        let mut value = div()
            .px_1()
            .rounded_sm()
            .font_family(fonts::MONO)
            .text_color(text)
            .when_some(fill, |d, f| d.bg(f))
            .border_1()
            .border_color(if at_cursor {
                theme.table_active_border
            } else {
                gpui::transparent_black()
            })
            .debug_selector(move || format!("marketdata-attr-{tile_id}-{i}"));
        value = match editor {
            Some((e, state)) if e == i => {
                value.child(div().min_w(px(80.)).child(Input::new(state)))
            }
            _ => value.child(attr.text.clone()),
        };
        row = row.child(
            h_flex()
                .gap_1()
                .child(
                    div()
                        .text_color(tone_colour(Tone::Plain, false, theme, tones))
                        .child(attr.label.clone()),
                )
                .child(value),
        );
    }

    // 4. Flex spacer.
    row = row.child(div().flex_1());

    // 5. State, then notice.
    if let Some((text, tone)) = &h.state {
        row = row.child(
            div()
                .text_color(tone_colour(*tone, false, theme, tones))
                .child(text.clone()),
        );
    }
    if let Some(n) = &h.notice {
        row = row.child(
            div()
                .text_color(tone_colour(Tone::Error, false, theme, tones))
                .child(n.clone()),
        );
    }

    // 6. Time, with the stale marker.
    if let Some(t) = &h.time {
        let colour = if h.stale { tones.warn } else { muted };
        row = row.child(
            h_flex()
                .gap_1()
                .text_color(colour)
                .child(t.clone())
                .when(h.stale, |d| d.child("stale")),
        );
    }

    // 7. `⋯` — Task 6 wires the click; painted now so the row's shape is
    // final.
    row = row.child(
        div()
            .px_1p5()
            .rounded_sm()
            .border_1()
            .border_color(theme.border)
            .when(menu_open, |d| d.bg(theme.secondary))
            .text_color(muted)
            .child("⋯")
            .debug_selector(move || format!("marketdata-menu-button-{tile_id}")),
    );
    let _ = tile;
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::CVI;

    fn model_with_header(entries: &[(&str, &str, &str)]) -> MatrixModel {
        MatrixModel {
            header: entries
                .iter()
                .map(|(column, label, text)| HeaderCell {
                    column: (*column).into(),
                    label: (*label).into(),
                    text: (*text).into(),
                    edited: false,
                })
                .collect(),
            rows: vec![crate::core::matrix::RowModel {
                label: "row".into(),
                cells: Vec::new(),
            }],
            ..MatrixModel::default()
        }
    }

    fn model_with_rows() -> MatrixModel {
        MatrixModel {
            rows: vec![crate::core::matrix::RowModel {
                label: "row".into(),
                cells: Vec::new(),
            }],
            ..MatrixModel::default()
        }
    }

    fn inputs<'a>(
        model: &'a MatrixModel,
        key: Option<&'a [String]>,
        badge: DraftBadge,
    ) -> HeaderInputs<'a> {
        HeaderInputs {
            spec: &CVI,
            key,
            model,
            badge,
            unresolved_restore: false,
            notice: None,
            source_at: None,
        }
    }

    #[test]
    fn a_clean_header_says_nothing_but_identity_attributes_and_time() {
        let model = model_with_header(&[
            ("anchor_date", "anchor", "2026-09-14"),
            ("spot_ref", "spot", "5000"),
        ]);
        let key = vec!["SPX.Z".to_string()];
        let h = HeaderModel::prepare(inputs(&model, Some(&key), DraftBadge::Clean));
        assert_eq!(
            h.texts(),
            vec!["CVI", "SPX.Z", "anchor 2026-09-14", "spot 5000"]
        );
        assert!(!h.dirty && h.state.is_none());
    }

    #[test]
    fn dirty_is_a_dot_and_behind_reads_update_hhmm() {
        let model = model_with_rows();
        let key = vec!["SPX.Z".to_string()];
        let dirty = HeaderModel::prepare(inputs(&model, Some(&key), DraftBadge::Dirty));
        assert!(dirty.dirty && dirty.state.is_none());
        let newer = chrono::Utc::now().to_rfc3339();
        let behind = HeaderModel::prepare(inputs(
            &model,
            Some(&key),
            DraftBadge::Behind {
                newer: newer.clone(),
            },
        ));
        let expected = format!(
            "update {}",
            chrono::DateTime::parse_from_rfc3339(&newer)
                .unwrap()
                .with_timezone(&chrono::Local)
                .format("%H:%M")
        );
        assert_eq!(
            behind
                .state
                .as_ref()
                .map(|(t, tone)| (t.to_string(), *tone)),
            Some((expected, Tone::Warn))
        );
    }

    #[test]
    fn the_no_underlying_no_document_and_parked_states() {
        let empty = MatrixModel::default();
        let none = HeaderModel::prepare(inputs(&empty, None, DraftBadge::Clean));
        assert_eq!(none.texts(), vec!["CVI", "no underlying — load…"]);
        let key = vec!["NKY.Z".to_string()];
        let waiting = HeaderModel::prepare(inputs(&empty, Some(&key), DraftBadge::Clean));
        assert_eq!(
            waiting.state.as_ref().map(|s| s.0.to_string()),
            Some("no document yet".into())
        );
        let mut parked = inputs(&empty, Some(&key), DraftBadge::Dirty);
        parked.unresolved_restore = true;
        let parked = HeaderModel::prepare(parked);
        assert_eq!(
            parked.state.as_ref().map(|s| s.0.to_string()),
            Some("edits await a document".into())
        );
        assert!(parked.dirty);
    }

    #[test]
    fn a_notice_is_kept_whole_after_the_state() {
        let model = model_with_rows();
        let key = vec!["SPX.Z".to_string()];
        let notice: SharedString = "'abc' is not a number".into();
        let mut i = inputs(
            &model,
            Some(&key),
            DraftBadge::Behind {
                newer: "2026-09-14T14:09:00Z".into(),
            },
        );
        i.notice = Some(&notice);
        let h = HeaderModel::prepare(i);
        let texts = h.texts();
        let state_at = texts.iter().position(|t| t.starts_with("update ")).unwrap();
        let notice_at = texts
            .iter()
            .position(|t| t == "'abc' is not a number")
            .unwrap();
        assert!(state_at < notice_at);
    }

    #[test]
    fn the_time_is_local_hhmmss_and_stale_is_a_flag() {
        let model = model_with_rows();
        let key = vec!["SPX.Z".to_string()];
        let at = chrono::Utc::now();
        let mut i = inputs(&model, Some(&key), DraftBadge::Clean);
        i.source_at = Some(at);
        let h = HeaderModel::prepare(i);
        assert_eq!(
            h.time.as_deref(),
            Some(
                at.with_timezone(&chrono::Local)
                    .format("%H:%M:%S")
                    .to_string()
                    .as_str()
            )
        );
        assert!(
            !h.stale,
            "staleness is the tile's clock reading, applied at paint"
        );
    }
}
