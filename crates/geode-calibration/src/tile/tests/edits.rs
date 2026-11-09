//! The graph edits: `c` connects through a picker that previews the edge,
//! `x` disconnects, `a` adds a stage through a kind picker and a name
//! field, `d` deletes through the y/n bar. Every key goes through the
//! app's keymap; every edit is read off the frame's queue, as the shell
//! drains it; the shell's answer comes back through the frame's notices.

use geode_core::calibration::{BUILTIN_CALIBRATION, CALIBRATION_DOC};
use geode_shell::actions::ActionId;
use geode_shell::frame::{ConfigEdit, TileNotice};
use geode_shell::keymap::KeyContext;
use geode_shell::tiling::TileId;
use toml::Value;

use super::harness::{Harness, builtin_config, config_from, open_with, restored};
use crate::content::CalibrationConfig;
use crate::core::menu::OPEN_STAGE;
use crate::core::model::EdgeTone;

fn open_spx(
    cx: &mut gpui::TestAppContext,
    config: CalibrationConfig,
    cursor: Option<&str>,
) -> (Harness, gpui::VisualTestContext) {
    open_with(cx, Some(config), Some(restored(Some("SPX"), None, cursor)))
}

fn normal() -> KeyContext {
    KeyContext::new(crate::KIND).pair("mode", "normal")
}

fn insert() -> KeyContext {
    KeyContext::new(crate::KIND).pair("mode", "insert")
}

fn list() -> KeyContext {
    KeyContext::new(crate::KIND)
        .pair("mode", "menu")
        .pair("popup", "list")
        .tilelist()
}

/// The builtin document with the publish stage reading one raster and no
/// gate: valid (a publish takes at least one raster), with `box_curve`
/// and `df_monotone` left to connect.
fn open_publish_config() -> CalibrationConfig {
    let text = BUILTIN_CALIBRATION.replace(
        r#"inputs   = ["repo_curve", "box_curve", "df_monotone"]"#,
        r#"inputs   = ["repo_curve"]"#,
    );
    assert_ne!(text, BUILTIN_CALIBRATION, "the fixture edits the publish");
    let config = config_from(&text);
    assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
    config
}

