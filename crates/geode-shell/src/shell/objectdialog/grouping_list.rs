//! The Groupings dialog's list. Besides the nine slots it leads with two rows
//! that are not configuration objects: the view default and the lane's ad
//! hoc chain. This module names those rows, says what applying a row does to
//! the frame, and owns the list's own keys; the shared browse and edit
//! handlers call in through `Domain::applies_from_browse`.

use geode_core::groupings::GroupingSlots;

use super::ObjectRow;
use crate::frame::{FrameView, GroupingChoice};

/// The view-default row's name. `0` is also its key, as `ctrl+0` is its chord.
pub const VIEW_DEFAULT: &str = "0";
/// The ad hoc row's name, and the marker the toolbar readout uses for it.
pub const AD_HOC: &str = "*";
pub const VIEW_DEFAULT_TEXT: &str = "view default";
pub const NO_AD_HOC_TEXT: &str = "no ad hoc chain";
/// The view-default row's right-hand note: what choosing it means.
pub const VIEW_DEFAULT_NOTE: &str = "each view's own grouping";

/// Which row of the list a name is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    ViewDefault,
    AdHoc,
    Slot(u8),
}

pub fn kind_of(name: &str) -> Option<RowKind> {
    match name {
        VIEW_DEFAULT => Some(RowKind::ViewDefault),
        AD_HOC => Some(RowKind::AdHoc),
        _ => name
            .parse::<u8>()
            .ok()
            .filter(|n| (1..=9).contains(n))
            .map(RowKind::Slot),
    }
}

/// Whether `name` is one of the two rows the frame supplies rather than the config.
pub fn is_lead(name: &str) -> bool {
    name == VIEW_DEFAULT || name == AD_HOC
}

/// The two leading rows for `frame`'s lane. Neither has a layer: nothing in
/// any config document defines them, so `d` and `r` have nothing to act on.
pub fn lead_rows(frame: &FrameView<'_>) -> Vec<ObjectRow> {
    let ad_hoc = frame
        .ad_hoc()
        .map(GroupingSlots::label_of)
        .unwrap_or_else(|| NO_AD_HOC_TEXT.to_string());
    [
        (VIEW_DEFAULT, VIEW_DEFAULT_TEXT.to_string()),
        (AD_HOC, ad_hoc),
    ]
    .into_iter()
    .map(|(name, summary)| ObjectRow {
        name: name.to_string(),
        summary,
        layer: None,
        overridden: false,
        drifted: false,
        prefix: None,
    })
    .collect()
}

/// The name of the row standing for the lane's choice.
pub fn active_row(choice: GroupingChoice) -> String {
    match choice {
        GroupingChoice::ViewDefault => VIEW_DEFAULT.to_string(),
        GroupingChoice::AdHoc => AD_HOC.to_string(),
        GroupingChoice::Slot(n) => n.to_string(),
    }
}

/// Whether the row holds a chain to apply, edit or save: a configured slot,
/// or the ad hoc row once a chain is stored. The view default never does.
pub fn has_chain(row: &ObjectRow) -> bool {
    match kind_of(&row.name) {
        Some(RowKind::Slot(_)) => row.layer.is_some(),
        Some(RowKind::AdHoc) => row.summary != NO_AD_HOC_TEXT,
        Some(RowKind::ViewDefault) | None => false,
    }
}

/// How an edit stage names its object on screen.
pub fn title_of(name: &str) -> String {
    if name == AD_HOC {
        "ad hoc".to_string()
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Frame;
    use geode_core::scopes::SavedScopes;

    fn frame() -> Frame {
        let mut slots = GroupingSlots::default();
        slots.set(3, vec!["lhu".into()]);
        Frame::new(slots, SavedScopes::new(), None)
    }

    #[test]
    fn names_resolve_to_their_row_kind() {
        assert_eq!(kind_of("0"), Some(RowKind::ViewDefault));
        assert_eq!(kind_of("*"), Some(RowKind::AdHoc));
        assert_eq!(kind_of("7"), Some(RowKind::Slot(7)));
        assert_eq!(kind_of("10"), None);
        assert_eq!(kind_of("book"), None);
        assert!(is_lead("0") && is_lead("*") && !is_lead("1"));
    }

    #[test]
    fn the_lead_rows_are_the_view_default_then_the_ad_hoc_chain() {
        let mut f = frame();
        let rows = lead_rows(&f.shared());
        assert_eq!(
            rows.iter()
                .map(|r| (r.name.as_str(), r.summary.as_str()))
                .collect::<Vec<_>>(),
            [("0", "view default"), ("*", "no ad hoc chain")]
        );
        assert!(rows.iter().all(|r| r.layer.is_none()));
        assert!(!has_chain(&rows[1]));

        f.shared_mut()
            .set_ad_hoc(vec!["underlying_ref".into(), "expiry".into()]);
        let rows = lead_rows(&f.shared());
        assert_eq!(rows[1].summary, "underlying_ref / expiry");
        assert!(has_chain(&rows[1]));
        assert!(!has_chain(&rows[0]), "the view default has no chain");
    }

    #[test]
    fn the_active_row_names_the_lanes_choice() {
        assert_eq!(active_row(GroupingChoice::ViewDefault), "0");
        assert_eq!(active_row(GroupingChoice::AdHoc), "*");
        assert_eq!(active_row(GroupingChoice::Slot(3)), "3");
    }
}
