//! Render a tile-bottom prompt with Input and inline error text.
//! The command popup scrolls the full candidate list in a viewport capped at
//! eight rows. The controller keeps keyboard selection visible.

use crate::commandline::{CommandLine, Prompt};
use crate::fonts;
use crate::shell::scale;
use crate::tiling::Rect;
use gpui::prelude::*;
use gpui::{App, Entity, IntoElement, Pixels, ScrollHandle, div, px, rems};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

/// Strip height, in pixels at the design rem (`shell::scale`).
pub const HEIGHT: f32 = 28.0;
/// Completion row height, in pixels at the design rem.
const ROW_HEIGHT: f32 = 24.0;
const MAX_ROWS: usize = 8;

/// `tile` is the focused tile's rect in window pixels; `rem_size` is the
/// window's rem, which the strip's own height, its completion rows and
/// the popup's width clamp follow (the 1 px insets are the tile border).
pub fn render(
    line: &CommandLine,
    input: &Entity<InputState>,
    scroll: &ScrollHandle,
    fuzzy_find: Option<&Entity<crate::fuzzyfind::FuzzyFind>>,
    tile: Rect,
    rem_size: Pixels,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let row_paint = crate::shell::listrow::row_paint(theme);
    let height = scale::design_px(HEIGHT, rem_size);
    let row_height = scale::design_px(ROW_HEIGHT, rem_size);
    let strip_top = tile.y + tile.h - height - 1.0;
    let mut strip = h_flex()
        .absolute()
        .left(px(tile.x + 1.0))
        .top(px(strip_top))
        .w(px((tile.w - 2.0).max(0.0)))
        .h(px(height))
        .items_center()
        .gap_2()
        .px_2()
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .border_t_1()
        .border_color(theme.border)
        // The command-line key context reclaims Tab from Root focus cycling for completion.
        .key_context("GeodeCommandLine")
        .debug_selector(|| "command-line".to_string())
        .child(
            div()
                .font_family(fonts::MONO)
                .text_color(theme.muted_foreground)
                .child(line.prompt.glyph()),
        )
        .child(Input::new(input).appearance(false).w_full());
    if let Some(error) = &line.error {
        strip = strip.child(div().text_color(theme.danger).child(error.clone()));
    }

    let mut layer = div().absolute().left_0().top_0().size_full().child(strip);
    if let Some(results) = fuzzy_find {
        if results.read(cx).has_table() {
            let (path, status) = results.read(cx).context();
            let context_height = scale::design_px(crate::fuzzyfind::CONTEXT_HEIGHT, rem_size);
            return layer.child(
                h_flex()
                    .absolute()
                    .left(px(tile.x + 1.0))
                    .top(px(strip_top - context_height))
                    .w(px((tile.w - 2.0).max(0.0)))
                    .h(px(context_height))
                    .px_2()
                    .gap_3()
                    .bg(theme.background)
                    .border_t_1()
                    .border_color(theme.border)
                    .text_color(theme.muted_foreground)
                    .text_xs()
                    .debug_selector(|| "find-context".into())
                    .child(div().flex_1().min_w_0().truncate().child(path))
                    .child(div().flex_shrink_0().child(status)),
            );
        }
        // The search surface temporarily occupies this tile; the underlying
        // tree keeps its expansion, selection, and scroll until a result is picked.
        return layer.child(
            div()
                .absolute()
                .left(px(tile.x + 1.0))
                .top(px(tile.y + 1.0))
                .w(px((tile.w - 2.0).max(0.0)))
                .h(px((strip_top - tile.y - 1.0).max(0.0)))
                .child(results.clone()),
        );
    }
    if line.prompt == Prompt::Command && !line.candidates.is_empty() {
        let rows = line.candidates.len().min(MAX_ROWS);
        // Account for p_1 on both edges and the physical border hairlines.
        let chrome_height = f32::from(rems(0.5).to_pixels(rem_size)) + 2.0;
        let list_height =
            (rows as f32 * row_height).min((strip_top - tile.y - 1.0 - chrome_height).max(0.0));
        let popup = v_flex()
            .absolute()
            .left(px(tile.x + 1.0))
            .top(px(strip_top - list_height - chrome_height))
            .w(px(((tile.w - 2.0) * 0.5).clamp(
                scale::design_px(160.0, rem_size),
                scale::design_px(420.0, rem_size),
            )))
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius)
            .p_1();
        let mut list = v_flex()
            .id("command-completions")
            .h(px(list_height))
            .overflow_y_scroll()
            .track_scroll(scroll)
            .debug_selector(|| "completion-list".to_string());
        for (i, r) in line.candidates.iter().enumerate() {
            let text = &line.words[r.row];
            let mut row = div()
                .h(px(row_height))
                .flex_shrink_0()
                .px_2()
                .flex()
                .items_center()
                .rounded(theme.radius)
                .font_family(fonts::MONO)
                .debug_selector(move || format!("completion-row-{i}"))
                .child(crate::palette::highlighted_title(
                    text,
                    &r.indices,
                    row_paint.accent,
                ));
            // No hover fill here: a completion row is not clickable (`tab`
            // completes it, and a mouse-down falls through to the tile,
            // which cancels the `:` line), and a hover promises a click.
            if i == line.highlighted {
                row = row.bg(row_paint.active).text_color(row_paint.text);
            }
            list = list.child(row);
        }
        layer = layer.child(popup.child(list));
    }
    layer
}
