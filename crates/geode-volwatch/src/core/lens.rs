//! The lens: which companion number stands beside a level, and the state
//! the keys cycle. One set per tile, shared by every tab, saved in the
//! session. Pure: the arithmetic over a board cell lives here so the
//! prepared model, the readout and the tests agree on one definition.

use std::time::Duration;

use geode_core::volboard::{Cell, Lookback, Stats};

/// A z of two is a full tint: beyond two standard deviations the chip
/// saturates.
pub const Z_CEILING: f64 = 2.0;
/// Fewer daily observations than this and a rank or a z is not shown as
/// meaningful: the rank paints muted, the z blank.
pub const PCTL_MIN_COUNT: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lens {
    Moves,
    Rich,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Z,
    Pctl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    M6,
    Y1,
    Y2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    Level,
    Companion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LensState {
    pub lens: Lens,
    pub lookback: Lookback,
    pub metric: Metric,
    pub window: Window,
    pub shown: Shown,
}

impl Default for LensState {
    fn default() -> Self {
        LensState {
            lens: Lens::Moves,
            lookback: Lookback::D1,
            metric: Metric::Z,
            window: Window::Y1,
            shown: Shown::Level,
        }
    }
}

impl Lens {
    pub fn toggle(self) -> Lens {
        match self {
            Lens::Moves => Lens::Rich,
            Lens::Rich => Lens::Moves,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Lens::Moves => "moves",
            Lens::Rich => "rich/cheap",
        }
    }

    /// The session's word for this lens.
    pub fn word(self) -> &'static str {
        match self {
            Lens::Moves => "moves",
            Lens::Rich => "rich",
        }
    }

    pub fn parse(w: &str) -> Option<Lens> {
        [Lens::Moves, Lens::Rich]
            .into_iter()
            .find(|l| l.word() == w)
    }
}

impl Metric {
    pub fn toggle(self) -> Metric {
        match self {
            Metric::Z => Metric::Pctl,
            Metric::Pctl => Metric::Z,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Metric::Z => "z",
            Metric::Pctl => "pctl",
        }
    }

    /// The session's word for this metric.
    pub fn word(self) -> &'static str {
        self.label()
    }

    pub fn parse(w: &str) -> Option<Metric> {
        [Metric::Z, Metric::Pctl]
            .into_iter()
            .find(|m| m.word() == w)
    }
}

impl Window {
    pub const ALL: [Window; 3] = [Window::M6, Window::Y1, Window::Y2];

    pub fn next(self) -> Window {
        match self {
            Window::M6 => Window::Y1,
            Window::Y1 => Window::Y2,
            Window::Y2 => Window::M6,
        }
    }

    pub fn days(self) -> u64 {
        match self {
            Window::M6 => 182,
            Window::Y1 => 365,
            Window::Y2 => 730,
        }
    }

    pub fn duration(self) -> Duration {
        Duration::from_secs(self.days() * 86_400)
    }

    /// The window whose span is exactly `d`; `None` for any other span,
    /// since a window is one of three and never approximated.
    pub fn from_duration(d: Duration) -> Option<Window> {
        Self::ALL.into_iter().find(|w| w.duration() == d)
    }

    pub fn label(self) -> &'static str {
        match self {
            Window::M6 => "6M",
            Window::Y1 => "1Y",
            Window::Y2 => "2Y",
        }
    }

    /// The session's word for this window.
    pub fn word(self) -> &'static str {
        self.label()
    }

    pub fn parse(w: &str) -> Option<Window> {
        Self::ALL.into_iter().find(|x| x.word() == w)
    }
}

impl Shown {
    pub fn toggle(self) -> Shown {
        match self {
            Shown::Level => Shown::Companion,
            Shown::Companion => Shown::Level,
        }
    }

    /// The session's word for the shown number.
    pub fn word(self) -> &'static str {
        match self {
            Shown::Level => "level",
            Shown::Companion => "companion",
        }
    }

    pub fn parse(w: &str) -> Option<Shown> {
        [Shown::Level, Shown::Companion]
            .into_iter()
            .find(|s| s.word() == w)
    }
}

