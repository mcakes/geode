//! The tile's header and footer: what it reads (the underlying, the
//! coordinate), one chip per loaded kind, the difference chip, the fixed
//! differences y-axis chip while one is set, the link chips, the datasets'
//! health and the action menu's `\u{22ef}`; below the chart, the first
//! notice and the key hints.
//!
//! [`HeaderModel::prepare`] and [`footer_notice`] format their text when
//! the tile's state changes and [`footer_hints`] when the chords do; paint
//! clones prepared strings and formats nothing. A chip click dispatches the
//! same action its key does, so the pointer and the keyboard cannot
//! disagree.

use geode_chart::core::scale::{LinearScale, fmt_percent, fmt_tick, unsigned_zero};
use geode_chart::xy::YFormat;
use geode_core::link::{DraftMark, Group};
use geode_shell::actions::ActionId;
use geode_shell::keymap::{Keystroke, Modifiers, parse_binding};
use geode_shell::module::{CloseHandle, StackHandle};
use geode_shell::shell::chip::{Tone, chip_paint};
use geode_shell::shell::control::{self, PointerStates as _};
use geode_shell::shell::{kbd, scale};
use geode_shell::tiling::TileId;
use geode_shell::tips::{self, Chords, chord_for};
use geode_tile::header::{Cluster, HealthChip, MenuTrigger, Mode, TileLinks};
use geode_tile::notice::{Notice, Tone as NoticeTone};
use gpui::prelude::*;
use gpui::{App, Div, ElementId, Entity, MouseButton, MouseDownEvent, SharedString, div};
use gpui_component::{Theme, h_flex, v_flex};

use crate::core::build::{DIFF_AXIS, Y_FORMAT};
use crate::core::menu::FIX_DIFF_Y;
use crate::core::model::{Kind, Loaded, Pair, State};
use crate::tile::VolsliceTile;

/// The footer's hint row height at the design rem, on the shell's scale.
pub(crate) const FOOTER_HEIGHT: f32 = 20.0;

/// What the header shows while the tile reads no underlying.
pub(crate) const NO_UNDERLYING: &str = "no underlying";

/// The footer's empty state: the tile reads no underlying, or the group it
/// follows names none or several. Nothing failed.
pub(crate) fn no_underlying(following: Option<Group>) -> String {
    match following {
        Some(g) => format!("{NO_UNDERLYING} in {}", g.letter()),
        None => NO_UNDERLYING.to_string(),
    }
}

fn is_empty_state(text: &str) -> bool {
    text == NO_UNDERLYING
        || Group::ALL
            .into_iter()
            .any(|g| text == no_underlying(Some(g)))
}

/// One loaded kind's chip.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct KindChip {
    pub kind: Kind,
    /// The digit that toggles it, painted as a key.
    pub digit: Keystroke,
    /// `cvi`, `chain`, or the draft's label with its mark's word.
    pub label: SharedString,
    pub hidden: bool,
    /// The registered toggle the click dispatches.
    pub action: &'static str,
}

/// Everything the header's left side paints, prepared once per change.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HeaderModel {
    pub underlying: SharedString,
    pub coordinate: SharedString,
    pub chips: Vec<KindChip>,
    /// `diff: <minuend> \u{2212} <subtrahend>`, one or two shown pairs in
    /// their order and parted by commas; `diff \u{00b7} N pairs` past two,
    /// so the chip stays short; `diff` with none shown.
    pub diff: SharedString,
    pub diff_set: bool,
    /// `y \u{2212}2%\u{2026}2%` while the differences axis is fixed, in
    /// the axis's own format; `None` while it autoscales.
    pub ylim: Option<SharedString>,
}

/// The `\u{22ef}` button's per-tile selector, formatted once, and whether
/// its menu is up.
pub(crate) struct MenuButton {
    pub selector: SharedString,
    pub open: bool,
}

