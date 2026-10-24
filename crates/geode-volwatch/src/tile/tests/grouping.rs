//! Grouping: the frame's chain, the pins, the dimensions reload, the
//! header chip and the session, each through the shell's doors.

use super::harness::*;
use crate::core::grouping::Pin;
use crate::core::session::{self, State};
use geode_core::link::{CursorScope, path_scope};
use geode_shell::module::FindEvent;
use std::cell::Cell;
use std::collections::BTreeSet;
use std::rc::Rc;

fn grouped(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
    let (h, mut vcx) = landed(cx);
    h.set_dims(&mut vcx, europe_dims());
    h.set_frame_chain(&mut vcx, &["region"]);
    (h, vcx)
}

fn shown(h: &Harness, vcx: &gpui::VisualTestContext) -> Vec<String> {
    h.shown(vcx)
}

#[gpui::test]
fn the_frame_grouping_folds_members_under_groups_with_averaged_cells(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = grouped(cx);
    assert_eq!(
        shown(&h, &vcx),
        ["group:Core", "DAX", "SX5E", "group:\u{2014}", "NEW"]
    );
    assert_eq!(h.grouping_chip(&vcx), "region");
    let level = h.column_index(&vcx, "atfm-1W-level");
    let (text, name) = h.tile.read_with(&vcx, |t, _| {
        let row = &t.prepared.rows[0];
        (
            row.cells[level].text.to_string(),
            row.cells[0].text.to_string(),
        )
    });
    assert_eq!(text, "15.9", "the mean of two 15.9 levels");
    assert_eq!(name, "Core \u{b7} 2");
    assert!(
        h.requests().is_empty(),
        "a regroup never asks the board again"
    );
    h.set_frame_chain(&mut vcx, &[]);
    assert_eq!(shown(&h, &vcx), ["DAX", "SX5E", "NEW"]);
    assert_eq!(h.grouping_chip(&vcx), "ungrouped");
}

#[gpui::test]
fn colon_group_pins_and_a_frame_change_does_not_regroup_until_unpin(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = grouped(cx);
    h.command(&mut vcx, "group region").unwrap();
    assert_eq!(h.grouping_chip(&vcx), "pinned region");
    h.set_frame_chain(&mut vcx, &["sector"]);
    assert_eq!(shown(&h, &vcx)[0], "group:Core", "the pin holds");
    h.command(&mut vcx, "unpin").unwrap();
    assert_eq!(shown(&h, &vcx)[0], "group:Index", "released to the frame");
    assert_eq!(h.grouping_chip(&vcx), "sector");
}

#[gpui::test]
fn colon_group_none_flattens_while_the_frame_stays_grouped(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = grouped(cx);
    h.command(&mut vcx, "group none").unwrap();
    assert_eq!(shown(&h, &vcx), ["DAX", "SX5E", "NEW"]);
    assert_eq!(h.grouping_chip(&vcx), "pinned ungrouped");
    h.set_frame_chain(&mut vcx, &["sector"]);
    assert_eq!(shown(&h, &vcx), ["DAX", "SX5E", "NEW"]);
    h.command(&mut vcx, "unpin").unwrap();
    assert_eq!(shown(&h, &vcx)[0], "group:Index");
}

#[gpui::test]
fn colon_group_slot_reads_the_slot_and_refuses_an_empty_one(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.set_dims(&mut vcx, europe_dims());
    assert_eq!(
        h.command(&mut vcx, "group slot 2").unwrap_err(),
        "slot 2 is empty"
    );
    h.set_slot(&mut vcx, 1, &["region"]);
    h.command(&mut vcx, "group slot 1").unwrap();
    assert_eq!(shown(&h, &vcx)[0], "group:Core");
    assert_eq!(h.grouping_chip(&vcx), "pinned region");
    // The slot's content changes: the pin re-reads it.
    h.set_slot(&mut vcx, 1, &["sector"]);
    assert_eq!(shown(&h, &vcx)[0], "group:Index");
}

#[gpui::test]
fn a_chain_naming_lhu_skips_it_and_the_header_says_so(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.set_dims(&mut vcx, europe_dims());
    h.set_frame_chain(&mut vcx, &["region", "lhu"]);
    assert_eq!(h.grouping_chip(&vcx), "region \u{b7} lhu skipped");
    assert_eq!(shown(&h, &vcx)[0], "group:Core");
    h.set_frame_chain(&mut vcx, &["lhu", "desk"]);
    assert_eq!(
        h.grouping_chip(&vcx),
        "ungrouped \u{b7} lhu, desk not applicable"
    );
    assert_eq!(shown(&h, &vcx), ["DAX", "SX5E", "NEW"]);
}

