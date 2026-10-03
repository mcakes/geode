//! The diagnostics page entity: one section at a time over the shell's
//! `Diagnostics` entity, the log ring, the loaded config, and the frame's
//! requery stats. Observers rebuild only the selected section from its own
//! inputs; the table paints a shared prepared table; the detail strip shows
//! the cursor row.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use geode_core::config::Config;
use geode_core::log::{Level, Ring};
use geode_shell::actions::ActionId;
use geode_shell::diagnostics::{DiagVersions, Diagnostics};
use geode_shell::frame::{FrameRef, FrameVersions};
use geode_shell::keymap::KeyContext;
use geode_shell::module::ShellActions;
use geode_shell::shell::{chip, scale};
use geode_tile::motion::{self, Motion};
use gpui::prelude::*;
use gpui::{
    AnyElement, AnyWindowHandle, App, ClipboardItem, Context, Entity, FocusHandle, Focusable as _,
    SharedString, Task, WeakEntity, Window, div,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::select::{SearchableVec, SelectEvent, SelectState};
use gpui_component::table::{TableEvent, TableState};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, IndexPath, Selectable as _, Sizable as _, h_flex, v_flex,
};

use crate::config_view::{self, ConfigView};
use crate::levels::{self, LevelRow, LevelsState};
use crate::log::{LogFilter, LogTail};
use crate::log_cache::{self, LogCache, Narrowed};
use crate::log_view::{self, LogView};
use crate::model::{self, Badges, Tone};
use crate::prepared::{self, PreparedTable, SINCE_COLUMN, SINCE_SEPARATOR};
use crate::section::Section;
use crate::table::{SectionDelegate, table_el};

/// Width of the filter input, in pixels at the design rem.
const FILTER_WIDTH: f32 = 240.0;

/// The target select's first item: no target filter.
const ALL_TARGETS: &str = "All targets";

/// The target select's delegate: `all`, then every target in the tail.
type TargetSelect = SelectState<SearchableVec<SharedString>>;

/// How often the Sources ages tick while the section is shown.
const AGES_TICK: Duration = Duration::from_secs(1);

/// The diagnostics counter each section's builder reads. Comparing only
/// this counter keeps an unrelated change, such as a perf tick, from
/// rebuilding the config rows. The log observer also asks the tail whether
/// the ring has new records, because the ring lives outside `Diagnostics`.
/// Reference also reads source health: its status chip carries a failing
/// source's reason, which changes between answers.
fn diag_version_for(section: Section, v: DiagVersions) -> (u64, u64) {
    match section {
        Section::Sources => (v.sources, 0),
        Section::Data => (v.data, 0),
        Section::Reference => (v.reference, v.sources),
        Section::Config => (v.config, 0),
        Section::Log => (v.log_levels, 0),
        Section::Perf => (v.perf, 0),
    }
}

/// The reference dataset `view` selects, clamped so a shorter list never
/// strands the selection; `None` when no dataset is declared.
fn selected_reference(names: &[String], view: usize) -> Option<&str> {
    names
        .get(view.min(names.len().saturating_sub(1)))
        .map(String::as_str)
}

fn title_for(section: Section) -> SharedString {
    SharedString::from(format!("Diagnostics · {}", section.title()))
}

pub struct DiagnosticsPage {
    pub(crate) frame: FrameRef,
    pub(crate) diagnostics: Entity<Diagnostics>,
    config: Rc<RefCell<Config>>,
    actions: ShellActions,
    focus_handle: FocusHandle,
    section: Section,
    /// Cursor row per section, kept across switches.
    cursors: [usize; 6],
    selected_keys: [Option<String>; 6],
    /// Filter text per section; the one input shows the selected section's.
    filters: [String; 6],
    filter_input: Entity<InputState>,
    filter_entry: Option<String>,
    table: Entity<TableState<SectionDelegate>>,
    prepared: Rc<PreparedTable>,
    /// The Sources rows' health `since` times, in `prepared` row order,
    /// for the ages tick; empty on every other section.
    source_since: Vec<Option<SystemTime>>,
    /// Current and historical configuration issues share a retained table.
    /// Motions target whichever configuration view is visible.
    diag_table: Entity<TableState<SectionDelegate>>,
    diag_prepared: Rc<PreparedTable>,
    /// Selected issue, independent of the effective-values cursor.
    diag_cursor: usize,
    /// The rem the delegates' column widths were prepared at; `refresh`
    /// runs only when the window's rem moves off it.
    last_rem: f32,
    collapsed_datasets: BTreeSet<String>,
    collapsed_docs: BTreeSet<String>,
    /// The issues view shows prior batches instead of the current one.
    config_history: bool,
    /// Effective values and issues each retain their own cursor.
    config_values: bool,
    /// Whether the catalog was taken under the frame's as-of; the Data
    /// toolbar chip. Cached at rebuild: both inputs rebuild the section.
    catalog_matches: bool,
    /// Index into `Diagnostics::reference_datasets` of the dataset the
    /// Reference section shows; clamped on read, so a shorter list never
    /// strands it.
    pub(crate) reference_view: usize,
    /// The Reference toolbar chip, formatted at rebuild.
    pub(crate) reference_status: (SharedString, Tone),
    /// Rows in the answer the Reference table was built from; the result
    /// summary's total, so it always counts what the table shows.
    reference_total: usize,
    /// One `(element id, label)` per declared reference dataset, for the
    /// toolbar's dataset buttons; prepared at rebuild so paint formats
    /// nothing.
    reference_views: Vec<(SharedString, SharedString)>,
    log: LogTail,
    log_filter: LogFilter,
    /// The tail's rows, formatted once per record.
    log_cache: LogCache,
    /// The narrowing the Log table shows: `None` without a query. While a
    /// changed query narrows off the UI thread this still holds the
    /// previous one, so the table keeps its last answer until the new one
    /// lands.
    log_narrowed: Option<Narrowed>,
    /// The narrowing in flight: its query, the cache generation it reads,
    /// and the task, whose drop cancels it.
    log_narrowing: Option<(String, u64, Task<()>)>,
    follow: bool,
    /// The window this page was created in: the target select's items
    /// can only be replaced with a window, and observers bring none.
    window: AnyWindowHandle,
    pub(crate) target_select: Entity<TargetSelect>,
    /// The select's current items, so an unchanged tail replaces nothing.
    target_items: Vec<SharedString>,
    pub(crate) levels: LevelsState,
    /// A programmatic `set_log_target` must show in the select even when
    /// the item set is unchanged; the next sync reselects.
    select_stale: bool,
    /// The popover's rows, prepared with the Log section.
    level_rows: Rc<Vec<LevelRow>>,
    badges: Badges,
    /// Rail badge text per section, formatted with the badges.
    rail_texts: [SharedString; 6],
    header_chips: Vec<(SharedString, Tone)>,
    /// The Config toolbar's History label, formatted with the badges.
    history_label: SharedString,
    /// The active row's detail joined for the copy button, cached
    /// with every cursor or table change so paint formats nothing.
    copy_text: Option<SharedString>,
    detail_scroll: gpui::ScrollHandle,
    /// [`Self::title`]'s cache, replaced only on a section change.
    title: SharedString,
    perf: Option<crate::perf_view::PerformanceView>,
    result_summary: SharedString,
    detail_position: SharedString,
    issue_count: usize,
    visible: bool,
    /// A page input holds focus; see `key_context`.
    insert_mode: bool,
    /// The Sources ages tick; held only while visible on Sources, so
    /// dropping it is what stops the loop.
    ages_timer: Option<Task<()>>,
    last_diag_versions: DiagVersions,
    last_frame_versions: FrameVersions,
    #[cfg(test)]
    pub(crate) rebuild_count: u32,
    /// Narrowing passes started, for tests of in-flight reuse.
    #[cfg(test)]
    log_passes: u32,
}