/// The fixed differences domain as the chip names it, in the axis's tick
/// format: each end with the fewest decimals that print it exactly (a
/// typed `-2% 2%` reads `\u{2212}2%\u{2026}2%`), and no finer than the
/// step a ten-tick axis over the range would take (a frozen autoscaled
/// domain reads to that step). Never a signed zero; the labels' minus.
pub(crate) fn ylim_text((lo, hi): (f64, f64)) -> String {
    let format = Y_FORMAT[DIFF_AXIS.index()];
    // In the units the label prints: a percent axis reads a hundredth.
    let unit = match format {
        YFormat::Percent => 100.0,
        YFormat::Plain => 1.0,
    };
    let finest = LinearScale::nice_step((hi - lo) * unit, 10);
    let end = |v: f64| {
        let shown = v * unit;
        let step = (0..6)
            .map(|d| 10f64.powi(-d))
            .find(|step| {
                let at = (shown / step).round() * step;
                (shown - at).abs() <= 1e-9 * shown.abs().max(1.0)
            })
            .map_or(finest, |step| step.max(finest));
        let text = match format {
            YFormat::Percent => fmt_percent(v, step / unit),
            YFormat::Plain => fmt_tick(v, step),
        };
        unsigned_zero(text).replacen('-', "\u{2212}", 1)
    };
    format!("y {}\u{2026}{}", end(lo), end(hi))
}

const KIND_ACTIONS: [&str; 3] = ["volslice::kind_1", "volslice::kind_2", "volslice::kind_3"];

const DIGITS: [&str; 3] = ["1", "2", "3"];

/// The draft chip's label: `cvi draft`, with the mark's word unless it is
/// a live edit.
pub(crate) fn draft_label(mark: DraftMark) -> String {
    let kind = Kind::Draft.label();
    match mark.label() {
        Some(word) => format!("{kind} \u{00b7} {word}"),
        None => kind.to_string(),
    }
}

impl HeaderModel {
    pub(crate) fn prepare(underlying: Option<&str>, state: &State, loaded: &Loaded) -> Self {
        let chips = loaded
            .kinds()
            .into_iter()
            .map(|kind| {
                let i = kind.index();
                let label = match (kind, &loaded.draft) {
                    (Kind::Draft, Some((_, mark))) => draft_label(*mark).into(),
                    _ => SharedString::new_static(kind.label()),
                };
                KindChip {
                    kind,
                    digit: Keystroke {
                        mods: Modifiers::NONE,
                        key: DIGITS[i].to_string(),
                    },
                    label,
                    hidden: state.hidden.contains(&kind),
                    action: KIND_ACTIONS[i],
                }
            })
            .collect();
        let (diff, diff_set) = if state.diffs.is_empty() {
            (SharedString::new_static("diff"), false)
        } else {
            (diff_text(&state.diffs).into(), true)
        };
        HeaderModel {
            ylim: state.diff_ylim.map(|r| ylim_text(r).into()),
            underlying: underlying
                .map(|u| SharedString::from(u.to_string()))
                .unwrap_or(SharedString::new_static(NO_UNDERLYING)),
            coordinate: SharedString::new_static(state.coordinate.name()),
            chips,
            diff,
            diff_set,
        }
    }
}

/// The most pairs the diff chip names one by one.
const DIFF_CHIP_NAMED: usize = 2;

fn diff_text(pairs: &[Pair]) -> String {
    if pairs.len() > DIFF_CHIP_NAMED {
        return format!("diff \u{00b7} {} pairs", pairs.len());
    }
    let labels: Vec<String> = pairs.iter().map(|p| p.label()).collect();
    format!("diff: {}", labels.join(", "))
}

/// The footer's notice: the first notice, and how many more stand behind
/// it. `None` with no notice. An empty state alone is said in the muted
/// status tone the sibling modules paint an empty state in; anything else
/// is a failure or a refusal, in the danger tone.
pub(crate) fn footer_notice<'a>(
    notices: impl Iterator<Item = &'a String> + Clone,
) -> Option<Notice> {
    let tone = if notices.clone().all(|n| is_empty_state(n)) {
        NoticeTone::Status
    } else {
        NoticeTone::Danger
    };
    let mut notices = notices;
    let first = notices.next()?;
    let more = notices.count();
    let text: SharedString = if more == 0 {
        first.clone().into()
    } else {
        format!("{first} (+{more} more)").into()
    };
    Some(Notice::new(text, tone))
}

