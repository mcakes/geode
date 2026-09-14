//! One diagnostics tile (Phase 4b Task 5, spec §4.6): observes the
//! shell-owned `Diagnostics` entity and the frame, rebuilds one of five
//! row lists only when an observed version changes, and paints them as a
//! `uniform_list` in the mono face.

use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::time::SystemTime;

use geode_core::config::Config;
use geode_core::log::{Record, Ring};
use geode_core::query::QueryKey;
use geode_shell::actions::ActionId;
use geode_shell::fonts;
use geode_shell::frame::{Frame, FrameVersions};
use geode_shell::keymap::KeyContext;
use geode_shell::module::FindEvent;
use geode_shell::tiling::TileId;
use gpui::prelude::*;
use gpui::{
    App, Context, Entity, IntoElement, ScrollStrategy, SharedString, UniformListScrollHandle,
    Window, div, px, uniform_list,
};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use crate::commands::{self, Command, Section};
use crate::sections::{self, Row, Tone};

/// The bounded local tail of log records this tile keeps (spec §4.6:
/// "the ring tail" — a per-tile copy, not the whole ring's own
/// capacity), oldest dropped first once full.
const LOG_CAP: usize = 4_096;

/// Which `FrameVersions` counters matter at all to this tile — `as_of`
/// (`sections::data_rows`) and `config` (the config section's explainer)
/// are the only two any section reads; `scope`/`grouping`/`flip` never
/// are (Phase 4b Task 5 fix round 1, MAJ-6 first drew this line; the
/// final review's MAJ-4 then narrowed it further — see
/// `diag_version_for_section` and the frame observer below, which now
/// also gate on *which* section is showing, not just on this pair).
/// `Frame::set_scope_in_session` bumps `scope` on *every keystroke* in
/// the toolbar's text field (Phase 4a requeries per keystroke by
/// design), so comparing it here would rebuild this tile's full row list
/// inside the same <50ms window §7.1 gives the requery that keystroke
/// just launched, for sections that have nothing to do with either
/// counter. `data` is redundant for a different reason: a publish
/// already reaches `Diagnostics::note_published`, which bumps the
/// diagnostics entity's own `data` version the other observer already
/// watches. `flip` stays excluded — CLAUDE.md: `flip` only tells a tile
/// with an already-staged snapshot that it may promote; this tile never
/// stages anything.
///
/// Which of `Diagnostics::versions`'s five counters the given section's
/// row builder actually reads (Phase 4b final review, MAJ-4) — see
/// `DiagVersions`'s own doc for the full mapping. `Section::Log`'s other
/// half — the ring's own `latest_seq` — is not a `Diagnostics` version at
/// all and is compared separately, at the one call site below that needs
/// it, against `latest_seq`.
fn diag_version_for_section(section: Section, v: geode_shell::diagnostics::DiagVersions) -> u64 {
    match section {
        Section::Sources => v.sources,
        Section::Data => v.data,
        Section::Config => v.config,
        Section::Log => v.log_levels,
        Section::Perf => v.perf,
    }
}

/// The header row's text (MIN-1, final review) — a pure function of
/// `section` alone, computed once per real section change rather than
/// on every paint.
fn header_text_for(section: Section) -> SharedString {
    format!("diagnostics · {} · [ ] to switch", section.name()).into()
}

pub struct DiagnosticsTile {
    tile: TileId,
    frame: Entity<Frame>,
    diagnostics: Entity<geode_shell::diagnostics::Diagnostics>,
    ring: Arc<Ring>,
    config: Rc<RefCell<Config>>,
    section: Section,
    cursor: usize,
    collapsed: BTreeSet<String>,
    filter: String,
    /// The log section only: keeps the cursor on the last row until the
    /// user moves it (spec §4.6).
    follow: bool,
    /// The ring sequence this tile has drained up to.
    since: u64,
    /// How many records this tile's own `since` had already fallen behind
    /// `ring.oldest_seq()` as of the last DRAIN that found new records
    /// (`Ring::oldest_seq`'s own doc comment: "a reader whose own since
    /// has already been overwritten can compare against this to know it
    /// was lapped, and by how much"). Recomputed fresh — not
    /// accumulated — every time the log section's rebuild finds
    /// `ring.latest_seq() > self.since`; a rebuild that finds nothing new
    /// leaves the previous value in place, which is still the true
    /// answer as of that unchanged `since` (MIN-3, final review: not a
    /// running total of past gaps, and never a number this tile did not
    /// itself compute). `since` is seeded from `ring.latest_seq()` at
    /// construction (MIN-3's other half), so a tile opened after the
    /// ring already wrapped past its own capacity does not claim records
    /// it never had on its very first drain. Surfaced as a leading "N
    /// records lost" row (spec §4.6's log section) when nonzero.
    lost_records: u64,
    /// Reused across drains (Phase 4b Task 5 fix round 1, MAJ-5):
    /// `Ring::drain_since`'s own doc comment says a reader "reuses one
    /// `Vec` for its life" — a fresh `Vec::new()` per drain violated
    /// that. Always empty between calls (`Vec::drain(..)` empties it into
    /// `records` right after each fill), so its only cost is the
    /// capacity it grows into and keeps.
    drain_buf: Vec<Record>,
    records: VecDeque<Record>,
    /// `Rc`, not a plain `Vec` (Phase 4b Task 5 fix round 1, MAJ-4):
    /// `render` clones this into the `uniform_list` closure on every
    /// single paint — every shell repaint, not just this tile's own
    /// rebuilds — so a `Vec<Row>` clone there would be a full
    /// re-allocation plus a `SharedString` refcount bump per row (up to
    /// `LOG_CAP` = 4,096 of both) on every keystroke anywhere in the
    /// shell. `rebuild` is the only place this is ever replaced (with a
    /// fresh `Rc`); every other reader — `render`, the test accessors —
    /// clones the `Rc` itself, which is one atomic increment regardless
    /// of row count.
    rows: Rc<Vec<Row>>,
    /// Per-population, not the entity's single combined `version()`
    /// (MAJ-4, final review) — the observer below compares only the
    /// field(s) [`diag_version_for_section`] says the current section
    /// reads, so a perf-only tick (the 500ms frame-histogram copy) does
    /// not rebuild, say, the config section's expensive explainer walk.
    last_diag_versions: geode_shell::diagnostics::DiagVersions,
    last_frame_versions: FrameVersions,
    /// MIN-1 (final review): a pure function of `section` alone, cached
    /// beside it and replaced only in `set_section` — `render` used to
    /// `format!()` this fresh on every single paint, not just this
    /// tile's own rebuilds.
    header_text: SharedString,
    visible: bool,
    scroll: UniformListScrollHandle,
    #[cfg(test)]
    rebuild_count: u32,
}

