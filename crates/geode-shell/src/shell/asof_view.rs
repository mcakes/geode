//! The as-of selector: a filter-only modal over [`AsOfState`]'s rows.
//! Timestamps use the clock captured when the dialog opens.
//!
//! List navigation moves the highlight; `1`–`5` with an empty query commits
//! a preset; Enter commits the highlighted row. Tab opens Custom, clears
//! the query, and seeds its segmented field. While Custom is open, Enter
//! commits, Escape or Tab closes the field, and other non-chord keys belong
//! to the field. Closing Custom leaves the cleared query in place. Escape
//! with no field open closes the modal through the shell input handler.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Hsla, MouseButton, SharedString, Window, div};
use gpui_component::{ActiveTheme as _, Theme, h_flex, v_flex};

use geode_core::colour::{Rgb, contrast_ratio, readable_on};
use geode_core::query::AsOf;
use geode_widgets::datefield::{FieldKey, SegmentPaint, route};

use crate::footer::{Hint, HintRow};
use crate::keymap::{Keystroke, Modifiers};
use crate::listfilter;

use super::asof_rows::{self, AsOfState, Commit, Row, Section};
use super::colours::{over, to_hsla, to_rgb};
use super::{ShellView, dialog, scale};

/// Content width for a label and a dated timestamp side by side.
const WIDTH: f32 = 640.0;

/// Row height and viewport cap. The explicit height bound gives the list
/// scrollable overflow, allowing scroll-follow to keep the highlight visible
/// without moving the dialog header or footer.
const ROW_HEIGHT: f32 = 28.0;
const VISIBLE_ROWS: usize = crate::choice::DEFAULT_CAP;

/// Open the dialog (`frame::as_of`, `mod+t`, the toolbar chip). A no-op
/// when this kind is already open (see `dialog::can_open`). The state is
/// built fresh from the frame, the clock and `now` — nothing survives a
/// close/reopen.
pub fn open(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if !dialog::can_open(view, dialog::DialogKind::AsOf) {
        return;
    }
    let clock = view.clock(cx);
    let frame = view.target_frame().read(cx);
    let publishes: Vec<_> = frame.recent_publishes().iter().cloned().collect();
    // The retained scroll handle must follow the fresh state's first row.
    view.as_of_scroll.scroll_to_item(0);
    view.as_of_dialog = Some(AsOfState::build(
        frame.as_of(),
        &publishes,
        clock,
        chrono::Utc::now(),
    ));
    // Track the global data revision so later publications refresh these rows.
    view.as_of_data_version = frame.versions().data;
    let entity = cx.entity();
    dialog::open_shell_dialog_with_key(
        view,
        window,
        cx,
        dialog::DialogKind::AsOf,
        "As of",
        move |shell, window, cx| build(shell, &entity, window, cx),
        Some(Rc::new(handle_key)),
        true,
    );
}

/// The `InputEvent::Change` arm's pure half (`shell/mod.rs`): the field's
/// text is the query. Answers whether the ranking changed.
pub fn on_query_changed(state: &mut AsOfState, text: &str) -> bool {
    state.set_query(text)
}

fn apply_commit(
    shell: &mut ShellView,
    commit: Commit,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    shell.target_frame().update(cx, |f, cx| {
        let next = match commit {
            Commit::At(t) => AsOf::At(t),
            Commit::Live => AsOf::Live,
        };
        if f.set_as_of(next) {
            cx.notify();
        }
    });
    shell.close_modal(window, cx);
}

