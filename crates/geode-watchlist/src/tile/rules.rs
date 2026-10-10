//! The rules popup: the shown list's rules, one row each, hung from the
//! header's `<k> rules` item, with a cursor the popup's own keys move
//! (`rules` mode: `j`/`k` step, `o` adds, `x` removes, `enter` edits,
//! `escape` and `r` close). A rule prompt (dataset, scope, expression)
//! opens over it; the popup stays painted beneath the field, inline under
//! the prompt bar, until the field is gone.
//!
//! The paint comes first; the tile's side (opening, the keys, the rule
//! verbs and the write) is the `impl WatchlistTile` beneath it.

use geode_core::scope::complete::ExprVocab;
use geode_core::watchlist::edit;
use geode_core::watchlist::fold::eligible_datasets;
use geode_core::watchlist::{Rule, Watchlist};
use geode_shell::shell::listrow::row_paint;
use geode_shell::shell::scale;
use geode_tile::notice::{self, Notice, Tone};
use geode_tile::popover::{self, ROW_INSET, anchor_popup, empty_row, row_shell};
use gpui::prelude::*;
use gpui::{Anchor, AnyElement, App, Context, Div, ElementId, Entity, SharedString, Window, div};
use gpui_component::ActiveTheme as _;

use super::{NOTHING_SHOWN, WatchlistTile, snapshot};
use crate::content::WatchlistConfig;
use crate::core::prompt::{self, Prompt, RuleContext};
use crate::core::rules::{RuleRow, RulesPopup};

/// What `o` says while no dataset in the schema may be read by a rule.
pub(crate) const NO_RULE_DATASET: &str = "no dataset carries underlying_ref";
/// What the popup lists while the list has no rules.
pub(crate) const NO_RULES: &str = "no rules \u{2014} `o` adds one";
/// What `x` and `enter` say with no rule under the cursor.
pub(crate) const NO_RULE: &str = "no rule under the cursor";
/// What a rules write that changed nothing says.
pub(crate) const RULES_UNCHANGED: &str = "rules unchanged";

/// The popup's surface: one row per rule (`rule <i> · <dataset> · <scope>`,
/// the fold's error in the warning tone beneath), the cursor row
/// highlighted, the empty row with none. A row press moves the cursor
/// there. `closer` is whether an outside press closes it (not while a
/// rule prompt is open: the field is outside the popup).
fn surface(
    rows: &[RuleRow],
    cursor: usize,
    closer: bool,
    tile: &Entity<WatchlistTile>,
    tile_id: u64,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    let hover = row_paint(theme).hover;
    let warning = notice::color(Tone::Warning, theme);
    let mut list = popover::surface(cx)
        .debug_selector(move || format!("watchlist-rules-popup-{tile_id}"))
        // The grid beneath must not take a press meant for a row.
        .occlude()
        .when(closer, |el| {
            el.on_mouse_down_out({
                let tile = tile.clone();
                move |_, _, cx| tile.update(cx, |t, cx| t.rules_outside_press(cx))
            })
        });
    if rows.is_empty() {
        return list.child(
            empty_row(theme, NO_RULES)
                .debug_selector(move || format!("watchlist-rules-empty-{tile_id}")),
        );
    }
    for (i, row) in rows.iter().enumerate() {
        let highlighted = i == cursor;
        list = list.child(
            row_shell(
                theme,
                hover,
                ElementId::NamedInteger(SharedString::new_static("watchlist-rule-row"), i as u64),
                highlighted,
                move || format!("watchlist-rule-row-{tile_id}-{i}"),
                {
                    let tile = tile.clone();
                    move |_, cx| tile.update(cx, |t, cx| t.rule_pressed(i, cx))
                },
            )
            .child(SharedString::from(row.text())),
        );
        if let Some(error) = &row.error {
            list = list.child(
                div()
                    .px(scale::design(ROW_INSET))
                    .text_xs()
                    .whitespace_normal()
                    .text_color(warning)
                    .debug_selector(move || format!("watchlist-rule-error-{tile_id}-{i}"))
                    .child(SharedString::from(error.clone())),
            );
        }
    }
    list
}

/// The popup hung from the header's rules item, over the grid (`deferred`
/// via `anchor_popup` escapes the tile's clip).
pub(crate) fn render_hung(
    rows: &[RuleRow],
    cursor: usize,
    tile: &Entity<WatchlistTile>,
    tile_id: u64,
    cx: &App,
) -> AnyElement {
    anchor_popup(
        surface(rows, cursor, true, tile, tile_id, cx),
        Anchor::TopLeft,
    )
    .into_any_element()
}

