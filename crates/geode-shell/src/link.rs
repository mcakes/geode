//! Link-group state held by the frame: four group lanes and which tile
//! follows and emits into which. Pure: no entity, no window. The frame owns
//! one [`Links`] and draws every scope generation from its own counter, so
//! a number names one scope in any lane or group.

use std::collections::BTreeMap;

use geode_core::link::{Group, Membership};
use geode_core::scope::Scope;

use crate::tiling::TileId;

/// One group's selection. A group carries a scope only; a follower's
/// grouping and as-of stay its workspace's.
#[derive(Debug, Default)]
pub(crate) struct GroupLane {
    pub(crate) scope: Scope,
    pub(crate) scope_gen: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Links {
    groups: [GroupLane; 4],
    following: BTreeMap<TileId, Group>,
    emitting: BTreeMap<TileId, Group>,
}

/// Set or clear one tile's entry; `true` when it changed.
fn assign(map: &mut BTreeMap<TileId, Group>, tile: TileId, to: Option<Group>) -> bool {
    match to {
        Some(g) => map.insert(tile, g) != Some(g),
        None => map.remove(&tile).is_some(),
    }
}

impl Links {
    pub(crate) fn group(&self, g: Group) -> &GroupLane {
        &self.groups[g.index()]
    }

    pub(crate) fn following(&self, tile: TileId) -> Option<Group> {
        self.following.get(&tile).copied()
    }

    pub(crate) fn membership(&self, tile: TileId) -> Membership {
        Membership {
            follow: self.following(tile),
            emit: self.emitting.get(&tile).copied(),
        }
    }

    pub(crate) fn follow(&mut self, tile: TileId, to: Option<Group>) -> bool {
        assign(&mut self.following, tile, to)
    }

    pub(crate) fn emit(&mut self, tile: TileId, to: Option<Group>) -> bool {
        assign(&mut self.emitting, tile, to)
    }

    /// Replace a group's scope, drawing its generation from the frame's
    /// counter. An equal scope is not a write.
    pub(crate) fn set_scope(&mut self, g: Group, scope: Scope, generation: &mut u64) -> bool {
        let lane = &mut self.groups[g.index()];
        if lane.scope == scope {
            return false;
        }
        lane.scope = scope;
        *generation += 1;
        lane.scope_gen = *generation;
        true
    }

    /// Each group's scope generation, in `Group::ALL` order.
    pub(crate) fn scope_gens(&self) -> [u64; 4] {
        [0, 1, 2, 3].map(|i| self.groups[i].scope_gen)
    }
}
