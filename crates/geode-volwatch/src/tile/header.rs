//! The tile's chrome, top to bottom: the shared header strip (the mark,
//! the `VolMonitor · <list> ▾` switch control with the switcher hung beneath it,
//! the member count, the grouping chip (muted while nothing groups), the
//! lens chips (the lookbacks under moves, the
//! windows under rich/cheap) with the one in force lit, the sort chip, the
//! members not in the surface in the warning tone, one chip per failed
//! dataset in the danger tone; in the cluster the mode, the in-flight `…`,
//! the per-dataset source times, link chips, health, `⋯` and ×), the view
//! strip (the tabs behind their chords, the shown one lit; at the right the
//! lens readout and the hints for the keys that matter from here), the
//! grid or the empty state, and the footer (the first reported notice on
//! the left, the cursor readout on the right).
//!
//! Every string is prepared outside render (`HeaderModel`, `times`,
//! `readout`, `strip_tabs`, `strip_readout`, `strip_hints`); paint clones
//! handles. A press takes the same door its key does: the switch control
//! and the first nine tabs dispatch their actions, a later tab takes the
//! view door its chord would, a lens chip sets its lookback or window as
//! `d` or `w` would.

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

/// The lens chips' element names: the lookbacks parallel to
/// `Lookback::ALL`, the windows to `Window::ALL`. The header's chip kind
/// picks the set, so a window chip never answers to a lookback's name.
const LOOKBACK_IDS: [&str; 3] = [
    "volwatch-lookback-1d",
    "volwatch-lookback-1w",
    "volwatch-lookback-1m",
];
const WINDOW_IDS: [&str; 3] = [
    "volwatch-window-6M",
    "volwatch-window-1Y",
    "volwatch-window-2Y",
];

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

/// The `VolMonitor · <list> ▾` control: a press toggles the switcher (`g w`'s
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
fn lens_chips(tile: &VolwatchTile, theme: &Theme, cx: &mut Context<VolwatchTile>) -> Div {
    let id = tile.tile.0;
    let h = &tile.header;
    let kind = h.chip_kind;
    let (ids, tip_selector, tip_title, action) = match kind {
        ChipKind::Lookback => (
            LOOKBACK_IDS,
            "tip-volwatch-lookback",
            "Change lookback",
            LOOKBACK_ACTION,
        ),
        ChipKind::Window => (
            WINDOW_IDS,
            "tip-volwatch-window",
            "Change window",
            WINDOW_ACTION,
        ),
    };
    let lit_paint = chip_paint(theme, ChipTone::Neutral);
    h_flex()
        .flex_none()
        .gap_0p5()
        .children(
            h.chips
                .iter()
                .zip(ids)
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
                            SharedString::new_static(tip_selector),
                            SharedString::new_static(tip_title),
                            Some(action),
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
        // The grouping chip: the chain in force, muted while nothing
        // groups.
        .child(
            div()
                .flex_none()
                .text_color(if h.grouping_muted {
                    muted
                } else {
                    theme.foreground
                })
                .debug_selector(move || format!("volwatch-grouping-{id}"))
                .child(h.grouping.clone()),
        )
        .child(lens_chips(tile, theme, cx))
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

/// The tabs behind their chords, the shown one lit in the neutral chip
/// tone; a press on one of the first nine dispatches its view action, a
/// later one takes the view door directly. At the right the lens readout,
/// then one hint per key that matters from here.
fn render_strip(tile: &VolwatchTile, theme: &Theme, cx: &mut Context<VolwatchTile>) -> Div {
    let id = tile.tile.0;
    let lit = chip_paint(theme, ChipTone::Neutral);
    let muted = theme.muted_foreground;
    let views = tile.strip_tabs.iter().enumerate().map(|(i, (name, keys))| {
        let active = tile.shown_name.as_ref() == Some(name);
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
        let action = VIEW_ACTIONS.get(i).copied();
        let pressed = name.clone();
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
                    entity.update(cx, |t, cx| match action {
                        Some(action) => dispatch(t, action, window, cx),
                        None => {
                            t.user_acted(cx);
                            let _ = t.set_view_by_name(&pressed, cx);
                        }
                    });
                }
            })
            // The tab's chord as Kbd chips, the one door a key reaches
            // the screen through; a tab past the ninth has none.
            .when(!keys.is_empty(), |el| {
                let key_selector = format!("volwatch-view-key-{name}-{id}");
                el.child(
                    div()
                        .flex_none()
                        .debug_selector(move || key_selector.clone())
                        .child(kbd::binding(keys)),
                )
            })
            .child(
                div()
                    .when(active, |el| el.debug_selector(move || lit_selector.clone()))
                    .child(name.clone()),
            )
    });
    let hints = tile.strip_hints.iter().map(|(action, keys, label)| {
        let suffix = action.strip_prefix("volwatch::").unwrap_or(action);
        div()
            .flex_none()
            .debug_selector(move || format!("volwatch-strip-hint-{suffix}-{id}"))
            .child(kbd::hint(keys, label.clone()))
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
        // The readout says what the lens is; each hint names only its own
        // key, so unbinding one keeps the others.
        .child(
            h_flex()
                .flex_none()
                .items_center()
                .gap_2()
                .text_color(muted)
                .child(
                    div()
                        .font_family(fonts::MONO)
                        .debug_selector(move || format!("volwatch-lens-readout-{id}"))
                        .child(tile.strip_readout.clone()),
                )
                .children(hints),
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
