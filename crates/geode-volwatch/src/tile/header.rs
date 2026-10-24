//! The tile's chrome, top to bottom: the shared header strip (the mark,
//! the `Vol · <list> ▾` switch control with the switcher hung beneath it,
//! the member count, the lookback chips with the one in force lit, the
//! sort chip, the members not in the surface in the warning tone, one chip
//! per failed dataset in the danger tone; in the cluster the mode, the
//! in-flight `…`, the per-dataset source times, link chips, health, `⋯`
//! and ×), the view strip (the tabs with their digits, the active one
//! lit, the step keys at the right), the grid or the empty state, and the
//! footer (the first reported notice on the left, the cursor readout on
//! the right).
//!
//! Every string is prepared outside render (`HeaderModel`, `times`,
//! `readout`, `strip_keys`); paint clones them. A press takes the same
//! door its key does: the switch control and the view strip dispatch
//! their actions, a lookback chip sets its lookback as `d` would.

use std::rc::Rc;

use geode_core::volboard::Lookback;
use geode_shell::actions::ActionId;
use geode_shell::fonts;
use geode_shell::shell::chip::{Tone as ChipTone, chip_paint};
use geode_shell::shell::control::{self, PointerStates as _};
use geode_shell::shell::{kbd, scale};
use geode_shell::tips;
use geode_tile::header::{Cluster, MenuTrigger, Mode, link_chips};
use geode_tile::menu::{self, MenuIds};
use geode_tile::notice::{self, Tone};
use gpui::prelude::*;
use gpui::{AnyElement, Context, Div, ElementId, MouseButton, SharedString, Window, div};
use gpui_component::theme::Theme;
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use super::chrome::MenuKind;
use super::table::table_el;
use super::{LOOKBACK_ACTION, SWITCH_ACTION, VIEW_ACTIONS, VolwatchTile, WINDOW_ACTION};
use crate::core::lens::Window as LensWindow;
use crate::core::model::{CHEVRON, ChipKind, MARK};

/// The view strip's and the footer's heights in design pixels, the
/// blotter's footer.
const STRIP_HEIGHT: f32 = 20.0;
const FOOTER_HEIGHT: f32 = 20.0;

/// The lookback chips' element names, parallel to `Lookback::ALL`.
const LOOKBACK_IDS: [&str; 3] = [
    "volwatch-lookback-1d",
    "volwatch-lookback-1w",
    "volwatch-lookback-1m",
];

/// The view digits as static text, parallel to `VIEW_ACTIONS`.
const DIGITS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

use super::MENU_ACTION;

/// Run `action` through the tile's own door, as its key would.
fn dispatch(
    t: &mut VolwatchTile,
    action: &str,
    window: &mut Window,
    cx: &mut Context<VolwatchTile>,
) {
    t.dispatch(&ActionId(action.to_string()), None, window, cx);
}

pub(crate) fn render(tile: &VolwatchTile, cx: &mut Context<VolwatchTile>) -> Div {
    let theme = cx.theme().clone();
    let id = tile.tile.0;
    let header = render_header(tile, &theme, cx);
    let strip = render_strip(tile, &theme, cx);
    let body = match &tile.empty {
        Some(text) => div()
            .flex_1()
            .min_h_0()
            .p_2()
            .text_color(theme.muted_foreground)
            .child(text.clone()),
        None => table_el(&tile.table, id),
    };
    let footer = render_footer(tile, &theme, cx);
    let actions = actions_menu(tile, cx);
    v_flex()
        .size_full()
        .bg(theme.background)
        .text_color(theme.foreground)
        .debug_selector(move || format!("volwatch-{id}"))
        .child(
            // The `.` menu hangs from the header's right edge, under `⋯`; a
            // right press hangs it from the pointer instead.
            div()
                .relative()
                .w_full()
                .child(header)
                .when_some(actions.0, |el, menu| {
                    el.child(
                        div()
                            .absolute()
                            .right_0()
                            .top(scale::design(geode_tile::header::HEADER_HEIGHT))
                            .child(menu),
                    )
                }),
        )
        .when_some(actions.1, |el, menu| el.child(menu))
        .child(strip)
        .child(body)
        // The inspection band docks between the grid and the footer.
        .when(tile.state.band && tile.empty.is_none(), |el| {
            let paints = tile.table.read(cx).delegate().paints;
            el.child(super::band::render(&tile.band_model, &paints, &theme, id))
        })
        .child(footer)
}

