//! Header, footer, and shorthand entry bar for the pricer tile.
//!
//! `prepare` formats header strings when state changes, including both fresh and stale
//! time labels. The tile compares the latest priced time with `stale_after` each frame;
//! `render` selects the prepared label using that result.

use crate::content::PricerSettings;
use crate::core::columns::{SHIFT, signed};
use crate::core::complete::Completion;
use crate::core::rollup::EffectiveChain;
use crate::core::sheet::{LineState, Sheet};
use crate::popup;
use crate::tile::{LOADING, PendingRemove, PricerTile};
use chrono::{DateTime, Utc};
use geode_core::clock::Clock;
use geode_shell::actions::ActionId;
use geode_shell::fonts;
use geode_shell::module::StackHandle;
use geode_shell::shell::aggregates::{self, AggregateCell};
use geode_shell::shell::chip::{Tone, chip_paint};
use geode_shell::shell::control::{self, PointerStates as _};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_shell::tips;
use geode_tile::confirm::{self, Confirm};
use geode_tile::notice::{self, Notice};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, ElementId, Entity, FontWeight, Hsla, IntoElement, SharedString, div, relative,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, Sizable as _, Theme, h_flex, v_flex};

pub(crate) const HEADER_HEIGHT: f32 = 22.0;
/// The entry bar's key context: `lib::init` reclaims `tab`/`shift-tab`
/// in it from gpui-component's focus cycling, for completion.
pub const ENTRY_CONTEXT: &str = "PricerEntry";
pub(crate) const FOOTER_HEIGHT: f32 = 20.0;
/// Labels preceding the active view and pricer names in the header.
pub(crate) const VIEW_LABEL: &str = "view";
pub(crate) const PRICER_LABEL: &str = "pricer";

pub(crate) struct HeaderInputs<'a> {
    pub sheet: &'a Sheet,
    /// The tile's notice, chosen in order from a pricing submission refusal or
    /// a transient notice. `None` lets `standing` show.
    pub notice: Option<SharedString>,
    /// A standing notice painted while `notice` is `None`: a refused frame
    /// scope (danger), else the view fallback (warning). `None` allows the
    /// missing-pricer notice.
    pub standing: Option<Notice>,
    /// Lines the frame's scope hides; `0` paints no chip.
    pub hidden: usize,
    /// `:unscoped`: the tile ignores the frame's scope.
    pub unscoped: bool,
    /// The grouping chain as resolved (`requested`) and what `pricer`
    /// keeps of it (`chain`); every level `chain` drops paints struck
    /// through.
    pub requested: &'a [String],
    pub chain: &'a EffectiveChain,
    /// `:group` / `:group slot N` pin the grouping: a `pinned` chip.
    pub pinned: bool,
    /// The armed `:rm` confirm's question.
    pub prompt: Option<SharedString>,
    /// The save state's own slot (a refused save, or a failed load that
    /// blocks saving). Separate from `notice` so a pricing notice can
    /// neither overwrite nor clear it; painted first, left of `notice`,
    /// and both may show.
    pub save: Option<SharedString>,
    pub settings: &'a PricerSettings,
    pub clock: Clock,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct HeaderModel {
    pub name: SharedString,
    pub view: SharedString,
    /// `spot +2.0%`, `vol -1.0`: the sheet-wide shifts, only those set,
    /// spelled as their cells spell them (`columns::signed`, one place).
    pub shifts: Vec<SharedString>,
    pub pricer: SharedString,
    /// `N pricing…` while any line is stale.
    pub pricing: Option<SharedString>,
    /// `N failed` while any line's last answer was a failure: the count
    /// the status column's reasons add up to, in danger text.
    pub failed: Option<SharedString>,
    pub last_priced: Option<DateTime<Utc>>,
    pub time: Option<SharedString>,
    pub time_stale: Option<SharedString>,
    /// `loading…` is a status (muted), a missing pricer a failure (danger
    /// text), everything else a warning.
    pub notice: Option<Notice>,
    /// The armed `:rm` confirm's question (see `HeaderInputs::prompt`).
    pub prompt: Option<SharedString>,
    /// The save state (see `HeaderInputs::save`), always a warning.
    pub save: Option<Notice>,
    /// `N hidden` while the frame's scope hides any line (muted).
    pub hidden: Option<SharedString>,
    /// The `unscoped` chip (warning tone) while `:unscoped` is on.
    pub unscoped: bool,
    /// The grouping chain in its written order, each level with whether
    /// `pricer` keeps it: kept levels read as text joined by `›`, a
    /// dropped one muted and struck through. Empty when nothing groups.
    pub chain: Vec<(SharedString, bool)>,
    /// The `pinned` chip (neutral) while the grouping is pinned.
    pub pinned: bool,
}

