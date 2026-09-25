//! The tile's one dense header row and its footer (line-pricer spec §8.3).
//! `prepare` formats everything once per change; `render` paints the
//! prepared strings and compares the last priced time against
//! `stale_after` (a compare, never a format, per frame).

use crate::content::PricerSettings;
use crate::core::columns::{SHIFT, signed};
use crate::core::sheet::{LineState, Sheet};
use crate::tile::{LOADING, PricerTile};
use chrono::{DateTime, Utc};
use geode_core::clock::Clock;
use geode_shell::actions::ActionId;
use geode_shell::module::StackHandle;
use geode_shell::shell::chip::{Tone, chip_paint};
use geode_shell::shell::control::{self, PointerStates as _};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_shell::tips;
use gpui::prelude::*;
use gpui::{ElementId, Entity, FontWeight, Hsla, IntoElement, SharedString, div};
use gpui_component::{Theme, h_flex};

pub(crate) const HEADER_HEIGHT: f32 = 22.0;
pub(crate) const FOOTER_HEIGHT: f32 = 20.0;
/// The muted labels ahead of the header's two values (`view vanilla`,
/// `pricer mock`), the market-data header's label/value pairs.
pub(crate) const VIEW_LABEL: &str = "view";
pub(crate) const PRICER_LABEL: &str = "pricer";

/// How the header paints its notice: `loading…` is a status (muted), a
/// missing pricer a failure (danger text), everything else a warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum NoticeTone {
    Status,
    #[default]
    Warning,
    Danger,
}

pub(crate) struct HeaderInputs<'a> {
    pub sheet: &'a Sheet,
    /// The tile's own notice, already chosen by precedence (a transient
    /// notice, then a view fallback); `None` lets a missing pricer speak.
    pub notice: Option<SharedString>,
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
    pub notice: Option<SharedString>,
    pub notice_tone: NoticeTone,
    /// The save state (see `HeaderInputs::save`).
    pub save: Option<SharedString>,
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
    let (notice, notice_tone) = match i.notice {
        Some(n) => {
            let tone = if n.as_ref() == LOADING {
                NoticeTone::Status
            } else {
                NoticeTone::Warning
            };
            (Some(n), tone)
        }
        None => (
            i.settings.pricer_missing.then(|| {
                format!(
                    "pricer '{}' is not built into this binary; set [pricing] adapter and restart",
                    i.settings.pricer
                )
                .into()
            }),
            NoticeTone::Danger,
        ),
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
        notice_tone,
        save: i.save,
    }
}

impl HeaderModel {
    /// Everything the header paints, for tests.
    #[cfg(test)]
    pub(crate) fn texts(&self) -> Vec<String> {
        let mut out = vec![
            self.name.to_string(),
            VIEW_LABEL.to_string(),
            self.view.to_string(),
        ];
        out.extend(self.shifts.iter().map(|s| s.to_string()));
        out.extend(self.save.iter().map(|s| s.to_string()));
        out.extend(self.notice.iter().map(|s| s.to_string()));
        out.extend(self.pricing.iter().map(|s| s.to_string()));
        out.extend(self.failed.iter().map(|s| s.to_string()));
        out.push(PRICER_LABEL.to_string());
        out.push(self.pricer.to_string());
        out.extend(self.time.iter().map(|s| s.to_string()));
        out
    }
}

/// A muted label and its value, parts of one reading (`view vanilla`,
/// the market-data header's label/value pairs).
fn pair(label: &'static str, value: SharedString, muted: Hsla, text: Hsla) -> impl IntoElement {
    h_flex()
        .gap_1()
        .child(div().text_color(muted).child(label))
        .child(div().text_color(text).child(value))
}

/// What `render` needs beyond the prepared model: the `⋯` trigger's
/// owner and state.
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
}

