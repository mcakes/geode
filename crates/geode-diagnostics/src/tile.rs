//! Diagnostics tile over the shell-owned `Diagnostics` entity and frame.
//! Observers rebuild the selected section when its inputs change; rendering
//! uses the prepared rows in a monospace `uniform_list`.

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
use geode_shell::frame::{FrameRef, FrameVersions};
use geode_shell::keymap::KeyContext;
use geode_shell::module::{FindEvent, StackHandle};
use geode_shell::shell::chip;
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use gpui::prelude::*;
use gpui::{
    App, Context, Entity, IntoElement, ScrollStrategy, SharedString, UniformListScrollHandle,
    Window, div, uniform_list,
};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use crate::commands::{self, Command, Section};
use crate::sections::{self, Row, Tone};

/// Maximum records retained in this tile's log tail, independent of the
/// shared ring's capacity. Drop the oldest records when full.
const LOG_CAP: usize = 4_096;
/// Header strip height, in pixels at the design rem
/// (`geode_shell::shell::scale`) — the blotter's own.
const HEADER_HEIGHT: f32 = 22.0;

/// The diagnostics version read by each section's row builder. Comparing
/// only this counter prevents unrelated changes, such as a perf tick, from
/// rebuilding expensive config rows. The log observer also compares the
/// ring's sequence because the ring lives outside `Diagnostics`.
fn diag_version_for_section(section: Section, v: geode_shell::diagnostics::DiagVersions) -> u64 {
    match section {
        Section::Sources => v.sources,
        Section::Data => v.data,
        Section::Config => v.config,
        Section::Log => v.log_levels,
        Section::Perf => v.perf,
    }
}

/// Header text computed once per section change, keeping formatting out of paint.
/// Backtick-quoted runs are keys, painted as chips by `kbd::marked`.
fn header_text_for(section: Section) -> SharedString {
    format!("diagnostics · {} · `[` `]` to switch", section.name()).into()
}

/// Tile title computed once per section change and shared with the tile list
/// and stack marker.
fn title_text_for(section: Section) -> SharedString {
    format!("diagnostics · {}", section.name()).into()
}

pub struct DiagnosticsTile {
    tile: TileId,
    frame: FrameRef,
    diagnostics: Entity<geode_shell::diagnostics::Diagnostics>,
    ring: Arc<Ring>,
    config: Rc<RefCell<Config>>,
    section: Section,
    cursor: usize,
    collapsed: BTreeSet<String>,
    filter: String,
    /// Keep the log cursor on the last row until the user moves it.
    follow: bool,
    /// The ring sequence this tile has drained up to.
    since: u64,
    /// Records missed by this tile when the shared ring wrapped, measured at
    /// the last drain with new records. An empty drain keeps that reading; a
    /// subsequent drain replaces it rather than accumulating gaps. Nonzero
    /// values appear as a leading warning row. Starting `since` at the current
    /// ring sequence excludes records overwritten before this tile opened.
    lost_records: u64,
    /// Scratch storage reused for every ring drain. Moving its records into
    /// `records` leaves it empty while retaining capacity for the next drain.
    drain_buf: Vec<Record>,
    records: VecDeque<Record>,
    /// Prepared rows shared with the list closure by cloning the `Rc`. This
    /// keeps each paint independent of row count; cloning the vector would
    /// allocate and copy every row. Only `rebuild` replaces the allocation.
    rows: Rc<Vec<Row>>,
    /// Last observed section counters. The observer compares only the inputs
    /// selected by [`diag_version_for_section`].
    last_diag_versions: geode_shell::diagnostics::DiagVersions,
    last_frame_versions: FrameVersions,
    /// Header cache, replaced only in `set_section` so painting formats nothing.
    header_text: SharedString,
    /// [`Self::title`]'s cache, same reasoning as `header_text` beside
    /// it — replaced only in `set_section`, never `format!`-ed in
    /// `title()` itself.
    title: SharedString,
    visible: bool,
    scroll: UniformListScrollHandle,
    /// This tile's stack position, painted in the header; `None` outside a stack.
    stack: Option<StackHandle>,
    #[cfg(test)]
    rebuild_count: u32,
}