fn inputs_of(edit: &ConfigEdit) -> Vec<String> {
    edit.value
        .as_ref()
        .and_then(Value::as_table)
        .and_then(|t| t.get("inputs"))
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

impl Harness {
    /// The open picker's painted rows (name and detail) and its highlight.
    fn picker_rows(&self, vcx: &gpui::VisualTestContext) -> Option<(Vec<(String, String)>, usize)> {
        self.tile.read_with(vcx, |t, _| t.picker_rows())
    }

    /// The open picker's title line.
    fn picker_title(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile.read_with(vcx, |t, _| t.picker_title())
    }

    /// The add picker's name field text, while it is up.
    fn picker_name(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
        self.tile.read_with(vcx, |t, cx| t.picker_name(cx))
    }

    /// The edge the model paints as the preview, if any.
    fn preview_edge(&self, vcx: &gpui::VisualTestContext) -> Option<(String, String)> {
        self.tile.read_with(vcx, |t, _| {
            t.model()?
                .edges
                .iter()
                .find(|e| e.tone == EdgeTone::Preview)
                .map(|e| (e.from.clone(), e.to.clone()))
        })
    }
}

/// `c` on the publish stage (one raster in, no gate) opens the picker
/// under the card over the two candidates in document order, the
/// highlighted one previewed as a dashed edge into the stage; `j` moves
/// the preview with the highlight; `enter` queues the stage's whole table
/// with the pick appended to `inputs`, closes the picker and drops the
/// preview. `escape` closes with nothing queued.
#[gpui::test]
fn c_opens_the_picker_previews_the_highlighted_edge_and_enter_queues_the_edit(
    cx: &mut gpui::TestAppContext,
) {
    let (h, mut vcx) = open_spx(cx, open_publish_config(), Some("publish"));
    assert_eq!(h.preview_edge(&vcx), None);
    h.press(&mut vcx, "c");
    assert_eq!(h.dispatched(), ["calibration::connect"]);
    assert_eq!(h.key_context(&vcx), list(), "a fieldless list");
    assert_eq!(
        h.picker_title(&vcx).as_deref(),
        Some("Connect publish from")
    );
    assert_eq!(
        h.picker_rows(&vcx),
        Some((
            vec![
                ("df_monotone".to_string(), "check".to_string()),
                ("box_curve".to_string(), "rasterize".to_string()),
            ],
            0
        ))
    );
    assert_eq!(
        h.preview_edge(&vcx),
        Some(("df_monotone".to_string(), "publish".to_string()))
    );
    h.draw(&mut vcx);
    assert!(
        vcx.debug_bounds("calibration-picker-7").is_some(),
        "the picker is painted"
    );
    let card = vcx.debug_bounds("calibration-card-7-publish").unwrap();
    let picker = vcx.debug_bounds("calibration-picker-7").unwrap();
    assert!(
        picker.top() >= card.bottom() - gpui::px(0.5),
        "under the cursor's card: {picker:?} below {card:?}"
    );

    h.press(&mut vcx, "j");
    assert_eq!(
        h.dispatched().last().map(String::as_str),
        Some(geode_tile::motion::MENU_DOWN),
        "the shared list step"
    );
    assert_eq!(h.picker_rows(&vcx).map(|(_, hl)| hl), Some(1));
    assert_eq!(
        h.preview_edge(&vcx),
        Some(("box_curve".to_string(), "publish".to_string()))
    );
    assert!(h.config_edits(&mut vcx).is_empty(), "nothing queued yet");

    h.press(&mut vcx, "enter");
    assert_eq!(
        h.dispatched().last().map(String::as_str),
        Some("calibration::commit")
    );
    assert_eq!(h.picker_rows(&vcx), None, "closed by the pick");
    assert_eq!(h.key_context(&vcx), normal());
    assert_eq!(h.preview_edge(&vcx), None, "the preview leaves with it");
    let edits = h.config_edits(&mut vcx);
    assert_eq!(edits.len(), 1, "{edits:?}");
    assert_eq!(edits[0].doc, CALIBRATION_DOC);
    assert_eq!(edits[0].object, "publish");
    assert_eq!(edits[0].origin, Some(TileId(Harness::TILE)));
    assert_eq!(inputs_of(&edits[0]), ["repo_curve", "box_curve"]);
    let table = edits[0].value.as_ref().unwrap().as_table().unwrap();
    assert_eq!(table["kind"].as_str(), Some("publish"));
    assert_eq!(table["document"].as_str(), Some("rate_curve"));
    assert_eq!(table["target"].as_str(), Some("sophis"));
    assert_eq!(h.cursor(&vcx).as_deref(), Some("publish"), "unmoved");

    // `escape` discards.
    h.press(&mut vcx, "c");
    assert!(h.picker_rows(&vcx).is_some());
    h.press(&mut vcx, "escape");
    assert_eq!(
        h.dispatched().last().map(String::as_str),
        Some("calibration::cancel")
    );
    assert_eq!(h.picker_rows(&vcx), None);
    assert_eq!(h.preview_edge(&vcx), None);
    assert!(h.config_edits(&mut vcx).is_empty());
    assert_eq!(h.key_context(&vcx), normal());
}

/// `repo_fit`'s one port is filled: `c` opens nothing and says so in the
/// header; with no cursor, the refusal names the missing cursor.
#[gpui::test]
fn c_on_a_stage_with_no_open_port_says_so(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx, builtin_config(), Some("repo_fit"));
    h.press(&mut vcx, "c");
    assert_eq!(h.dispatched(), ["calibration::connect"]);
    assert_eq!(h.picker_rows(&vcx), None);
    assert_eq!(h.key_context(&vcx), normal());
    assert_eq!(
        h.notices(&vcx),
        vec!["repo_fit has no open input".to_string()]
    );
    assert!(h.config_edits(&mut vcx).is_empty());

    let (h, mut vcx) = open_spx(cx, builtin_config(), None);
    h.press(&mut vcx, "c");
    assert_eq!(
        h.notices(&vcx),
        vec![crate::core::menu::NO_CURSOR.to_string()]
    );
}

