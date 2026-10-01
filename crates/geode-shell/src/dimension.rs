//! Dimension actions: what a row's single-valued columns let the user do.
//! A crate implements [`DimensionAction`] and `geode-app` registers it on
//! the roster; tile kinds join the same menu through
//! `ModuleFactory::accepts`. [`menu_rows`] turns a row's
//! [`DimensionContext`] into the shell's row menu: one section per column
//! that has rows, the clicked column first.

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
}

impl MenuPick for RowPick {
    fn element_name(&self) -> SharedString {
        match self {
            RowPick::Open { kind } => format!("row-menu-open-{kind}").into(),
            RowPick::Action { index } => format!("row-menu-action-{index}").into(),
        }
    }
}

/// The row menu for `ctx`: for each column in menu order (`ctx.first`,
/// then the rest in context order), a section headed `{column} · {value}`
/// holding "Open {Kind}" for every kind whose first accepted column (in
/// menu order) this is, then every action on this column. Sections with
/// no rows are left out; sections are parted by separators. Empty when
/// nothing applies.
pub fn menu_rows(ctx: &DimensionContext, roster: &ModuleRoster) -> Vec<Row<RowPick>> {
    let mut order: Vec<&(String, String)> = Vec::new();
    if let Some(first) = &ctx.first {
        order.extend(ctx.values.iter().filter(|(c, _)| c == first));
    }
    order.extend(
        ctx.values
            .iter()
            .filter(|(c, _)| Some(c) != ctx.first.as_ref()),
    );

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
            shape(&menu_rows(&ctx, &roster())),
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
        let s = shape(&menu_rows(&ctx, &roster()));
        assert_eq!(s[0], "# position_ref \u{b7} P7");
    }

    #[test]
    fn a_disabled_action_keeps_its_reason() {
        let ctx = DimensionContext::of(&[("position_ref", "P7")]);
        let rows = menu_rows(&ctx, &roster());
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
        let s = shape(&menu_rows(&ctx, &r));
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
        assert!(menu_rows(&DimensionContext::default(), &roster()).is_empty());
        assert!(menu_rows(&DimensionContext::of(&[("lhu", "7")]), &roster()).is_empty());
    }

    #[test]
    fn picks_name_the_kind_or_the_action_index() {
        let ctx = DimensionContext::of(&[("underlying_ref", "SPX"), ("position_ref", "P7")]);
        let picks: Vec<RowPick> = menu_rows(&ctx, &roster())
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
