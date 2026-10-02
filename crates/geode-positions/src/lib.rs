//! "Move LHU…": the row menu action that asks the position system to move
//! positions to another LHU. It picks the target LHU from the live `lhu`
//! values, confirms, then sends one `MoveLhuParams` through the data
//! handle. Request-then-wait: nothing on screen changes until a snapshot
//! carries the move; the notice says `· sent`, and the position system's
//! answer replaces it.

use std::cell::Cell;
use std::rc::Rc;

use geode_core::context::DimensionContext;
use geode_core::positions::{MoveLhuParams, noun, sent_notice};
use geode_data::DataHandle;
use geode_shell::dimension::DimensionAction;
use geode_shell::shell::row_menu::ActionCx;
use gpui::SharedString;

/// The column naming one position.
const POSITION: &str = "position_ref";
/// The column naming a position's LHU.
const LHU: &str = "lhu";

/// The moving positions: the selection's when the target row is inside one
/// (every row must name one `position_ref`), else the target row's.
/// `Err(n)`: `n` acting rows name no single position (a subtotal over
/// several, or a row without one).
pub fn moving_positions(ctx: &DimensionContext) -> Result<Vec<String>, usize> {
    if !ctx.selection.is_empty() {
        return ctx.selection_values(POSITION);
    }
    match ctx.get(POSITION) {
        Some(p) => Ok(vec![p.to_string()]),
        None => Err(1),
    }
}

/// The LHU every moving position already shares, when the context says:
/// the target row's `lhu` without a selection; with one, the `lhu` every
/// selected row names, if they all name the same one.
pub fn shared_lhu(ctx: &DimensionContext) -> Option<String> {
    if ctx.selection.is_empty() {
        return ctx.get(LHU).map(str::to_string);
    }
    let mut shared: Option<&str> = None;
    for row in &ctx.selection {
        let lhu = row
            .iter()
            .find(|(c, _)| c == LHU)
            .map(|(_, v)| v.as_str())?;
        match shared {
            Some(s) if s != lhu => return None,
            _ => shared = Some(lhu),
        }
    }
    shared.map(str::to_string)
}

/// "Move LHU…" on a `position_ref`: choose an LHU from the live values
/// (the one the positions already share left out), confirm, send.
pub struct MoveLhu {
    data: DataHandle,
    configured: bool,
    /// The last command tag sent; the next command takes one more. Shared
    /// with the confirm's yes handler, which outlives the `chosen` call.
    next_tag: Rc<Cell<u64>>,
}

impl MoveLhu {
    /// `configured`: whether startup resolved a position service
    /// (`positions.toml`). Without one the action is disabled.
    pub fn new(data: DataHandle, configured: bool) -> Self {
        Self {
            data,
            configured,
            next_tag: Rc::default(),
        }
    }
}