pub(crate) fn render(h: &HeaderModel, c: HeaderChrome, theme: &Theme) -> impl IntoElement {
    let muted = theme.muted_foreground;
    let warn = chip_paint(theme, Tone::WarningText).text;
    let danger = chip_paint(theme, Tone::DangerText).text;
    let chip = chip_paint(theme, Tone::Neutral);
    let notice_colour = match h.notice_tone {
        NoticeTone::Status => muted,
        NoticeTone::Warning => warn,
        NoticeTone::Danger => danger,
    };
    let stale = c.stale;
    let tile_id = c.tile_id.0;
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
        .child(
            h_flex()
                .gap_1p5()
                .child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.foreground)
                        .child(h.name.clone()),
                )
                .child(pair(VIEW_LABEL, h.view.clone(), muted, theme.foreground)),
        )
        .children(h.shifts.iter().map(|s| {
            div()
                .px_1()
                .rounded(theme.radius_tokens().sm)
                .when_some(chip.fill, |el, fill| el.bg(fill))
                .text_color(chip.text)
                .child(s.clone())
        }))
        .child(div().flex_1())
        .when_some(h.save.clone(), |el, n| {
            el.child(
                div()
                    .text_color(warn)
                    .debug_selector(|| "pricer-save-notice".into())
                    .child(n),
            )
        })
        .when_some(h.notice.clone(), |el, n| {
            el.child(
                div()
                    .text_color(notice_colour)
                    .debug_selector(|| "pricer-notice".into())
                    .child(n),
            )
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
        // `⋯` — the pointer's door onto the action menu, the click's own
        // form of `.` (market-data's trigger, copied): a persistent fill
        // while the menu is open, bare control states while closed.
        //
        // On the CAPTURE phase, and it does NOT stop propagation: the
        // menu's own `on_mouse_down_out` is a capture listener too and
        // would close an open menu before a bubble handler here could
        // ask whether one was open, so a second click would reopen it.
        // Capturing first lets this toggle decide; the shell's bubble
        // phase (click-to-focus) still runs, so a click on an unfocused
        // tile focuses it and `mode == menu` reaches the right tile.
        //
        // It enters through `dispatch`, exactly as `.` does, so with an
        // entry field or cell editor open it first closes them (blurring
        // before the drop, without committing) and the closers may re-sync
        // the cursor. That is benign and intended: the menu acts on the
        // cursor row, and a click here must not leave a live field under
        // an open menu any more than the key would.
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

/// Always laid out, text or not, so the table's height never changes
/// (spec §8.3). Every footer line is a user error or a failure, so it
/// paints in danger text, at the status-line size the blotter's footer
/// and the timeseries notice line use.
pub(crate) fn render_footer(text: Option<&SharedString>, theme: &Theme) -> impl IntoElement {
    h_flex()
        .w_full()
        .h(scale::design(FOOTER_HEIGHT))
        .px_2()
        .text_xs()
        .border_t_1()
        .border_color(theme.border)
        .text_color(chip_paint(theme, Tone::DangerText).text)
        .debug_selector(|| "pricer-footer".into())
        .children(text.cloned())
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
            save: None,
            settings: &settings(true),
            clock: Clock::utc(),
        });
        assert_eq!(
            h.notice.as_deref(),
            Some(
                "pricer 'vendor' is not built into this binary; set [pricing] adapter and restart"
            )
        );
        assert_eq!(
            h.notice_tone,
            NoticeTone::Danger,
            "a failure, not a warning"
        );
        let h = prepare(HeaderInputs {
            sheet: &s,
            notice: Some(LOADING.into()),
            save: None,
            settings: &settings(true),
            clock: Clock::utc(),
        });
        assert_eq!(h.notice.as_deref(), Some("loading…"));
        assert_eq!(h.notice_tone, NoticeTone::Status, "loading is a status");
        let h = prepare(HeaderInputs {
            sheet: &s,
            notice: Some("sheet 'book' was not found; opened empty".into()),
            save: None,
            settings: &settings(true),
            clock: Clock::utc(),
        });
        assert_eq!(h.notice_tone, NoticeTone::Warning);
    }
}
