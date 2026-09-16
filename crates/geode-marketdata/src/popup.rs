//! The tile-owned anchored popup (market-data spec 2026-09-14 §6.1): the
//! panel's own overlay, painted with gpui's `deferred(anchored(..))` so
//! it escapes the tile's own clip and paints above neighbouring tiles —
//! instant, no animation, Geode's own chrome rather than a gpui-component
//! `Dialog` or `Popover`. `Popup::Menu` is the only variant this task
//! builds; Task 7 adds `Picker`.

use crate::core::menu::MenuRow;
use crate::tile::MarketDataTile;
use gpui::prelude::*;
use gpui::{
    Anchor, AnchoredPositionMode, Entity, IntoElement, MouseButton, SharedString, anchored,
    deferred, div, px,
};
use gpui_component::{Theme, h_flex, v_flex};

/// What the tile currently has open. One variant today; a second
/// (`Picker`, Task 7) will make this crate's own `mode == menu`/
/// `mode == insert` split real at the popup level too.
pub(crate) enum Popup {
    Menu(MenuState),
}

/// The action list's own state: the prepared rows ([`crate::core::menu::rows`],
/// built once when the menu opens — never in `render`) and which one is
/// highlighted.
pub(crate) struct MenuState {
    pub rows: Vec<MenuRow>,
    pub highlighted: usize,
}

/// Paint the action list, anchored at the header's own right edge (spec
/// §6.1). `tile`/`tile_id` are this popup's own mouse door — a row click
/// picks it, exactly as `enter` on the highlighted row would — and a
/// click anywhere outside closes it.
pub(crate) fn render_menu(
    m: &MenuState,
    theme: &Theme,
    tile: &Entity<MarketDataTile>,
    tile_id: u64,
) -> impl IntoElement {
    let mut list = v_flex()
        .min_w(px(240.))
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .text_sm()
        .shadow_md()
        .debug_selector(move || format!("marketdata-menu-{tile_id}"))
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, _, cx| tile.update(cx, |t, cx| t.close_popup(cx))
        });
    for (i, row) in m.rows.iter().enumerate() {
        list = list.child(match row {
            MenuRow::Separator => div().h(px(1.)).my_1().bg(theme.border).into_any_element(),
            MenuRow::Section(s) => div()
                .px_3()
                .pt_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(s.clone())
                .into_any_element(),
            MenuRow::Action {
                title,
                hint,
                enabled,
                ..
            } => {
                let disabled = enabled.is_err();
                let reason: SharedString = match enabled {
                    Err(r) => (*r).into(),
                    Ok(()) => hint.clone(),
                };
                h_flex()
                    .px_3()
                    .py_0p5()
                    .justify_between()
                    .gap_4()
                    .when(i == m.highlighted, |d| d.bg(theme.list_active))
                    .text_color(if disabled {
                        theme.muted_foreground
                    } else {
                        theme.popover_foreground
                    })
                    .debug_selector(move || format!("marketdata-menu-row-{tile_id}-{i}"))
                    .on_mouse_down(MouseButton::Left, {
                        let tile = tile.clone();
                        move |_, window, cx| {
                            cx.stop_propagation();
                            tile.update(cx, |t, cx| t.menu_pick(i, window, cx))
                        }
                    })
                    .child(title.clone())
                    .child(div().text_color(theme.muted_foreground).child(reason))
                    .into_any_element()
            }
        });
    }
    deferred(
        anchored()
            .anchor(Anchor::TopRight)
            .position_mode(AnchoredPositionMode::Local)
            .snap_to_window_with_margin(px(8.))
            .child(list),
    )
    .with_priority(1)
}