impl DiagnosticsTile {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tile: TileId,
        frame: Entity<Frame>,
        diagnostics: Entity<geode_shell::diagnostics::Diagnostics>,
        ring: Arc<Ring>,
        config: Rc<RefCell<Config>>,
        restored: Option<&toml::Table>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let section = restored
            .and_then(|t| t.get("section"))
            .and_then(|v| v.as_str())
            .and_then(|name| Section::ALL.iter().find(|s| s.name() == name).copied())
            .unwrap_or(Section::Sources);
        let filter = restored
            .and_then(|t| t.get("filter"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_default();

        let last_diag_versions = diagnostics.read(cx).versions();
        let last_frame_versions = frame.read(cx).versions();
        // MIN-3 (final review): `since` starts at the ring's CURRENT
        // `latest_seq`, not `0` — a tile constructed after the ring
        // already holds records (a second tile, a session already in
        // progress) never had those records to lose, so it must not
        // report `oldest_seq() - 1` of them "lost" on its very first
        // drain.
        let initial_since = ring.latest_seq();

        cx.observe(&diagnostics, |this, diagnostics, cx| {
            let now = diagnostics.read(cx).versions();
            // MAJ-4 (final review): the log section's staleness check is
            // the ring's own `latest_seq`, not a `Diagnostics` version —
            // compared here (not deferred into `rebuild`) so a tick that
            // touches neither the ring nor `log_levels` skips the rebuild
            // entirely, same as every other section.
            let relevant = if this.section == Section::Log {
                this.ring.latest_seq() > this.since
                    || now.log_levels != this.last_diag_versions.log_levels
            } else {
                diag_version_for_section(this.section, now)
                    != diag_version_for_section(this.section, this.last_diag_versions)
            };
            this.last_diag_versions = now;
            if relevant {
                this.rebuild(cx);
            }
        })
        .detach();
        // MIN-7 (final review): the `Config` `Section::Config` rebuilds
        // from (`self.config`, below) is the SAME `Rc<RefCell<Config>>`
        // `geode-app::main`'s own `cx.observe(&frame, ..)` refreshes on
        // this identical `config` version bump — freshness here depends
        // on that other observer having already run this render, which
        // holds only because it is registered first, at window setup,
        // before any diagnostics tile can exist to register this one.
        // gpui invokes an entity's observers in registration order; see
        // that call site's own comment for the fuller story.
        cx.observe(&frame, |this, frame, cx| {
            let now = frame.read(cx).versions();
            let as_of_changed = now.as_of != this.last_frame_versions.as_of;
            let config_changed = now.config != this.last_frame_versions.config;
            // MAJ-4 (final review): narrowed the same way as the
            // diagnostics observer above — a config reload must rebuild
            // the config section, but not a tile currently showing
            // sources/data/log/perf, none of which read the frame's
            // `config` version; the frame's `as_of` is `Data`'s alone.
            let relevant = match this.section {
                Section::Data => as_of_changed,
                Section::Config => config_changed,
                Section::Sources | Section::Log | Section::Perf => false,
            };
            this.last_frame_versions = now;
            if relevant {
                this.rebuild(cx);
            }
            // Phase 4b Task 5 fix round 1, MAJ-7: the data thread computes
            // the data section's resolved-generation marker under the
            // as-of carried on the *request* that produced the held
            // `CatalogSnapshot` — nothing re-requested one when the as-of
            // changed, so a stale snapshot kept marking a generation the
            // engine would no longer resolve to. Only while visible
            // (`watch`'s own reasoning: a request whose outcome nothing
            // will show is a database round trip spent for nothing).
            if as_of_changed && this.visible {
                this.diagnostics.update(cx, |d, cx| {
                    d.request_catalog();
                    cx.notify();
                });
            }
            // Phase 4 §3.10, found by the market-data panel's own review
            // (2026-09-14): this tile is a real occupant, so
            // `ShellView::visible_tile_keys` puts its key in every
            // barrier — and it submits no query through the pool and so
            // never arrives, holding every blotter on screen open until
            // `FLIP_DEADLINE` (250 ms) on every scope keystroke, an as-of
            // change and a grouping change alike. It has nothing coming,
            // ever, so it answers unconditionally; `barrier_wants` is the
            // only gate needed (a hidden tile's key was never in the set,
            // since `visible_tile_keys` is what built it).
            let key = QueryKey(this.tile.0);
            if frame.read(cx).barrier_wants(key, now) {
                frame.update(cx, |f, cx| {
                    if f.arrived(key, now) {
                        cx.notify();
                    }
                });
            }
        })
        .detach();

        let mut this = DiagnosticsTile {
            tile,
            frame,
            diagnostics,
            ring,
            config,
            section,
            cursor: 0,
            collapsed: BTreeSet::new(),
            filter,
            follow: true,
            since: initial_since,
            lost_records: 0,
            drain_buf: Vec::new(),
            records: VecDeque::new(),
            rows: Rc::new(Vec::new()),
            last_diag_versions,
            last_frame_versions,
            header_text: header_text_for(section),
            visible: false,
            scroll: UniformListScrollHandle::new(),
            #[cfg(test)]
            rebuild_count: 0,
        };
        this.rebuild(cx);
        this
    }

    /// Rebuild `self.rows` from the current section's row builder. The
    /// one and only place any of the five pure builders in `sections.rs`
    /// are called — every caller here is an observed version change
    /// (`new`'s initial call, or the two `cx.observe` closures above).
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        #[cfg(test)]
        {
            self.rebuild_count += 1;
        }
        if self.section == Section::Log {
            let latest = self.ring.latest_seq();
            if latest > self.since {
                // `Ring::oldest_seq`'s own doc comment: read BEFORE
                // draining, so this reflects what was still retained at
                // the moment we asked — a gap exists only when the
                // oldest surviving record's seq is more than one past
                // `since` (seq `since + 1` itself is still readable; see
                // that method's `oldest_seq_is_none_when_empty_then_
                // tracks_the_surviving_floor_through_a_wrap` test).
                self.lost_records = self
                    .ring
                    .oldest_seq()
                    .map(|oldest| oldest.saturating_sub(self.since + 1))
                    .unwrap_or(0);
                // MAJ-5 (fix round 1): `drain_buf` is a persistent field,
                // not a fresh `Vec::new()` per call — `Ring::drain_since`'s
                // own contract ("a tile following the tail reuses one
                // `Vec` for its life") named this exact shape.
                self.ring.drain_since(self.since, &mut self.drain_buf);
                self.since = latest;
                // `drain(..)` moves the records out of `drain_buf` (no
                // clone — they were already cloned once, by `drain_since`
                // itself, which the ring's own doc comment sanctions)
                // into the tail, leaving `drain_buf` empty but with its
                // capacity intact for the next call.
                self.records.extend(self.drain_buf.drain(..));
                while self.records.len() > LOG_CAP {
                    self.records.pop_front();
                }
            }
        }
        let now = SystemTime::now();
        let new_rows: Vec<Row> = {
            let d = self.diagnostics.read(cx);
            let frame = self.frame.read(cx);
            match self.section {
                Section::Sources => sections::sources_rows(d, now),
                Section::Data => sections::data_rows(d, frame.as_of(), &self.collapsed),
                Section::Config => sections::config_rows(d, &self.config.borrow(), &self.filter),
                Section::Log => {
                    // MAJ-5 (fix round 1): `make_contiguous` hands back a
                    // slice of the existing `VecDeque` storage — no clone
                    // of the tail (up to `LOG_CAP` = 4,096 `Record`s, each
                    // with its own `String`) on every rebuild, including
                    // rebuilds that have nothing to do with the log (a
                    // health note, a poll, ...).
                    let records = self.records.make_contiguous();
                    let mut rows = sections::log_rows(records, &self.filter);
                    if self.lost_records > 0 {
                        rows.insert(
                            0,
                            Row {
                                text: format!(
                                    "{} records lost — the ring wrapped",
                                    self.lost_records
                                )
                                .into(),
                                depth: 0,
                                tone: Tone::Warn,
                                collapsible: None,
                            },
                        );
                    }
                    rows
                }
                Section::Perf => sections::perf_rows(d, &frame.requery),
            }
        };
        // MAJ-4 (fix round 1): a fresh `Rc` here, once per rebuild — every
        // *reader* (`render`, chiefly, on every paint) clones the `Rc`
        // itself, never this `Vec`.
        self.rows = Rc::new(new_rows);
        if self.section == Section::Log && self.follow {
            self.cursor = self.rows.len().saturating_sub(1);
        } else {
            self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        }
        // MAJ-1 (fix round 1): a rebuild is the "follow-mode append" case
        // — `follow` just moved the cursor to the tail above, and a
        // uniform_list does not track a cursor index on its own. Cheap
        // and idempotent to call even when the cursor didn't move (non-
        // strict scrolling: a no-op if already visible).
        self.sync_scroll();
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn rebuild_count(&self) -> u32 {
        self.rebuild_count
    }

    #[cfg(test)]
    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// MAJ-4 (fix round 1): a clone of the `Rc` itself, for
    /// `Rc::ptr_eq` — pins "two paints with no rebuild between them share
    /// the same allocation" (a real `Vec` clone would still pass a
    /// content equality check, which is why the test needs pointer
    /// identity, not `rows()`'s slice).
    #[cfg(test)]
    pub(crate) fn rows_rc(&self) -> Rc<Vec<Row>> {
        self.rows.clone()
    }

    #[cfg(test)]
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    #[cfg(test)]
    pub(crate) fn follow(&self) -> bool {
        self.follow
    }

    #[cfg(test)]
    pub(crate) fn section(&self) -> Section {
        self.section
    }

    /// MIN-1 (final review): the header's own cache — a clone (cheap:
    /// `SharedString` is `Arc`-backed once past its inline-string
    /// threshold), for comparing `.as_ptr()` across paints the way
    /// `rows_rc`'s `Rc::ptr_eq` pins the row list's.
    #[cfg(test)]
    pub(crate) fn header_text(&self) -> SharedString {
        self.header_text.clone()
    }

    /// MAJ-1 (fix round 1): the row index `scroll_to_item` most recently
    /// asked the list to show — `UniformListScrollHandle::
    /// logical_scroll_top_index`'s own doc comment: "the index of the
    /// topmost visible child", answered from the still-pending deferred
    /// scroll when one is queued (exactly the case right after a cursor
    /// move, before the next paint consumes it).
    #[cfg(test)]
    pub(crate) fn scroll_target(&self) -> usize {
        self.scroll.logical_scroll_top_index()
    }

    #[cfg(test)]
    pub(crate) fn drain_buf_capacity(&self) -> usize {
        self.drain_buf.capacity()
    }

    pub fn key_context(&self) -> KeyContext {
        KeyContext::new("diagnostics")
            .pair("section", self.section.name())
            .counts()
    }

    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len() as isize;
        let target = (self.cursor as isize + delta).clamp(0, len - 1);
        self.cursor = target as usize;
        self.follow = false;
        self.sync_scroll();
        cx.notify();
    }

