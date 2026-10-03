//! The page's chrome: the header with its state chips and back control,
//! the section rail with its badges, and the cursor row's detail strip.
//! Native controls and shell keycaps share the page's command handlers.

use geode_shell::actions::ActionId;
use geode_shell::fonts;
use geode_shell::module::ShellActions;
use geode_shell::shell::{chip, kbd, scale};
use gpui::prelude::*;
use gpui::{AnyElement, Context, Div, FontWeight, SharedString, Stateful, WeakEntity, div};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{ActiveTheme as _, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use crate::model::{Badges, Tone};
use crate::page::DiagnosticsPage;
use crate::prepared::PreparedRow;
use crate::section::Section;

/// Navigation width follows the shell's design-rem scale.
const RAIL_WIDTH: f32 = 176.0;

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
        Section::Reference => "diagnostics-rail-reference",
        Section::Config => "diagnostics-rail-config",
        Section::Log => "diagnostics-rail-log",
        Section::Perf => "diagnostics-rail-perf",
    }
}

/// The rail badge text per section, formatted once per badge refresh.
pub(crate) fn rail_texts(badges: &Badges) -> [SharedString; 6] {
    let (errors, warnings) = badges.config;
    [
        SharedString::from(badges.sources.1.to_string()),
        SharedString::from(badges.datasets.to_string()),
        SharedString::from(badges.reference.to_string()),
        if errors + warnings > 0 {
            SharedString::from(format!("{} issues", errors + warnings))
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
    section: Section,
    chips: &[(SharedString, Tone)],
    actions: ShellActions,
    cx: &mut Context<DiagnosticsPage>,
) -> AnyElement {
    let theme = cx.theme();
    let chips = chips.iter().map(|(text, tone)| {
        let paint = chip::chip_paint(theme, chip_tone(*tone));
        div()
            .px_1()
            .rounded(theme.radius)
            .text_xs()
            .when_some(paint.fill, |el, fill| el.bg(fill))
            .text_color(paint.text)
            .child(text.clone())
    });
    h_flex()
        .min_h_8()
        .flex_none()
        .gap_2()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(theme.border)
        .debug_selector(|| "diagnostics-header".to_string())
        .child(probed(
            "diagnostics-back",
            Button::new("diagnostics-back")
                .ghost()
                .small()
                .icon(IconName::ChevronLeft)
                .label("Back")
                .tooltip("Back to workspace (Escape)")
                .on_click(move |_, window, cx| {
                    actions(&ActionId("page::close".into()), window, cx);
                }),
        ))
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child("Diagnostics"),
        )
        .child(div().text_color(theme.muted_foreground).child("/"))
        .child(div().text_sm().child(section.title()))
        .child(div().flex_1())
        .child(h_flex().gap_1().flex_wrap().justify_end().children(chips))
        .into_any_element()
}

pub(crate) fn rail(
    section: Section,
    badges: &Badges,
    texts: &[SharedString; 6],
    weak: WeakEntity<DiagnosticsPage>,
    cx: &mut Context<DiagnosticsPage>,
) -> AnyElement {
    let theme = cx.theme();
    let warning = chip::chip_paint(theme, chip::Tone::WarningText).text;
    let danger = chip::chip_paint(theme, chip::Tone::DangerText).text;
    let rows = Section::ALL.into_iter().map(|s| {
        let weak = weak.clone();
        let text = texts[s as usize].clone();
        let tone = match s {
            Section::Config if badges.config.0 > 0 => danger,
            Section::Config if badges.config.1 > 0 => warning,
            Section::Log if badges.log_errors > 0 => danger,
            _ => theme.muted_foreground,
        };
        probed(
            rail_id(s),
            Button::new(rail_id(s))
                .ghost()
                .small()
                .w_full()
                .selected(s == section)
                .accessibility_label(s.title())
                .tooltip(match s {
                    Section::Sources => "Source health and activity (g s)",
                    Section::Data => "Stored datasets and generations (g d)",
                    Section::Reference => "Reference data tables (g r)",
                    Section::Config => {
                        "Configuration and data issues, history, and effective values (g c)"
                    }
                    Section::Log => "Retained log records (g l)",
                    Section::Perf => {
                        "Timing, storage, and event delivery (g p); badge shows frame interval p95"
                    }
                })
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(s.title())
                        .child(div().flex_1())
                        .child(div().text_xs().text_color(tone).child(text)),
                )
                .on_click(move |_, window, cx| {
                    let _ = weak.update(cx, |p, cx| p.set_section(s, window, cx));
                }),
        )
    });
    v_flex()
        .w(scale::design(RAIL_WIDTH))
        .flex_none()
        .h_full()
        .p_2()
        .gap_1()
        .bg(theme.sidebar)
        .border_r_1()
        .border_color(theme.border)
        .children(rows)
        .into_any_element()
}

