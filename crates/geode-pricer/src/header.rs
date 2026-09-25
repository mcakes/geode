//! The tile's one dense header row and its footer (line-pricer spec §8.3).
//! `prepare` formats everything once per change; `render` paints the
//! prepared strings and compares the last priced time against
//! `stale_after` (a compare, never a format, per frame).

use crate::content::PricerSettings;
use crate::core::sheet::Sheet;
use chrono::{DateTime, Utc};
use geode_core::clock::Clock;
use geode_shell::module::StackHandle;
use geode_shell::shell::chip::{Tone, chip_paint};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use gpui::prelude::*;
use gpui::{FontWeight, IntoElement, SharedString, div};
use gpui_component::{Theme, h_flex};

pub(crate) const HEADER_HEIGHT: f32 = 22.0;
pub(crate) const FOOTER_HEIGHT: f32 = 20.0;

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
    /// `spot +2%`, `vol −1`: the sheet-wide shifts, only those set.
    pub shifts: Vec<SharedString>,
    pub pricer: SharedString,
    /// `N pricing…` while any line is stale.
    pub pricing: Option<SharedString>,
    pub last_priced: Option<DateTime<Utc>>,
    pub time: Option<SharedString>,
    pub time_stale: Option<SharedString>,
    pub notice: Option<SharedString>,
    /// The save state (see `HeaderInputs::save`).
    pub save: Option<SharedString>,
}

/// `+2` / `−1.5`: a shift is a delta, so its sign is the information; the
/// minus is U+2212 (spec §8.3's `vol −1`).
fn signed(v: f64) -> String {
    if v < 0.0 {
        format!("\u{2212}{}", -v)
    } else {
        format!("+{v}")
    }
}

pub(crate) fn prepare(i: HeaderInputs) -> HeaderModel {
    let s = i.sheet;
    let own = s.sheet_shift();
    let mut shifts = Vec::new();
    if let Some(v) = own.spot_pct {
        shifts.push(format!("spot {}%", signed(v)).into());
    }
    if let Some(v) = own.vol_pts {
        shifts.push(format!("vol {}", signed(v)).into());
    }
    let stale = s.stale_lines().count();
    let last_priced = (0..s.len())
        .filter(|r| s.is_line(*r))
        .filter_map(|r| s.priced_at(r))
        .max();
    let time = last_priced.map(|t| i.clock.hms(t));
    let notice = i.notice.or_else(|| {
        i.settings.pricer_missing.then(|| {
            format!(
                "pricer \"{}\" is not built into this binary",
                i.settings.pricer
            )
            .into()
        })
    });
    HeaderModel {
        name: s.name.clone().into(),
        view: s.view.clone().into(),
        shifts,
        pricer: i.settings.pricer.clone().into(),
        pricing: (stale > 0).then(|| format!("{stale} pricing…").into()),
        last_priced,
        time_stale: time.as_ref().map(|t| format!("{t} stale").into()),
        time: time.map(Into::into),
        notice,
        save: i.save,
    }
}

impl HeaderModel {
    /// Everything the header paints, for tests.
    #[cfg(test)]
    pub(crate) fn texts(&self) -> Vec<String> {
        let mut out = vec![self.name.to_string(), self.view.to_string()];
        out.extend(self.shifts.iter().map(|s| s.to_string()));
        out.extend(self.save.iter().map(|s| s.to_string()));
        out.extend(self.notice.iter().map(|s| s.to_string()));
        out.extend(self.pricing.iter().map(|s| s.to_string()));
        out.push(self.pricer.to_string());
        out.extend(self.time.iter().map(|s| s.to_string()));
        out
    }
}

/// `stale`: whether the last priced time is older than `stale_after` —
/// computed by the caller per frame and passed in, so rendering never
/// writes the prepared model.
pub(crate) fn render(
    h: &HeaderModel,
    stale: bool,
    theme: &Theme,
    stack: Option<&StackHandle>,
    tile: TileId,
) -> impl IntoElement {
    let muted = theme.muted_foreground;
    let warn = chip_paint(theme, Tone::WarningText).text;
    let chip = chip_paint(theme, Tone::Neutral);
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
        .children(stack.and_then(|s| s.marker(theme, tile)))
        .child(
            div()
                .font_weight(FontWeight::BOLD)
                .text_color(theme.foreground)
                .child(h.name.clone()),
        )
        .child(h.view.clone())
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
                    .text_color(warn)
                    .debug_selector(|| "pricer-notice".into())
                    .child(n),
            )
        })
        .when_some(h.pricing.clone(), |el, p| el.child(p))
        .child(h.pricer.clone())
        .when_some(
            if stale {
                h.time_stale.clone()
            } else {
                h.time.clone()
            },
            |el, t| el.child(div().when(stale, |el| el.text_color(warn)).child(t)),
        )
}

/// Always laid out, text or not, so the table's height never changes
/// (spec §8.3). Every footer line is a user error or a failure, so it
/// paints in danger text.
pub(crate) fn render_footer(text: Option<&SharedString>, theme: &Theme) -> impl IntoElement {
    h_flex()
        .w_full()
        .h(scale::design(FOOTER_HEIGHT))
        .px_2()
        .text_sm()
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
                "vanilla",
                "spot +2%",
                "vol \u{2212}1",
                "2 pricing…",
                "vendor"
            ]
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
        assert_eq!(h.last_priced, Some(at(0)));
        assert!(h.time.is_some());
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
            Some("pricer \"vendor\" is not built into this binary")
        );
        let h = prepare(HeaderInputs {
            sheet: &s,
            notice: Some("loading…".into()),
            save: None,
            settings: &settings(true),
            clock: Clock::utc(),
        });
        assert_eq!(h.notice.as_deref(), Some("loading…"));
    }
}