/// Route modal keys into [`AsOfState`] and apply successful commits.
/// The shell's key path calls `sync_dialog_text` afterward, keeping the
/// shared Input aligned with model changes such as Custom clearing the query.
/// Enter relies on that equality to preserve the current highlight.
fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let Some(state) = shell.as_of_dialog.as_mut() else {
        return false;
    };
    // Custom claims every non-chord key, including keys the segmented field
    // cannot use. Letting one reach the shared Input could filter away Custom.
    // Chords remain with the outer modal handler.
    if state.field().is_some() {
        if ks.mods.is_chord() {
            return false;
        }
        if ks.key == "tab" {
            // Bare `tab` AND `shift+tab` leave the field — `route` never
            // claims "tab" (it is not a field key), so both spellings
            // land here the same way.
            state.close_field();
            cx.notify();
            return true;
        }
        match route(&ks.key, ks.mods.shift, false) {
            Some(FieldKey::Commit) => match state.commit() {
                Ok(commit) => apply_commit(shell, commit, window, cx),
                Err(_) => cx.notify(),
            },
            Some(FieldKey::Cancel) => {
                state.close_field();
                cx.notify();
            }
            Some(other) => {
                if let Some(field) = state.field_mut() {
                    field.apply(other);
                }
                cx.notify();
            }
            // An unrouted, non-chord key belongs to nobody but this
            // modal — swallowed, not left to fall through.
            None => {}
        }
        return true;
    }
    if ks.mods == Modifiers::NONE {
        match ks.key.as_str() {
            "enter" => {
                let live = shell.dialog_input.read(cx).value().to_string();
                let Some(state) = shell.as_of_dialog.as_mut() else {
                    return false;
                };
                // Input changes and `sync_dialog_text` must keep these equal. Re-ranking
                // here on divergence could reset the highlight and commit another row.
                debug_assert_eq!(
                    state.query(),
                    live.as_str(),
                    "the sync arm keeps the field and the model equal"
                );
                match state.commit() {
                    Ok(commit) => apply_commit(shell, commit, window, cx),
                    Err(_) => cx.notify(),
                }
                return true;
            }
            "tab" => {
                state.open_field();
                shell.as_of_scroll.scroll_to_item(asof_rows::child_index_of(
                    state.painted(),
                    state.highlighted(),
                ));
                cx.notify();
                return true;
            }
            key => {
                if let Some(commit) = state.jump_digit(key) {
                    apply_commit(shell, commit, window, cx);
                    return true;
                }
            }
        }
    }
    if let Some(cmd) = listfilter::nav_command(ks) {
        state.nav(cmd);
        shell.as_of_scroll.scroll_to_item(asof_rows::child_index_of(
            state.painted(),
            state.highlighted(),
        ));
        cx.notify();
        return true;
    }
    false
}

/// The Custom row's segment colours over the popover: the active segment
/// on `primary` in `primary_foreground` floored to the readable ratio
/// against it, a typing segment on `accent` in `accent_foreground`
/// floored the same way, the rest bare in `foreground`. The sweep
/// `segment_colours_are_readable_on_every_bundled_theme` checks all
/// three on every theme.
pub fn segment_paint(theme: &Theme) -> SegmentPaint {
    let popover = to_rgb(theme.popover);
    let black = Rgb {
        r: 0.0,
        g: 0.0,
        b: 0.0,
    };
    let white = Rgb {
        r: 1.0,
        g: 1.0,
        b: 1.0,
    };
    // Use the black or white endpoint with greater contrast against the fill.
    // The theme foreground can itself be below the required ratio, leaving
    // `readable_on` unable to reach the floor.
    let floored = |text: Hsla, fill: Hsla| -> Hsla {
        let ground = over(fill, popover);
        let toward = if contrast_ratio(black, ground) >= contrast_ratio(white, ground) {
            black
        } else {
            white
        };
        to_hsla(readable_on(to_rgb(text), ground, toward))
    };
    SegmentPaint {
        rest_text: theme.foreground,
        rest_fill: None,
        active_text: floored(theme.primary_foreground, theme.primary),
        active_fill: theme.primary,
        typing_text: floored(theme.accent_foreground, theme.accent),
        typing_fill: theme.accent,
        separator: theme.muted_foreground,
        suffix: theme.muted_foreground,
        radius: theme.radius_tokens().sm,
        flush: false,
    }
}