    /// MAJ-1 (fix round 1): scroll the list so the cursor stays visible —
    /// a `uniform_list` does not follow a cursor index on its own. Called
    /// from every path that moves `self.cursor` directly (this and the
    /// `top`/`bottom` dispatch arms; `rebuild`'s own tail covers the
    /// follow-mode and clamp cases).
    fn sync_scroll(&self) {
        self.scroll
            .scroll_to_item(self.cursor, ScrollStrategy::Nearest);
    }

    fn set_section(&mut self, section: Section, cx: &mut Context<Self>) {
        if self.section == section {
            return;
        }
        self.section = section;
        self.header_text = header_text_for(section);
        self.cursor = 0;
        self.rebuild(cx);
    }

    fn cycle_section(&mut self, forward: bool, cx: &mut Context<Self>) {
        let idx = Section::ALL
            .iter()
            .position(|s| *s == self.section)
            .unwrap_or(0);
        let len = Section::ALL.len();
        let next = if forward {
            (idx + 1) % len
        } else {
            (idx + len - 1) % len
        };
        self.set_section(Section::ALL[next], cx);
    }

    /// The dataset name of the nearest collapsible header row at or
    /// before the cursor (the "data" section's `zo`/`zc` target) —
    /// parsed off the header's own `"{name}: ..."` text rather than a
    /// second parallel index, since this tile owns the exact format
    /// `sections::data_rows` writes.
    fn nearest_header_name(&self) -> Option<String> {
        if self.rows.is_empty() {
            return None;
        }
        let start = self.cursor.min(self.rows.len() - 1);
        for i in (0..=start).rev() {
            let r = &self.rows[i];
            if r.depth == 0 && r.collapsible.is_some() {
                return r.text.split_once(": ").map(|(name, _)| name.to_string());
            }
        }
        None
    }

    fn set_collapsed_at_cursor(&mut self, collapse: bool, cx: &mut Context<Self>) {
        if self.section != Section::Data {
            return;
        }
        let Some(name) = self.nearest_header_name() else {
            return;
        };
        if collapse {
            self.collapsed.insert(name);
        } else {
            self.collapsed.remove(&name);
        }
        self.rebuild(cx);
    }