/// The popup painted in the tile's flow under the prompt bar while a rule
/// prompt is open: the field above it keeps the keys, and the field's own
/// rows float over it.
pub(crate) fn render_inline(
    rows: &[RuleRow],
    cursor: usize,
    tile: &Entity<WatchlistTile>,
    tile_id: u64,
    cx: &App,
) -> AnyElement {
    div()
        .flex_none()
        .px_2()
        .py_1()
        .child(surface(rows, cursor, false, tile, tile_id, cx))
        .into_any_element()
}

/// The popup's lifetime, keys and the rule verbs.
impl WatchlistTile {
    /// `r`, `Watchlist: Rules…`, the `⋯` row: open the popup on the first
    /// rule, or close it when it is open. Nothing shown: refused.
    pub(super) fn toggle_rules(&mut self, cx: &mut Context<Self>) {
        if self.rules.is_some() {
            self.close_rules(cx);
            return;
        }
        if self.shown(&snapshot(cx)).is_none() {
            self.refuse(NOTHING_SHOWN, cx);
            return;
        }
        self.close_menu(cx);
        self.rules = Some(RulesPopup::open_at(0, self.chrome.rules.len()));
        cx.notify();
    }

    /// Close the popup: `escape`, `r` again, a press outside it, a verb
    /// that leaves it, the list going away. Whether it was open.
    pub(super) fn close_rules(&mut self, cx: &mut Context<Self>) -> bool {
        let was = self.rules.take().is_some();
        if was {
            cx.notify();
        }
        was
    }

    /// A press outside the hung popup closes it; while a rule prompt is
    /// open the popup is inline and the press is the field's business.
    fn rules_outside_press(&mut self, cx: &mut Context<Self>) {
        if self.prompt.is_none() {
            self.close_rules(cx);
        }
    }

    /// `j`/`k` (the shared list steps) move the cursor, clamped.
    pub(super) fn rules_step(&mut self, delta: i64, cx: &mut Context<Self>) {
        let len = self.chrome.rules.len();
        if let Some(p) = self.rules.as_mut() {
            p.step(delta, len);
            cx.notify();
        }
    }

    /// A press on a popup row moves the cursor there.
    fn rule_pressed(&mut self, row: usize, cx: &mut Context<Self>) {
        self.user_acted(cx);
        let len = self.chrome.rules.len();
        if let Some(p) = self.rules.as_mut() {
            p.cursor = row;
            p.clamp(len);
            cx.notify();
        }
    }

    /// The rule under the popup's cursor.
    fn cursor_rule(&self) -> Option<&RuleRow> {
        self.rules.as_ref().and_then(|p| p.row(&self.chrome.rules))
    }

    /// The configuration a rule is validated against: the factory's
    /// snapshot, empty before the first push (no dataset is eligible
    /// then, and `o` says so).
    pub(super) fn rule_config(&self) -> WatchlistConfig {
        self.shared.config.borrow().clone().unwrap_or_default()
    }

