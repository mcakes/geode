//! Prepared panel header: identity, attributes, draft status, upload/echo
//! feedback, source time, and the action-menu control. HeaderModel::prepare formats
//! text on state changes; render uses the prepared strings and current theme.

use crate::core::SegmentPaint;
use crate::core::draft::{DraftBadge, local_hhmm};
use crate::core::matrix::{HeaderCell, MatrixModel, RowState};
use crate::core::spec::PanelSpec;
use crate::delegate::{CellPaint, cell_paint};
use crate::tile::{DateFieldPaint, EditorPaint, FlooredTones, MarketDataTile, display_key};
use chrono::{DateTime, Utc};
use geode_core::clock::Clock;
use geode_shell::fonts;
use geode_shell::module::StackHandle;
use geode_shell::shell::control::{self, PointerStates as _};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_shell::tips;
use gpui::prelude::*;
use gpui::{ElementId, Entity, FocusHandle, Hsla, SharedString, div};
use gpui_component::input::Input;
use gpui_component::{Theme, h_flex};

/// The header strip's height, in pixels at the design rem
/// (`geode_shell::shell::scale`) — the blotter's own header height, so
/// the two tiles' strips line up side by side; the tile anchors its
/// popup under it by the same constant.
pub(crate) const HEADER_HEIGHT: f32 = 22.0;

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

/// Date-segment colors: typing uses the accent pair; active selection uses
/// primary fill with contrast-adjusted primary text; inactive segments use plain
/// foreground. The bundled-theme test checks these against their painted ground.
pub(crate) fn date_segment_paint(
    theme: &Theme,
    tones: &FlooredTones,
    active: bool,
    typing: bool,
) -> CellPaint {
    if typing {
        CellPaint {
            fill: Some(theme.accent),
            text: theme.accent_foreground,
            strike: false,
        }
    } else if active {
        CellPaint {
            fill: Some(theme.primary),
            text: tones.primary_text,
            strike: false,
        }
    } else {
        CellPaint {
            fill: None,
            text: theme.foreground,
            strike: false,
        }
    }
}