/// `x` on `implied_rates` lists its three inputs in port order; `j` and
/// `enter` queue the stage's table without the chosen one. A source has
/// nothing to disconnect and the refusal says so.
#[gpui::test]
fn x_removes_the_chosen_input(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx, builtin_config(), Some("implied_rates"));
    h.press(&mut vcx, "x");
    assert_eq!(h.dispatched(), ["calibration::disconnect"]);
    assert_eq!(h.key_context(&vcx), list());
    assert_eq!(
        h.picker_title(&vcx).as_deref(),
        Some("Disconnect from implied_rates")
    );
    assert_eq!(
        h.picker_rows(&vcx),
        Some((
            vec![
                ("parity".to_string(), "parity_fit".to_string()),
                ("dividends".to_string(), "source".to_string()),
                ("rates".to_string(), "source".to_string()),
            ],
            0
        ))
    );
    assert_eq!(h.preview_edge(&vcx), None, "a disconnect previews nothing");
    h.press(&mut vcx, "j");
    h.press(&mut vcx, "enter");
    assert_eq!(h.picker_rows(&vcx), None);
    let edits = h.config_edits(&mut vcx);
    assert_eq!(edits.len(), 1, "{edits:?}");
    assert_eq!(edits[0].object, "implied_rates");
    assert_eq!(inputs_of(&edits[0]), ["parity", "rates"]);
    assert_eq!(
        edits[0].value.as_ref().unwrap()["kind"].as_str(),
        Some("implied_rates")
    );

    h.press(&mut vcx, "g 1");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("chain"));
    h.press(&mut vcx, "x");
    assert_eq!(h.picker_rows(&vcx), None);
    assert_eq!(
        h.notices(&vcx),
        vec!["chain has no inputs to disconnect".to_string()]
    );
}

/// `a` lists the seven kinds; a pick opens the name field in `insert`;
/// `enter` on a taken name is refused and keeps the field; a good name
/// queues the new stage's table with the kind's required fields at the
/// builtin's values, moves the cursor to the new name and asks the shell
/// for `config::calibration`, so the Edit screen opens on it.
#[gpui::test]
fn a_picks_a_kind_names_the_stage_and_opens_its_edit_screen(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx, builtin_config(), Some("parity"));
    h.press(&mut vcx, "a");
    assert_eq!(h.dispatched(), ["calibration::add"]);
    assert_eq!(h.key_context(&vcx), list());
    assert_eq!(h.picker_title(&vcx).as_deref(), Some("Add stage"));
    let (rows, highlighted) = h.picker_rows(&vcx).expect("the kind list");
    assert_eq!(
        rows.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        [
            "source",
            "parity_fit",
            "implied_rates",
            "check",
            "kalman_fit",
            "rasterize",
            "publish"
        ]
    );
    assert_eq!(highlighted, 0);
    assert!(rows.iter().all(|(_, line)| !line.is_empty()), "{rows:?}");
    h.press(&mut vcx, "j j j j");
    assert_eq!(h.picker_rows(&vcx).map(|(_, hl)| hl), Some(4), "kalman_fit");
    h.press(&mut vcx, "enter");
    assert_eq!(
        h.key_context(&vcx),
        insert(),
        "the name field holds the keys"
    );
    assert_eq!(h.picker_title(&vcx).as_deref(), Some("Name the kalman_fit"));
    assert_eq!(h.picker_name(&vcx).as_deref(), Some(""));
    h.draw(&mut vcx);
    assert!(
        vcx.debug_bounds("calibration-picker-name-7").is_some(),
        "the field is painted"
    );

    // A taken name is refused and the field stays.
    vcx.simulate_input("repo_fit");
    vcx.run_until_parked();
    assert_eq!(h.picker_name(&vcx).as_deref(), Some("repo_fit"));
    h.press(&mut vcx, "enter");
    assert_eq!(
        h.dispatched().last().map(String::as_str),
        Some("calibration::commit")
    );
    assert_eq!(h.key_context(&vcx), insert(), "still up");
    assert_eq!(
        h.notices(&vcx),
        vec!["repo_fit is already a stage".to_string()]
    );
    assert!(h.config_edits(&mut vcx).is_empty());
    assert!(h.requested_actions(&mut vcx).is_empty());

    // A fresh name: the edit, the cursor and the dialog request.
    vcx.simulate_keystrokes(
        "backspace backspace backspace backspace backspace backspace backspace backspace",
    );
    vcx.simulate_input("fit2");
    vcx.run_until_parked();
    h.press(&mut vcx, "enter");
    assert_eq!(h.key_context(&vcx), normal());
    assert_eq!(h.picker_rows(&vcx), None);
    assert_eq!(h.picker_name(&vcx), None);
    let edits = h.config_edits(&mut vcx);
    assert_eq!(edits.len(), 1, "{edits:?}");
    assert_eq!(edits[0].doc, CALIBRATION_DOC);
    assert_eq!(edits[0].object, "fit2");
    assert_eq!(edits[0].origin, Some(TileId(Harness::TILE)));
    let table = edits[0].value.as_ref().unwrap().as_table().unwrap();
    assert_eq!(table["pipeline"].as_str(), Some("implied_forwards"));
    assert_eq!(table["kind"].as_str(), Some("kalman_fit"));
    assert_eq!(table["series"].as_str(), Some("repo"));
    assert_eq!(table["measurement_noise"].as_str(), Some("spread"));
    assert!(table.contains_key("knots") && table.contains_key("process_noise"));
    assert!(!table.contains_key("inputs"), "connected afterwards");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("fit2"));
    assert_eq!(
        h.requested_actions(&mut vcx),
        vec![(TileId(Harness::TILE), ActionId(OPEN_STAGE.to_string()))]
    );
    assert!(h.notices(&vcx).is_empty(), "the refusal left with the verb");

    // `escape` in the field cancels the whole add.
    h.press(&mut vcx, "a");
    h.press(&mut vcx, "enter");
    assert_eq!(h.key_context(&vcx), insert());
    vcx.simulate_input("src2");
    vcx.run_until_parked();
    h.press(&mut vcx, "escape");
    assert_eq!(h.key_context(&vcx), normal());
    assert_eq!(h.picker_name(&vcx), None);
    assert!(h.config_edits(&mut vcx).is_empty());
    assert!(h.requested_actions(&mut vcx).is_empty());
}

