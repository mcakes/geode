//! The panel's header row (spec 2026-09-14 §4): prepared once per change
//! by [`HeaderModel::prepare`] — the tile's `changed()` door — and painted
//! by [`render`] with no formatting of its own.

use crate::core::Segment;
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

/// One date-field segment's colours (header spec §5.2, 2026-09-19): the
/// active segment on the theme's `primary` (the mockup's cursor-blue
/// block) in `primary_foreground` FLOORED against that fill
/// (`FlooredTones::primary_text` — seven bundled themes ship the pair
/// under 3:1), a segment mid-typing on `accent` in `accent_foreground`
/// (the mockup's "typing colour"; every bundled theme's pair clears the
/// floor as shipped), every other segment bare in `foreground`. The sweep
/// below checks all three rather than assuming them. Read by `render` and
/// by that test.
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

/// The segmented date field in an attribute's editor slot — or, since
/// spec §4.4, in a `Date` cell of the grid, painted there by
/// `MatrixDelegate::render_td` through this same function so the two
/// cannot paint or route keys differently: three spans in the data face
/// separated by `-`, the active segment highlighted, the container
/// bordered as the cell editor is. The field is the focusable
/// (`track_focus`) so its `on_key_down` sits on the focused element and
/// runs before the shell root's: `MarketDataTile::date_field_key` decides,
/// and a consumed key stops here. A click on a segment selects it and
/// STOPS propagation — the attribute value's own mouse-down (or the
/// table's own cell click, which cancels an open editor) would otherwise
/// cancel the editor the click was aimed into; a click on the container's
/// padding or the separators bubbles as before, so a click "elsewhere"
/// still cancels.
pub(crate) fn render_date_field(
    paint: &DateFieldPaint,
    focus: &FocusHandle,
    theme: &Theme,
    tones: &FlooredTones,
    tile: &Entity<MarketDataTile>,
    tile_id: u64,
) -> impl IntoElement {
    let separator: Hsla = theme.muted_foreground;
    let mut field = h_flex()
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
    for (i, text) in paint.segments.iter().enumerate() {
        let active = paint.active == i;
        let CellPaint {
            fill, text: colour, ..
        } = date_segment_paint(theme, tones, active, active && paint.typing);
        if i > 0 {
            field = field.child(div().text_color(separator).child("-"));
        }
        field = field.child(
            div()
                .px_0p5()
                .rounded(theme.radius_tokens().sm)
                .text_color(colour)
                .when_some(fill, |d, f| d.bg(f))
                .debug_selector(move || format!("marketdata-date-seg-{tile_id}-{i}"))
                .on_mouse_down(gpui::MouseButton::Left, {
                    let tile = tile.clone();
                    move |_event, window, cx| {
                        if let Some(segment) = Segment::at(i) {
                            tile.update(cx, |t, cx| t.date_segment_clicked(segment, window, cx));
                        }
                        cx.stop_propagation();
                    }
                })
                .child(text.clone()),
        );
    }
    field
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
    /// `Draft::incomplete_rows` — inserted rows with a required cell
    /// still empty (spec §5.2).
    pub incomplete: usize,
    /// The `AppClock` global (as-of dialog spec §6.1), read once by the
    /// tile and carried in here so `prepare` stays a pure function of
    /// its inputs — the tile's own `clock` field, never a fresh global
    /// read from inside `prepare`.
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
    /// Whether the draft has any edit — cell, attribute, or a row
    /// inserted or deleted (the badge reads `Dirty` for all three, one
    /// draft state) — the dirty dot.
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
    /// The one short state run (spec §4 item 5), if any.
    pub state: Option<(SharedString, Tone)>,
    /// `N rows incomplete` (dividend spec §5.2), painted after the state
    /// run and ahead of the notice, only while the count is non-zero.
    /// Prepared as a `(text, tone)` pair like `state`, so `render` paints
    /// the two through the same arm.
    pub incomplete: Option<(SharedString, Tone)>,
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
            DraftBadge::Sent => (false, Some(("sent".into(), Tone::Time))),
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
/// `cursor_attr`/`editor` (Task 5, spec §5.1/§5.2): which attribute, if
/// any, the cursor is on, and the open editor's own index and form
/// ([`EditorPaint`]: the text `Input`, or the segmented date field) when
/// it is an attribute being edited. `menu_open` (Task 6, spec §6.1)
/// is whether the action list is open, painting `⋯`'s own pressed state;
/// `tile` is this attribute strip's own mouse door (`cursor_to_attr`) and
/// `⋯`'s (`toggle_menu`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn render(
    h: &HeaderModel,
    cursor_attr: Option<usize>,
    editor: Option<(usize, EditorPaint<'_>)>,
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

    // 0. The stack marker (tile-stacks spec §5.1), first in the strip,
    //    through the one builder every module uses (`StackHandle::marker`)
    //    — a trader reading the chip never learns a second shape per
    //    module.
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
            // The mouse's form of `k` (spec §5.1): a click on an
            // attribute value moves the cursor to `Attr(i)` — cancelling
            // an open cell editor first, exactly as a grid cell click
            // does — and a DOUBLE-click opens the editor on it (user
            // ruling 2026-09-17; `attr_clicked` is the door, the count
            // decides). Deliberately no `cx.stop_propagation()` — the
            // shell's own tile-level mouse-down (focus re-arm) must still
            // run, the same rule every other tile mouse-down in this
            // codebase keeps (CLAUDE.md's focus rule); it is the shell's
            // insert-mode rule on that restore, not a swallowed event,
            // that keeps the opened editor focused.
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
    // The incomplete-rows chip (dividend spec §5.2), between the state
    // and the notice: a warning, since an incomplete row is one an
    // upload will refuse, and not an error, since nothing has gone wrong.
    if let Some((text, tone)) = &h.incomplete {
        row = row.child(
            div()
                .debug_selector(move || format!("marketdata-incomplete-{tile_id}"))
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

    // 7. `⋯` — the mouse door onto the action list (spec §6.1), the
    // click's own form of `.`.
    //
    // **On the CAPTURE phase, not the bubble one — and it does NOT stop
    // propagation (fix round 1, IMPORTANT-1).** The popup's own
    // `on_mouse_down_out` (`popup.rs`) is a Capture-phase listener that
    // fires on ANY mouse-down whose position is outside the popup's own
    // bounds — the button included, since the button is not inside the
    // popup — and Capture always runs to completion BEFORE Bubble even
    // starts. A Bubble-phase handler here would always run one beat
    // behind that: `down_out` would have already closed the popup by the
    // time this button's own handler asked whether one was open, so a
    // second click on the button (meant to close it) would instead see
    // it already closed and reopen it. Capturing here, ahead of
    // `down_out` in the same pass, is what lets this button decide the
    // click before the popup's own "outside" rule gets a say — that
    // ordering alone is what the toggle needs, and it needs nothing more:
    //
    // - First click (no popup open, so no `down_out` listener is even
    //   painted yet — the popup this click is about to open does not
    //   exist in the frame the click was dispatched against): this
    //   handler opens the menu; Bubble then runs untouched, exactly as
    //   any other tile click does — click-to-focus, drag arming,
    //   `pending_focus_restore` all still fire.
    // - Second click (popup open, so `down_out` IS painted): this
    //   handler closes the menu first, in Capture, ahead of `down_out`;
    //   `down_out`'s own `close_popup` then runs on an already-`None`
    //   popup and is a no-op; Bubble again runs untouched.
    //
    // `cx.stop_propagation()` was here in the first cut and was wrong: it
    // suppressed the shell's ENTIRE bubble phase for this click, so a
    // click on `⋯` on an unfocused tile opened the menu without ever
    // focusing that tile — `mode == menu` reached a context stack no
    // longer topped by this tile, and the menu's own `j`/`k`/`enter`/
    // `escape` drove whichever tile the shell had focused instead. This
    // button needs to go FIRST in Capture, never to be the LAST thing
    // that runs — unlike the attribute strip's click above, which was
    // never a propagation question at all (it always let Bubble run).
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
            // A bare control's pointer states (`control::PointerStates`)
            // while CLOSED; open, the button keeps its persistent fill
            // above and answers the pointer with nothing, as the guide
            // asks of a button that owns a popup (and as gpui-component's
            // own `Button` does while `selected`). The header sits on the
            // tile surface, the window background.
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
            source_at: None,
            incomplete: 0,
            clock: Clock::utc(),
        }
    }

    /// §5.2: an inserted row with a required cell still empty is counted
    /// in the header, in the warning tone, after the state run and ahead
    /// of the notice — and a count of zero paints nothing at all.
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

    /// The date field's two highlighted segments — the active one on
    /// `primary`, a mid-typing one on `accent` — must be readable on EVERY
    /// bundled theme at the same 3:1 floor the cell states hold to. Each is
    /// the theme's own paired token, but a pair is a promise the author
    /// made, not one this crate checked: seven bundled themes broke the
    /// `primary` pair (which is why `FlooredTones::primary_text` exists),
    /// none the `accent` one. Checked here rather than assumed, with the
    /// bare segment's `foreground` against the header ground beside them.
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