impl DiagnosticsTile {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tile: TileId,
        frame: FrameRef,
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
        // Start at the current sequence so the tile cannot report records
        // overwritten before it opened as its own loss.
        let initial_since = ring.latest_seq();

        cx.observe(&diagnostics, |this, diagnostics, cx| {
            let now = diagnostics.read(cx).versions();
            // The ring has its own sequence. Check it here so an unrelated
            // diagnostics notification with no new records skips the rebuild.
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
        // Timestamps are formatted during rebuild, so a clock-setting change
        // must rebuild the selected section.
        cx.observe_global::<geode_shell::clock::AppClock>(|this, cx| this.rebuild(cx))
            .detach();
        // The app registers its config-refresh frame observer before tiles
        // are created. That observer must update the shared `Config` before
        // this observer rebuilds config rows on the same version change.
        cx.observe(frame.entity(), |this, _, cx| {
            // Read through the tile's own handle: the observed entity alone
            // would answer for the shared lane, not this workspace's.
            let frame = this.frame.clone();
            let now = frame.read(cx).versions();
            let as_of_changed = now.as_of != this.last_frame_versions.as_of;
            let config_changed = now.config != this.last_frame_versions.config;
            // Only data rows read frame as-of; only config rows read the loaded
            // config. Scope/grouping keystrokes must not rebuild these unrelated
            // lists. Publications arrive through diagnostics versions, and this
            // tile has no staged snapshot to promote on a flip.
            let relevant = match this.section {
                Section::Data => as_of_changed,
                Section::Config => config_changed,
                Section::Sources | Section::Log | Section::Perf => false,
            };
            this.last_frame_versions = now;
            if relevant {
                this.rebuild(cx);
            }
            // The catalog resolves generation markers under the request's as-of.
            // A visible tile needs a fresh snapshot when it changes; hidden tiles
            // request one when they become visible.
            if as_of_changed && this.visible {
                this.diagnostics.update(cx, |d, cx| {
                    d.request_catalog_refresh();
                    cx.notify();
                });
            }
            // Visible diagnostics tiles participate in the flip barrier but
            // submit no view query whose result could signal arrival. Arrive
            // here so other tiles do not wait for the 250 ms deadline.
            // `barrier_wants` excludes tiles outside the active barrier.
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
            title: title_text_for(section),
            visible: false,
            scroll: UniformListScrollHandle::new(),
            stack: None,
            #[cfg(test)]
            rebuild_count: 0,
        };
        this.rebuild(cx);
        this
    }

    /// Rebuild the selected section after an input change, then synchronize the
    /// cursor and scroll position. Called at construction, by observers, and
    /// by local section, filter, and collapse changes.
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
                // Reuse the scratch buffer so repeated drains retain capacity.
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
        // Module test fixtures may omit `AppClock`; use the machine clock then.
        let clock = cx
            .try_global::<geode_shell::clock::AppClock>()
            .map(|c| c.0)
            .unwrap_or_else(|| geode_core::clock::Clock::machine().0);
        let new_rows: Vec<Row> = {
            let d = self.diagnostics.read(cx);
            let frame = self.frame.read(cx);
            match self.section {
                Section::Sources => sections::sources_rows(d, now, clock),
                Section::Data => sections::data_rows(d, frame.as_of(), &self.collapsed, clock),
                Section::Config => {
                    sections::config_rows(d, &self.config.borrow(), &self.filter, clock)
                }
                Section::Log => {
                    // Borrow the retained tail as a slice, avoiding a clone of up to
                    // `LOG_CAP` records and their strings on each rebuild.
                    let records = self.records.make_contiguous();
                    let mut rows = sections::log_rows(records, &self.filter, clock);
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
        // Replace the row allocation only on rebuild. Paint shares this `Rc`.
        self.rows = Rc::new(new_rows);
        if self.section == Section::Log && self.follow {
            self.cursor = self.rows.len().saturating_sub(1);
        } else {
            self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        }
        // The list does not track the cursor itself. Follow-mode appends and
        // clamping can move it; non-strict scrolling keeps it visible and is
        // a no-op when it is already in view.
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

    /// Expose row allocation identity to tests. Equal contents alone would not
    /// detect an unnecessary vector copy between paints.
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

    /// Expose the shared header cache so tests can compare its backing
    /// allocation across paints.
    #[cfg(test)]
    pub(crate) fn header_text(&self) -> SharedString {
        self.header_text.clone()
    }

    /// Read the top visible index or the pending deferred scroll target.
    /// Tests inspect it before paint consumes the queued scroll request.
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

    /// Keep the cursor visible: `uniform_list` does not follow it automatically.
    /// Every cursor mutation, including follow-mode rebuilds, calls this.
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
        self.title = title_text_for(section);
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
            // Apply count prefixes to page movement too: `3 ctrl+d` moves 15 rows.
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
            Command::Refused(message) => return Err(message.to_string()),
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
                // Accept the committed text even if no preceding `Changed` event
                // delivered it; the commit must be self-contained.
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
            // Notify after unwatching so the bridge and other observers see the
            // visibility change, just as they do when watching begins.
            self.diagnostics.update(cx, |d, cx| {
                d.unwatch();
                cx.notify();
            });
        }
    }

    pub fn set_stack(&mut self, stack: Option<StackHandle>, cx: &mut Context<Self>) {
        self.stack = stack;
        cx.notify();
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
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
        // `geode_shell::shell::chip` decides both the `filtered` pill and a
        // warning row's text: `warning_foreground` is the background family
        // at the pinned rev, unreadable on a tint (30 of 44 bundled themes)
        // and worse on the bare surface, and `theme.warning` itself is
        // under 3:1 against `background` on ten, so the door floors it.
        let neutral_chip = chip::chip_paint(theme, chip::Tone::Neutral);
        let warn_text = chip::chip_paint(theme, chip::Tone::WarningText).text;
        let danger_text = chip::chip_paint(theme, chip::Tone::DangerText).text;
        let mut header = h_flex()
            .w_full()
            .h(scale::design(HEADER_HEIGHT))
            .items_center()
            .gap_2()
            .px_2()
            .text_sm()
            .font_family(fonts::MONO)
            .text_color(theme.muted_foreground)
            .border_b_1()
            .border_color(theme.border)
            .debug_selector(|| format!("diagnostics-header-{}", self.tile.0));
        // Paint the shared stack marker first in the header.
        header = header
            .children(self.stack.as_ref().and_then(|s| s.marker(theme, self.tile)))
            .child(geode_shell::shell::kbd::marked(&self.header_text));
        // Expose the active filter, including one restored from a session.
        if !self.filter.is_empty() {
            header = header.child(
                div()
                    .text_color(neutral_chip.text)
                    .when_some(neutral_chip.fill, |el, fill| el.bg(fill))
                    .px_1()
                    .rounded(theme.radius_tokens().sm)
                    .debug_selector(|| format!("diagnostics-filtered-{}", self.tile.0))
                    .child("filtered"),
            );
        }

        let rows = self.rows.clone();
        let cursor = self.cursor;
        let selection_bg = theme.selection;
        let foreground = theme.foreground;
        let muted_foreground = theme.muted_foreground;
        let primary = theme.primary;
        let count = rows.len();
        let list = uniform_list("diagnostics-rows", count, move |range, _window, _cx| {
            range
                .map(|i| {
                    let r = &rows[i];
                    let color = match r.tone {
                        Tone::Normal => foreground,
                        Tone::Muted => muted_foreground,
                        Tone::Warn => warn_text,
                        Tone::Error => danger_text,
                        Tone::Marked => primary,
                    };
                    // Clip each row to one line: wrapping inside a fixed-height list
                    // slot would paint over the next row.
                    let mut cell = div()
                        .w_full()
                        .pl(scale::design(8.0 + r.depth as f32 * 12.0))
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
    use geode_shell::tiling::WorkspaceIx;
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
                                FrameRef::new(frame.clone(), WorkspaceIx::FIRST),
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

    /// Construct a tile directly on the log section after the ring has wrapped.
    /// This models session restoration and opening a second tile: records
    /// overwritten before construction must not count as that tile's loss.
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
                                FrameRef::new(frame.clone(), WorkspaceIx::FIRST),
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

    /// A visible diagnostics tile must arrive at the flip barrier without
    /// waiting for a query outcome. The shell registers its frame observer
    /// first, so it opens the barrier before the tile observer runs. One
    /// update block reproduces that order here.
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

    /// Every accepted command and refusal must leave application log levels,
    /// the overlay, and frame state unchanged. Compare frame counters to catch
    /// writes such as saving a slot even when the active value stays the same.
    #[gpui::test]
    fn every_colon_command_leaves_the_app_alone(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        let lines = [
            "section log",
            "section perf",
            "level ingest debug",
            "overlay",
        ];
        for word in commands::COMMANDS {
            assert!(
                lines
                    .iter()
                    .any(|l| l.split_whitespace().next() == Some(word)),
                "no sweep line for `:{word}`"
            );
        }
        let before = h.frame.read_with(&vcx, |f, _| f.versions());
        for line in lines {
            let result = h.tile.update(&mut vcx, |t, cx| t.command(line, cx));
            if line.starts_with("level") {
                assert_eq!(result, Err(commands::REFUSED_LEVEL.to_string()));
            } else if line == "overlay" {
                assert_eq!(result, Err(commands::REFUSED_OVERLAY.to_string()));
            } else {
                result.unwrap();
            }
            let (level, overlay) = h.diagnostics.update(&mut vcx, |d, _| {
                (d.take_pending_level(), d.take_pending_overlay_toggle())
            });
            assert!(level.is_none(), "`:{line}` queued a log-level change");
            assert!(!overlay, "`:{line}` queued an overlay toggle");
            let after = h.frame.read_with(&vcx, |f, _| f.versions());
            assert_eq!(
                (after.scope, after.grouping, after.as_of),
                (before.scope, before.grouping, before.as_of),
                "`:{line}` moved the frame"
            );
        }
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

    /// Log timestamps are formatted into `Row.text` during rebuild. Changing
    /// `AppClock` must trigger the observer and replace the cached text.
    /// The expected Tokyo and UTC times are literal values, independent of
    /// the clock implementation being tested.
    #[gpui::test]
    fn the_log_section_reads_the_installed_app_clock_and_follows_a_later_change(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            cx.set_global(geode_shell::clock::AppClock(
                geode_core::clock::Clock::in_zone_named("Asia/Tokyo"),
            ))
        });
        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| {
            t.command("section log", cx).unwrap();
        });
        let at: SystemTime = chrono::DateTime::parse_from_rfc3339("2026-09-12T14:00:00Z")
            .unwrap()
            .to_utc()
            .into();
        h.ring.push(Record {
            at,
            level: Level::INFO,
            target: "geode::shell",
            message: "m".into(),
            seq: 0,
        });
        // Ring pushes carry no notify of their own; `note_dropped` +
        // `cx.notify()` is the existing tests' stand-in for the real
        // caller (a health note, the reload-poll tick, …) that actually
        // triggers the next rebuild — same trick as
        // `the_log_section_follows_the_tail_until_the_cursor_moves`.
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.note_dropped(1);
            cx.notify();
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let before = h
            .tile
            .read_with(&vcx, |t, _| t.rows().last().unwrap().text.to_string());
        assert!(
            before.starts_with("23:00:00"),
            "Tokyo is UTC+9 on the 14:00:00Z fixture: {before}"
        );

        vcx.update(|_window, cx| {
            cx.set_global(geode_shell::clock::AppClock(geode_core::clock::Clock::utc()))
        });
        vcx.run_until_parked();
        let after = h
            .tile
            .read_with(&vcx, |t, _| t.rows().last().unwrap().text.to_string());
        assert!(
            after.starts_with("14:00:00"),
            "the observer refreshed and repainted: {after}"
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

    /// A perf-only version change must not rebuild the config section, whose
    /// effective-config rows require walking the loaded documents.
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

    /// A config change must still rebuild the config section.
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

    /// Hiding a tile must remove its watch and notify diagnostics observers.
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

    #[gpui::test]
    fn title_names_the_section(cx: &mut gpui::TestAppContext) {
        let (h, vcx) = open(cx);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.title()).as_ref(),
            "diagnostics · sources"
        );
    }

    /// The cached title must follow section changes.
    #[gpui::test]
    fn title_follows_a_section_change(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.tile.update(&mut vcx, |t, cx| {
            t.command("section perf", cx).unwrap();
        });
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.title()).as_ref(),
            "diagnostics · perf"
        );
    }

    /// The stack marker paints first in the header only for a stack with
    /// more than one member.
    #[gpui::test]
    fn the_stack_marker_paints_only_while_a_member(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        assert!(vcx.debug_bounds("stack-marker-9").is_none());

        h.tile.update(&mut vcx, |t, cx| {
            t.set_stack(Some(StackHandle::new(2, 4, |_, _| {})), cx);
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let marker = vcx.debug_bounds("stack-marker-9").expect("painted");
        let header = vcx.debug_bounds("diagnostics-header-9").unwrap();
        assert!(
            marker.left() - header.left() < gpui::px(20.0),
            "first in the strip"
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.title()).as_ref(),
            "diagnostics · sources"
        );
    }

    // Cursor visibility and allocation stability.

    /// `uniform_list` does not follow a cursor index automatically. Moving to
    /// the bottom of a 200-row section must queue a scroll to the last row.
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

    /// Two paints without a rebuild must preserve `self.rows` allocation
    /// identity. Content equality would also pass after an unnecessary copy.
    ///
    /// This test cannot detect a temporary vector copy inside the list closure:
    /// that local allocation exists only during `window.draw()`. Review the
    /// render path separately to ensure it clones the `Rc` itself.
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

    /// Two paints without a section change must share the cached header
    /// allocation. Only `set_section` replaces it.
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

    /// A rebuild with no new ring records must preserve the drain buffer's
    /// capacity, as must subsequent drains that fit within that capacity.
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

        // A large drain followed by a small drain proves capacity reuse.
        // Single-record drains alone cannot distinguish reuse from a fresh
        // vector: the growth policy can give both the same capacity.
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

    /// A scope change must not rebuild config rows; a config reload must.
    #[gpui::test]
    fn a_scope_only_frame_change_does_not_rebuild_but_a_config_reload_does(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        // Show the config section so a config reload exercises its rebuild gate.
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

    /// The sources section ignores both frame config and as-of changes.
    /// This exercises the frame observer's no-rebuild arm independently of
    /// the data and config sections.
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

    /// An as-of change while visible must request a fresh catalog so the data
    /// section can display the newly resolved generation.
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

    /// An as-of change while hidden must not request a catalog nobody will see.
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

    /// Count prefixes apply to half-page movement: `3 ctrl+d` moves 15 rows.
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

    /// A tile with a saved filter must show the filtered indicator after
    /// session restoration so the narrowed list has a visible explanation.
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
