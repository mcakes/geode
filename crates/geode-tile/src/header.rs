//! The header strip every tile paints: one height, the shared stack marker
//! first, the module's own left side, a flexible spacer, and a right cluster
//! in a fixed order — status, notices, times, health, `⋯`. The frame formats
//! nothing: every string arrives prepared, and a time run's stale flag is
//! the only thing a tile decides per frame.
//!
//! [`HealthWatch`] is the health half each tile keeps: the last
//! `DiagVersions::sources` it read from the shared [`Diagnostics`] and the
//! chip prepared from its answer, so a tile re-asks only when source health
//! or descriptions moved. The chip shows only while a source the tile reads
//! is not ok; a click queues the diagnostics page, which the shell opens and
//! never closes from here.

use std::rc::Rc;

use geode_shell::diagnostics::{Diagnostics, Health, TileHealth};
use geode_shell::shell::chip::{self, chip_paint};
use geode_shell::shell::control::{self, PointerStates as _};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_shell::tips;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Div, ElementId, Entity, Hsla, MouseButton, SharedString, Stateful, Window, div,
};
use gpui_component::{Theme, h_flex};

use crate::notice::{self, Notice};

/// Every tile header's height, in pixels at the design rem
/// (`shell::scale`), so headers line up across a split. Popups anchor
/// under it by the same constant.
pub const HEADER_HEIGHT: f32 = 22.0;

/// The page's own action, named in the chip's tooltip: its binding is the
/// chip's keyboard route. The chip has no key of its own.
const DIAGNOSTICS_ACTION: &str = "page::toggle_diagnostics";

/// One source time in the cluster: its label prepared, `stale` decided per
/// frame. A module that says `stale` in words prepares `stale_label`; one
/// that only colours the run leaves it `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeRun {
    pub label: SharedString,
    pub stale_label: Option<SharedString>,
    pub stale: bool,
}

impl TimeRun {
    /// What the run paints: the stale label while stale, if it has one.
    pub fn text(&self) -> &SharedString {
        match (&self.stale_label, self.stale) {
            (Some(stale), true) => stale,
            _ => &self.label,
        }
    }
}

/// A time run's text color: the floored warning text tone while stale,
/// the muted tone otherwise — one color in every tile.
pub fn time_color(stale: bool, theme: &Theme) -> Hsla {
    if stale {
        chip_paint(theme, chip::Tone::WarningText).text
    } else {
        theme.muted_foreground
    }
}

/// The health chip, prepared from a [`TileHealth`] when the answer changes.
#[derive(Clone)]
pub struct HealthChip {
    word: SharedString,
    tone: chip::Tone,
    title: SharedString,
    more: Option<SharedString>,
    tip_selector: SharedString,
    tile: TileId,
    diagnostics: Entity<Diagnostics>,
}

impl HealthChip {
    pub fn prepare(h: &TileHealth, tile: TileId, diagnostics: Entity<Diagnostics>) -> HealthChip {
        let (word, tone) = match h.worst {
            Health::Failed { .. } => ("failed", chip::Tone::Danger),
            Health::Degraded { .. } => ("degraded", chip::Tone::Warning),
            // `TileHealth` never carries Ok or Pending: this is PendingTooLong.
            _ => ("pending", chip::Tone::Warning),
        };
        let why = if h.reason.is_empty() {
            word
        } else {
            h.reason.as_str()
        };
        HealthChip {
            word: SharedString::new_static(word),
            tone,
            title: format!("{}: {why}", h.source).into(),
            more: (h.others > 0).then(|| format!("+{} more", h.others).into()),
            tip_selector: format!("tip-tile-health-{}", tile.0).into(),
            tile,
            diagnostics,
        }
    }

    pub fn word(&self) -> &str {
        &self.word
    }

    pub fn tone(&self) -> chip::Tone {
        self.tone
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn more(&self) -> Option<&str> {
        self.more.as_deref()
    }
}

/// What a `⋯` press runs: the module's own open or close of its menu.
pub type OnPress = Rc<dyn Fn(&mut Window, &mut App)>;

/// The `⋯` control: the module's ids and what a press does.
pub struct MenuTrigger {
    pub id: ElementId,
    pub selector: SharedString,
    pub tip_selector: SharedString,
    /// The menu's action, named in the tooltip with its live key.
    pub action: &'static str,
    /// The menu is up: the control keeps its selected fill.
    pub open: bool,
    pub on_press: OnPress,
}

/// The right-hand cluster, assembled per paint from prepared parts.
pub struct Cluster<'a> {
    pub tile: TileId,
    /// What only one module has (the pricer's prompt and counts, market-data's
    /// state), built each paint from prepared strings.
    pub status: Vec<AnyElement>,
    /// Painted in order through the notice door.
    pub notices: Vec<Notice>,
    pub times: Vec<TimeRun>,
    pub health: Option<&'a HealthChip>,
    /// `None` for a tile without an action menu (the blotter).
    pub menu: Option<MenuTrigger>,
}