/// Wrapped, scrollable details for the active row. Copy uses the same
/// cached text as the keyboard command, without truncation.
pub(crate) fn detail_strip(
    selector: &'static str,
    row: Option<&PreparedRow>,
    position: SharedString,
    copy: Option<SharedString>,
    scroll: &gpui::ScrollHandle,
    cx: &mut Context<DiagnosticsPage>,
) -> AnyElement {
    let theme = cx.theme();
    // Scroll belongs to the full detail viewport; padding stays inside it.
    // Wrapping keeps long paths and messages readable instead of ellipsizing
    // the very text this inspector is meant to reveal.
    let lines = v_flex().gap_1().px_3().py_2().when_some(row, |el, row| {
        el.children(
            row.detail
                .iter()
                .cloned()
                .map(|line| div().w_full().whitespace_normal().child(line)),
        )
    });
    v_flex()
        .h_32()
        .flex_none()
        .min_w_0()
        .border_t_1()
        .border_color(theme.border)
        .debug_selector(move || selector.to_string())
        .child(
            h_flex()
                .px_3()
                .py_1()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(position),
                )
                .child(div().flex_1())
                .when_some(copy, |el, _| {
                    el.child(probed(
                        "diagnostics-copy",
                        Button::new("diagnostics-copy")
                            .ghost()
                            .small()
                            .icon(IconName::Copy)
                            .label("Copy")
                            .tooltip("Copy row details (Y)")
                            .on_click(cx.listener(|p, _, _, cx| p.copy_details(cx))),
                    ))
                }),
        )
        .child(
            div()
                .id(selector)
                .flex_1()
                .min_h_0()
                .font_family(fonts::MONO)
                .text_xs()
                .child(lines)
                .overflow_y_scroll()
                .track_scroll(scroll)
                .vertical_scrollbar(scroll),
        )
        .into_any_element()
}

/// The footer's keyboard hint for a section: each names that section's own
/// keys, since the same key (`r`, `tab`) does different things per section.
pub(crate) fn footer_hint(section: Section, filtering: bool, config_values: bool) -> &'static str {
    if filtering {
        "`enter` Keep filter   `escape` Cancel filter"
    } else {
        match section {
            Section::Sources => {
                "`[` / `]` Sections   `/` Filter   `up` / `down` Rows   `y` Copy   `escape` Back"
            }
            Section::Data => {
                "`[` / `]` Sections   `/` Filter   `space` / `enter` Expand   `r` Refresh   `y` Copy   `escape` Back"
            }
            Section::Reference => {
                "`[` / `]` Sections   `tab` Dataset   `/` Filter   `r` Poll now   `y` Copy   `escape` Back"
            }
            Section::Config if !config_values => {
                "`[` / `]` Sections   `tab` / `shift+tab` Views   `/` Filter   `y` Copy   `escape` Back"
            }
            Section::Config => {
                "`[` / `]` Sections   `tab` / `shift+tab` Views   `/` Filter   `space` / `enter` Expand   `y` Copy   `escape` Back"
            }
            Section::Log => {
                "`[` / `]` Sections   `/` Filter   `-` / `=` Levels   `t` Target   `f` Follow   `y` Copy   `escape` Back"
            }
            Section::Perf => "`[` / `]` Sections   `escape` Back",
        }
    }
}

/// Use the same keycap presentation as the shell's dialogs and menus.
pub(crate) fn footer(
    section: Section,
    filtering: bool,
    config_values: bool,
    cx: &Context<DiagnosticsPage>,
) -> AnyElement {
    let hint = footer_hint(section, filtering, config_values);
    div()
        .flex_none()
        .px_3()
        .py_1()
        .border_t_1()
        .border_color(cx.theme().border)
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .debug_selector(|| "diagnostics-footer".to_string())
        .child(kbd::marked(hint))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each section's hint names its own keys: Sources has no datasets and
    /// its `r` refreshes the catalog; Reference steps datasets and polls.
    #[test]
    fn sources_and_reference_footers_name_their_own_keys() {
        assert_eq!(
            footer_hint(Section::Sources, false, false),
            "`[` / `]` Sections   `/` Filter   `up` / `down` Rows   `y` Copy   `escape` Back"
        );
        assert_eq!(
            footer_hint(Section::Reference, false, false),
            "`[` / `]` Sections   `tab` Dataset   `/` Filter   `r` Poll now   `y` Copy   `escape` Back"
        );
    }
}