/// `requested` in order, each level marked kept or dropped. `kept` is a
/// subsequence of `requested` (`effective_chain` only drops), so a
/// greedy match assigns a repeated name's first occurrence to the kept
/// one and the repeat to the dropped.
fn chain_items(requested: &[String], chain: &EffectiveChain) -> Vec<(SharedString, bool)> {
    let mut kept = chain.kept.iter().peekable();
    requested
        .iter()
        .map(|name| {
            let is_kept = kept.peek().is_some_and(|k| *k == name);
            if is_kept {
                kept.next();
            }
            (SharedString::from(name.clone()), is_kept)
        })
        .collect()
}

pub(crate) fn prepare(i: HeaderInputs) -> HeaderModel {
    let s = i.sheet;
    let own = s.sheet_shift();
    let mut shifts = Vec::new();
    if let Some(v) = own.spot_pct {
        shifts.push(format!("spot {}%", signed(v, &SHIFT)).into());
    }
    if let Some(v) = own.vol_pts {
        shifts.push(format!("vol {}", signed(v, &SHIFT)).into());
    }
    let stale = s.stale_lines().count();
    let failed = (0..s.len())
        .filter(|r| s.is_line(*r) && matches!(s.state(*r), LineState::Failed(_)))
        .count();
    let last_priced = (0..s.len())
        .filter(|r| s.is_line(*r))
        .filter_map(|r| s.priced_at(r))
        .max();
    let time = last_priced.map(|t| i.clock.hms(t));
    let notice = match i.notice {
        Some(n) if n.as_ref() == LOADING => Some(Notice::status(n)),
        Some(n) => Some(Notice::warning(n)),
        None => i.standing.or_else(|| {
            i.settings.pricer_missing.then(|| {
                Notice::danger(format!(
                    "pricer '{}' is not built into this binary; set [pricing] adapter and restart",
                    i.settings.pricer
                ))
            })
        }),
    };
    HeaderModel {
        name: s.name.clone().into(),
        view: s.view.clone().into(),
        shifts,
        pricer: i.settings.pricer.clone().into(),
        pricing: (stale > 0).then(|| format!("{stale} pricing…").into()),
        failed: (failed > 0).then(|| format!("{failed} failed").into()),
        last_priced,
        time_stale: time.as_ref().map(|t| format!("{t} stale").into()),
        time: time.map(Into::into),
        notice,
        prompt: i.prompt,
        save: i.save.map(Notice::warning),
        hidden: (i.hidden > 0).then(|| format!("{} hidden", i.hidden).into()),
        unscoped: i.unscoped,
        chain: chain_items(i.requested, i.chain),
        pinned: i.pinned,
    }
}

