//! The tile's chrome: the shared header strip (the shape title, the scope
//! readout and the state chips on the left; mode, the in-flight mark,
//! dataset times, link chips, health, `⋯` and × in the cluster), the `.`
//! menu hung under the header's right edge, the table with its two-level
//! column header, and the footer (the reported notice on the left; the
//! selection's totals, or the cursor readout, on the right).

use crate::core::readout::ScopeReadout;
use crate::tile::ScenarioTile;
use geode_shell::fonts;
use geode_shell::shell::chip::{Tone, chip_paint};
use geode_shell::shell::control::{self, PointerStates as _};
use geode_shell::shell::{aggregates, scale};
use geode_shell::tips;
use geode_tile::header::{Cluster, HEADER_HEIGHT, MenuTrigger, Mode, link_chips};
use geode_tile::notice;
use gpui::prelude::*;
use gpui::{Context, Div, ElementId, SharedString, div};
use gpui_component::table::DataTable;
use gpui_component::theme::Theme;
use gpui_component::{ActiveTheme as _, Sizable as _, Size, h_flex, v_flex};
use std::rc::Rc;

/// The footer strip's height in design pixels, the blotter's.
const FOOTER_HEIGHT: f32 = 20.0;

/// One header chip, prepared when the state it reads changes: the words,
/// the tone, the tooltip, and the action a click dispatches (`None` for a
/// chip only a `:` command changes).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Chip {
    /// The chip's stable element name (`scenario-chip-currency`), one per
    /// kind of chip.
    pub key: &'static str,
    pub text: SharedString,
    pub tone: Tone,
    pub tip: SharedString,
    pub action: Option<&'static str>,
    /// The tooltip's selector, `tip-{key}-{tile}`.
    pub selector: SharedString,
}

pub(crate) fn render(tile: &ScenarioTile, cx: &mut Context<ScenarioTile>) -> Div {
    let theme = cx.theme().clone();
    let id = tile.tile.0;
    let header = render_header(tile, &theme, cx);
    let menu = tile.render_menu(cx);
    let footer = render_footer(tile, &theme, cx);
    v_flex()
        .size_full()
        .debug_selector(move || format!("tile-content-{id}"))
        .child(
            // The `.` menu hangs from the header's right edge, under `⋯`.
            div()
                .relative()
                .w_full()
                .child(header)
                .when_some(menu, |el, menu| {
                    el.child(
                        div()
                            .absolute()
                            .right_0()
                            .top(scale::design(HEADER_HEIGHT))
                            .child(menu),
                    )
                }),
        )
        .child(
            // `min_h_0`: without it the virtualised list takes its intrinsic
            // height and paints no rows.
            div().flex_1().min_h_0().w_full().child(
                DataTable::new(&tile.table)
                    .with_size(Size::XSmall)
                    .bordered(false)
                    .stripe(false),
            ),
        )
        .child(footer)
}

