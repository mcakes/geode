//! "Open in Nemo": the row menu actions that hand the desk's pricing app
//! a position or an instrument by URL (`nemo://position/{id}`,
//! `nemo://instrument/{id}`). The URL shapes are ours until Nemo's own
//! scheme is known; the id is percent-encoded so any id stays one path
//! segment. The notice says what Geode did (`opened <url>`), never that
//! Nemo launched: the OS reports nothing back.

use std::rc::Rc;

use geode_core::context::DimensionContext;
use geode_shell::dimension::DimensionAction;
use geode_shell::shell::row_menu::ActionCx;
use gpui::SharedString;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

/// A position's URL prefix.
pub const POSITION_URL: &str = "nemo://position/";
/// An instrument's URL prefix.
pub const INSTRUMENT_URL: &str = "nemo://instrument/";

/// RFC 3986 unreserved characters pass; everything else is encoded.
const ID: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// `prefix` followed by `id`, percent-encoded as one path segment.
pub fn url(prefix: &str, id: &str) -> String {
    format!("{prefix}{}", utf8_percent_encode(id, ID))
}

/// "Open in Nemo" on one column's value.
pub struct OpenInNemo {
    column: &'static str,
    prefix: &'static str,
    id: &'static str,
}

impl OpenInNemo {
    pub fn position() -> Self {
        Self {
            column: "position_ref",
            prefix: POSITION_URL,
            id: "nemo::open_position",
        }
    }
    pub fn instrument() -> Self {
        Self {
            column: "instrument_ref",
            prefix: INSTRUMENT_URL,
            id: "nemo::open_instrument",
        }
    }
}

impl DimensionAction for OpenInNemo {
    fn id(&self) -> &'static str {
        self.id
    }
    fn title(&self) -> SharedString {
        SharedString::new_static("Open in Nemo")
    }
    fn column(&self) -> &'static str {
        self.column
    }
    /// The target row's value only; a selection is ignored.
    fn run(&self, ctx: &DimensionContext, acx: &mut ActionCx<'_, '_>) {
        let Some(id) = ctx.get(self.column) else {
            return;
        };
        let url = url(self.prefix, id);
        acx.open_url(&url);
        acx.notice(format!("opened {url}"));
    }
}

/// Both actions, position first, for `ModuleRoster::add_action`.
pub fn actions() -> Vec<Rc<dyn DimensionAction>> {
    vec![
        Rc::new(OpenInNemo::position()),
        Rc::new(OpenInNemo::instrument()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_id_is_appended() {
        assert_eq!(url(POSITION_URL, "P7"), "nemo://position/P7");
        assert_eq!(
            url(INSTRUMENT_URL, "SPX-Z26_5000.C~1"),
            "nemo://instrument/SPX-Z26_5000.C~1"
        );
    }

    #[test]
    fn ids_are_percent_encoded_as_one_path_segment() {
        assert_eq!(url(POSITION_URL, "P 7/8"), "nemo://position/P%207%2F8");
        assert_eq!(url(POSITION_URL, "a#b?c"), "nemo://position/a%23b%3Fc");
        assert_eq!(url(POSITION_URL, "é"), "nemo://position/%C3%A9");
    }

    #[test]
    fn the_actions_sit_in_their_columns_under_one_title() {
        let a = actions();
        let shape: Vec<(&str, &str, String)> = a
            .iter()
            .map(|x| (x.id(), x.column(), x.title().to_string()))
            .collect();
        assert_eq!(
            shape,
            vec![
                (
                    "nemo::open_position",
                    "position_ref",
                    "Open in Nemo".to_string()
                ),
                (
                    "nemo::open_instrument",
                    "instrument_ref",
                    "Open in Nemo".to_string()
                ),
            ]
        );
    }
}