/// `d` on `implied_rates` asks on the bar, naming its two edges into the
/// fits; `n` leaves everything; `y` queues the stage's removal and both
/// fits' tables without it as one batch, and moves the cursor to its first
/// input. A source nothing reads asks without edges.
#[gpui::test]
fn d_confirms_and_deletes_with_its_edges_in_one_batch(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx, builtin_config(), Some("implied_rates"));
    h.press(&mut vcx, "d");
    assert_eq!(h.dispatched(), ["calibration::delete"]);
    assert_eq!(
        h.confirm_text(&vcx).as_deref(),
        Some("Delete implied_rates and its 2 edges? y/n")
    );
    assert_eq!(h.key_context(&vcx), insert(), "the bar holds the keys");
    h.press(&mut vcx, "n");
    assert_eq!(h.confirm_text(&vcx), None);
    assert!(h.config_edits(&mut vcx).is_empty(), "no answers nothing");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("implied_rates"));

    h.press(&mut vcx, "d");
    h.press(&mut vcx, "y");
    assert_eq!(h.confirm_text(&vcx), None);
    let edits = h.config_edits(&mut vcx);
    let objects: Vec<&str> = edits.iter().map(|e| e.object.as_str()).collect();
    assert_eq!(
        objects,
        ["implied_rates", "repo_fit", "box_fit"],
        "one batch"
    );
    assert_eq!(edits[0].value, None);
    assert!(inputs_of(&edits[1]).is_empty());
    assert!(inputs_of(&edits[2]).is_empty());
    assert_eq!(
        edits[1].value.as_ref().unwrap()["series"].as_str(),
        Some("repo"),
        "the fit keeps its fields"
    );
    assert!(
        edits
            .iter()
            .all(|e| e.origin == Some(TileId(Harness::TILE)))
    );
    assert_eq!(
        h.cursor(&vcx).as_deref(),
        Some("parity"),
        "the deleted stage's first input"
    );

    // Nothing reads the publish: its deletion has no edges and the cursor
    // falls back to its first input.
    h.press(&mut vcx, "g 9");
    h.press(&mut vcx, "l");
    assert_eq!(h.cursor(&vcx).as_deref(), Some("publish"));
    h.press(&mut vcx, "d");
    assert_eq!(h.confirm_text(&vcx).as_deref(), Some("Delete publish? y/n"));
    h.press(&mut vcx, "y");
    let edits = h.config_edits(&mut vcx);
    assert_eq!(edits.len(), 1);
    assert_eq!(
        (edits[0].object.as_str(), &edits[0].value),
        ("publish", &None)
    );
    assert_eq!(h.cursor(&vcx).as_deref(), Some("repo_curve"));

    // Without a cursor there is nothing to delete.
    let (h, mut vcx) = open_spx(cx, builtin_config(), None);
    h.press(&mut vcx, "d");
    assert_eq!(h.confirm_text(&vcx), None);
    assert_eq!(
        h.notices(&vcx),
        vec![crate::core::menu::NO_CURSOR.to_string()]
    );
}

