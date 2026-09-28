//! The diagnostics page entity: one section at a time over the shell's
//! `Diagnostics` entity, the log ring, the loaded config, and the frame's
//! requery stats. Observers rebuild only the selected section from its own
//! inputs; the table paints a shared prepared table; the detail strip shows
//! the cursor row.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::SystemTime;

use geode_core::config::Config;
use geode_core::log::Ring;
use geode_shell::actions::ActionId;
use geode_shell::diagnostics::{DiagVersions, Diagnostics};
use geode_shell::frame::{Frame, FrameVersions};
use geode_shell::keymap::KeyContext;
use geode_shell::module::ShellActions;
use geode_shell::shell::{chip, scale};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable as _, SharedString, Task, WeakEntity,
    Window, div,
};
use gpui_component::button::Button;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::table::{DataTable, TableEvent, TableState};
use gpui_component::{ActiveTheme as _, Sizable as _, Size, h_flex, v_flex};

use crate::config_view::{self, ConfigView};
use crate::log::{LogFilter, LogTail};
use crate::model::{self, Badges, Tone};
use crate::prepared::{self, PreparedTable};
use crate::section::Section;
use crate::table::SectionDelegate;

/// Width of the filter input, in pixels at the design rem.
const FILTER_WIDTH: f32 = 240.0;

/// The diagnostics counter each section's builder reads. Comparing only
/// this counter keeps an unrelated change, such as a perf tick, from
/// rebuilding the config rows. The log observer also asks the tail whether
/// the ring has new records, because the ring lives outside `Diagnostics`.
fn diag_version_for(section: Section, v: DiagVersions) -> u64 {
    match section {
        Section::Sources => v.sources,
        Section::Data => v.data,
        Section::Config => v.config,
        Section::Log => v.log_levels,
        Section::Perf => v.perf,
    }
}

fn title_for(section: Section) -> SharedString {
    SharedString::from(format!("Diagnostics · {}", section.title()))
}

pub struct DiagnosticsPage {
    pub(crate) frame: Entity<Frame>,
    pub(crate) diagnostics: Entity<Diagnostics>,
    config: Rc<RefCell<Config>>,
    actions: ShellActions,
    focus_handle: FocusHandle,
    section: Section,
    /// Cursor row per section, kept across switches.
    cursors: [usize; 5],
    /// Filter text per section; the one input shows the selected section's.
    filters: [String; 5],
    filter_input: Entity<InputState>,
    table: Entity<TableState<SectionDelegate>>,
    prepared: Rc<PreparedTable>,
    /// The Config section's left panel: the current or historical
    /// diagnostics. Pointer-driven; the keys stay with `table`.
    diag_table: Entity<TableState<SectionDelegate>>,
    diag_prepared: Rc<PreparedTable>,
    /// The left panel's selected row, feeding its own detail strip.
    diag_cursor: usize,
    /// The rem the delegates' column widths were prepared at; `refresh`
    /// runs only when the window's rem moves off it.
    last_rem: f32,
    collapsed_datasets: BTreeSet<String>,
    collapsed_docs: BTreeSet<String>,
    /// The left panel shows prior batches instead of the current one.
    config_history: bool,
    /// Whether the catalog was taken under the frame's as-of; the Data
    /// toolbar chip. Cached at rebuild: both inputs rebuild the section.
    catalog_matches: bool,
    log: LogTail,
    log_filter: LogFilter,
    follow: bool,
    badges: Badges,
    /// Rail badge text per section, formatted with the badges.
    rail_texts: [SharedString; 5],
    header_chips: Vec<(SharedString, Tone)>,
    /// The Config toolbar's History label, formatted with the badges.
    history_label: SharedString,
    /// The log cursor row's detail joined for the copy button, cached
    /// with every cursor or table change so paint formats nothing.
    copy_text: Option<SharedString>,
    /// [`Self::title`]'s cache, replaced only on a section change.
    title: SharedString,
    perf: Option<model::PerfModel>,
    visible: bool,
    /// A page input holds focus; see `key_context`.
    insert_mode: bool,
    #[allow(dead_code)]
    ages_timer: Option<Task<()>>,
    #[allow(dead_code)]
    ages_now: SystemTime,
    last_diag_versions: DiagVersions,
    last_frame_versions: FrameVersions,
    #[cfg(test)]
    pub(crate) rebuild_count: u32,
}