/// The footer's hint row, in order: each entry's actions (painted as keys,
/// parted by a slash) and its word. Every word but the last carries its
/// ` \u{00b7}` separator, so paint formats nothing.
const FOOTER_HINTS: &[(&[(&str, &str)], &str)] = &[
    (
        &[("volslice::strip_down", "j"), ("volslice::strip_up", "k")],
        "expiry",
    ),
    (&[("volslice::solo", "space")], "solo"),
    (&[("volslice::toggle_expiry", "shift+space")], "add"),
    (&[("volslice::coordinate", "x")], "coord"),
    (&[("volslice::density", "shift+d")], "dens"),
    (&[("volslice::diff", "d")], "diff"),
];

/// One footer hint: its bindings, then its word.
pub(crate) type FooterHint = (Vec<Vec<Keystroke>>, SharedString);

/// The hints with each verb's live chord, or its shipped key where the
/// keymap has none. Resolved on a `Chords` change, never per frame.
pub(crate) fn footer_hints(cx: &App) -> Vec<FooterHint> {
    let empty = Vec::new();
    let bindings = cx
        .try_global::<Chords>()
        .map(|c| c.0.as_slice())
        .unwrap_or(&empty);
    let last = FOOTER_HINTS.len() - 1;
    FOOTER_HINTS
        .iter()
        .enumerate()
        .map(|(i, (actions, word))| {
            let keys = actions
                .iter()
                .map(|(action, shipped)| {
                    chord_for(bindings, action).unwrap_or_else(|| {
                        parse_binding(shipped, Modifiers::NONE)
                            .expect("shipped footer keys are valid")
                    })
                })
                .collect();
            let word = if i == last {
                SharedString::new_static(word)
            } else {
                format!("{word} \u{00b7}").into()
            };
            (keys, word)
        })
        .collect()
}

/// A clickable header chip: bare at rest, the shell's pointer states, and
/// a left press that dispatches `action` through the tile's own door. The
/// press is left to propagate so the shell still focuses the tile.
fn chip_control(
    id: ElementId,
    theme: &Theme,
    paint: control::ControlPaint,
    tile: &Entity<VolsliceTile>,
    action: &'static str,
) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_1()
        .rounded(theme.radius)
        .pointer_states(paint)
        .on_mouse_down(MouseButton::Left, {
            let tile = tile.clone();
            move |_: &MouseDownEvent, window, cx| {
                tile.update(cx, |t, cx| {
                    t.dispatch(&ActionId(action.to_string()), None, window, cx);
                });
            }
        })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_header(
    h: &HeaderModel,
    theme: &Theme,
    tile: &Entity<VolsliceTile>,
    tile_id: u64,
    stack: Option<&StackHandle>,
    close: Option<&CloseHandle>,
    health: Option<&HealthChip>,
    mode: Mode,
    links: TileLinks,
    menu: MenuButton,
) -> Div {
    let bare = control::paint(
        theme,
        control::Rest::Bare,
        theme.background,
        theme.muted_foreground,
    );
    let mut left = h_flex().items_center().gap_2().child(
        div()
            .px_1p5()
            .rounded(theme.radius_tokens().sm)
            .bg(theme.secondary)
            .text_color(theme.secondary_foreground)
            .text_xs()
            .child("Vol slice"),
    );
    left = left
        .child(
            div()
                .text_color(theme.foreground)
                .debug_selector(move || format!("volslice-underlying-{tile_id}"))
                .child(h.underlying.clone()),
        )
        .child(div().child(h.coordinate.clone()));
    for chip in &h.chips {
        let label = chip.label.clone();
        let kind = chip.kind.label();
        let hidden = chip.hidden;
        let el = chip_control(
            ElementId::Name(SharedString::new_static(kind)),
            theme,
            bare,
            tile,
            chip.action,
        )
        .debug_selector(move || format!("volslice-kind-{tile_id}-{kind}-{label}"))
        .text_color(if hidden {
            theme.muted_foreground
        } else {
            theme.foreground
        })
        // A hidden kind stays in the header: its digit is a toggle, and a
        // chip that vanished would leave nothing to press again.
        .when(hidden, |d| d.opacity(0.5).line_through())
        .tooltip(tips::tip(
            "tip-volslice-kind",
            if hidden { "Show" } else { "Hide" },
            Some(chip.action),
            None,
        ))
        .child(kbd::chip(&chip.digit))
        .child(chip.label.clone());
        left = left.child(el);
    }
    let diff_paint = chip_paint(theme, Tone::Neutral);
    let diff_label = h.diff.clone();
    left = left.child(
        chip_control(
            ElementId::Name(SharedString::new_static("volslice-diff-chip")),
            theme,
            if h.diff_set {
                control::for_chip(theme, &diff_paint, theme.background)
            } else {
                bare
            },
            tile,
            "volslice::diff",
        )
        .debug_selector(move || format!("volslice-diff-chip-{tile_id}-{diff_label}"))
        .when(h.diff_set, |d| {
            d.text_color(diff_paint.text)
                .when_some(diff_paint.fill, |d, fill| d.bg(fill))
        })
        .tooltip(tips::tip(
            "tip-volslice-diff",
            "Difference",
            Some("volslice::diff"),
            None,
        ))
        .child(h.diff.clone()),
    );
    // The fixed differences axis: a set chip whose click frees it, as the
    // action menu's `Fix diff y-axis` row and `:ylim off` do.
    if let Some(ylim) = &h.ylim {
        let label = ylim.clone();
        left = left.child(
            chip_control(
                ElementId::Name(SharedString::new_static("volslice-ylim-chip")),
                theme,
                control::for_chip(theme, &diff_paint, theme.background),
                tile,
                FIX_DIFF_Y,
            )
            .debug_selector(move || format!("volslice-ylim-chip-{tile_id}-{label}"))
            .text_color(diff_paint.text)
            .when_some(diff_paint.fill, |d, fill| d.bg(fill))
            .tooltip(tips::tip(
                "tip-volslice-ylim",
                "Free the difference y-axis",
                Some(FIX_DIFF_Y),
                None,
            ))
            .child(ylim.clone()),
        );
    }
    let mut cluster = Cluster::new(TileId(tile_id));
    cluster.close = close.cloned();
    cluster.mode = mode;
    cluster.links = links;
    cluster.health = health;
    cluster.menu = Some(MenuTrigger {
        id: ElementId::Name(SharedString::new_static("volslice-menu-button")),
        selector: menu.selector,
        tip_selector: SharedString::new_static("tip-volslice-menu"),
        action: "volslice::menu",
        open: menu.open,
        on_press: std::rc::Rc::new({
            let tile = tile.clone();
            move |window, cx| tile.update(cx, |t, cx| t.toggle_menu(window, cx))
        }),
    });
    geode_tile::header::frame(
        stack.and_then(|s| s.marker(theme, TileId(tile_id))),
        left,
        cluster,
        theme,
    )
    .debug_selector(move || format!("volslice-header-{tile_id}"))
}

