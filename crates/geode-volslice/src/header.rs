//! The tile's header and footer: what it reads (the underlying, the
//! coordinate), one chip per loaded kind, the difference chip, the link
//! chips and the datasets' health; below the chart, the first notice and
//! the key hints.
//!
//! [`HeaderModel::prepare`] and [`footer_notice`] format their text when
//! the tile's state changes and [`footer_hints`] when the chords do; paint
//! clones prepared strings and formats nothing. A chip click dispatches the
//! same action its key does, so the pointer and the keyboard cannot
//! disagree.

use geode_core::link::DraftMark;
use geode_shell::actions::ActionId;
use geode_shell::keymap::{Keystroke, Modifiers, parse_binding};
use geode_shell::module::StackHandle;
use geode_shell::shell::chip::{Tone, chip_paint};
use geode_shell::shell::control::{self, PointerStates as _};
use geode_shell::shell::{kbd, scale};
use geode_shell::tiling::TileId;
use geode_shell::tips::{self, Chords, chord_for};
use geode_tile::header::{Cluster, HealthChip, LinkChip, Mode};
use gpui::prelude::*;
use gpui::{App, Div, ElementId, Entity, MouseButton, MouseDownEvent, SharedString, div};
use gpui_component::{Theme, h_flex, v_flex};

use crate::core::model::{Kind, Loaded, Pair, State};
use crate::tile::VolsliceTile;

/// The footer's hint row height at the design rem, on the shell's scale.
pub(crate) const FOOTER_HEIGHT: f32 = 20.0;

/// What the header shows while the tile reads no underlying.
pub(crate) const NO_UNDERLYING: &str = "no underlying";

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
    /// `diff: <minuend> \u{2212} <subtrahend>`, or `diff` with no pair set.
    pub diff: SharedString,
    pub diff_set: bool,
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
        let (diff, diff_set) = match state.diff {
            Some(p) => (diff_text(p).into(), true),
            None => (SharedString::new_static("diff"), false),
        };
        HeaderModel {
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

fn diff_text(p: Pair) -> String {
    format!("diff: {}", p.label())
}

/// The footer's notice: the first notice, and how many more stand behind
/// it. `None` with no notice.
pub(crate) fn footer_notice<'a>(
    mut notices: impl Iterator<Item = &'a String>,
) -> Option<SharedString> {
    let first = notices.next()?;
    let more = notices.count();
    Some(if more == 0 {
        first.clone().into()
    } else {
        format!("{first} (+{more} more)").into()
    })
}

/// The footer's hint row, in order: each entry's actions (painted as keys,
/// parted by a slash) and its word. Every word but the last carries its
/// ` \u{00b7}` separator, so paint formats nothing.
const FOOTER_HINTS: &[(&[(&str, &str)], &str)] = &[
    (
        &[("volslice::strip_down", "j"), ("volslice::strip_up", "k")],
        "expiry",
    ),
    (&[("volslice::solo", "enter")], "solo"),
    (&[("volslice::toggle_expiry", "space")], "toggle"),
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
    health: Option<&HealthChip>,
    mode: Mode,
    links: [Option<LinkChip>; 2],
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
    let mut cluster = Cluster::new(TileId(tile_id));
    cluster.mode = mode;
    cluster.links = links;
    cluster.health = health;
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
    notice: Option<&SharedString>,
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
                    .child(
                        geode_tile::notice::paint(n, geode_tile::notice::Tone::Danger, theme)
                            .truncate(),
                    ),
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
        state.diff = Pair::new(Kind::Draft, Kind::Cvi);
        let h = HeaderModel::prepare(Some("SPX.Z"), &state, &loaded);
        assert_eq!(h.underlying.as_ref(), "SPX.Z");
        assert_eq!(h.coordinate.as_ref(), "moneyness");
        let labels: Vec<_> = h.chips.iter().map(|c| c.label.to_string()).collect();
        assert_eq!(labels, ["cvi", "cvi draft \u{00b7} sent", "chain"]);
        assert_eq!(h.chips[2].digit.key, "3");
        assert!(h.chips[2].hidden && !h.chips[0].hidden);
        assert_eq!(h.diff.as_ref(), "diff: cvi draft \u{2212} cvi");
        loaded.draft = None;
        let h = HeaderModel::prepare(None, &State::default(), &loaded);
        assert_eq!(h.underlying.as_ref(), NO_UNDERLYING);
        assert_eq!(h.chips.len(), 2, "only loaded kinds");
        assert_eq!((h.diff.as_ref(), h.diff_set), ("diff", false));
    }

    #[test]
    fn the_footer_notice_counts_the_rest() {
        let n = ["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(footer_notice(n.iter()).as_deref(), Some("a (+2 more)"));
        assert_eq!(footer_notice(n[..1].iter()).as_deref(), Some("a"));
        assert_eq!(footer_notice([].iter()), None);
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
                "toggle \u{00b7}",
                "coord \u{00b7}",
                "dens \u{00b7}",
                "diff"
            ]
        );
        assert_eq!(hints[0].0.len(), 2, "j and k");
        assert_eq!(hints[0].0[0][0].key, "j");
    }
}
