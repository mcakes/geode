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
use geode_shell::actions::ActionId;
use geode_shell::fonts;
use geode_shell::frame::{Frame, FrameVersions};
use geode_shell::keymap::KeyContext;
use geode_shell::module::FindEvent;
use geode_shell::tiling::TileId;
use gpui::prelude::*;
use gpui::{
    App, Context, Entity, IntoElement, UniformListScrollHandle, Window, div, px, uniform_list,
};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use crate::commands::{self, Command, Section};
use crate::sections::{self, Row, Tone};

/// The bounded local tail of log records this tile keeps (spec §4.6:
/// "the ring tail" — a per-tile copy, not the whole ring's own
/// capacity), oldest dropped first once full.
const LOG_CAP: usize = 4_096;

/// `FrameVersions` equality for "does this tile need to rebuild",
/// deliberately excluding `flip` (CLAUDE.md: `flip` only tells a tile
/// with an already-staged snapshot that it may promote — a tile that
/// compares it rebuilds on every flip barrier release for no reason;
/// this tile never stages anything, so comparing `flip` would just mean
/// "rebuild on every OTHER tile's scope/grouping/as-of change too").
fn frame_versions_relevant_eq(a: FrameVersions, b: FrameVersions) -> bool {
    a.scope == b.scope
        && a.grouping == b.grouping
        && a.as_of == b.as_of
        && a.data == b.data
        && a.config == b.config
        && a.saved_scopes == b.saved_scopes
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
    records: VecDeque<Record>,
    rows: Vec<Row>,
    last_diagnostics_version: u64,
    last_frame_versions: FrameVersions,
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

        let last_diagnostics_version = diagnostics.read(cx).version();
        let last_frame_versions = frame.read(cx).versions();

        cx.observe(&diagnostics, |this, diagnostics, cx| {
            let now = diagnostics.read(cx).version();
            if now != this.last_diagnostics_version {
                this.last_diagnostics_version = now;
                this.rebuild(cx);
            }
        })
        .detach();
        cx.observe(&frame, |this, frame, cx| {
            let now = frame.read(cx).versions();
            if !frame_versions_relevant_eq(now, this.last_frame_versions) {
                this.last_frame_versions = now;
                this.rebuild(cx);
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
            since: 0,
            records: VecDeque::new(),
            rows: Vec::new(),
            last_diagnostics_version,
            last_frame_versions,
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
                let mut drained = Vec::new();
                self.ring.drain_since(self.since, &mut drained);
                self.since = latest;
                for r in drained {
                    self.records.push_back(r);
                }
                while self.records.len() > LOG_CAP {
                    self.records.pop_front();
                }
            }
        }
        let now = SystemTime::now();
        {
            let d = self.diagnostics.read(cx);
            let frame = self.frame.read(cx);
            self.rows = match self.section {
                Section::Sources => sections::sources_rows(d, now),
                Section::Data => sections::data_rows(d, frame.as_of(), &self.collapsed),
                Section::Config => sections::config_rows(d, &self.config.borrow(), &self.filter),
                Section::Log => {
                    let records: Vec<Record> = self.records.iter().cloned().collect();
                    sections::log_rows(&records, &self.filter)
                }
                Section::Perf => sections::perf_rows(d, &frame.requery),
            };
        }
        if self.section == Section::Log && self.follow {
            self.cursor = self.rows.len().saturating_sub(1);
        } else {
            self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        }
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
        cx.notify();
    }

    fn set_section(&mut self, section: Section, cx: &mut Context<Self>) {
        if self.section == section {
            return;
        }
        self.section = section;
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
                cx.notify();
            }
            "bottom" => {
                self.cursor = self.rows.len().saturating_sub(1);
                self.follow = self.section == Section::Log;
                cx.notify();
            }
            "page_down" => self.move_cursor(5, cx),
            "page_up" => self.move_cursor(-5, cx),
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
                self.filter = query;
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
            self.diagnostics.update(cx, |d, _cx| {
                d.unwatch();
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
        let header = h_flex()
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
            .child(format!(
                "diagnostics · {} · [ ] to switch",
                self.section.name()
            ));

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
                    let mut cell = div()
                        .w_full()
                        .pl(px(8.0 + r.depth as f32 * 12.0))
                        .font_family(fonts::MONO)
                        .text_color(color)
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
}
