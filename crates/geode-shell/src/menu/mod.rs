//! One model and one renderer for a tile's `.` action menu.
//!
//! Rows are built by the module when the menu opens, when its content
//! changes, and when the keymap is republished — never in paint. A row's
//! key hint is an action identity resolved to keystrokes through the live
//! keymap (`tips::Chords`) at that moment, so a user rebind shows. A
//! disabled row keeps its reason: the lane shows it (or its short form) and a
//! pick returns it for the tile's notice. Keyboard stepping lands only on
//! enabled actions; from a separator, a section or no cursor it lands on the
//! first enabled action. An all-disabled menu has no cursor.
//!
//! In a tile's menu, the keys that step are geode-tile's shared
//! `motion::menu_down`/`menu_up` (the shell's builtin bindings under
//! `tilelist`, which the tile publishes while its menu is open); what picks
//! and closes stays the module's own. The module maps both onto
//! [`Menu::step`] and [`Menu::pick`]. The shell's row menu reads its keys
//! itself (`handle_row_menu_key`): bare `j`/`down` and `k`/`up` step,
//! `enter` picks, `escape` closes.
//!
//! The shell owns the menu; geode-tile re-exports it as `geode_tile::menu`.

mod paint;
mod render;

pub use paint::{MenuPaint, RowPaint, row_paint};
pub use render::{MenuHost, MenuIds, TICK_SLOT, render_menu};

use std::sync::Arc;

use crate::actions::ActionId;
use crate::keymap::{Binding, Keystroke};
use crate::tips::{Chords, chord_for};
use gpui::{App, SharedString};

/// A module's pick type: what a row does when picked. `element_name` names
/// the row's element; it must be unique within one menu and stable across
/// rebuilds of the same row, so a rebuilt menu keeps each row's hover state.
pub trait MenuPick: Clone + PartialEq + std::fmt::Debug + 'static {
    fn element_name(&self) -> SharedString;
}

impl MenuPick for ActionId {
    fn element_name(&self) -> SharedString {
        SharedString::from(self.0.clone())
    }
}

/// What an enabled row's trailing lane names, before the keymap is read.
#[derive(Clone, Debug, PartialEq)]
pub enum Hint {
    /// Nothing trails the title.
    None,
    /// The action's live chord; `unbound` when the keymap binds none.
    Chord {
        action: SharedString,
        unbound: Unbound,
    },
    /// Text that is not a key (a preset's `1w`): painted as text.
    Label(SharedString),
}

impl Hint {
    /// The live chord, or an empty lane when unbound.
    pub fn chord(action: &'static str) -> Hint {
        Hint::Chord {
            action: SharedString::new_static(action),
            unbound: Unbound::Blank,
        }
    }

    /// The live chord, or the `:` verb that reaches the same action.
    pub fn chord_or_verb(action: &'static str, verb: &'static str) -> Hint {
        Hint::Chord {
            action: SharedString::new_static(action),
            unbound: Unbound::Verb(SharedString::new_static(verb)),
        }
    }

    pub fn label(text: &'static str) -> Hint {
        Hint::Label(SharedString::new_static(text))
    }
}

/// What a [`Hint::Chord`] shows when the keymap binds the action nowhere.
#[derive(Clone, Debug, PartialEq)]
pub enum Unbound {
    Blank,
    Verb(SharedString),
}

/// A hint resolved against the keymap: what an enabled row's lane holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Lane {
    #[default]
    Empty,
    Keys(Vec<Keystroke>),
    Text(SharedString),
}

/// What a row's trailing lane paints this frame: keys as `Kbd`, or text.
#[derive(Clone, Debug, PartialEq)]
pub enum Trailing<'a> {
    None,
    Keys(&'a [Keystroke]),
    Text(&'a SharedString),
}

/// One pickable row of a menu.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionRow<P> {
    pick: P,
    name: SharedString,
    title: SharedString,
    hint: Hint,
    lane: Lane,
    enabled: Result<(), SharedString>,
    short_reason: Option<SharedString>,
    checked: Option<bool>,
}

impl<P: MenuPick> ActionRow<P> {
    pub fn new(pick: P, title: impl Into<SharedString>) -> Self {
        let name = pick.element_name();
        Self {
            pick,
            name,
            title: title.into(),
            hint: Hint::None,
            lane: Lane::Empty,
            enabled: Ok(()),
            short_reason: None,
            checked: None,
        }
    }
}