/// The actions menu, as (hung under the header, hung at the pointer).
fn actions_menu(
    tile: &VolwatchTile,
    cx: &mut Context<VolwatchTile>,
) -> (Option<gpui::Deferred>, Option<gpui::Anchored>) {
    let Some(m) = tile.menu.as_ref().filter(|m| m.kind == MenuKind::Actions) else {
        return (None, None);
    };
    let ids = MenuIds::new("volwatch-actions", "volwatch-actions-row");
    let corner = if tile.menu_at.is_some() {
        gpui::Anchor::TopLeft
    } else {
        gpui::Anchor::TopRight
    };
    let menu = menu::render_menu(
        &m.menu,
        &ids,
        corner,
        &cx.entity(),
        |t: &mut VolwatchTile, _, cx| {
            if t.is_actions() {
                t.close_menu();
                cx.notify();
            }
        },
        cx,
    );
    match tile.menu_at {
        Some(at) => (
            None,
            Some(
                gpui::anchored()
                    .position_mode(gpui::AnchoredPositionMode::Window)
                    .position(at)
                    .child(menu),
            ),
        ),
        None => (Some(menu), None),
    }
}

/// The `Vol · <list> ▾` control: a press toggles the switcher (`g w`'s
/// action), which hangs from the control's bottom-left edge. The press is
/// taken in the capture phase, ahead of the open switcher's outside-press
/// closer: in the bubble phase the closing press would find the switcher
/// already closed and reopen it.
fn switch_control(
    tile: &VolwatchTile,
    theme: &Theme,
    cx: &mut Context<VolwatchTile>,
) -> AnyElement {
    let id = tile.tile.0;
    let entity = cx.entity();
    let switcher = tile
        .menu
        .as_ref()
        .filter(|m| m.kind == MenuKind::Switch)
        .map(|m| {
            let ids = MenuIds::new("volwatch-menu", "volwatch-menu-row");
            div()
                .debug_selector(|| "volwatch-switcher".into())
                .child(menu::render_menu(
                    &m.menu,
                    &ids,
                    gpui::Anchor::TopLeft,
                    &entity,
                    |t: &mut VolwatchTile, _, cx| {
                        if t.menu.is_some() {
                            t.close_menu();
                            cx.notify();
                        }
                    },
                    cx,
                ))
        });
    let open = switcher.is_some();
    let control = h_flex()
        .id(ElementId::NamedInteger(
            SharedString::new_static("volwatch-switch"),
            id,
        ))
        .flex_none()
        .gap_1()
        .px_1()
        .rounded(theme.radius_tokens().sm)
        .debug_selector(move || format!("volwatch-switch-{id}"))
        // Open keeps a selected fill, as the `⋯` trigger does.
        .when(open, |el| el.bg(theme.secondary))
        .when(!open, |el| {
            el.pointer_states(control::paint(
                theme,
                control::Rest::Bare,
                theme.background,
                theme.foreground,
            ))
        })
        .capture_any_mouse_down(move |event, window, cx| {
            if event.button == MouseButton::Left {
                entity.update(cx, |t, cx| dispatch(t, SWITCH_ACTION, window, cx));
            }
        })
        .tooltip(tips::tip_with(
            SharedString::new_static("tip-volwatch-switch"),
            SharedString::new_static("Switch watchlist"),
            Some(SWITCH_ACTION),
            None,
        ))
        .child(
            div()
                .text_color(theme.foreground)
                .child(tile.header.title.clone()),
        )
        .child(div().text_color(theme.muted_foreground).child(CHEVRON));
    div()
        .relative()
        .flex_none()
        .child(control)
        .when_some(switcher, |el, m| {
            el.child(div().absolute().left_0().bottom_0().child(m))
        })
        .into_any_element()
}