impl Cluster<'_> {
    pub fn new(tile: TileId) -> Self {
        Cluster {
            tile,
            status: Vec::new(),
            notices: Vec::new(),
            times: Vec::new(),
            health: None,
            menu: None,
        }
    }
}

/// The strip: marker, the module's left side, a spacer, the cluster. The
/// caller adds its own debug selector (and the blotter its mono face).
pub fn frame(
    marker: Option<Stateful<Div>>,
    left: impl IntoElement,
    cluster: Cluster<'_>,
    theme: &Theme,
) -> Div {
    h_flex()
        .w_full()
        .h(scale::design(HEADER_HEIGHT))
        .items_center()
        .gap_3()
        .px_2()
        .text_sm()
        .text_color(theme.muted_foreground)
        .border_b_1()
        .border_color(theme.border)
        .children(marker)
        .child(left)
        .child(div().flex_1())
        .child(paint_cluster(cluster, theme))
}

fn paint_cluster(c: Cluster<'_>, theme: &Theme) -> Div {
    let tile = c.tile.0;
    let mut row = h_flex().items_center().gap_3();
    row = row.children(c.status);
    row = row.children(c.notices.iter().enumerate().map(|(i, n)| {
        notice::render(n, theme).debug_selector(move || format!("tile-notice-{tile}-{i}"))
    }));
    row = row.children(c.times.iter().enumerate().map(|(i, t)| {
        div()
            .text_color(time_color(t.stale, theme))
            .debug_selector(move || format!("tile-time-{tile}-{i}"))
            .child(t.text().clone())
    }));
    row = row.children(c.health.map(|h| health_chip(h, theme)));
    row = row.children(c.menu.map(|m| menu_button(m, theme)));
    row
}

fn health_chip(h: &HealthChip, theme: &Theme) -> Stateful<Div> {
    let paint = chip_paint(theme, h.tone);
    let tile = h.tile.0;
    let diagnostics = h.diagnostics.clone();
    div()
        .id(ElementId::NamedInteger(
            SharedString::new_static("tile-health"),
            tile,
        ))
        .px_1()
        .rounded(theme.radius_tokens().sm)
        .text_color(paint.text)
        .when_some(paint.fill, |el, fill| el.bg(fill))
        .pointer_states(control::for_chip(theme, &paint, theme.background))
        .debug_selector(move || format!("tile-health-{tile}"))
        .child(h.word.clone())
        .tooltip(tips::tip_with(
            h.tip_selector.clone(),
            h.title.clone(),
            Some(DIAGNOSTICS_ACTION),
            h.more.clone(),
        ))
        // The press is the chip's own: it neither refocuses the tile nor
        // reaches the shell root. The shell opens the page (never toggles).
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            window.prevent_default();
            diagnostics.update(cx, |d, cx| {
                d.request_diagnostics_page();
                cx.notify();
            });
        })
}

fn menu_button(m: MenuTrigger, theme: &Theme) -> Stateful<Div> {
    let muted = theme.muted_foreground;
    let selector = m.selector.clone();
    let on_press = m.on_press;
    div()
        .id(m.id)
        .px_1p5()
        .rounded(theme.radius_tokens().sm)
        .border_1()
        .border_color(theme.border)
        .when(m.open, |d| d.bg(theme.secondary))
        .text_color(muted)
        // Closed: bare-control hover and press. Open keeps its selected fill.
        .when(!m.open, |d| {
            d.pointer_states(control::paint(
                theme,
                control::Rest::Bare,
                theme.background,
                muted,
            ))
        })
        .child("⋯")
        .debug_selector(move || selector.to_string())
        // Capture phase, ahead of the open menu's outside-press closer: in
        // the bubble phase the closing click would see the menu already
        // closed and reopen it. Propagation continues so the shell still
        // focuses the tile.
        .capture_any_mouse_down(move |event, window, cx| {
            if event.button != MouseButton::Left {
                return;
            }
            on_press(window, cx);
        })
        .tooltip(tips::tip_with(
            m.tip_selector,
            SharedString::new_static("Actions"),
            Some(m.action),
            None,
        ))
}