impl<P> ActionRow<P> {
    pub fn hint(mut self, hint: Hint) -> Self {
        self.hint = hint;
        self
    }

    /// `Err` is why the row cannot be picked: the lane shows it and a pick
    /// returns it.
    pub fn enabled(mut self, enabled: Result<(), SharedString>) -> Self {
        self.enabled = enabled;
        self
    }

    /// A disabled row's lane text when its reason is a sentence too long for
    /// the lane; a pick still returns the whole reason.
    pub fn short_reason(mut self, reason: &'static str) -> Self {
        self.short_reason = Some(SharedString::new_static(reason));
        self
    }

    /// A toggle's or a choice's state: a tick, or a same-width blank, ahead
    /// of the title so a group's titles align.
    pub fn checked(mut self, on: bool) -> Self {
        self.checked = Some(on);
        self
    }

    pub fn pick(&self) -> &P {
        &self.pick
    }

    pub fn name(&self) -> &SharedString {
        &self.name
    }

    pub fn title(&self) -> &SharedString {
        &self.title
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.is_ok()
    }

    pub fn reason(&self) -> Option<&SharedString> {
        self.enabled.as_ref().err()
    }

    /// The row's tick slot: `None` when the row has none (not a toggle or a
    /// choice), else whether it is ticked. Not `checked`: that name is the
    /// builder's, and one type cannot carry both.
    pub fn tick(&self) -> Option<bool> {
        self.checked
    }

    pub fn lane(&self) -> &Lane {
        &self.lane
    }

    pub fn trailing(&self) -> Trailing<'_> {
        match (&self.enabled, &self.lane) {
            (Err(reason), _) => Trailing::Text(self.short_reason.as_ref().unwrap_or(reason)),
            (Ok(()), Lane::Empty) => Trailing::None,
            (Ok(()), Lane::Keys(keys)) => Trailing::Keys(keys),
            (Ok(()), Lane::Text(text)) => Trailing::Text(text),
        }
    }
}

/// One row of a menu.
#[derive(Clone, Debug, PartialEq)]
pub enum Row<P> {
    Action(ActionRow<P>),
    Separator,
    /// A muted heading over the rows that follow.
    Section(SharedString),
}

impl<P> Row<P> {
    pub fn action(&self) -> Option<&ActionRow<P>> {
        match self {
            Row::Action(a) => Some(a),
            _ => None,
        }
    }

    pub fn is_action(&self) -> bool {
        matches!(self, Row::Action(_))
    }

    /// Whether keyboard stepping may land here: an enabled action.
    pub fn lands(&self) -> bool {
        matches!(self, Row::Action(a) if a.enabled.is_ok())
    }
}

/// The first row keyboard stepping may land on; `None` when every action is
/// disabled.
pub fn first_enabled<P>(rows: &[Row<P>]) -> Option<usize> {
    rows.iter().position(Row::lands)
}

/// Move `delta` enabled actions from `from`, skipping disabled actions,
/// separators and sections, clamped at either end. From a row that is not
/// an action (or no row), land on the first enabled action. From a disabled
/// action the pointer left lit, search from there; with nothing enabled in
/// the requested direction, stay.
pub fn step<P>(rows: &[Row<P>], from: Option<usize>, delta: isize) -> Option<usize> {
    let Some(from) = from.filter(|&i| rows.get(i).is_some_and(Row::is_action)) else {
        return first_enabled(rows);
    };
    let mut at = from;
    for _ in 0..delta.unsigned_abs() {
        let next = if delta > 0 {
            (at + 1..rows.len()).find(|&i| rows[i].lands())
        } else {
            (0..at).rev().find(|&i| rows[i].lands())
        };
        match next {
            Some(i) => at = i,
            None => break,
        }
    }
    Some(at)
}

/// Where a highlight goes when a menu's rows are rebuilt under it: kept on an
/// action row, else the nearest action before it, else after it. Disabled
/// actions qualify (a pointer may have lit one). Past the end clamps first.
pub fn snap<P>(rows: &[Row<P>], at: Option<usize>) -> Option<usize> {
    let Some(at) = at else {
        return first_enabled(rows);
    };
    let at = at.min(rows.len().saturating_sub(1));
    (0..=at)
        .rev()
        .find(|&i| rows.get(i).is_some_and(Row::is_action))
        .or_else(|| (at..rows.len()).find(|&i| rows[i].is_action()))
}