/// The shell's answer to a queued edit reaches the header: a refusal as a
/// danger notice, which `escape` dismisses, and a fork as a status notice,
/// which (as every status notice) stays until the next edit goes out.
#[gpui::test]
fn a_refused_edit_notice_reaches_the_header(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx, builtin_config(), Some("implied_rates"));
    h.press(&mut vcx, "x");
    h.press(&mut vcx, "enter");
    assert_eq!(h.config_edits(&mut vcx).len(), 1);
    assert!(h.notices(&vcx).is_empty());
    h.post_notice(
        &mut vcx,
        TileNotice::Refused("no writable user config directory — nothing was changed".into()),
    );
    assert_eq!(
        h.notices(&vcx),
        vec!["no writable user config directory — nothing was changed".to_string()]
    );
    h.post_notice(
        &mut vcx,
        TileNotice::Forked("copied 'implied_rates' to your config".into()),
    );
    assert_eq!(
        h.notices(&vcx),
        vec![
            "no writable user config directory — nothing was changed".to_string(),
            "copied 'implied_rates' to your config".to_string(),
        ]
    );
    h.press(&mut vcx, "escape");
    assert_eq!(
        h.notices(&vcx),
        vec!["copied 'implied_rates' to your config".to_string()],
        "the refusal is dismissed; a status notice is not dismissable"
    );
    // The next edit out takes the last edit's answers with it.
    h.press(&mut vcx, "x");
    h.press(&mut vcx, "enter");
    assert_eq!(h.config_edits(&mut vcx).len(), 1);
    assert!(h.notices(&vcx).is_empty(), "cleared by the next edit");
}

/// The `⋯` menu's editor rows are live: each carries its key and a pick
/// dispatches the verb; `Add stage…` needs only a pipeline, the others a
/// cursor.
#[gpui::test]
fn the_menu_rows_dispatch_the_editors(cx: &mut gpui::TestAppContext) {
    let (h, mut vcx) = open_spx(cx, builtin_config(), Some("implied_rates"));
    h.press(&mut vcx, ".");
    for id in [
        "calibration::connect",
        "calibration::disconnect",
        "calibration::add",
        "calibration::delete",
    ] {
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.menu_reason(id)),
            None,
            "{id}"
        );
    }
    // Row 7 (rows count the separators) is Disconnect…: the menu closes
    // and the picker opens.
    h.draw(&mut vcx);
    let at = vcx
        .debug_bounds("calibration-menu-row-7-7")
        .expect("the row is painted")
        .center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    h.draw(&mut vcx);
    assert_eq!(h.tile.read_with(&vcx, |t, _| t.menu_open()), None);
    assert_eq!(
        h.picker_title(&vcx).as_deref(),
        Some("Disconnect from implied_rates")
    );
    assert_eq!(h.key_context(&vcx), list());
    // The list owns the keys: `.` is not its, so it reaches no binding and
    // the list stays; a verb reaching the tile anyway (the palette) closes
    // the picker before it runs.
    let before = h.dispatched().len();
    h.press(&mut vcx, ".");
    assert_eq!(h.dispatched().len(), before, ". reached no binding");
    assert!(h.picker_rows(&vcx).is_some());
    assert!(h.act(&mut vcx, "calibration::menu"));
    assert_eq!(h.picker_rows(&vcx), None, "another verb closes the picker");
    assert!(h.tile.read_with(&vcx, |t, _| t.menu_open()).is_some());
    h.press(&mut vcx, "escape");
    // `escape` closes a list too.
    h.press(&mut vcx, "a");
    assert!(h.picker_rows(&vcx).is_some());
    h.press(&mut vcx, "escape");
    assert_eq!(h.picker_rows(&vcx), None);
    assert_eq!(h.key_context(&vcx), normal());

    let (h, mut vcx) = open_spx(cx, builtin_config(), None);
    h.press(&mut vcx, ".");
    for id in [
        "calibration::connect",
        "calibration::disconnect",
        "calibration::delete",
    ] {
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.menu_reason(id)).as_deref(),
            Some(crate::core::menu::NO_CURSOR),
            "{id}"
        );
    }
    assert_eq!(
        h.tile
            .read_with(&vcx, |t, _| t.menu_reason("calibration::add")),
        None
    );
}
