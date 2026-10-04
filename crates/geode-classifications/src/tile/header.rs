//! The tile's header: the classification it shows (a press, or `g c`,
//! opens the switcher hung beneath it), the source column it maps, how
//! many source values the grid holds and how many of them are
//! unclassified, the layer its definition comes from, and the shared right
//! cluster with `⋯` and ×.
//!
//! [`HeaderModel::prepare`] formats every string when the configuration or
//! the shown classification changes; paint clones prepared strings.

use std::rc::Rc;

use geode_core::config::Layer;
use geode_core::dimensions::DerivedDimension;
use geode_shell::actions::ActionId;
use geode_shell::module::{CloseHandle, StackHandle};
use geode_shell::shell::control::{self, PointerStates as _};
use geode_shell::tiling::TileId;
use geode_shell::tips;
use geode_tile::header::{Cluster, MenuTrigger, Mode, OnPress, TileLinks};
use geode_tile::notice::Notice;
use gpui::prelude::*;
use gpui::{AnyElement, Div, ElementId, Entity, SharedString, div};
use gpui_component::{Theme, h_flex};

use crate::tile::ClassificationsTile;

/// What the switch control says while no classification is shown.
pub(crate) const NONE_SHOWN: &str = "Classifications";

/// Everything the header's left side paints, prepared once per change.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct HeaderModel {
    /// The classification shown, `None` while there is none.
    pub name: Option<SharedString>,
    /// The source column it maps.
    pub from: Option<SharedString>,
    /// `<n> values`: how many source values the grid holds (the map's and
    /// the data's together).
    pub values: Option<SharedString>,
    /// `<k> unclassified`, while any value is.
    pub unclassified: Option<SharedString>,
    /// The layer its winning definition comes from.
    pub layer: Option<&'static str>,
    /// A values read is on its way: the counts are the map's alone until
    /// it answers.
    pub loading: bool,
}

/// What the header says while the values read is on its way.
pub(crate) const LOADING: &str = "loading values\u{2026}";

impl HeaderModel {
    /// `counts` is the grid's `(values, unclassified)`.
    pub(crate) fn prepare(
        dim: Option<&DerivedDimension>,
        layer: Option<Layer>,
        counts: (usize, usize),
    ) -> Self {
        let Some(dim) = dim else {
            return HeaderModel::default();
        };
        let (n, k) = counts;
        HeaderModel {
            name: Some(dim.name.clone().into()),
            from: Some(dim.from.clone().into()),
            values: Some(
                if n == 1 {
                    "1 value".to_string()
                } else {
                    format!("{n} values")
                }
                .into(),
            ),
            unclassified: (k > 0).then(|| format!("{k} unclassified").into()),
            layer: layer.map(Layer::name),
            loading: false,
        }
    }

    /// Mark the values read as on its way (or not).
    pub(crate) fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    /// The left side's words in paint order, for tests that read the header
    /// rather than its painted elements.
    #[cfg(test)]
    pub(crate) fn text(&self) -> String {
        let mut parts = vec![match &self.name {
            Some(n) => format!("Classification: {n}"),
            None => NONE_SHOWN.to_string(),
        }];
        parts.extend(self.from.iter().map(|f| format!("from {f}")));
        parts.extend(self.values.iter().map(|v| v.to_string()));
        parts.extend(self.loading.then(|| LOADING.to_string()));
        parts.extend(self.unclassified.iter().map(|v| v.to_string()));
        parts.extend(self.layer.map(str::to_string));
        parts.join(" \u{00b7} ")
    }
}

/// Live inputs the tile holds and the header only installs handlers on.
pub(crate) struct HeaderChrome<'a> {
    pub tile: &'a Entity<ClassificationsTile>,
    pub tile_id: TileId,
    pub stack: Option<&'a StackHandle>,
    pub close: Option<&'a CloseHandle>,
    pub mode: Mode,
    pub links: TileLinks,
    pub notices: Vec<Notice>,
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
fn dispatch(tile: &Entity<ClassificationsTile>, action: &'static str) -> OnPress {
    let tile = tile.clone();
    Rc::new(move |window, cx| {
        tile.update(cx, |t, cx| {
            t.dispatch(&ActionId(action.to_string()), None, window, cx);
        });
    })
}

