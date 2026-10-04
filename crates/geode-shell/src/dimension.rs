//! Dimension actions: what a row's single-valued columns let the user do.
//! A crate implements [`DimensionAction`] and `geode-app` registers it on
//! the roster; tile kinds join the same menu through
//! `ModuleFactory::accepts`. [`menu_rows`] turns a row's
//! [`DimensionContext`] into the shell's row menu: one section per column
//! that has rows, the clicked column first.

use std::collections::BTreeSet;

use geode_core::context::DimensionContext;
use gpui::SharedString;

use crate::defaults::capitalize;
use crate::menu::{ActionRow, MenuPick, Row};
use crate::module::ModuleRoster;
use crate::shell::row_menu::ActionCx;

/// One thing a row's value lets the user do (open it elsewhere, act on
/// it). Registered on the roster by `geode-app`; runs in the shell.
pub trait DimensionAction {
    /// Stable identity, `crate::verb` (`"nemo::open_position"`).
    fn id(&self) -> &'static str;
    /// The menu row's words (`"Open in Nemo"`).
    fn title(&self) -> SharedString;
    /// The column whose section the row sits in (`"position_ref"`); also a
    /// context column every row carries (`ModuleRoster::context_columns`).
    fn column(&self) -> &'static str;
    /// Enabled, or disabled with the reason the row shows.
    fn available(&self, _ctx: &DimensionContext) -> Result<(), SharedString> {
        Ok(())
    }
    /// Do it. Called once the menu has closed.
    fn run(&self, ctx: &DimensionContext, acx: &mut ActionCx<'_, '_>);
    /// A value picked from `ActionCx::choose_value`, with the context the
    /// action ran on. Called once the choice dialog has closed. Does
    /// nothing unless the action chooses.
    fn chosen(&self, _ctx: &DimensionContext, _value: &str, _acx: &mut ActionCx<'_, '_>) {}
}

/// Where `ActionCx::open_url` sends a URL when set. Unset in production,
/// so the OS opens it (`App::open_url`); a test sets one to record the
/// URLs an action opened.
pub struct UrlOpener(pub std::rc::Rc<OpenUrl>);

/// What a [`UrlOpener`] calls with each URL.
pub type OpenUrl = dyn Fn(&str, &mut gpui::App);

impl gpui::Global for UrlOpener {}