impl DimensionAction for MoveLhu {
    fn id(&self) -> &'static str {
        "positions::move_lhu"
    }
    fn title(&self) -> SharedString {
        SharedString::new_static("Move LHU\u{2026}")
    }
    fn column(&self) -> &'static str {
        POSITION
    }
    fn available(&self, ctx: &DimensionContext) -> Result<(), SharedString> {
        if !self.configured {
            return Err(SharedString::new_static("no position service configured"));
        }
        match moving_positions(ctx) {
            Ok(_) => Ok(()),
            Err(n) => Err(format!("{n} selected rows hold several positions").into()),
        }
    }
    /// Ask for the LHU to move to: `lhu`'s live values, minus the one the
    /// moving positions already share.
    fn run(&self, ctx: &DimensionContext, acx: &mut ActionCx<'_, '_>) {
        acx.choose_value(
            ctx.clone(),
            LHU,
            "Move to LHU".into(),
            shared_lhu(ctx),
            "no LHU values to move to",
        );
    }
    /// Confirm the move; yes sends it and says it was sent, not done.
    fn chosen(&self, ctx: &DimensionContext, value: &str, acx: &mut ActionCx<'_, '_>) {
        // `available` gated the menu row on this.
        let Ok(positions) = moving_positions(ctx) else {
            return;
        };
        let n = positions.len();
        let question = format!("Move {n} {} to LHU {value}?", noun(n));
        let data = self.data.clone();
        let next_tag = self.next_tag.clone();
        let lhu = value.to_string();
        acx.confirm(
            question.into(),
            Rc::new(move |acx| {
                let tag = next_tag.get() + 1;
                next_tag.set(tag);
                let params = MoveLhuParams {
                    tag,
                    positions: positions.clone(),
                    lhu: lhu.clone(),
                };
                match data.move_lhu(params) {
                    Ok(()) => acx.notice(sent_notice(n, &lhu)),
                    Err(refusal) => acx.notice(format!("move to LHU {lhu} refused: {refusal}")),
                }
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(c, v)| (c.to_string(), v.to_string()))
            .collect()
    }

    fn action(configured: bool) -> MoveLhu {
        let (data, _rx) = DataHandle::for_tests();
        MoveLhu::new(data, configured)
    }

    #[test]
    fn the_target_row_moves_alone_without_a_selection() {
        let ctx = DimensionContext::of(&[("lhu", "L1"), ("position_ref", "P7")]);
        assert_eq!(moving_positions(&ctx), Ok(vec!["P7".to_string()]));
        assert_eq!(action(true).available(&ctx), Ok(()));
    }

    #[test]
    fn a_selection_moves_every_selected_row() {
        let mut ctx = DimensionContext::of(&[("position_ref", "P8")]);
        ctx.selection = vec![
            row(&[("lhu", "L2"), ("position_ref", "P9")]),
            row(&[("lhu", "L1"), ("position_ref", "P8")]),
        ];
        assert_eq!(
            moving_positions(&ctx),
            Ok(vec!["P9".to_string(), "P8".to_string()]),
            "every selected row, in selection order, not the target row alone"
        );
        assert_eq!(action(true).available(&ctx), Ok(()));
    }

    #[test]
    fn a_selection_with_a_subtotal_disables_move_lhu() {
        let mut ctx = DimensionContext::of(&[("position_ref", "P9")]);
        ctx.selection = vec![
            row(&[("lhu", "L1")]),
            row(&[("lhu", "L2"), ("position_ref", "P9")]),
        ];
        assert_eq!(moving_positions(&ctx), Err(1));
        assert_eq!(
            action(true).available(&ctx),
            Err(SharedString::from("1 selected rows hold several positions"))
        );
    }

    #[test]
    fn shared_lhu_is_the_common_lhu() {
        let target = DimensionContext::of(&[("lhu", "L1"), ("position_ref", "P7")]);
        assert_eq!(shared_lhu(&target).as_deref(), Some("L1"));
        assert_eq!(
            shared_lhu(&DimensionContext::of(&[("position_ref", "P7")])),
            None
        );

        let mut same = DimensionContext::of(&[("lhu", "L1"), ("position_ref", "P7")]);
        same.selection = vec![
            row(&[("lhu", "L1"), ("position_ref", "P7")]),
            row(&[("lhu", "L1"), ("position_ref", "P8")]),
        ];
        assert_eq!(shared_lhu(&same).as_deref(), Some("L1"));

        let mut mixed = same.clone();
        mixed.selection[1] = row(&[("lhu", "L2"), ("position_ref", "P8")]);
        assert_eq!(
            shared_lhu(&mixed),
            None,
            "the target row's L1 is not shared"
        );

        let mut missing = same.clone();
        missing.selection[1] = row(&[("position_ref", "P8")]);
        assert_eq!(shared_lhu(&missing), None);
    }

    #[test]
    fn without_a_position_service_move_lhu_is_disabled() {
        let ctx = DimensionContext::of(&[("position_ref", "P7")]);
        assert_eq!(
            action(false).available(&ctx),
            Err(SharedString::from("no position service configured"))
        );
    }

    #[test]
    fn the_action_sits_under_position_ref_as_move_lhu() {
        let a = action(true);
        assert_eq!(a.id(), "positions::move_lhu");
        assert_eq!(a.column(), "position_ref");
        assert_eq!(a.title().as_ref(), "Move LHU\u{2026}");
    }
}
