//! The tile's header: the watchlist it shows (a press, or `g w`, opens the
//! switcher hung beneath it), how many live names it holds, how many rules
//! define it (warning tone while any is bad), its resolution state
//! (`resolving…`, or `as of <time>` on the display clock, in the warning
//! tone with `failed` after a failed resolution), the layer its definition
//! comes from, and the shared right cluster with `⋯` and ×.
//!
//! [`HeaderModel::prepare`] formats every string when the snapshot, the
//! clock or the shown list changes; paint clones prepared strings.

use std::rc::Rc;

use geode_core::clock::Clock;
use geode_core::config::Layer;
use geode_core::watchlist::state::{Status, WatchlistState};
use geode_shell::actions::ActionId;
use geode_shell::module::{CloseHandle, StackHandle};
use geode_shell::shell::control::{self, PointerStates as _};
use geode_shell::tiling::TileId;
use geode_shell::tips;
use geode_tile::header::{Cluster, MenuTrigger, Mode, OnPress, TileLinks};
use geode_tile::notice::{self, Notice, OnDismiss, Tone};
use gpui::prelude::*;
use gpui::{AnyElement, Div, ElementId, Entity, SharedString, div};
use gpui_component::{Theme, h_flex};

use crate::tile::WatchlistTile;

/// What the switch control says while no watchlist is shown.
pub(crate) const NONE_SHOWN: &str = "Watchlists";

/// What the header says while the list's resolution is on its way.
pub(crate) const RESOLVING: &str = "resolving\u{2026}";

/// What the header says after a resolution failed whole.
pub(crate) const FAILED: &str = "failed";

/// Everything the header's left side paints, prepared once per change.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct HeaderModel {
    /// The watchlist shown, `None` while there is none.
    pub name: Option<SharedString>,
    /// `<n> names`: the live members (excluded ones left out).
    pub names: Option<SharedString>,
    /// `<k> rules`, and whether any of them is bad.
    pub rules: Option<(SharedString, bool)>,
    /// `resolving…` while an answer is on its way, `failed` after a
    /// failed one; `None` while the members are current.
    pub resolution: Option<SharedString>,
    /// `as of <time>`: when the members were last resolved, on the display
    /// clock. Shown with a current or failed resolution, not while one is
    /// on its way.
    pub as_of: Option<SharedString>,
    /// The last resolution failed: `resolution` and `as_of` paint in the
    /// warning tone.
    pub failed: bool,
    /// The layer its winning definition comes from.
    pub layer: Option<&'static str>,
}

impl HeaderModel {
    /// `name` is the list shown; `state` its snapshot entry, `None` while
    /// the snapshot no longer holds it; `live` the grid's live count (the
    /// rows as shown, a pending edit included, not what the snapshot last
    /// resolved).
    pub(crate) fn prepare(
        name: Option<&str>,
        state: Option<&WatchlistState>,
        live: usize,
        clock: &Clock,
    ) -> Self {
        let (Some(name), Some(state)) = (name, state) else {
            return HeaderModel::default();
        };
        let n = live;
        let k = state.definition.rules.len();
        let as_of = state
            .resolved_at
            .map(|t| SharedString::from(format!("as of {}", clock.hms(t))));
        let (resolution, as_of, failed) = match &state.status {
            Status::Resolving => (Some(SharedString::new_static(RESOLVING)), None, false),
            Status::Current => (None, as_of, false),
            Status::Failed(_) => (Some(SharedString::new_static(FAILED)), as_of, true),
        };
        HeaderModel {
            name: Some(name.to_string().into()),
            names: Some(plural(n, "name").into()),
            rules: Some((plural(k, "rule").into(), !state.rule_errors.is_empty())),
            resolution,
            as_of,
            failed,
            layer: state.layer.map(Layer::name),
        }
    }

    /// The left side's words in paint order, for tests that read the header
    /// rather than its painted elements.
    #[cfg(test)]
    pub(crate) fn text(&self) -> String {
        let mut parts = vec![match &self.name {
            Some(n) => format!("Watchlist: {n}"),
            None => NONE_SHOWN.to_string(),
        }];
        parts.extend(self.names.iter().map(|v| v.to_string()));
        parts.extend(self.rules.iter().map(|(v, _)| v.to_string()));
        parts.extend(self.resolution.iter().map(|v| v.to_string()));
        parts.extend(self.as_of.iter().map(|v| v.to_string()));
        parts.extend(self.layer.map(str::to_string));
        parts.join(" \u{00b7} ")
    }
}

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Live inputs the tile holds and the header only installs handlers on.
pub(crate) struct HeaderChrome<'a> {
    pub tile: &'a Entity<WatchlistTile>,
    pub tile_id: TileId,
    pub stack: Option<&'a StackHandle>,
    pub close: Option<&'a CloseHandle>,
    pub mode: Mode,
    pub links: TileLinks,
    /// The notices to paint: the reported ones less those dismissed.
    pub notices: Vec<Notice>,
    /// A press on a warning or danger notice dismisses it.
    pub on_dismiss: OnDismiss,
    /// The `⋯` menu is up: its control keeps the selected fill.
    pub actions_open: bool,
    /// The open switcher, rendered by the tile, hung from the name.
    pub switcher: Option<AnyElement>,
    /// Built once with the tile: the `⋯` control's selector and tooltip
    /// selector, and the name's tooltip selector.
    pub menu_selector: SharedString,
    pub menu_tip: SharedString,
    pub switch_tip: SharedString,
}

