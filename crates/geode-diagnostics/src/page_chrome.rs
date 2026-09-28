//! The page's chrome: the header with its state chips and back control,
//! the section rail with its badges, and the cursor row's detail strip.
//! Pointer routes only; every action here has a keyboard route in `page`.

use geode_shell::actions::ActionId;
use geode_shell::fonts;
use geode_shell::module::ShellActions;
use geode_shell::shell::{chip, listrow, scale};
use gpui::prelude::*;
use gpui::{
    AnyElement, ClipboardItem, Context, Div, FontWeight, MouseButton, SharedString, Stateful,
    WeakEntity, div,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use crate::model::{Badges, Tone, health_tone};
use crate::page::DiagnosticsPage;
use crate::prepared::PreparedRow;
use crate::section::Section;

/// Header strip height, in pixels at the design rem.
const HEADER_HEIGHT: f32 = 28.0;
/// Rail width and row height, in pixels at the design rem.
const RAIL_WIDTH: f32 = 120.0;
const RAIL_ROW_HEIGHT: f32 = 24.0;
/// The health dot's diameter, in pixels at the design rem.
const DOT_SIZE: f32 = 7.0;
/// Detail strip height, in pixels at the design rem: three mono lines.
const DETAIL_HEIGHT: f32 = 64.0;

/// A model tone as a chip tone: warning and error through the chip door,
/// anything else neutral.
fn chip_tone(tone: Tone) -> chip::Tone {
    match tone {
        Tone::Warn => chip::Tone::Warning,
        Tone::Error => chip::Tone::Danger,
        Tone::Normal | Tone::Muted | Tone::Marked => chip::Tone::Neutral,
    }
}

/// A toolbar control wrapped so a pointer test can find it by `id`.
pub(crate) fn probed(id: &'static str, control: impl IntoElement) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .child(control)
}

/// Stable ids for the rail rows; a static string per section, so paint
/// formats nothing.
fn rail_id(section: Section) -> &'static str {
    match section {
        Section::Sources => "diagnostics-rail-sources",
        Section::Data => "diagnostics-rail-data",
        Section::Config => "diagnostics-rail-config",
        Section::Log => "diagnostics-rail-log",
        Section::Perf => "diagnostics-rail-perf",
    }
}

/// The rail badge text per section, formatted once per badge refresh.
pub(crate) fn rail_texts(badges: &Badges) -> [SharedString; 5] {
    let (errors, warnings) = badges.config;
    [
        SharedString::from(badges.sources.1.to_string()),
        SharedString::from(badges.datasets.to_string()),
        if errors + warnings > 0 {
            SharedString::from(format!("{errors} · {warnings}"))
        } else {
            SharedString::default()
        },
        if badges.log_errors > 0 {
            SharedString::from(format!("err {}", badges.log_errors))
        } else {
            SharedString::default()
        },
        SharedString::from(badges.perf_p95.clone()),
    ]
}

/// The title, the state chips worst first, and the back control, which
/// closes the page through the shell-actions handle: `page::close` is a
/// shell action, and the shell defers it past this listener.
pub(crate) fn header(
    chips: &[(SharedString, Tone)],
    actions: ShellActions,
    cx: &mut Context<DiagnosticsPage>,
) -> AnyElement {
    let theme = cx.theme();
    let chips: Vec<_> = chips
        .iter()
        .map(|(text, tone)| {
            let paint = chip::chip_paint(theme, chip_tone(*tone));
            div()
                .px_1()
                .rounded(theme.radius)
                .text_xs()
                .when_some(paint.fill, |el, fill| el.bg(fill))
                .text_color(paint.text)
                .child(text.clone())
        })
        .collect();
    h_flex()
        .h(scale::design(HEADER_HEIGHT))
        .flex_none()
        .items_center()
        .gap_2()
        .px_2()
        .border_b_1()
        .border_color(theme.border)
        .debug_selector(|| "diagnostics-header".to_string())
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child("Diagnostics"),
        )
        .children(chips)
        .child(div().flex_1())
        .child(
            div()
                .id("diagnostics-back")
                .debug_selector(|| "diagnostics-back".to_string())
                .child(
                    Button::new("diagnostics-back")
                        .ghost()
                        .xsmall()
                        .icon(IconName::ChevronLeft)
                        .tooltip("Close page")
                        .on_click(move |_, window, cx| {
                            actions(&ActionId("page::close".into()), window, cx);
                        }),
                ),
        )
        .into_any_element()
}

