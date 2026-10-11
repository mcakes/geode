//! The tile's chrome: the shared header strip (the title and readout on
//! the left; mode, link chips, health and × in the cluster), the table,
//! and the footer (the reported notice on the left; the selection's
//! totals, or the cursor readout, on the right).

use crate::tile::ScenarioTile;
use geode_shell::fonts;
use geode_shell::shell::{aggregates, scale};
use geode_tile::header::{Cluster, Mode, link_chips};
use geode_tile::notice;
use gpui::prelude::*;
use gpui::{Context, Div, ElementId, SharedString, div};
use gpui_component::table::DataTable;
use gpui_component::theme::Theme;
use gpui_component::{ActiveTheme as _, Sizable as _, Size, h_flex, v_flex};

/// The footer strip's height in design pixels, the blotter's.
const FOOTER_HEIGHT: f32 = 20.0;

pub(crate) fn render(tile: &ScenarioTile, cx: &mut Context<ScenarioTile>) -> Div {
    let theme = cx.theme().clone();
    let id = tile.tile.0;
    let mode = tile
        .key_context(cx)
        .get("mode")
        .unwrap_or("normal")
        .to_string();
    let mut cluster = Cluster::new(tile.tile);
    cluster.mode = Mode::from_key_mode(&mode);
    cluster.close = tile.close.clone();
    cluster.links = link_chips(&tile.frame, cx);
    cluster.health = tile.health.chip();
    let left = div()
        .min_w_0()
        .truncate()
        .text_color(theme.foreground)
        .debug_selector(move || format!("scenario-title-{id}"))
        .child(tile.title(cx));
    let header = geode_tile::header::frame(
        tile.stack
            .as_ref()
            .and_then(|s| s.marker(&theme, tile.tile)),
        left,
        cluster,
        &theme,
    )
    .debug_selector(move || format!("scenario-header-{id}"));
    let footer = render_footer(tile, &theme, cx);
    v_flex()
        .size_full()
        .debug_selector(move || format!("tile-content-{id}"))
        .child(header)
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