fn resolve(hint: &Hint, bindings: &[Binding]) -> Lane {
    match hint {
        Hint::None => Lane::Empty,
        Hint::Label(text) => Lane::Text(text.clone()),
        Hint::Chord { action, unbound } => match chord_for(bindings, action) {
            Some(keys) => Lane::Keys(keys),
            None => match unbound {
                Unbound::Blank => Lane::Empty,
                Unbound::Verb(verb) => Lane::Text(verb.clone()),
            },
        },
    }
}

fn resolve_all<P>(rows: &mut [Row<P>], bindings: &[Binding]) {
    for row in rows {
        if let Row::Action(a) = row {
            a.lane = resolve(&a.hint, bindings);
        }
    }
}

/// An open menu: its prepared rows and the one highlight keyboard and
/// pointer share.
#[derive(Clone, Debug, PartialEq)]
pub struct Menu<P> {
    rows: Vec<Row<P>>,
    highlighted: Option<usize>,
}

impl<P: MenuPick> Menu<P> {
    /// `rows` with their hints resolved against `bindings`, the highlight on
    /// the first enabled action.
    pub fn new(mut rows: Vec<Row<P>>, bindings: &[Binding]) -> Self {
        resolve_all(&mut rows, bindings);
        let highlighted = first_enabled(&rows);
        Self { rows, highlighted }
    }

    /// Open with the highlight on `at` (a module's own start rule, such as
    /// the value in force) when it is an action row.
    pub fn open_at(mut self, at: Option<usize>) -> Self {
        if let Some(i) = at.filter(|&i| self.rows.get(i).is_some_and(Row::is_action)) {
            self.highlighted = Some(i);
        }
        self
    }

    pub fn rows(&self) -> &[Row<P>] {
        &self.rows
    }

    pub fn highlighted(&self) -> Option<usize> {
        self.highlighted
    }

    pub fn step(&mut self, delta: isize) {
        self.highlighted = step(&self.rows, self.highlighted, delta);
    }

    /// The pointer's row: the mouse form of stepping. Only action rows take
    /// it (disabled ones too: a hover is a hover, and a pick on one gives its
    /// reason). Answers whether it moved, so a caller notifies only on a
    /// change: gpui fires a row's mouse-move on every pointer move over it.
    pub fn highlight(&mut self, index: usize) -> bool {
        if self.highlighted == Some(index) || !self.rows.get(index).is_some_and(Row::is_action) {
            return false;
        }
        self.highlighted = Some(index);
        true
    }

    /// Row `index` picked: `Ok` with its pick when enabled, `Err` with its
    /// whole reason when disabled (never the pick: a disabled row must not
    /// also run the verb its reason refuses), `None` for structure.
    pub fn pick(&self, index: usize) -> Option<Result<P, SharedString>> {
        let Some(Row::Action(action)) = self.rows.get(index) else {
            return None;
        };
        Some(match &action.enabled {
            Ok(()) => Ok(action.pick.clone()),
            Err(reason) => Err(reason.clone()),
        })
    }

    /// Rows rebuilt under an open menu (a delivery, a reload, a `:` line):
    /// hints resolved, the highlight snapped. Answers whether the rows moved.
    pub fn replace_rows(&mut self, mut rows: Vec<Row<P>>, bindings: &[Binding]) -> bool {
        resolve_all(&mut rows, bindings);
        let moved = rows != self.rows;
        self.rows = rows;
        self.highlighted = snap(&self.rows, self.highlighted);
        moved
    }

    /// Re-resolve every hint against a republished keymap.
    pub fn rehint(&mut self, bindings: &[Binding]) {
        resolve_all(&mut self.rows, bindings);
    }
}

