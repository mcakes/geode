//! Moving rows: the predicate both the keys' steps and the grip's drag
//! land by, held to the mover's own rollup group under a value grouping.

use super::*;
use crate::core::reorder::Placements;
use crate::core::select::{OFF_END, OFF_GROUP_END};

/// The siblings a move may land beside, boxed: shown, and under a value
/// grouping painted in the movers' own group.
pub(crate) type Lands<'a> = Box<dyn Fn(usize) -> bool + 'a>;

impl PricerTile {
    /// What a move of `movers` (top-most rows sharing a parent) steps by,
    /// and its refusal at the end. With no value grouping: every shown
    /// sibling, to the end of the roots or the package ([`OFF_END`]).
    /// Under one: only siblings painted in the movers' own group node, so
    /// the step hops other groups' lines and the group's painted order
    /// changes exactly as asked; roots stop at the group's end
    /// ([`OFF_GROUP_END`]), legs at their package's ([`OFF_END`]). Movers
    /// painted under more than one group refuse.
    pub(crate) fn move_lands(
        &self,
        movers: &[usize],
    ) -> Result<(Lands<'_>, &'static str), &'static str> {
        let shown = |r: usize| self.visibility.is_shown(r);
        if !self.grouped() {
            return Ok((Box::new(shown), OFF_END));
        }
        let same = Placements::of(&self.rollup).same_node(movers)?;
        let roots = movers
            .first()
            .is_some_and(|&r| self.sheet.parent(r).is_none());
        let edge = if roots { OFF_GROUP_END } else { OFF_END };
        Ok((Box::new(move |r| shown(r) && same(r)), edge))
    }
}