/// The left side: the shape's title, the readout of the scope the next
/// query runs under (`all` muted, `nothing` in the danger tone), then the
/// chips in spec order.
fn render_header(tile: &ScenarioTile, theme: &Theme, cx: &mut Context<ScenarioTile>) -> Div {
    let id = tile.tile.0;
    let (readout, readout_text) = tile.readout();
    let readout_color = match readout {
        ScopeReadout::All => theme.muted_foreground,
        ScopeReadout::Nothing => chip_paint(theme, Tone::DangerText).text,
        ScopeReadout::Scope(_) => theme.foreground,
    };
    let entity = cx.entity();
    let mut left = h_flex()
        .gap_1()
        .items_center()
        .min_w_0()
        .overflow_hidden()
        .debug_selector(move || format!("scenario-title-{id}"))
        .child(
            div()
                .flex_none()
                .text_color(theme.foreground)
                .child(tile.shape.title.clone()),
        )
        .child(div().flex_none().child("\u{b7}"))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(readout_color)
                .debug_selector(move || format!("scenario-readout-{id}"))
                .child(readout_text.clone()),
        );
    for chip in tile.chips() {
        let paint = chip_paint(theme, chip.tone);
        let key = chip.key;
        let el = div()
            .id(ElementId::NamedInteger(SharedString::new_static(key), id))
            .flex_none()
            .ml_1()
            .px_1()
            .rounded(theme.radius_tokens().sm)
            .text_color(paint.text)
            .when_some(paint.fill, |el, fill| el.bg(fill))
            .debug_selector(move || format!("{key}-{id}"))
            .child(chip.text.clone())
            .tooltip(tips::tip_with(
                chip.selector.clone(),
                chip.tip.clone(),
                chip.action,
                None,
            ));
        left = left.child(match chip.action {
            // A clickable chip takes the same path as its key.
            Some(action) => {
                let entity = entity.clone();
                el.pointer_states(control::for_chip(theme, &paint, theme.background))
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |t, cx| {
                            t.dispatch(
                                &geode_shell::actions::ActionId(action.to_string()),
                                None,
                                window,
                                cx,
                            );
                        });
                    })
            }
            None => el,
        });
    }
    let mut cluster = Cluster::new(tile.tile);
    let mode = tile
        .key_context(cx)
        .get("mode")
        .unwrap_or("normal")
        .to_string();
    cluster.mode = Mode::from_key_mode(&mode);
    cluster.close = tile.close.clone();
    if tile.following.in_flight() {
        cluster
            .status
            .push(div().child("\u{2026}").into_any_element());
    }
    cluster.times = tile.time_runs(chrono::Utc::now());
    cluster.links = link_chips(&tile.frame, cx);
    cluster.health = tile.health.chip();
    cluster.menu = Some(MenuTrigger {
        id: ElementId::NamedInteger(SharedString::new_static("scenario-menu-button"), id),
        selector: tile.menu_selector.clone(),
        tip_selector: SharedString::new_static("tip-scenario-menu"),
        action: "scenario::menu",
        open: tile.menu.is_some(),
        on_press: Rc::new(move |window, cx| {
            entity.update(cx, |t, cx| t.toggle_menu(window, cx));
        }),
    });
    geode_tile::header::frame(
        tile.stack.as_ref().and_then(|s| s.marker(theme, tile.tile)),
        left,
        cluster,
        theme,
    )
    .debug_selector(move || format!("scenario-header-{id}"))
}

fn render_footer(tile: &ScenarioTile, theme: &Theme, cx: &mut Context<ScenarioTile>) -> Div {
    let id = tile.tile.0;
    let notices = tile.dismissed.visible(tile.reported_notices());
    let on_dismiss = notice::on_dismiss_with(&cx.entity(), |t: &mut ScenarioTile, n, cx| {
        t.dismiss_notice(n, cx)
    });
    let left = notices.first().map(|n| {
        notice::truncated(
            n,
            ElementId::NamedInteger(SharedString::new_static("scenario-notice"), id),
            SharedString::new_static("tip-scenario-notice"),
            Some(&on_dismiss),
            theme,
        )
    });
    let d = tile.table.read(cx).delegate();
    let right = match &d.extent {
        Some(extent) => aggregates::strip(Some(extent), &d.summary, &[], theme).into_any_element(),
        None => div()
            .text_color(theme.muted_foreground)
            .font_family(fonts::MONO)
            .children(tile.cursor_readout(cx))
            .into_any_element(),
    };
    h_flex()
        .w_full()
        .h(scale::design(FOOTER_HEIGHT))
        .items_center()
        .justify_between()
        .gap_2()
        .px_2()
        .text_xs()
        .border_t_1()
        .border_color(theme.border)
        .debug_selector(move || format!("scenario-footer-{id}"))
        .child(div().min_w_0().flex_1().children(left))
        .child(right)
}