    /// `o` in the popup (`watchlist::rule_add`): the dataset step over the
    /// eligible datasets, the popup kept open beneath.
    pub(super) fn rule_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shown(&snapshot(cx)).is_none() {
            self.refuse(NOTHING_SHOWN, cx);
            return;
        }
        if let Err(why) = self.verbs_allowed() {
            self.refuse(why, cx);
            return;
        }
        self.close_menu(cx);
        if self.rules.is_none() {
            self.rules = Some(RulesPopup::open_at(0, self.chrome.rules.len()));
        }
        self.open_rule_prompt(Prompt::RuleDataset, window, cx);
    }

    /// `enter` in the popup (`watchlist::rule_edit`): the scope step over
    /// the cursor rule's dataset, replacing that rule in place.
    pub(super) fn rule_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shown(&snapshot(cx)).is_none() {
            self.refuse(NOTHING_SHOWN, cx);
            return;
        }
        if let Err(why) = self.verbs_allowed() {
            self.refuse(why, cx);
            return;
        }
        let Some(row) = self.cursor_rule() else {
            self.refuse(NO_RULE, cx);
            return;
        };
        let next = Prompt::RuleScope {
            dataset: row.dataset.clone(),
            replace: Some(row.index),
        };
        self.close_menu(cx);
        self.open_rule_prompt(next, window, cx);
    }

    /// `x` in the popup (`watchlist::rule_remove`): the rules without the
    /// cursor's, written whole; the cursor keeps its index (the next rule
    /// lands under it).
    pub(super) fn rule_remove(&mut self, cx: &mut Context<Self>) {
        let snapshot = snapshot(cx);
        let Some((name, state)) = self.shown(&snapshot) else {
            self.refuse(NOTHING_SHOWN, cx);
            return;
        };
        if let Err(why) = self.verbs_allowed() {
            self.refuse(why, cx);
            return;
        }
        let Some(index) = self.cursor_rule().map(|r| r.index) else {
            self.refuse(NO_RULE, cx);
            return;
        };
        self.close_menu(cx);
        let config = state.definition.clone();
        let current = self.history.current(&config).clone();
        let mut rules = current.rules.clone();
        if index >= rules.len() {
            self.refuse(NO_RULE, cx);
            return;
        }
        rules.remove(index);
        let (next, entry) = edit::set_rules(&current, rules);
        let said = format!("removed rule {}", index + 1);
        self.commit(&name, &config, next, entry, Notice::status(said), cx);
    }

    /// Open the field for rule step `next`: a closed choice over the
    /// eligible datasets (refused with none), over the scope choices for
    /// the dataset, or the expression completion over the dataset's
    /// columns and the derived dimensions.
    pub(super) fn open_rule_prompt(
        &mut self,
        next: Prompt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let wc = self.rule_config();
        match &next {
            Prompt::RuleDataset => {
                let options: Vec<String> = eligible_datasets(&wc.schema)
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                if options.is_empty() {
                    self.refuse(NO_RULE_DATASET, cx);
                    return;
                }
                self.open_prompt(next, options, window, cx);
            }
            Prompt::RuleScope { dataset, .. } => {
                let options = {
                    let snapshot = snapshot(cx);
                    let current = self
                        .shown(&snapshot)
                        .map(|(_, s)| self.history.current(&s.definition).clone())
                        .unwrap_or_default();
                    let ctx = RuleContext {
                        schema: &wc.schema,
                        dims: &wc.dims,
                        saved: &wc.saved,
                        named: &wc.named,
                        current: &current,
                    };
                    prompt::scope_choices(dataset, &ctx)
                };
                self.open_prompt(next, options, window, cx);
            }
            Prompt::RuleExpression { dataset, .. } => {
                // The vocabulary is the rule's dataset alone: a column
                // another dataset holds would be offered and then refused.
                let mut schema = (*wc.schema).clone();
                schema.datasets.retain(|d| &d.name == dataset);
                let vocab = ExprVocab::new(&schema, &wc.dims);
                self.open_expr_prompt(next, vocab, window, cx);
            }
            Prompt::AddName => self.open_add(window, cx),
        }
    }

    /// Write `rules` as the shown list's, whole, through the config door
    /// (`set_rules` → `commit`): one change, undone whole. `replace` is the
    /// rule edited in place, for the word and the cursor; appended
    /// otherwise. Rules equal to the current ones write nothing.
    pub(super) fn write_rules(
        &mut self,
        name: &str,
        config: &Watchlist,
        rules: Vec<Rule>,
        replace: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let current = self.history.current(config).clone();
        let at = match replace {
            Some(i) if i < current.rules.len() => i,
            _ => rules.len().saturating_sub(1),
        };
        let said = match replace {
            Some(i) if i < current.rules.len() => format!("changed rule {}", i + 1),
            _ => format!("added rule {}", rules.len()),
        };
        let (next, entry) = edit::set_rules(&current, rules);
        if entry.is_empty() {
            self.notices.outcome.clear();
            self.notices.outcome(Notice::status(RULES_UNCHANGED));
            self.rebuild_chrome(cx);
            cx.notify();
            return;
        }
        if let Some(p) = self.rules.as_mut() {
            p.cursor = at;
        }
        self.commit(name, config, next, entry, Notice::status(said), cx);
    }

    /// The open popup's cursor and rows; `None` while it is closed.
    #[cfg(test)]
    pub(super) fn rules_state(&self) -> Option<(usize, Vec<RuleRow>)> {
        self.rules.map(|p| (p.cursor, self.chrome.rules.clone()))
    }
}
