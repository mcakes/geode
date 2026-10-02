//! The header strip every tile paints: one height, the shared stack marker
//! first, the module's own left side taking the free width (clipped when it
//! runs out), and a right cluster in a fixed order — the mode icon, status,
//! notices, times, link chips, health, `⋯`. The mode icon shows only while
//! the tile is in edit or visual mode ([`Mode`]). Status and notices are
//! text of any length: they shrink, one line each and cut with an ellipsis,
//! within at most [`TEXT_SHARE`] of the header; times, link chips, health
//! and `⋯` never shrink. The frame formats nothing: every string arrives
//! prepared, and a time run's stale flag is the only thing a tile decides
//! per frame.
//!
//! A tile in a link group shows it in the fixed tail, after the times: a
//! chip per group it follows or emits into ([`link_chips`], read from the
//! tile's frame handle at paint). The chips never shrink and take no press.
//!
//! [`HealthWatch`] is the health half each tile keeps: the last
//! `DiagVersions::sources` it read from the shared [`Diagnostics`] and the
//! chip prepared from its answer, so a tile re-asks only when source health
//! or descriptions moved. The chip shows only while a source the tile reads
//! is not ok; a click queues the diagnostics page, which the shell opens and
//! never closes from here.

use std::rc::Rc;

use geode_core::link::Group;
use geode_shell::diagnostics::{Diagnostics, Health, TileHealth};
use geode_shell::frame::FrameRef;
use geode_shell::link::group_color;
use geode_shell::shell::chip::{self, chip_paint};
use geode_shell::shell::control::{self, PointerStates as _};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_shell::tips;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Div, ElementId, Entity, Hsla, MouseButton, SharedString, Stateful, Window,
    div, relative,
};
use gpui_component::{Icon, Sizable as _, Theme, h_flex};
use gpui_kit_assets::IconName;

use crate::notice::{self, Notice};

/// Every tile header's height, in pixels at the design rem
/// (`shell::scale`), so headers line up across a split. Popups anchor
/// under it by the same constant.
pub const HEADER_HEIGHT: f32 = 22.0;

/// The most of the header's width the cluster's status and notices may
/// take together. A 100-character error then leaves the left side the rest
/// less the times, chip and `⋯`, instead of collapsing it to nothing.
pub const TEXT_SHARE: f32 = 0.5;

/// The notices' shared tooltip selector: each tooltip carries its notice's
/// full text, the only place a cut notice can be read whole.
const NOTICE_TIP: &str = "tip-tile-notice";

/// The page's own action, named in the chip's tooltip: its binding is the
/// chip's keyboard route. The chip has no key of its own.
const DIAGNOSTICS_ACTION: &str = "page::toggle_diagnostics";

/// The link group chooser's action, named in a link chip's tooltip: its
/// binding is how the membership the chip shows is changed. The chip takes
/// no press of its own.
const LINK_ACTION: &str = "tile::link_group";

/// The link chips' shared tooltip selector: only one tooltip shows at once.
const LINK_TIP: &str = "tip-tile-link";

/// What a tile does with the group a link chip names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkRole {
    /// The tile's scope comes from the group.
    Follow,
    /// The tile's selection goes to the group.
    Emit,
    /// Both, with one group.
    Both,
}

/// One link chip: a group and what the tile does with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkChip {
    pub group: Group,
    pub role: LinkRole,
}

/// The link chips of the tile `frame` is bound to, read from the frame at
/// paint: one [`LinkRole::Both`] chip when it follows and emits into the
/// same group, otherwise its follow chip then its emit chip, filled from
/// the front. A handle bound to no tile has none. A module passes the
/// answer to [`Cluster::links`] and stores nothing: membership is the
/// frame's, and a copy kept in a tile would outlive a change made through
/// the shell.
pub fn link_chips(frame: &FrameRef, cx: &App) -> [Option<LinkChip>; 2] {
    let Some(tile) = frame.tile() else {
        return [None, None];
    };
    let membership = frame.entity().read(cx).membership(tile);
    let chip = |group, role| LinkChip { group, role };
    match (membership.follow, membership.emit) {
        (Some(follow), Some(emit)) if follow == emit => [Some(chip(follow, LinkRole::Both)), None],
        (Some(follow), emit) => [
            Some(chip(follow, LinkRole::Follow)),
            emit.map(|group| chip(group, LinkRole::Emit)),
        ],
        (None, emit) => [emit.map(|group| chip(group, LinkRole::Emit)), None],
    }
}

