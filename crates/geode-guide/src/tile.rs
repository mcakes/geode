mod chrome;

use geode_core::query::QueryKey;
use geode_shell::actions::ActionId;
use geode_shell::frame::FrameRef;
use geode_shell::keymap::{KeyContext, Keystroke};
use geode_shell::module::{CloseHandle, FindEvent, StackHandle};
use geode_shell::tiling::TileId;
use geode_shell::tips::{self, Chords};
use geode_tile::following::{self, DeferredDoor, FrameDoor};
use geode_tile::menu::{self, ActionRow, Hint, Menu, MenuHost, MenuIds, MenuPick, Row};
use geode_tile::notice::Notice;
use gpui::prelude::*;
use gpui::{App, ClipboardItem, Context, Entity, ListOffset, SharedString, Window};
use gpui_component::ActiveTheme as _;
use gpui_component::text::{SelectionFormat, TextViewState};

use crate::document::{section_for, sections};

#[derive(Clone, Debug, PartialEq)]
enum Pick {
    Section(SharedString),
    Action(&'static str),
}

#[derive(Clone, Copy, PartialEq)]
enum MenuKind {
    Contents,
    Actions,
}

struct OpenMenu {
    kind: MenuKind,
    menu: Menu<Pick>,
}

type HintKeys = Vec<(&'static str, Vec<Keystroke>)>;

fn hint_keys(cx: &App) -> HintKeys {
    let bindings = menu::live_bindings(cx);
    [
        "guide::contents",
        "guide::previous",
        "guide::next",
        "tile::find",
        "guide::menu",
        "guide::next_match",
        "guide::previous_match",
        "guide::clear_find",
    ]
    .into_iter()
    .filter_map(|action| tips::chord_for(&bindings, action).map(|keys| (action, keys)))
    .collect()
}

impl MenuPick for Pick {
    fn element_name(&self) -> SharedString {
        match self {
            Self::Section(anchor) => anchor.clone(),
            Self::Action(action) => SharedString::new_static(action),
        }
    }
}

pub(crate) struct GuideTile {
    id: TileId,
    frame: FrameRef,
    pub section: usize,
    pub(crate) text: Entity<TextViewState>,
    pub stack: Option<StackHandle>,
    pub close: Option<CloseHandle>,
    menu: Option<OpenMenu>,
    actions_ids: MenuIds,
    hints: HintKeys,
    query: Option<regex::Regex>,
    query_text: String,
    pub(crate) search_status: SharedString,
    marked_source: String,
    pending_scroll: Option<ListOffset>,
    expected_blocks: usize,
    menu_ids: MenuIds,
    matches: Vec<usize>,
    find_entry: Option<(usize, ListOffset)>,
    notice: Option<Notice>,
}

impl GuideTile {
    pub fn new(
        id: TileId,
        frame: FrameRef,
        restored: Option<&toml::Table>,
        cx: &mut Context<Self>,
    ) -> Self {
        let section = restored
            .and_then(|r| r.get("section"))
            .and_then(|v| v.as_str())
            .and_then(section_for)
            .unwrap_or(0);
        let marked_source = sections()[section].markdown.clone();
        let text = cx.new(|cx| {
            TextViewState::markdown(&marked_source, cx)
                .scrollable(true)
                .selectable(true)
                .selection_format(SelectionFormat::Plain)
        });
        // The shell includes visible tiles in frame flip barriers. This reader
        // has no asynchronous result to wait for, so it answers immediately.
        cx.observe(frame.entity(), |this, _, cx| this.arrive(cx))
            .detach();
        cx.observe_global::<Chords>(|this, cx| {
            this.hints = hint_keys(cx);
            if let Some(open) = &mut this.menu {
                open.menu.rehint(&menu::live_bindings(cx));
            }
            cx.notify();
        })
        .detach();
        cx.observe_global::<gpui_component::Theme>(|this, cx| {
            if this.query.is_some() {
                let offset = this.text.read(cx).list_state().logical_scroll_top();
                this.refresh_text(false, cx);
                this.text.read(cx).list_state().scroll_to(offset);
            }
            cx.notify();
        })
        .detach();
        Self {
            id,
            frame,
            section,
            text,
            stack: None,
            close: None,
            menu: None,
            actions_ids: MenuIds::new(
                format!("guide-actions-{}", id.0),
                format!("guide-action-{}", id.0),
            ),
            hints: hint_keys(cx),
            query: None,
            query_text: String::new(),
            search_status: SharedString::default(),
            marked_source,
            pending_scroll: None,
            expected_blocks: 0,
            menu_ids: MenuIds::new(
                format!("guide-contents-{}", id.0),
                format!("guide-chapter-{}", id.0),
            ),
            matches: Vec::new(),
            find_entry: None,
            notice: None,
        }
    }

    pub fn key_context(&self) -> KeyContext {
        let context = KeyContext::new("guide").counts();
        if self.menu.is_some() {
            context.pair("mode", "menu").tilelist()
        } else {
            context.pair("mode", "normal")
        }
    }

    pub fn arrive(&self, cx: &mut App) {
        following::arrive_immediately(&mut FrameDoor::new(&self.frame, cx), QueryKey(self.id.0));
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if !visible {
            self.menu = None;
        }
        following::arrive_immediately(&mut DeferredDoor::new(&self.frame, cx), QueryKey(self.id.0));
    }

    fn show_section(&mut self, section: usize, cx: &mut Context<Self>) {
        self.section = section.min(sections().len() - 1);
        self.menu = None;
        self.notice = None;
        self.refresh_text(true, cx);
        cx.notify();
    }

    fn refresh_text(&mut self, scroll: bool, cx: &mut Context<Self>) {
        // The component's HTML mark consumes CSS color, resolved from the live
        // selection token rather than embedding a light-only palette value.
        let color = format!("#{:08x}", u32::from(gpui::Rgba::from(cx.theme().selection)));
        let mut marked = sections()[self.section]
            .document
            .render(self.query.as_ref(), &color);
        if self.query.is_none() {
            marked.source = sections()[self.section].markdown.clone();
        }
        self.search_status = if self.query.is_none() {
            SharedString::default()
        } else if self.matches.is_empty() {
            format!("No matches for “{}”", self.query_text).into()
        } else {
            let at = self.matches.iter().position(|&ix| ix == self.section);
            match at {
                Some(at) => format!(
                    "{} {} · section {} of {}",
                    marked.count,
                    if marked.count == 1 {
                        "match"
                    } else {
                        "matches"
                    },
                    at + 1,
                    self.matches.len()
                )
                .into(),
                None => {
                    format!("No matches here · {} matching sections", self.matches.len()).into()
                }
            }
        };
        self.marked_source = marked.source;
        self.expected_blocks = marked.block_count;
        self.pending_scroll = Some(if scroll {
            ListOffset {
                item_ix: marked.first_block.unwrap_or(0),
                offset_in_item: Default::default(),
            }
        } else {
            self.text.read(cx).list_state().logical_scroll_top()
        });
        self.text.update(cx, |text, cx| {
            text.clear_selection(cx);
            text.set_text(&self.marked_source, cx);
        });
    }

    fn clear_find(&mut self, cx: &mut Context<Self>) {
        self.query = None;
        self.query_text.clear();
        self.matches.clear();
        self.find_entry = None;
        self.refresh_text(false, cx);
        cx.notify();
    }

    fn menu_is(&self, kind: MenuKind) -> bool {
        self.menu.as_ref().is_some_and(|open| open.kind == kind)
    }

    fn toggle_actions(&mut self, cx: &mut Context<Self>) {
        if self.menu_is(MenuKind::Actions) {
            self.menu = None;
        } else {
            let rows = [
                ("guide::contents", "Contents…"),
                ("guide::previous", "Previous section"),
                ("guide::next", "Next section"),
                ("guide::down", "Scroll down"),
                ("guide::up", "Scroll up"),
                ("guide::page_down", "Page down"),
                ("guide::page_up", "Page up"),
                ("guide::top", "Top of section"),
                ("guide::bottom", "Bottom of section"),
                ("guide::next_match", "Next matching section"),
                ("guide::previous_match", "Previous matching section"),
                ("guide::clear_find", "Clear search"),
                ("guide::copy", "Copy section"),
            ]
            .into_iter()
            .map(|(action, title)| {
                let reason = match action {
                    "guide::previous" if self.section == 0 => Some("First section"),
                    "guide::next" if self.section + 1 == sections().len() => Some("Last section"),
                    "guide::next_match" | "guide::previous_match" if self.matches.is_empty() => {
                        Some("No matching sections")
                    }
                    "guide::clear_find" if self.query.is_none() => Some("No search"),
                    _ => None,
                };
                Row::Action(
                    ActionRow::new(Pick::Action(action), title)
                        .hint(Hint::chord(action))
                        .enabled(reason.map_or(Ok(()), |reason| Err(reason.into()))),
                )
            })
            .collect();
            self.menu = Some(OpenMenu {
                kind: MenuKind::Actions,
                menu: Menu::new(rows, &menu::live_bindings(cx)),
            });
        }
        cx.notify();
    }

    fn toggle_contents(&mut self, cx: &mut Context<Self>) {
        if self.menu_is(MenuKind::Contents) {
            self.menu = None;
        } else {
            let chapter_anchor = &sections()[..=self.section]
                .iter()
                .rfind(|s| s.chapter)
                .unwrap()
                .anchor;
            let rows: Vec<_> = sections()
                .iter()
                .filter(|s| s.chapter)
                .map(|s| {
                    Row::Action(
                        ActionRow::new(Pick::Section(s.anchor.clone()), s.title.clone())
                            .checked(&s.anchor == chapter_anchor),
                    )
                })
                .collect();
            let chapter = sections()[..=self.section]
                .iter()
                .filter(|s| s.chapter)
                .count()
                - 1;
            self.menu = Some(OpenMenu {
                kind: MenuKind::Contents,
                menu: Menu::new(rows, &[]).open_at(Some(chapter)),
            });
        }
        cx.notify();
    }

    pub fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let n = count.unwrap_or(1).max(1) as usize;
        // Palette actions reach the tile even while contents is open.
        // Run ordinary reader commands after dismissing that transient menu.
        if action.0.starts_with("guide::")
            && !matches!(
                action.0.as_str(),
                "guide::menu" | "guide::contents" | "guide::pick" | "guide::cancel"
            )
        {
            self.menu = None;
        }
        if let Some(open) = self.menu.as_mut() {
            let menu = &mut open.menu;
            match action.0.as_str() {
                "motion::menu_down" => menu.step(n.min(menu.rows().len()) as isize),
                "motion::menu_up" => menu.step(-(n.min(menu.rows().len()) as isize)),
                "guide::pick" => {
                    if let Some(ix) = menu.highlighted() {
                        self.menu_pick(ix, window, cx);
                    }
                }
                "guide::cancel" => self.menu = None,
                "guide::contents" => self.toggle_contents(cx),
                "guide::menu" => self.toggle_actions(cx),
                _ => return false,
            }
            cx.notify();
            return true;
        }
        match action.0.as_str() {
            "guide::contents" => self.toggle_contents(cx),
            "guide::menu" => self.toggle_actions(cx),
            "guide::clear_find" => self.clear_find(cx),
            "guide::next" => self.show_section(self.section.saturating_add(n), cx),
            "guide::previous" => self.show_section(self.section.saturating_sub(n), cx),
            "guide::down" | "guide::up" | "guide::page_down" | "guide::page_up" => {
                let text = self.text.read(cx);
                let step = if action.0.contains("page_") {
                    text.bounds().size.height * 0.8
                } else {
                    window.rem_size() * 1.5
                };
                let direction = if action.0.ends_with("up") { -1.0 } else { 1.0 };
                text.list_state().scroll_by(step * direction * n as f32);
            }
            "guide::top" => self
                .text
                .read(cx)
                .list_state()
                .scroll_to(ListOffset::default()),
            "guide::bottom" => self.text.read(cx).list_state().scroll_to_end(),
            "guide::next_match" => self.step_match(n as isize, cx),
            "guide::previous_match" => self.step_match(-(n as isize), cx),
            "guide::copy" => cx.write_to_clipboard(ClipboardItem::new_string(
                sections()[self.section].markdown.clone(),
            )),
            _ => return false,
        }
        cx.notify();
        true
    }

    pub fn command(
        &mut self,
        line: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let mut words = line.split_whitespace();
        if words.next() != Some("section") {
            return Err("Use :section <heading-anchor> to open a guide section".into());
        }
        let anchor = words.next();
        if words.next().is_some() {
            return Err("Use :section <heading-anchor>".into());
        }
        let section = match anchor {
            None => 0,
            Some(anchor) => section_for(anchor.trim_start_matches('#'))
                .ok_or_else(|| format!("Unknown guide section: {anchor}"))?,
        };
        self.show_section(section, cx);
        Ok(())
    }

    pub fn find(&mut self, event: FindEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let (query, committed) = match event {
            FindEvent::Changed(query) => (query, false),
            FindEvent::Committed(query) => (query, true),
            FindEvent::Cancelled => {
                self.query = None;
                self.query_text.clear();
                if let Some((section, offset)) = self.find_entry.take() {
                    self.show_section(section, cx);
                    self.pending_scroll = Some(offset);
                }
                self.matches.clear();
                self.notice = None;
                cx.notify();
                return;
            }
        };
        let entry = *self.find_entry.get_or_insert_with(|| {
            (
                self.section,
                self.text.read(cx).list_state().logical_scroll_top(),
            )
        });
        self.query_text = query.trim().to_string();
        self.query = crate::search::query(&query);
        self.matches = sections()
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                self.query
                    .as_ref()
                    .is_some_and(|query| s.document.contains(query))
            })
            .map(|(ix, _)| ix)
            .collect();
        if let Some(&section) = self.matches.first() {
            self.show_section(section, cx);
        } else {
            self.show_section(entry.0, cx);
            self.pending_scroll = Some(entry.1);
        }
        if committed {
            self.find_entry = None;
        }
        cx.notify();
    }

    fn step_match(&mut self, by: isize, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        let at = self.matches.iter().position(|&ix| ix == self.section);
        let target = match at {
            Some(at) => (at as isize + by).rem_euclid(self.matches.len() as isize) as usize,
            None if by > 0 => 0,
            None => self.matches.len() - 1,
        };
        self.show_section(self.matches[target], cx);
    }
}

impl MenuHost for GuideTile {
    fn menu_pick(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let pick = self.menu.as_ref().and_then(|open| open.menu.pick(ix));
        self.menu = None;
        match pick {
            Some(Ok(Pick::Section(anchor))) => {
                if let Some(section) = section_for(&anchor) {
                    self.show_section(section, cx);
                }
            }
            Some(Ok(Pick::Action(action))) => {
                self.dispatch(&ActionId(action.into()), None, window, cx);
            }
            Some(Err(reason)) => self.notice = Some(Notice::status(reason)),
            None => {}
        }
        cx.notify();
    }
    fn menu_hover(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(menu) = &mut self.menu
            && menu.menu.highlight(ix)
        {
            cx.notify();
        }
    }
}