/// The three lens chips (the lookbacks under moves, the windows under
/// rich/cheap): the one in force lit in the neutral chip tone; a press on
/// another sets it, asking again as `d` or `w` does.
fn lookback_chips(tile: &VolwatchTile, theme: &Theme, cx: &mut Context<VolwatchTile>) -> Div {
    let id = tile.tile.0;
    let h = &tile.header;
    let kind = h.chip_kind;
    let lit_paint = chip_paint(theme, ChipTone::Neutral);
    h_flex()
        .flex_none()
        .gap_0p5()
        .children(
            h.chips
                .iter()
                .zip(LOOKBACK_IDS)
                .enumerate()
                .map(|(i, ((text, lit), name))| {
                    let text = SharedString::from(text.clone());
                    let entity = cx.entity();
                    let (pointer, text_color) = if *lit {
                        (
                            control::for_chip(theme, &lit_paint, theme.background),
                            lit_paint.text,
                        )
                    } else {
                        let muted = theme.muted_foreground;
                        (
                            control::paint(theme, control::Rest::Bare, theme.background, muted),
                            muted,
                        )
                    };
                    div()
                        .id(ElementId::NamedInteger(SharedString::new_static(name), id))
                        .flex_none()
                        .px_1()
                        .rounded(theme.radius_tokens().sm)
                        .text_color(text_color)
                        .when_some(lit_paint.fill.filter(|_| *lit), |el, fill| el.bg(fill))
                        .debug_selector(move || name.to_string())
                        .pointer_states(pointer)
                        .capture_any_mouse_down(move |event, _, cx| {
                            if event.button == MouseButton::Left {
                                entity.update(cx, |t, cx| {
                                    t.user_acted(cx);
                                    match kind {
                                        ChipKind::Lookback => {
                                            t.set_lens(|l| l.lookback = Lookback::ALL[i], true, cx)
                                        }
                                        ChipKind::Window => {
                                            t.set_lens(|l| l.window = LensWindow::ALL[i], true, cx)
                                        }
                                    }
                                });
                            }
                        })
                        .tooltip(tips::tip_with(
                            SharedString::new_static("tip-volwatch-lookback"),
                            SharedString::new_static(match kind {
                                ChipKind::Lookback => "Change lookback",
                                ChipKind::Window => "Change window",
                            }),
                            Some(match kind {
                                ChipKind::Lookback => LOOKBACK_ACTION,
                                ChipKind::Window => WINDOW_ACTION,
                            }),
                            None,
                        ))
                        .child(text)
                }),
        )
}

/// The left side, then the shared cluster.
fn render_header(tile: &VolwatchTile, theme: &Theme, cx: &mut Context<VolwatchTile>) -> Div {
    let id = tile.tile.0;
    let h = &tile.header;
    let muted = theme.muted_foreground;
    let left = h_flex()
        .items_center()
        .gap_2()
        .min_w_0()
        .overflow_hidden()
        .child(div().flex_none().text_color(muted).child(MARK))
        .child(switch_control(tile, theme, cx))
        .child(div().flex_none().text_color(muted).child("\u{b7}"))
        .child(
            div()
                .flex_none()
                .text_color(muted)
                .debug_selector(move || format!("volwatch-count-{id}"))
                .child(h.count_text.clone()),
        )
        .child(lookback_chips(tile, theme, cx))
        .when_some(h.sort.clone(), |el, sort| {
            el.child(
                div()
                    .flex_none()
                    .text_color(theme.foreground)
                    .debug_selector(move || format!("volwatch-sort-{id}"))
                    .child(sort),
            )
        })
        .when_some(h.missing_text.clone(), |el, missing| {
            el.child(
                div()
                    .flex_none()
                    .text_color(notice::color(Tone::Warning, theme))
                    .debug_selector(move || format!("volwatch-missing-{id}"))
                    .child(missing),
            )
        })
        .children(h.failed.iter().map(|dataset| {
            let name = dataset.clone();
            div()
                .flex_none()
                .text_color(notice::color(Tone::Danger, theme))
                .debug_selector(move || format!("volwatch-failed-{name}"))
                .child(dataset.clone())
        }));
    let entity = cx.entity();
    let mut cluster = Cluster::new(tile.tile);
    cluster.mode = Mode::from_key_mode(tile.mode());
    cluster.close = tile.close.clone();
    if tile.in_flight {
        cluster
            .status
            .push(div().child("\u{2026}").into_any_element());
    }
    cluster.times = tile.times.clone();
    cluster.links = link_chips(&tile.frame, cx);
    cluster.health = tile.health.chip();
    cluster.menu = Some(MenuTrigger {
        id: ElementId::NamedInteger(SharedString::new_static("volwatch-menu-button"), id),
        selector: SharedString::new_static("volwatch-menu-button"),
        tip_selector: SharedString::new_static("tip-volwatch-menu"),
        action: MENU_ACTION,
        open: tile.is_actions(),
        on_press: Rc::new(move |_, cx| {
            entity.update(cx, |t, cx| t.toggle_menu(cx));
        }),
    });
    geode_tile::header::frame(
        tile.stack.as_ref().and_then(|s| s.marker(theme, tile.tile)),
        left,
        cluster,
        theme,
    )
    .debug_selector(move || format!("volwatch-header-{id}"))
}