/// A link chip's tooltip title: its letter and arrows in words.
pub fn link_title(chip: LinkChip) -> &'static str {
    match (chip.role, chip.group) {
        (LinkRole::Follow, Group::A) => "Following group A",
        (LinkRole::Follow, Group::B) => "Following group B",
        (LinkRole::Follow, Group::C) => "Following group C",
        (LinkRole::Follow, Group::D) => "Following group D",
        (LinkRole::Emit, Group::A) => "Emitting into group A",
        (LinkRole::Emit, Group::B) => "Emitting into group B",
        (LinkRole::Emit, Group::C) => "Emitting into group C",
        (LinkRole::Emit, Group::D) => "Emitting into group D",
        (LinkRole::Both, Group::A) => "Following and emitting into group A",
        (LinkRole::Both, Group::B) => "Following and emitting into group B",
        (LinkRole::Both, Group::C) => "Following and emitting into group C",
        (LinkRole::Both, Group::D) => "Following and emitting into group D",
    }
}

/// What the tile's keys are doing, as its header shows it: an icon at the
/// head of the cluster while a field holds the keys ([`Mode::Edit`]) or a
/// selection is live ([`Mode::Visual`]), nothing otherwise. A module derives
/// it from the same `mode` its key context publishes
/// ([`Mode::from_key_mode`]), so the cue cannot disagree with the keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Normal,
    Edit,
    Visual,
}

impl Mode {
    /// The header's reading of a key context's `mode` value: `insert` is
    /// [`Mode::Edit`], `visual` is [`Mode::Visual`]; `normal` and `menu`
    /// (an open action menu is its own surface) show nothing.
    pub fn from_key_mode(mode: &str) -> Mode {
        match mode {
            "insert" => Mode::Edit,
            "visual" => Mode::Visual,
            _ => Mode::Normal,
        }
    }
}

/// The mode tooltip's detail: the default key that leaves edit and visual
/// alike, spelled `escape` as every binding spells it so its chip reads ⎋.
/// A static hint: a rebound leaving key is not reflected here.
pub const LEAVE_HINT: &str = "`escape` leaves";

/// The mode tooltip's title and detail, `None` in normal mode.
pub fn mode_tip(mode: Mode) -> Option<(&'static str, &'static str)> {
    match mode {
        Mode::Normal => None,
        Mode::Edit => Some(("Editing", LEAVE_HINT)),
        Mode::Visual => Some(("Visual selection", LEAVE_HINT)),
    }
}

/// The mode icon's color, `None` in normal mode: edit the floored warning
/// text tone, visual the floored info text tone — two hues, each clearing
/// the readable floor against the header's ground on every bundled theme.
pub fn mode_color(mode: Mode, theme: &Theme) -> Option<Hsla> {
    let tone = match mode {
        Mode::Normal => return None,
        Mode::Edit => chip::Tone::WarningText,
        Mode::Visual => chip::Tone::InfoText,
    };
    Some(chip_paint(theme, tone).text)
}

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
    /// The tile's mode, painted as an icon ahead of everything else.
    pub mode: Mode,
    /// What only one module has (the pricer's prompt and counts, market-data's
    /// state), built each paint from prepared strings.
    pub status: Vec<AnyElement>,
    /// Painted in order through the notice door.
    pub notices: Vec<Notice>,
    pub times: Vec<TimeRun>,
    /// The tile's link groups, from [`link_chips`]; painted in the fixed
    /// tail after the times.
    pub links: [Option<LinkChip>; 2],
    pub health: Option<&'a HealthChip>,
    /// `None` for a tile without an action menu (the blotter).
    pub menu: Option<MenuTrigger>,
}

impl Cluster<'_> {
    pub fn new(tile: TileId) -> Self {
        Cluster {
            tile,
            mode: Mode::Normal,
            status: Vec::new(),
            notices: Vec::new(),
            times: Vec::new(),
            links: [None, None],
            health: None,
            menu: None,
        }
    }
}

/// The strip: marker, the module's left side (flexible, clipped), the
/// cluster's text (shrinking, capped at [`TEXT_SHARE`]), its fixed tail. The
/// caller adds its own debug selector (and the blotter its mono face).
pub fn frame(
    marker: Option<Stateful<Div>>,
    left: impl IntoElement,
    cluster: Cluster<'_>,
    theme: &Theme,
) -> Div {
    let tile = cluster.tile.0;
    let mode = mode_icon(cluster.mode, tile, theme);
    let (text, tail) = paint_cluster(cluster, theme);
    // The left side has no basis of its own: it takes what the cluster
    // leaves and clips. The cluster's text takes its natural width up to
    // TEXT_SHARE and then cuts, so neither side collapses the other; the
    // tail (times, link chips, health, `⋯`) never shrinks, so it stays on the tile
    // unless the tile is narrower than the tail alone.
    let slot = h_flex().min_w_0().overflow_hidden();
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
        .child(
            slot.flex_1()
                .items_center()
                .debug_selector(move || format!("tile-header-left-{tile}"))
                .child(left),
        )
        .children(mode)
        .children(text)
        .child(tail.flex_none())
}