/// The keymap as the shell last published it; empty before it does (a
/// test with no `Chords`, a module constructed before the shell's first
/// publish). Cloning the `Arc` copies no binding.
pub fn live_bindings(cx: &App) -> Arc<Vec<Binding>> {
    cx.try_global::<Chords>()
        .map(|c| Arc::clone(&c.0))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{ActionDef, ActionRegistry};
    use crate::defaults::default_mod;
    use crate::keymap::{Modifiers, build_keymap, parse_binding};
    use geode_core::config::{Layer, LayerDoc};

    #[derive(Clone, Debug, PartialEq)]
    pub(crate) struct Id(pub &'static str);

    impl MenuPick for Id {
        fn element_name(&self) -> SharedString {
            SharedString::new_static(self.0)
        }
    }

    fn act(id: &'static str) -> Row<Id> {
        Row::Action(ActionRow::new(Id(id), id))
    }

    fn off(id: &'static str) -> Row<Id> {
        Row::Action(ActionRow::new(Id(id), id).enabled(Err("not now".into())))
    }

    /// 0 a, 1 —, 2 b (disabled), 3 c, 4 [View], 5 d.
    fn mixed() -> Vec<Row<Id>> {
        vec![
            act("a"),
            Row::Separator,
            off("b"),
            act("c"),
            Row::Section("View".into()),
            act("d"),
        ]
    }

    #[test]
    fn stepping_skips_separators_and_sections_and_clamps() {
        let rows = vec![
            act("a"),
            Row::Separator,
            act("b"),
            Row::Section("S".into()),
            act("c"),
        ];
        assert_eq!(step(&rows, Some(0), 1), Some(2));
        assert_eq!(step(&rows, Some(2), 1), Some(4), "over the section");
        assert_eq!(step(&rows, Some(4), 1), Some(4), "clamped at the end");
        assert_eq!(step(&rows, Some(4), -2), Some(0));
        assert_eq!(step(&rows, Some(0), -1), Some(0), "clamped at the start");
        assert_eq!(step(&rows, Some(0), 7), Some(4));
    }

    #[test]
    fn stepping_skips_disabled_actions() {
        let rows = mixed();
        assert_eq!(step(&rows, Some(0), 1), Some(3), "over the separator and b");
        assert_eq!(step(&rows, Some(3), -1), Some(0));
        assert_eq!(
            step(&rows, Some(2), 1),
            Some(3),
            "from a pointer-lit disabled row"
        );
        assert_eq!(step(&rows, Some(2), -1), Some(0));
        assert_eq!(
            step(&rows, Some(2), 0),
            Some(2),
            "a zero step keeps an action row"
        );
    }

    #[test]
    fn stepping_from_a_non_action_row_lands_on_the_first_enabled_action() {
        let rows = mixed();
        assert_eq!(step(&rows, Some(1), 1), Some(0), "from the separator");
        assert_eq!(step(&rows, Some(4), -1), Some(0), "from the section");
        assert_eq!(step(&rows, None, 1), Some(0), "from no cursor");
        assert_eq!(step(&rows, Some(99), 1), Some(0), "from past the end");
    }

    #[test]
    fn an_all_disabled_menu_has_no_cursor() {
        let rows = vec![Row::Section("S".into()), off("a"), Row::Separator, off("b")];
        assert_eq!(first_enabled(&rows), None);
        assert_eq!(step(&rows, None, 1), None);
        let menu = Menu::new(rows, &[]);
        assert_eq!(menu.highlighted(), None);
    }

    #[test]
    fn snap_keeps_or_finds_the_nearest_action() {
        let rows = mixed();
        assert_eq!(snap(&rows, Some(3)), Some(3), "an action row stays");
        assert_eq!(snap(&rows, Some(2)), Some(2), "a disabled action stays");
        assert_eq!(snap(&rows, Some(4)), Some(3), "a section gives way upward");
        assert_eq!(
            snap(&rows, Some(99)),
            Some(5),
            "past the end clamps to the last row"
        );
        assert_eq!(snap(&rows, None), Some(0));
        let lead = vec![Row::Section("S".into()), act("a")];
        assert_eq!(
            snap(&lead, Some(0)),
            Some(1),
            "nothing before it: the next one"
        );
    }

    #[test]
    fn highlight_moves_only_onto_action_rows_and_reports_a_change() {
        let mut menu = Menu::new(mixed(), &[]);
        assert_eq!(menu.highlighted(), Some(0));
        assert!(!menu.highlight(0), "no change, no report");
        assert!(!menu.highlight(1), "a separator takes no highlight");
        assert!(!menu.highlight(4), "a section takes no highlight");
        assert!(
            menu.highlight(2),
            "a disabled action does (a hover is a hover)"
        );
        assert_eq!(menu.highlighted(), Some(2));
    }

    #[test]
    fn a_pick_of_a_disabled_row_is_its_reason() {
        let menu = Menu::new(mixed(), &[]);
        assert_eq!(menu.pick(0), Some(Ok(Id("a"))));
        assert_eq!(menu.pick(2), Some(Err(SharedString::from("not now"))));
        assert_eq!(menu.pick(1), None, "structure picks nothing");
        assert_eq!(menu.pick(99), None);
    }

    fn registry(ids: &[&str]) -> ActionRegistry {
        let mut r = ActionRegistry::default();
        for id in ids {
            r.register(ActionDef {
                id: ActionId(id.to_string()),
                title: id.to_string(),
                category: "Demo".into(),
            })
            .unwrap();
        }
        r
    }

    pub(crate) fn doc(layer: Layer, text: &str) -> LayerDoc {
        LayerDoc {
            layer,
            name: "keymap".to_string(),
            file: format!("{}/keymap.toml", layer.name()).into(),
            table: text.parse().unwrap(),
        }
    }

    pub(crate) fn bindings(user: Option<&str>) -> Vec<Binding> {
        let mut docs = vec![doc(
            Layer::Builtin,
            "[[bindings]]\n[bindings.keys]\n\"a\" = \"demo::alpha\"\n",
        )];
        if let Some(text) = user {
            docs.push(doc(Layer::User, text));
        }
        let (keymap, diags) = build_keymap(&docs, default_mod(), &registry(&["demo::alpha"]));
        assert!(diags.is_empty(), "{diags:?}");
        keymap.bindings().to_vec()
    }

    fn keys(spec: &str) -> Vec<Keystroke> {
        parse_binding(spec, Modifiers::NONE).unwrap()
    }

    #[test]
    fn a_hint_resolves_to_the_live_chord_or_its_unbound_form() {
        let rows = vec![
            Row::Action(ActionRow::new(Id("a"), "Alpha").hint(Hint::chord("demo::alpha"))),
            Row::Action(
                ActionRow::new(Id("v"), "Verb").hint(Hint::chord_or_verb("demo::verb", ":verb")),
            ),
            Row::Action(ActionRow::new(Id("u"), "Unbound").hint(Hint::chord("demo::unbound"))),
            Row::Action(ActionRow::new(Id("l"), "1 week").hint(Hint::label("1w"))),
        ];
        let menu = Menu::new(rows, &bindings(None));
        let lanes: Vec<Lane> = menu
            .rows()
            .iter()
            .map(|r| r.action().unwrap().lane().clone())
            .collect();
        assert_eq!(
            lanes,
            vec![
                Lane::Keys(keys("a")),
                Lane::Text(":verb".into()),
                Lane::Empty,
                Lane::Text("1w".into()),
            ]
        );
    }

    #[test]
    fn a_disabled_row_trails_its_short_reason_else_its_reason() {
        let short = ActionRow::new(Id("f"), "1 minute")
            .hint(Hint::label("1m"))
            .enabled(Err(
                "1m over 1y is 525,600 points; the cap is 500,000".into()
            ))
            .short_reason("over cap");
        let long = ActionRow::new(Id("g"), "Group").enabled(Err("not in a package".into()));
        let label = ActionRow::new(Id("l"), "1 week").hint(Hint::label("1w"));
        let menu = Menu::new(
            vec![Row::Action(short), Row::Action(long), Row::Action(label)],
            &[],
        );
        let trail = |i: usize| match menu.rows()[i].action().unwrap().trailing() {
            Trailing::Text(t) => t.to_string(),
            Trailing::Keys(k) => format!("{k:?}"),
            Trailing::None => String::new(),
        };
        assert_eq!(trail(0), "over cap");
        assert_eq!(trail(1), "not in a package");
        assert_eq!(trail(2), "1w", "a label is text, not a key");
        assert_eq!(
            menu.pick(0),
            Some(Err(SharedString::from(
                "1m over 1y is 525,600 points; the cap is 500,000"
            ))),
            "the pick gives the whole reason"
        );
    }

    #[test]
    fn replace_rows_resolves_hints_and_snaps_the_highlight() {
        let mut menu = Menu::new(mixed(), &[]);
        assert!(menu.highlight(5));
        let fewer = vec![act("a"), Row::Separator, act("c")];
        assert!(menu.replace_rows(fewer.clone(), &[]), "the rows moved");
        assert_eq!(menu.highlighted(), Some(2), "clamped onto the last action");
        assert!(!menu.replace_rows(fewer, &[]), "the same rows did not move");
    }
}