impl HeaderModel {
    /// Prepared text used by header tests, including the fresh time label.
    /// Stale-time selection and the action trigger are rendered separately.
    #[cfg(test)]
    pub(crate) fn texts(&self) -> Vec<String> {
        let mut out = vec![
            self.name.to_string(),
            VIEW_LABEL.to_string(),
            self.view.to_string(),
        ];
        out.extend(self.shifts.iter().map(|s| s.to_string()));
        out.extend(self.chain.iter().map(|(name, kept)| {
            if *kept {
                name.to_string()
            } else {
                format!("~{name}~")
            }
        }));
        if self.pinned {
            out.push("pinned".to_string());
        }
        if self.unscoped {
            out.push("unscoped".to_string());
        }
        out.extend(self.hidden.iter().map(|s| s.to_string()));
        out.extend(self.save.iter().map(|s| s.text().to_string()));
        out.extend(self.notice.iter().map(|s| s.text().to_string()));
        out.extend(self.prompt.iter().map(|s| s.to_string()));
        out.extend(self.pricing.iter().map(|s| s.to_string()));
        out.extend(self.failed.iter().map(|s| s.to_string()));
        out.push(PRICER_LABEL.to_string());
        out.push(self.pricer.to_string());
        out.extend(self.time.iter().map(|s| s.to_string()));
        out
    }
}

/// Render a muted label beside its value, such as the active view name.
fn pair(label: &'static str, value: SharedString, muted: Hsla, text: Hsla) -> impl IntoElement {
    h_flex()
        .gap_1()
        .child(div().text_color(muted).child(label))
        .child(div().text_color(text).child(value))
}

/// Live rendering inputs: freshness, stack marker, menu trigger, and confirmation
/// focus. The tile retains their state; the header only installs their handlers.
pub(crate) struct HeaderChrome<'a> {
    /// Whether the last priced time is older than `stale_after` —
    /// computed by the caller per frame and passed in, so rendering never
    /// writes the prepared model.
    pub stale: bool,
    pub stack: Option<&'a StackHandle>,
    pub tile_id: TileId,
    pub tile: &'a Entity<PricerTile>,
    pub menu_open: bool,
    /// The `⋯` tooltip's selector, built once with the tile.
    pub menu_tip: SharedString,
    /// The armed `:rm` confirm: its prompt is painted through the confirm door.
    pub confirm: Option<&'a Confirm<PendingRemove>>,
    /// The sheet name's tooltip selector, built once with the tile.
    pub name_tip: SharedString,
    /// The open rename field, painted in the sheet name's place.
    pub rename: Option<&'a Entity<InputState>>,
    /// The open sheet picker, rendered by the tile, hung from the name.
    pub picker: Option<AnyElement>,
    /// The `unscoped` chip's tooltip selector, built once with the tile.
    pub unscoped_tip: SharedString,
}

/// The rename field's key context: `lib::init` reclaims `tab`/`shift-tab`
/// in it from gpui-component's focus cycling, which would otherwise take
/// the keyboard out of the field while it stays open.
pub const RENAME_CONTEXT: &str = "PricerRename";

/// The rename field's width: room for a long sheet name in the header's
/// text size without pushing the rest of the header about as it grows.
const RENAME_WIDTH: f32 = 160.0;