/// A painted row's element id, from what the row IS rather than where
/// the filter left it; `listrow::paint_row` needs one for its hover.
fn row_id(row: &Row) -> gpui::ElementId {
    match row {
        Row::Current(_) => ("as-of-current", 0usize).into(),
        Row::Live => ("as-of-live", 0usize).into(),
        Row::Preset(i) => ("as-of-preset", *i).into(),
        Row::Custom => ("as-of-custom", 0usize).into(),
        Row::Publish(i) => ("as-of-publish", *i).into(),
    }
}

fn build(
    shell: &ShellView,
    entity: &Entity<ShellView>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(state) = shell.as_of_dialog.as_ref() else {
        return div().into_any_element();
    };
    let theme = cx.theme();
    let paint = super::listrow::row_paint(theme);
    let muted = theme.muted_foreground;
    let danger = theme.danger;
    let radius = theme.radius;
    let segment_paint = segment_paint(theme);
    let clock = state.clock();

    let mut list = v_flex()
        .id("as-of-rows")
        .w_full()
        .gap_0p5()
        .max_h(scale::design(VISIBLE_ROWS as f32 * ROW_HEIGHT))
        .overflow_y_scroll()
        .track_scroll(&shell.as_of_scroll)
        .debug_selector(|| "as-of-rows".to_string());
    if state.painted().is_empty() {
        list = list.child(
            div()
                .px_2()
                .py_1()
                .text_sm()
                .text_color(muted)
                .child("nothing matches — edit the filter"),
        );
    }
    let mut last_section: Option<Section> = None;
    for (position, row) in state.painted().iter().enumerate() {
        if last_section != Some(row.section) {
            if let Some(eyebrow) = row.section.eyebrow() {
                list = list.child(
                    div()
                        .flex_shrink_0()
                        .px_2()
                        .pt_2()
                        .pb_0p5()
                        .text_xs()
                        .text_color(muted)
                        .child(eyebrow.to_uppercase()),
                );
            }
            last_section = Some(row.section);
        }
        let is_highlighted = position == state.highlighted();
        let el = h_flex()
            .id(row_id(&row.row))
            .w_full()
            .h(scale::design(ROW_HEIGHT))
            .flex_shrink_0()
            .px_2()
            .items_center()
            .gap_2()
            .text_sm()
            .rounded(radius)
            .debug_selector(move || format!("as-of-row-{position}"));
        let mut el = super::listrow::paint_row(el, paint, is_highlighted);
        let entity_for_click = entity.clone();
        let is_custom = matches!(row.row, Row::Custom);
        el = el.child(super::keybindings_view::highlighted_text(
            &row.label,
            &row.indices,
            paint.accent,
        ));
        if is_custom {
            if let Some(field) = state.field() {
                let segments = field.segments();
                let suffix: SharedString = clock.abbreviation(state.now()).into();
                let seg_entity = entity.clone();
                el = el.child(div().font_family(crate::fonts::MONO).child(
                    geode_widgets::datefield::paint(
                        &segments,
                        Some(suffix),
                        segment_paint,
                        "as-of-custom-seg".into(),
                        move |segment, window, cx| {
                            seg_entity.update(cx, |shell, cx| {
                                if let Some(f) =
                                    shell.as_of_dialog.as_mut().and_then(|s| s.field_mut())
                                {
                                    f.select(segment);
                                }
                                // Reconcile the shared Input after selecting a field segment.
                                dialog::sync_dialog_text(shell, window, cx);
                                cx.notify();
                            });
                        },
                    ),
                ));
                if let Some(refusal) = state.field_refusal() {
                    el = el.child(
                        div()
                            .ml_auto()
                            .text_xs()
                            .text_color(danger)
                            .debug_selector(|| "as-of-custom-refusal".to_string())
                            .child(refusal.to_string()),
                    );
                }
            } else {
                el = el.child(
                    div()
                        .ml_auto()
                        .text_xs()
                        .text_color(muted)
                        .child("tab edits"),
                );
            }
        } else {
            el = el.child(
                div()
                    .ml_auto()
                    .font_family(crate::fonts::MONO)
                    .text_xs()
                    .text_color(if is_highlighted { paint.text } else { muted })
                    .child(row.right.clone()),
            );
        }
        el = el.on_mouse_down(MouseButton::Left, move |_event, window, cx| {
            entity_for_click.update(cx, |shell, cx| {
                let Some(state) = shell.as_of_dialog.as_mut() else {
                    return;
                };
                if is_custom {
                    // A Custom body click opens the field if needed. Segment clicks stop
                    // propagation; both paths reconcile the shared Input.
                    if state.field().is_none() {
                        state.open_field();
                    }
                    dialog::sync_dialog_text(shell, window, cx);
                    cx.notify();
                    return;
                }
                if state.field().is_some() {
                    state.close_field();
                }
                if state.set_highlighted(position) {
                    match state.commit() {
                        Ok(commit) => apply_commit(shell, commit, window, cx),
                        Err(_) => cx.notify(),
                    }
                }
            });
        });
        list = list.child(el);
    }

    let hints: Vec<Hint> = if state.field().is_some() {
        vec![
            Hint::new(HintRow::Move, &["left", "right"], "segment"),
            Hint::new(HintRow::Move, &["up", "down"], "step").selector("as-of-hint-step"),
            Hint::new(HintRow::Move, &["shift+up"], "×10"),
            Hint::range(HintRow::Edit, "0", "9", "type"),
            Hint::new(HintRow::Edit, &["backspace"], "clear segment"),
            Hint::new(HintRow::Go, &["enter"], "set as-of"),
            Hint::new(HintRow::Go, &["escape"], "back to list"),
        ]
    } else {
        vec![
            Hint::prose(HintRow::Move, "type to filter"),
            Hint::new(HintRow::Move, &["up", "down"], "row"),
            Hint::range(HintRow::Move, "1", "5", "preset"),
            Hint::new(HintRow::Edit, &["tab"], "custom time").selector("as-of-hint-tab"),
            Hint::new(HintRow::Go, &["enter"], "set as-of"),
            Hint::new(HintRow::Go, &["escape"], "close"),
        ]
    };
    let footer = v_flex()
        .w_full()
        .gap_1()
        .pt_2()
        .border_t_1()
        .border_color(theme.border)
        .child(dialog::hint_rows(&hints));

    v_flex()
        .gap_2()
        .w(scale::design(WIDTH))
        .child(dialog::filter_row(&shell.dialog_input, None, cx))
        .child(list)
        .child(footer)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::colour::READABLE_RATIO;

    /// The three segment states over the popover on every bundled theme,
    /// no exception list — the same floor `chip_paint` and `row_paint`
    /// keep.
    #[gpui::test]
    fn segment_colours_are_readable_on_every_bundled_theme(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = crate::theme::load_bundled();
        let mut failures = Vec::new();
        let mut checked = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                let p = segment_paint(theme);
                let popover = to_rgb(theme.popover);
                for (state, text, fill) in [
                    ("rest", p.rest_text, None),
                    ("active", p.active_text, Some(p.active_fill)),
                    ("typing", p.typing_text, Some(p.typing_fill)),
                ] {
                    checked += 1;
                    let ground = fill.map(|f| over(f, popover)).unwrap_or(popover);
                    let ratio = contrast_ratio(to_rgb(text), ground);
                    if ratio < READABLE_RATIO {
                        failures.push(format!("{name}: {state} at {ratio:.2}:1"));
                    }
                }
            });
        }
        assert!(
            checked >= 3 * 40,
            "the sweep saw {checked} checks — bundled themes missing?"
        );
        assert!(
            failures.is_empty(),
            "unreadable segments:\n{}",
            failures.join("\n")
        );
    }
}