/// Dispatch `action` through the tile's own door, as its key would.
fn dispatch(tile: &Entity<WatchlistTile>, action: &'static str) -> OnPress {
    let tile = tile.clone();
    Rc::new(move |window, cx| {
        tile.update(cx, |t, cx| {
            t.dispatch(&ActionId(action.to_string()), None, window, cx);
        });
    })
}

/// The muted `Watchlist:` label, the shown name and a chevron, one
/// control: a press toggles the switcher (`watchlist::switch`, the
/// palette's route too), which hangs from the control's bottom-left edge.
/// The press is taken in the capture phase, ahead of the open switcher's
/// outside-press closer: in the bubble phase the closing press would find
/// the switcher already closed and reopen it.
fn switch_control(h: &HeaderModel, c: &mut HeaderChrome, theme: &Theme) -> AnyElement {
    let tile_id = c.tile_id.0;
    let switcher = c.switcher.take();
    let open = switcher.is_some();
    let on_press = dispatch(c.tile, "watchlist::switch");
    let words = match &h.name {
        Some(name) => h_flex()
            .gap_1()
            .child(div().text_color(theme.muted_foreground).child("Watchlist:"))
            .child(div().text_color(theme.foreground).child(name.clone())),
        None => h_flex().text_color(theme.foreground).child(NONE_SHOWN),
    };
    let control = h_flex()
        .id(ElementId::NamedInteger(
            SharedString::new_static("watchlist-switch"),
            tile_id,
        ))
        .flex_none()
        .gap_1()
        .px_1()
        .rounded(theme.radius_tokens().sm)
        .debug_selector(move || format!("watchlist-switch-{tile_id}"))
        // Open keeps a selected fill, as the `⋯` trigger does.
        .when(open, |el| el.bg(theme.secondary))
        .when(!open, |el| {
            el.pointer_states(control::paint(
                theme,
                control::Rest::Bare,
                theme.background,
                theme.foreground,
            ))
        })
        .capture_any_mouse_down(move |event, window, cx| {
            if event.button == gpui::MouseButton::Left {
                on_press(window, cx);
            }
        })
        .tooltip(tips::tip_with(
            c.switch_tip.clone(),
            SharedString::new_static("Switch watchlist"),
            Some("watchlist::switch"),
            None,
        ))
        .child(words)
        .child(div().text_color(theme.muted_foreground).child("\u{25be}"));
    div()
        .relative()
        .flex_none()
        .child(control)
        .when_some(switcher, |el, m| {
            el.child(div().absolute().left_0().bottom_0().child(m))
        })
        .into_any_element()
}

/// The layer badge: outlined, monospace, no fill — the object dialog's
/// layer marker, so one definition reads the same in both places.
fn layer_badge(layer: &'static str, name: &SharedString, tile_id: u64, theme: &Theme) -> Div {
    // Formatted only when a test asks for the selector, not every paint.
    let name = name.clone();
    div()
        .flex_none()
        .font_family(geode_shell::fonts::MONO)
        .text_xs()
        .text_color(theme.muted_foreground)
        .border_1()
        .border_color(theme.border)
        .px_1()
        .rounded(theme.radius_tokens().sm)
        .debug_selector(move || format!("watchlist-layer-{tile_id}-{name}"))
        .child(layer)
}