/// The sheet's name: a control whose single click toggles the sheet
/// picker and whose double-click opens the rename field
/// (`PricerTile::name_pressed`), or the rename field itself while it is
/// open. A press outside the field cancels it, never commits. The picker
/// hangs from the name's bottom-left edge.
fn sheet_name(name: SharedString, c: &mut HeaderChrome, tile_id: u64, theme: &Theme) -> AnyElement {
    let label: AnyElement = match c.rename {
        Some(input) => div()
            .w(scale::design(RENAME_WIDTH))
            .font_weight(FontWeight::BOLD)
            .text_color(theme.foreground)
            .debug_selector(|| "pricer-rename-field".into())
            .key_context(RENAME_CONTEXT)
            // `lib::init` unbinds `tab` here from gpui-component's focus
            // cycling; consuming it keeps the keyboard in the one-line field.
            .on_key_down(|event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "tab" {
                    cx.stop_propagation();
                }
            })
            .on_mouse_down_out({
                let tile = c.tile.clone();
                move |_, window, cx| tile.update(cx, |t, cx| t.close_rename_field(window, cx))
            })
            .child(Input::new(input).appearance(false).xsmall().w_full())
            .into_any_element(),
        None => div()
            .id(ElementId::NamedInteger(
                SharedString::new_static("pricer-sheet-name"),
                tile_id,
            ))
            .px_1()
            .rounded(theme.radius_tokens().sm)
            .font_weight(FontWeight::BOLD)
            .text_color(theme.foreground)
            .pointer_states(control::paint(
                theme,
                control::Rest::Bare,
                theme.background,
                theme.foreground,
            ))
            .debug_selector(|| "pricer-sheet-name".into())
            .capture_any_mouse_down({
                let tile = c.tile.clone();
                move |event, window, cx| {
                    if event.button != gpui::MouseButton::Left {
                        return;
                    }
                    tile.update(cx, |t, cx| t.name_pressed(event, window, cx));
                }
            })
            // Every press off the name, wherever it lands (this listener is
            // not hover-gated, so a press on a surface painted over the tile
            // counts too): the next press on the name follows no press of it.
            .on_mouse_down_out({
                let tile = c.tile.clone();
                move |_, _, cx| tile.update(cx, |t, _| t.name_press_elsewhere())
            })
            .tooltip(tips::tip_with(
                c.name_tip.clone(),
                SharedString::new_static("Sheets"),
                Some("pricer::open_sheet"),
                Some(SharedString::new_static("Double-click to rename")),
            ))
            .child(name)
            .into_any_element(),
    };
    div()
        .relative()
        .child(label)
        .when_some(c.picker.take(), |el, p| {
            el.child(div().absolute().left_0().bottom_0().child(p))
        })
        .into_any_element()
}