#[gpui::test]
fn a_dims_reload_regroups_and_a_dropped_dimension_is_skipped(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = grouped(cx);
    h.set_dims(
        &mut vcx,
        geode_core::dimensions::DerivedDimensions::default(),
    );
    assert_eq!(shown(&h, &vcx), ["DAX", "SX5E", "NEW"]);
    assert_eq!(
        h.grouping_chip(&vcx),
        "ungrouped \u{b7} region not applicable"
    );
    h.set_dims(&mut vcx, europe_dims());
    assert_eq!(shown(&h, &vcx)[0], "group:Core");
    assert!(h.requests().is_empty());
}

fn record(pin: Pin, collapsed: &[&[&str]]) -> toml::Table {
    let mut t = session::to_table(&State {
        list: Some(LIST.into()),
        pin,
        collapsed: collapsed
            .iter()
            .map(|p| p.iter().map(|s| s.to_string()).collect())
            .collect::<BTreeSet<Vec<String>>>(),
        ..State::default()
    });
    t.insert("view".into(), toml::Value::String("vol".into()));
    t
}

#[gpui::test]
fn the_session_restores_the_pin_and_the_collapsed_set(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed_with_record(
        cx,
        record(Pin::Grouping(vec!["region".into()]), &[&["Core"]]),
    );
    h.set_dims(&mut vcx, europe_dims());
    assert_eq!(
        shown(&h, &vcx),
        ["group:Core", "group:\u{2014}", "NEW"],
        "Core restored collapsed"
    );
    assert_eq!(h.grouping_chip(&vcx), "pinned region");
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    assert_eq!(
        saved["pinned"].as_array().unwrap(),
        &[toml::Value::String("region".into())]
    );
    assert_eq!(
        saved["collapsed"].as_array().unwrap(),
        &[toml::Value::Array(vec![toml::Value::String("Core".into())])]
    );
}

#[gpui::test]
fn a_collapsed_path_the_classifications_no_longer_hold_is_kept_and_harmless(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = landed_with_record(
        cx,
        record(Pin::Grouping(vec!["region".into()]), &[&["Core"]]),
    );
    // `Core` was relabelled `Eurozone`: the new group opens, the dead path
    // stays saved and hurts nothing.
    h.set_dims(
        &mut vcx,
        dims_from(
            "[region]\nfrom = \"underlying_ref\"\n[region.values]\nEurozone = [\"DAX\", \"SX5E\"]\n",
        ),
    );
    assert_eq!(
        shown(&h, &vcx),
        ["group:Eurozone", "DAX", "SX5E", "group:\u{2014}", "NEW"]
    );
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    assert_eq!(saved["collapsed"].as_array().unwrap().len(), 1);
}

#[gpui::test]
fn colon_group_underlying_ref_alone_pins_a_flat_list(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = grouped(cx);
    h.command(&mut vcx, "group underlying_ref").unwrap();
    assert_eq!(shown(&h, &vcx), ["DAX", "SX5E", "NEW"]);
    assert_eq!(h.grouping_chip(&vcx), "pinned ungrouped");
}

#[gpui::test]
fn colon_group_with_nothing_applicable_refuses_and_keeps_the_grouping(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = grouped(cx);
    let err = h.command(&mut vcx, "group lhu desk").unwrap_err();
    assert!(err.starts_with("nothing in lhu desk applies"), "{err}");
    assert_eq!(shown(&h, &vcx)[0], "group:Core");
    assert_eq!(h.grouping_chip(&vcx), "region");
}

#[gpui::test]
fn completions_offer_the_applicable_dimensions_none_and_slot(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = landed(cx);
    h.set_dims(&mut vcx, europe_dims());
    assert_eq!(
        h.completions(&vcx, "group ", 6),
        ["underlying_ref", "region", "sector", "none", "slot"]
    );
    assert_eq!(
        h.completions(&vcx, "", 0),
        ["group", "sort", "unpin", "view"]
    );
}

fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
    p.iter()
        .map(|(c, v)| (c.to_string(), v.to_string()))
        .collect()
}