impl LensState {
    /// `Δ`, `z` or `pctl`: the companion's head and the readout's word.
    pub fn companion_head(&self) -> &'static str {
        match (self.lens, self.metric) {
            (Lens::Moves, _) => "\u{394}",
            (Lens::Rich, Metric::Z) => "z",
            (Lens::Rich, Metric::Pctl) => "pctl",
        }
    }

    /// The strip's lens readout; `single` adds the swap when the
    /// companion is the shown number. A pair view shows both numbers, so
    /// it never names a swap.
    pub fn readout(&self, single: bool) -> String {
        let mut s = match self.lens {
            Lens::Moves => format!("moves \u{b7} \u{394} {}", self.lookback.label()),
            Lens::Rich => format!(
                "rich/cheap \u{b7} {} \u{b7} {}",
                self.metric.label(),
                self.window.label()
            ),
        };
        if single && self.shown == Shown::Companion {
            s.push_str(" \u{b7} showing ");
            s.push_str(self.companion_head());
        }
        s
    }
}

fn finite(v: f64) -> Option<f64> {
    v.is_finite().then_some(v)
}

/// `(level − mean) / sd` over the window; blank for a flat or short
/// history, since a z over those says nothing.
pub fn z_score(cell: &Cell) -> Option<f64> {
    let current = finite(cell.current?)?;
    z_from(current, cell.stats.as_ref()?)
}

fn z_from(current: f64, stats: &Stats) -> Option<f64> {
    if stats.count < PCTL_MIN_COUNT
        || !stats.sd.is_finite()
        || stats.sd <= 0.0
        || !stats.mean.is_finite()
    {
        return None;
    }
    finite((current - stats.mean) / stats.sd)
}

/// The number beside the level under `state`: the change over the
/// lookback, the z, or the rank in `[0, 1]`.
pub fn companion(cell: &Cell, state: &LensState) -> Option<f64> {
    match (state.lens, state.metric) {
        (Lens::Moves, _) => finite(cell.changes.first()?.as_ref()?.value),
        (Lens::Rich, Metric::Z) => z_score(cell),
        (Lens::Rich, Metric::Pctl) => finite(cell.pctl?.0),
    }
}