pub(crate) fn render(h: &HeaderModel, mut c: HeaderChrome, theme: &Theme) -> impl IntoElement {
    let muted = theme.muted_foreground;
    let warn = chip_paint(theme, Tone::WarningText).text;
    let danger = chip_paint(theme, Tone::DangerText).text;
    let chip = chip_paint(theme, Tone::Neutral);
    let warn_chip = chip_paint(theme, Tone::Warning);
    let stale = c.stale;
    let unscoped_tip = c.unscoped_tip.clone();
    let tile_id = c.tile_id.0;
    let name = sheet_name(h.name.clone(), &mut c, tile_id, theme);
    h_flex()
        .w_full()
        .h(scale::design(HEADER_HEIGHT))
        .items_center()
        .gap_3()
        .px_2()
        .text_sm()
        .text_color(muted)
        .border_b_1()
        .border_color(theme.border)
        .children(c.stack.and_then(|s| s.marker(theme, c.tile_id)))
        // The sheet's identity: its name and the view it is shown
        // through, one group (closer than the groups around it).
        .child(h_flex().gap_1p5().child(name).child(pair(
            VIEW_LABEL,
            h.view.clone(),
            muted,
            theme.foreground,
        )))
        .children(h.shifts.iter().map(|s| {
            div()
                .px_1()
                .rounded(theme.radius_tokens().sm)
                .when_some(chip.fill, |el, fill| el.bg(fill))
                .text_color(chip.text)
                .child(s.clone())
        }))
        // The grouping chain: kept levels as text joined by `›`, a level
        // `pricer` cannot group by muted and struck through; the neutral
        // `pinned` chip while `:group` pins it (the blotter's chip).
        .when(!h.chain.is_empty(), |el| {
            el.child(
                h_flex()
                    .gap_1()
                    .debug_selector(|| "pricer-chain".into())
                    .children(h.chain.iter().enumerate().flat_map(|(i, (name, kept))| {
                        let sep = (i > 0).then(|| div().text_color(muted).child("›"));
                        let level = div()
                            .id(ElementId::NamedInteger(
                                SharedString::new_static("pricer-chain-level"),
                                i as u64,
                            ))
                            .map(|el| {
                                if *kept {
                                    el.text_color(theme.foreground)
                                } else {
                                    el.text_color(muted)
                                        .line_through()
                                        .debug_selector(|| "pricer-chain-dropped".into())
                                }
                            })
                            .child(name.clone());
                        sep.into_iter()
                            .map(IntoElement::into_any_element)
                            .chain(std::iter::once(level.into_any_element()))
                    })),
            )
        })
        .when(h.pinned, |el| {
            el.child(
                div()
                    .debug_selector(|| "pricer-pinned".into())
                    .text_color(chip.text)
                    .when_some(chip.fill, |el, fill| el.bg(fill))
                    .px_1()
                    .rounded(theme.radius_tokens().sm)
                    .child("pinned"),
            )
        })
        // The frame's scope over this tile: detached (`:unscoped`), or
        // how many lines it hides. The blotter's chip and tooltip.
        .when(h.unscoped, |el| {
            el.child(
                div()
                    .id(ElementId::NamedInteger(
                        SharedString::new_static("pricer-unscoped"),
                        tile_id,
                    ))
                    .debug_selector(|| "pricer-unscoped".into())
                    .text_color(warn_chip.text)
                    .when_some(warn_chip.fill, |el, fill| el.bg(fill))
                    .px_1()
                    .rounded(theme.radius_tokens().sm)
                    .child("unscoped")
                    .tooltip(tips::tip_with(
                        unscoped_tip,
                        SharedString::new_static("Ignores the shared scope"),
                        None,
                        Some(SharedString::new_static(":unscoped re-attaches it")),
                    )),
            )
        })
        .when_some(h.hidden.clone(), |el, n| {
            el.child(
                div()
                    .text_color(muted)
                    .debug_selector(|| "pricer-hidden".into())
                    .child(n),
            )
        })
        .child(div().flex_1())
        .when_some(h.save.as_ref(), |el, n| {
            el.child(notice::render(n, theme).debug_selector(|| "pricer-save-notice".into()))
        })
        .when_some(h.notice.as_ref(), |el, n| {
            el.child(notice::render(n, theme).debug_selector(|| "pricer-notice".into()))
        })
        // The removal prompt owns the keyboard; the confirm door answers
        // every key on it before the shell root sees one, and paints its
        // Yes/No buttons.
        .when_some(h.prompt.as_ref().and(c.confirm), |el, pending| {
            el.child(confirm::prompt(
                pending,
                c.tile,
                move || format!("pricer-remove-confirm-{tile_id}"),
                theme,
            ))
        })
        .when_some(h.pricing.clone(), |el, p| el.child(p))
        .when_some(h.failed.clone(), |el, f| {
            el.child(
                div()
                    .text_color(danger)
                    .debug_selector(|| "pricer-failed".into())
                    .child(f),
            )
        })
        .child(pair(
            PRICER_LABEL,
            h.pricer.clone(),
            muted,
            theme.foreground,
        ))
        .when_some(
            if stale {
                h.time_stale.clone()
            } else {
                h.time.clone()
            },
            |el, t| el.child(div().when(stale, |el| el.text_color(warn)).child(t)),
        )
        // Toggle the action menu during capture, before its outside-click closer. A
        // bubble-phase toggle would see the menu already closed and reopen it on the
        // second click. Keep propagation so the shell's click-to-focus still runs.
        //
        // Use the same dispatch route as the menu key: cancel and blur any open field
        // before opening the menu on the current cursor row.
        .child(
            div()
                .id(ElementId::NamedInteger(
                    SharedString::new_static("pricer-menu-button"),
                    tile_id,
                ))
                .px_1p5()
                .rounded(theme.radius_tokens().sm)
                .border_1()
                .border_color(theme.border)
                .when(c.menu_open, |d| d.bg(theme.secondary))
                .text_color(muted)
                .when(!c.menu_open, |d| {
                    d.pointer_states(control::paint(
                        theme,
                        control::Rest::Bare,
                        theme.background,
                        muted,
                    ))
                })
                .child("⋯")
                .debug_selector(|| "pricer-menu-button".into())
                .capture_any_mouse_down({
                    let tile = c.tile.clone();
                    move |event, window, cx| {
                        if event.button != gpui::MouseButton::Left {
                            return;
                        }
                        tile.update(cx, |t, cx| {
                            t.dispatch(&ActionId("pricer::menu".to_string()), None, window, cx)
                        });
                    }
                })
                .tooltip(tips::tip_with(
                    c.menu_tip,
                    SharedString::new_static("Actions"),
                    Some("pricer::menu"),
                    None,
                )),
        )
}