#[gpui::test]
fn space_toggles_the_cursors_group_and_the_z_chords_fold_as_the_blotters(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = grouped(cx);
    assert_eq!(
        h.cursor(&vcx),
        Some("group:Core".into()),
        "the cursor rests on the first row"
    );
    h.press(&mut vcx, "space");
    assert_eq!(shown(&h, &vcx), ["group:Core", "group:\u{2014}", "NEW"]);
    h.press(&mut vcx, "space");
    assert_eq!(shown(&h, &vcx).len(), 5);
    h.press(&mut vcx, "j");
    assert_eq!(h.cursor(&vcx), Some("DAX".into()));
    h.press(&mut vcx, "space");
    assert_eq!(shown(&h, &vcx).len(), 5, "space on a member folds nothing");
    h.press(&mut vcx, "z c");
    assert_eq!(shown(&h, &vcx), ["group:Core", "group:\u{2014}", "NEW"]);
    assert_eq!(
        h.cursor(&vcx),
        Some("group:Core".into()),
        "z c on a member lands on its group"
    );
    h.press(&mut vcx, "z o");
    assert_eq!(shown(&h, &vcx).len(), 5);
    h.press(&mut vcx, "z shift-m");
    assert_eq!(shown(&h, &vcx), ["group:Core", "group:\u{2014}"]);
    h.press(&mut vcx, "z shift-r");
    assert_eq!(shown(&h, &vcx).len(), 5);
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    assert!(
        !saved.contains_key("collapsed"),
        "every fold open saves no key"
    );
    h.press(&mut vcx, "space");
    let saved = vcx.update(|_, cx| h.content.serialize(cx));
    assert_eq!(saved["collapsed"].as_array().unwrap().len(), 1);
}

#[gpui::test]
fn enter_and_t_on_a_group_row_refuse_and_y_yanks_its_members(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = grouped(cx);
    h.press(&mut vcx, "enter");
    assert!(!h.band_open(&vcx));
    assert!(
        h.notices(&vcx)
            .contains(&"inspect needs an underlying".to_string())
    );
    h.press(&mut vcx, "t");
    assert!(
        h.notices(&vcx)
            .contains(&"timeseries needs an underlying".to_string())
    );
    h.press(&mut vcx, "y");
    let text = vcx.read_from_clipboard().and_then(|c| c.text());
    assert_eq!(text.as_deref(), Some("DAX\nSX5E"));
}

#[gpui::test]
fn the_cursor_emits_the_group_path_on_a_group_and_the_member_under_it_on_a_leaf(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = grouped(cx);
    let changed = Rc::new(Cell::new(0));
    let _sub = vcx.update(|_, cx| {
        h.content.watch_emission(
            Rc::new({
                let c = changed.clone();
                move |_| c.set(c.get() + 1)
            }),
            cx,
        )
    });
    let e = vcx.update(|_, cx| h.content.emission(cx));
    assert_eq!(
        e.cursor,
        CursorScope::Path(path_scope(&pairs(&[("region", "Core")])))
    );
    h.press(&mut vcx, "j");
    let e = vcx.update(|_, cx| h.content.emission(cx));
    assert_eq!(
        e.cursor,
        CursorScope::Path(path_scope(&pairs(&[
            ("region", "Core"),
            ("underlying_ref", "DAX")
        ])))
    );
    assert!(changed.get() >= 1);
    h.press(&mut vcx, "shift-g");
    assert_eq!(h.cursor(&vcx), Some("NEW".into()));
    let e = vcx.update(|_, cx| h.content.emission(cx));
    assert_eq!(
        e.cursor,
        CursorScope::NullIn("region".into()),
        "a path through the unmapped group"
    );
    h.press(&mut vcx, "k");
    let e = vcx.update(|_, cx| h.content.emission(cx));
    assert_eq!(e.cursor, CursorScope::NullIn("region".into()));
    h.command(&mut vcx, "group none").unwrap();
    h.press(&mut vcx, "g g");
    let e = vcx.update(|_, cx| h.content.emission(cx));
    assert_eq!(
        e.cursor,
        CursorScope::Path(path_scope(&pairs(&[("underlying_ref", "DAX")]))),
        "flat: the one-value path as before"
    );
}

#[gpui::test]
fn sort_under_grouping_orders_sibling_groups_by_their_mean_and_members_within(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = landed_board(cx, |board| {
        let sx5e = board
            .rows
            .iter_mut()
            .find(|r| r.underlying == "SX5E")
            .unwrap();
        for c in &mut sx5e.cells {
            c.current = Some(20.0);
        }
    });
    h.set_dims(&mut vcx, europe_dims());
    h.set_frame_chain(&mut vcx, &["region"]);
    h.command(&mut vcx, "sort atfm-1W-level desc").unwrap();
    assert_eq!(
        shown(&h, &vcx),
        ["group:Core", "SX5E", "DAX", "group:\u{2014}", "NEW"],
        "members by value within Core; the blank group last"
    );
    h.command(&mut vcx, "sort atfm-1W-level asc").unwrap();
    assert_eq!(
        shown(&h, &vcx),
        ["group:Core", "DAX", "SX5E", "group:\u{2014}", "NEW"],
        "blank still last ascending"
    );
}