/// A rank's signed distance from the median, in `[-1, 1]`: the chip's
/// magnitude and sign for a percentile.
pub fn pctl_side(rank: f64) -> f64 {
    (rank - 0.5) / 0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use geode_core::volboard::{Change, Measure};

    fn cell(
        current: Option<f64>,
        change: Option<f64>,
        pctl: Option<f64>,
        stats: Option<(f64, f64, usize)>,
    ) -> Cell {
        Cell {
            measure: Measure::atfm("3M"),
            current,
            current_at: current.map(|_| Utc::now()),
            changes: vec![change.map(|v| Change {
                value: v,
                from: 1.0,
                at: Utc::now(),
            })],
            pctl: pctl.map(|r| (r, 251)),
            stats: stats.map(|(mean, sd, count)| Stats { mean, sd, count }),
        }
    }

    #[test]
    fn the_defaults_are_moves_1d_z_1y_level_and_every_key_cycles() {
        let s = LensState::default();
        assert_eq!(
            (s.lens, s.lookback, s.metric, s.window, s.shown),
            (
                Lens::Moves,
                Lookback::D1,
                Metric::Z,
                Window::Y1,
                Shown::Level
            )
        );
        assert_eq!(Lens::Moves.toggle(), Lens::Rich);
        assert_eq!(Lens::Rich.toggle(), Lens::Moves);
        assert_eq!(Metric::Z.toggle(), Metric::Pctl);
        assert_eq!(Window::M6.next(), Window::Y1);
        assert_eq!(Window::Y1.next(), Window::Y2);
        assert_eq!(Window::Y2.next(), Window::M6);
        assert_eq!(Shown::Level.toggle(), Shown::Companion);
        assert_eq!(
            Window::Y1.duration(),
            std::time::Duration::from_secs(365 * 86_400)
        );
        assert_eq!(
            Window::from_duration(Window::M6.duration()),
            Some(Window::M6)
        );
        assert_eq!(
            Window::from_duration(std::time::Duration::from_secs(1)),
            None
        );
        for w in [Window::M6, Window::Y1, Window::Y2] {
            assert_eq!(Window::parse(w.word()), Some(w));
        }
        for l in [Lens::Moves, Lens::Rich] {
            assert_eq!(Lens::parse(l.word()), Some(l));
        }
        for m in [Metric::Z, Metric::Pctl] {
            assert_eq!(Metric::parse(m.word()), Some(m));
        }
        for s in [Shown::Level, Shown::Companion] {
            assert_eq!(Shown::parse(s.word()), Some(s));
        }
    }

    #[test]
    fn the_readout_names_the_lens_its_metric_and_the_shown_number() {
        let mut s = LensState::default();
        assert_eq!(s.readout(false), "moves \u{b7} \u{394} 1d");
        assert_eq!(s.companion_head(), "\u{394}");
        s.lens = Lens::Rich;
        assert_eq!(s.readout(false), "rich/cheap \u{b7} z \u{b7} 1Y");
        assert_eq!(s.companion_head(), "z");
        s.metric = Metric::Pctl;
        s.window = Window::M6;
        assert_eq!(s.readout(false), "rich/cheap \u{b7} pctl \u{b7} 6M");
        s.shown = Shown::Companion;
        assert_eq!(
            s.readout(true),
            "rich/cheap \u{b7} pctl \u{b7} 6M \u{b7} showing pctl"
        );
        assert_eq!(
            s.readout(false),
            "rich/cheap \u{b7} pctl \u{b7} 6M",
            "a pair view never shows the swap"
        );
    }

    #[test]
    fn z_is_blank_for_a_zero_sd_or_a_short_count() {
        let ok = cell(Some(19.6), None, None, Some((16.9, 1.9, 251)));
        assert!((z_score(&ok).unwrap() - (19.6 - 16.9) / 1.9).abs() < 1e-12);
        assert_eq!(
            z_score(&cell(Some(19.6), None, None, Some((19.6, 0.0, 251)))),
            None
        );
        assert_eq!(
            z_score(&cell(Some(19.6), None, None, Some((16.9, 1.9, 59)))),
            None
        );
        assert_eq!(
            z_score(&cell(Some(19.6), None, None, Some((16.9, f64::NAN, 251)))),
            None
        );
        assert_eq!(
            z_score(&cell(None, None, None, Some((16.9, 1.9, 251)))),
            None
        );
        assert_eq!(z_score(&cell(Some(19.6), None, None, None)), None);
    }

    /// Sixty observations is enough: the minimum is inclusive, so a
    /// window exactly at it shows its z rather than a blank. A negative
    /// sd, or a mean or level that is not a number, blanks it: a division
    /// by zero already reads as blank, so the sign guard is what keeps a
    /// corrupt sd from showing a plausible z.
    #[test]
    fn a_count_of_exactly_sixty_yields_a_z_and_a_negative_sd_blanks_it() {
        let at_minimum = cell(Some(19.6), None, None, Some((16.9, 1.9, PCTL_MIN_COUNT)));
        assert!((z_score(&at_minimum).unwrap() - (19.6 - 16.9) / 1.9).abs() < 1e-12);
        assert_eq!(
            z_score(&cell(
                Some(19.6),
                None,
                None,
                Some((16.9, 1.9, PCTL_MIN_COUNT - 1))
            )),
            None
        );
        assert_eq!(
            z_score(&cell(Some(19.6), None, None, Some((16.9, -1.9, 251)))),
            None,
            "a negative sd is not a dispersion"
        );
        assert_eq!(
            z_score(&cell(Some(19.6), None, None, Some((f64::NAN, 1.9, 251)))),
            None
        );
        assert_eq!(
            z_score(&cell(Some(f64::NAN), None, None, Some((16.9, 1.9, 251)))),
            None
        );
    }

    #[test]
    fn the_companion_follows_the_lens_and_the_metric() {
        let c = cell(Some(19.6), Some(0.8), Some(0.91), Some((16.9, 1.9, 251)));
        let mut s = LensState::default();
        assert_eq!(companion(&c, &s), Some(0.8));
        s.lens = Lens::Rich;
        assert!((companion(&c, &s).unwrap() - (19.6 - 16.9) / 1.9).abs() < 1e-12);
        s.metric = Metric::Pctl;
        assert_eq!(companion(&c, &s), Some(0.91));
        assert!((pctl_side(0.91) - 0.82).abs() < 1e-12);
        assert_eq!(pctl_side(0.5), 0.0);
        assert!((pctl_side(0.0) + 1.0).abs() < 1e-12);
        let no_change = cell(Some(19.6), None, None, None);
        s.lens = Lens::Moves;
        assert_eq!(companion(&no_change, &s), None);
    }
}