/// Shared segmented-date painter for header attributes and grid cells.
/// The container tracks the editor's focus handle and routes handled keys before
/// the shell listener. Segment clicks are consumed by the shared painter to avoid
/// the parent cell/attribute click closing the editor; padding and separators
/// still allow parent click handling.
pub(crate) fn render_date_field(
    paint: &DateFieldPaint,
    focus: &FocusHandle,
    theme: &Theme,
    tones: &FlooredTones,
    tile: &Entity<MarketDataTile>,
    tile_id: u64,
) -> impl IntoElement {
    let separator: Hsla = theme.muted_foreground;
    let field = h_flex()
        .track_focus(focus)
        .items_center()
        .px_1()
        .rounded(theme.radius_tokens().sm)
        .border_1()
        .border_color(theme.table_active_border)
        .font_family(fonts::MONO)
        .debug_selector(move || format!("marketdata-date-{tile_id}"))
        .on_key_down({
            let tile = tile.clone();
            move |event: &gpui::KeyDownEvent, window, cx| {
                let handled = tile.update(cx, |t, cx| t.date_field_key(event, window, cx));
                if handled {
                    cx.stop_propagation();
                }
            }
        });
    let rest = date_segment_paint(theme, tones, false, false);
    let active = date_segment_paint(theme, tones, true, false);
    let typing = date_segment_paint(theme, tones, true, true);
    let segment_paint = SegmentPaint {
        rest_text: rest.text,
        rest_fill: rest.fill,
        active_text: active.text,
        // A missing fill here would mean `date_segment_paint(.., true, ..)`
        // stopped filling the active/typing state — a colour-derivation
        // bug, not a case worth panicking the render thread over: fall
        // back to the theme's own primary/accent fill so a future edit
        // degrades instead of crashing the window.
        active_fill: active.fill.unwrap_or(theme.primary),
        typing_text: typing.text,
        typing_fill: typing.fill.unwrap_or(theme.accent),
        separator,
        suffix: separator,
        radius: theme.radius_tokens().sm,
    };
    let tile = tile.clone();
    field.child(geode_widgets::datefield::paint(
        &paint.segments,
        None,
        segment_paint,
        paint.selector.clone(),
        move |segment, window, cx| {
            tile.update(cx, |t, cx| t.date_segment_clicked(segment, window, cx));
        },
    ))
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
    /// Last upload failure for the associated edit set. Cleared when those edits
    /// change or a subsequent upload is submitted.
    pub upload_error: Option<&'a SharedString>,
    /// Prepared upload-echo result and tone: confirmed timestamps or a mismatch
    /// warning. Transport success and publication confirmation are separate states.
    pub echo: Option<(&'a SharedString, Tone)>,
    /// The armed `:upload` confirm's question, `upload … to <target>?
    /// (y/n)`.
    pub prompt: Option<&'a SharedString>,
    pub source_at: Option<DateTime<Utc>>,
    /// Count of inserted rows with a required cell still empty.
    pub incomplete: usize,
    /// Tile-supplied clock used to format timestamps without reading GPUI globals.
    pub clock: Clock,
}

/// The header row, prepared once per change: every string already
/// formatted, so `render` clones refcounts and copies a `Copy` flag.
#[derive(Debug, Clone)]
pub(crate) struct HeaderModel {
    /// `spec.title`, the kind badge.
    pub title: SharedString,
    /// `display_key(key)`, or `None` with no underlying loaded at all.
    pub underlying: Option<SharedString>,
    /// Dirty indicator for Dirty and Behind badges. Sent retains draft edits but
    /// suppresses this indicator because those edits are no longer unsent work.
    pub dirty: bool,
    /// A clone of `model.header` — `Rc`-cheap `SharedString`s, prepared by
    /// [`crate::core::matrix::MatrixModel::build`] already.
    pub attrs: Vec<HeaderCell>,
    /// The draft's badge, carried alongside `state` so `render` can tell
    /// a `Behind` state run from the other `Tone::Warn` runs (`state`'s
    /// own tone doesn't distinguish "different document received" from
    /// "no document yet"/"edits await a document") without reparsing
    /// `state`'s text.
    pub badge: DraftBadge,
    /// Prepared primary state message, if any.
    pub state: Option<(SharedString, Tone)>,
    /// Nonzero incomplete-row count and warning tone, painted after state and
    /// before upload/notice feedback.
    pub incomplete: Option<(SharedString, Tone)>,
    pub notice: Option<SharedString>,
    /// `upload failed: <e>`, painted in the error tone ahead of the
    /// notice.
    pub upload_error: Option<SharedString>,
    /// The echo's line, painted after the state and the incomplete-rows
    /// chip, ahead of the upload error and the notice.
    pub echo: Option<(SharedString, Tone)>,
    /// The armed upload confirm's question, painted last before the time
    /// on the element that holds the keyboard while it is armed.
    pub prompt: Option<SharedString>,
    /// The generation's source time, `HH:MM:SS` on the trader's own clock.
    pub time: Option<SharedString>,
    /// The tile's own clock reading, applied at paint (`render`'s job,
    /// never `prepare`'s — a staleness comparison is per frame, not per
    /// change).
    pub stale: bool,
}

impl HeaderModel {
    /// Build formatted header text from supplied tile state. An empty model with
    /// a selected key overrides the draft state message with its document-wait state.
    /// Staleness starts false and is supplied separately at paint time.
    pub(crate) fn prepare(i: HeaderInputs) -> HeaderModel {
        let underlying = i.key.map(|k| SharedString::from(display_key(k)));
        let (dirty, mut state) = match &i.badge {
            DraftBadge::Clean => (false, None),
            DraftBadge::Dirty => (true, None),
            DraftBadge::Behind { newer } => (
                true,
                Some((
                    format!("update {}", local_hhmm(newer, i.clock)).into(),
                    Tone::Warn,
                )),
            ),
            DraftBadge::Sent { at } => (
                false,
                Some((
                    format!("sent {}", local_hhmm(at, i.clock)).into(),
                    Tone::Time,
                )),
            ),
        };
        if i.key.is_some() && i.model.rows.is_empty() {
            state = Some(if i.unresolved_restore && dirty {
                ("edits await a document".into(), Tone::Warn)
            } else {
                ("no document yet".into(), Tone::Warn)
            });
        }
        let incomplete = match i.incomplete {
            0 => None,
            1 => Some(("1 row incomplete".into(), Tone::Warn)),
            n => Some((format!("{n} rows incomplete").into(), Tone::Warn)),
        };
        HeaderModel {
            title: i.spec.title.into(),
            underlying,
            dirty,
            attrs: i.model.header.clone(),
            badge: i.badge,
            state,
            incomplete,
            notice: i.notice.cloned(),
            upload_error: i.upload_error.cloned(),
            echo: i.echo.map(|(text, tone)| (text.clone(), tone)),
            prompt: i.prompt.cloned(),
            time: i.source_at.map(|t| i.clock.hms(t).into()),
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
        if let Some((text, _)) = &self.incomplete {
            out.push(text.to_string());
        }
        if let Some((text, _)) = &self.echo {
            out.push(text.to_string());
        }
        if let Some(e) = &self.upload_error {
            out.push(e.to_string());
        }
        if let Some(n) = &self.notice {
            out.push(n.to_string());
        }
        if let Some(p) = &self.prompt {
            out.push(p.to_string());
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

/// Render prepared identity, attribute, status, upload, and time runs.
/// The attribute cursor and editor are passed separately from prepared values;
/// menu_open controls the action button's selected appearance. A pending upload
/// prompt carries its own focus handle and consumes its confirmation keys.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render(
    h: &HeaderModel,
    cursor_attr: Option<usize>,
    editor: Option<(usize, EditorPaint<'_>)>,
    confirm: Option<&FocusHandle>,
    menu_open: bool,
    theme: &Theme,
    tones: &FlooredTones,
    tile: &Entity<MarketDataTile>,
    tile_id: u64,
    menu_tip_selector: SharedString,
    state_tip_selector: SharedString,
    stack: Option<&StackHandle>,
) -> impl IntoElement {
    let muted = theme.muted_foreground;
    let mut row = h_flex()
        .w_full()
        .h(scale::design(HEADER_HEIGHT))
        .items_center()
        .gap_3()
        .px_2()
        .text_sm()
        .text_color(muted)
        .border_b_1()
        .border_color(theme.border)
        .debug_selector(move || format!("marketdata-header-{tile_id}"));

    // Keep the shared stack marker first in the header strip.
    row = row.children(stack.and_then(|s| s.marker(theme, TileId(tile_id))));

    // 1. Kind badge.
    row = row.child(
        div()
            .px_1p5()
            .rounded(theme.radius_tokens().sm)
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
                        .size(scale::design(8.))
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
        // An attribute is a document-level value: it belongs to no row,
        // so it paints as a document row's cell would.
        let CellPaint { fill, text, .. } =
            cell_paint(theme, false, attr.edited, RowState::Document);
        let at_cursor = cursor_attr == Some(i);
        let mut value = div()
            .px_1()
            .rounded(theme.radius_tokens().sm)
            .font_family(fonts::MONO)
            .text_color(text)
            .when_some(fill, |d, f| d.bg(f))
            .border_1()
            .border_color(if at_cursor {
                theme.table_active_border
            } else {
                gpui::transparent_black()
            })
            .debug_selector(move || format!("marketdata-attr-{tile_id}-{i}"))
            // Select the clicked attribute, closing an existing editor first; a double
            // click opens its editor. Leave propagation enabled so the shell can focus
            // this tile and reconcile focus without stealing an active insert field.
            .on_mouse_down(gpui::MouseButton::Left, {
                let tile = tile.clone();
                move |event: &gpui::MouseDownEvent, window, cx| {
                    tile.update(cx, |t, cx| t.attr_clicked(i, event.click_count, window, cx))
                }
            });
        value = match &editor {
            Some((e, EditorPaint::Text(state))) if *e == i => {
                value.child(div().min_w(scale::design(80.)).child(Input::new(state)))
            }
            Some((e, EditorPaint::Date { paint, focus })) if *e == i => {
                value.child(render_date_field(paint, focus, theme, tones, tile, tile_id))
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
                .id(ElementId::NamedInteger(
                    SharedString::new_static("marketdata-state"),
                    tile_id,
                ))
                .debug_selector(move || format!("marketdata-state-{tile_id}"))
                .text_color(tone_colour(*tone, false, theme, tones))
                .child(text.clone())
                .when(matches!(h.badge, DraftBadge::Behind { .. }), |el| {
                    el.tooltip(tips::tip_with(
                        state_tip_selector,
                        text.clone(),
                        None,
                        Some(SharedString::new_static(
                            ":rebase adopts it · :revert drops your edits",
                        )),
                    ))
                }),
        );
    }
    // Incomplete inserted rows show a warning because upload will reject them.
    if let Some((text, tone)) = &h.incomplete {
        row = row.child(
            div()
                .debug_selector(move || format!("marketdata-incomplete-{tile_id}"))
                .text_color(tone_colour(*tone, false, theme, tones))
                .child(text.clone()),
        );
    }
    if let Some((text, tone)) = &h.echo {
        row = row.child(
            div()
                .debug_selector(move || format!("marketdata-echo-{tile_id}"))
                .text_color(tone_colour(*tone, false, theme, tones))
                .child(text.clone()),
        );
    }
    if let Some(e) = &h.upload_error {
        row = row.child(
            div()
                .debug_selector(move || format!("marketdata-upload-error-{tile_id}"))
                .text_color(tone_colour(Tone::Error, false, theme, tones))
                .child(e.clone()),
        );
    }
    if let Some(n) = &h.notice {
        row = row.child(
            div()
                .text_color(tone_colour(Tone::Error, false, theme, tones))
                .child(n.clone()),
        );
    }
    // The focused upload prompt consumes its keys before shell routing. Bare y
    // submits; any other key cancels. Its text uses the ordinary foreground tone.
    if let (Some(p), Some(focus)) = (&h.prompt, confirm) {
        let tile = tile.clone();
        row = row.child(
            div()
                .track_focus(focus)
                .debug_selector(move || format!("marketdata-upload-confirm-{tile_id}"))
                .text_color(tone_colour(Tone::Key, false, theme, tones))
                .child(p.clone())
                .on_key_down(move |event: &gpui::KeyDownEvent, window, cx| {
                    if tile.update(cx, |t, cx| t.confirm_key(event, window, cx)) {
                        cx.stop_propagation();
                    }
                }),
        );
    }

    // 6. Time, with the stale marker — `tone_colour` decides the colour,
    // as it does for every other run here, so the stale rule is spelled
    // once.
    if let Some(t) = &h.time {
        row = row.child(
            h_flex()
                .gap_1()
                .text_color(tone_colour(Tone::Time, h.stale, theme, tones))
                .child(t.clone())
                .when(h.stale, |d| d.child("stale")),
        );
    }

    // Toggle the action menu in capture phase, before its outside-press
    // listener can close it. A bubble-phase toggle would see an already-closed
    // popup and reopen it on the second click. Keep propagation enabled so the
    // shell still focuses the tile and runs its normal pointer handling.
    row = row.child(
        div()
            .id(ElementId::NamedInteger(
                SharedString::new_static("marketdata-menu-button"),
                tile_id,
            ))
            .px_1p5()
            .rounded(theme.radius_tokens().sm)
            .border_1()
            .border_color(theme.border)
            .when(menu_open, |d| d.bg(theme.secondary))
            .text_color(muted)
            // Closed uses bare-control pointer feedback; open retains its selected fill.
            // Colors are derived against the header's tile background.
            .when(!menu_open, |d| {
                d.pointer_states(control::paint(
                    theme,
                    control::Rest::Bare,
                    theme.background,
                    muted,
                ))
            })
            .child("⋯")
            .debug_selector(move || format!("marketdata-menu-button-{tile_id}"))
            .capture_any_mouse_down({
                let tile = tile.clone();
                move |event, window, cx| {
                    if event.button != gpui::MouseButton::Left {
                        return;
                    }
                    tile.update(cx, |t, cx| t.toggle_menu(window, cx))
                }
            })
            .tooltip(tips::tip_with(
                menu_tip_selector,
                SharedString::new_static("Actions"),
                Some("marketdata::menu"),
                None,
            )),
    );
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
                state: RowState::Document,
            }],
            ..MatrixModel::default()
        }
    }

    fn model_with_rows() -> MatrixModel {
        MatrixModel {
            rows: vec![crate::core::matrix::RowModel {
                label: "row".into(),
                cells: Vec::new(),
                state: RowState::Document,
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
            upload_error: None,
            echo: None,
            prompt: None,
            source_at: None,
            incomplete: 0,
            clock: Clock::utc(),
        }
    }

    /// Nonzero incomplete-row counts appear in warning tone between state and
    /// notice; zero adds no message.
    #[test]
    fn incomplete_rows_are_a_warn_chip_after_the_state() {
        let model = model_with_rows();
        let key = vec!["SPX.Z".to_string()];
        let mut one = inputs(&model, Some(&key), DraftBadge::Dirty);
        one.incomplete = 1;
        let h = HeaderModel::prepare(one);
        assert_eq!(
            h.incomplete
                .as_ref()
                .map(|(t, tone)| (t.to_string(), *tone)),
            Some(("1 row incomplete".to_string(), Tone::Warn))
        );
        assert_eq!(h.texts(), vec!["CVI", "SPX.Z", "1 row incomplete"]);

        let notice: SharedString = "'abc' is not a number".into();
        let mut two = inputs(
            &model,
            Some(&key),
            DraftBadge::Behind {
                newer: "2026-09-14T14:09:00Z".into(),
            },
        );
        two.incomplete = 2;
        two.notice = Some(&notice);
        let texts = HeaderModel::prepare(two).texts();
        let state_at = texts.iter().position(|t| t.starts_with("update ")).unwrap();
        let chip_at = texts.iter().position(|t| t == "2 rows incomplete").unwrap();
        let notice_at = texts
            .iter()
            .position(|t| t == "'abc' is not a number")
            .unwrap();
        assert!(state_at < chip_at && chip_at < notice_at, "{texts:?}");

        let none = HeaderModel::prepare(inputs(&model, Some(&key), DraftBadge::Dirty));
        assert!(none.incomplete.is_none());
        assert_eq!(none.texts(), vec!["CVI", "SPX.Z"]);
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
            Clock::utc().hm(chrono::DateTime::parse_from_rfc3339(&newer)
                .unwrap()
                .to_utc())
        );
        assert_eq!(
            behind
                .state
                .as_ref()
                .map(|(t, tone)| (t.to_string(), *tone)),
            Some((expected, Tone::Warn))
        );
    }

    /// Sent uses the supplied clock for its timestamp and Time tone, without a
    /// dirty dot. Behind uses the same formatter but retains warning state.
    #[test]
    fn sent_reads_sent_hhmm_and_carries_no_dirty_dot() {
        let model = model_with_rows();
        let key = vec!["SPX.Z".to_string()];
        let at = chrono::Utc::now().to_rfc3339();
        let sent = HeaderModel::prepare(inputs(
            &model,
            Some(&key),
            DraftBadge::Sent { at: at.clone() },
        ));
        let expected = format!(
            "sent {}",
            Clock::utc().hm(chrono::DateTime::parse_from_rfc3339(&at).unwrap().to_utc())
        );
        assert!(!sent.dirty);
        assert_eq!(
            sent.state.as_ref().map(|(t, tone)| (t.to_string(), *tone)),
            Some((expected, Tone::Time))
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
    fn the_time_is_on_the_clock_hhmmss_and_stale_is_a_flag() {
        let model = model_with_rows();
        let key = vec!["SPX.Z".to_string()];
        let at = chrono::DateTime::parse_from_rfc3339("2026-09-18T22:00:00Z")
            .unwrap()
            .to_utc();
        let mut i = inputs(&model, Some(&key), DraftBadge::Clean);
        i.source_at = Some(at);
        let h = HeaderModel::prepare(i);
        assert_eq!(h.time.as_deref(), Some("22:00:00"));
        assert!(
            !h.stale,
            "staleness is the tile's clock reading, applied at paint"
        );
    }

    /// Check active, typing, and inactive date-segment text against their actual
    /// fills on every bundled theme, including the adjusted primary text color.
    #[gpui::test]
    fn date_segment_colours_are_readable_on_every_bundled_theme(cx: &mut gpui::TestAppContext) {
        use crate::delegate::tests::{ground, over};
        use geode_core::colour::{READABLE_RATIO, contrast_ratio};
        use geode_shell::shell::colours::to_rgb;
        use gpui_component::ActiveTheme as _;

        cx.update(gpui_component::init);
        let (service, _) = geode_shell::theme::load_bundled();
        let mut failures = Vec::new();
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                let tones = FlooredTones::derive(theme);
                for (state, active, typing) in [
                    ("active", true, false),
                    ("typing", true, true),
                    ("bare", false, false),
                ] {
                    let paint = date_segment_paint(theme, &tones, active, typing);
                    let under = match paint.fill {
                        Some(fill) => over(fill, ground(theme)),
                        None => ground(theme),
                    };
                    let ratio = contrast_ratio(to_rgb(paint.text), under);
                    if ratio < READABLE_RATIO {
                        failures.push(format!("{name}: {state} segment at {ratio:.2}:1"));
                    }
                }
            });
        }
        assert!(
            failures.is_empty(),
            "unreadable date segments:\n{}",
            failures.join("\n")
        );
    }
}