#[gpui::test]
fn slash_keeps_a_group_while_a_member_under_it_matches(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = grouped(cx);
    h.find(&mut vcx, FindEvent::Changed("SX".into()));
    assert_eq!(shown(&h, &vcx), ["group:Core", "SX5E"]);
    h.find(&mut vcx, FindEvent::Changed("Core".into()));
    assert!(shown(&h, &vcx).is_empty(), "a group label never matches");
    h.find(&mut vcx, FindEvent::Cancelled);
    assert_eq!(shown(&h, &vcx).len(), 5);
}

#[gpui::test]
fn a_group_row_paints_a_chevron_the_member_none_and_a_press_on_it_toggles(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = grouped(cx);
    h.draw(&mut vcx);
    let chevron = vcx
        .debug_bounds("volwatch-fold-group:Core")
        .expect("a group row paints its chevron");
    assert!(vcx.debug_bounds("volwatch-fold-DAX").is_none());
    let group_text = vcx
        .debug_bounds("7-group:Core-underlying-text")
        .expect("the group's label");
    let member_text = vcx
        .debug_bounds("7-DAX-underlying-text")
        .expect("the member's label");
    assert!(
        member_text.origin.x > chevron.origin.x,
        "a member sits right of its group's chevron"
    );
    assert!(
        member_text.origin.x >= group_text.origin.x,
        "and no further left than its label"
    );
    // A press on the chevron folds; the cursor stays where it was, and
    // the press still reaches the shell (its click-to-focus listens on the
    // tile in the bubble phase).
    h.press(&mut vcx, "j");
    let blank = vcx
        .debug_bounds("volwatch-fold-group:\u{2014}")
        .expect("the unmapped group's chevron");
    let before = h.presses();
    let click = |vcx: &mut gpui::VisualTestContext, at: gpui::Point<gpui::Pixels>| {
        vcx.simulate_mouse_down(at, gpui::MouseButton::Left, gpui::Modifiers::none());
        vcx.simulate_mouse_up(at, gpui::MouseButton::Left, gpui::Modifiers::none());
    };
    click(&mut vcx, blank.center());
    h.draw(&mut vcx);
    assert_eq!(
        shown(&h, &vcx),
        ["group:Core", "DAX", "SX5E", "group:\u{2014}"]
    );
    assert_eq!(
        h.cursor(&vcx),
        Some("DAX".into()),
        "the chevron never moves the cursor"
    );
    assert_eq!(h.presses(), before + 1, "the press bubbles on to the shell");
    click(&mut vcx, chevron.center());
    h.draw(&mut vcx);
    assert_eq!(shown(&h, &vcx), ["group:Core", "group:\u{2014}"]);
    assert_eq!(
        h.cursor(&vcx),
        Some("group:Core".into()),
        "a fold hiding the cursor rests it on the group"
    );
}

#[gpui::test]
fn the_header_paints_the_grouping_chip_and_the_menu_offers_the_folds(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = grouped(cx);
    h.draw(&mut vcx);
    assert!(vcx.debug_bounds("volwatch-grouping-7").is_some());
    h.press(&mut vcx, ".");
    let titles = h.menu_titles(&vcx);
    for t in ["Toggle group", "Expand all groups", "Collapse all groups"] {
        assert!(titles.iter().any(|x| x == t), "{t} missing from {titles:?}");
    }
    let row = |vcx: &gpui::VisualTestContext, title: &str| {
        h.tile.read_with(vcx, |t, _| {
            t.menu
                .as_ref()
                .unwrap()
                .menu
                .rows()
                .iter()
                .filter_map(geode_tile::menu::Row::action)
                .find(|a| a.title().as_ref() == title)
                .unwrap_or_else(|| panic!("no row {title}"))
                .clone()
        })
    };
    assert!(
        row(&vcx, "Toggle group").is_enabled(),
        "the cursor is on a group"
    );
    assert!(row(&vcx, "Expand all groups").is_enabled());
    h.press(&mut vcx, "escape");
    // On a member the toggle row says why it is off; the whole-tree folds stay.
    h.press(&mut vcx, "j");
    h.press(&mut vcx, ".");
    let toggle = row(&vcx, "Toggle group");
    assert!(!toggle.is_enabled());
    assert_eq!(
        toggle.reason().unwrap().as_ref(),
        "the cursor is not on a group"
    );
    assert!(row(&vcx, "Collapse all groups").is_enabled());
    h.press(&mut vcx, "escape");
    // Flat, every fold row says the tile is not grouped.
    h.command(&mut vcx, "group none").unwrap();
    h.press(&mut vcx, ".");
    assert_eq!(
        row(&vcx, "Expand all groups").reason().unwrap().as_ref(),
        "the tile is not grouped"
    );
    h.press(&mut vcx, "escape");
}