/// The cluster in two parts: its text (status and notices; `None` when it
/// has none, so no empty gap is laid out) and its fixed tail.
fn paint_cluster(c: Cluster<'_>, theme: &Theme) -> (Option<Stateful<Div>>, Div) {
    let tile = c.tile.0;
    let text = (!c.status.is_empty() || !c.notices.is_empty()).then(|| {
        h_flex()
            .id(ElementId::NamedInteger(
                SharedString::new_static("tile-cluster-text"),
                tile,
            ))
            .flex_shrink(1.0)
            .min_w_0()
            .max_w(relative(TEXT_SHARE))
            .overflow_hidden()
            .items_center()
            .gap_3()
            // A status item is the module's own element: the wrapper bounds
            // it, and its text inherits one line and the ellipsis.
            .children(
                c.status
                    .into_iter()
                    .map(|s| div().min_w_0().truncate().child(s)),
            )
            .children(c.notices.iter().enumerate().map(|(i, n)| {
                notice::truncated(n, i, SharedString::new_static(NOTICE_TIP), theme)
                    .debug_selector(move || format!("tile-notice-{tile}-{i}"))
            }))
    });
    let mut row = h_flex().items_center().gap_3();
    row = row.whitespace_nowrap();
    row = row.children(c.times.iter().enumerate().map(|(i, t)| {
        div()
            .text_color(time_color(t.stale, theme))
            .debug_selector(move || format!("tile-time-{tile}-{i}"))
            .child(t.text().clone())
    }));
    row = row.children(
        c.links
            .into_iter()
            .flatten()
            .map(|chip| link_chip(chip, tile, theme)),
    );
    row = row.children(c.health.map(|h| health_chip(h, theme)));
    row = row.children(c.menu.map(|m| menu_button(m, theme)));
    (text, row)
}

/// A link chip: a solid chip in the group's color, carrying the group's
/// letter and its role's arrows (down for what the tile takes from the
/// group, up for what it sends) in text readable on that fill. The fill is
/// the group's color unchanged, the value the bundled-theme sweep holds
/// apart from the other groups; painting the color as text instead puts it
/// through a second floor that merges groups on low-chroma themes. The
/// letter and the arrows carry the meaning; the color repeats the group.
/// It takes no press and so has no pointer states: a click on it focuses
/// the tile like the rest of the header, and the tooltip names the
/// chooser's key. Ids derive from the tile and the role, which two chips
/// never share.
fn link_chip(chip: LinkChip, tile: u64, theme: &Theme) -> Stateful<Div> {
    let paint = chip::colored(theme, group_color(theme, chip.group), theme.background);
    let (name, role, follows, emits) = match chip.role {
        LinkRole::Follow => ("tile-link-follow", "follow", true, false),
        LinkRole::Emit => ("tile-link-emit", "emit", false, true),
        LinkRole::Both => ("tile-link-both", "both", true, true),
    };
    let letter = chip.group.letter();
    let arrow = |icon: IconName| Icon::new(icon).xsmall().text_color(paint.text);
    h_flex()
        .id(ElementId::NamedInteger(
            SharedString::new_static(name),
            tile,
        ))
        .flex_none()
        .items_center()
        .gap_0p5()
        .px_1()
        .rounded(theme.radius_tokens().sm)
        .text_color(paint.text)
        .when_some(paint.fill, |el, fill| el.bg(fill))
        .debug_selector(move || format!("tile-link-{tile}-{letter}-{role}"))
        .child(letter)
        .when(follows, |el| el.child(arrow(IconName::ArrowDown)))
        .when(emits, |el| el.child(arrow(IconName::ArrowUp)))
        .tooltip(tips::tip_with(
            SharedString::new_static(LINK_TIP),
            SharedString::new_static(link_title(chip)),
            Some(LINK_ACTION),
            None,
        ))
}

