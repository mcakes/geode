//! The Log section's toolbar: level toggles, the target select, the
//! message filter, the Follow switch, Clear, and the Levels popover that
//! requests per-target log levels through the `Diagnostics` entity. The
//! page keeps every piece of state; this module only paints it and routes
//! pointer actions back through the page's methods, which the keyboard
//! routes share.

use std::rc::Rc;

use geode_shell::shell::scale;
use gpui::prelude::*;
use gpui::{AnyElement, Entity, SharedString, WeakEntity, div};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use gpui_component::popover::Popover;
use gpui_component::select::{SearchableVec, Select, SelectState};
use gpui_component::switch::Switch;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex,
};

use crate::levels::{LevelRow, level_word};
use crate::log::LEVELS;
use crate::page::DiagnosticsPage;
use crate::page_chrome::probed;

/// The target select's width, in pixels at the design rem.
const TARGET_WIDTH: f32 = 160.0;
/// The popover's target-name column and the new-target field, in pixels
/// at the design rem.
const POPOVER_NAME_WIDTH: f32 = 96.0;
/// Each popover row's level buttons take one id slot per level, with
/// room to spare so a row index never collides with the next row's.
const PICK_STRIDE: usize = 8;

/// The toggle labels in [`LEVELS`] order: `Level`'s own display, spelled
/// once so paint formats nothing.
const LEVEL_LABELS: [&str; 5] = ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"];

/// What the page hands the Log toolbar to paint; the page keeps the state.
pub(crate) struct LogView<'a> {
    /// Which levels the filter admits, in [`LEVELS`] order.
    pub levels_on: [bool; 5],
    pub target_select: &'a Entity<SelectState<SearchableVec<SharedString>>>,
    pub filter: Input,
    pub follow: bool,
    pub popover_open: bool,
    /// Prepared at rebuild; the popover paints them without reading the
    /// entity.
    pub level_rows: Rc<Vec<LevelRow>>,
    pub new_target_input: &'a Entity<InputState>,
    /// Whether the new-target field names a target its buttons can set.
    pub new_target_ok: bool,
}

pub(crate) fn toolbar(view: LogView<'_>, weak: WeakEntity<DiagnosticsPage>) -> AnyElement {
    let toggle = weak.clone();
    let toggles = LEVELS.iter().enumerate().map(|(ix, _)| {
        let weak = toggle.clone();
        let label = LEVEL_LABELS[ix];
        div()
            .id(("diagnostics-level", ix))
            .debug_selector(move || format!("diagnostics-level-{label}"))
            .child(
                Button::new(("diagnostics-level", ix))
                    .xsmall()
                    .label(label)
                    .selected(view.levels_on[ix])
                    .on_click(move |_, _window, cx| {
                        let _ = weak.update(cx, |p, cx| p.toggle_level(ix, cx));
                    }),
            )
    });
    let (follow, clear, open, popover) = (weak.clone(), weak.clone(), weak.clone(), weak);
    h_flex()
        .gap_2()
        .p_2()
        .items_center()
        .flex_wrap()
        .child(h_flex().gap_1().children(toggles))
        // The select fills its box (`size_full`), so the box is what
        // sizes it; unboxed it would take the whole row.
        .child(
            div().flex_none().w(scale::design(TARGET_WIDTH)).child(
                Select::new(view.target_select)
                    .xsmall()
                    .placeholder("target"),
            ),
        )
        .child(view.filter)
        .child(probed(
            "diagnostics-follow",
            Switch::new("diagnostics-follow")
                .xsmall()
                .checked(view.follow)
                .label("Follow")
                .on_change(move |&on, _window, cx| {
                    let _ = follow.update(cx, |p, cx| p.set_follow(on, cx));
                }),
        ))
        .child(probed(
            "diagnostics-log-clear",
            Button::new("diagnostics-log-clear")
                .outline()
                .xsmall()
                .label("Clear")
                .on_click(move |_, _window, cx| {
                    let _ = clear.update(cx, |p, cx| p.clear_log(cx));
                }),
        ))
        .child(probed(
            "diagnostics-levels-open",
            Popover::new("diagnostics-levels")
                .trigger(
                    Button::new("diagnostics-levels-open")
                        .outline()
                        .xsmall()
                        .label("Levels…"),
                )
                .open(view.popover_open)
                .on_open_change(move |&is_open, _window, cx| {
                    let _ = open.update(cx, |p, cx| p.set_levels_open(is_open, cx));
                })
                .content({
                    let rows = view.level_rows;
                    let input = view.new_target_input.clone();
                    let new_target_ok = view.new_target_ok;
                    move |_, _, cx| levels_popover(&rows, &input, new_target_ok, &popover, cx)
                }),
        ))
        .into_any_element()
}

