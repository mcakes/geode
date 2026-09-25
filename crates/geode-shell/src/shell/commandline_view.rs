//! Render a tile-bottom prompt with Input and inline error text.
//! The command popup paints the first eight ranked candidates, without scrolling
//! or click handlers. Selection cycles across the full candidate list, so a
//! highlight beyond those eight is not visible in this popup.

use crate::commandline::{CommandLine, Prompt};
use crate::fonts;
use crate::shell::scale;
use crate::tiling::Rect;
use gpui::prelude::*;
use gpui::{App, Entity, IntoElement, Pixels, div, px};
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
    if line.prompt == Prompt::Command && !line.candidates.is_empty() {
        let rows = line.candidates.len().min(MAX_ROWS);
        let mut list = v_flex()
            .absolute()
            .left(px(tile.x + 1.0))
            .top(px(strip_top - rows as f32 * row_height - 2.0))
            .w(px(((tile.w - 2.0) * 0.5).clamp(
                scale::design_px(160.0, rem_size),
                scale::design_px(420.0, rem_size),
            )))
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius)
            .p_1();
        for (i, r) in line.candidates.iter().take(MAX_ROWS).enumerate() {
            let text = &line.words[r.row];
            let mut row = div()
                .h(px(row_height))
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
        layer = layer.child(list);
    }
    layer
}