/// One row per section with its badge; a press selects the section.
pub(crate) fn rail(
    section: Section,
    badges: &Badges,
    texts: &[SharedString; 5],
    weak: WeakEntity<DiagnosticsPage>,
    cx: &mut Context<DiagnosticsPage>,
) -> AnyElement {
    let theme = cx.theme();
    let paint = listrow::row_paint(theme);
    let warning = chip::chip_paint(theme, chip::Tone::WarningText).text;
    let danger = chip::chip_paint(theme, chip::Tone::DangerText).text;
    let muted = theme.muted_foreground;
    let (worst, _) = &badges.sources;
    let dot = match health_tone(worst.as_ref()) {
        Tone::Warn => warning,
        Tone::Error => danger,
        Tone::Muted => muted,
        Tone::Normal | Tone::Marked => theme.success,
    };
    let (errors, _) = badges.config;
    let rows = Section::ALL.into_iter().map(|s| {
        let weak = weak.clone();
        let text = texts[s as usize].clone();
        let badge = match s {
            Section::Sources => h_flex()
                .gap_1()
                .items_center()
                .when(worst.is_some(), |el| {
                    el.child(div().size(scale::design(DOT_SIZE)).rounded_full().bg(dot))
                })
                .child(div().text_color(muted).child(text)),
            Section::Config => div()
                .text_color(if errors > 0 { danger } else { warning })
                .child(text),
            Section::Log => div().text_color(danger).child(text),
            Section::Data | Section::Perf => div().text_color(muted).child(text),
        };
        let row = h_flex()
            .id(rail_id(s))
            .debug_selector(move || rail_id(s).to_string())
            .h(scale::design(RAIL_ROW_HEIGHT))
            .items_center()
            .justify_between()
            .px_2()
            .text_sm()
            .child(s.title())
            .child(badge.text_xs())
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                let _ = weak.update(cx, |p, cx| p.set_section(s, window, cx));
            });
        listrow::paint_row(row, paint, s == section)
    });
    v_flex()
        .w(scale::design(RAIL_WIDTH))
        .flex_none()
        .h_full()
        .border_r_1()
        .border_color(theme.border)
        .children(rows)
        .into_any_element()
}

/// A selected row's detail lines; `copy` adds a button that puts that
/// text on the clipboard (the log's full record). `selector` names the
/// strip for tests: the Config section paints one per table.
pub(crate) fn detail_strip(
    selector: &'static str,
    row: Option<&PreparedRow>,
    copy: Option<SharedString>,
    cx: &mut Context<DiagnosticsPage>,
) -> AnyElement {
    let theme = cx.theme();
    let lines = v_flex().flex_1().min_w_0().when_some(row, |el, row| {
        el.children(row.detail.iter().cloned().map(|line| {
            div()
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .child(line)
        }))
    });
    let lines = if row.is_none() {
        lines.child(
            div()
                .text_color(theme.muted_foreground)
                .child("select a row"),
        )
    } else {
        lines
    };
    h_flex()
        .h(scale::design(DETAIL_HEIGHT))
        .flex_none()
        .items_start()
        .border_t_1()
        .border_color(theme.border)
        .px_2()
        .py_1()
        .font_family(fonts::MONO)
        .text_xs()
        .debug_selector(move || selector.to_string())
        .child(lines)
        .when_some(copy, |el, text| {
            el.child(
                div()
                    .id("diagnostics-copy")
                    .debug_selector(|| "diagnostics-copy".to_string())
                    .child(
                        Button::new("diagnostics-copy")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Copy)
                            .tooltip("Copy")
                            .on_click(move |_, _window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
                            }),
                    ),
            )
        })
        .into_any_element()
}