    pub fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(name) = action.0.strip_prefix("diagnostics::") else {
            return false;
        };
        let n = count.unwrap_or(1).max(1) as isize;
        match name {
            "down" => self.move_cursor(n, cx),
            "up" => self.move_cursor(-n, cx),
            "top" => {
                self.cursor = 0;
                self.follow = false;
                self.sync_scroll();
                cx.notify();
            }
            "bottom" => {
                self.cursor = self.rows.len().saturating_sub(1);
                self.follow = self.section == Section::Log;
                self.sync_scroll();
                cx.notify();
            }
            // MIN-4 (fix round 1): `n` (the pending count prefix) was
            // computed above and used by `down`/`up` only — `key_context`
            // advertises `.counts()`, so `3 ctrl+d` silently moved 5 rows
            // instead of 15.
            "page_down" => self.move_cursor(5 * n, cx),
            "page_up" => self.move_cursor(-5 * n, cx),
            // `ctrl+f`/`ctrl+b`: `vimnav`'s ±10 step, the same fixed
            // offset every dialog list uses, counted like `ctrl+d`.
            "page_down_full" => self.move_cursor(10 * n, cx),
            "page_up_full" => self.move_cursor(-10 * n, cx),
            "next_section" => self.cycle_section(true, cx),
            "prev_section" => self.cycle_section(false, cx),
            "expand" => self.set_collapsed_at_cursor(false, cx),
            "collapse" => self.set_collapsed_at_cursor(true, cx),
            _ => return false,
        }
        true
    }

    pub fn command(&mut self, line: &str, cx: &mut Context<Self>) -> Result<(), String> {
        match commands::parse(line)? {
            Command::Section(section) => self.set_section(section, cx),
            Command::Level { target, level } => {
                self.diagnostics.update(cx, |d, cx| {
                    d.request_level(&target, level);
                    cx.notify();
                });
            }
            Command::Overlay => {
                self.diagnostics.update(cx, |d, cx| {
                    d.request_overlay_toggle();
                    cx.notify();
                });
            }
        }
        Ok(())
    }

    pub fn completions(&self, line: &str, cursor: usize, _cx: &App) -> Vec<String> {
        commands::completions(line, cursor)
    }

    pub fn find(&mut self, event: FindEvent, cx: &mut Context<Self>) {
        match event {
            FindEvent::Changed(query) => {
                self.filter = query;
                self.rebuild(cx);
            }
            FindEvent::Committed(query) => {
                // MIN-10 (fix round 1): `Changed` already rebuilt with
                // this exact text in the ordinary case (the shell sends
                // `Changed` before `Committed`), so this is a no-op then
                // — but must not silently depend on that ordering holding
                // forever.
                self.filter = query;
                self.rebuild(cx);
            }
            FindEvent::Cancelled => {
                self.filter.clear();
                self.rebuild(cx);
            }
        }
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.diagnostics.update(cx, |d, cx| {
                d.watch();
                cx.notify();
            });
        } else {
            // MIN-5 (fix round 1): the `true` branch above notifies
            // (`Diagnostics::watch`'s own doc comment calls that
            // mandatory, for the bridge's drain); this branch used to
            // discard `cx` (`|d, _cx|`) and never notify at all — every
            // OTHER observer of this entity (not just the bridge) is
            // owed the same courtesy on any real mutation, `unwatch`
            // included.
            self.diagnostics.update(cx, |d, cx| {
                d.unwatch();
                cx.notify();
            });
        }
    }

    pub fn serialize(&self) -> toml::Table {
        let mut t = toml::Table::new();
        t.insert(
            "section".into(),
            toml::Value::String(self.section.name().to_string()),
        );
        if !self.filter.is_empty() {
            t.insert("filter".into(), toml::Value::String(self.filter.clone()));
        }
        t
    }
}

impl gpui::Render for DiagnosticsTile {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mut header = h_flex()
            .w_full()
            .h(px(22.))
            .items_center()
            .gap_2()
            .px_2()
            .text_sm()
            .font_family(fonts::MONO)
            .text_color(theme.muted_foreground)
            .border_b_1()
            .border_color(theme.border)
            .debug_selector(|| format!("diagnostics-header-{}", self.tile.0))
            .child(self.header_text.clone());
        // MIN-11 (fix round 1): a tile restored from a session with a
        // saved `filter` used to paint a narrowed list with no on-screen
        // indication why — same "filtered" pill the blotter's own header
        // shows for its own tile-scope filter.
        if !self.filter.is_empty() {
            header = header.child(
                div()
                    .text_color(theme.warning_foreground)
                    .bg(theme.warning.opacity(0.25))
                    .px_1()
                    .rounded(px(3.))
                    .debug_selector(|| format!("diagnostics-filtered-{}", self.tile.0))
                    .child("filtered"),
            );
        }