/// The muted `Classification:` label, the shown name and a chevron, one
/// control: a press toggles the switcher (`classifications::switch`, the
/// palette's route too), which hangs from the control's bottom-left edge.
/// The press is taken in the capture phase, ahead of the open switcher's
/// outside-press closer: in the bubble phase the closing press would find
/// the switcher already closed and reopen it.
fn switch_control(h: &HeaderModel, c: &mut HeaderChrome, theme: &Theme) -> AnyElement {
    let tile_id = c.tile_id.0;
    let switcher = c.switcher.take();
    let open = switcher.is_some();
    let on_press = dispatch(c.tile, "classifications::switch");
    let words = match &h.name {
        Some(name) => h_flex()
            .gap_1()
            .child(
                div()
                    .text_color(theme.muted_foreground)
                    .child("Classification:"),
            )
            .child(div().text_color(theme.foreground).child(name.clone())),
        None => h_flex().text_color(theme.foreground).child(NONE_SHOWN),
    };
    let control = h_flex()
        .id(ElementId::NamedInteger(
            SharedString::new_static("classifications-switch"),
            tile_id,
        ))
        .flex_none()
        .gap_1()
        .px_1()
        .rounded(theme.radius_tokens().sm)
        .debug_selector(move || format!("classifications-switch-{tile_id}"))
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
            SharedString::new_static("Switch classification"),
            Some("classifications::switch"),
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
        .debug_selector(move || format!("classifications-layer-{tile_id}-{name}"))
        .child(layer)
}

pub(crate) fn render(h: &HeaderModel, mut c: HeaderChrome, theme: &Theme) -> Div {
    let tile_id = c.tile_id.0;
    let switch = switch_control(h, &mut c, theme);
    let muted = theme.muted_foreground;
    let left = h_flex()
        .items_center()
        .gap_3()
        .child(switch)
        .when_some(h.from.clone(), |el, from| {
            el.child(
                h_flex()
                    .gap_1()
                    .flex_none()
                    .debug_selector(move || format!("classifications-from-{tile_id}"))
                    .child(div().text_color(muted).child("from"))
                    .child(div().text_color(theme.foreground).child(from)),
            )
        })
        .when_some(h.values.clone(), |el, values| {
            el.child(div().flex_none().text_color(muted).child(values))
        })
        // Muted and in words, beside the count it qualifies: the grid shows
        // the map alone meanwhile, and a mark that moved would read as news.
        .when(h.loading, |el| {
            el.child(
                div()
                    .flex_none()
                    .text_color(muted)
                    .debug_selector(move || format!("classifications-loading-{tile_id}"))
                    .child(LOADING),
            )
        })
        .when_some(h.unclassified.clone(), |el, k| {
            el.child(
                div()
                    .flex_none()
                    .text_color(muted)
                    .debug_selector(move || format!("classifications-unclassified-{tile_id}"))
                    .child(k),
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
    cluster.menu = Some(MenuTrigger {
        id: ElementId::NamedInteger(
            SharedString::new_static("classifications-menu-button"),
            tile_id,
        ),
        selector: c.menu_selector,
        tip_selector: c.menu_tip,
        action: "classifications::menu",
        open: c.actions_open,
        on_press: dispatch(c.tile, "classifications::menu"),
    });
    geode_tile::header::frame(
        c.stack.and_then(|s| s.marker(theme, c.tile_id)),
        left,
        cluster,
        theme,
    )
    .debug_selector(move || format!("classifications-header-{tile_id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn the_header_names_the_classification_its_source_count_and_layer() {
        let dim = DerivedDimension {
            name: "desk".into(),
            from: "book".into(),
            values: BTreeMap::from([("B1".into(), "Flow".into()), ("B2".into(), "Flow".into())]),
        };
        let h = HeaderModel::prepare(Some(&dim), Some(Layer::User), (2, 0));
        assert_eq!(
            h.text(),
            "Classification: desk \u{00b7} from book \u{00b7} 2 values \u{00b7} user"
        );
        assert!(
            HeaderModel::prepare(Some(&dim), None, (1, 0))
                .text()
                .ends_with("1 value")
        );
        assert_eq!(
            HeaderModel::prepare(Some(&dim), None, (3, 1)).text(),
            "Classification: desk \u{00b7} from book \u{00b7} 3 values \u{00b7} 1 unclassified"
        );
        assert_eq!(HeaderModel::prepare(None, None, (0, 0)).text(), NONE_SHOWN);
        assert_eq!(
            HeaderModel::prepare(Some(&dim), None, (2, 0))
                .loading(true)
                .text(),
            "Classification: desk \u{00b7} from book \u{00b7} 2 values \u{00b7} loading values\u{2026}"
        );
    }
}
