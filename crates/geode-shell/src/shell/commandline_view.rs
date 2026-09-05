//! Paints the per-tile command line (Phase 3 §3.4): a one-line strip
//! along the bottom edge of the focused tile with the prompt glyph, the
//! shared `Input`, and an inline error; above it, when there are
//! candidates, a popup of ranked rows with match highlighting, reusing
//! the palette's row look.

use crate::commandline::{CommandLine, Prompt};
use crate::fonts;
use crate::tiling::Rect;
use gpui::prelude::*;
use gpui::{App, Entity, IntoElement, div, px};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

pub const HEIGHT: f32 = 28.0;
const ROW_HEIGHT: f32 = 24.0;
const MAX_ROWS: usize = 8;

pub fn render(
    line: &CommandLine,
    input: &Entity<InputState>,
    tile: Rect,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let strip_top = tile.y + tile.h - HEIGHT - 1.0;
    let mut strip = h_flex()
        .absolute()
        .left(px(tile.x + 1.0))
        .top(px(strip_top))
        .w(px((tile.w - 2.0).max(0.0)))
        .h(px(HEIGHT))
        .items_center()
        .gap_2()
        .px_2()
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .border_t_1()
        .border_color(theme.border)
        // `Root` binds bare `tab` to its own focus-cycling, unconditionally
        // (`dialog::init_reclaimed_keybindings`'s doc comment, bullet 3) —
        // this context is what lets `tab` reach `ShellView::
        // handle_command_line_key` instead of jumping focus elsewhere.
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
            .top(px(strip_top - rows as f32 * ROW_HEIGHT - 2.0))
            .w(px(((tile.w - 2.0) * 0.5).clamp(160.0, 420.0)))
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .rounded(px(4.))
            .p_1();
        for (i, r) in line.candidates.iter().take(MAX_ROWS).enumerate() {
            let text = &line.words[r.row];
            let mut row = div()
                .h(px(ROW_HEIGHT))
                .px_2()
                .flex()
                .items_center()
                .rounded(px(4.))
                .font_family(fonts::MONO)
                .debug_selector(move || format!("completion-row-{i}"))
                .child(crate::palette::highlighted_title(
                    text,
                    &r.indices,
                    theme.primary,
                ));
            if i == line.highlighted {
                row = row.bg(theme.selection);
            }
            list = list.child(row);
        }
        layer = layer.child(list);
    }
    layer
}
