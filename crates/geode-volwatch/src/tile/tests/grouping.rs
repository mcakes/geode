//! Grouping: the frame's chain, the pins, the dimensions reload, the
//! header chip and the session, each through the shell's doors.

use super::harness::*;
use crate::core::grouping::Pin;
use crate::core::session::{self, State};
use std::collections::BTreeSet;

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