impl DiagnosticsPage {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        frame: Entity<Frame>,
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

        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder("filter…"));
        cx.subscribe_in(
            &filter_input,
            window,
            |this, input, event: &InputEvent, _window, cx| match event {
                InputEvent::Change => {
                    let text = input.read(cx).value();
                    let ix = this.section as usize;
                    if this.filters[ix] != text.as_ref() {
                        this.filters[ix] = text.to_string();
                        this.rebuild(cx);
                    }
                }
                InputEvent::Focus => {
                    this.insert_mode = true;
                    cx.notify();
                }
                InputEvent::Blur => {
                    this.insert_mode = false;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {}
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
        // The left panel's rows expand nothing: a selection only moves its
        // detail strip, and the keyboard cursor stays on the right table.
        let diag_table = Self::new_table(
            SectionDelegate::with_row_selector("diagnostics-diag-row"),
            window,
            cx,
        );
        cx.subscribe_in(
            &diag_table,
            window,
            |this, _table, event: &TableEvent, _window, cx| {
                if let TableEvent::SelectRow(ix) = event
                    && this.diag_cursor != *ix
                {
                    this.diag_cursor = *ix;
                    cx.notify();
                }
            },
        )
        .detach();

        let last_diag_versions = diagnostics.read(cx).versions();
        let last_frame_versions = frame.read(cx).versions();

        cx.observe(&diagnostics, |this, diagnostics, cx| {
            let now = diagnostics.read(cx).versions();
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
            this.last_diag_versions = now;
            if relevant {
                this.rebuild(cx);
            } else if any {
                this.refresh_badges(cx);
            }
        })
        .detach();
        // Timestamps are formatted at rebuild, so a clock-setting change
        // rebuilds the selected section.
        cx.observe_global::<geode_shell::clock::AppClock>(|this, cx| this.rebuild(cx))
            .detach();
        // The app registers its config-refresh frame observer before pages
        // are created; it must update the shared `Config` before this
        // observer rebuilds config rows on the same version change.
        cx.observe(&frame, |this, frame, cx| {
            let now = frame.read(cx).versions();
            let as_of_changed = now.as_of != this.last_frame_versions.as_of;
            let config_changed = now.config != this.last_frame_versions.config;
            // Only data rows read the frame's as-of; only config rows read
            // the loaded config. Scope and grouping keystrokes rebuild
            // nothing here.
            let relevant = match this.section {
                Section::Data => as_of_changed,
                Section::Config => config_changed,
                Section::Sources | Section::Log | Section::Perf => false,
            };
            this.last_frame_versions = now;
            if relevant {
                this.rebuild(cx);
            }
            // The catalog resolves generation markers under the request's
            // as-of: a visible page needs a fresh snapshot when it changes.
            // A hidden page requests one when it becomes visible.
            if as_of_changed && this.visible {
                this.diagnostics.update(cx, |d, cx| {
                    d.request_catalog_refresh();
                    cx.notify();
                });
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
            cursors: [0; 5],
            filters: Default::default(),
            filter_input,
            table,
            prepared: Rc::new(PreparedTable::empty()),
            diag_table,
            diag_prepared: Rc::new(PreparedTable::empty()),
            diag_cursor: 0,
            last_rem: scale::DESIGN_REM,
            collapsed_datasets: BTreeSet::new(),
            collapsed_docs: BTreeSet::new(),
            config_history: false,
            catalog_matches: false,
            log,
            log_filter: LogFilter::all(),
            follow: true,
            badges: Badges {
                sources: (None, 0),
                datasets: 0,
                config: (0, 0),
                log_errors: 0,
                perf_p95: String::new(),
            },
            rail_texts: Default::default(),
            header_chips: Vec::new(),
            history_label: SharedString::default(),
            copy_text: None,
            title: title_for(section),
            perf: None,
            visible: false,
            insert_mode: false,
            ages_timer: None,
            ages_now: SystemTime::now(),
            last_diag_versions,
            last_frame_versions,
            #[cfg(test)]
            rebuild_count: 0,
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
        self.ages_now = now;
        if self.section == Section::Log {
            self.log.drain();
        }
        let filter = self.filters[self.section as usize].clone();
        // The Config section's left panel is built alongside its cursor
        // table: both read inputs the config version covers.
        let mut diag_prepared = None;
        let prepared = {
            let d = self.diagnostics.read(cx);
            let frame = self.frame.read(cx);
            match self.section {
                Section::Sources => {
                    prepared::sources_table(&model::source_rows(d, clock), now, &filter)
                }
                Section::Data => {
                    self.catalog_matches = model::catalog_matches_frame(d, frame.as_of());
                    prepared::data_table(
                        &model::dataset_rows(d, frame.as_of(), clock),
                        &self.collapsed_datasets,
                        &filter,
                    )
                }
                Section::Config => {
                    let diags = if self.config_history {
                        model::history_diagnostics(d, clock)
                    } else {
                        model::current_diagnostics(d)
                    };
                    diag_prepared = Some(prepared::diagnostics_table(&diags, self.config_history));
                    prepared::config_table(
                        &model::config_docs(&self.config.borrow(), &filter),
                        &self.collapsed_docs,
                    )
                }
                Section::Log => {
                    // The one input is the log's message filter.
                    self.log_filter.text = filter;
                    prepared::log_table(
                        &model::log_rows(self.log.records(), &self.log_filter, clock),
                        self.log.lost(),
                    )
                }
                Section::Perf => {
                    self.perf = Some(model::perf_model(d, &frame.requery));
                    PreparedTable::empty()
                }
            }
        };
        self.prepared = Rc::new(prepared);
        let len = self.prepared.rows.len();
        let ix = self.section as usize;
        if self.section == Section::Log && self.follow {
            self.cursors[ix] = len.saturating_sub(1);
        } else {
            self.cursors[ix] = self.cursors[ix].min(len.saturating_sub(1));
        }
        let cursor = self.cursors[ix];
        let shared = self.prepared.clone();
        // `column()` is read only when the table prepares its layout, so
        // every new table needs a `refresh` before it paints.
        self.table.update(cx, |t, cx| {
            t.delegate_mut().set(shared);
            t.refresh(cx);
            if len > 0 {
                t.set_selected_row(cursor, cx);
            }
        });
        if let Some(diag) = diag_prepared {
            self.diag_prepared = Rc::new(diag);
            let len = self.diag_prepared.rows.len();
            self.diag_cursor = self.diag_cursor.min(len.saturating_sub(1));
            let (shared, cursor) = (self.diag_prepared.clone(), self.diag_cursor);
            self.diag_table.update(cx, |t, cx| {
                t.delegate_mut().set(shared);
                t.refresh(cx);
                if len > 0 {
                    t.set_selected_row(cursor, cx);
                }
            });
        }
        self.refresh_copy_text();
        self.refresh_badges(cx);
        cx.notify();
    }

    /// Only the log offers a copy of the cursor row's detail.
    fn refresh_copy_text(&mut self) {
        self.copy_text = (self.section == Section::Log)
            .then(|| self.prepared.rows.get(self.cursor()))
            .flatten()
            .map(|r| SharedString::from(r.detail.join("\n")));
    }

    fn refresh_badges(&mut self, cx: &mut Context<Self>) {
        // The Log badge counts the whole tail, so the tail is drained here
        // for every section, not only when the Log table rebuilds. A second
        // drain after the Log rebuild's own returns false and changes
        // nothing; the loss gap is still measured at the last drain.
        self.log.drain();
        let clock = Self::clock(cx);
        let d = self.diagnostics.read(cx);
        let log_errors = self
            .log
            .records()
            .filter(|r| r.level == geode_core::log::Level::ERROR)
            .count();
        self.badges = model::badges(d, log_errors);
        self.header_chips = model::header_chips(d, clock)
            .into_iter()
            .map(|(s, t)| (SharedString::from(s), t))
            .collect();
        self.rail_texts = crate::page_chrome::rail_texts(&self.badges);
        // The front batch is the current one; history is the rest.
        self.history_label = SharedString::from(format!(
            "History ({} batches)",
            d.config_history.len().saturating_sub(1)
        ));
        cx.notify();
    }

    /// Switch the Config section's left panel between the current batch
    /// and the prior ones. The lists are unrelated, so the panel's
    /// selection restarts at the top.
    pub(crate) fn set_config_history(&mut self, history: bool, cx: &mut Context<Self>) {
        if self.config_history == history {
            return;
        }
        self.config_history = history;
        self.diag_cursor = 0;
        self.rebuild(cx);
    }

    /// Select a section: rebuild it and show its filter text in the one
    /// input. The input's `set_value` needs the window, so every caller
    /// brings one; nothing syncs the input from render.
    pub fn set_section(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        if self.section == section {
            return;
        }
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
        if self.section == Section::Log {
            self.follow = false;
        }
        if len > 0 {
            self.table.update(cx, |t, cx| t.set_selected_row(ix, cx));
        }
        self.refresh_copy_text();
        cx.notify();
    }

    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let cur = self.cursor() as isize;
        let target = (cur + delta).max(0) as usize;
        self.set_cursor(target, cx);
    }

    fn toggle_expansion_at_cursor(&mut self, expand: Option<bool>, cx: &mut Context<Self>) {
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
            Section::Sources | Section::Log | Section::Perf => return,
        };
        let collapsed_now = set.contains(&key);
        let collapse = match expand {
            Some(e) => !e,
            None => !collapsed_now,
        };
        if collapse {
            set.insert(key);
        } else {
            set.remove(&key);
        }
        self.rebuild(cx);
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
        KeyContext::new("diagnostics")
            .pair("section", self.section.name())
            .pair("mode", if self.insert_mode { "insert" } else { "normal" })
            .counts()
    }

    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.filter_input.focus_handle(cx).is_focused(window)
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
        let Some(name) = action.0.strip_prefix("diagnostics::") else {
            return false;
        };
        let n = count.unwrap_or(1).max(1) as isize;
        match name {
            "down" => self.move_cursor(n, cx),
            "up" => self.move_cursor(-n, cx),
            "top" => self.set_cursor(0, cx),
            "bottom" => {
                let last = self.prepared.rows.len().saturating_sub(1);
                self.set_cursor(last, cx);
                if self.section == Section::Log {
                    self.follow = true;
                }
            }
            "page_down" => self.move_cursor(5 * n, cx),
            "page_up" => self.move_cursor(-5 * n, cx),
            "page_down_full" => self.move_cursor(10 * n, cx),
            "page_up_full" => self.move_cursor(-10 * n, cx),
            "next_section" => self.set_section(self.section.next(), window, cx),
            "prev_section" => self.set_section(self.section.prev(), window, cx),
            "expand" => self.toggle_expansion_at_cursor(Some(true), cx),
            "collapse" => self.toggle_expansion_at_cursor(Some(false), cx),
            "activate" => self.toggle_expansion_at_cursor(None, cx),
            // Perf paints no input: focusing its handle would leave a focused
            // handle with no element. The action is consumed and does nothing.
            "filter" if self.section == Section::Perf => {}
            "filter" => self.filter_input.update(cx, |i, cx| i.focus(window, cx)),
            "blur" => self.focus_handle.focus(window, cx),
            _ => return false,
        }
        cx.notify();
        true
    }

    /// Visibility drives watched demand: a watch queues the initial
    /// catalog, and the shell's unwatch cancels it when the page closes.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
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

    fn sync_ages_timer(&mut self, cx: &mut Context<Self>) {
        // The Sources ages timer lands with the Perf section; no timer yet.
        let _ = cx;
    }

    fn filter_input_el(&self) -> Input {
        Input::new(&self.filter_input)
            .cleanable(true)
            .w(scale::design(FILTER_WIDTH))
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let weak: WeakEntity<Self> = cx.weak_entity();
        match self.section {
            Section::Sources | Section::Log => h_flex()
                .gap_2()
                .p_2()
                .child(self.filter_input_el())
                .into_any_element(),
            Section::Data => {
                let theme = cx.theme();
                let (text, tone) = if self.catalog_matches {
                    ("catalog as-of = frame", chip::Tone::Neutral)
                } else {
                    ("catalog pending", chip::Tone::Warning)
                };
                let paint = chip::chip_paint(theme, tone);
                let diagnostics = self.diagnostics.clone();
                let (expand, collapse) = (weak.clone(), weak);
                h_flex()
                    .gap_2()
                    .p_2()
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
                                    .outline()
                                    .xsmall()
                                    .label("Refresh catalog")
                                    .on_click(move |_, _window, cx| {
                                        diagnostics.update(cx, |d, cx| {
                                            d.request_catalog();
                                            cx.notify();
                                        });
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id("diagnostics-expand-all")
                            .debug_selector(|| "diagnostics-expand-all".to_string())
                            .child(
                                Button::new("diagnostics-expand-all")
                                    .outline()
                                    .xsmall()
                                    .label("Expand all")
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
                                    .outline()
                                    .xsmall()
                                    .label("Collapse all")
                                    .on_click(move |_, _window, cx| {
                                        let _ = collapse.update(cx, |p, cx| {
                                            p.set_all_datasets_collapsed(true, cx)
                                        });
                                    }),
                            ),
                    )
                    .into_any_element()
            }
            // Config's toolbars belong to its two panels (`config_view`).
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
        let header = crate::page_chrome::header(&self.header_chips, self.actions.clone(), cx);
        let rail = crate::page_chrome::rail(
            self.section,
            &self.badges,
            &self.rail_texts,
            weak.clone(),
            cx,
        );
        let toolbar = self.render_toolbar(cx);
        let body: AnyElement = match self.section {
            Section::Perf => {
                crate::perf_view::render(self.perf.as_ref(), weak, cx).into_any_element()
            }
            Section::Config => config_view::render(
                ConfigView {
                    diag_table: &self.diag_table,
                    diag_row: self.diag_prepared.rows.get(self.diag_cursor),
                    history: self.config_history,
                    history_label: self.history_label.clone(),
                    table: &self.table,
                    row: self.prepared.rows.get(self.cursor()),
                    filter: self.filter_input_el(),
                    actions: self.actions.clone(),
                },
                weak,
                cx,
            ),
            Section::Sources | Section::Data | Section::Log => {
                let row = self.prepared.rows.get(self.cursor());
                let copy = self.copy_text.clone();
                v_flex()
                    .size_full()
                    .child(
                        div().flex_1().min_h_0().w_full().child(
                            DataTable::new(&self.table)
                                .with_size(Size::XSmall)
                                .bordered(false)
                                .stripe(false),
                        ),
                    )
                    .child(crate::page_chrome::detail_strip(
                        "diagnostics-detail",
                        row,
                        copy,
                        cx,
                    ))
                    .into_any_element()
            }
        };
        v_flex()
            .size_full()
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
                            .child(body),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::groupings::GroupingSlots;
    use geode_core::log::LogLevels;
    use geode_core::scopes::SavedScopes;
    use geode_shell::diagnostics::Health;

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
        #[allow(dead_code)]
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
        cx.update(gpui_component::init);
        let ring = Arc::new(Ring::new(64));
        let config = Rc::new(RefCell::new(Config::default()));
        let dispatched: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let recorder = dispatched.clone();
        let actions: ShellActions = Rc::new(move |a: &ActionId, _w: &mut Window, _cx: &mut App| {
            recorder.borrow_mut().push(a.0.clone());
        });
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let frame =
                        cx.new(|_| Frame::new(GroupingSlots::default(), SavedScopes::new(), None));
                    let diagnostics = cx.new(|_| Diagnostics::new(LogLevels::default()));
                    let (ring2, config2, actions2) =
                        (ring.clone(), config.clone(), actions.clone());
                    cx.new(|cx| {
                        let page = cx.new(|cx| {
                            DiagnosticsPage::new(
                                frame.clone(),
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
        let (frame, diagnostics) =
            page.read_with(&vcx, |p, _| (p.frame.clone(), p.diagnostics.clone()));
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

    /// The Log rail badge counts errors in the tail whatever section is
    /// selected: the tail must be drained for the badge, not only for the
    /// Log table.
    #[gpui::test]
    fn the_log_rail_badge_counts_errors_while_another_section_is_selected(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        assert_eq!(h.page.read_with(&vcx, |p, _| p.section()), Section::Sources);
        h.ring.push(geode_core::log::Record {
            at: SystemTime::now(),
            level: geode_core::log::Level::ERROR,
            target: "geode::shell",
            message: "boom".into(),
            seq: 0,
        });
        h.diagnostics.update(&mut vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
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
        h.frame.update(&mut vcx, |f, cx| {
            let _ = f.set_as_of(at.clone());
            cx.notify();
        });
        vcx.run_until_parked();
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
                let _ = p.dispatch(&ActionId("diagnostics::bottom".into()), None, window, cx);
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

    /// The Config section's left panel lists the current batch; the
    /// History button switches it to the prior batches, batch column
    /// first, through the pointer route.
    #[gpui::test]
    fn the_config_section_paints_current_diagnostics_and_switches_to_history(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        note_two_config_batches(&h, &mut vcx);
        open_config_section(&h, &mut vcx);
        let current = h.page.read_with(&vcx, |p, cx| {
            p.diag_table.read(cx).delegate().table().clone()
        });
        assert_eq!(current.rows.len(), 1);
        assert_eq!(current.rows[0].cells[3].text.as_ref(), "second");
        assert_eq!(
            h.page
                .read_with(&vcx, |p, _| p.history_label.clone())
                .as_ref(),
            "History (1 batches)",
            "the current batch is not history"
        );
        let b = vcx.debug_bounds("diagnostics-diag-history").unwrap();
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        let history = h.page.read_with(&vcx, |p, cx| {
            p.diag_table.read(cx).delegate().table().clone()
        });
        assert_eq!(history.columns.len(), 5, "batch column first");
        assert_eq!(history.rows[0].cells[4].text.as_ref(), "first");
        assert_eq!(
            history.rows[0].cells[3].text.as_ref(),
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
        assert_eq!(current.rows[0].cells[3].text.as_ref(), "second");
    }

    /// A click on a left-panel row drives that panel's detail strip and
    /// leaves the keyboard cursor, which belongs to the right table, alone.
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
        vcx.update(|window, cx| {
            h.page.update(cx, |p, cx| {
                let _ = p.dispatch(&ActionId("diagnostics::down".into()), None, window, cx);
            });
        });
        let before = h.page.read_with(&vcx, |p, _| p.cursor());
        assert_eq!(h.page.read_with(&vcx, |p, _| p.diag_cursor), 0);
        let row = vcx
            .debug_bounds("diagnostics-diag-row-1")
            .expect("the second diagnostic row is painted in the left panel");
        let at = gpui::point(row.origin.x + gpui::px(4.0), row.center().y);
        vcx.simulate_click(at, gpui::Modifiers::default());
        vcx.run_until_parked();
        assert_eq!(h.page.read_with(&vcx, |p, _| p.diag_cursor), 1);
        assert_eq!(
            h.page.read_with(&vcx, |p, _| p.cursor()),
            before,
            "the right table's cursor is untouched"
        );
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            vcx.debug_bounds("diagnostics-diag-detail").is_some(),
            "the left panel paints its own detail strip"
        );
    }

    /// Only the Config section reads the loaded config, so only it
    /// rebuilds on a config version bump.
    #[gpui::test]
    fn a_config_version_change_rebuilds_only_the_config_section(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
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
}
