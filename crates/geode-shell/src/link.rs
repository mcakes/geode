//! Link-group state held by the frame: four group lanes, which tile follows
//! and emits into which, and each group's board. Pure: no entity, no window.
//! The frame owns one [`Links`] and draws every scope generation from its
//! own counter, so a number names one scope in any lane or group.
//!
//! A board holds draft documents. It is derived from the last emission of
//! every tile emitting into the group, so a draft leaves the moment its
//! emitter stops listing it, leaves the group or closes. Board changes move
//! their own revisions ([`BoardWatch`]) and never the frame's publish
//! counter: a draft is not a publish.
//!
//! [`group_color`] is each group's color under a theme: the one place the
//! four hues live, read by every surface that marks a group.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use geode_core::colour::{Definition, Tone, resolve};
use geode_core::document::{DocumentRows, is_key_prefix, join_key};
use geode_core::link::{Emission, Group, Membership};
use geode_core::scope::Scope;
use gpui::Hsla;
use gpui_component::Theme;

use crate::shell::colours::{anchors_from_theme, to_hsla, tokens_from_theme};
use crate::tiling::TileId;

/// Each group's hue in degrees on the theme's own wheel, in `Group::ALL`
/// order. Themes name no group colors, so these are generated, spread
/// around the wheel. The readability floor then moves each one's lightness
/// toward the foreground, which on a low-chroma or light theme brings hues
/// together. `every_bundled_theme_keeps_the_group_colors_readable_and_distinct`
/// holds the four a minimum distance apart, as painted, on every bundled
/// theme; a hue changed here without that sweep can merge two groups on a
/// theme nobody looked at. These four pass the sweep at its separation
/// floor (`GROUP_SEPARATION`). A group's mark always carries its letter:
/// the color is a second cue beside it, not the identity.
const HUES: [f32; 4] = [215.0, 25.0, 285.0, 130.0];

/// A group's color under `theme`: its hue between the theme's anchors,
/// floored to the generated-color contrast against the theme's background.
/// A surface paints it unchanged (a chip's fill through `chip::colored`):
/// a second floor applied to it would move it out from under the sweep.
/// Resolved per call from the live theme, so a theme change shows on the
/// next paint; a caller painting many marks per frame resolves once.
pub fn group_color(theme: &Theme, group: Group) -> Hsla {
    to_hsla(resolve(
        &Definition::hue(HUES[group.index()], Tone::Normal),
        &anchors_from_theme(theme),
        &tokens_from_theme(theme),
    ))
}

/// A board's key: the dataset and the document key within it.
type BoardKey = (String, Vec<String>);

/// One draft on a group's board.
#[derive(Debug)]
struct Posted {
    rows: Arc<DocumentRows>,
    /// The tile whose emission put it there. Recorded for diagnostics;
    /// nothing reads it.
    #[allow(dead_code)]
    emitter: TileId,
}

/// One group's scope and board. A group selects by scope only: a
/// follower's grouping and as-of stay its workspace's.
#[derive(Debug, Default)]
pub(crate) struct GroupLane {
    pub(crate) scope: Scope,
    pub(crate) scope_gen: u64,
    board: BTreeMap<BoardKey, Posted>,
    /// Counts this board's changes; a repeated emission leaves it alone.
    board_gen: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Links {
    groups: [GroupLane; 4],
    following: BTreeMap<TileId, Group>,
    emitting: BTreeMap<TileId, Group>,
    /// Each emitter's last emission and the order it was posted in. A
    /// group's board is derived from these, so an emitter that leaves
    /// uncovers a key another emitter still lists.
    last: HashMap<TileId, (u64, Emission)>,
    seq: u64,
    /// Board watches, held weakly: a closed tile drops its watch and the
    /// next registration reaps the entry.
    watches: Vec<WatchSlot>,
    /// Source of every board watch revision. Separate from the frame's
    /// publish counter: a draft keystroke must not requery tiles that watch
    /// published data.
    board_rev: u64,
}

#[derive(Debug)]
struct WatchSlot {
    group: Group,
    dataset: String,
    /// The joined key, or a prefix of it; `None` hears every key.
    key: Option<String>,
    revision: Weak<Cell<u64>>,
}

/// A tile's retained interest in one group's board: a dataset, or one
/// document key (or key prefix) in it. Read `revision()` and compare with
/// the value last acted on; a board change is never staged behind a flip.
#[derive(Debug, Clone)]
pub struct BoardWatch {
    group: Group,
    dataset: String,
    key: Option<String>,
    revision: Rc<Cell<u64>>,
}

impl BoardWatch {
    pub fn revision(&self) -> u64 {
        self.revision.get()
    }