impl DiagnosticsPage {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        frame: FrameRef,
        diagnostics: Entity<Diagnostics>,
        ring: Arc<Ring>,
        config: Rc<RefCell<Config>>,
        actions: ShellActions,
        restored: Option<&toml::Table>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let section = restored
            .and_then(|t| t.get("section"))
            .and_then(|v| v.as_str())
            .and_then(Section::from_name)
            .unwrap_or(Section::Sources);

        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter…"));
        cx.subscribe_in(
            &filter_input,
            window,
            |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let text = input.read(cx).value();
                    let ix = this.section as usize;
                    if this.filters[ix] != text.as_ref() {
                        this.filters[ix] = text.to_string();
                        this.rebuild(cx);
                    }
                }
                InputEvent::Focus => {
                    this.filter_entry = Some(this.filters[this.section as usize].clone());
                    this.sync_insert_mode(window, cx);
                }
                InputEvent::Blur => {
                    this.filter_entry = None;
                    this.sync_insert_mode(window, cx);
                }
                InputEvent::PressEnter { .. } => this.focus_handle.focus(window, cx),
            },
        )
        .detach();
        let target_items = vec![SharedString::from(ALL_TARGETS)];
        let target_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(target_items.clone()),
                Some(IndexPath::default()),
                window,
                cx,
            )
        });
        cx.subscribe_in(
            &target_select,
            window,
            |this, _select, event: &SelectEvent<SearchableVec<SharedString>>, _window, cx| {
                let SelectEvent::Confirm(value) = event;
                let target = value
                    .as_ref()
                    .filter(|v| v.as_ref() != ALL_TARGETS)
                    .map(|v| v.to_string());
                this.set_log_target(target, cx);
            },
        )
        .detach();

        let table = Self::new_table(SectionDelegate::new(), window, cx);
        // `set_selected_row` echoes `SelectRow`; `set_cursor` returns early
        // when the row is already the cursor, so the echo is inert. A
        // single click only selects, so a click never surprises with a
        // layout change; the double-click toggles the row like `activate`.
        cx.subscribe_in(
            &table,
            window,
            |this, _table, event: &TableEvent, _window, cx| match event {
                TableEvent::SelectRow(ix) => this.set_cursor(*ix, cx),
                TableEvent::DoubleClickedRow(ix) => {
                    this.set_cursor(*ix, cx);
                    this.toggle_expansion_at_cursor(None, cx);
                }
                _ => {}
            },
        )
        .detach();
        // Issue rows expand nothing; pointer selection and motions share
        // the same cursor and details.
        let diag_table = Self::new_table(
            SectionDelegate::with_row_selector("diagnostics-diag-row"),
            window,
            cx,
        );
        cx.subscribe_in(
            &diag_table,
            window,
            |this, _table, event: &TableEvent, _window, cx| {
                if let TableEvent::SelectRow(ix) = event {
                    this.set_diag_cursor(*ix, cx);
                }
            },
        )
        .detach();

        let last_diag_versions = diagnostics.read(cx).versions();
        let last_frame_versions = frame.read(cx).versions();

        cx.observe(&diagnostics, |this, diagnostics, cx| {
            let now = diagnostics.read(cx).versions();
            // A hidden page is retained for the window's lifetime: every
            // poll, publication, and config batch would otherwise run a
            // section builder and format badges for a surface nobody sees.
            // The baseline moves so nothing is stale; `set_visible(true)`
            // rebuilds once, and that rebuild drains the ring, so a wrap
            // while closed is reported then rather than overwritten by an
            // idle drain.
            if !this.visible {
                this.last_diag_versions = now;
                return;
            }
            let has_new = this.log.has_new();
            let relevant = if this.section == Section::Log {
                has_new || now.log_levels != this.last_diag_versions.log_levels
            } else {
                diag_version_for(this.section, now)
                    != diag_version_for(this.section, this.last_diag_versions)
            };
            // Badges read every counter and the tail's error count: refresh
            // them on any counter change or new record, whatever section
            // is selected.
            let any = has_new || now != this.last_diag_versions;
            // A publication of a reference dataset is an edge to re-ask on;
            // `reference` itself is not, since a refusal bumps it.
            let published = now.reference_published != this.last_diag_versions.reference_published;
            this.last_diag_versions = now;
            if relevant {
                this.rebuild(cx);
            } else if any {
                this.refresh_badges(cx);
            }
            if published {
                this.request_reference(cx);
            }
        })
        .detach();
        // Timestamps are formatted at rebuild, so a clock-setting change
        // rebuilds the selected section; a hidden page waits for its show.
        cx.observe_global::<geode_shell::clock::AppClock>(|this, cx| {
            if this.visible {
                this.rebuild(cx);
            }
        })
        .detach();
        // The app registers its config-refresh frame observer before pages
        // are created; it must update the shared `Config` before this
        // observer rebuilds config rows on the same version change.
        cx.observe(frame.entity(), |this, _, cx| {
            // Read through the page's own handle: the observed entity alone
            // would answer for the shared lane, not the bound workspace's.
            let now = this.frame.read(cx).versions();
            // Hidden: move the baseline only, as the diagnostics observer
            // does. The catalog refresh below is a visible page's too; the
            // show requests its own.
            if !this.visible {
                this.last_frame_versions = now;
                return;
            }
            let as_of_changed = now.as_of != this.last_frame_versions.as_of;
            let config_changed = now.config != this.last_frame_versions.config;
            // Only data and reference rows read the frame's as-of; only
            // config rows read the loaded config. Scope and grouping
            // keystrokes rebuild nothing here.
            let relevant = match this.section {
                Section::Data | Section::Reference => as_of_changed,
                Section::Config => config_changed,
                Section::Sources | Section::Log | Section::Perf => false,
            };
            this.last_frame_versions = now;
            if relevant {
                this.rebuild(cx);
            } else if as_of_changed {
                // The Reference badge counts only an answer at the frame's
                // as-of, whatever section is shown.
                this.refresh_badges(cx);
            }
            // The catalog resolves generation markers under the request's
            // as-of: a visible page needs a fresh snapshot when it changes.
            // A hidden page requests one when it becomes visible.
            if as_of_changed && this.visible {
                this.diagnostics.update(cx, |d, cx| {
                    d.request_catalog_refresh();
                    cx.notify();
                });
                this.request_reference(cx);
            }
        })
        .detach();

        // The tail owns the ring; the page reads records through it.
        let log = LogTail::new(ring);
        let mut this = DiagnosticsPage {
            frame,
            diagnostics,
            config,
            actions,
            focus_handle: cx.focus_handle(),
            section,
            cursors: [0; 6],
            selected_keys: Default::default(),
            filters: Default::default(),
            filter_input,
            filter_entry: None,
            table,
            prepared: Rc::new(PreparedTable::empty()),
            source_since: Vec::new(),
            diag_table,
            diag_prepared: Rc::new(PreparedTable::empty()),
            diag_cursor: 0,
            last_rem: scale::DESIGN_REM,
            collapsed_datasets: BTreeSet::new(),
            collapsed_docs: BTreeSet::new(),
            config_history: false,
            config_values: false,
            catalog_matches: false,
            reference_view: 0,
            reference_status: (SharedString::new_static("Loading"), Tone::Muted),
            reference_total: 0,
            reference_views: Vec::new(),
            log,
            log_filter: LogFilter::all(),
            log_cache: LogCache::default(),
            log_narrowed: None,
            log_narrowing: None,
            follow: true,
            window: window.window_handle(),
            target_select,
            target_items,
            levels: LevelsState::default(),
            select_stale: false,
            level_rows: Rc::new(Vec::new()),
            badges: Badges {
                sources: (None, 0),
                datasets: 0,
                reference: 0,
                config: (0, 0),
                log_errors: 0,
                perf_p95: String::new(),
            },
            rail_texts: Default::default(),
            header_chips: Vec::new(),
            history_label: SharedString::default(),
            copy_text: None,
            detail_scroll: gpui::ScrollHandle::new(),
            title: title_for(section),
            perf: None,
            result_summary: SharedString::default(),
            detail_position: SharedString::default(),
            issue_count: 0,
            visible: false,
            insert_mode: false,
            ages_timer: None,
            last_diag_versions,
            last_frame_versions,
            #[cfg(test)]
            rebuild_count: 0,
            #[cfg(test)]
            log_passes: 0,
        };
        this.rebuild(cx);
        this
    }

    /// Both tables share one shape: row selection only, resizable fixed
    /// columns, no sorting.
    fn new_table(
        delegate: SectionDelegate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TableState<SectionDelegate>> {
        cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .row_selectable(true)
                .col_selectable(false)
                .cell_selectable(false)
                .col_resizable(true)
                .col_movable(false)
                .sortable(false)
                .loop_selection(false)
        })
    }

    pub fn section(&self) -> Section {
        self.section
    }

    pub fn cursor(&self) -> usize {
        self.cursors[self.section as usize]
    }

    pub fn prepared(&self) -> &Rc<PreparedTable> {
        &self.prepared
    }

    /// Module test fixtures may omit `AppClock`; the machine clock then.
    fn clock(cx: &App) -> geode_core::clock::Clock {
        cx.try_global::<geode_shell::clock::AppClock>()
            .map(|c| c.0)
            .unwrap_or_else(|| geode_core::clock::Clock::machine().0)
    }

    /// Rebuild the selected section's prepared table, the badges, and the
    /// header chips. Only the selected section's builder runs.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        #[cfg(test)]
        {
            self.rebuild_count += 1;
        }
        let clock = Self::clock(cx);
        let now = SystemTime::now();
        if self.section == Section::Log {
            self.drain_tail();
        }
        let filter = self.filters[self.section as usize].clone();
        // Configuration issues and effective values share the same input
        // version. Both tables are retained across view switches.
        let mut diag_prepared = None;
        let mut source_since = Vec::new();
        let mut narrow_log = false;
        let prepared = {
            let d = self.diagnostics.read(cx);
            let frame = self.frame.read(cx);
            match self.section {
                Section::Sources => {
                    let (table, since) =
                        prepared::sources_table(&model::source_rows(d, clock), now, &filter);
                    source_since = since;
                    table
                }
                Section::Data => {
                    self.catalog_matches = model::catalog_matches_frame(d, frame.as_of());
                    prepared::data_table(
                        &model::dataset_rows(d, frame.as_of(), clock),
                        &self.collapsed_datasets,
                        &filter,
                    )
                }
                Section::Reference => {
                    let names = &d.reference_datasets;
                    let dataset = selected_reference(names, self.reference_view);
                    if self.reference_views.len() != names.len()
                        || self
                            .reference_views
                            .iter()
                            .zip(names)
                            .any(|((_, label), name)| label.as_ref() != name)
                    {
                        self.reference_views = names
                            .iter()
                            .map(|n| {
                                (
                                    SharedString::from(format!("diagnostics-reference-view-{n}")),
                                    SharedString::from(n.clone()),
                                )
                            })
                            .collect();
                    }
                    let (text, tone) = model::reference_status(d, dataset, frame.as_of(), &clock);
                    self.reference_status = (text.into(), tone);
                    // Filtered by dataset, not as-of: the previous
                    // as-of's rows stay while the chip reads Loading.
                    let answer = d
                        .reference
                        .as_ref()
                        .filter(|o| Some(o.dataset.as_str()) == dataset)
                        .and_then(|o| o.table.as_ref().ok())
                        .and_then(|t| t.as_ref());
                    self.reference_total = answer.map_or(0, |t| t.rows.len());
                    prepared::reference_table(answer, &filter)
                }
                Section::Config => {
                    let diags = if self.config_history {
                        model::history_diagnostics(d, clock)
                    } else {
                        model::current_diagnostics(d)
                    };
                    self.issue_count = diags.len();
                    diag_prepared = Some(prepared::diagnostics_table(
                        &diags,
                        self.config_history,
                        &filter,
                    ));
                    let expanded_docs = BTreeSet::new();
                    prepared::config_table(
                        &model::config_docs(&self.config.borrow(), &filter),
                        // A blank query narrows nothing, so it leaves
                        // the stored expansion alone too.
                        if filter.trim().is_empty() {
                            &self.collapsed_docs
                        } else {
                            &expanded_docs
                        },
                    )
                }
                Section::Log => {
                    // The one input is the log's text filter.
                    self.log_filter.text = filter;
                    self.level_rows = Rc::new(levels::level_rows(&d.levels));
                    self.log_cache.sync(self.log.records(), clock);
                    // A clock change reformats every entry: an answer,
                    // held or in flight, matched the old time text.
                    if self
                        .log_narrowed
                        .as_ref()
                        .is_some_and(|n| n.is_stale(&self.log_cache))
                    {
                        self.log_narrowed = None;
                        self.log_narrowing = None;
                    }
                    if self
                        .log_narrowing
                        .as_ref()
                        .is_some_and(|(_, g, _)| *g != self.log_cache.generation())
                    {
                        self.log_narrowing = None;
                    }
                    if self.log_filter.text.trim().is_empty() {
                        self.log_narrowed = None;
                        self.log_narrowing = None;
                    } else {
                        narrow_log = true;
                    }
                    if let (Some(narrowed), Some(first)) =
                        (&mut self.log_narrowed, self.log_cache.first_seq())
                    {
                        narrowed.prune(first);
                    }
                    log_cache::log_table(
                        &self.log_cache,
                        &self.log_filter,
                        self.log_narrowed.as_ref(),
                        self.log.lost(),
                    )
                }
                Section::Perf => {
                    self.perf = Some(crate::perf_view::PerformanceView::new(&model::perf_model(
                        d,
                        &frame.requery,
                        clock,
                    )));
                    PreparedTable::empty()
                }
            }
        };
        self.prepared = Rc::new(prepared);
        self.source_since = source_since;
        if narrow_log {
            self.narrow_log(cx);
        }
        let len = self.prepared.rows.len();
        let ix = self.section as usize;
        if self.section == Section::Log && self.follow {
            self.cursors[ix] = len.saturating_sub(1);
        } else {
            self.cursors[ix] = self.selected_keys[ix]
                .as_ref()
                .and_then(|key| self.prepared.rows.iter().position(|r| &r.key == key))
                .unwrap_or(self.cursors[ix].min(len.saturating_sub(1)));
        }
        let cursor = self.cursors[ix];
        self.selected_keys[ix] = self.prepared.rows.get(cursor).map(|r| r.key.clone());
        let shared = self.prepared.clone();
        let (empty_title, empty_help): (SharedString, SharedString) =
            if self.section == Section::Reference {
                self.reference_empty_text(ix)
            } else if !self.filters[ix].is_empty() {
                (
                    "No matching rows".into(),
                    "Try a broader filter or clear the search field.".into(),
                )
            } else {
                let (title, help) = match self.section {
                    Section::Sources => (
                        "No sources reported",
                        "Source health appears here when a configured source reports activity.",
                    ),
                    Section::Data => (
                        "No datasets available",
                        "Refresh the catalog to check for stored datasets.",
                    ),
                    Section::Config => (
                        "No configuration values",
                        "Loaded configuration documents appear here.",
                    ),
                    Section::Log if self.log.is_empty() => (
                        "No log records yet",
                        "New records appear here while Diagnostics is open.",
                    ),
                    Section::Log => (
                        "No matching records",
                        "Enable a level, choose All targets, or clear the search field.",
                    ),
                    // Reference words its own above; Perf paints no table.
                    Section::Reference | Section::Perf => ("", ""),
                };
                (title.into(), help.into())
            };
        // `column()` is read only when the table prepares its layout, so
        // every new table needs a `refresh` before it paints.
        self.table.update(cx, |t, cx| {
            t.delegate_mut().set(shared);
            t.delegate_mut().set_empty(empty_title, empty_help);
            t.refresh(cx);
            if len > 0 {
                t.set_selected_row(cursor, cx);
            }
        });
        if let Some(diag) = diag_prepared {
            let selected_key = self
                .diag_prepared
                .rows
                .get(self.diag_cursor)
                .map(|r| r.key.clone());
            self.diag_prepared = Rc::new(diag);
            let len = self.diag_prepared.rows.len();
            self.diag_cursor = selected_key
                .as_ref()
                .and_then(|key| self.diag_prepared.rows.iter().position(|r| &r.key == key))
                .unwrap_or(self.diag_cursor.min(len.saturating_sub(1)));
            let (shared, cursor) = (self.diag_prepared.clone(), self.diag_cursor);
            let (title, help) = if !self.filters[ix].is_empty() {
                (
                    "No matching issues",
                    "Try a broader filter or clear the search field.",
                )
            } else if self.config_history {
                (
                    "No earlier issues",
                    "Previous configuration and data diagnostics appear here after a change.",
                )
            } else {
                (
                    "No current issues",
                    "Configuration and data checks have no warnings or errors to report.",
                )
            };
            self.diag_table.update(cx, |t, cx| {
                t.delegate_mut().set(shared);
                t.delegate_mut().set_empty(title, help);
                t.refresh(cx);
                if len > 0 {
                    t.set_selected_row(cursor, cx);
                }
            });
        }
        if self.section == Section::Log {
            self.sync_target_items(cx);
        }
        self.refresh_copy_text();
        self.refresh_result_summary(cx);
        self.refresh_badges(cx);
        cx.notify();
    }

    /// The Reference table's empty state, from the status the chip shows:
    /// no dataset declared, Loading, a refusal, an error or no generation.
    /// A filter that hides every row of an answer says how many it hid.
    fn reference_empty_text(&self, ix: usize) -> (SharedString, SharedString) {
        if !self.filters[ix].is_empty() && self.reference_total > 0 {
            return (
                format!(
                    "Filter matches nothing ({} row{})",
                    self.reference_total,
                    if self.reference_total == 1 { "" } else { "s" }
                )
                .into(),
                "Try a broader filter or clear the search field.".into(),
            );
        }
        let help = if self.reference_views.is_empty() {
            "Declare a dataset of the reference family in the schema to inspect it here."
        } else {
            "Press r to poll the dataset's sources and read it again."
        };
        (self.reference_status.0.clone(), help.into())
    }

    /// Start the narrowing the Log's query still needs, off the UI thread:
    /// the whole cache for a changed query, or only the entries newer than
    /// the current narrowing for an unchanged one.
    ///
    /// A pass already in flight for the same query is left to finish,
    /// however many records have arrived since it started: restarting it
    /// per arrival could starve it under a busy source. When it lands,
    /// [`Self::apply_log_narrowing`] rebuilds, and this chains the cheap
    /// pass over just the newer records. A pass for another query is
    /// replaced; dropping its task stops its result from applying.
    fn narrow_log(&mut self, cx: &mut Context<Self>) {
        let query = self.log_filter.text.clone();
        let target = self.log_cache.last_seq();
        let generation = self.log_cache.generation();
        if self
            .log_narrowing
            .as_ref()
            .is_some_and(|(q, g, _)| *q == query && *g == generation)
        {
            return;
        }
        let from = match &self.log_narrowed {
            Some(n) if n.query() == query => {
                if n.through() >= target {
                    self.log_narrowing = None;
                    return;
                }
                n.through()
            }
            _ => None,
        };
        let entries = self.log_cache.after(from);
        #[cfg(test)]
        {
            self.log_passes += 1;
        }
        let task_query = query.clone();
        let task = cx.spawn(async move |this, cx| {
            let narrowed = cx
                .background_executor()
                .spawn(async move { Narrowed::run(&task_query, &entries) })
                .await;
            let _ = this.update(cx, |page, cx| page.apply_log_narrowing(narrowed, cx));
        });
        self.log_narrowing = Some((query, generation, task));
    }

    /// Take a finished narrowing: append it to the current one when it
    /// continues the same query, else replace it, then rebuild if the Log
    /// still shows. A result for a query the input no longer holds is
    /// dropped.
    fn apply_log_narrowing(&mut self, narrowed: Narrowed, cx: &mut Context<Self>) {
        self.log_narrowing = None;
        if narrowed.query() != self.log_filter.text || narrowed.is_stale(&self.log_cache) {
            return;
        }
        match &mut self.log_narrowed {
            // An older stretch than the one already held is stale.
            Some(n) if n.query() == narrowed.query() => {
                if !n.extend(narrowed) {
                    return;
                }
            }
            _ => self.log_narrowed = Some(narrowed),
        }
        if self.visible && self.section == Section::Log {
            self.rebuild(cx);
            cx.notify();
        }
    }

    /// Pull the ring into the tail, when the page is shown. A hidden
    /// page's drain would report a wrap nobody sees and then clear it.
    fn drain_tail(&mut self) {
        if self.visible {
            self.log.drain();
        }
    }

    /// Offer `all` and every target in the tail, plus the selected target
    /// when the tail no longer holds it, so the select never shows a
    /// filter it cannot name; reselect the filter's target whenever the
    /// items or the target changed. Both need the window, which the
    /// observers that rebuild do not carry, so the work is deferred to the
    /// window this page lives in.
    fn sync_target_items(&mut self, cx: &mut Context<Self>) {
        let mut items = vec![SharedString::from(ALL_TARGETS)];
        items.extend(
            model::log_targets(self.log.records())
                .into_iter()
                .map(SharedString::from),
        );
        if let Some(t) = &self.log_filter.target
            && !items.iter().any(|i| i.as_ref() == t)
        {
            items.push(SharedString::from(t.clone()));
        }
        let changed = items != self.target_items;
        if !changed && !self.select_stale {
            return;
        }
        self.select_stale = false;
        let items = changed.then(|| {
            self.target_items = items.clone();
            items
        });
        let selected = self
            .log_filter
            .target
            .clone()
            .map(SharedString::from)
            .unwrap_or_else(|| SharedString::from(ALL_TARGETS));
        let (select, handle) = (self.target_select.clone(), self.window);
        cx.defer(move |cx| {
            let _ = cx.update_window(handle, |_, window, cx| {
                select.update(cx, |s, cx| {
                    if let Some(items) = items {
                        s.set_items(SearchableVec::new(items), window, cx);
                    }
                    s.set_selected_value(&selected, window, cx);
                });
            });
        });
    }

    fn showing_issues(&self) -> bool {
        self.section == Section::Config && !self.config_values
    }

    fn active_row(&self) -> Option<&prepared::PreparedRow> {
        if self.showing_issues() {
            self.diag_prepared.rows.get(self.diag_cursor)
        } else {
            self.prepared.rows.get(self.cursor())
        }
    }

    fn refresh_copy_text(&mut self) {
        let (cursor, count) = if self.showing_issues() {
            (self.diag_cursor, self.diag_prepared.rows.len())
        } else {
            (self.cursor(), self.prepared.rows.len())
        };
        self.detail_position = if count == 0 {
            SharedString::default()
        } else {
            format!("Details · row {} of {count}", cursor + 1).into()
        };
        let text = self
            .active_row()
            .filter(|r| !r.detail.is_empty())
            .map(|r| SharedString::from(r.detail.join("\n")));
        if self.copy_text != text {
            self.detail_scroll.set_offset(Default::default());
            self.copy_text = text;
        }
    }

    fn has_filters(&self) -> bool {
        !self.filters[self.section as usize].is_empty()
            || (self.section == Section::Log
                && (self.log_filter.target.is_some()
                    || self.log_filter.levels.iter().any(|on| !on)))
    }

    /// Reset only the visible section and return focus before its reset
    /// button disappears. In Log, levels and target are also view filters.
    pub(crate) fn reset_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.section == Section::Perf {
            return;
        }
        self.focus_handle.focus(window, cx);
        self.filter_entry = None;
        self.filters[self.section as usize].clear();
        self.filter_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        if self.section == Section::Log {
            self.log_filter = LogFilter::all();
            self.select_stale = true;
        }
        self.rebuild(cx);
    }

    fn refresh_result_summary(&mut self, cx: &Context<Self>) {
        let rows = &self.prepared.rows;
        let parents = || {
            rows.iter()
                .filter(|r| matches!(r.kind, prepared::RowKind::Parent { .. }))
                .count()
        };
        self.result_summary = match self.section {
            Section::Sources => format!(
                "{} of {} sources",
                rows.len(),
                self.diagnostics.read(cx).sources.len()
            ),
            Section::Data => format!(
                "{} of {} datasets · {} rows shown",
                parents(),
                self.diagnostics.read(cx).datasets.len(),
                rows.len()
            ),
            // The total is the answer the table was built from, so the
            // two never disagree, even over a stale as-of's rows.
            Section::Reference => format!(
                "{} of {} row{}",
                rows.len(),
                self.reference_total,
                if self.reference_total == 1 { "" } else { "s" }
            ),
            Section::Config if self.showing_issues() => format!(
                "{} of {} {} issue{}",
                self.diag_prepared.rows.len(),
                self.issue_count,
                if self.config_history {
                    "historical"
                } else {
                    "current"
                },
                if self.issue_count == 1 { "" } else { "s" }
            ),
            Section::Config => format!(
                "{} documents · {} values shown",
                parents(),
                rows.iter()
                    .filter(|r| r.kind == prepared::RowKind::Child)
                    .count()
            ),
            Section::Log => format!(
                "{} of {} retained records · limit {}",
                rows.iter()
                    .filter(|r| r.kind != prepared::RowKind::Notice)
                    .count(),
                self.log.len(),
                crate::log::LOG_CAP
            ),
            Section::Perf => String::new(),
        }
        .into();
    }

    fn render_results(&self, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .min_h_8()
            .flex_none()
            .flex_wrap()
            .gap_2()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .debug_selector(|| "diagnostics-results".to_string())
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.result_summary.clone()),
            )
            .when(self.section == Section::Log, |el| {
                el.child(div().text_xs().child(if self.follow {
                    "Following latest"
                } else {
                    "Auto-follow paused"
                }))
            })
            .child(div().flex_1())
            .when(self.has_filters(), |el| {
                el.child(crate::page_chrome::probed(
                    "diagnostics-reset-filters",
                    Button::new("diagnostics-reset-filters")
                        .ghost()
                        .small()
                        .label("Reset filters")
                        .tooltip("Reset this section’s filters (Alt+Backspace)")
                        .on_click(cx.listener(|p, _, window, cx| p.reset_filters(window, cx))),
                ))
            })
            .into_any_element()
    }

    pub(crate) fn copy_details(&self, cx: &mut Context<Self>) {
        if let Some(text) = &self.copy_text {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        }
    }

    fn set_diag_cursor(&mut self, ix: usize, cx: &mut Context<Self>) {
        let ix = ix.min(self.diag_prepared.rows.len().saturating_sub(1));
        if self.diag_cursor == ix {
            return;
        }
        self.diag_cursor = ix;
        if !self.diag_prepared.rows.is_empty() {
            self.diag_table
                .update(cx, |t, cx| t.set_selected_row(ix, cx));
        }
        self.refresh_copy_text();
        cx.notify();
    }

    pub(crate) fn set_config_values(&mut self, cx: &mut Context<Self>) {
        self.config_values = true;
        self.refresh_copy_text();
        self.refresh_result_summary(cx);
        cx.notify();
    }

    fn cycle_config_view(&mut self, backwards: bool, cx: &mut Context<Self>) {
        let current = if self.config_values {
            2
        } else {
            usize::from(self.config_history)
        };
        match (current + if backwards { 2 } else { 1 }) % 3 {
            0 => self.set_config_history(false, cx),
            1 => self.set_config_history(true, cx),
            _ => self.set_config_values(cx),
        }
    }

    fn refresh_badges(&mut self, cx: &mut Context<Self>) {
        // The Log badge counts the whole tail, so the tail is drained here
        // for every section, not only when the Log table rebuilds. A second
        // drain after the Log rebuild's own returns false and changes
        // nothing; the loss gap is still measured at the last drain.
        self.drain_tail();
        let clock = Self::clock(cx);
        let d = self.diagnostics.read(cx);
        let dataset = selected_reference(&d.reference_datasets, self.reference_view);
        let as_of = self.frame.read(cx).as_of();
        let log_errors = self
            .log
            .records()
            .filter(|r| r.level == geode_core::log::Level::ERROR)
            .count();
        self.badges = model::badges(d, log_errors, dataset, as_of);
        self.header_chips = model::header_chips(d, clock)
            .into_iter()
            .map(|(s, t)| (SharedString::from(s), t))
            .collect();
        self.rail_texts = crate::page_chrome::rail_texts(&self.badges);
        // The front batch is the current one; history is the rest.
        let prior = d.config_history.len().saturating_sub(1);
        self.history_label = SharedString::from(if prior == 1 {
            "History (1 batch)".to_string()
        } else {
            format!("History ({prior} batches)")
        });
        cx.notify();
    }

    /// Switch the Config issues view between the current batch
    /// and the prior ones. The lists are unrelated, so the panel's
    /// selection restarts at the top.
    pub(crate) fn set_config_history(&mut self, history: bool, cx: &mut Context<Self>) {
        if self.config_history == history && !self.config_values {
            return;
        }
        self.config_values = false;
        self.config_history = history;
        self.diag_cursor = 0;
        self.rebuild(cx);
    }

    /// Filter the tail to one target (`None` for all). The select's
    /// confirm lands here, and a programmatic call is shown by the select
    /// on the rebuild's sync.
    pub fn set_log_target(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        if self.log_filter.target == target {
            return;
        }
        self.log_filter.target = target;
        self.select_stale = true;
        self.rebuild(cx);
    }

    /// Flip one level toggle, indexed as [`LogFilter::levels`].
    pub(crate) fn toggle_level(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(on) = self.log_filter.levels.get_mut(ix) {
            *on = !*on;
            self.rebuild(cx);
        }
    }

    /// Show one more (`more`) or one fewer level, as a minimum severity:
    /// the levels from ERROR down to the next step on, the rest off. The
    /// step counts from the most verbose level shown, so a hand-picked
    /// set becomes the contiguous one it reaches; ERROR always stays.
    pub(crate) fn step_min_level(&mut self, more: bool, cx: &mut Context<Self>) {
        let last = self.log_filter.levels.len() - 1;
        let shown = self.log_filter.levels.iter().rposition(|on| *on);
        let next = match (shown, more) {
            (None, _) => 0,
            (Some(ix), true) => (ix + 1).min(last),
            (Some(ix), false) => ix.saturating_sub(1),
        };
        let levels = std::array::from_fn(|ix| ix <= next);
        if levels != self.log_filter.levels {
            self.log_filter.levels = levels;
            self.rebuild(cx);
        }
    }

    /// Step the target filter through the select's items (all, then each
    /// target in the tail), wrapping. The rebuild's sync shows the pick in
    /// the select.
    pub(crate) fn step_target(&mut self, backwards: bool, cx: &mut Context<Self>) {
        let mut targets: Vec<Option<String>> = vec![None];
        targets.extend(
            model::log_targets(self.log.records())
                .into_iter()
                .map(|t| Some(t.to_string())),
        );
        if self.log_filter.target.is_some() && !targets.contains(&self.log_filter.target) {
            targets.push(self.log_filter.target.clone());
        }
        let len = targets.len();
        let at = targets
            .iter()
            .position(|t| *t == self.log_filter.target)
            .unwrap_or(0);
        let next = if backwards {
            (at + len - 1) % len
        } else {
            (at + 1) % len
        };
        let target = targets.swap_remove(next);
        self.set_log_target(target, cx);
    }

    /// Follow on jumps to the last row, as `bottom` does; off leaves the
    /// cursor where it is.
    pub(crate) fn set_follow(&mut self, follow: bool, cx: &mut Context<Self>) {
        if follow {
            self.jump_to_bottom(cx);
        } else {
            self.follow = false;
            cx.notify();
        }
    }

    fn jump_to_bottom(&mut self, cx: &mut Context<Self>) {
        let last = self.prepared.rows.len().saturating_sub(1);
        self.set_cursor(last, cx);
        if self.section == Section::Log {
            self.follow = true;
            cx.notify();
        }
    }

    /// Forget the retained tail; the next drain continues from where the
    /// tail was, so nothing already retained comes back.
    pub(crate) fn clear_log(&mut self, cx: &mut Context<Self>) {
        self.log.clear();
        self.rebuild(cx);
    }

    pub(crate) fn set_levels_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.levels.open != open {
            self.levels.open = open;
            cx.notify();
        }
    }

    /// Request `level` for `target` (the store's bare-suffix spelling)
    /// through the entity; the shell's drain applies and persists it.
    pub(crate) fn pick_level(&mut self, target: &str, level: Level, cx: &mut Context<Self>) {
        self.diagnostics.update(cx, |d, cx| {
            d.request_level(target, level);
            cx.notify();
        });
    }

    /// Select a section: rebuild it and show its filter text in the one
    /// input. The input's `set_value` needs the window, so every caller
    /// brings one; nothing syncs the input from render.
    pub fn set_section(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        if self.section == section {
            return;
        }
        // The new section may not paint the input (Perf) or the popover
        // (every other section): a surface dropping a focused input blurs
        // it first, so focus never rests on an unrendered handle with the
        // page still in insert mode.
        self.focus_handle.focus(window, cx);
        self.levels.open = false;
        self.section = section;
        self.title = title_for(section);
        // Setting the value emits no `Change`, so the section is not
        // rebuilt a second time.
        let text = self.filters[section as usize].clone();
        self.filter_input.update(cx, |input, cx| {
            if input.value().as_ref() != text {
                input.set_value(text, window, cx);
            }
        });
        self.rebuild(cx);
        self.request_reference(cx);
        self.sync_ages_timer(cx);
    }

    fn set_cursor(&mut self, ix: usize, cx: &mut Context<Self>) {
        let len = self.prepared.rows.len();
        let ix = ix.min(len.saturating_sub(1));
        let slot = self.section as usize;
        if self.cursors[slot] == ix {
            return;
        }
        self.cursors[slot] = ix;
        self.selected_keys[slot] = self.prepared.rows.get(ix).map(|r| r.key.clone());
        if self.section == Section::Log {
            self.follow = false;
        }
        if len > 0 {
            self.table.update(cx, |t, cx| t.set_selected_row(ix, cx));
        }
        self.refresh_copy_text();
        cx.notify();
    }

    /// One shared motion over the section's rows. Only a bare `G` in the
    /// Log section follows the tail; every other motion stops following,
    /// even one that lands on the row the cursor is on, so a reader is not
    /// dragged away by new records. The page has no column cursor, so
    /// column motions are not handled. An empty section is left alone: a
    /// following empty log keeps following through `g g`.
    fn apply_motion(&mut self, m: Motion, cx: &mut Context<Self>) -> bool {
        if !m.moves_rows() {
            return false;
        }
        if self.showing_issues() {
            self.set_diag_cursor(
                motion::row(self.diag_cursor, self.diag_prepared.rows.len(), m, false),
                cx,
            );
            return true;
        }
        let follow = self.section == Section::Log && m == Motion::Bottom(None);
        let len = self.prepared.rows.len();
        if len == 0 && !follow {
            return true;
        }
        self.set_cursor(motion::row(self.cursor(), len, m, false), cx);
        if self.section == Section::Log {
            self.follow = follow;
        }
        cx.notify();
        true
    }

    fn toggle_expansion_at_cursor(&mut self, expand: Option<bool>, cx: &mut Context<Self>) {
        if self.showing_issues() {
            return;
        }
        let Some(key) = self
            .prepared
            .parent_key_at(self.cursor())
            .map(str::to_string)
        else {
            return;
        };
        let set = match self.section {
            Section::Data => &mut self.collapsed_datasets,
            Section::Config => &mut self.collapsed_docs,
            Section::Sources | Section::Reference | Section::Log | Section::Perf => return,
        };
        let collapsed_now = set.contains(&key);
        let collapse = match expand {
            Some(e) => !e,
            None => !collapsed_now,
        };
        self.selected_keys[self.section as usize] = Some(key.clone());
        if collapse {
            set.insert(key);
        } else {
            set.remove(&key);
        }
        self.rebuild(cx);
    }

    /// The selected reference dataset, if any is declared.
    fn reference_dataset(&self, cx: &App) -> Option<String> {
        selected_reference(
            &self.diagnostics.read(cx).reference_datasets,
            self.reference_view,
        )
        .map(str::to_string)
    }

    /// Ask for the selected reference table at the frame's as-of. Only
    /// while shown on Reference: the shell also drops a request with no
    /// watcher, so a hidden page never costs a read. Callers are edges
    /// only (show, section, as-of, dataset, poll, publication), never a
    /// rebuild: a refusal rebuilds the section, and asking there would
    /// turn one refusal into a loop.
    fn request_reference(&self, cx: &mut Context<Self>) {
        if !self.visible || self.section != Section::Reference {
            return;
        }
        let Some(dataset) = self.reference_dataset(cx) else {
            return;
        };
        self.diagnostics.update(cx, |d, cx| {
            d.request_reference(&dataset);
            cx.notify();
        });
    }

    /// `r` and Poll now on Reference: poll the dataset's snapshot sources
    /// and read the table again. An unchanged poll publishes nothing, so
    /// the read is what retries a refused one.
    fn poll_reference(&self, cx: &mut Context<Self>) {
        let Some(dataset) = self.reference_dataset(cx) else {
            return;
        };
        self.diagnostics.update(cx, |d, cx| {
            d.request_poll(&dataset);
            cx.notify();
        });
        self.request_reference(cx);
    }

    /// Show the reference dataset at `ix`: a different table, so the
    /// cursor restarts at the top.
    fn set_reference_view(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix == self.reference_view {
            return;
        }
        self.reference_view = ix;
        let slot = Section::Reference as usize;
        self.cursors[slot] = 0;
        self.selected_keys[slot] = None;
        self.rebuild(cx);
        self.request_reference(cx);
    }

    /// Step the shown reference dataset, wrapping; one or none declared
    /// leaves it alone.
    fn cycle_reference_view(&mut self, backwards: bool, cx: &mut Context<Self>) {
        let len = self.diagnostics.read(cx).reference_datasets.len();
        if len < 2 {
            return;
        }
        let at = self.reference_view.min(len - 1);
        let next = if backwards {
            at.checked_sub(1).unwrap_or(len - 1)
        } else {
            (at + 1) % len
        };
        self.set_reference_view(next, cx);
    }

    fn request_catalog(&self, cx: &mut Context<Self>) {
        if matches!(self.section, Section::Sources | Section::Data) {
            self.diagnostics.update(cx, |d, cx| {
                d.request_catalog();
                cx.notify();
            });
        }
    }

    /// Collapse every dataset (`collapse = true`) or none.
    fn set_all_datasets_collapsed(&mut self, collapse: bool, cx: &mut Context<Self>) {
        self.collapsed_datasets = if collapse {
            self.diagnostics.read(cx).datasets.keys().cloned().collect()
        } else {
            BTreeSet::new()
        };
        self.rebuild(cx);
    }

    /// `mode = insert` while a page input owns focus, tracked by the input
    /// subscription's `Focus`/`Blur` events because the shell asks for the
    /// context without a `Window` (the market-data panel does the same
    /// with its editor flag). The shell's insert branch confirms with
    /// `holds_focus`.
    pub fn key_context(&self) -> KeyContext {
        // `grid` is the flag the shell's shared motion bindings are written
        // under: without it no motion key reaches the page. Their context
        // also wants `mode == normal`, so the filter keeps its keys.
        KeyContext::new("diagnostics")
            .grid()
            .pair("section", self.section.name())
            .pair("mode", if self.insert_mode { "insert" } else { "normal" })
            .counts()
    }

    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.filter_input.focus_handle(cx).is_focused(window)
    }

    /// The flag follows what the window says rather than the last event,
    /// so a blur delivered after the handle was refocused cannot clear it.
    fn sync_insert_mode(&mut self, window: &Window, cx: &mut Context<Self>) {
        let insert = self.holds_focus(window, cx);
        if self.insert_mode != insert {
            self.insert_mode = insert;
            cx.notify();
        }
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    pub fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(m) = motion::parse(action, count) {
            return self.apply_motion(m, cx);
        }
        // The shell offers `page::close` (Escape, the Back button) to the
        // page first: an open Levels popover is the topmost surface, so
        // Escape dismisses it and the page stays.
        if action.0 == "page::close" {
            if self.levels.open {
                self.set_levels_open(false, cx);
                return true;
            }
            return false;
        }
        let Some(name) = action.0.strip_prefix("diagnostics::") else {
            return false;
        };
        match name {
            "next_section" => self.set_section(self.section.next(), window, cx),
            "prev_section" => self.set_section(self.section.prev(), window, cx),
            "sources" => self.set_section(Section::Sources, window, cx),
            "data" => self.set_section(Section::Data, window, cx),
            "reference" => self.set_section(Section::Reference, window, cx),
            "config" => self.set_section(Section::Config, window, cx),
            "log" => self.set_section(Section::Log, window, cx),
            "perf" => self.set_section(Section::Perf, window, cx),
            "copy" => self.copy_details(cx),
            "reset_filters" => self.reset_filters(window, cx),
            "next_view" if self.section == Section::Config => self.cycle_config_view(false, cx),
            "prev_view" if self.section == Section::Config => self.cycle_config_view(true, cx),
            "next_view" if self.section == Section::Reference => {
                self.cycle_reference_view(false, cx)
            }
            "prev_view" if self.section == Section::Reference => {
                self.cycle_reference_view(true, cx)
            }
            "next_view" | "prev_view" => {}
            "follow" if self.section == Section::Log => self.set_follow(!self.follow, cx),
            "follow" => {}
            "refresh" if self.section == Section::Reference => self.poll_reference(cx),
            "refresh" => self.request_catalog(cx),
            "expand_all" if self.section == Section::Data => {
                self.set_all_datasets_collapsed(false, cx)
            }
            "collapse_all" if self.section == Section::Data => {
                self.set_all_datasets_collapsed(true, cx)
            }
            "expand_all" | "collapse_all" => {}
            "more_levels" if self.section == Section::Log => self.step_min_level(true, cx),
            "fewer_levels" if self.section == Section::Log => self.step_min_level(false, cx),
            "next_target" if self.section == Section::Log => self.step_target(false, cx),
            "prev_target" if self.section == Section::Log => self.step_target(true, cx),
            "clear_log" if self.section == Section::Log => self.clear_log(cx),
            // The popover's level buttons take no keyboard focus, so the
            // keyboard route is the shell's log-level chooser, which
            // requests through the same `Diagnostics` channel.
            "log_levels" if self.section == Section::Log => {
                self.set_levels_open(false, cx);
                (self.actions)(&ActionId("log::level".into()), window, cx);
            }
            "open_config_dir" if self.section == Section::Config => {
                (self.actions)(&ActionId("config::open_directory".into()), window, cx);
            }
            "more_levels" | "fewer_levels" | "next_target" | "prev_target" | "clear_log"
            | "log_levels" | "open_config_dir" => {}
            "expand" => self.toggle_expansion_at_cursor(Some(true), cx),
            "collapse" => self.toggle_expansion_at_cursor(Some(false), cx),
            "activate" => self.toggle_expansion_at_cursor(None, cx),
            // Perf paints no input: focusing its handle would leave a focused
            // handle with no element. The action is consumed and does nothing.
            "filter" if self.section == Section::Perf => {}
            "filter" => self.filter_input.update(cx, |i, cx| i.focus(window, cx)),
            "blur" => {
                if let Some(text) = self.filter_entry.take() {
                    self.filters[self.section as usize] = text.clone();
                    self.filter_input
                        .update(cx, |i, cx| i.set_value(text, window, cx));
                    self.rebuild(cx);
                }
                self.focus_handle.focus(window, cx);
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    /// Visibility drives watched demand: a watch queues the initial
    /// catalog, and the shell's unwatch cancels it when the page closes.
    /// Hiding also closes the Levels popover, so the next `mod+d` cannot
    /// reopen the page with the popover armed.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if !visible {
            self.set_levels_open(false, cx);
        }
        self.diagnostics.update(cx, |d, cx| {
            if visible {
                d.watch();
            } else {
                d.unwatch();
            }
            cx.notify();
        });
        if visible {
            self.rebuild(cx);
            self.request_reference(cx);
        }
        self.sync_ages_timer(cx);
    }

    /// Only the section is saved; filters, cursors, and expansion are
    /// transient.
    pub fn serialize(&self) -> toml::Table {
        let mut t = toml::Table::new();
        t.insert(
            "section".into(),
            toml::Value::String(self.section.name().to_string()),
        );
        t
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    /// One-second ticks while the page is visible and Sources is selected.
    /// A tick refreshes age text in place; it never runs a section builder.
    /// Dropping the task stops the loop, so a hidden page or another
    /// section costs nothing per second.
    fn sync_ages_timer(&mut self, cx: &mut Context<Self>) {
        let wanted = self.visible && self.section == Section::Sources;
        if !wanted {
            self.ages_timer = None;
            return;
        }
        if self.ages_timer.is_some() {
            return;
        }
        self.ages_timer = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AGES_TICK).await;
                if this.update(cx, |page, cx| page.tick_ages(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// The timer's tick: [`Self::tick_ages_at`] against the machine clock.
    pub(crate) fn tick_ages(&mut self, cx: &mut Context<Self>) {
        self.tick_ages_at(SystemTime::now(), cx);
    }

    /// Rewrite the Since cells from the retained `since` times as of `now`.
    /// The prepared table is cloned only when an age text changed, and the
    /// rebuild counter never moves: a tick is not a rebuild. The delegate
    /// takes the new table without a `refresh`: the columns are unchanged,
    /// and the notify repaints the rows.
    pub(crate) fn tick_ages_at(&mut self, now: SystemTime, cx: &mut Context<Self>) {
        if self.section != Section::Sources {
            return;
        }
        let fresh: Vec<(usize, SharedString)> = self
            .prepared
            .rows
            .iter()
            .zip(self.source_since.iter())
            .enumerate()
            .filter_map(|(ix, (row, since))| {
                let since = (*since)?;
                let cell = &row.cells.get(SINCE_COLUMN)?.text;
                let hms = cell
                    .split_once(SINCE_SEPARATOR)
                    .map_or(cell.as_ref(), |(h, _)| h);
                let text = format!(
                    "{hms}{SINCE_SEPARATOR}{}",
                    model::age_text(Some(since), now)
                );
                (cell.as_ref() != text).then(|| (ix, SharedString::from(text)))
            })
            .collect();
        if fresh.is_empty() {
            return;
        }
        let mut table = (*self.prepared).clone();
        for (ix, text) in fresh {
            table.rows[ix].cells[SINCE_COLUMN].text = text;
        }
        let shared = Rc::new(table);
        self.prepared = shared.clone();
        self.table.update(cx, |t, cx| {
            t.delegate_mut().set(shared);
            cx.notify();
        });
        cx.notify();
    }

    fn filter_input_el(&self) -> Input {
        Input::new(&self.filter_input)
            .small()
            .prefix(Icon::new(IconName::Search))
            .cleanable(true)
            .w(scale::design(FILTER_WIDTH))
    }

    /// Filter, status chip, Poll now, and one button per dataset when
    /// there is more than one to choose between. Every string is prepared
    /// at rebuild.
    fn render_reference_toolbar(
        &self,
        weak: WeakEntity<Self>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (text, tone) = &self.reference_status;
        let paint = chip::chip_paint(
            theme,
            if *tone == Tone::Warn {
                chip::Tone::Warning
            } else {
                chip::Tone::Neutral
            },
        );
        let poll = weak.clone();
        h_flex()
            .gap_2()
            .px_3()
            .py_2()
            .flex_wrap()
            .items_center()
            .child(self.filter_input_el())
            .child(
                div()
                    .px_1()
                    .rounded(theme.radius)
                    .text_xs()
                    .when_some(paint.fill, |el, fill| el.bg(fill))
                    .text_color(paint.text)
                    .child(text.clone()),
            )
            .child(crate::page_chrome::probed(
                "diagnostics-reference-poll",
                Button::new("diagnostics-reference-poll")
                    .ghost()
                    .small()
                    .label("Poll now")
                    .tooltip("Poll this dataset's sources and read it again (R)")
                    .on_click(move |_, window, cx| {
                        let _ = poll.update(cx, |p, cx| {
                            p.poll_reference(cx);
                            p.focus_handle.focus(window, cx);
                        });
                    }),
            ))
            .when(self.reference_views.len() > 1, |el| {
                let selected = self
                    .reference_view
                    .min(self.reference_views.len().saturating_sub(1));
                el.children(
                    self.reference_views
                        .iter()
                        .enumerate()
                        .map(|(ix, (id, label))| {
                            let pick = weak.clone();
                            let selector = id.clone();
                            div()
                                .id(id.clone())
                                .debug_selector(move || selector.to_string())
                                .child(
                                    Button::new(id.clone())
                                        .ghost()
                                        .small()
                                        .selected(ix == selected)
                                        .label(label.clone())
                                        .tooltip(
                                            "Show this dataset (Tab / Shift+Tab step datasets)",
                                        )
                                        .on_click(move |_, window, cx| {
                                            let _ = pick.update(cx, |p, cx| {
                                                p.set_reference_view(ix, cx);
                                                p.focus_handle.focus(window, cx);
                                            });
                                        }),
                                )
                        }),
                )
            })
            .into_any_element()
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let weak: WeakEntity<Self> = cx.weak_entity();
        match self.section {
            Section::Sources => h_flex()
                .gap_2()
                .px_3()
                .py_2()
                .child(self.filter_input_el())
                .into_any_element(),
            Section::Reference => self.render_reference_toolbar(weak, cx),
            Section::Log => log_view::toolbar(
                LogView {
                    levels_on: self.log_filter.levels,
                    target_select: &self.target_select,
                    filter: self.filter_input_el(),
                    follow: self.follow,
                    popover_open: self.levels.open,
                    level_rows: self.level_rows.clone(),
                },
                weak,
            ),
            Section::Data => {
                let theme = cx.theme();
                let (text, tone) = if self.catalog_matches {
                    ("Catalog up to date", chip::Tone::Neutral)
                } else {
                    ("Refreshing catalog", chip::Tone::Warning)
                };
                let paint = chip::chip_paint(theme, tone);
                let refresh = weak.clone();
                let (expand, collapse) = (weak.clone(), weak);
                h_flex()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .flex_wrap()
                    .items_center()
                    .child(self.filter_input_el())
                    .child(
                        div()
                            .px_1()
                            .rounded(theme.radius)
                            .text_xs()
                            .when_some(paint.fill, |el, fill| el.bg(fill))
                            .text_color(paint.text)
                            .child(text),
                    )
                    .child(
                        div()
                            .id("diagnostics-refresh-catalog")
                            .debug_selector(|| "diagnostics-refresh-catalog".to_string())
                            .child(
                                Button::new("diagnostics-refresh-catalog")
                                    .ghost()
                                    .small()
                                    .label("Refresh catalog")
                                    .tooltip("Refresh catalog (R)")
                                    .on_click(move |_, _window, cx| {
                                        let _ = refresh.update(cx, |p, cx| p.request_catalog(cx));
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id("diagnostics-expand-all")
                            .debug_selector(|| "diagnostics-expand-all".to_string())
                            .child(
                                Button::new("diagnostics-expand-all")
                                    .ghost()
                                    .small()
                                    .label("Expand all")
                                    .tooltip("Expand all datasets (Z Shift+R)")
                                    .on_click(move |_, _window, cx| {
                                        let _ = expand.update(cx, |p, cx| {
                                            p.set_all_datasets_collapsed(false, cx)
                                        });
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id("diagnostics-collapse-all")
                            .debug_selector(|| "diagnostics-collapse-all".to_string())
                            .child(
                                Button::new("diagnostics-collapse-all")
                                    .ghost()
                                    .small()
                                    .label("Collapse all")
                                    .tooltip("Collapse all datasets (Z Shift+M)")
                                    .on_click(move |_, _window, cx| {
                                        let _ = collapse.update(cx, |p, cx| {
                                            p.set_all_datasets_collapsed(true, cx)
                                        });
                                    }),
                            ),
                    )
                    .into_any_element()
            }
            // Config owns its view tabs and toolbar (`config_view`).
            Section::Config | Section::Perf => div().into_any_element(),
        }
    }
}

impl gpui::Render for DiagnosticsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Column widths follow the rem scale. The delegate's `column()` is
        // read only on `refresh`, so a rem change needs one; an unchanged
        // rem must not refresh per frame.
        let rem = f32::from(window.rem_size());
        if rem != self.last_rem {
            self.last_rem = rem;
            for table in [&self.table, &self.diag_table] {
                table.update(cx, |t, cx| {
                    t.delegate_mut().set_rem(rem);
                    t.refresh(cx);
                });
            }
        }
        let weak = cx.weak_entity();
        let header =
            crate::page_chrome::header(self.section, &self.header_chips, self.actions.clone(), cx);
        let rail = crate::page_chrome::rail(
            self.section,
            &self.badges,
            &self.rail_texts,
            weak.clone(),
            cx,
        );
        let toolbar = self.render_toolbar(cx);
        let body: AnyElement = match self.section {
            Section::Perf => crate::perf_view::render(self.perf.as_ref(), weak, cx),
            Section::Config => config_view::render(
                ConfigView {
                    diag_table: &self.diag_table,
                    focus: &self.focus_handle,
                    history: self.config_history,
                    values: self.config_values,
                    history_label: self.history_label.clone(),
                    table: &self.table,
                    filter: self.filter_input_el(),
                    actions: self.actions.clone(),
                    results: self.render_results(cx),
                },
                weak,
                cx,
            ),
            Section::Sources | Section::Data | Section::Reference | Section::Log => v_flex()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .child(self.render_results(cx))
                .child(table_el(&self.table, &self.focus_handle))
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .track_focus(&self.focus_handle)
            .debug_selector(|| "diagnostics-page".to_string())
            .child(header)
            // `h_flex` centers its items; the rail and the content column
            // must stretch to the row's height or the table, whose list
            // has no intrinsic height, paints no rows.
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(rail)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(toolbar)
                            .child(body)
                            .when(self.active_row().is_some(), |el| {
                                el.child(crate::page_chrome::detail_strip(
                                    if self.showing_issues() {
                                        "diagnostics-diag-detail"
                                    } else {
                                        "diagnostics-detail"
                                    },
                                    self.active_row(),
                                    self.detail_position.clone(),
                                    self.copy_text.clone(),
                                    &self.detail_scroll,
                                    cx,
                                ))
                            }),
                    ),
            )
            .child(crate::page_chrome::footer(
                self.section,
                self.insert_mode,
                self.config_values,
                cx,
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::groupings::GroupingSlots;
    use geode_core::log::LogLevels;
    use geode_core::scopes::SavedScopes;
    use geode_shell::diagnostics::Health;
    use geode_shell::frame::Frame;
    use geode_shell::tiling::WorkspaceIx;

    use crate::prepared::RowKind;

    mod keys;
    mod reference;

    struct Host {
        page: Entity<DiagnosticsPage>,
    }
    impl gpui::Render for Host {
        fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.page.clone())
        }
    }

    pub(super) struct Harness {
        pub page: Entity<DiagnosticsPage>,
        #[allow(dead_code)]
        pub frame: Entity<Frame>,
        pub diagnostics: Entity<Diagnostics>,
        pub ring: Arc<Ring>,
        pub actions: Rc<RefCell<Vec<String>>>,
    }

    pub(super) fn open(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
        open_with(cx, None)
    }

    pub(super) fn open_with(
        cx: &mut gpui::TestAppContext,
        restored: Option<&toml::Table>,
    ) -> (Harness, gpui::VisualTestContext) {
        open_with_ring(cx, restored, 64)
    }

    /// `ring_capacity` is small by default so a wrap is cheap to provoke;
    /// the timing test asks for a ring wider than the retained tail.
    fn open_with_ring(
        cx: &mut gpui::TestAppContext,
        restored: Option<&toml::Table>,
        ring_capacity: usize,
    ) -> (Harness, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        let ring = Arc::new(Ring::new(ring_capacity));
        let config = Rc::new(RefCell::new(Config::default()));
        let dispatched: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let recorder = dispatched.clone();
        let actions: ShellActions = Rc::new(move |a: &ActionId, _w: &mut Window, _cx: &mut App| {
            recorder.borrow_mut().push(a.0.clone());
        });
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    // The page is bound to unpinned workspace 1, so the shared
                    // lane is its lane and tests address it as `f.shared()` /
                    // `f.shared_mut()`.
                    let frame =
                        cx.new(|_| Frame::new(GroupingSlots::default(), SavedScopes::new(), None));
                    let diagnostics = cx.new(|_| Diagnostics::new(LogLevels::default()));
                    let (ring2, config2, actions2) =
                        (ring.clone(), config.clone(), actions.clone());
                    cx.new(|cx| {
                        let page = cx.new(|cx| {
                            DiagnosticsPage::new(
                                FrameRef::new(frame.clone(), WorkspaceIx::FIRST),
                                diagnostics.clone(),
                                ring2,
                                config2,
                                actions2,
                                restored,
                                window,
                                cx,
                            )
                        });
                        Host { page }
                    })
                })
            })
            .unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let page = window
            .root(&mut vcx)
            .unwrap()
            .read_with(&vcx, |h, _| h.page.clone());
        let (frame, diagnostics) = page.read_with(&vcx, |p, _| {
            (p.frame.entity().clone(), p.diagnostics.clone())
        });
        // Focus-in and focus-out reach their listeners only in an active
        // window, as a shown window is; the test platform activates on
        // its executor, so it is parked before the first draw.
        vcx.update(|window, _cx| window.activate_window());
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (
            Harness {
                page,
                frame,
                diagnostics,
                ring,
                actions: dispatched,
            },
            vcx,
        )
    }

    #[gpui::test]
    fn visibility_watches_and_requests_a_catalog(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
        assert_eq!(h.diagnostics.read_with(&vcx, |d, _| d.watchers()), 1);
        assert!(
            h.diagnostics
                .update(&mut vcx, |d, _| d.take_pending_catalog_request())
        );
        h.page.update(&mut vcx, |p, cx| p.set_visible(false, cx));
        assert_eq!(h.diagnostics.read_with(&vcx, |d, _| d.watchers()), 0);
    }

    #[gpui::test]
    fn sections_cycle_with_the_bracket_actions_and_persist(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        assert_eq!(h.page.read_with(&vcx, |p, _| p.section()), Section::Sources);
        vcx.update(|window, cx| {
            h.page.update(cx, |p, cx| {
                assert!(p.dispatch(
                    &ActionId("diagnostics::next_section".into()),
                    None,
                    window,
                    cx
                ));
            });
        });
        assert_eq!(h.page.read_with(&vcx, |p, _| p.section()), Section::Data);
        let t = h.page.read_with(&vcx, |p, _| p.serialize());
        assert_eq!(t.get("section").and_then(|v| v.as_str()), Some("data"));
        assert!(t.get("filter").is_none(), "filters are transient");
    }

    /// `g r` dispatches `diagnostics::reference`; the section sits between
    /// Data and Config in the rail and persists by name.
    #[gpui::test]
    fn the_reference_action_selects_the_reference_section(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        vcx.update(|window, cx| {
            h.page.update(cx, |p, cx| {
                assert!(p.dispatch(&ActionId("diagnostics::reference".into()), None, window, cx));
            });
        });
        assert_eq!(
            h.page.read_with(&vcx, |p, _| p.section()),
            Section::Reference
        );
        let t = h.page.read_with(&vcx, |p, _| p.serialize());
        assert_eq!(t.get("section").and_then(|v| v.as_str()), Some("reference"));
        let (h, vcx) = open_with(cx, Some(&t));
        assert_eq!(
            h.page.read_with(&vcx, |p, _| p.section()),
            Section::Reference,
            "a saved reference section restores"
        );
    }

    #[gpui::test]
    fn an_unknown_saved_section_restores_as_sources(cx: &mut gpui::TestAppContext) {
        let mut t = toml::Table::new();
        t.insert("section".into(), toml::Value::String("database".into()));
        let (h, vcx) = open_with(cx, Some(&t));
        assert_eq!(h.page.read_with(&vcx, |p, _| p.section()), Section::Sources);
    }

    #[gpui::test]
    fn an_unchanged_entity_does_not_rebuild(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
        let before = h.page.read_with(&vcx, |p, _| p.rebuild_count);
        h.diagnostics.update(&mut vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.rebuild_count), before);
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.note_health("risk", Health::Ok, String::new(), SystemTime::now());
            cx.notify();
        });
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.rebuild_count), before + 1);
        // A perf tick does not rebuild the Sources section.
        h.diagnostics.update(&mut vcx, |d, cx| {
            let mut hist = geode_shell::perf::FrameHistogram::new();
            hist.record_micros(1);
            d.refresh_frame_hist(&hist);
            cx.notify();
        });
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.rebuild_count), before + 1);
    }

    /// The perf section repaints when the refusal counter moves.
    #[gpui::test]
    fn a_refused_request_reaches_the_perf_row(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
        dispatch(&h, &mut vcx, "diagnostics::perf");
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.note_refused(4);
            cx.notify();
        });
        vcx.run_until_parked();
        let row = h.page.read_with(&vcx, |p, _| {
            p.perf
                .as_ref()
                .unwrap()
                .resource("Refused requests")
                .map(str::to_string)
        });
        assert_eq!(row.as_deref(), Some("4"));
    }

    /// A closed page lives on for the window: its observers move their
    /// baselines and build nothing until it is shown, and the show rebuilds
    /// exactly once with everything that arrived meanwhile.
    #[gpui::test]
    fn a_hidden_page_neither_rebuilds_nor_refreshes_badges_until_shown(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
        h.page.update(&mut vcx, |p, cx| p.set_visible(false, cx));
        let (before, texts) = h
            .page
            .read_with(&vcx, |p, _| (p.rebuild_count, p.rail_texts.clone()));
        assert_eq!(h.page.read_with(&vcx, |p, _| p.prepared().rows.len()), 0);
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.note_health("risk", Health::Ok, String::new(), SystemTime::now());
            cx.notify();
        });
        h.frame.update(&mut vcx, |f, cx| {
            f.note_config_reloaded();
            cx.notify();
        });
        vcx.run_until_parked();
        h.page.read_with(&vcx, |p, _| {
            assert_eq!(p.rebuild_count, before, "hidden: no section builder ran");
            assert_eq!(p.rail_texts, texts, "hidden: no badge refresh");
            assert_eq!(p.prepared().rows.len(), 0);
        });
        h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
        h.page.read_with(&vcx, |p, _| {
            assert_eq!(p.rebuild_count, before + 1, "shown: rebuilt once");
            assert!(
                p.prepared()
                    .rows
                    .iter()
                    .any(|r| r.cells[0].text.as_ref() == "risk"),
                "the source noted while hidden is in the shown table"
            );
            assert_ne!(p.rail_texts, texts, "the badges caught up on the show");
        });
        // The baselines moved while hidden: a notify with nothing new
        // after the show rebuilds nothing.
        h.diagnostics.update(&mut vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.rebuild_count), before + 1);
        // The frame observer the same way: a config bump is relevant to
        // the Config section, and still builds nothing while hidden.
        open_config_section(&h, &mut vcx);
        h.page.update(&mut vcx, |p, cx| p.set_visible(false, cx));
        let before = h.page.read_with(&vcx, |p, _| p.rebuild_count);
        h.frame.update(&mut vcx, |f, cx| {
            f.note_config_reloaded();
            cx.notify();
        });
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.rebuild_count), before);
        h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
        assert_eq!(h.page.read_with(&vcx, |p, _| p.rebuild_count), before + 1);
        h.frame.update(&mut vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.rebuild_count), before + 1);
    }

    /// The Log rail badge counts errors in the tail whatever section is
    /// selected: the tail must be drained for the badge, not only for the
    /// Log table.
    #[gpui::test]
    fn the_log_rail_badge_counts_errors_while_another_section_is_selected(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
        assert_eq!(h.page.read_with(&vcx, |p, _| p.section()), Section::Sources);
        push(&h.ring, Level::ERROR, "geode::shell", "boom");
        notify(&h, &mut vcx);
        assert_eq!(
            h.page
                .read_with(&vcx, |p, _| p.rail_texts[Section::Log as usize].clone())
                .as_ref(),
            "err 1"
        );
    }

    #[gpui::test]
    fn the_open_config_directory_button_goes_through_the_shell_actions_handle(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        vcx.update(|window, cx| {
            h.page
                .update(cx, |p, cx| p.set_section(Section::Config, window, cx));
            let _ = window.draw(cx);
        });
        let b = vcx
            .debug_bounds("diagnostics-open-config-dir")
            .expect("button painted");
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        assert_eq!(
            *h.actions.borrow(),
            vec!["config::open_directory".to_string()]
        );
    }

    /// A catalog answer for `as_of` holding `datasets`, with the resource
    /// figures zero: the data tests read only the as-of and the datasets.
    fn snapshot_with(
        as_of: geode_core::query::AsOf,
        datasets: Vec<geode_core::query::DatasetCatalog>,
    ) -> geode_core::query::CatalogSnapshot {
        geode_core::query::CatalogSnapshot {
            as_of,
            datasets,
            database_bytes: 0,
            used_blocks: 0,
            block_size: 0,
            memory_bytes: 0,
            memory_limit_bytes: 0,
            temp_bytes: 0,
            memory_top: Vec::new(),
            threads: 1,
            identities: Vec::new(),
        }
    }

    /// A platform-shaped double-click: down/up at `click_count` 1, then
    /// down/up at `click_count` 2, with a draw between them as the OS
    /// delivers them across frames. The second click is the one the
    /// table's row handler reads as a double-click.
    fn double_click(cx: &mut gpui::VisualTestContext, at: gpui::Point<gpui::Pixels>) {
        for count in 1..=2 {
            cx.update(|window, cx| {
                window.dispatch_event(
                    gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                        button: gpui::MouseButton::Left,
                        position: at,
                        modifiers: gpui::Modifiers::default(),
                        click_count: count,
                        first_mouse: false,
                    }),
                    cx,
                );
                window.dispatch_event(
                    gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                        button: gpui::MouseButton::Left,
                        position: at,
                        modifiers: gpui::Modifiers::default(),
                        click_count: count,
                    }),
                    cx,
                );
            });
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }
    }

    /// Move the page's frame lane to `as_of`, as the as-of picker does,
    /// and let the page's frame observer run.
    fn set_frame_as_of(
        h: &Harness,
        vcx: &mut gpui::VisualTestContext,
        as_of: geode_core::query::AsOf,
    ) {
        h.frame.update(vcx, |f, cx| {
            let _ = f.shared_mut().set_as_of(as_of);
            cx.notify();
        });
        vcx.run_until_parked();
    }

    fn open_data_section(h: &Harness, vcx: &mut gpui::VisualTestContext) {
        vcx.update(|window, cx| {
            h.page
                .update(cx, |p, cx| p.set_section(Section::Data, window, cx));
            let _ = window.draw(cx);
        });
    }

    /// An as-of change requests a watched catalog; until the answer for
    /// that as-of arrives no generation is marked resolved, because a
    /// catalog under another as-of would mark the wrong one.
    #[gpui::test]
    fn resolved_markers_wait_for_a_matching_catalog(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
        open_data_section(&h, &mut vcx);
        // Drain the watch's initial request so the next one is the as-of's.
        assert!(
            h.diagnostics
                .update(&mut vcx, |d, _| d.take_pending_catalog_request())
        );
        let at = geode_core::query::AsOf::At(chrono::Utc::now());
        set_frame_as_of(&h, &mut vcx, at.clone());
        // A watched refresh was requested for the new as-of.
        assert!(
            h.diagnostics
                .update(&mut vcx, |d, _| d.take_pending_catalog_request())
        );
        // Catalog for another as-of: no markers.
        let other = geode_core::query::AsOf::At(chrono::Utc::now() + chrono::Duration::hours(2));
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.set_catalog(
                snapshot_with(other, vec![crate::model::tests::dataset_catalog()]),
                SystemTime::now(),
            );
            cx.notify();
        });
        vcx.run_until_parked();
        assert_eq!(
            h.page.read_with(&vcx, |p, _| p.prepared().rows.len()),
            3,
            "the mismatched catalog still lists the dataset and its generations"
        );
        assert!(h.page.read_with(&vcx, |p, _| {
            p.prepared().rows.iter().all(|r| r.tone != Tone::Marked)
        }));
        assert!(!h.page.read_with(&vcx, |p, _| p.catalog_matches));
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.set_catalog(
                snapshot_with(at, vec![crate::model::tests::dataset_catalog()]),
                SystemTime::now(),
            );
            cx.notify();
        });
        vcx.run_until_parked();
        assert_eq!(
            h.page.read_with(&vcx, |p, _| p
                .prepared()
                .rows
                .iter()
                .filter(|r| r.tone == Tone::Marked)
                .count()),
            1
        );
        assert!(h.page.read_with(&vcx, |p, _| p.catalog_matches));
    }

    /// A single click on a dataset row only selects it; `activate` and a
    /// double-click toggle its generations; the toolbar buttons expand or
    /// collapse every dataset at once.
    #[gpui::test]
    fn clicking_a_dataset_row_then_enter_collapses_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.set_catalog(
                snapshot_with(
                    geode_core::query::AsOf::Live,
                    vec![crate::model::tests::dataset_catalog()],
                ),
                SystemTime::now(),
            );
            cx.notify();
        });
        open_data_section(&h, &mut vcx);
        let rows =
            |vcx: &gpui::VisualTestContext| h.page.read_with(vcx, |p, _| p.prepared().rows.len());
        assert_eq!(rows(&vcx), 3, "parent + 2 generations");
        // Park the cursor on a child, then single-click the parent: the
        // cursor moves and nothing collapses.
        vcx.update(|window, cx| {
            h.page.update(cx, |p, cx| {
                let _ = p.dispatch(&ActionId("motion::bottom".into()), None, window, cx);
            });
        });
        assert_eq!(h.page.read_with(&vcx, |p, _| p.cursor()), 2);
        let row = vcx
            .debug_bounds("diagnostics-row-0")
            .expect("the dataset row is painted");
        let at = gpui::point(row.origin.x + gpui::px(4.0), row.center().y);
        vcx.simulate_click(at, gpui::Modifiers::default());
        vcx.run_until_parked();
        assert_eq!(
            h.page.read_with(&vcx, |p, _| p.cursor()),
            0,
            "a click selects"
        );
        assert_eq!(rows(&vcx), 3, "and collapses nothing");
        vcx.update(|window, cx| {
            h.page.update(cx, |p, cx| {
                let _ = p.dispatch(&ActionId("diagnostics::activate".into()), None, window, cx);
            });
        });
        assert_eq!(rows(&vcx), 1, "enter collapses the parent under the cursor");
        // Collapse-all / expand-all buttons.
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let b = vcx.debug_bounds("diagnostics-expand-all").unwrap();
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        assert_eq!(rows(&vcx), 3);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let b = vcx.debug_bounds("diagnostics-collapse-all").unwrap();
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        assert_eq!(rows(&vcx), 1);
        // A double-click on the parent row expands it again, and a second
        // one collapses it.
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let row = vcx.debug_bounds("diagnostics-row-0").unwrap();
        let at = gpui::point(row.origin.x + gpui::px(4.0), row.center().y);
        double_click(&mut vcx, at);
        vcx.run_until_parked();
        assert_eq!(rows(&vcx), 3, "a double-click expands");
        double_click(&mut vcx, at);
        vcx.run_until_parked();
        assert_eq!(rows(&vcx), 1, "and the next collapses");
    }

    /// Focus the page's own handle and deliver the focus events, as the
    /// shell does on open.
    fn focus_page(h: &Harness, vcx: &mut gpui::VisualTestContext) {
        vcx.update(|window, cx| {
            h.page.read(cx).focus_handle().focus(window, cx);
            let _ = window.draw(cx);
        });
    }

    /// A row click selects through the table without moving window focus
    /// into it: the table's own `escape` would otherwise take the first
    /// Escape after a click to clear its selection, and only the second
    /// would close the page.
    #[gpui::test]
    fn a_row_click_selects_without_taking_focus_from_the_page(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.set_catalog(
                snapshot_with(
                    geode_core::query::AsOf::Live,
                    vec![crate::model::tests::dataset_catalog()],
                ),
                SystemTime::now(),
            );
            cx.notify();
        });
        open_data_section(&h, &mut vcx);
        focus_page(&h, &mut vcx);
        dispatch(&h, &mut vcx, "motion::bottom");
        assert_eq!(h.page.read_with(&vcx, |p, _| p.cursor()), 2);
        let row = vcx
            .debug_bounds("diagnostics-row-0")
            .expect("the dataset row is painted");
        let at = gpui::point(row.origin.x + gpui::px(4.0), row.center().y);
        vcx.simulate_click(at, gpui::Modifiers::default());
        vcx.run_until_parked();
        assert_eq!(
            h.page.read_with(&vcx, |p, _| p.cursor()),
            0,
            "the table's SelectRow still moved the cursor"
        );
        vcx.update(|window, cx| {
            let page = h.page.read(cx);
            assert!(
                !page.table.read(cx).focus_handle(cx).is_focused(window),
                "the table did not take focus"
            );
            assert!(
                page.focus_handle.is_focused(window),
                "the page handle kept it"
            );
        });
    }

    fn open_config_section(h: &Harness, vcx: &mut gpui::VisualTestContext) {
        vcx.update(|window, cx| {
            h.page
                .update(cx, |p, cx| p.set_section(Section::Config, window, cx));
            let _ = window.draw(cx);
        });
    }

    /// Two config batches five seconds apart: the second is current, the
    /// first is history.
    fn note_two_config_batches(h: &Harness, vcx: &mut gpui::VisualTestContext) {
        use geode_core::config::{Diagnostic, Severity};
        let t0 = SystemTime::now();
        h.diagnostics.update(vcx, |d, cx| {
            d.note_config(
                vec![Diagnostic {
                    severity: Severity::Error,
                    layer: None,
                    file: Some("views.toml".into()),
                    message: "first".into(),
                    path: Some("blotter.columns.4".into()),
                }],
                t0,
            );
            d.note_config(
                vec![Diagnostic {
                    severity: Severity::Warning,
                    layer: None,
                    file: None,
                    message: "second".into(),
                    path: None,
                }],
                t0 + std::time::Duration::from_secs(5),
            );
            cx.notify();
        });
    }

    /// The Config issues view lists the current batch; the
    /// History button switches it to the prior batches, batch column
    /// first, through the pointer route.
    #[gpui::test]
    fn the_config_section_paints_current_diagnostics_and_switches_to_history(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
        note_two_config_batches(&h, &mut vcx);
        open_config_section(&h, &mut vcx);
        let current = h.page.read_with(&vcx, |p, cx| {
            p.diag_table.read(cx).delegate().table().clone()
        });
        assert_eq!(current.rows.len(), 1);
        assert_eq!(current.rows[0].cells[3].text.as_ref(), "second");
        let label = |vcx: &gpui::VisualTestContext| {
            h.page
                .read_with(vcx, |p, _| p.history_label.clone())
                .to_string()
        };
        assert_eq!(
            label(&vcx),
            "History (1 batch)",
            "the current batch is not history"
        );
        // A third batch: two prior ones, plural.
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.note_config(Vec::new(), SystemTime::now() + Duration::from_secs(10));
            cx.notify();
        });
        vcx.run_until_parked();
        assert_eq!(label(&vcx), "History (2 batches)");
        let current = h.page.read_with(&vcx, |p, cx| {
            p.diag_table.read(cx).delegate().table().clone()
        });
        assert!(current.rows.is_empty(), "a clean load is the current batch");
        let b = vcx.debug_bounds("diagnostics-diag-history").unwrap();
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        let history = h.page.read_with(&vcx, |p, cx| {
            p.diag_table.read(cx).delegate().table().clone()
        });
        assert_eq!(history.columns.len(), 5, "batch column first");
        // Newest prior batch first: "second", then "first".
        assert_eq!(history.rows[0].cells[4].text.as_ref(), "second");
        assert_eq!(history.rows[1].cells[4].text.as_ref(), "first");
        assert_eq!(
            history.rows[1].cells[3].text.as_ref(),
            "views.toml › blotter.columns.4"
        );
        // Current again through its own button.
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let b = vcx.debug_bounds("diagnostics-diag-current").unwrap();
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        let current = h.page.read_with(&vcx, |p, cx| {
            p.diag_table.read(cx).delegate().table().clone()
        });
        assert_eq!(current.columns.len(), 4);
        assert!(current.rows.is_empty());
    }

    /// Pointer selection and motions both move the visible issues cursor;
    /// the retained effective-values cursor is independent.
    #[gpui::test]
    fn clicking_a_diagnostics_panel_row_moves_only_its_own_detail(cx: &mut gpui::TestAppContext) {
        use geode_core::config::{Diagnostic, Severity};
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| {
            let diag = |m: &str| Diagnostic {
                severity: Severity::Warning,
                layer: None,
                file: None,
                message: m.into(),
                path: None,
            };
            d.note_config(vec![diag("one"), diag("two")], SystemTime::now());
            cx.notify();
        });
        open_config_section(&h, &mut vcx);
        focus_page(&h, &mut vcx);
        vcx.update(|window, cx| {
            h.page.update(cx, |p, cx| {
                let _ = p.dispatch(&ActionId("motion::down".into()), None, window, cx);
            });
        });
        let before = h.page.read_with(&vcx, |p, _| p.cursor());
        assert_eq!(h.page.read_with(&vcx, |p, _| p.diag_cursor), 1);
        let row = vcx
            .debug_bounds("diagnostics-diag-row-0")
            .expect("the first issue is visible");
        let at = gpui::point(row.origin.x + gpui::px(4.0), row.center().y);
        vcx.simulate_click(at, gpui::Modifiers::default());
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.diag_cursor), 0);
        vcx.update(|window, cx| {
            let page = h.page.read(cx);
            assert!(
                !page.diag_table.read(cx).focus_handle(cx).is_focused(window),
                "the table did not take focus"
            );
            assert!(page.focus_handle.is_focused(window));
        });
        assert_eq!(
            h.page.read_with(&vcx, |p, _| p.cursor()),
            before,
            "the effective-values cursor is untouched"
        );
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            vcx.debug_bounds("diagnostics-diag-detail").is_some(),
            "the issues view paints its detail strip"
        );
    }

    /// Only the Config section reads the loaded config, so only it
    /// rebuilds on a config version bump.
    #[gpui::test]
    fn a_config_version_change_rebuilds_only_the_config_section(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
        open_config_section(&h, &mut vcx);
        let before = h.page.read_with(&vcx, |p, _| p.rebuild_count);
        h.frame.update(&mut vcx, |f, cx| {
            f.note_config_reloaded();
            cx.notify();
        });
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.rebuild_count), before + 1);
        vcx.update(|window, cx| {
            h.page
                .update(cx, |p, cx| p.set_section(Section::Sources, window, cx));
        });
        let before = h.page.read_with(&vcx, |p, _| p.rebuild_count);
        h.frame.update(&mut vcx, |f, cx| {
            f.note_config_reloaded();
            cx.notify();
        });
        vcx.run_until_parked();
        assert_eq!(
            h.page.read_with(&vcx, |p, _| p.rebuild_count),
            before,
            "sources ignore a config bump"
        );
    }

    fn push(ring: &Ring, level: geode_core::log::Level, target: &'static str, msg: &str) {
        ring.push(geode_core::log::Record {
            at: SystemTime::now(),
            level,
            target,
            message: msg.into(),
            seq: 0,
        });
    }

    /// A visible page on the Log section, painted once: the shell shows a
    /// page before it takes keys, and a hidden page drains nothing.
    fn open_log_section(h: &Harness, vcx: &mut gpui::VisualTestContext) {
        vcx.update(|window, cx| {
            h.page.update(cx, |p, cx| {
                p.set_visible(true, cx);
                p.set_section(Section::Log, window, cx);
            });
            let _ = window.draw(cx);
        });
    }

    /// Dispatch one action by its full id, uncounted; it must be consumed.
    fn dispatch(h: &Harness, vcx: &mut gpui::VisualTestContext, id: &str) {
        let id = ActionId(id.into());
        vcx.update(|window, cx| {
            h.page.update(cx, |p, cx| {
                assert!(p.dispatch(&id, None, window, cx));
            });
        });
    }

    fn notify(h: &Harness, vcx: &mut gpui::VisualTestContext) {
        h.diagnostics.update(vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
    }

    fn click(vcx: &mut gpui::VisualTestContext, selector: &'static str) {
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let b = vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is painted"));
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
    }

    #[gpui::test]
    fn the_log_section_follows_until_the_cursor_moves_and_bottom_resumes(
        cx: &mut gpui::TestAppContext,
    ) {
        use geode_core::log::Level;
        let (h, mut vcx) = open(cx);
        open_log_section(&h, &mut vcx);
        for i in 0..5 {
            push(&h.ring, Level::INFO, "geode::shell", &format!("m{i}"));
        }
        notify(&h, &mut vcx);
        assert_eq!(h.page.read_with(&vcx, |p, _| p.cursor()), 4);
        dispatch(&h, &mut vcx, "motion::up");
        push(&h.ring, Level::INFO, "geode::shell", "m5");
        notify(&h, &mut vcx);
        assert_eq!(
            h.page.read_with(&vcx, |p, _| p.cursor()),
            3,
            "not following"
        );
        dispatch(&h, &mut vcx, "motion::bottom");
        push(&h.ring, Level::INFO, "geode::shell", "m6");
        notify(&h, &mut vcx);
        assert_eq!(
            h.page.read_with(&vcx, |p, _| p.cursor()),
            6,
            "following again"
        );
        // The Follow switch is the pointer route to the same state.
        dispatch(&h, &mut vcx, "motion::top");
        assert_eq!(
            h.page.read_with(&vcx, |p, _| (p.cursor(), p.follow)),
            (0, false)
        );
        click(&mut vcx, "diagnostics-follow");
        assert_eq!(
            h.page.read_with(&vcx, |p, _| (p.cursor(), p.follow)),
            (6, true),
            "turning follow on jumps to the last row"
        );
        // A row click seats the cursor through the table and stops
        // following like a motion does.
        click(&mut vcx, "diagnostics-row-0");
        vcx.run_until_parked();
        assert_eq!(
            h.page.read_with(&vcx, |p, _| (p.cursor(), p.follow)),
            (0, false),
            "a click stops following"
        );
    }

    /// Dispatch one action by its full id with a count; it must be consumed.
    fn dispatch_counted(h: &Harness, vcx: &mut gpui::VisualTestContext, id: &str, count: u32) {
        let id = ActionId(id.into());
        vcx.update(|window, cx| {
            h.page.update(cx, |p, cx| {
                assert!(p.dispatch(&id, Some(count), window, cx));
            });
        });
    }

    /// The page publishes `grid` beside its mode: the shell's shared motion
    /// bindings are written under `grid && (mode == normal || …)`, so they
    /// reach the cursor in normal mode and stay out while the filter holds
    /// focus, where the context reads `mode == insert`.
    #[gpui::test]
    fn the_key_context_publishes_grid_and_normal_mode(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        open_log_section(&h, &mut vcx);
        let ctx = h.page.read_with(&vcx, |p, _| p.key_context());
        assert!(ctx.has_flag(geode_shell::keymap::GRID));
        assert_eq!(ctx.get("mode"), Some("normal"));
        dispatch(&h, &mut vcx, "diagnostics::filter");
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let ctx = h.page.read_with(&vcx, |p, _| p.key_context());
        assert!(
            ctx.has_flag(geode_shell::keymap::GRID),
            "the mode is the gate, not the flag"
        );
        assert_eq!(ctx.get("mode"), Some("insert"));
    }

    /// A bare `G` follows the log; a counted `G` jumps to that row without
    /// following, even onto the row the cursor is on; a bare `j` at the
    /// last row wraps and stops following; on an empty log a motion changes
    /// nothing, so following survives a `g g` there.
    #[gpui::test]
    fn bare_g_follows_the_log_and_a_counted_g_only_jumps(cx: &mut gpui::TestAppContext) {
        use geode_core::log::Level;
        let (h, mut vcx) = open(cx);
        open_log_section(&h, &mut vcx);
        let state =
            |vcx: &gpui::VisualTestContext| h.page.read_with(vcx, |p, _| (p.cursor(), p.follow));
        assert_eq!(state(&vcx), (0, true), "fixture: an empty log follows");
        dispatch(&h, &mut vcx, "motion::top");
        assert_eq!(
            state(&vcx),
            (0, true),
            "an empty section keeps following through g g"
        );
        for i in 0..6 {
            push(&h.ring, Level::INFO, "geode::shell", &format!("m{i}"));
        }
        notify(&h, &mut vcx);
        assert_eq!(state(&vcx), (5, true), "fixture: the tail is followed");
        dispatch_counted(&h, &mut vcx, "motion::bottom", 3);
        assert_eq!(
            state(&vcx),
            (2, false),
            "3G: row 3, 1-based, and a counted G does not follow"
        );
        dispatch(&h, &mut vcx, "motion::bottom");
        assert_eq!(state(&vcx), (5, true), "a bare G follows");
        dispatch_counted(&h, &mut vcx, "motion::bottom", 6);
        assert_eq!(
            state(&vcx),
            (5, false),
            "6G onto the cursor row still stops following"
        );
        dispatch(&h, &mut vcx, "motion::bottom");
        assert_eq!(state(&vcx), (5, true));
        dispatch(&h, &mut vcx, "motion::down");
        assert_eq!(
            state(&vcx),
            (0, false),
            "a bare j at the last row wraps and stops following"
        );
        push(&h.ring, Level::INFO, "geode::shell", "m6");
        notify(&h, &mut vcx);
        assert_eq!(state(&vcx), (0, false), "not following");
    }

    /// `3 ctrl+d` moves fifteen rows: the count reaches the shared
    /// half-page step through the page's dispatch.
    #[gpui::test]
    fn a_count_prefix_multiplies_page_down(cx: &mut gpui::TestAppContext) {
        use geode_core::log::Level;
        let (h, mut vcx) = open(cx);
        open_log_section(&h, &mut vcx);
        for i in 0..40 {
            push(&h.ring, Level::INFO, "geode::shell", &format!("m{i}"));
        }
        notify(&h, &mut vcx);
        dispatch(&h, &mut vcx, "motion::top");
        dispatch_counted(&h, &mut vcx, "motion::half_page_down", 3);
        assert_eq!(
            h.page.read_with(&vcx, |p, _| p.cursor()),
            15,
            "3 ctrl+d must move 5 * 3 = 15 rows"
        );
    }

    /// `ctrl+f`/`ctrl+b` (`motion::page_down`/`page_up`) are the ten-row
    /// step, counted the same way `ctrl+d` is: `2 ctrl+f` moves 20 and
    /// `ctrl+b` brings back 10.
    #[gpui::test]
    fn ctrl_f_and_ctrl_b_page_by_ten(cx: &mut gpui::TestAppContext) {
        use geode_core::log::Level;
        let (h, mut vcx) = open(cx);
        open_log_section(&h, &mut vcx);
        for i in 0..40 {
            push(&h.ring, Level::INFO, "geode::shell", &format!("m{i}"));
        }
        notify(&h, &mut vcx);
        dispatch(&h, &mut vcx, "motion::top");
        dispatch_counted(&h, &mut vcx, "motion::page_down", 2);
        assert_eq!(
            h.page.read_with(&vcx, |p, _| p.cursor()),
            20,
            "2 ctrl+f must move 10 * 2 = 20 rows"
        );
        dispatch(&h, &mut vcx, "motion::page_up");
        assert_eq!(
            h.page.read_with(&vcx, |p, _| p.cursor()),
            10,
            "ctrl+b must move back 10 rows"
        );
    }

    #[gpui::test]
    fn level_toggles_and_the_target_select_filter_the_tail(cx: &mut gpui::TestAppContext) {
        use geode_core::log::Level;
        let (h, mut vcx) = open(cx);
        open_log_section(&h, &mut vcx);
        push(&h.ring, Level::DEBUG, "geode::query", "planned");
        push(&h.ring, Level::ERROR, "geode::shell", "boom");
        notify(&h, &mut vcx);
        let rows =
            |vcx: &gpui::VisualTestContext| h.page.read_with(vcx, |p, _| p.prepared().rows.len());
        assert_eq!(rows(&vcx), 2);
        click(&mut vcx, "diagnostics-level-DEBUG");
        assert_eq!(rows(&vcx), 1);
        h.page.update(&mut vcx, |p, cx| {
            p.set_log_target(Some("geode::query".into()), cx)
        });
        assert_eq!(rows(&vcx), 0, "DEBUG off and only query");
        let shown = |vcx: &gpui::VisualTestContext| {
            h.page.read_with(vcx, |p, cx| {
                p.target_select.read(cx).selected_value().cloned()
            })
        };
        vcx.run_until_parked();
        assert_eq!(
            shown(&vcx).as_deref(),
            Some("geode::query"),
            "a programmatic target shows in the select although the items did not change"
        );
        click(&mut vcx, "diagnostics-level-DEBUG");
        assert_eq!(rows(&vcx), 1, "DEBUG back on, still only query");
        // A target outside the tail is still offered, and selected, by the
        // select once its items are refreshed.
        h.page.update(&mut vcx, |p, cx| {
            p.set_log_target(Some("geode::ingest".into()), cx)
        });
        vcx.run_until_parked();
        assert_eq!(
            h.page
                .read_with(&vcx, |p, cx| p
                    .target_select
                    .read(cx)
                    .selected_value()
                    .cloned())
                .as_deref(),
            Some("geode::ingest")
        );
        h.page.update(&mut vcx, |p, cx| p.set_log_target(None, cx));
        assert_eq!(rows(&vcx), 2);
        vcx.run_until_parked();
        assert_eq!(shown(&vcx).as_deref(), Some(ALL_TARGETS));
    }

    #[gpui::test]
    fn a_wrap_while_closed_is_reported_on_the_next_drain(cx: &mut gpui::TestAppContext) {
        use geode_core::log::Level;
        let (h, mut vcx) = open(cx); // ring capacity 64
        open_log_section(&h, &mut vcx);
        h.page.update(&mut vcx, |p, cx| p.set_visible(false, cx));
        for i in 0..100 {
            push(&h.ring, Level::WARN, "geode::ingest", &format!("w{i}"));
        }
        // A hidden page ignores the entity's notifications: nothing drains
        // until it is shown again.
        notify(&h, &mut vcx);
        assert_eq!(h.page.read_with(&vcx, |p, _| p.prepared().rows.len()), 0);
        h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
        let rows = h.page.read_with(&vcx, |p, _| p.prepared().clone());
        assert!(matches!(rows.rows[0].kind, RowKind::Notice));
        assert!(
            rows.rows[0].cells[0].text.contains("36 records lost"),
            "{}",
            rows.rows[0].cells[0].text
        );
        assert_eq!(rows.rows.len(), 65);
    }

    #[gpui::test]
    fn clear_drops_the_retained_tail_and_keeps_draining(cx: &mut gpui::TestAppContext) {
        use geode_core::log::Level;
        let (h, mut vcx) = open(cx);
        open_log_section(&h, &mut vcx);
        push(&h.ring, Level::INFO, "geode::shell", "a");
        notify(&h, &mut vcx);
        click(&mut vcx, "diagnostics-log-clear");
        assert_eq!(h.page.read_with(&vcx, |p, _| p.prepared().rows.len()), 0);
        push(&h.ring, Level::INFO, "geode::shell", "b");
        notify(&h, &mut vcx);
        assert_eq!(h.page.read_with(&vcx, |p, _| p.prepared().rows.len()), 1);
    }

    #[gpui::test]
    fn a_levels_pick_requests_the_level_through_the_entity(cx: &mut gpui::TestAppContext) {
        use geode_core::log::Level;
        let (h, mut vcx) = open(cx);
        open_log_section(&h, &mut vcx);
        click(&mut vcx, "diagnostics-levels-open");
        assert!(h.page.read_with(&vcx, |p, _| p.levels.open));
        click(&mut vcx, "diagnostics-level-pick-ingest-debug");
        let pending = h
            .diagnostics
            .update(&mut vcx, |d, _| d.take_pending_level());
        // The store's spelling: the bare suffix `LogLevels::with` keeps.
        assert_eq!(pending, Some(("ingest".to_string(), Level::DEBUG)));
        assert_eq!(
            h.diagnostics
                .read_with(&vcx, |d, _| crate::levels::effective_level(
                    &d.levels, "ingest"
                )),
            Level::DEBUG
        );
        // The popover stays open for the next pick, and the rebuilt rows
        // show the pick.
        vcx.run_until_parked();
        assert!(h.page.read_with(&vcx, |p, _| p.levels.open));
        let ingest = h.page.read_with(&vcx, |p, _| {
            p.level_rows
                .iter()
                .find(|r| r.target == "ingest")
                .map(|r| (r.effective, r.explicit))
        });
        assert_eq!(ingest, Some((Level::DEBUG, true)));
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            vcx.debug_bounds("diagnostics-level-pick-ingest-trace")
                .is_some()
        );
    }

    /// Perf paints no input: leaving Log with the filter focused would
    /// park focus on an unrendered handle and keep the page in insert mode.
    #[gpui::test]
    fn switching_to_a_section_without_the_input_blurs_it_first(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        open_log_section(&h, &mut vcx);
        dispatch(&h, &mut vcx, "diagnostics::filter");
        // The input's focus event, which sets insert mode, is delivered
        // with the next frame.
        let draw = |vcx: &mut gpui::VisualTestContext| {
            vcx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        };
        draw(&mut vcx);
        vcx.update(|window, cx| {
            assert!(h.page.read(cx).holds_focus(window, cx));
            assert_eq!(h.page.read(cx).key_context().get("mode"), Some("insert"));
            h.page
                .update(cx, |p, cx| p.set_section(Section::Perf, window, cx));
        });
        draw(&mut vcx);
        vcx.update(|window, cx| {
            let page = h.page.read(cx);
            assert!(!page.holds_focus(window, cx));
            assert!(
                page.focus_handle.is_focused(window),
                "the page handle took focus"
            );
            assert_eq!(page.key_context().get("mode"), Some("normal"));
        });
    }

    /// The popover belongs to the Log toolbar; a section that does not
    /// paint it cannot leave it open for the return trip.
    #[gpui::test]
    fn leaving_the_log_section_closes_the_levels_popover(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        open_log_section(&h, &mut vcx);
        click(&mut vcx, "diagnostics-levels-open");
        assert!(h.page.read_with(&vcx, |p, _| p.levels.open));
        vcx.update(|window, cx| {
            h.page.update(cx, |p, cx| {
                p.set_section(Section::Sources, window, cx);
                p.set_section(Section::Log, window, cx);
            });
            let _ = window.draw(cx);
        });
        assert!(!h.page.read_with(&vcx, |p, _| p.levels.open));
        assert!(
            vcx.debug_bounds("diagnostics-level-pick-ingest-debug")
                .is_none(),
            "no popover content is painted"
        );
        // Closing the page (mod+d) with the popover open must not reopen
        // the page with it armed.
        click(&mut vcx, "diagnostics-levels-open");
        assert!(h.page.read_with(&vcx, |p, _| p.levels.open));
        h.page.update(&mut vcx, |p, cx| p.set_visible(false, cx));
        assert!(
            !h.page.read_with(&vcx, |p, _| p.levels.open),
            "hiding closes the popover"
        );
    }

    #[gpui::test]
    fn the_copy_button_puts_the_cursor_row_on_the_clipboard(cx: &mut gpui::TestAppContext) {
        use geode_core::log::Level;
        let (h, mut vcx) = open(cx);
        open_log_section(&h, &mut vcx);
        push(&h.ring, Level::WARN, "geode::ingest", "stale partition");
        notify(&h, &mut vcx);
        click(&mut vcx, "diagnostics-copy");
        let text = vcx.read_from_clipboard().and_then(|i| i.text());
        assert!(
            text.as_deref()
                .is_some_and(|t| t.contains("WARN geode::ingest stale partition")),
            "{text:?}"
        );
    }

    #[gpui::test]
    fn the_refresh_button_records_an_explicit_catalog_request(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        open_data_section(&h, &mut vcx);
        assert_eq!(
            h.diagnostics
                .update(&mut vcx, |d, _| d.take_catalog_request()),
            None,
            "a hidden page has requested nothing"
        );
        let b = vcx.debug_bounds("diagnostics-refresh-catalog").unwrap();
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        assert_eq!(
            h.diagnostics
                .update(&mut vcx, |d, _| d.take_catalog_request()),
            Some(geode_shell::diagnostics::CatalogRequest::Explicit)
        );
    }

    #[gpui::test]
    fn the_overlay_switch_flips_through_the_entity_channel(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        vcx.update(|window, cx| {
            h.page.update(cx, |p, cx| {
                p.set_visible(true, cx);
                p.set_section(Section::Perf, window, cx);
            });
            let _ = window.draw(cx);
        });
        let b = vcx.debug_bounds("diagnostics-overlay-switch").unwrap();
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        assert!(
            h.diagnostics
                .update(&mut vcx, |d, _| d.take_pending_overlay_toggle())
        );
        // The shell mirrors the value back; the page repaints on the perf
        // counter.
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.set_overlay_visible(true);
            cx.notify();
        });
        vcx.run_until_parked();
        assert!(
            h.page
                .read_with(&vcx, |p, _| p.perf.as_ref().unwrap().is_overlay_visible())
        );
    }

    #[gpui::test]
    fn the_ages_timer_runs_only_while_visible_on_sources(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        assert!(
            h.page.read_with(&vcx, |p, _| p.ages_timer.is_none()),
            "hidden: no timer"
        );
        h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
        assert!(h.page.read_with(&vcx, |p, _| p.ages_timer.is_some()));
        vcx.update(|window, cx| {
            h.page
                .update(cx, |p, cx| p.set_section(Section::Log, window, cx));
        });
        assert!(
            h.page.read_with(&vcx, |p, _| p.ages_timer.is_none()),
            "log: no timer"
        );
        vcx.update(|window, cx| {
            h.page
                .update(cx, |p, cx| p.set_section(Section::Sources, window, cx));
        });
        assert!(h.page.read_with(&vcx, |p, _| p.ages_timer.is_some()));
        // The loop reaches `tick_ages`: a retained `since` moved back is
        // repainted after one tick of the executor's clock. The loop reads
        // the machine clock, so the assertion is that the cell changed, not
        // what it changed to (`a_tick_refreshes_ages_without_rebuilding_rows`
        // pins the text against an explicit `now`).
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.note_health("s", Health::Ok, String::new(), SystemTime::now());
            cx.notify();
        });
        vcx.run_until_parked();
        let since_cell = |vcx: &gpui::VisualTestContext| {
            h.page.read_with(vcx, |p, _| {
                (
                    Rc::as_ptr(p.prepared()),
                    p.prepared().rows[0].cells[SINCE_COLUMN].text.clone(),
                )
            })
        };
        let (table_before, text_before) = since_cell(&vcx);
        h.page.update(&mut vcx, |p, _| {
            p.source_since[0] = Some(SystemTime::now() - Duration::from_secs(3_600));
        });
        let before = h.page.read_with(&vcx, |p, _| p.rebuild_count);
        vcx.executor().advance_clock(AGES_TICK);
        vcx.run_until_parked();
        let (table_after, text_after) = since_cell(&vcx);
        assert_ne!(table_after, table_before, "the tick published a table");
        assert_ne!(text_after, text_before, "{text_after}");
        assert_eq!(h.page.read_with(&vcx, |p, _| p.rebuild_count), before);
        h.page.update(&mut vcx, |p, cx| p.set_visible(false, cx));
        assert!(h.page.read_with(&vcx, |p, _| p.ages_timer.is_none()));
    }

    /// Ticks against an explicit `now`, so the reading does not depend on
    /// how long the test took between the health note and the tick.
    #[gpui::test]
    fn a_tick_refreshes_ages_without_rebuilding_rows(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        let reported_at = SystemTime::now();
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.note_health("s", Health::Ok, String::new(), reported_at);
            cx.notify();
        });
        h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
        let before = h.page.read_with(&vcx, |p, _| p.rebuild_count);
        let since_cell = |vcx: &gpui::VisualTestContext| {
            h.page.read_with(vcx, |p, _| {
                p.prepared().rows[0].cells[SINCE_COLUMN].text.clone()
            })
        };
        let hms = since_cell(&vcx);
        let hms = hms.split_once(SINCE_SEPARATOR).map(|(h, _)| h.to_string());
        assert!(hms.is_some(), "clock text then age");
        // Thirty seconds on: only the age is rewritten; the clock text stays.
        let now = reported_at + Duration::from_secs(30);
        h.page.update(&mut vcx, |p, cx| p.tick_ages_at(now, cx));
        assert_eq!(h.page.read_with(&vcx, |p, _| p.rebuild_count), before);
        assert_eq!(
            since_cell(&vcx).as_ref(),
            format!("{}{SINCE_SEPARATOR}30 s", hms.as_ref().unwrap())
        );
        // The same instant again: an unchanged age publishes nothing, so
        // the same table stays shared.
        let table_before = h.page.read_with(&vcx, |p, _| Rc::as_ptr(p.prepared()));
        h.page.update(&mut vcx, |p, cx| p.tick_ages_at(now, cx));
        assert_eq!(h.page.read_with(&vcx, |p, _| p.rebuild_count), before);
        assert_eq!(
            h.page.read_with(&vcx, |p, _| Rc::as_ptr(p.prepared())),
            table_before
        );
        // Four minutes on: the age scales without touching the clock text.
        h.page.update(&mut vcx, |p, cx| {
            p.tick_ages_at(reported_at + Duration::from_secs(240), cx)
        });
        assert_eq!(
            since_cell(&vcx).as_ref(),
            format!("{}{SINCE_SEPARATOR}4 m", hms.unwrap())
        );
        // The table entity paints the rewritten cell, not the stale one.
        let painted = h.page.read_with(&vcx, |p, cx| {
            p.table.read(cx).delegate().table().rows[0].cells[SINCE_COLUMN]
                .text
                .clone()
        });
        assert!(painted.ends_with(" · 4 m"), "{painted}");
    }

    /// The headless measurements recorded in `docs/current/performance.md`,
    /// over a full 4,096-record tail every query below keeps whole. Per
    /// query: a keystroke (the query changes: the synchronous `rebuild`
    /// that shows the held answer and starts the narrowing), a settled
    /// rebuild (records or gates change under a narrowed query), and the
    /// narrowing pass itself, which runs off the UI thread. Not a painted
    /// frame. Run with
    /// `cargo test -p geode-diagnostics --release -- --ignored log_rebuild_timing --nocapture`.
    #[gpui::test]
    #[ignore]
    fn log_rebuild_timing_over_a_full_tail(cx: &mut gpui::TestAppContext) {
        use geode_core::log::Level;
        let (h, mut vcx) = open_with_ring(cx, None, 8_192);
        open_log_section(&h, &mut vcx);
        for i in 0..crate::log::LOG_CAP {
            let level = match i % 5 {
                0 => Level::ERROR,
                1 => Level::WARN,
                2 => Level::DEBUG,
                _ => Level::INFO,
            };
            push(
                &h.ring,
                level,
                "geode::ingest",
                &format!(
                    "record {i}: partition 2026-09-27 · EU_TECH loaded {} rows in {} ms",
                    i * 13,
                    i % 97
                ),
            );
        }
        notify(&h, &mut vcx);
        assert_eq!(
            h.page.read_with(&vcx, |p, _| p.prepared().rows.len()),
            crate::log::LOG_CAP
        );
        const RUNS: usize = 20;
        let median = |mut samples: Vec<std::time::Duration>| {
            samples.sort();
            (samples[samples.len() / 2], *samples.last().unwrap())
        };
        let build = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        for query in [
            "",
            "eutch ld",
            "partition loaded rows",
            "record partition loaded rows ms",
            "partition2026loaded",
        ] {
            let mut keystroke = Vec::with_capacity(RUNS);
            for _ in 0..RUNS {
                // From the empty query, so each run is a changed query.
                h.page.update(&mut vcx, |p, cx| {
                    p.filters[Section::Log as usize].clear();
                    p.rebuild(cx);
                });
                vcx.run_until_parked();
                // Timed inside the update: the rebuild, not GPUI's flush.
                keystroke.push(h.page.update(&mut vcx, |p, cx| {
                    p.filters[Section::Log as usize] = query.to_string();
                    let started = std::time::Instant::now();
                    p.rebuild(cx);
                    started.elapsed()
                }));
                vcx.run_until_parked();
            }
            assert_eq!(
                h.page.read_with(&vcx, |p, _| p.prepared().rows.len()),
                crate::log::LOG_CAP,
                "every record matches {query:?}"
            );
            let mut settled = Vec::with_capacity(RUNS);
            for _ in 0..RUNS {
                settled.push(h.page.update(&mut vcx, |p, cx| {
                    let started = std::time::Instant::now();
                    p.rebuild(cx);
                    started.elapsed()
                }));
            }
            let entries = h.page.read_with(&vcx, |p, _| p.log_cache.after(None));
            let mut pass = Vec::with_capacity(RUNS);
            for _ in 0..RUNS {
                let started = std::time::Instant::now();
                std::hint::black_box(Narrowed::run(query, &entries));
                pass.push(started.elapsed());
            }
            let (k, k_max) = median(keystroke);
            let (s, s_max) = median(settled);
            let (n, n_max) = median(pass);
            eprintln!(
                "log over {} records, filter {query:?}: keystroke median {k:?} (max {k_max:?}), \
                 settled rebuild median {s:?} (max {s_max:?}), off-thread narrowing median \
                 {n:?} (max {n_max:?}); {RUNS} runs, headless, {build} build",
                crate::log::LOG_CAP,
            );
        }
    }
    #[gpui::test]
    fn configuration_views_route_motion_and_copy_to_the_visible_table(
        cx: &mut gpui::TestAppContext,
    ) {
        use geode_core::config::{ConfigSources, Diagnostic, LayerDoc, Severity};
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, _| {
            *p.config.borrow_mut() = Config::load(&ConfigSources {
                builtin: vec![LayerDoc::builtin("app", "name = \"example\"\n").unwrap()],
                ..Default::default()
            });
        });
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.note_config(
                ["first issue", "second issue"]
                    .into_iter()
                    .map(|message| Diagnostic {
                        severity: Severity::Warning,
                        layer: None,
                        file: None,
                        path: None,
                        message: message.into(),
                    })
                    .collect(),
                SystemTime::now(),
            );
            cx.notify();
        });
        open_config_section(&h, &mut vcx);
        dispatch(&h, &mut vcx, "motion::down");
        dispatch(&h, &mut vcx, "diagnostics::copy");
        assert!(
            vcx.read(|cx| cx.read_from_clipboard().unwrap().text().unwrap())
                .contains("second issue")
        );
        click(&mut vcx, "diagnostics-config-values");
        assert!(vcx.debug_bounds("diagnostics-diag-row-0").is_none());
        assert!(vcx.debug_bounds("diagnostics-row-0").is_some());
        dispatch(&h, &mut vcx, "motion::down");
        dispatch(&h, &mut vcx, "diagnostics::copy");
        assert!(
            vcx.read(|cx| cx.read_from_clipboard().unwrap().text().unwrap())
                .contains("example")
        );
        dispatch(&h, &mut vcx, "diagnostics::prev_view");
        assert!(
            h.page
                .read_with(&vcx, |p, _| p.config_history && !p.config_values)
        );
        dispatch(&h, &mut vcx, "diagnostics::next_view");
        assert!(h.page.read_with(&vcx, |p, _| p.config_values));
        assert_eq!(
            h.page.read_with(&vcx, |p, _| p.cursor()),
            1,
            "returning to values retains its cursor"
        );
    }

    #[gpui::test]
    fn filtering_keeps_the_selected_record_and_enter_returns_to_navigation(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        open_log_section(&h, &mut vcx);
        for message in ["noise", "keep first", "keep selected", "keep later"] {
            push(&h.ring, Level::INFO, "geode::shell", message);
        }
        notify(&h, &mut vcx);
        dispatch(&h, &mut vcx, "diagnostics::follow");
        dispatch(&h, &mut vcx, "motion::up");
        let selected = h
            .page
            .read_with(&vcx, |p, _| p.active_row().unwrap().key.clone());
        dispatch(&h, &mut vcx, "diagnostics::filter");
        vcx.simulate_input("keep");
        vcx.run_until_parked();
        assert_eq!(
            h.page
                .read_with(&vcx, |p, _| p.active_row().unwrap().key.clone()),
            selected
        );
        assert_eq!(h.page.read_with(&vcx, |p, _| p.cursor()), 1);
        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();
        assert!(!h.page.read_with(&vcx, |p, _| p.insert_mode));
        assert_eq!(
            h.page
                .read_with(&vcx, |p, _| p.filters[Section::Log as usize].clone()),
            "keep"
        );
        dispatch(&h, &mut vcx, "diagnostics::filter");
        vcx.simulate_input(" unmatched");
        vcx.run_until_parked();
        assert!(h.page.read_with(&vcx, |p, _| p.prepared.rows.is_empty()));
        assert!(vcx.debug_bounds("diagnostics-empty").is_some());
        dispatch(&h, &mut vcx, "diagnostics::blur");
        assert_eq!(
            h.page
                .read_with(&vcx, |p, _| p.filters[Section::Log as usize].clone()),
            "keep"
        );
        assert!(!h.page.read_with(&vcx, |p, _| p.insert_mode));
    }

    /// Typing into the filter narrows fuzzily, keeps the tail's order, and
    /// hands the delegate cells already marked: the matches are prepared
    /// with the table, never computed in paint.
    #[gpui::test]
    fn typing_a_fuzzy_filter_narrows_in_order_and_marks_the_painted_cells(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        open_log_section(&h, &mut vcx);
        push(&h.ring, Level::INFO, "geode::ingest", "partition loaded");
        push(&h.ring, Level::WARN, "geode::shell", "slow paint");
        push(
            &h.ring,
            Level::INFO,
            "geode::ingest",
            "Partition LOADED again",
        );
        notify(&h, &mut vcx);
        dispatch(&h, &mut vcx, "diagnostics::filter");
        vcx.simulate_input("INGST ptn");
        vcx.run_until_parked();
        let painted = h.page.read_with(&vcx, |p, cx| {
            assert!(Rc::ptr_eq(p.table.read(cx).delegate().table(), &p.prepared));
            p.prepared.clone()
        });
        let messages: Vec<&str> = painted
            .rows
            .iter()
            .map(|r| r.cells[3].text.as_ref())
            .collect();
        assert_eq!(messages, ["partition loaded", "Partition LOADED again"]);
        for row in &painted.rows {
            let target = &row.cells[2];
            let marked: Vec<&str> = target
                .marks
                .iter()
                .map(|r| &target.text[r.clone()])
                .collect();
            assert_eq!(marked, ["ing", "st"]);
            assert!(!row.cells[3].marks.is_empty(), "ptn lands in the message");
            assert!(row.cells[0].marks.is_empty() && row.cells[1].marks.is_empty());
        }
        assert!(
            vcx.debug_bounds("diagnostics-row-1").is_some(),
            "both rows paint"
        );
        vcx.simulate_keystrokes(&["backspace"; 9].join(" "));
        vcx.run_until_parked();
        h.page.read_with(&vcx, |p, _| {
            assert_eq!(p.prepared.rows.len(), 3);
            assert!(
                p.prepared
                    .rows
                    .iter()
                    .all(|r| r.cells.iter().all(|c| c.marks.is_empty())),
                "a cleared filter marks nothing"
            );
        });
    }

    /// Records that arrive under a filter are narrowed on their own and
    /// appended to the held answer; a finished pass for a query the input
    /// no longer holds is dropped.
    #[gpui::test]
    fn records_arriving_under_a_filter_are_narrowed_and_stale_passes_dropped(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        open_log_section(&h, &mut vcx);
        push(&h.ring, Level::INFO, "geode::ingest", "partition loaded");
        notify(&h, &mut vcx);
        dispatch(&h, &mut vcx, "diagnostics::filter");
        vcx.simulate_input("ptn");
        vcx.run_until_parked();
        let messages = |vcx: &gpui::VisualTestContext| {
            h.page.read_with(vcx, |p, _| {
                p.prepared
                    .rows
                    .iter()
                    .map(|r| r.cells[3].text.to_string())
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(messages(&vcx), ["partition loaded"]);
        push(&h.ring, Level::INFO, "geode::shell", "noise");
        push(&h.ring, Level::INFO, "geode::ingest", "partition again");
        notify(&h, &mut vcx);
        vcx.run_until_parked();
        assert_eq!(messages(&vcx), ["partition loaded", "partition again"]);
        h.page.read_with(&vcx, |p, _| {
            let held = p.log_narrowed.as_ref().expect("a held narrowing");
            assert_eq!(held.query(), "ptn");
            assert_eq!(held.through(), p.log_cache.last_seq(), "it covers the tail");
            assert!(p.log_narrowing.is_none(), "nothing in flight");
        });
        h.page.update(&mut vcx, |p, cx| {
            let stale = Narrowed::run("zz", &p.log_cache.after(None));
            p.apply_log_narrowing(stale, cx);
        });
        assert_eq!(
            messages(&vcx),
            ["partition loaded", "partition again"],
            "a pass for another query changes nothing"
        );
    }

    /// The table a fresh, whole-tail narrowing of the page's cache would
    /// show: what the held answer must equal once it settles.
    fn fresh_log_rows(
        h: &Harness,
        vcx: &gpui::VisualTestContext,
    ) -> Vec<(String, Vec<Vec<std::ops::Range<usize>>>)> {
        let rows = |t: &PreparedTable| {
            t.rows
                .iter()
                .map(|r| {
                    let marks = r.cells.iter().map(|c| c.marks.clone()).collect();
                    (r.key.clone(), marks)
                })
                .collect::<Vec<_>>()
        };
        h.page.read_with(vcx, |p, _| {
            let fresh = (!p.log_filter.text.trim().is_empty())
                .then(|| Narrowed::run(&p.log_filter.text, &p.log_cache.after(None)));
            let fresh =
                log_cache::log_table(&p.log_cache, &p.log_filter, fresh.as_ref(), p.log.lost());
            assert_eq!(
                rows(&p.prepared),
                rows(&fresh),
                "the shown table is settled"
            );
            rows(&fresh)
        })
    }

    /// A clock change reformats every time cell, so an answer that matched
    /// the old time text is dropped and the query narrowed again. The
    /// record's time is fixed: 06:08:46 UTC is 11:38:46 in Kolkata, where
    /// `06:08` is no longer a subsequence.
    #[gpui::test]
    fn a_clock_change_drops_a_narrowing_of_the_old_time_text(cx: &mut gpui::TestAppContext) {
        use geode_core::clock::Clock;
        let (h, mut vcx) = open(cx);
        vcx.update(|_, cx| cx.set_global(geode_shell::clock::AppClock(Clock::utc())));
        open_log_section(&h, &mut vcx);
        h.ring.push(geode_core::log::Record {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(6 * 3600 + 8 * 60 + 46),
            level: Level::INFO,
            target: "geode::ingest",
            message: "partition loaded".into(),
            seq: 0,
        });
        notify(&h, &mut vcx);
        h.page.update(&mut vcx, |p, cx| {
            p.filters[Section::Log as usize] = "06:08".into();
            p.rebuild(cx);
        });
        vcx.run_until_parked();
        let rows = fresh_log_rows(&h, &vcx);
        assert_eq!(rows.len(), 1, "the UTC time matches");
        assert_eq!(rows[0].1[0], vec![0..5]);
        vcx.update(|_, cx| {
            cx.set_global(geode_shell::clock::AppClock(Clock::in_zone_named(
                "Asia/Kolkata",
            )))
        });
        vcx.run_until_parked();
        assert_eq!(
            h.page
                .read_with(&vcx, |p, _| p.log_cache.after(None)[0].texts_for_test()[0]
                    .to_string()),
            "11:38:46.000"
        );
        assert!(
            fresh_log_rows(&h, &vcx).is_empty(),
            "the old answer must not hold over the new time text"
        );
    }

    /// A changed query's pass survives records arriving while it runs: it
    /// is not restarted per arrival. When it lands, one pass over just the
    /// newcomers follows, and the table equals a fresh narrowing.
    #[gpui::test]
    fn a_pass_in_flight_survives_arrivals_and_an_extension_follows(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_with_ring(cx, None, 512);
        open_log_section(&h, &mut vcx);
        for i in 0..200 {
            push(
                &h.ring,
                Level::INFO,
                "geode::ingest",
                &format!("rec {i} partition"),
            );
        }
        notify(&h, &mut vcx);
        let before = h.page.read_with(&vcx, |p, _| p.log_passes);
        h.page.update(&mut vcx, |p, cx| {
            p.filters[Section::Log as usize] = "ptn".into();
            p.rebuild(cx);
        });
        for i in 0..5 {
            push(
                &h.ring,
                Level::INFO,
                "geode::ingest",
                &format!("late {i} partition"),
            );
            h.page.update(&mut vcx, |p, cx| p.rebuild(cx));
        }
        h.page.read_with(&vcx, |p, _| {
            assert_eq!(p.log_passes, before + 1, "one full pass, never restarted");
            assert!(p.log_narrowing.is_some());
            assert!(p.log_narrowed.is_none(), "no answer yet: every row shows");
        });
        vcx.run_until_parked();
        h.page.read_with(&vcx, |p, _| {
            assert_eq!(p.log_passes, before + 2, "then one extension");
            assert!(p.log_narrowing.is_none());
            assert_eq!(
                p.log_narrowed.as_ref().and_then(Narrowed::through),
                p.log_cache.last_seq()
            );
        });
        assert_eq!(fresh_log_rows(&h, &vcx).len(), 205);
    }

    /// A blank query narrows nothing, so it leaves collapsed documents
    /// collapsed; a real query reveals its matches inside them.
    #[gpui::test]
    fn a_blank_filter_leaves_collapsed_documents_collapsed(cx: &mut gpui::TestAppContext) {
        use geode_core::config::{ConfigSources, LayerDoc};
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, _| {
            *p.config.borrow_mut() = Config::load(&ConfigSources {
                builtin: vec![LayerDoc::builtin("app", "name = \"example\"\n").unwrap()],
                ..Default::default()
            });
            p.collapsed_docs.insert("app".into());
        });
        open_config_section(&h, &mut vcx);
        click(&mut vcx, "diagnostics-config-values");
        let parents = |vcx: &gpui::VisualTestContext| {
            h.page.read_with(vcx, |p, _| {
                p.prepared
                    .rows
                    .iter()
                    .filter_map(|r| match r.kind {
                        RowKind::Parent { expanded } => Some(expanded),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(parents(&vcx), [false]);
        dispatch(&h, &mut vcx, "diagnostics::filter");
        vcx.simulate_input("  ");
        vcx.run_until_parked();
        assert_eq!(
            h.page
                .read_with(&vcx, |p, _| p.filters[Section::Config as usize].clone()),
            "  "
        );
        assert_eq!(parents(&vcx), [false], "spaces force nothing open");
        vcx.simulate_input("exa");
        vcx.run_until_parked();
        assert_eq!(parents(&vcx), [true], "a match is revealed");
    }

    #[gpui::test]
    fn clicking_a_filtered_row_returns_focus_to_navigation(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        open_log_section(&h, &mut vcx);
        push(&h.ring, Level::INFO, "geode::shell", "matching record");
        notify(&h, &mut vcx);
        dispatch(&h, &mut vcx, "diagnostics::filter");
        vcx.simulate_input("matching");
        vcx.run_until_parked();
        assert!(h.page.read_with(&vcx, |p, _| p.insert_mode));
        let row = vcx.debug_bounds("diagnostics-row-0").unwrap();
        vcx.simulate_click(row.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
        assert!(!h.page.read_with(&vcx, |p, _| p.insert_mode));
        vcx.update(|window, cx| assert!(h.page.read(cx).focus_handle.is_focused(window)));
    }

    #[gpui::test]
    fn data_filter_input_reveals_collapsed_leaves_and_reset_restores_expansion(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
        h.diagnostics.update(&mut vcx, |d, cx| {
            let risk = crate::model::tests::dataset_catalog();
            let mut vol = risk.clone();
            vol.name = "vol".into();
            vol.partitions[0].book = Some("US_BANKS".into());
            d.set_catalog(
                snapshot_with(geode_core::query::AsOf::Live, vec![risk, vol]),
                SystemTime::now(),
            );
            cx.notify();
        });
        open_data_section(&h, &mut vcx);
        click(&mut vcx, "diagnostics-collapse-all");
        assert_eq!(h.page.read_with(&vcx, |p, _| p.prepared.rows.len()), 2);
        dispatch(&h, &mut vcx, "diagnostics::filter");
        vcx.simulate_input("eu_tech");
        vcx.run_until_parked();
        h.page.read_with(&vcx, |p, _| {
            assert_eq!(p.result_summary.as_ref(), "1 of 2 datasets · 3 rows shown");
            assert_eq!(p.prepared.rows[0].key, "risk");
            assert_eq!(p.prepared.rows[0].kind, RowKind::Parent { expanded: true });
            assert!(
                p.prepared.rows[1..]
                    .iter()
                    .all(|r| r.cells[0].text.contains("EU_TECH"))
            );
            assert_eq!(
                p.collapsed_datasets,
                BTreeSet::from(["risk".into(), "vol".into()])
            );
        });
        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();
        assert!(!h.page.read_with(&vcx, |p, _| p.insert_mode));
        dispatch(&h, &mut vcx, "motion::down");
        dispatch(&h, &mut vcx, "diagnostics::copy");
        let copied = vcx.read(|cx| cx.read_from_clipboard().unwrap().text().unwrap());
        assert!(copied.contains("gen 1") && copied.contains("archive"));
        click(&mut vcx, "diagnostics-reset-filters");
        h.page.read_with(&vcx, |p, _| {
            assert_eq!(p.result_summary.as_ref(), "2 of 2 datasets · 2 rows shown");
            assert!(
                p.prepared
                    .rows
                    .iter()
                    .all(|r| r.kind == RowKind::Parent { expanded: false })
            );
        });
        vcx.update(|window, cx| assert!(h.page.read(cx).focus_handle.is_focused(window)));
    }

    #[gpui::test]
    fn reset_filters_restores_the_log_and_keeps_other_sections_filtered(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        focus_page(&h, &mut vcx);
        dispatch(&h, &mut vcx, "diagnostics::filter");
        vcx.simulate_input("source query");
        vcx.run_until_parked();
        open_log_section(&h, &mut vcx);
        push(&h.ring, Level::INFO, "geode::shell", "keep");
        push(&h.ring, Level::DEBUG, "geode::query", "hidden");
        notify(&h, &mut vcx);
        click(&mut vcx, "diagnostics-level-DEBUG");
        h.page.update(&mut vcx, |p, cx| {
            p.set_log_target(Some("geode::shell".into()), cx)
        });
        dispatch(&h, &mut vcx, "diagnostics::filter");
        vcx.simulate_input("unmatched");
        vcx.run_until_parked();
        assert!(h.page.read_with(&vcx, |p, _| {
            p.result_summary.starts_with("0 of 2 retained records")
        }));
        assert!(vcx.debug_bounds("diagnostics-empty").is_some());
        click(&mut vcx, "diagnostics-reset-filters");
        h.page.read_with(&vcx, |p, cx| {
            assert!(!p.has_filters());
            assert_eq!(p.log_filter, LogFilter::all());
            assert_eq!(
                p.target_select
                    .read(cx)
                    .selected_value()
                    .as_ref()
                    .map(|v| v.as_ref()),
                Some(ALL_TARGETS)
            );
            assert_eq!(p.filters[Section::Sources as usize], "source query");
            assert!(p.filter_input.read(cx).value().is_empty());
            assert!(p.result_summary.starts_with("2 of 2 retained records"));
            assert_eq!(p.detail_position.as_ref(), "Details · row 2 of 2");
            assert!(!p.insert_mode);
        });
        vcx.update(|window, cx| assert!(h.page.read(cx).focus_handle.is_focused(window)));
        assert!(vcx.debug_bounds("diagnostics-reset-filters").is_none());
        dispatch(&h, &mut vcx, "diagnostics::sources");
        dispatch(&h, &mut vcx, "diagnostics::reset_filters");
        assert!(
            h.page
                .read_with(&vcx, |p, _| p.filters.iter().all(String::is_empty))
        );
    }

    #[gpui::test]
    fn result_counts_follow_config_views_and_exclude_log_loss_notices(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.page.update(&mut vcx, |p, cx| p.set_visible(true, cx));
        note_two_config_batches(&h, &mut vcx);
        open_config_section(&h, &mut vcx);
        let summary = |vcx: &gpui::VisualTestContext| {
            h.page.read_with(vcx, |p, _| p.result_summary.to_string())
        };
        assert_eq!(summary(&vcx), "1 of 1 current issue");
        click(&mut vcx, "diagnostics-diag-history");
        assert_eq!(summary(&vcx), "1 of 1 historical issue");
        dispatch(&h, &mut vcx, "diagnostics::filter");
        vcx.simulate_input("unmatched");
        vcx.run_until_parked();
        assert_eq!(summary(&vcx), "0 of 1 historical issue");
        click(&mut vcx, "diagnostics-config-values");
        assert_eq!(summary(&vcx), "0 documents · 0 values shown");
        open_log_section(&h, &mut vcx);
        for _ in 0..70 {
            push(&h.ring, Level::INFO, "geode::shell", "message");
        }
        notify(&h, &mut vcx);
        assert_eq!(h.page.read_with(&vcx, |p, _| p.prepared.rows.len()), 65);
        assert_eq!(summary(&vcx), "64 of 64 retained records · limit 4096");
    }

    #[gpui::test]
    fn performance_columns_stay_aligned_at_larger_text_sizes(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        dispatch(&h, &mut vcx, "diagnostics::perf");
        for rem in [12., 20.] {
            for width in [800., 1280.] {
                vcx.simulate_resize(gpui::size(gpui::px(width), gpui::px(800.)));
                vcx.update(|window, cx| {
                    window.set_rem_size(gpui::px(rem));
                    window.refresh();
                    let _ = window.draw(cx);
                });
                let page = vcx.debug_bounds("diagnostics-performance").unwrap();
                let overlay = vcx.debug_bounds("diagnostics-overlay-switch").unwrap();
                assert!(overlay.left() >= page.left() && overlay.right() <= page.right());
                let mut previous_right = page.left();
                for label in ["Median (p50)", "p95", "Maximum", "Samples"] {
                    let frame = vcx
                        .debug_bounds(format!("diagnostics-metric-Frame interval-{label}").leak())
                        .unwrap();
                    assert!(frame.left() >= previous_right && frame.right() <= page.right());
                    assert!(frame.size.width > gpui::px(60.));
                    for metric in ["Query → snapshot", "Snapshot → paint"] {
                        let row = vcx
                            .debug_bounds(format!("diagnostics-metric-{metric}-{label}").leak())
                            .unwrap();
                        assert_eq!(frame.right(), row.right());
                        assert_eq!(frame.left(), row.left());
                    }
                    previous_right = frame.right();
                }
            }
        }
    }

    #[gpui::test]
    fn diagnostics_regions_remain_aligned_when_resized_and_zoomed(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        open_log_section(&h, &mut vcx);
        push(
            &h.ring,
            Level::ERROR,
            "geode::shell",
            &"A long diagnostic message with context. ".repeat(40),
        );
        notify(&h, &mut vcx);
        for rem in [12., 16., 20.] {
            for (width, height) in [(800., 600.), (1280., 800.)] {
                vcx.simulate_resize(gpui::size(gpui::px(width), gpui::px(height)));
                vcx.update(|window, cx| {
                    window.set_rem_size(gpui::px(rem));
                    window.refresh();
                    let _ = window.draw(cx);
                });
                let page = vcx.debug_bounds("diagnostics-page").unwrap();
                let header = vcx.debug_bounds("diagnostics-header").unwrap();
                let table = vcx.debug_bounds("diagnostics-table").unwrap();
                let detail = vcx.debug_bounds("diagnostics-detail").unwrap();
                let footer = vcx.debug_bounds("diagnostics-footer").unwrap();
                assert_eq!(table.left(), detail.left());
                assert_eq!(table.right(), detail.right());
                assert!(table.top() >= header.bottom());
                assert!(
                    table.size.height > gpui::px(40.),
                    "rows remain usable at {width}×{height}, rem {rem}: {table:?}"
                );
                assert!(detail.bottom() <= footer.top() + gpui::px(1.));
                assert!(footer.bottom() <= page.bottom() + gpui::px(1.));
                let results = vcx.debug_bounds("diagnostics-results").unwrap();
                assert_eq!(results.left(), table.left());
                assert_eq!(results.right(), table.right());
                assert!(results.bottom() <= table.top());
                let clear = vcx.debug_bounds("diagnostics-log-clear").unwrap();
                assert!(
                    clear.right() <= page.right(),
                    "toolbar actions stay inside the page"
                );
                let copy = vcx.debug_bounds("diagnostics-copy").unwrap();
                assert!(copy.right() <= detail.right());
            }
        }
        dispatch(&h, &mut vcx, "diagnostics::copy");
        let text = vcx.read(|cx| cx.read_from_clipboard().unwrap().text().unwrap());
        assert!(text.contains(&"A long diagnostic message with context. ".repeat(40)));
    }
}