/// Reserve footer height even without text so the table does not resize. Errors and
/// line failures use danger text at the status-line font size. With no text and a
/// live selection (`extent`), the same row paints the selection's extent and its
/// prepared position totals instead; a refusal always wins the row.
pub(crate) fn render_footer(
    text: Option<&SharedString>,
    extent: Option<&SharedString>,
    totals: &[AggregateCell],
    theme: &Theme,
) -> impl IntoElement {
    let row = h_flex()
        .w_full()
        .h(scale::design(FOOTER_HEIGHT))
        .px_2()
        .text_xs()
        .border_t_1()
        .border_color(theme.border)
        .debug_selector(|| "pricer-footer".into());
    match (text, extent) {
        // The extent reads from the left edge; the totals sit at the right,
        // under the risk columns they total, which the pricer's views place
        // towards the right of the sheet.
        (None, Some(extent)) => row
            .justify_between()
            .child(aggregates::strip(Some(extent), &[], &[], theme))
            .child(aggregates::strip(None, totals, &[], theme)),
        _ => row
            .text_color(chip_paint(theme, Tone::DangerText).text)
            .children(text.cloned()),
    }
}

/// Shorthand entry between the header and table. A muted label identifies the
/// insertion destination; the borderless field holds the draft. Submission errors
/// appear below that draft in danger text so the failure stays beside its input.
pub(crate) fn render_entry_bar(
    input: &Entity<InputState>,
    label: &SharedString,
    error: Option<&SharedString>,
    completion: &Completion,
    tile: &Entity<PricerTile>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let danger = chip_paint(theme, Tone::DangerText).text;
    let hint = completion.hint();
    let list = popup::render_entry_list(completion, tile, cx);
    v_flex()
        .relative()
        .w_full()
        .px_2()
        .py_1()
        .gap_0p5()
        .border_b_1()
        .border_color(theme.border)
        .debug_selector(|| "pricer-entry".into())
        .key_context(ENTRY_CONTEXT)
        .on_key_down({
            let tile = tile.clone();
            move |event: &gpui::KeyDownEvent, window, cx| {
                if tile.update(cx, |t, cx| t.entry_key(event, window, cx)) {
                    cx.stop_propagation();
                }
            }
        })
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .flex_shrink_0()
                        .max_w(relative(0.4))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .debug_selector(|| "pricer-entry-label".into())
                        .child(label.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(fonts::MONO)
                        .debug_selector(|| "pricer-entry-field".into())
                        .child(Input::new(input).appearance(false).w_full()),
                ),
        )
        // Always painted, a no-break space when there is no hint, so the
        // bar keeps its height and the table under it never jumps as the
        // caret crosses into the last slot.
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .font_family(fonts::MONO)
                .debug_selector(|| "pricer-entry-hint".into())
                .child(if hint.is_empty() {
                    SharedString::new_static("\u{a0}")
                } else {
                    hint.clone()
                }),
        )
        .when_some(error.cloned(), |el, e| {
            el.child(
                div()
                    .text_xs()
                    .text_color(danger)
                    .debug_selector(|| "pricer-entry-error".into())
                    .child(e),
            )
        })
        // Hangs from the bar's bottom edge over the table.
        .when_some(list, |el, list| {
            el.child(div().absolute().left_0().bottom_0().child(list))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::edit::Edit;
    use crate::core::sheet::OwnShifts;
    use crate::core::sheet::tests::{at, line, push, result, spx};
    use geode_core::pricing::OptionKind;

    fn settings(missing: bool) -> PricerSettings {
        PricerSettings {
            pricer: "vendor".into(),
            pricer_missing: missing,
            ..PricerSettings::default()
        }
    }

    #[test]
    fn the_header_names_the_sheet_view_shifts_and_pricing_count() {
        let mut s = Sheet::new("book");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s.apply(Edit::SetSheetShift(OwnShifts {
            spot_pct: Some(2.0),
            vol_pts: Some(-1.0),
        }))
        .unwrap();
        let h = prepare(HeaderInputs {
            sheet: &s,
            notice: None,
            standing: None,
            hidden: 0,
            unscoped: false,
            requested: &[],
            chain: &EffectiveChain::default(),
            pinned: false,
            prompt: None,
            save: None,
            settings: &settings(false),
            clock: Clock::utc(),
        });
        assert_eq!(
            h.texts(),
            vec![
                "book",
                "view",
                "vanilla",
                "spot +2.0%",
                "vol -1.0",
                "2 pricing…",
                "pricer",
                "vendor"
            ],
            "label/value pairs; a chip spells a shift as its cell does"
        );
        let id = s.id(0);
        let rev = s.revision(0);
        s.deliver(id, rev, Ok(result(1.0)), at(0));
        let h = prepare(HeaderInputs {
            sheet: &s,
            notice: None,
            standing: None,
            hidden: 0,
            unscoped: false,
            requested: &[],
            chain: &EffectiveChain::default(),
            pinned: false,
            prompt: None,
            save: None,
            settings: &settings(false),
            clock: Clock::utc(),
        });
        assert_eq!(h.pricing.as_deref(), Some("1 pricing…"));
        assert_eq!(h.failed, None);
        assert_eq!(h.last_priced, Some(at(0)));
        assert!(h.time.is_some());
        let id = s.id(1);
        let rev = s.revision(1);
        s.deliver(id, rev, Err("refused by the mock".into()), at(1));
        let h = prepare(HeaderInputs {
            sheet: &s,
            notice: None,
            standing: None,
            hidden: 0,
            unscoped: false,
            requested: &[],
            chain: &EffectiveChain::default(),
            pinned: false,
            prompt: None,
            save: None,
            settings: &settings(false),
            clock: Clock::utc(),
        });
        assert_eq!(h.pricing, None);
        assert_eq!(
            h.failed.as_deref(),
            Some("1 failed"),
            "a failure is counted, not only coloured"
        );
    }

    #[test]
    fn a_missing_pricer_speaks_only_when_nothing_else_does() {
        let s = Sheet::new("book");
        let h = prepare(HeaderInputs {
            sheet: &s,
            notice: None,
            standing: None,
            hidden: 0,
            unscoped: false,
            requested: &[],
            chain: &EffectiveChain::default(),
            pinned: false,
            prompt: None,
            save: None,
            settings: &settings(true),
            clock: Clock::utc(),
        });
        assert_eq!(
            h.notice.as_ref().map(|n| n.text().as_ref()),
            Some(
                "pricer 'vendor' is not built into this binary; set [pricing] adapter and restart"
            )
        );
        assert_eq!(
            h.notice.as_ref().map(Notice::tone),
            Some(notice::Tone::Danger),
            "a failure, not a warning"
        );
        let h = prepare(HeaderInputs {
            sheet: &s,
            notice: Some(LOADING.into()),
            standing: None,
            hidden: 0,
            unscoped: false,
            requested: &[],
            chain: &EffectiveChain::default(),
            pinned: false,
            prompt: None,
            save: None,
            settings: &settings(true),
            clock: Clock::utc(),
        });
        assert_eq!(
            h.notice.as_ref().map(|n| n.text().as_ref()),
            Some("loading…")
        );
        assert_eq!(
            h.notice.as_ref().map(Notice::tone),
            Some(notice::Tone::Status),
            "loading is a status"
        );
        let h = prepare(HeaderInputs {
            sheet: &s,
            notice: Some("sheet 'book' was not found; opened empty".into()),
            standing: None,
            hidden: 0,
            unscoped: false,
            requested: &[],
            chain: &EffectiveChain::default(),
            pinned: false,
            prompt: None,
            save: None,
            settings: &settings(true),
            clock: Clock::utc(),
        });
        assert_eq!(
            h.notice.as_ref().map(Notice::tone),
            Some(notice::Tone::Warning)
        );
    }
}