/// The health half of a tile header: the last `sources` version read and
/// the chip prepared from the answer.
pub struct HealthWatch {
    diagnostics: Entity<Diagnostics>,
    tile: TileId,
    seen: Option<u64>,
    last: Option<TileHealth>,
    chip: Option<HealthChip>,
}

impl HealthWatch {
    pub fn new(diagnostics: Entity<Diagnostics>, tile: TileId) -> Self {
        HealthWatch {
            diagnostics,
            tile,
            seen: None,
            last: None,
            chip: None,
        }
    }

    /// Re-ask only when `versions().sources` moved since the last read — a
    /// diagnostics notify for a publication, a catalog or a log level asks
    /// nothing. Returns whether the chip changed.
    pub fn refresh(
        &mut self,
        cx: &App,
        ask: impl FnOnce(&Diagnostics) -> Option<TileHealth>,
    ) -> bool {
        let now = self.diagnostics.read(cx).versions().sources;
        if self.seen == Some(now) {
            return false;
        }
        self.reask(cx, ask)
    }

    /// Ask regardless of the version: the tile's question changed (its
    /// datasets or sources), or it is asking for the first time.
    pub fn reask(
        &mut self,
        cx: &App,
        ask: impl FnOnce(&Diagnostics) -> Option<TileHealth>,
    ) -> bool {
        let d = self.diagnostics.read(cx);
        self.seen = Some(d.versions().sources);
        let next = ask(d);
        if next == self.last {
            return false;
        }
        self.chip = next
            .as_ref()
            .map(|h| HealthChip::prepare(h, self.tile, self.diagnostics.clone()));
        self.last = next;
        true
    }