/// The footer: the notice line when there is one, then the hint row.
pub(crate) fn render_footer(
    notice: Option<&Notice>,
    hints: &[FooterHint],
    theme: &Theme,
    tile_id: u64,
) -> Div {
    let hint_row = h_flex()
        .w_full()
        .h(scale::design(FOOTER_HEIGHT))
        .items_center()
        .gap_1()
        .px_2()
        .text_xs()
        .text_color(theme.muted_foreground)
        .overflow_hidden()
        .children(hints.iter().map(|(bindings, word)| {
            let mut el = h_flex().gap_1().items_center().flex_shrink_0();
            for (i, keys) in bindings.iter().enumerate() {
                if i > 0 {
                    el = el.child("/");
                }
                el = el.child(kbd::binding(keys));
            }
            el.child(word.clone())
        }));
    v_flex()
        .w_full()
        .border_t_1()
        .border_color(theme.border)
        .when_some(notice, |el, n| {
            el.child(
                h_flex()
                    .w_full()
                    .px_2()
                    .text_xs()
                    .debug_selector(move || format!("volslice-notice-{tile_id}"))
                    .child(geode_tile::notice::render(n, theme).truncate()),
            )
        })
        .child(hint_row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::tests::fixture;

    #[test]
    fn the_header_names_each_loaded_kind_with_its_digit_and_the_mark() {
        let mut loaded = fixture();
        loaded.draft = loaded.draft.map(|(rows, _)| (rows, DraftMark::Sent));
        let mut state = State::default();
        state.toggle_kind(Kind::Chain);
        state.diffs = vec![
            Pair::new(Kind::Draft, Kind::Cvi).unwrap(),
            Pair::new(Kind::Cvi, Kind::Chain).unwrap(),
        ];
        let h = HeaderModel::prepare(Some("SPX.Z"), &state, &loaded);
        assert_eq!(h.underlying.as_ref(), "SPX.Z");
        assert_eq!(h.coordinate.as_ref(), "moneyness");
        let labels: Vec<_> = h.chips.iter().map(|c| c.label.to_string()).collect();
        assert_eq!(labels, ["cvi", "cvi draft \u{00b7} sent", "chain"]);
        assert_eq!(h.chips[2].digit.key, "3");
        assert!(h.chips[2].hidden && !h.chips[0].hidden);
        assert_eq!(
            h.diff.as_ref(),
            "diff: cvi draft \u{2212} cvi, cvi \u{2212} chain",
            "every shown pair, in order"
        );
        state
            .diffs
            .push(Pair::new(Kind::Draft, Kind::Chain).unwrap());
        let h3 = HeaderModel::prepare(Some("SPX.Z"), &state, &fixture());
        assert_eq!(
            (h3.diff.as_ref(), h3.diff_set),
            ("diff \u{00b7} 3 pairs", true),
            "past two pairs, a count"
        );
        assert_eq!(h3.ylim, None, "autoscaled: no chip");
        state.diff_ylim = Some((-0.02, 0.02));
        let h = HeaderModel::prepare(Some("SPX.Z"), &state, &fixture());
        assert_eq!(
            h.ylim.as_ref().map(|s| s.as_ref()),
            Some("y \u{2212}2%\u{2026}2%")
        );
        loaded.draft = None;
        let h = HeaderModel::prepare(None, &State::default(), &loaded);
        assert_eq!(h.underlying.as_ref(), NO_UNDERLYING);
        assert_eq!(h.chips.len(), 2, "only loaded kinds");
        assert_eq!((h.diff.as_ref(), h.diff_set), ("diff", false));
    }

    /// The chip reads at the step its range needs, with the labels' minus
    /// and no signed zero.
    #[test]
    fn the_ylim_chip_reads_in_the_axis_format() {
        assert_eq!(ylim_text((-0.02, 0.02)), "y \u{2212}2%\u{2026}2%");
        assert_eq!(ylim_text((-0.0123, 0.0234)), "y \u{2212}1.2%\u{2026}2.3%");
        assert_eq!(ylim_text((-0.00001, 0.05)), "y 0%\u{2026}5%");
        assert_eq!(ylim_text((-0.015, 0.03)), "y \u{2212}1.5%\u{2026}3%");
    }

    #[test]
    fn the_footer_notice_counts_the_rest() {
        let n = ["a".to_string(), "b".to_string(), "c".to_string()];
        let text = |n: Option<Notice>| n.map(|n| n.text().to_string());
        assert_eq!(
            text(footer_notice(n.iter())).as_deref(),
            Some("a (+2 more)")
        );
        assert_eq!(text(footer_notice(n[..1].iter())).as_deref(), Some("a"));
        assert_eq!(footer_notice([].iter()), None);
    }

    /// An empty state is not a failure: alone it is the status tone; any
    /// other notice beside it, or alone, is the danger tone.
    #[test]
    fn an_empty_state_is_muted_and_a_failure_is_danger() {
        let tone = |n: &[String]| footer_notice(n.iter()).map(|n| n.tone());
        assert_eq!(tone(&[no_underlying(None)]), Some(NoticeTone::Status));
        assert_eq!(
            tone(&[no_underlying(Some(Group::C))]),
            Some(NoticeTone::Status)
        );
        assert_eq!(
            tone(&[no_underlying(Some(Group::A)), "refused".to_string()]),
            Some(NoticeTone::Danger)
        );
        assert_eq!(
            tone(&["no cvi curve at 2026-10-16: x".to_string()]),
            Some(NoticeTone::Danger)
        );
    }

    #[gpui::test]
    fn the_hints_fall_back_to_the_shipped_keys(cx: &mut gpui::TestAppContext) {
        let hints = cx.update(|cx| footer_hints(cx));
        let words: Vec<_> = hints.iter().map(|(_, w)| w.to_string()).collect();
        assert_eq!(
            words,
            [
                "expiry \u{00b7}",
                "solo \u{00b7}",
                "add \u{00b7}",
                "coord \u{00b7}",
                "dens \u{00b7}",
                "diff"
            ]
        );
        assert_eq!(hints[0].0.len(), 2, "j and k");
        assert_eq!(hints[0].0[0][0].key, "j");
    }
}