    /// Whether this watch was registered for exactly this group, dataset
    /// and key: the test for keeping a watch or registering another.
    pub fn is_for(&self, group: Group, dataset: &str, key: Option<&str>) -> bool {
        self.group == group && self.dataset == dataset && self.key.as_deref() == key
    }
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

    /// Point `tile`'s emissions at `to`. A tile that leaves or switches
    /// group takes what it posted with it: its drafts leave the old board
    /// at once and nothing reaches the new one until its next post. The
    /// old group's scope stays as last written.
    pub(crate) fn emit(&mut self, tile: TileId, to: Option<Group>) -> bool {
        let left = self.emitting.get(&tile).copied();
        if !assign(&mut self.emitting, tile, to) {
            return false;
        }
        self.last.remove(&tile);
        if let Some(g) = left {
            self.rebuild_board(g);
        }
        true
    }

    /// Drop a closed tile's membership and what it posted. `true` when it
    /// was in a group.
    pub(crate) fn forget(&mut self, tile: TileId) -> bool {
        let followed = self.follow(tile, None);
        let emitted = self.emit(tile, None);
        followed | emitted
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

    /// Record `tile`'s emission. `true` when the group's scope or board
    /// changed. An emission equal to the tile's last is not a write: the
    /// shell re-pulls on every notify of an emitting tile.
    pub(crate) fn post(&mut self, tile: TileId, emission: Emission, generation: &mut u64) -> bool {
        let Some(&g) = self.emitting.get(&tile) else {
            return false;
        };
        if self.last.get(&tile).is_some_and(|(_, e)| *e == emission) {
            return false;
        }
        // Compared before it is cloned: a draft edited at typing speed posts
        // a changed board under the scope the group already holds.
        let changed = match &emission.scope {
            Some(scope) if self.groups[g.index()].scope != *scope => {
                self.set_scope(g, scope.clone(), generation)
            }
            _ => false,
        };
        self.seq += 1;
        self.last.insert(tile, (self.seq, emission));
        changed | self.rebuild_board(g)
    }

    /// Derive `g`'s board from its emitters' last emissions, later posts
    /// winning a key, and bump the watches of every key that changed.
    /// `true` when the board changed.
    fn rebuild_board(&mut self, g: Group) -> bool {
        let mut posts: Vec<(u64, TileId, &Emission)> = self
            .last
            .iter()
            .filter(|(tile, _)| self.emitting.get(tile) == Some(&g))
            .map(|(tile, (seq, e))| (*seq, *tile, e))
            .collect();
        posts.sort_by_key(|p| p.0);
        let mut next: BTreeMap<BoardKey, Posted> = BTreeMap::new();
        for (_, emitter, emission) in posts {
            for entry in &emission.board {
                next.insert(
                    (entry.dataset.clone(), entry.key.clone()),
                    Posted {
                        rows: Arc::clone(&entry.rows),
                        emitter,
                    },
                );
            }
        }
        let lane = &mut self.groups[g.index()];
        // A key changed when it left, arrived, or now holds another
        // allocation; the dataset and joined key are what a watch matches.
        let mut touched: Vec<(String, String)> = Vec::new();
        for (key, old) in &lane.board {
            if !next
                .get(key)
                .is_some_and(|new| Arc::ptr_eq(&new.rows, &old.rows))
            {
                touched.push((key.0.clone(), join_key(&key.1)));
            }
        }
        for key in next.keys() {
            if !lane.board.contains_key(key) {
                touched.push((key.0.clone(), join_key(&key.1)));
            }
        }
        lane.board = next;
        if touched.is_empty() {
            return false;
        }
        lane.board_gen += 1;
        self.board_rev += 1;
        for slot in &self.watches {
            let hears = slot.group == g
                && touched.iter().any(|(dataset, key)| {
                    slot.dataset == *dataset
                        && slot.key.as_deref().is_none_or(|w| is_key_prefix(w, key))
                });
            if hears && let Some(revision) = slot.revision.upgrade() {
                revision.set(self.board_rev);
            }
        }
        true
    }

    /// Watch `g`'s board for `dataset`, or for one joined document key (or
    /// a prefix of it at a part boundary). A watch already held for the
    /// same interest is shared; a new one starts at the current revision,
    /// so registering is not itself a change.
    pub(crate) fn watch(&mut self, g: Group, dataset: &str, key: Option<&str>) -> BoardWatch {
        // Reap on registration, not on every post: the list follows live
        // interests, hidden tiles included.
        self.watches
            .retain(|slot| slot.revision.strong_count() != 0);
        let held = self
            .watches
            .iter()
            .find(|slot| slot.group == g && slot.dataset == dataset && slot.key.as_deref() == key)
            .and_then(|slot| slot.revision.upgrade());
        let revision = held.unwrap_or_else(|| {
            let revision = Rc::new(Cell::new(self.board_rev));
            self.watches.push(WatchSlot {
                group: g,
                dataset: dataset.to_owned(),
                key: key.map(str::to_owned),
                revision: Rc::downgrade(&revision),
            });
            revision
        });
        BoardWatch {
            group: g,
            dataset: dataset.to_owned(),
            key: key.map(str::to_owned),
            revision,
        }
    }

    /// The draft on `g`'s board for exactly this dataset and document key.
    pub(crate) fn entry(
        &self,
        g: Group,
        dataset: &str,
        key: &[String],
    ) -> Option<Arc<DocumentRows>> {
        // A board holds a handful of drafts: scanning them costs less than
        // building an owned key to look one up.
        self.groups[g.index()]
            .board
            .iter()
            .find(|((d, k), _)| d == dataset && k.as_slice() == key)
            .map(|(_, posted)| Arc::clone(&posted.rows))
    }

    pub(crate) fn board_gen(&self, g: Group) -> u64 {
        self.groups[g.index()].board_gen
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::link::BoardEntry;

    fn rows() -> Arc<DocumentRows> {
        Arc::new(DocumentRows {
            key: Vec::new(),
            attributes: Vec::new(),
            axes: Vec::new(),
            values: Vec::new(),
        })
    }

    fn posting(dataset: &str, key: &[&str]) -> Emission {
        Emission {
            scope: None,
            board: vec![BoardEntry {
                dataset: dataset.into(),
                key: key.iter().map(|part| part.to_string()).collect(),
                rows: rows(),
            }],
        }
    }

    /// Two tiles watching the same key share one revision cell, and a
    /// dropped watch does not keep its slot: the list would otherwise grow
    /// with every tile ever opened and every key ever selected.
    #[test]
    fn a_board_watch_is_shared_while_held_and_reaped_once_dropped() {
        let mut links = Links::default();
        let mut generation = 0;
        let tile = TileId(1);
        links.emit(tile, Some(Group::A));
        let first = links.watch(Group::A, "cvi_params", Some("SPX.Z"));
        let again = links.watch(Group::A, "cvi_params", Some("SPX.Z"));
        assert!(Rc::ptr_eq(&first.revision, &again.revision));
        assert_eq!(links.watches.len(), 1);

        assert!(links.post(tile, posting("cvi_params", &["SPX.Z"]), &mut generation));
        assert_eq!((first.revision(), again.revision()), (1, 1));
        drop((first, again));
        let late = links.watch(Group::A, "cvi_params", Some("NDX"));
        assert_eq!(links.watches.len(), 1, "the dropped watch was reaped");
        assert_eq!(
            late.revision(),
            1,
            "a new watch starts at the current revision"
        );
    }

    /// A watch on an underlying hears every expiry posted under it, and a
    /// key that merely starts with the same characters is another
    /// document.
    #[test]
    fn a_board_watch_hears_keys_under_its_prefix_at_a_part_boundary() {
        let mut links = Links::default();
        let mut generation = 0;
        let tile = TileId(1);
        links.emit(tile, Some(Group::A));
        let key = ["SPX".to_string(), "2026-10-16".to_string()];
        let under = links.watch(Group::A, "vol_slices", Some("SPX"));
        let exact = links.watch(Group::A, "vol_slices", Some(&join_key(&key)));
        let sibling = links.watch(Group::A, "vol_slices", Some("SP"));
        let other_dataset = links.watch(Group::A, "cvi_params", Some("SPX"));

        assert!(links.post(
            tile,
            posting("vol_slices", &["SPX", "2026-10-16"]),
            &mut generation
        ));
        assert_eq!((under.revision(), exact.revision()), (1, 1));
        assert_eq!((sibling.revision(), other_dataset.revision()), (0, 0));
        assert!(links.entry(Group::A, "vol_slices", &key).is_some());
        assert!(links.entry(Group::A, "vol_slices", &key[..1]).is_none());
        assert!(links.entry(Group::A, "cvi_params", &key).is_none());
        assert_eq!(generation, 0, "a board post draws no scope generation");
    }

    /// How far apart two group chips' fills must stay in OKLab. Lower than
    /// the chart palette's 0.07 because a group's mark always carries its
    /// letter: the letter is the identity and the color a second cue,
    /// where a chart series has its color alone. 0.07 is also out of reach
    /// for four `Tone::Normal` hues: none hold it on every bundled theme,
    /// and the best tuple on a 5-degree grid reaches about 0.054.
    const GROUP_SEPARATION: f32 = 0.05;

    /// How many bundled themes the sweep must visit. A theme file that
    /// fails to parse yields a load warning, which the sweep asserts
    /// empty. The count covers the other way to sweep too few: a file
    /// left out of the bundle warns of nothing, and the loop passes over
    /// themes that are not there.
    const BUNDLED_THEMES: usize = 44;

    /// The sweep measures the chip as painted (`chip::colored` over the
    /// header's surface), not the color it starts from: a floor applied
    /// between the two would otherwise go unguarded. On every bundled
    /// theme each group's fill must clear the generated-color floor
    /// against the surface, its text must clear the text floor on that
    /// fill, and the four fills must stand apart, or two groups read as
    /// one at a glance.
    #[gpui::test]
    fn every_bundled_theme_keeps_the_group_colors_readable_and_distinct(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::shell::chip;
        use crate::shell::colours::{over, to_rgb};
        use geode_core::colour::oklab::srgb_to_oklab;
        use geode_core::colour::{READABLE_RATIO, contrast_ratio};
        use gpui_component::ActiveTheme as _;

        cx.update(gpui_component::init);
        let (service, warnings) = crate::theme::load_bundled();
        assert!(warnings.is_empty(), "{warnings:?}");
        let mut failures = Vec::new();
        let mut themes = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                themes += 1;
                let surface = to_rgb(theme.background);
                let paints = Group::ALL
                    .map(|g| chip::colored(theme, group_color(theme, g), theme.background));
                let colors = paints.map(|p| over(p.fill.expect("a group chip is filled"), surface));
                for (i, g) in Group::ALL.into_iter().enumerate() {
                    let ratio = contrast_ratio(colors[i], surface);
                    if ratio < READABLE_RATIO {
                        failures.push(format!("{name}: group {} at {ratio:.2}:1", g.letter()));
                    }
                    if !chip::is_readable(theme, &paints[i]) {
                        failures.push(format!("{name}: group {} text is unreadable", g.letter()));
                    }
                    let a = srgb_to_oklab(colors[i]);
                    for (j, other) in Group::ALL.into_iter().enumerate().skip(i + 1) {
                        let b = srgb_to_oklab(colors[j]);
                        let distance =
                            ((a.l - b.l).powi(2) + (a.a - b.a).powi(2) + (a.b - b.b).powi(2))
                                .sqrt();
                        if distance < GROUP_SEPARATION {
                            failures.push(format!(
                                "{name}: groups {} and {} only {distance:.3} apart",
                                g.letter(),
                                other.letter()
                            ));
                        }
                    }
                }
            });
        }
        assert!(
            themes >= BUNDLED_THEMES,
            "the sweep saw {themes} themes: bundled themes missing?"
        );
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