    pub fn chip(&self) -> Option<&HealthChip> {
        self.chip.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::log::LogLevels;
    use geode_shell::diagnostics::SourceSummary;
    use gpui::{Context, Render, TestAppContext, VisualTestContext};
    use gpui_component::ActiveTheme as _;
    use std::cell::Cell;
    use std::time::SystemTime;

    const TILE: TileId = TileId(3);

    fn failing(d: &mut Diagnostics, source: &str, health: Health, detail: &str) {
        d.describe_source(source, SourceSummary::for_dataset("risk"));
        d.note_health(source, health, detail.into(), SystemTime::UNIX_EPOCH);
    }

    fn ask(d: &Diagnostics) -> Option<TileHealth> {
        d.health_for_datasets(&["risk"])
    }

    #[gpui::test]
    fn the_chip_words_tones_and_tooltip(cx: &mut TestAppContext) {
        let diagnostics = cx.new(|_| Diagnostics::new(LogLevels::default()));
        let make = |h: Health, reason: &str, others: usize| {
            HealthChip::prepare(
                &TileHealth {
                    worst: h,
                    source: "risk_src".into(),
                    reason: reason.into(),
                    others,
                },
                TILE,
                diagnostics.clone(),
            )
        };
        let f = make(
            Health::Failed {
                reason: "torn".into(),
            },
            "torn",
            0,
        );
        assert_eq!((f.word(), f.tone()), ("failed", chip::Tone::Danger));
        assert_eq!(f.title(), "risk_src: torn");
        assert_eq!(f.more(), None);
        let d = make(
            Health::Degraded {
                reason: "col".into(),
            },
            "col",
            2,
        );
        assert_eq!((d.word(), d.tone()), ("degraded", chip::Tone::Warning));
        assert_eq!(d.more(), Some("+2 more"));
        let p = make(Health::PendingTooLong, "", 0);
        assert_eq!((p.word(), p.tone()), ("pending", chip::Tone::Warning));
        assert_eq!(
            p.title(),
            "risk_src: pending",
            "no reason: the word stands in"
        );
    }

    #[test]
    fn a_stale_run_reads_its_stale_label() {
        let mut run = TimeRun {
            label: "14:00:00".into(),
            stale_label: Some("14:00:00 stale".into()),
            stale: false,
        };
        assert_eq!(run.text().as_ref(), "14:00:00");
        run.stale = true;
        assert_eq!(run.text().as_ref(), "14:00:00 stale");
        let bare = TimeRun {
            label: "risk 14:00".into(),
            stale_label: None,
            stale: true,
        };
        assert_eq!(
            bare.text().as_ref(),
            "risk 14:00",
            "no stale label: tone only"
        );
    }

    #[gpui::test]
    fn a_stale_run_takes_the_warning_text_tone(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let theme = cx.theme();
            assert_eq!(time_color(false, theme), theme.muted_foreground);
            assert_eq!(
                time_color(true, theme),
                chip_paint(theme, chip::Tone::WarningText).text
            );
        });
    }

    #[gpui::test]
    fn refresh_asks_only_when_the_sources_version_moved(cx: &mut TestAppContext) {
        let diagnostics = cx.new(|_| Diagnostics::new(LogLevels::default()));
        let mut watch = HealthWatch::new(diagnostics.clone(), TILE);
        let asks = Cell::new(0);
        let counted = |cx: &mut TestAppContext, watch: &mut HealthWatch| {
            cx.update(|cx| {
                watch.refresh(cx, |d| {
                    asks.set(asks.get() + 1);
                    ask(d)
                })
            })
        };
        assert!(!counted(cx, &mut watch), "nothing to show yet");
        assert!(!counted(cx, &mut watch));
        assert_eq!(asks.get(), 1, "an unmoved version asks nothing");
        diagnostics.update(cx, |d, _| {
            failing(
                d,
                "risk_src",
                Health::Failed {
                    reason: "torn".into(),
                },
                "torn",
            )
        });
        assert!(counted(cx, &mut watch), "the chip appeared");
        assert_eq!(asks.get(), 2);
        assert_eq!(watch.chip().map(HealthChip::word), Some("failed"));
        diagnostics.update(cx, |d, _| {
            d.note_health(
                "risk_src",
                Health::Ok,
                String::new(),
                SystemTime::UNIX_EPOCH,
            )
        });
        assert!(counted(cx, &mut watch), "recovery clears it");
        assert!(watch.chip().is_none());
    }

    struct Strip {
        watch: HealthWatch,
        presses: Rc<Cell<u32>>,
        /// Presses that reached the strip's parent in the bubble phase —
        /// what the shell's click-to-focus would see.
        parent: Rc<Cell<u32>>,
        times: Vec<TimeRun>,
    }

    fn plain_time() -> TimeRun {
        TimeRun {
            label: "14:00:00".into(),
            stale_label: None,
            stale: false,
        }
    }

    impl Render for Strip {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let presses = self.presses.clone();
            let parent = self.parent.clone();
            let mut cluster = Cluster::new(TILE);
            cluster.status.push(
                div()
                    .debug_selector(|| "strip-status".into())
                    .child("2 pricing…")
                    .into_any_element(),
            );
            cluster.notices.push(Notice::warning("not saved"));
            cluster.times = self.times.clone();
            cluster.health = self.watch.chip();
            cluster.menu = Some(MenuTrigger {
                id: ElementId::Name("strip-menu".into()),
                selector: "strip-menu".into(),
                tip_selector: "tip-strip-menu".into(),
                action: "strip::menu",
                open: false,
                on_press: Rc::new(move |_, _| presses.set(presses.get() + 1)),
            });
            div()
                .on_mouse_down(MouseButton::Left, move |_, _, _| {
                    parent.set(parent.get() + 1)
                })
                .child(
                    frame(None, div().child("left"), cluster, cx.theme())
                        .debug_selector(|| "strip".into()),
                )
        }
    }

    fn open_strip(
        cx: &mut TestAppContext,
    ) -> (
        Entity<Strip>,
        Entity<Diagnostics>,
        Rc<Cell<u32>>,
        &mut VisualTestContext,
    ) {
        cx.update(gpui_component::init);
        let diagnostics = cx.new(|_| Diagnostics::new(LogLevels::default()));
        let presses = Rc::new(Cell::new(0));
        let (view, vcx) = cx.add_window_view({
            let diagnostics = diagnostics.clone();
            let presses = presses.clone();
            move |_, _| Strip {
                watch: HealthWatch::new(diagnostics, TILE),
                presses,
                parent: Rc::new(Cell::new(0)),
                times: vec![plain_time()],
            }
        });
        (view, diagnostics, presses, vcx)
    }

    fn parent_presses(view: &Entity<Strip>, vcx: &mut VisualTestContext) -> u32 {
        view.read_with(vcx, |s, _| s.parent.get())
    }

    fn show_failure(
        view: &Entity<Strip>,
        diagnostics: &Entity<Diagnostics>,
        vcx: &mut VisualTestContext,
    ) {
        diagnostics.update(vcx, |d, _| {
            failing(
                d,
                "risk_src",
                Health::Failed {
                    reason: "torn".into(),
                },
                "torn",
            )
        });
        view.update(vcx, |s, cx| {
            s.watch.refresh(cx, ask);
            cx.notify();
        });
        vcx.run_until_parked();
    }

    fn centre(vcx: &mut VisualTestContext, selector: &'static str) -> gpui::Point<gpui::Pixels> {
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        vcx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is painted"))
            .center()
    }

    fn click(vcx: &mut VisualTestContext, at: gpui::Point<gpui::Pixels>) {
        vcx.simulate_event(gpui::MouseDownEvent {
            position: at,
            modifiers: gpui::Modifiers::default(),
            button: MouseButton::Left,
            click_count: 1,
            first_mouse: false,
        });
        vcx.simulate_event(gpui::MouseUpEvent {
            position: at,
            modifiers: gpui::Modifiers::default(),
            button: MouseButton::Left,
            click_count: 1,
        });
    }

    #[gpui::test]
    fn the_cluster_paints_in_order_and_no_chip_without_health(cx: &mut TestAppContext) {
        let (view, diagnostics, _, vcx) = open_strip(cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            vcx.debug_bounds("tile-health-3").is_none(),
            "healthy: no chip"
        );
        show_failure(&view, &diagnostics, vcx);
        let order = [
            "strip-status",
            "tile-notice-3-0",
            "tile-time-3-0",
            "tile-health-3",
            "strip-menu",
        ]
        .map(|s| centre(vcx, s).x);
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    }

    #[gpui::test]
    fn clicking_the_health_chip_queues_the_diagnostics_page(cx: &mut TestAppContext) {
        let (view, diagnostics, _, vcx) = open_strip(cx);
        show_failure(&view, &diagnostics, vcx);
        let at = centre(vcx, "tile-health-3");
        click(vcx, at);
        assert!(diagnostics.update(vcx, |d, _| d.take_pending_diagnostics_page()));
        assert_eq!(
            parent_presses(&view, vcx),
            0,
            "the chip's press stops at the chip: no tile focus, no shell root"
        );
    }

    #[gpui::test]
    fn the_menu_trigger_runs_its_action(cx: &mut TestAppContext) {
        let (view, _, presses, vcx) = open_strip(cx);
        let at = centre(vcx, "strip-menu");
        click(vcx, at);
        assert_eq!(presses.get(), 1);
        assert_eq!(
            parent_presses(&view, vcx),
            1,
            "the menu press still reaches the tile's own listeners"
        );
    }

    /// The paint route reads the run's `text()`: a stale run with a stale
    /// label paints exactly as wide as a fresh run whose label is that text,
    /// and wider than its own fresh label.
    #[gpui::test]
    fn a_stale_run_paints_its_stale_label(cx: &mut TestAppContext) {
        let (view, _, _, vcx) = open_strip(cx);
        view.update(vcx, |s, cx| {
            s.times = vec![
                TimeRun {
                    label: "14:00:00".into(),
                    stale_label: Some("14:00:00 stale".into()),
                    stale: true,
                },
                TimeRun {
                    label: "14:00:00 stale".into(),
                    stale_label: None,
                    stale: false,
                },
                plain_time(),
            ];
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let width = |vcx: &mut VisualTestContext, s: &'static str| {
            f32::from(vcx.debug_bounds(s).unwrap().size.width)
        };
        let stale = width(vcx, "tile-time-3-0");
        let worded = width(vcx, "tile-time-3-1");
        let fresh = width(vcx, "tile-time-3-2");
        assert!((stale - worded).abs() < 0.5, "{stale} vs {worded}");
        assert!(stale > fresh + 0.5, "{stale} vs {fresh}");
    }

    #[gpui::test]
    fn the_frame_is_header_height(cx: &mut TestAppContext) {
        let (_, _, _, vcx) = open_strip(cx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let rem = vcx.update(|window, _| window.rem_size());
        let h = f32::from(vcx.debug_bounds("strip").unwrap().size.height);
        assert!(
            (h - scale::design_px(HEADER_HEIGHT, rem)).abs() < 0.5,
            "{h}"
        );
    }
}