pub(crate) fn render(h: &HeaderModel, mut c: HeaderChrome, theme: &Theme) -> Div {
    let tile_id = c.tile_id.0;
    let switch = switch_control(h, &mut c, theme);
    let muted = theme.muted_foreground;
    let warning = notice::color(Tone::Warning, theme);
    let tone = |warn: bool| if warn { warning } else { muted };
    let left = h_flex()
        .items_center()
        .gap_3()
        .child(switch)
        .when_some(h.names.clone(), |el, names| {
            el.child(
                div()
                    .flex_none()
                    .text_color(muted)
                    .debug_selector(move || format!("watchlist-names-{tile_id}"))
                    .child(names),
            )
        })
        .when_some(h.rules.clone(), |el, (rules, bad)| {
            el.child(
                div()
                    .flex_none()
                    .text_color(tone(bad))
                    .debug_selector(move || format!("watchlist-rules-{tile_id}"))
                    .child(rules),
            )
        })
        .when_some(h.resolution.clone(), |el, state| {
            el.child(
                div()
                    .flex_none()
                    .text_color(tone(h.failed))
                    .debug_selector(move || format!("watchlist-resolution-{tile_id}"))
                    .child(state),
            )
        })
        .when_some(h.as_of.clone(), |el, as_of| {
            el.child(
                div()
                    .flex_none()
                    .text_color(tone(h.failed))
                    .debug_selector(move || format!("watchlist-asof-{tile_id}"))
                    .child(as_of),
            )
        })
        .when_some(h.layer.zip(h.name.as_ref()), |el, (layer, name)| {
            el.child(layer_badge(layer, name, tile_id, theme))
        });
    let mut cluster = Cluster::new(c.tile_id);
    cluster.close = c.close.cloned();
    cluster.mode = c.mode;
    cluster.links = c.links;
    cluster.notices = c.notices;
    cluster.on_dismiss = Some(c.on_dismiss);
    cluster.menu = Some(MenuTrigger {
        id: ElementId::NamedInteger(SharedString::new_static("watchlist-menu-button"), tile_id),
        selector: c.menu_selector,
        tip_selector: c.menu_tip,
        action: "watchlist::menu",
        open: c.actions_open,
        on_press: dispatch(c.tile, "watchlist::menu"),
    });
    geode_tile::header::frame(
        c.stack.and_then(|s| s.marker(theme, c.tile_id)),
        left,
        cluster,
        theme,
    )
    .debug_selector(move || format!("watchlist-header-{tile_id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::watchlist::fold::RuleError;
    use geode_core::watchlist::members::{Member, Origin};
    use geode_core::watchlist::{Rule, Watchlist};

    fn member(name: &str, origin: Origin) -> Member {
        Member {
            name: name.into(),
            origin,
        }
    }

    fn resolved_at() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-10-10T14:03:12Z")
            .unwrap()
            .to_utc()
    }

    /// Three live members and one excluded, two rules with one bad, a
    /// current resolution and a desk definition.
    fn state() -> WatchlistState {
        WatchlistState {
            definition: Watchlist {
                include: vec!["SMI".into()],
                exclude: vec!["UKX".into()],
                rules: vec![Rule::default(), Rule::default()],
            },
            layer: Some(Layer::Desk),
            shadowed: None,
            rule_errors: vec![RuleError {
                index: 1,
                reason: "unknown dataset".into(),
            }],
            members: vec![
                member("DAX", Origin::Rules(vec![0])),
                member("SMI", Origin::Manual),
                member("SX5E", Origin::Rules(vec![0])),
                member(
                    "UKX",
                    Origin::Excluded {
                        rules: vec![0],
                        manual: false,
                    },
                ),
            ],
            resolved_at: Some(resolved_at()),
            status: Status::Current,
        }
    }

    #[test]
    fn the_header_shows_counts_state_and_layer() {
        let utc = Clock::utc();
        let h = HeaderModel::prepare(Some("europe_risk"), Some(&state()), 3, &utc);
        assert_eq!(
            h.text(),
            "Watchlist: europe_risk \u{00b7} 3 names \u{00b7} 2 rules \u{00b7} as of 14:03:12 \u{00b7} desk"
        );
        assert_eq!(
            h.rules.as_ref().map(|(_, bad)| *bad),
            Some(true),
            "a bad rule paints the count in the warning tone"
        );
        assert!(!h.failed);
        // The time is the display clock's, not UTC's.
        let tokyo = Clock::in_zone_named("Asia/Tokyo");
        let h = HeaderModel::prepare(Some("europe_risk"), Some(&state()), 3, &tokyo);
        assert_eq!(h.as_of.as_deref(), Some("as of 23:03:12"));
        // Nothing shown, or a list the snapshot no longer holds.
        assert_eq!(HeaderModel::prepare(None, None, 0, &utc).text(), NONE_SHOWN);
        assert_eq!(
            HeaderModel::prepare(Some("gone"), None, 0, &utc).text(),
            NONE_SHOWN
        );
    }

    #[test]
    fn the_header_says_resolving_and_failed() {
        let utc = Clock::utc();
        let mut s = state();
        s.status = Status::Resolving;
        let h = HeaderModel::prepare(Some("a"), Some(&s), 3, &utc);
        assert_eq!(h.resolution.as_deref(), Some(RESOLVING));
        assert_eq!(h.as_of, None, "the time is the old answer's");
        assert!(!h.failed);
        s.status = Status::Failed("timed out".into());
        let h = HeaderModel::prepare(Some("a"), Some(&s), 3, &utc);
        assert_eq!(
            h.text(),
            "Watchlist: a \u{00b7} 3 names \u{00b7} 2 rules \u{00b7} failed \u{00b7} as of 14:03:12 \u{00b7} desk"
        );
        assert!(h.failed);
        // Singulars, no rules, no errors, no layer, never resolved.
        let s = WatchlistState {
            definition: Watchlist {
                include: vec!["SPX".into()],
                ..Watchlist::default()
            },
            layer: None,
            shadowed: None,
            rule_errors: vec![],
            members: vec![member("SPX", Origin::Manual)],
            resolved_at: None,
            status: Status::Current,
        };
        let h = HeaderModel::prepare(Some("one"), Some(&s), 1, &utc);
        assert_eq!(h.text(), "Watchlist: one \u{00b7} 1 name \u{00b7} 0 rules");
        assert_eq!(h.rules.as_ref().map(|(_, bad)| *bad), Some(false));
    }
}