/// The tabs with their digits, the active one lit in the neutral chip
/// tone; a press dispatches that tab's digit action. The step key sits
/// at the right.
fn render_strip(tile: &VolwatchTile, theme: &Theme, cx: &mut Context<VolwatchTile>) -> Div {
    let id = tile.tile.0;
    let lit = chip_paint(theme, ChipTone::Neutral);
    let muted = theme.muted_foreground;
    let shown = tile.shown_view().map(|v| v.name);
    let views =
        tile.tabs()
            .into_iter()
            .zip(VIEW_ACTIONS)
            .zip(DIGITS)
            .map(|((name, action), digit)| {
                let active = shown.as_deref() == Some(name.as_str());
                let entity = cx.entity();
                let selector = SharedString::from(format!("volwatch-view-{name}-{id}"));
                let lit_selector = format!("volwatch-view-lit-{name}");
                let (pointer, text) = if active {
                    (control::for_chip(theme, &lit, theme.background), lit.text)
                } else {
                    (
                        control::paint(theme, control::Rest::Bare, theme.background, muted),
                        muted,
                    )
                };
                h_flex()
                    .id(ElementId::Name(selector.clone()))
                    .flex_none()
                    .gap_1()
                    .px_1()
                    .rounded(theme.radius_tokens().sm)
                    .text_color(text)
                    .when_some(lit.fill.filter(|_| active), |el, fill| el.bg(fill))
                    .debug_selector(move || selector.to_string())
                    .pointer_states(pointer)
                    .capture_any_mouse_down(move |event, window, cx| {
                        if event.button == MouseButton::Left {
                            entity.update(cx, |t, cx| dispatch(t, action, window, cx));
                        }
                    })
                    .child(
                        div()
                            .font_family(fonts::MONO)
                            .child(SharedString::new_static(digit)),
                    )
                    .child(
                        div()
                            .when(active, |el| el.debug_selector(move || lit_selector.clone()))
                            .child(SharedString::from(name)),
                    )
            });
    h_flex()
        .w_full()
        .h(scale::design(STRIP_HEIGHT))
        .flex_none()
        .items_center()
        .justify_between()
        .gap_2()
        .px_2()
        .text_xs()
        .border_b_1()
        .border_color(theme.border)
        .debug_selector(move || format!("volwatch-strip-{id}"))
        .child(
            h_flex()
                .items_center()
                .gap_1()
                .min_w_0()
                .overflow_hidden()
                .children(views),
        )
        // Each step key hints on its own: unbinding one keeps the other.
        .child(
            h_flex()
                .flex_none()
                .gap_2()
                .text_color(muted)
                .when_some(tile.strip_keys.0.as_deref(), |el, next| {
                    el.child(
                        div()
                            .debug_selector(move || format!("volwatch-strip-next-{id}"))
                            .child(kbd::hint(next, "next view")),
                    )
                })
                .when_some(tile.strip_keys.1.as_deref(), |el, prev| {
                    el.child(
                        div()
                            .debug_selector(move || format!("volwatch-strip-prev-{id}"))
                            .child(kbd::hint(prev, "previous view")),
                    )
                }),
        )
}

/// The first reported notice on the left, the cursor readout on the
/// right.
fn render_footer(tile: &VolwatchTile, theme: &Theme, cx: &mut Context<VolwatchTile>) -> Div {
    let id = tile.tile.0;
    let notices = tile.dismissed.visible(tile.reported.iter().cloned());
    let on_dismiss = notice::on_dismiss_with(&cx.entity(), |t: &mut VolwatchTile, n, cx| {
        t.dismiss_notice(n, cx)
    });
    let left = notices.first().map(|n| {
        notice::truncated(
            n,
            ElementId::NamedInteger(SharedString::new_static("volwatch-notice"), id),
            SharedString::new_static("tip-volwatch-notice"),
            Some(&on_dismiss),
            theme,
        )
    });
    h_flex()
        .w_full()
        .h(scale::design(FOOTER_HEIGHT))
        .flex_none()
        .items_center()
        .justify_between()
        .gap_2()
        .px_2()
        .text_xs()
        .border_t_1()
        .border_color(theme.border)
        .debug_selector(move || format!("volwatch-footer-{id}"))
        .child(div().min_w_0().flex_1().children(left))
        .child(
            div()
                .flex_none()
                .text_color(theme.muted_foreground)
                .font_family(fonts::MONO)
                .debug_selector(move || format!("volwatch-readout-{id}"))
                .child(tile.readout.clone()),
        )
}