/// One row per target with its five level buttons, the effective level
/// selected; the default row only reads, because `request_level` files a
/// target, and a `default` target is not the default. The last row sets
/// a level for a target the list does not know yet.
fn levels_popover(
    rows: &Rc<Vec<LevelRow>>,
    new_target_input: &Entity<InputState>,
    new_target_ok: bool,
    weak: &WeakEntity<DiagnosticsPage>,
    cx: &mut gpui::App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let name = |text: SharedString| {
        div()
            .w(scale::design(POPOVER_NAME_WIDTH))
            .flex_none()
            .text_xs()
            .child(text)
    };
    let known = rows.iter().enumerate().map(|(row_ix, row)| {
        let line = h_flex()
            .gap_1()
            .items_center()
            .child(name(SharedString::from(row.target.clone())));
        if row_ix == 0 {
            return line.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(level_word(row.effective)),
            );
        }
        line.children(LEVELS.iter().enumerate().map(|(level_ix, level)| {
            let (weak, rows, level) = (weak.clone(), rows.clone(), *level);
            let word = level_word(level);
            let target = row.target.as_str();
            pick_button(
                row_ix * PICK_STRIDE + level_ix,
                word,
                move || format!("diagnostics-level-pick-{target}-{word}"),
                row.effective == level,
                false,
                move |_, _window, cx| {
                    let _ = weak.update(cx, |p, cx| {
                        p.pick_level(&rows[row_ix].target, level, cx);
                    });
                },
            )
        }))
    });
    let new_row_ix = rows.len();
    let fresh = h_flex()
        .gap_1()
        .items_center()
        .child(
            Input::new(new_target_input)
                .xsmall()
                .w(scale::design(POPOVER_NAME_WIDTH)),
        )
        .children(LEVELS.iter().enumerate().map(|(level_ix, level)| {
            let (weak, level) = (weak.clone(), *level);
            let word = level_word(level);
            pick_button(
                new_row_ix * PICK_STRIDE + level_ix,
                word,
                move || format!("diagnostics-level-pick-new-{word}"),
                false,
                !new_target_ok,
                move |_, window, cx| {
                    let _ = weak.update(cx, |p, cx| p.pick_new_target_level(level, window, cx));
                },
            )
        }));
    v_flex()
        .gap_1()
        .children(known)
        .child(fresh)
        .into_any_element()
}

fn pick_button(
    slot: usize,
    word: &'static str,
    selector: impl FnOnce() -> String,
    selected: bool,
    disabled: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(("diagnostics-level-pick", slot))
        .debug_selector(selector)
        .child(
            Button::new(("diagnostics-level-pick", slot))
                .xsmall()
                .ghost()
                .label(word)
                .selected(selected)
                .disabled(disabled)
                .on_click(on_click),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_toggle_labels_are_the_levels_own_display() {
        for (ix, level) in LEVELS.iter().enumerate() {
            assert_eq!(LEVEL_LABELS[ix], level.to_string());
        }
        assert!(LEVELS.len() < PICK_STRIDE, "one id slot per level per row");
    }
}