/// What a row menu row does when picked.
#[derive(Clone, Debug, PartialEq)]
pub enum RowPick {
    /// Split a new tile of `kind` with its `launch_state` of the context.
    Open { kind: &'static str },
    /// Run the roster's action at `index`.
    Action { index: usize },
    /// Open the color pick list for `value` of `column`.
    Color { column: String, value: String },
}

/// The row menu's color row.
pub const COLOR_ROW: &str = "Color\u{2026}";

impl MenuPick for RowPick {
    fn element_name(&self) -> SharedString {
        match self {
            RowPick::Open { kind } => format!("row-menu-open-{kind}").into(),
            RowPick::Action { index } => format!("row-menu-action-{index}").into(),
            RowPick::Color { .. } => "row-menu-color".into(),
        }
    }
}

/// The row menu for `ctx`: for each column in menu order (`ctx.first`,
/// then the rest in context order), a section headed `{column} · {value}`
/// holding "Open {Kind}" for every kind whose first accepted column (in
/// menu order) this is, then every action on this column, then the one
/// `Color…` row when this column is the color target (see
/// `color_target`; `text_dims` names the text dimensions). Sections with
/// no rows are left out; sections are parted by separators. Empty when
/// nothing applies.
pub fn menu_rows(
    ctx: &DimensionContext,
    roster: &ModuleRoster,
    text_dims: &BTreeSet<String>,
) -> Vec<Row<RowPick>> {
    let mut order: Vec<&(String, String)> = Vec::new();
    if let Some(first) = &ctx.first {
        order.extend(ctx.values.iter().filter(|(c, _)| c == first));
    }
    order.extend(
        ctx.values
            .iter()
            .filter(|(c, _)| Some(c) != ctx.first.as_ref()),
    );

    let color = color_target(ctx, text_dims);
    let mut rows: Vec<Row<RowPick>> = Vec::new();
    let mut placed: Vec<&'static str> = Vec::new();
    for (column, value) in order {
        let mut section: Vec<Row<RowPick>> = Vec::new();
        for kind in roster.kinds() {
            if placed.contains(&kind) {
                continue;
            }
            let Some(f) = roster.factory(kind) else {
                continue;
            };
            if f.accepts().contains(&column.as_str()) {
                placed.push(kind);
                section.push(Row::Action(ActionRow::new(
                    RowPick::Open { kind },
                    format!("Open {}", capitalize(kind)),
                )));
            }
        }
        for (index, action) in roster.actions().iter().enumerate() {
            if action.column() == column {
                section.push(Row::Action(
                    ActionRow::new(RowPick::Action { index }, action.title())
                        .enabled(action.available(ctx)),
                ));
            }
        }
        if color.is_some_and(|(c, _)| c == column) {
            section.push(Row::Action(ActionRow::new(
                RowPick::Color {
                    column: column.clone(),
                    value: value.clone(),
                },
                COLOR_ROW,
            )));
        }
        if section.is_empty() {
            continue;
        }
        if !rows.is_empty() {
            rows.push(Row::Separator);
        }
        rows.push(Row::Section(format!("{column} \u{b7} {value}").into()));
        rows.extend(section);
    }
    rows
}

/// The `(column, value)` the menu's one `Color…` row is for: the clicked
/// column when it is a text dimension with a value at the row, else the
/// row's own column under the same test.
fn color_target<'a>(
    ctx: &'a DimensionContext,
    text_dims: &BTreeSet<String>,
) -> Option<&'a (String, String)> {
    [ctx.first.as_ref(), ctx.own.as_ref()]
        .into_iter()
        .flatten()
        .filter(|column| text_dims.contains(*column))
        .find_map(|column| ctx.values.iter().find(|(c, _)| c == column))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::ModuleRoster;
    use crate::module::recording::{RecordingAction, RecordingFactory};
    use geode_core::context::DimensionContext;
    use std::rc::Rc;

    fn roster() -> ModuleRoster {
        let mut r = ModuleRoster::new();
        let mut cvi = RecordingFactory::new("cvi");
        cvi.accepts = &["underlying_ref"];
        let mut dividend = RecordingFactory::new("dividend");
        dividend.accepts = &["underlying_ref"];
        r.add(Box::new(cvi));
        r.add(Box::new(dividend));
        r.add(Box::new(RecordingFactory::new("plain")));
        r.add_action(Rc::new(RecordingAction::new(
            "nemo::position",
            "Open in Nemo",
            "position_ref",
        )));
        let mut lhu =
            RecordingAction::new("positions::move_lhu", "Move LHU\u{2026}", "position_ref");
        lhu.available = Err("no position service configured".into());
        r.add_action(Rc::new(lhu));
        r
    }

    /// (section titles, action titles) in order, a separator as "|".
    fn shape(rows: &[Row<RowPick>]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                Row::Section(t) => format!("# {t}"),
                Row::Separator => "|".into(),
                Row::Action(a) => a.title().to_string(),
            })
            .collect()
    }

    #[test]
    fn a_section_per_column_with_rows_in_context_order() {
        let ctx = DimensionContext::of(&[
            ("lhu", "7"),
            ("underlying_ref", "SPX"),
            ("position_ref", "P7"),
        ]);
        assert_eq!(
            shape(&menu_rows(&ctx, &roster(), &BTreeSet::new())),
            vec![
                "# underlying_ref \u{b7} SPX",
                "Open Cvi",
                "Open Dividend",
                "|",
                "# position_ref \u{b7} P7",
                "Open in Nemo",
                "Move LHU\u{2026}",
            ],
            "lhu has no rows, so no section"
        );
    }

    #[test]
    fn the_clicked_column_leads() {
        let mut ctx = DimensionContext::of(&[("underlying_ref", "SPX"), ("position_ref", "P7")]);
        ctx.first = Some("position_ref".into());
        let s = shape(&menu_rows(&ctx, &roster(), &BTreeSet::new()));
        assert_eq!(s[0], "# position_ref \u{b7} P7");
    }

    #[test]
    fn a_disabled_action_keeps_its_reason() {
        let ctx = DimensionContext::of(&[("position_ref", "P7")]);
        let rows = menu_rows(&ctx, &roster(), &BTreeSet::new());
        let lhu = rows
            .iter()
            .find_map(|r| match r {
                Row::Action(a) if a.title().as_ref() == "Move LHU\u{2026}" => Some(a.clone()),
                _ => None,
            })
            .unwrap();
        assert!(!lhu.is_enabled());
        assert_eq!(
            lhu.reason().map(|r| r.as_ref()),
            Some("no position service configured")
        );
    }

    #[test]
    fn a_kind_accepting_two_present_columns_sits_in_the_first_only() {
        let mut r = roster();
        let mut both = RecordingFactory::new("both");
        both.accepts = &["position_ref", "underlying_ref"];
        r.add(Box::new(both));
        let ctx = DimensionContext::of(&[("underlying_ref", "SPX"), ("position_ref", "P7")]);
        let s = shape(&menu_rows(&ctx, &r, &BTreeSet::new()));
        assert_eq!(s.iter().filter(|t| *t == "Open Both").count(), 1);
        let under = s
            .iter()
            .position(|t| t == "# underlying_ref \u{b7} SPX")
            .unwrap();
        let pos = s
            .iter()
            .position(|t| t == "# position_ref \u{b7} P7")
            .unwrap();
        let both_at = s.iter().position(|t| t == "Open Both").unwrap();
        assert!(under < both_at && both_at < pos, "{s:?}");
    }

    #[test]
    fn an_empty_or_actionless_context_has_no_rows() {
        assert!(menu_rows(&DimensionContext::default(), &roster(), &BTreeSet::new()).is_empty());
        assert!(
            menu_rows(
                &DimensionContext::of(&[("lhu", "7")]),
                &roster(),
                &BTreeSet::new()
            )
            .is_empty()
        );
    }

    fn text(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn color_is_offered_once_for_the_rows_own_text_dimension() {
        let mut ctx = DimensionContext::of(&[("book", "BK1"), ("underlying_ref", "SPX")]);
        ctx.own = Some("underlying_ref".into());
        let rows = menu_rows(
            &ctx,
            &ModuleRoster::default(),
            &text(&["book", "underlying_ref"]),
        );
        assert_eq!(
            shape(&rows),
            ["# underlying_ref \u{b7} SPX", "Color\u{2026}"],
            "one row, in the own value's section; `book` gets none"
        );
        let picked = rows.iter().find_map(|r| match r {
            Row::Action(a) => Some(a.pick().clone()),
            _ => None,
        });
        assert_eq!(
            picked,
            Some(RowPick::Color {
                column: "underlying_ref".into(),
                value: "SPX".into()
            })
        );
    }

    #[test]
    fn a_clicked_text_dimension_takes_color_from_the_rows_own() {
        let mut ctx = DimensionContext::of(&[("book", "BK1"), ("underlying_ref", "SPX")]);
        ctx.own = Some("underlying_ref".into());
        ctx.first = Some("book".into());
        let rows = menu_rows(
            &ctx,
            &ModuleRoster::default(),
            &text(&["book", "underlying_ref"]),
        );
        assert_eq!(shape(&rows), ["# book \u{b7} BK1", "Color\u{2026}"]);
        // A clicked column that is not a text dimension falls back to own.
        ctx.first = Some("strike".into());
        let rows = menu_rows(
            &ctx,
            &ModuleRoster::default(),
            &text(&["book", "underlying_ref"]),
        );
        assert_eq!(
            shape(&rows),
            ["# underlying_ref \u{b7} SPX", "Color\u{2026}"]
        );
    }

    #[test]
    fn no_color_row_without_a_text_dimension_value_at_the_row() {
        let mut ctx = DimensionContext::of(&[("underlying_ref", "SPX")]);
        // Not a text dimension.
        ctx.own = Some("underlying_ref".into());
        assert!(menu_rows(&ctx, &ModuleRoster::default(), &text(&[])).is_empty());
        // A text dimension the row holds no single value of.
        ctx.own = Some("book".into());
        assert!(menu_rows(&ctx, &ModuleRoster::default(), &text(&["book"])).is_empty());
        // No own, nothing clicked.
        ctx.own = None;
        assert!(menu_rows(&ctx, &ModuleRoster::default(), &text(&["underlying_ref"])).is_empty());
    }

    #[test]
    fn color_sits_last_in_a_section_other_rows_already_fill() {
        let mut ctx = DimensionContext::of(&[("underlying_ref", "SPX"), ("position_ref", "P7")]);
        ctx.own = Some("underlying_ref".into());
        let rows = menu_rows(&ctx, &roster(), &text(&["underlying_ref"]));
        let shape = shape(&rows);
        let section = shape
            .iter()
            .position(|r| r.starts_with("# underlying_ref"))
            .unwrap();
        let end = shape[section..]
            .iter()
            .position(|r| r == "|")
            .map_or(shape.len(), |i| section + i);
        assert_eq!(shape[end - 1], "Color\u{2026}", "{shape:?}");
        assert!(
            end - section > 2,
            "the section's own rows are still there: {shape:?}"
        );
    }

    #[test]
    fn picks_name_the_kind_or_the_action_index() {
        let ctx = DimensionContext::of(&[("underlying_ref", "SPX"), ("position_ref", "P7")]);
        let picks: Vec<RowPick> = menu_rows(&ctx, &roster(), &BTreeSet::new())
            .iter()
            .filter_map(|r| match r {
                Row::Action(a) => Some(a.pick().clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            picks,
            vec![
                RowPick::Open { kind: "cvi" },
                RowPick::Open { kind: "dividend" },
                RowPick::Action { index: 0 },
                RowPick::Action { index: 1 },
            ]
        );
    }
}