/// The mode icon: a bare glyph in its mode's color, no fill, at the
/// header's text size, never shrinking. Ids and selectors derive from the
/// tile; the tooltip selector is shared, as only one tooltip shows at once.
fn mode_icon(mode: Mode, tile: u64, theme: &Theme) -> Option<Stateful<Div>> {
    let (icon, name, tip) = match mode {
        Mode::Normal => return None,
        Mode::Edit => (IconName::Pencil, "tile-mode-edit", "tip-tile-mode-edit"),
        Mode::Visual => (
            IconName::SquareDashedMousePointer,
            "tile-mode-visual",
            "tip-tile-mode-visual",
        ),
    };
    let (title, detail) = mode_tip(mode)?;
    let color = mode_color(mode, theme)?;
    Some(
        div()
            .id(ElementId::NamedInteger(
                SharedString::new_static(name),
                tile,
            ))
            .flex_none()
            .flex()
            .items_center()
            .debug_selector(move || format!("{name}-{tile}"))
            .child(Icon::new(icon).small().text_color(color))
            .tooltip(tips::tip_with(
                SharedString::new_static(tip),
                SharedString::new_static(title),
                None,
                Some(SharedString::new_static(detail)),
            )),
    )
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
    use geode_core::groupings::GroupingSlots;
    use geode_core::log::LogLevels;
    use geode_core::scopes::SavedScopes;
    use geode_shell::diagnostics::SourceSummary;
    use geode_shell::frame::Frame;
    use geode_shell::tiling::WorkspaceIx;
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
        /// How many fixed-width runs the left side carries: enough of them
        /// overflow a narrow header.
        left_runs: usize,
        notice: Notice,
        mode: Mode,
        /// The tile's own handle on a frame: what its link chips are read
        /// from, each paint, as a module reads them.
        frame: FrameRef,
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
            cluster.mode = self.mode;
            cluster.status.push(
                div()
                    .debug_selector(|| "strip-status".into())
                    .child("2 pricing…")
                    .into_any_element(),
            );
            cluster.notices.push(self.notice.clone());
            cluster.times = self.times.clone();
            cluster.health = self.watch.chip();
            cluster.links = link_chips(&self.frame, cx);
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
                    frame(
                        None,
                        h_flex().gap_3().child("left").children(
                            (0..self.left_runs)
                                .map(|i| div().flex_none().child(format!("left run {i}"))),
                        ),
                        cluster,
                        cx.theme(),
                    )
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
        let frame = fresh_frame(cx);
        let (view, vcx) = cx.add_window_view({
            let diagnostics = diagnostics.clone();
            let presses = presses.clone();
            move |_, _| Strip {
                watch: HealthWatch::new(diagnostics, TILE),
                presses,
                parent: Rc::new(Cell::new(0)),
                times: vec![plain_time()],
                left_runs: 0,
                notice: Notice::warning("not saved"),
                mode: Mode::Normal,
                frame: FrameRef::for_tile(frame, WorkspaceIx::FIRST, TILE),
            }
        });
        (view, diagnostics, presses, vcx)
    }

    fn fresh_frame(cx: &mut TestAppContext) -> Entity<Frame> {
        cx.new(|_| Frame::new(GroupingSlots::default(), SavedScopes::new(), None))
    }

    /// Set the strip's tile's membership on its frame and repaint the
    /// strip, as the shell's doors do for a real tile.
    fn link(
        view: &Entity<Strip>,
        vcx: &mut VisualTestContext,
        follow: Option<Group>,
        emit: Option<Group>,
    ) {
        let frame = view.read_with(vcx, |s, _| s.frame.entity().clone());
        frame.update(vcx, |f, cx| {
            f.follow(TILE, follow);
            f.emit(TILE, emit);
            cx.notify();
        });
        view.update(vcx, |_, cx| cx.notify());
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    fn chip(group: Group, role: LinkRole) -> Option<LinkChip> {
        Some(LinkChip { group, role })
    }

    /// Another tile's membership is not this tile's.
    #[gpui::test]
    fn a_tile_in_no_group_has_no_link_chip(cx: &mut TestAppContext) {
        let frame = fresh_frame(cx);
        frame.update(cx, |f, _| {
            f.follow(TileId(9), Some(Group::A));
            f.emit(TileId(9), Some(Group::B));
        });
        let tile = FrameRef::for_tile(frame, WorkspaceIx::FIRST, TILE);
        assert_eq!(cx.read(|cx| link_chips(&tile, cx)), [None, None]);
    }

    /// A handle bound to no tile (a page, a shell door) names no tile to
    /// look up, whatever the frame holds; the same frame through the
    /// tile's own handle shows the chip.
    #[gpui::test]
    fn an_unbound_frame_ref_has_none(cx: &mut TestAppContext) {
        let frame = fresh_frame(cx);
        frame.update(cx, |f, _| {
            f.follow(TILE, Some(Group::A));
        });
        let unbound = FrameRef::new(frame.clone(), WorkspaceIx::FIRST);
        assert_eq!(cx.read(|cx| link_chips(&unbound, cx)), [None, None]);
        let bound = FrameRef::for_tile(frame, WorkspaceIx::FIRST, TILE);
        assert_eq!(
            cx.read(|cx| link_chips(&bound, cx)),
            [chip(Group::A, LinkRole::Follow), None]
        );
    }

    #[gpui::test]
    fn following_and_emitting_one_group_is_one_chip_and_two_groups_are_two(
        cx: &mut TestAppContext,
    ) {
        let frame = fresh_frame(cx);
        let tile = FrameRef::for_tile(frame.clone(), WorkspaceIx::FIRST, TILE);
        let mut chips = |follow: Option<Group>, emit: Option<Group>| {
            frame.update(cx, |f, _| {
                f.follow(TILE, follow);
                f.emit(TILE, emit);
            });
            cx.read(|cx| link_chips(&tile, cx))
        };
        assert_eq!(
            chips(Some(Group::A), Some(Group::A)),
            [chip(Group::A, LinkRole::Both), None]
        );
        assert_eq!(
            chips(Some(Group::A), Some(Group::B)),
            [
                chip(Group::A, LinkRole::Follow),
                chip(Group::B, LinkRole::Emit)
            ]
        );
        assert_eq!(
            chips(None, Some(Group::C)),
            [chip(Group::C, LinkRole::Emit), None],
            "an emitter alone takes the first slot: no gap is laid out"
        );
        assert_eq!(
            chips(Some(Group::D), None),
            [chip(Group::D, LinkRole::Follow), None]
        );
    }

    /// The chip is part of the tail that never shrinks: a left side wider
    /// than the tile clips, and the chip stays inside the header, after the
    /// left side and the time and before the health chip and the menu.
    /// Leaving the group removes it.
    #[gpui::test]
    fn the_link_chip_paints_in_the_fixed_tail(cx: &mut TestAppContext) {
        let (view, diagnostics, _, vcx) = open_strip(cx);
        view.update(vcx, |s, cx| {
            s.left_runs = 40;
            cx.notify();
        });
        vcx.simulate_resize(gpui::size(gpui::px(480.0), gpui::px(300.0)));
        show_failure(&view, &diagnostics, vcx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        for s in [
            "tile-link-3-A-follow",
            "tile-link-3-A-emit",
            "tile-link-3-A-both",
        ] {
            assert!(vcx.debug_bounds(s).is_none(), "no group: no {s}");
        }

        link(&view, vcx, Some(Group::A), None);
        let strip = vcx.debug_bounds("strip").expect("the strip is painted");
        let chip = vcx
            .debug_bounds("tile-link-3-A-follow")
            .expect("the chip is painted");
        assert!(
            chip.left() >= strip.left()
                && chip.right() <= strip.right()
                && chip.top() >= strip.top()
                && chip.bottom() <= strip.bottom(),
            "the chip {chip:?} escapes the header {strip:?}"
        );
        let left = vcx.debug_bounds("tile-header-left-3").unwrap();
        let time = vcx.debug_bounds("tile-time-3-0").unwrap();
        let health = vcx.debug_bounds("tile-health-3").unwrap();
        let menu = vcx.debug_bounds("strip-menu").unwrap();
        assert!(left.right() <= chip.left(), "{left:?} vs {chip:?}");
        assert!(time.right() <= chip.left(), "{time:?} vs {chip:?}");
        assert!(chip.right() <= health.left(), "{chip:?} vs {health:?}");
        assert!(health.right() <= menu.left(), "{health:?} vs {menu:?}");
        // Two filled chips side by side: one a pixel taller than the other
        // shows as a step along the strip.
        let same_height = |a: gpui::Bounds<gpui::Pixels>, b: gpui::Bounds<gpui::Pixels>| {
            (f32::from(a.size.height) - f32::from(b.size.height)).abs() < 0.5
                && (f32::from(a.top()) - f32::from(b.top())).abs() < 0.5
        };
        assert!(same_height(chip, health), "{chip:?} vs {health:?}");

        // Two groups: the follow chip, then the emit chip.
        link(&view, vcx, Some(Group::A), Some(Group::B));
        let follow = vcx.debug_bounds("tile-link-3-A-follow").unwrap();
        let emit = vcx.debug_bounds("tile-link-3-B-emit").unwrap();
        let health = vcx.debug_bounds("tile-health-3").unwrap();
        assert!(follow.right() <= emit.left(), "{follow:?} vs {emit:?}");
        assert!(emit.right() <= health.left(), "{emit:?} vs {health:?}");

        // One group both ways is one chip under its own selector, its two
        // arrows making it no taller.
        link(&view, vcx, Some(Group::B), Some(Group::B));
        let both = vcx.debug_bounds("tile-link-3-B-both").unwrap();
        let health = vcx.debug_bounds("tile-health-3").unwrap();
        assert!(same_height(both, health), "{both:?} vs {health:?}");
        assert!(vcx.debug_bounds("tile-link-3-B-follow").is_none());
        assert!(vcx.debug_bounds("tile-link-3-B-emit").is_none());

        link(&view, vcx, None, None);
        for s in [
            "tile-link-3-A-follow",
            "tile-link-3-B-emit",
            "tile-link-3-B-both",
        ] {
            assert!(vcx.debug_bounds(s).is_none(), "left the group: no {s}");
        }
    }

    /// What the chip paints is the group's color itself, as its fill. The
    /// bundled-theme sweep holds the four group colors apart and readable
    /// as fills; a header that painted the color some other way (as text
    /// on a neutral chip, or through another floor) would show colors the
    /// sweep never measured. Read from the painted scene: the quad at the
    /// chip's bounds.
    #[gpui::test]
    fn the_link_chip_fill_is_the_group_color_unchanged(cx: &mut TestAppContext) {
        let (view, _, _, vcx) = open_strip(cx);
        for group in Group::ALL {
            link(&view, vcx, Some(group), None);
            let chip = vcx
                .debug_bounds(match group {
                    Group::A => "tile-link-3-A-follow",
                    Group::B => "tile-link-3-B-follow",
                    Group::C => "tile-link-3-C-follow",
                    Group::D => "tile-link-3-D-follow",
                })
                .expect("the chip is painted");
            let (fills, color) = vcx.update(|window, cx| {
                let at = chip.scale(window.scale_factor());
                let close = |a: gpui::ScaledPixels, b: gpui::ScaledPixels| {
                    (a.as_f32() - b.as_f32()).abs() < 0.5
                };
                let fills: Vec<Hsla> = window
                    .painted_quads()
                    .iter()
                    .filter(|q| {
                        close(q.bounds.origin.x, at.origin.x)
                            && close(q.bounds.origin.y, at.origin.y)
                            && close(q.bounds.size.width, at.size.width)
                            && close(q.bounds.size.height, at.size.height)
                    })
                    .filter_map(|q| q.background.as_solid())
                    .collect();
                (fills, group_color(cx.theme(), group))
            });
            assert_eq!(fills, vec![color], "group {}", group.letter());
        }
    }

    /// The tooltip says what the chip's letter and arrow mean, in words,
    /// for every role and group.
    #[test]
    fn the_link_chip_tooltip_names_the_role_and_the_group() {
        let title = |group, role| link_title(LinkChip { group, role });
        assert_eq!(title(Group::A, LinkRole::Follow), "Following group A");
        assert_eq!(title(Group::B, LinkRole::Emit), "Emitting into group B");
        assert_eq!(
            title(Group::D, LinkRole::Both),
            "Following and emitting into group D"
        );
        for group in Group::ALL {
            for role in [LinkRole::Follow, LinkRole::Emit, LinkRole::Both] {
                assert!(
                    title(group, role).ends_with(group.letter()),
                    "{group:?} {role:?}"
                );
            }
        }
    }

    /// Publish the builtin keymap as the live `Chords`, with `mod` as alt,
    /// the way the shell does at startup. Without it a tooltip resolves no
    /// chord and a test could not tell an action named from none.
    fn publish_chords(vcx: &mut VisualTestContext) {
        let mut registry = geode_shell::actions::ActionRegistry::default();
        geode_shell::defaults::register_builtin_actions(&mut registry);
        let builtin =
            geode_core::config::LayerDoc::builtin("keymap", geode_shell::defaults::BUILTIN_KEYMAP)
                .unwrap();
        let docs = geode_shell::keymap::fragments::splice(&[builtin], &[]);
        let (keymap, diags) = geode_shell::keymap::build_keymap(
            &docs,
            geode_shell::keymap::Modifiers::ALT,
            &registry,
        );
        assert!(diags.is_empty(), "{diags:?}");
        let bindings = keymap.bindings().to_vec();
        vcx.update(|_, cx| cx.set_global(tips::Chords(std::sync::Arc::new(bindings))));
        vcx.run_until_parked();
    }

    /// Hovering the chip shows that title and the chooser's key: the chip
    /// takes no press, so the tooltip is the only place its keyboard route
    /// is named.
    #[gpui::test]
    fn hovering_the_link_chip_names_the_group(cx: &mut TestAppContext) {
        let (view, _, _, vcx) = open_strip(cx);
        publish_chords(vcx);
        link(&view, vcx, Some(Group::C), None);
        let at = centre(vcx, "tile-link-3-C-follow");
        vcx.simulate_mouse_move(at, None, gpui::Modifiers::none());
        vcx.executor()
            .advance_clock(std::time::Duration::from_millis(600));
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("tip-tile-link-title").is_some());
        assert!(
            vcx.debug_bounds("tip-tile-link-chord-alt+u").is_some(),
            "the tooltip names the link group key"
        );
    }

    /// A press on the chip is not the chip's: it reaches the tile's own
    /// listeners, so clicking it focuses the tile like any other part of
    /// the header.
    #[gpui::test]
    fn a_press_on_the_link_chip_reaches_the_tile(cx: &mut TestAppContext) {
        let (view, _, _, vcx) = open_strip(cx);
        link(&view, vcx, Some(Group::A), None);
        let at = centre(vcx, "tile-link-3-A-follow");
        click(vcx, at);
        assert_eq!(parent_presses(&view, vcx), 1);
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

    /// A left side wider than the tile clips; the cluster — the health chip
    /// and `⋯` — stays inside the header.
    #[gpui::test]
    fn an_overlong_left_side_leaves_the_cluster_inside_the_header(cx: &mut TestAppContext) {
        let (view, diagnostics, _, vcx) = open_strip(cx);
        view.update(vcx, |s, cx| {
            s.left_runs = 40;
            cx.notify();
        });
        vcx.simulate_resize(gpui::size(gpui::px(480.0), gpui::px(300.0)));
        show_failure(&view, &diagnostics, vcx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let strip = vcx.debug_bounds("strip").expect("the strip is painted");
        for s in ["tile-health-3", "strip-menu"] {
            let b = vcx
                .debug_bounds(s)
                .unwrap_or_else(|| panic!("{s} is painted"));
            assert!(
                b.left() >= strip.left() && b.right() <= strip.right(),
                "{s} at {b:?} escapes the header {strip:?}"
            );
        }
    }

    /// A notice far wider than the tile cuts to one line inside its share:
    /// it ends before the time run, the time, chip and `⋯` stay inside the
    /// header, and the left side keeps a share of its own.
    #[gpui::test]
    fn an_overlong_notice_cuts_and_leaves_the_tail_and_left_side(cx: &mut TestAppContext) {
        let (view, diagnostics, _, vcx) = open_strip(cx);
        view.update(vcx, |s, cx| {
            s.notice = Notice::danger(
                "IO Error: Could not set lock on file \"/tmp/geode-demo/store.duckdb\": \
                 Conflicting lock is held in another process; see the concurrency docs",
            );
            cx.notify();
        });
        vcx.simulate_resize(gpui::size(gpui::px(640.0), gpui::px(300.0)));
        show_failure(&view, &diagnostics, vcx);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let rem = vcx.update(|window, _| window.rem_size());
        let strip = vcx.debug_bounds("strip").expect("the strip is painted");
        let height = f32::from(strip.size.height);
        assert!(
            (height - scale::design_px(HEADER_HEIGHT, rem)).abs() < 0.5,
            "{height}"
        );
        let notice = vcx
            .debug_bounds("tile-notice-3-0")
            .expect("the notice is painted");
        assert!(
            notice.top() >= strip.top() && notice.bottom() <= strip.bottom(),
            "the notice wraps out of the strip: {notice:?} in {strip:?}"
        );
        let time = vcx
            .debug_bounds("tile-time-3-0")
            .expect("the time is painted");
        assert!(
            notice.right() <= time.left(),
            "the notice {notice:?} runs into the time {time:?}"
        );
        for s in ["tile-time-3-0", "tile-health-3", "strip-menu"] {
            let b = vcx
                .debug_bounds(s)
                .unwrap_or_else(|| panic!("{s} is painted"));
            assert!(
                b.left() >= strip.left()
                    && b.right() <= strip.right()
                    && b.top() >= strip.top()
                    && b.bottom() <= strip.bottom(),
                "{s} at {b:?} escapes the header {strip:?}"
            );
        }
        let left = vcx
            .debug_bounds("tile-header-left-3")
            .expect("the left slot is painted");
        assert!(
            left.size.width > strip.size.width * 0.1,
            "the notice collapsed the left side: {left:?} of {strip:?}"
        );
    }

    #[test]
    fn the_header_reads_insert_as_edit_and_visual_as_visual() {
        assert_eq!(Mode::from_key_mode("insert"), Mode::Edit);
        assert_eq!(Mode::from_key_mode("visual"), Mode::Visual);
        assert_eq!(Mode::from_key_mode("normal"), Mode::Normal);
        assert_eq!(
            Mode::from_key_mode("menu"),
            Mode::Normal,
            "an open action menu is its own surface: no cue"
        );
    }

    fn set_mode(view: &Entity<Strip>, vcx: &mut VisualTestContext, mode: Mode) {
        view.update(vcx, |s, cx| {
            s.mode = mode;
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    /// Edit paints the edit icon, visual the visual icon — each alone —
    /// and normal paints neither. The icon leads the cluster: it sits
    /// before the status, which leads everything else.
    #[gpui::test]
    fn the_mode_icon_paints_per_mode_ahead_of_the_cluster(cx: &mut TestAppContext) {
        let (view, _, _, vcx) = open_strip(cx);
        let painted = |vcx: &mut VisualTestContext| {
            (
                vcx.debug_bounds("tile-mode-edit-3").is_some(),
                vcx.debug_bounds("tile-mode-visual-3").is_some(),
            )
        };
        set_mode(&view, vcx, Mode::Normal);
        assert_eq!(painted(vcx), (false, false), "normal: no cue");
        set_mode(&view, vcx, Mode::Edit);
        assert_eq!(painted(vcx), (true, false), "edit: the edit icon alone");
        let order =
            ["tile-header-left-3", "tile-mode-edit-3", "strip-status"].map(|s| centre(vcx, s).x);
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
        set_mode(&view, vcx, Mode::Visual);
        assert_eq!(painted(vcx), (false, true), "visual: the visual icon alone");
        let icon = centre(vcx, "tile-mode-visual-3").x;
        let status = centre(vcx, "strip-status").x;
        assert!(icon < status, "{icon:?} vs {status:?}");
    }

    /// Hovering the icon names the mode and the key that leaves it.
    #[gpui::test]
    fn hovering_the_mode_icon_names_the_mode(cx: &mut TestAppContext) {
        let (view, _, _, vcx) = open_strip(cx);
        set_mode(&view, vcx, Mode::Visual);
        let at = centre(vcx, "tile-mode-visual-3");
        vcx.simulate_mouse_move(at, MouseButton::Left, gpui::Modifiers::none());
        vcx.executor()
            .advance_clock(std::time::Duration::from_millis(600));
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("tip-tile-mode-visual-title").is_some());
        assert!(vcx.debug_bounds("tip-tile-mode-visual-detail").is_some());
        assert!(
            vcx.debug_bounds("kbd:escape").is_some(),
            "the detail's key paints as the escape chip"
        );
        assert_eq!(
            mode_tip(Mode::Visual),
            Some(("Visual selection", LEAVE_HINT))
        );
        assert_eq!(mode_tip(Mode::Edit), Some(("Editing", LEAVE_HINT)));
        assert_eq!(LEAVE_HINT, "`escape` leaves");
    }

    /// Each icon takes its tone's floored color — edit the warning text
    /// tone, visual the info text tone — both clear the readable floor
    /// against the header's ground, and the two differ, on every bundled
    /// theme.
    #[gpui::test]
    fn the_mode_icon_colors_clear_the_floor_on_every_bundled_theme(cx: &mut TestAppContext) {
        use geode_core::colour::{TEXT_READABLE_RATIO, contrast_ratio};
        use geode_shell::shell::colours::{over, to_rgb};
        cx.update(gpui_component::init);
        let (service, _) = geode_shell::theme::load_bundled();
        let mut worst: Option<(f32, String)> = None;
        let mut failures = Vec::new();
        let mut same = Vec::new();
        let mut themes = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                themes += 1;
                assert_eq!(mode_color(Mode::Normal, theme), None);
                let edit = mode_color(Mode::Edit, theme);
                let visual = mode_color(Mode::Visual, theme);
                if edit == visual {
                    same.push(name.to_string());
                }
                for (mode, tone) in [
                    (Mode::Edit, chip::Tone::WarningText),
                    (Mode::Visual, chip::Tone::InfoText),
                ] {
                    let color = mode_color(mode, theme).expect("a mode has a color");
                    assert_eq!(color, chip_paint(theme, tone).text, "{name}: {mode:?}");
                    let ground = to_rgb(theme.background);
                    let ratio = contrast_ratio(over(color, ground), ground);
                    if ratio < TEXT_READABLE_RATIO {
                        failures.push(format!("{name}: {mode:?} at {ratio:.2}:1"));
                    }
                    if worst.as_ref().is_none_or(|(w, _)| ratio < *w) {
                        worst = Some((ratio, format!("{name} {mode:?}")));
                    }
                }
            });
        }
        assert!(themes >= 40, "the sweep saw {themes} themes");
        assert!(
            failures.is_empty(),
            "faint mode icons:\n{}",
            failures.join("\n")
        );
        assert!(same.is_empty(), "edit and visual share a color: {same:?}");
        eprintln!("worst mode icon contrast: {worst:?}");
    }
}