        let rows = self.rows.clone();
        let cursor = self.cursor;
        let selection_bg = theme.selection;
        let foreground = theme.foreground;
        let muted_foreground = theme.muted_foreground;
        let warning_foreground = theme.warning_foreground;
        let danger = theme.danger;
        let primary = theme.primary;
        let count = rows.len();
        let list = uniform_list("diagnostics-rows", count, move |range, _window, _cx| {
            range
                .map(|i| {
                    let r = &rows[i];
                    let color = match r.tone {
                        Tone::Normal => foreground,
                        Tone::Muted => muted_foreground,
                        Tone::Warn => warning_foreground,
                        Tone::Error => danger,
                        Tone::Marked => primary,
                    };
                    // One line per slot, clipped: a `uniform_list` row has a
                    // fixed height, so a row that wrapped would paint its
                    // second line over the slot beneath it (seen on a display
                    // 2026-09-08 with a long source path). The section
                    // builders keep rows short; this is the backstop.
                    let mut cell = div()
                        .w_full()
                        .pl(px(8.0 + r.depth as f32 * 12.0))
                        .font_family(fonts::MONO)
                        .text_color(color)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(r.text.clone());
                    if i == cursor {
                        cell = cell.bg(selection_bg);
                    }
                    cell.into_any_element()
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.scroll)
        .flex_1()
        .debug_selector(|| format!("diagnostics-rows-{}", self.tile.0));

        v_flex().size_full().child(header).child(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::groupings::GroupingSlots;
    use geode_core::log::{Level, LogLevels};
    use geode_core::query::AsOf;
    use geode_core::scope::Scope;
    use geode_core::scopes::SavedScopes;
    use geode_shell::diagnostics::{Diagnostics, Health};
    use geode_shell::frame::Frame;
    use gpui::{Entity, Window};

    struct Host {
        tile: Entity<DiagnosticsTile>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
        ring: Arc<Ring>,
    }
    impl gpui::Render for Host {
        fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.tile.clone())
        }
    }

    struct Harness {
        tile: Entity<DiagnosticsTile>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
        ring: Arc<Ring>,
    }

    fn open(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
        open_with(cx, None)
    }

    fn open_with(
        cx: &mut gpui::TestAppContext,
        restored: Option<&toml::Table>,
    ) -> (Harness, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        let ring = Arc::new(Ring::new(64));
        let config = Rc::new(RefCell::new(Config::default()));
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let frame =
                        cx.new(|_| Frame::new(GroupingSlots::default(), SavedScopes::new(), None));
                    let diagnostics = cx.new(|_| Diagnostics::new(LogLevels::default()));
                    let ring2 = ring.clone();
                    let config2 = config.clone();
                    cx.new(|cx| {
                        let tile = cx.new(|cx| {
                            DiagnosticsTile::new(
                                TileId(9),
                                frame.clone(),
                                diagnostics.clone(),
                                ring2.clone(),
                                config2.clone(),
                                restored,
                                window,
                                cx,
                            )
                        });
                        Host {
                            tile,
                            frame,
                            diagnostics,
                            ring: ring2,
                        }
                    })
                })
            })
            .unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let (tile, frame, diagnostics, ring) =
            window.root(&mut vcx).unwrap().read_with(&vcx, |h, _| {
                (
                    h.tile.clone(),
                    h.frame.clone(),
                    h.diagnostics.clone(),
                    h.ring.clone(),
                )
            });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (
            Harness {
                tile,
                frame,
                diagnostics,
                ring,
            },
            vcx,
        )
    }

    /// MIN-3 (final review): a tile constructed AFTER the ring already
    /// holds more records than its capacity, opened directly on the log
    /// section — the shape a session restore or a second tile hits, not
    /// covered by `open_with`'s "empty ring, then push, then switch"
    /// order. Duplicates `open_with`'s construction (rather than adding a
    /// parameter to it and touching all 22 existing call sites) with one
    /// difference: the ring is pre-populated before `DiagnosticsTile::new`
    /// ever runs.
    fn open_with_a_prepopulated_ring(
        cx: &mut gpui::TestAppContext,
        prepopulate: usize,
    ) -> (Harness, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        let ring = Arc::new(Ring::new(64));
        for i in 0..prepopulate {
            ring.push(Record {
                at: SystemTime::UNIX_EPOCH,
                level: Level::INFO,
                target: "geode::shell",
                message: format!("m{i}"),
                seq: 0,
            });
        }
        let mut section = toml::Table::new();
        section.insert("section".into(), toml::Value::String("log".into()));
        let config = Rc::new(RefCell::new(Config::default()));
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let frame =
                        cx.new(|_| Frame::new(GroupingSlots::default(), SavedScopes::new(), None));
                    let diagnostics = cx.new(|_| Diagnostics::new(LogLevels::default()));
                    let ring2 = ring.clone();
                    let config2 = config.clone();
                    cx.new(|cx| {
                        let tile = cx.new(|cx| {
                            DiagnosticsTile::new(
                                TileId(9),
                                frame.clone(),
                                diagnostics.clone(),
                                ring2.clone(),
                                config2.clone(),
                                Some(&section),
                                window,
                                cx,
                            )
                        });
                        Host {
                            tile,
                            frame,
                            diagnostics,
                            ring: ring2,
                        }
                    })
                })
            })
            .unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let (tile, frame, diagnostics, ring) =
            window.root(&mut vcx).unwrap().read_with(&vcx, |h, _| {
                (
                    h.tile.clone(),
                    h.frame.clone(),
                    h.diagnostics.clone(),
                    h.ring.clone(),
                )
            });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (
            Harness {
                tile,
                frame,
                diagnostics,
                ring,
            },
            vcx,
        )
    }

    /// Phase 4 §3.10, found by the market-data panel's review
    /// (2026-09-14) and fixed at both sites in the same round: this tile
    /// is a real occupant, so `ShellView::visible_tile_keys` puts its key
    /// in every flip barrier — and it never submits a query, so without
    /// this it holds every blotter on screen open until `FLIP_DEADLINE`
    /// (250 ms) on every scope keystroke. The shell's own frame observer
    /// is registered before any occupant's, so the mutation and
    /// `open_flip` really do land before this tile's observer runs —
    /// which is what one update block reproduces.
    #[gpui::test]
    fn the_tile_answers_a_flip_barrier_it_has_nothing_coming_for(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.frame.update(&mut vcx, |f, cx| {
            f.set_scope(Scope {
                text: Some("spx".into()),
                ..Default::default()
            });
            f.open_flip([QueryKey(9)], std::time::Instant::now());
            cx.notify();
        });
        assert!(
            !h.frame.read_with(&vcx, |f, _| f.barrier_open()),
            "a tile that will never answer must answer at once, or every \
             other following tile waits out the deadline"
        );
    }

    #[gpui::test]
    fn a_freshly_opened_tile_does_not_claim_records_it_never_had(cx: &mut gpui::TestAppContext) {
        let (h, _vcx) = open_with_a_prepopulated_ring(cx, 100);
        let joined = h.tile.read_with(&_vcx, |t, _| {
            t.rows()
                .iter()
                .map(|r| r.text.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        });
        assert!(
            !joined.contains("records lost"),
            "a newly opened tile never had these records — nothing was lost from ITS \
             perspective, even though the ring itself wrapped before it existed: {joined}"
        );
    }

    #[gpui::test]
    fn a_health_event_shows_in_the_sources_section(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.note_health(
                "risk",
                Health::Degraded {
                    reason: "reason".into(),
                },
                "reason".into(),
                SystemTime::UNIX_EPOCH,
            );
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let found = h.tile.read_with(&vcx, |t, _| {
            t.rows()
                .iter()
                .any(|r| r.text.contains("risk: degraded — reason"))
        });
        assert!(
            found,
            "expected a sources row naming risk's degraded reason"
        );
    }

    #[gpui::test]
    fn level_ingest_debug_changes_the_entity_and_queues_a_persist(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| {
            t.command("level ingest debug", cx).unwrap();
        });
        let levels = h.diagnostics.read_with(&vcx, |d, _| d.levels.clone());
        assert_eq!(levels.targets, vec![("ingest".to_string(), Level::DEBUG)]);
        let pending = h
            .diagnostics
            .update(&mut vcx, |d, _| d.take_pending_level());
        assert_eq!(pending, Some(("ingest".to_string(), Level::DEBUG)));
    }

    /// `Ring::oldest_seq`'s own contract: a reader whose `since` has
    /// fallen behind what the ring still retains has lost records — the
    /// log section reports how many rather than silently skipping the
    /// gap. `open`'s harness ring is 64 deep; 100 pushes before the tile
    /// ever switches to the log section (so it never got a chance to
    /// drain along the way) wraps past `since = 0`.
    #[gpui::test]
    fn the_log_section_reports_lost_records_when_the_ring_wrapped_past_since(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        for i in 0..100 {
            h.ring.push(Record {
                at: SystemTime::UNIX_EPOCH,
                level: Level::INFO,
                target: "geode::shell",
                message: format!("m{i}"),
                seq: 0,
            });
        }
        h.tile.update(&mut vcx, |t, cx| {
            t.command("section log", cx).unwrap();
        });
        let joined = h.tile.read_with(&vcx, |t, _| {
            t.rows()
                .iter()
                .map(|r| r.text.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        });
        assert!(joined.contains("records lost"), "{joined}");
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.rows()[0].tone),
            Tone::Warn,
            "the lost-records row leads, toned as a warning"
        );
    }

    #[gpui::test]
    fn the_log_section_follows_the_tail_until_the_cursor_moves(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| {
            t.command("section log", cx).unwrap();
        });
        for i in 0..3 {
            h.ring.push(Record {
                at: SystemTime::UNIX_EPOCH,
                level: Level::INFO,
                target: "geode::shell",
                message: format!("m{i}"),
                seq: 0,
            });
        }
        // Ring pushes carry no notify of their own (no gpui entity wraps
        // the ring); a real caller's next `Diagnostics`/`Frame` version
        // bump (the reload-poll tick's `refresh_frame_hist`, a health
        // note, ...) is what actually triggers the tile's next rebuild,
        // which then drains whatever the ring has accumulated since. A
        // `note_dropped` bump stands in for that here.
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.note_dropped(1);
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let (cursor, len) = h.tile.read_with(&vcx, |t, _| (t.cursor(), t.rows().len()));
        assert_eq!(
            cursor,
            len - 1,
            "following keeps the cursor on the last row"
        );
        assert!(h.tile.read_with(&vcx, |t, _| t.follow()));

        h.tile.update(&mut vcx, |t, cx| {
            t.dispatch(&ActionId("diagnostics::up".into()), None, cx);
        });
        let cursor_after_up = h.tile.read_with(&vcx, |t, _| t.cursor());
        assert_eq!(cursor_after_up, cursor - 1);
        assert!(!h.tile.read_with(&vcx, |t, _| t.follow()));

        h.ring.push(Record {
            at: SystemTime::UNIX_EPOCH,
            level: Level::INFO,
            target: "geode::shell",
            message: "m3".into(),
            seq: 0,
        });
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.note_dropped(2);
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let cursor_final = h.tile.read_with(&vcx, |t, _| t.cursor());
        assert_eq!(
            cursor_final, cursor_after_up,
            "cursor stays put once following was broken"
        );
        assert!(!h.tile.read_with(&vcx, |t, _| t.follow()));
    }

    #[gpui::test]
    fn the_perf_section_reads_the_frames_own_requery_stats(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.frame.update(&mut vcx, |f, cx| {
            f.requery.record_submit_to_snapshot(4_000);
            f.requery.record_snapshot_to_paint(1_000);
            cx.notify();
        });
        h.tile.update(&mut vcx, |t, cx| {
            t.command("section perf", cx).unwrap();
        });
        let joined = h.tile.read_with(&vcx, |t, _| {
            t.rows()
                .iter()
                .map(|r| r.text.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        });
        assert!(joined.contains("last requery"), "{joined}");
    }

    #[gpui::test]
    fn switching_sections_and_serialising_round_trips(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| {
            t.command("section perf", cx).unwrap();
            t.find(FindEvent::Changed("theme".into()), cx);
        });
        let state = h.tile.read_with(&vcx, |t, _| t.serialize());
        assert_eq!(state.get("section").and_then(|v| v.as_str()), Some("perf"));
        assert_eq!(state.get("filter").and_then(|v| v.as_str()), Some("theme"));

        let (h2, vcx2) = open_with(cx, Some(&state));
        let section = h2.tile.read_with(&vcx2, |t, _| t.section());
        assert_eq!(section, Section::Perf);
        let _ = vcx2;
    }

    #[gpui::test]
    fn an_unchanged_entity_does_not_rebuild_rows(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        let before = h.tile.read_with(&vcx, |t, _| t.rebuild_count());
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let after = h.tile.read_with(&vcx, |t, _| t.rebuild_count());
        assert_eq!(before, after, "no observed version change, no rebuild");
        assert_eq!(before, 1, "exactly the constructor's own initial rebuild");

        // A `cx.notify()` with no real state change (the frame histogram
        // refresh's own no-op path, `Diagnostics::refresh_frame_hist`'s
        // "identical histogram" case, ends up calling `cx.notify()` this
        // way in real usage) must not trigger a rebuild either — the
        // guard inside the `cx.observe` closures, not just the absence of
        // any notify at all, is what this test pins.
        h.diagnostics.update(&mut vcx, |_, cx| cx.notify());
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        h.frame.update(&mut vcx, |_, cx| cx.notify());
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let after_bare_notifies = h.tile.read_with(&vcx, |t, _| t.rebuild_count());
        assert_eq!(
            before, after_bare_notifies,
            "a notify with no version change must not rebuild either"
        );
    }

    /// MAJ-4 (final review): before per-population versions, ANY
    /// `Diagnostics` mutation — including the perf-only reload-tick copy
    /// of the frame histogram, which fires roughly every 500ms while a
    /// tile is visible — rebuilt whatever section happened to be showing.
    /// For the config section (the expensive one: a walk of every loaded
    /// doc's leaves) that meant a full rebuild twice a second regardless
    /// of what actually changed. Pinned here: a perf-only bump must leave
    /// the config section's `rebuild_count` untouched.
    #[gpui::test]
    fn refresh_frame_hist_does_not_rebuild_the_config_section(cx: &mut gpui::TestAppContext) {
        use geode_shell::perf::FrameHistogram;

        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| {
            t.set_visible(true, cx);
            t.command("section config", cx).unwrap();
        });
        let before = h.tile.read_with(&vcx, |t, _| t.rebuild_count());

        h.diagnostics.update(&mut vcx, |d, cx| {
            let mut hist = FrameHistogram::new();
            hist.record_micros(1_000);
            let bumped = d.refresh_frame_hist(&hist);
            assert!(bumped, "sanity: the histogram copy must have happened");
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let after = h.tile.read_with(&vcx, |t, _| t.rebuild_count());
        assert_eq!(
            before, after,
            "a perf-only version bump must not rebuild the config section"
        );
    }

    /// MAJ-4's other half: the config section MUST still rebuild on an
    /// actual config change — the fix narrows what wakes a rebuild, it
    /// must not silence real ones.
    #[gpui::test]
    fn note_config_rebuilds_the_config_section(cx: &mut gpui::TestAppContext) {
        use geode_core::config::{Diagnostic, Layer};
        use std::path::PathBuf;

        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| {
            t.command("section config", cx).unwrap();
        });
        let before = h.tile.read_with(&vcx, |t, _| t.rebuild_count());

        h.diagnostics.update(&mut vcx, |d, cx| {
            d.note_config(
                vec![Diagnostic::error(Layer::User, PathBuf::new(), "bad")],
                SystemTime::UNIX_EPOCH,
            );
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let after = h.tile.read_with(&vcx, |t, _| t.rebuild_count());
        assert!(
            after > before,
            "a real config change must rebuild the config section"
        );
    }

    #[gpui::test]
    fn visibility_watches_and_requests_a_catalog(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| {
            t.set_visible(true, cx);
        });
        let watchers = h.diagnostics.read_with(&vcx, |d, _| d.watchers());
        assert_eq!(watchers, 1);
        let pending = h
            .diagnostics
            .update(&mut vcx, |d, _| d.take_pending_catalog_request());
        assert!(pending);
    }

    /// MIN-5 (fix round 1): `visibility_watches_and_requests_a_catalog`
    /// only ever covered `set_visible(true)` — nothing pinned the `false`
    /// side, `Diagnostics::watch`'s own doc comment's mandatory
    /// caller-must-notify contract on the way out, or `unwatch` actually
    /// running at all (exactly what MAJ-2's `occupants.rs` fix depends
    /// on downstream).
    #[gpui::test]
    fn set_visible_false_unwatches_and_notifies(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| {
            t.set_visible(true, cx);
        });
        assert_eq!(h.diagnostics.read_with(&vcx, |d, _| d.watchers()), 1);

        // A raw observer of our own, independent of the tile's own
        // `cx.observe` — proves `set_visible(false)` itself notifies the
        // entity (`Diagnostics::watch`'s own doc comment calls this
        // mandatory; `unwatch` bumps no version, so nothing else here
        // would wake a version-gated observer).
        let notified = Rc::new(std::cell::Cell::new(false));
        let notified_for_observer = notified.clone();
        let diagnostics = h.diagnostics.clone();
        vcx.update(|_, cx| {
            cx.observe(&diagnostics, move |_, _| {
                notified_for_observer.set(true);
            })
            .detach();
        });

        h.tile.update(&mut vcx, |t, cx| {
            t.set_visible(false, cx);
        });
        vcx.run_until_parked();
        assert_eq!(
            h.diagnostics.read_with(&vcx, |d, _| d.watchers()),
            0,
            "set_visible(false) must unwatch"
        );
        assert!(
            notified.get(),
            "set_visible(false) must notify the entity itself"
        );
    }

    // --- Fix round 1 -----------------------------------------------------

    /// MAJ-1: `scroll_to_item` is never called anywhere in the original
    /// implementation — a `uniform_list` does not follow a cursor index
    /// on its own, so the log section's "follows the tail" behaviour (and
    /// every `j`/`ctrl+d`/`G` press) was invisible on screen. `G`
    /// (`diagnostics::bottom`) on a 200-row section must scroll the list
    /// to the last row.
    #[gpui::test]
    fn pressing_bottom_scrolls_the_list_to_the_last_row(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| {
            for i in 0..200 {
                d.note_health(
                    &format!("src{i}"),
                    Health::Ok,
                    "".into(),
                    SystemTime::UNIX_EPOCH,
                );
            }
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let row_count = h.tile.read_with(&vcx, |t, _| t.rows().len());
        assert_eq!(row_count, 200, "sanity: 200 sources make 200 rows");

        // Read the scroll target inside the same update as the dispatch,
        // before any effects flush: the deferred `scroll_to_item` this
        // pins is consumed by the list's own prepaint once a real draw
        // runs, at which point `logical_scroll_top_index` falls back to
        // whatever the (headless, effectively unsized) test window's
        // actual scroll offset resolved to — not what this test is
        // about, which is that `scroll_to_item` was queued for the right
        // row at all.
        let target = h.tile.update(&mut vcx, |t, cx| {
            t.dispatch(&ActionId("diagnostics::bottom".into()), None, cx);
            t.scroll_target()
        });
        assert_eq!(target, row_count - 1, "G must scroll to the last row");
    }

    /// MAJ-4 (partial coverage — see caveat below): `self.rows` itself
    /// must not be reallocated by anything except a real `rebuild` —
    /// pinned here via `Rc::ptr_eq` across two paints with no rebuild
    /// between them (a full `Vec` clone stored back into `self.rows`
    /// would still compare equal by content, which is why this needs
    /// pointer identity, not `rows()`'s slice).
    ///
    /// **What this does NOT prove**: that `render`'s own `let rows =
    /// self.rows.clone();` clones the `Rc` (cheap) rather than the `Vec`
    /// behind it (`Rc::new((*self.rows).clone())`, a full reallocation +
    /// per-row `SharedString` bump) — `render`'s local `rows` binding is
    /// captured by the `uniform_list` closure and lives only for the
    /// duration of one `window.draw()`, with no hook this test harness
    /// can observe from outside that call to tell the two apart (both
    /// produce a type-identical `Rc<Vec<Row>>`; the difference is only in
    /// whether `self.rows`'s own refcount rises during the draw, which
    /// nothing here can inspect mid-call). Verified by reading instead:
    /// `render` at the marked line clones `self.rows` directly with no
    /// intervening `(*...).clone()`.
    #[gpui::test]
    fn rebuilding_does_not_reallocate_rows_between_paints(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        let before_count = h.tile.read_with(&vcx, |t, _| t.rebuild_count());
        let a = h.tile.read_with(&vcx, |t, _| t.rows_rc());
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let b = h.tile.read_with(&vcx, |t, _| t.rows_rc());
        assert!(
            Rc::ptr_eq(&a, &b),
            "no rebuild between paints must not reallocate the row Vec"
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.rebuild_count()),
            before_count,
            "sanity: genuinely no rebuild happened"
        );
    }

    /// MIN-1 (final review): the header text used to be a fresh
    /// `format!()` on every single `render` call — every shell repaint,
    /// not just this tile's own rebuilds. It is now a cached
    /// `SharedString`, replaced only in `set_section`, so two paints with
    /// no section change share the same backing allocation.
    #[gpui::test]
    fn the_header_text_is_cached_across_paints_and_replaced_on_section_change(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        let a = h.tile.read_with(&vcx, |t, _| t.header_text());
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let b = h.tile.read_with(&vcx, |t, _| t.header_text());
        assert_eq!(
            a.as_str().as_ptr(),
            b.as_str().as_ptr(),
            "no section change between paints must not reallocate the header text"
        );
        assert!(a.contains("sources"), "{a}");

        h.tile.update(&mut vcx, |t, cx| {
            t.command("section perf", cx).unwrap();
        });
        let c = h.tile.read_with(&vcx, |t, _| t.header_text());
        assert!(
            c.contains("perf"),
            "the header must follow a real section change: {c}"
        );
    }

    /// MAJ-5: a rebuild that finds nothing new in the ring (`latest_seq()
    /// <= since`) must not touch `drain_buf` at all — its capacity must
    /// be exactly as stable across a no-op drain as `Ring::drain_since`'s
    /// own "a hit allocates nothing" contract promises the ring side of
    /// this exchange.
    #[gpui::test]
    fn a_no_op_log_drain_does_not_grow_the_drain_buffer(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| {
            t.command("section log", cx).unwrap();
        });
        h.ring.push(Record {
            at: SystemTime::UNIX_EPOCH,
            level: Level::INFO,
            target: "geode::shell",
            message: "m".into(),
            seq: 0,
        });
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.note_dropped(1);
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let cap = h.tile.read_with(&vcx, |t, _| t.drain_buf_capacity());

        // A further rebuild with nothing new in the ring — the no-op path.
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.note_dropped(2);
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let cap_after = h.tile.read_with(&vcx, |t, _| t.drain_buf_capacity());
        assert_eq!(
            cap, cap_after,
            "a no-op drain must not grow or shrink drain_buf's capacity"
        );

        // The other half of MAJ-5's claim — reuse, not just "a no-op
        // costs nothing" (which held even before the fix, since the old
        // `let mut drained = Vec::new()` sat *inside* the `if latest >
        // since` branch too): a big real drain (forcing `drain_buf` to
        // grow well past a single-record capacity) followed by a tiny
        // real drain must not show the tiny drain collapsing the
        // capacity back down — that only happens if the tiny drain
        // allocated its OWN fresh `Vec` instead of reusing the grown one.
        // A single-record-at-a-time version of this assertion is too
        // weak: `Vec`'s own growth policy can size a fresh one-record
        // `Vec` identically to a reused one that only ever held one
        // record, so the two cases would coincidentally read the same
        // capacity either way.
        for i in 0..64 {
            h.ring.push(Record {
                at: SystemTime::UNIX_EPOCH,
                level: Level::INFO,
                target: "geode::shell",
                message: format!("big{i}"),
                seq: 0,
            });
        }
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.note_dropped(3);
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let cap_after_big_drain = h.tile.read_with(&vcx, |t, _| t.drain_buf_capacity());
        assert!(
            cap_after_big_drain >= 64,
            "a 64-record drain must grow drain_buf's capacity to at least 64, got {cap_after_big_drain}"
        );

        h.ring.push(Record {
            at: SystemTime::UNIX_EPOCH,
            level: Level::INFO,
            target: "geode::shell",
            message: "tiny".into(),
            seq: 0,
        });
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.note_dropped(4);
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let cap_after_tiny_drain = h.tile.read_with(&vcx, |t, _| t.drain_buf_capacity());
        assert!(
            cap_after_tiny_drain >= cap_after_big_drain,
            "a tiny drain right after a big one must reuse drain_buf's grown \
             capacity, not replace it with a fresh, small Vec \
             (after big drain: {cap_after_big_drain}, after tiny: {cap_after_tiny_drain})"
        );
    }

    /// MAJ-6: `frame_versions_relevant_eq` narrowed to `as_of` + `config`
    /// only — a scope-only frame change (what every keystroke in the
    /// toolbar's text field bumps) must not rebuild this tile, and a
    /// config reload (`Frame::note_config_reloaded`) must.
    #[gpui::test]
    fn a_scope_only_frame_change_does_not_rebuild_but_a_config_reload_does(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        // MAJ-4 (final review): the frame observer's rebuild gate is now
        // per-section too (not just the diagnostics entity's), so this
        // test must actually be showing the config section for a config
        // reload to be expected to rebuild it.
        h.tile.update(&mut vcx, |t, cx| {
            t.command("section config", cx).unwrap();
        });
        let before = h.tile.read_with(&vcx, |t, _| t.rebuild_count());

        h.frame.update(&mut vcx, |f, cx| {
            f.set_scope(Scope {
                text: Some("x".into()),
                ..Scope::default()
            });
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.rebuild_count()),
            before,
            "a scope-only change must not rebuild — no section reads it"
        );

        h.frame.update(&mut vcx, |f, cx| {
            f.note_config_reloaded();
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            h.tile.read_with(&vcx, |t, _| t.rebuild_count()) > before,
            "a config reload must rebuild — the config section reads it"
        );
    }

    /// MAJ-4's frame-observer narrowing covers every section, not just
    /// `Data`/`Config` — the successor of the retired "MAJ-6:
    /// frame_versions_relevant_eq widens back to include scope" mutation
    /// entry (final review round 2, NEW-3): that entry's own test
    /// (above) only ever exercises the `Config` section: this pins the
    /// `Section::Sources | Section::Log | Section::Perf => false` arm
    /// directly, on the tile's default section.
    #[gpui::test]
    fn a_config_reload_while_showing_sources_does_not_rebuild(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.section()), Section::Sources);
        let before = h.tile.read_with(&vcx, |t, _| t.rebuild_count());

        h.frame.update(&mut vcx, |f, cx| {
            f.note_config_reloaded();
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.rebuild_count()),
            before,
            "a config reload must not rebuild a tile showing sources — no \
             section but Config reads the frame's config version"
        );
    }

    /// MAJ-7: an as-of change while the tile is visible must queue a
    /// fresh catalog request, so the data section's resolved-generation
    /// marker gets a chance to refresh under the new as-of rather than
    /// keep asserting the old one.
    #[gpui::test]
    fn an_as_of_change_while_visible_requests_a_fresh_catalog(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| {
            t.set_visible(true, cx);
        });
        // Drain `watch()`'s own first request so only the as-of-driven
        // one is left to observe.
        h.diagnostics
            .update(&mut vcx, |d, _| d.take_pending_catalog_request());

        h.frame.update(&mut vcx, |f, cx| {
            f.set_as_of(AsOf::At(chrono::Utc::now()));
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let pending = h
            .diagnostics
            .update(&mut vcx, |d, _| d.take_pending_catalog_request());
        assert!(
            pending,
            "an as-of change while visible must request a fresh catalog"
        );
    }

    /// MAJ-7's other half: an as-of change while the tile is NOT visible
    /// must not spend a database round trip nothing will show.
    #[gpui::test]
    fn an_as_of_change_while_invisible_does_not_request_a_catalog(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.frame.update(&mut vcx, |f, cx| {
            f.set_as_of(AsOf::At(chrono::Utc::now()));
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let pending = h
            .diagnostics
            .update(&mut vcx, |d, _| d.take_pending_catalog_request());
        assert!(
            !pending,
            "an as-of change while invisible must not request a catalog"
        );
    }

    /// MIN-4: a count prefix must reach `ctrl+d`/`ctrl+u`
    /// (`page_down`/`page_up`), not just `j`/`k` — `3 ctrl+d` must move
    /// 15 rows (5 * 3), not 5.
    #[gpui::test]
    fn a_count_prefix_multiplies_page_down(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| {
            for i in 0..30 {
                d.note_health(
                    &format!("src{i}"),
                    Health::Ok,
                    "".into(),
                    SystemTime::UNIX_EPOCH,
                );
            }
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        h.tile.update(&mut vcx, |t, cx| {
            t.dispatch(&ActionId("diagnostics::page_down".into()), Some(3), cx);
        });
        let cursor = h.tile.read_with(&vcx, |t, _| t.cursor());
        assert_eq!(cursor, 15, "3 ctrl+d must move 5 * 3 = 15 rows");
    }

    /// `ctrl+f`/`ctrl+b` (`page_down_full`/`page_up_full`) are the
    /// ±10 step every dialog list already has (`vimnav`'s convention),
    /// counted the same way `ctrl+d` is: `2 ctrl+f` moves 20, `ctrl+b`
    /// brings back 10.
    #[gpui::test]
    fn ctrl_f_and_ctrl_b_page_by_ten(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| {
            for i in 0..30 {
                d.note_health(
                    &format!("src{i}"),
                    Health::Ok,
                    "".into(),
                    SystemTime::UNIX_EPOCH,
                );
            }
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        h.tile.update(&mut vcx, |t, cx| {
            t.dispatch(&ActionId("diagnostics::page_down_full".into()), Some(2), cx);
        });
        let cursor = h.tile.read_with(&vcx, |t, _| t.cursor());
        assert_eq!(cursor, 20, "2 ctrl+f must move 10 * 2 = 20 rows");
        h.tile.update(&mut vcx, |t, cx| {
            t.dispatch(&ActionId("diagnostics::page_up_full".into()), None, cx);
        });
        let cursor = h.tile.read_with(&vcx, |t, _| t.cursor());
        assert_eq!(cursor, 10, "ctrl+b must move back 10 rows");
    }

    /// MIN-11: a tile carrying a filter shows a "filtered" indicator in
    /// its header — same pill the blotter's own header shows for its
    /// tile-scope filter (`blotter-filtered-<tile>`), so a session-
    /// restored filtered tile is not silently narrowed with no on-screen
    /// explanation.
    #[gpui::test]
    fn a_filtered_tile_shows_the_filtered_pill(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        assert!(vcx.debug_bounds("diagnostics-filtered-9").is_none());
        h.tile.update(&mut vcx, |t, cx| {
            t.find(FindEvent::Changed("theme".into()), cx);
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(vcx.debug_bounds("diagnostics-filtered-9").is_some());
    }
}
