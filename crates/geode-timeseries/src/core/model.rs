//! The tile's pure state (spec §9.2). Every verb returns a [`Changed`]
//! bitset so the tile knows what to do next — fetch, requery, repaint,
//! persist — and the tests can assert it. The key table and the `:`
//! line are two front ends over these methods.

use std::ops::{BitOr, BitOrAssign};

use chrono::{DateTime, Utc};
use geode_chart::core::layout::{SPLIT_DEFAULT, SPLIT_MAX, SPLIT_MIN};
use geode_chart::core::palette::Palette;
use geode_chart::{Axis, AxisMode, MAX_DENSITY_QUADS, View};
use geode_core::query::AsOf;
use geode_core::series::expr::Expr;
use geode_core::series::{
    BucketRule, Frequency, MAX_BINS, MIN_BINS, SERIES_POINT_CAP, SlotKind, cap_message,
};

use super::range::Range;
use super::rgb::Rgb8;

pub const LABEL_MAX: usize = 24;
pub const SPLIT_STEP: f32 = 0.05;
pub const PAN_FRACTION: f64 = 0.1;
pub const ZOOM_FACTOR: f64 = 1.25;
pub const DEFAULT_BINS: u32 = 40;
pub const DEFAULT_PERCENTILES: [f64; 3] = [0.05, 0.5, 0.95];

/// What a verb changed: the QUERY (requery), a FETCH (the range moved),
/// the CHROME (repaint, rebuild the chart model), the SESSION (persist).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Changed(u8);

impl Changed {
    pub const NONE: Changed = Changed(0);
    pub const QUERY: Changed = Changed(1);
    pub const FETCH: Changed = Changed(2);
    pub const CHROME: Changed = Changed(4);
    pub const SESSION: Changed = Changed(8);
    pub fn query(self) -> bool {
        self.0 & 1 != 0
    }
    pub fn fetch(self) -> bool {
        self.0 & 2 != 0
    }
    pub fn chrome(self) -> bool {
        self.0 & 4 != 0
    }
    pub fn session(self) -> bool {
        self.0 & 8 != 0
    }
    pub fn is_none(self) -> bool {
        self.0 == 0
    }
}
impl BitOr for Changed {
    type Output = Changed;
    fn bitor(self, o: Changed) -> Changed {
        Changed(self.0 | o.0)
    }
}
impl BitOrAssign for Changed {
    fn bitor_assign(&mut self, o: Changed) {
        self.0 |= o.0
    }
}

const ALL: Changed = Changed(15);
const SETTING: Changed = Changed(1 | 4 | 8); // QUERY | CHROME | SESSION
const LOOK: Changed = Changed(4 | 8); // CHROME | SESSION

/// A slot's color. `Palette` and `Named` follow the theme; `Custom` is
/// absolute — painted exactly as picked, with no readability floor, so
/// it can disappear on a theme it was not picked against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Color {
    Palette(usize),
    Named(String),
    Custom(Rgb8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotState {
    Idle,
    Fetching,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Slot {
    pub number: u8,
    pub kind: SlotKind,
    /// The expression as typed; `None` for a source slot.
    pub text: Option<String>,
    pub color: Color,
    pub axis: Axis,
    pub visible: bool,
    pub state: SlotState,
    /// An expression restored from a session saved when expressions
    /// named slots by handle, whose text could not be rewritten to
    /// names. It keeps its original text, is `Failed` with the reason,
    /// holds no operands and is never sent; the session writes it back
    /// marked so a later restore tries the rewrite again. An edit
    /// (`replace_expr`) clears it.
    pub legacy: bool,
}

impl Slot {
    /// The chip/popup label (§9.3), and the name `:` commands and
    /// expressions use for a source series: the identity, `@source` only
    /// when the source is not the default. An expression's label is its
    /// text, cut to `LABEL_MAX` characters ending in `…`.
    pub fn label(&self, default_source: Option<&str>) -> String {
        match &self.kind {
            SlotKind::Source {
                source, identity, ..
            } => {
                if Some(source.as_str()) == default_source {
                    identity.clone()
                } else {
                    format!("{identity}@{source}")
                }
            }
            SlotKind::Expr(_) => {
                let text = self.text.as_deref().unwrap_or("");
                if text.chars().count() > LABEL_MAX {
                    let mut cut: String = text.chars().take(LABEL_MAX - 1).collect();
                    cut.push('…');
                    cut
                } else {
                    text.to_string()
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Removal {
    pub removed: Vec<u8>,
    pub changed: Changed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    slots: Vec<Slot>,
    cursor: Option<usize>,
    range: Range,
    frequency: Frequency,
    axis_mode: AxisMode,
    split: f32,
    density: Option<u32>,
    /// Remembered across a toggle so `D` twice returns to the same count.
    last_bins: u32,
    percentiles: Vec<f64>,
    last_percentiles: Vec<f64>,
    view: View,
    full: (f64, f64),
    dataset: Option<String>,
    next_number: u8,
    notice: Option<String>,
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

impl Model {
    pub fn new() -> Model {
        Model {
            slots: Vec::new(),
            cursor: None,
            range: Range::default(),
            frequency: Frequency::D1,
            axis_mode: AxisMode::Session,
            split: SPLIT_DEFAULT,
            density: Some(DEFAULT_BINS),
            last_bins: DEFAULT_BINS,
            percentiles: DEFAULT_PERCENTILES.to_vec(),
            last_percentiles: DEFAULT_PERCENTILES.to_vec(),
            view: View::full((0.0, 0.0)),
            full: (0.0, 0.0),
            dataset: None,
            next_number: 1,
            notice: None,
        }
    }

    // ---- readers ----
    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }
    /// For `session::from_table`, to clear a restored slot's state
    /// (§9.11: slot state is not persisted).
    pub(crate) fn slots_mut(&mut self) -> &mut [Slot] {
        &mut self.slots
    }
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }
    pub fn cursor_slot(&self) -> Option<&Slot> {
        self.cursor.and_then(|i| self.slots.get(i))
    }
    pub fn range(&self) -> &Range {
        &self.range
    }
    pub fn frequency(&self) -> Frequency {
        self.frequency
    }
    pub fn axis_mode(&self) -> AxisMode {
        self.axis_mode
    }
    pub fn split(&self) -> f32 {
        self.split
    }
    pub fn density(&self) -> Option<u32> {
        self.density
    }
    pub fn percentiles(&self) -> &[f64] {
        &self.percentiles
    }
    pub fn view(&self) -> View {
        self.view
    }
    pub fn full(&self) -> (f64, f64) {
        self.full
    }
    pub fn dataset(&self) -> Option<&str> {
        self.dataset.as_deref()
    }
    pub fn stats_on(&self) -> bool {
        self.density.is_some() || !self.percentiles.is_empty()
    }
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }
    pub fn slot_by_number(&self, number: u8) -> Option<&Slot> {
        self.slots.iter().find(|s| s.number == number)
    }
    pub fn index_of(&self, number: u8) -> Option<usize> {
        self.slots.iter().position(|s| s.number == number)
    }
    pub fn source_slots(&self) -> impl Iterator<Item = (u8, &str, &str)> {
        self.slots.iter().filter_map(|s| match &s.kind {
            SlotKind::Source {
                source, identity, ..
            } => Some((s.number, source.as_str(), identity.as_str())),
            SlotKind::Expr(_) => None,
        })
    }
    pub fn holds_pair(&self, source: &str, identity: &str) -> bool {
        self.source_slots()
            .any(|(_, s, i)| s == source && i == identity)
    }
    /// `1y · 1d` or `2025-01-01 → 2026-09-19 · 1h` (§9.3).
    pub fn header_text(&self) -> String {
        format!("{} · {}", self.range.label(), self.frequency.as_str())
    }
    /// The label of the slot at `index` ([`Slot::label`]).
    pub fn label(&self, index: usize, default_source: Option<&str>) -> String {
        self.slots[index].label(default_source)
    }
    /// The slot a `:` command acts on: the named source series, or the
    /// selected slot when no name is given. An expression has no name,
    /// so it is reachable only by selection.
    pub fn target(&self, name: Option<&str>, default_source: Option<&str>) -> Result<u8, String> {
        match name {
            Some(n) => super::resolve::find_named(n, &self.slots, default_source),
            None => self
                .cursor_slot()
                .map(|s| s.number)
                .ok_or_else(|| "select a series or name one".into()),
        }
    }
    /// Every source series' label, once each, in slot order: the names
    /// completion offers where a `:` command takes a series.
    pub fn series_names(&self, default_source: Option<&str>) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in &self.slots {
            if matches!(s.kind, SlotKind::Source { .. }) {
                let label = s.label(default_source);
                if !out.contains(&label) {
                    out.push(label);
                }
            }
        }
        out
    }
    fn gone() -> String {
        "that series is gone".into()
    }

    // ---- slots ----
    fn take_number(&mut self) -> Result<u8, String> {
        if self.next_number == u8::MAX {
            return Err("this tile has used every slot number; `:clear` starts again".into());
        }
        let n = self.next_number;
        self.next_number += 1;
        Ok(n)
    }
    /// For `session::from_table`: a restored slot takes its recorded
    /// number, so `add_source`/`add_expr` then bump `next_number` past it.
    pub(crate) fn set_next_number(&mut self, n: u8) {
        self.next_number = n;
    }
    fn push(&mut self, slot: Slot) {
        self.slots.push(slot);
        self.cursor = Some(self.slots.len() - 1);
    }
    fn palette_next(&self) -> usize {
        self.slots.len() % Palette::LEN
    }

    pub fn add_source(
        &mut self,
        identity: &str,
        source: &str,
        dataset: &str,
    ) -> Result<(u8, Changed), String> {
        if let Some(d) = &self.dataset
            && d != dataset
        {
            return Err(format!(
                "this tile plots '{d}'; '{source}' feeds '{dataset}'"
            ));
        }
        let number = self.take_number()?;
        let color = Color::Palette(self.palette_next());
        self.dataset.get_or_insert_with(|| dataset.to_string());
        self.push(Slot {
            number,
            kind: SlotKind::Source {
                source: source.into(),
                identity: identity.into(),
                rule: BucketRule::Last,
            },
            text: None,
            color,
            axis: Axis::Left,
            visible: true,
            state: SlotState::Fetching,
            legacy: false,
        });
        let mut changed = Changed::FETCH | LOOK;
        changed |= self.enforce_density_budget();
        Ok((number, changed))
    }

    /// `expr` is already resolved (`core::resolve`); the model only files it.
    pub fn add_expr(&mut self, text: &str, expr: Expr) -> Result<(u8, Changed), String> {
        let number = self.take_number()?;
        let color = Color::Palette(self.palette_next());
        self.push(Slot {
            number,
            kind: SlotKind::Expr(expr),
            text: Some(text.into()),
            color,
            axis: Axis::Left,
            visible: true,
            state: SlotState::Idle,
            legacy: false,
        });
        Ok((number, SETTING | self.enforce_density_budget()))
    }

    /// For `session::from_table`: a saved expression whose handle text
    /// could not be rewritten to names ([`Slot::legacy`]). Its kind is a
    /// reference-free placeholder that `request::params` never sends.
    pub(crate) fn add_legacy_expr(&mut self, text: &str, why: String) -> Result<u8, String> {
        let number = self.take_number()?;
        let color = Color::Palette(self.palette_next());
        self.push(Slot {
            number,
            kind: SlotKind::Expr(Expr::Num(0.0)),
            text: Some(text.into()),
            color,
            axis: Axis::Left,
            visible: true,
            state: SlotState::Failed(why),
            legacy: true,
        });
        Ok(number)
    }

    pub fn replace_expr(&mut self, number: u8, text: &str, expr: Expr) -> Result<Changed, String> {
        let i = self.index_of(number).ok_or_else(Self::gone)?;
        let slot = &mut self.slots[i];
        if !matches!(slot.kind, SlotKind::Expr(_)) {
            return Err(format!("{} is not an expression", slot.label(None)));
        }
        slot.kind = SlotKind::Expr(expr);
        slot.text = Some(text.into());
        slot.legacy = false;
        slot.state = SlotState::Idle;
        Ok(SETTING)
    }

    /// Every expression that references `number` (§7: "removing an
    /// operand removes every expression that references it"). Only a
    /// source slot has dependants: an expression names sources only.
    pub fn dependants(&self, number: u8) -> Vec<u8> {
        self.slots
            .iter()
            .filter(|s| matches!(&s.kind, SlotKind::Expr(e) if e.slots().contains(&number)))
            .map(|s| s.number)
            .collect()
    }

    pub fn remove(&mut self, number: u8) -> Result<Removal, String> {
        if self.index_of(number).is_none() {
            return Err(Self::gone());
        }
        let mut removed = self.dependants(number);
        removed.insert(0, number);
        self.slots.retain(|s| !removed.contains(&s.number));
        self.cursor = if self.slots.is_empty() {
            None
        } else {
            Some(self.cursor.unwrap_or(0).min(self.slots.len() - 1))
        };
        if self.source_slots().next().is_none() {
            self.dataset = None;
        }
        Ok(Removal {
            removed,
            changed: SETTING,
        })
    }

    /// A cleared tile is a fresh tile, numbering included: no slot
    /// remains for slot 1 to collide with, and `take_number`'s own
    /// exhaustion message ("`:clear` starts again") is only true
    /// because of this line.
    pub fn clear(&mut self) -> Changed {
        self.slots.clear();
        self.cursor = None;
        self.dataset = None;
        self.next_number = 1;
        SETTING
    }

    pub fn set_state(&mut self, number: u8, state: SlotState) -> Changed {
        match self.index_of(number) {
            Some(i) => {
                self.slots[i].state = state;
                Changed::CHROME
            }
            None => Changed::NONE,
        }
    }
    pub fn set_pair_state(&mut self, source: &str, identity: &str, state: SlotState) -> Changed {
        let mut changed = Changed::NONE;
        for s in &mut self.slots {
            if let SlotKind::Source {
                source: ss,
                identity: ii,
                ..
            } = &s.kind
                && ss == source
                && ii == identity
            {
                s.state = state.clone();
                changed = Changed::CHROME;
            }
        }
        changed
    }
    /// By slot number rather than by cursor, which is what the
    /// session restore needs: it replays a recorded `visible` onto a
    /// slot it has just added, with no cursor anywhere near it.
    pub fn set_visible(&mut self, number: u8, visible: bool) -> Result<Changed, String> {
        let i = self.index_of(number).ok_or_else(Self::gone)?;
        self.slots[i].visible = visible;
        Ok(LOOK | self.enforce_density_budget())
    }

    // ---- cursor ----
    pub fn set_cursor(&mut self, index: usize) -> Changed {
        if index < self.slots.len() && self.cursor != Some(index) {
            self.cursor = Some(index);
            Changed::CHROME
        } else {
            Changed::NONE
        }
    }
    pub fn cursor_next(&mut self, count: usize) -> Changed {
        self.step_cursor(count as isize)
    }
    pub fn cursor_prev(&mut self, count: usize) -> Changed {
        self.step_cursor(-(count as isize))
    }
    fn step_cursor(&mut self, by: isize) -> Changed {
        let n = self.slots.len() as isize;
        if n == 0 {
            return Changed::NONE;
        }
        let cur = self.cursor.unwrap_or(0) as isize;
        self.cursor = Some(((cur + by).rem_euclid(n)) as usize);
        Changed::CHROME
    }

    // ---- the cursor's slot ----
    fn at_cursor(&mut self) -> Option<&mut Slot> {
        self.cursor.and_then(|i| self.slots.get_mut(i))
    }
    pub fn toggle_visible(&mut self) -> Changed {
        let Some(s) = self.at_cursor() else {
            return Changed::NONE;
        };
        s.visible = !s.visible;
        LOOK | self.enforce_density_budget()
    }
    pub fn cycle_axis(&mut self, forward: bool, count: usize) -> Changed {
        let Some(s) = self.at_cursor() else {
            return Changed::NONE;
        };
        for _ in 0..count.max(1) {
            s.axis = if forward {
                s.axis.next()
            } else {
                s.axis.prev()
            };
        }
        LOOK
    }
    pub fn set_axis(&mut self, number: u8, axis: Axis) -> Result<Changed, String> {
        let i = self.index_of(number).ok_or_else(Self::gone)?;
        self.slots[i].axis = axis;
        Ok(LOOK)
    }
    pub fn cycle_color(&mut self) -> Changed {
        let Some(s) = self.at_cursor() else {
            return Changed::NONE;
        };
        s.color = match &s.color {
            Color::Palette(i) => Color::Palette((i + 1) % Palette::LEN),
            // `c` steps the palette: off a name or an absolute color it
            // starts the palette over.
            Color::Named(_) | Color::Custom(_) => Color::Palette(0),
        };
        LOOK
    }
    pub fn set_color(&mut self, number: u8, color: Color) -> Result<Changed, String> {
        let i = self.index_of(number).ok_or_else(Self::gone)?;
        self.slots[i].color = color;
        Ok(LOOK)
    }
    pub fn cycle_rule(&mut self) -> Changed {
        let Some(s) = self.at_cursor() else {
            return Changed::NONE;
        };
        match &mut s.kind {
            SlotKind::Source { rule, .. } => {
                *rule = rule.next();
                SETTING
            }
            SlotKind::Expr(_) => Changed::NONE,
        }
    }
    pub fn set_rule(&mut self, number: u8, new: BucketRule) -> Result<Changed, String> {
        let i = self.index_of(number).ok_or_else(Self::gone)?;
        match &mut self.slots[i].kind {
            SlotKind::Source { rule, .. } => {
                *rule = new;
                Ok(SETTING)
            }
            SlotKind::Expr(_) => Err(format!(
                "{} is an expression; its rule is its operands'",
                self.slots[i].label(None)
            )),
        }
    }

    // ---- settings ----
    fn check_cap(
        &self,
        frequency: Frequency,
        range: &Range,
        now: DateTime<Utc>,
        as_of: &AsOf,
    ) -> Result<(), String> {
        let (from, to) = range.resolve(now, as_of);
        let points = frequency.buckets_in(from, to);
        if points > SERIES_POINT_CAP {
            Err(cap_message(frequency, from, to, points))
        } else {
            Ok(())
        }
    }
    pub fn set_frequency(
        &mut self,
        f: Frequency,
        now: DateTime<Utc>,
        as_of: &AsOf,
    ) -> Result<Changed, String> {
        self.check_cap(f, &self.range, now, as_of)?;
        if self.frequency == f {
            return Ok(Changed::NONE);
        }
        self.frequency = f;
        Ok(SETTING)
    }
    /// What [`Model::set_frequency`] would refuse `f` with over the range
    /// in force, without writing anything — the frequency menu asks it
    /// per row when it opens, so a row it would refuse is disabled with
    /// this same reason rather than failing when picked.
    pub fn frequency_refusal(
        &self,
        f: Frequency,
        now: DateTime<Utc>,
        as_of: &AsOf,
    ) -> Result<(), String> {
        self.check_cap(f, &self.range, now, as_of)
    }
    /// A range change refetches everything — `fetch_pending` selects
    /// by `SlotState` — so every SOURCE slot's state moves to
    /// `Fetching` on success.
    pub fn set_range(
        &mut self,
        range: Range,
        now: DateTime<Utc>,
        as_of: &AsOf,
    ) -> Result<Changed, String> {
        self.check_cap(self.frequency, &range, now, as_of)?;
        if self.range == range {
            return Ok(Changed::NONE);
        }
        self.range = range;
        for s in &mut self.slots {
            if matches!(s.kind, SlotKind::Source { .. }) {
                s.state = SlotState::Fetching;
            }
        }
        Ok(ALL)
    }
    /// Marks every source slot `Fetching`, for a caller that refetches
    /// everything outside a `set_range` (e.g. a source reconnect).
    pub fn mark_all_fetching(&mut self) -> Changed {
        let mut any = false;
        for s in &mut self.slots {
            if matches!(s.kind, SlotKind::Source { .. }) {
                s.state = SlotState::Fetching;
                any = true;
            }
        }
        if any { Changed::CHROME } else { Changed::NONE }
    }
    pub fn set_axis_mode(&mut self, mode: AxisMode) -> Changed {
        if self.axis_mode == mode {
            Changed::NONE
        } else {
            self.axis_mode = mode;
            LOOK
        }
    }
    pub fn set_split(&mut self, split: f32) -> Result<Changed, String> {
        if !(SPLIT_MIN..=SPLIT_MAX).contains(&split) {
            return Err(format!("split is {SPLIT_MIN}..={SPLIT_MAX}"));
        }
        self.split = split;
        Ok(LOOK)
    }
    pub fn step_split(&mut self, grow: bool, count: usize) -> Changed {
        let d = SPLIT_STEP * count.max(1) as f32;
        self.split =
            (if grow { self.split + d } else { self.split - d }).clamp(SPLIT_MIN, SPLIT_MAX);
        LOOK
    }
    fn visible_slots(&self) -> usize {
        self.slots.iter().filter(|s| s.visible).count()
    }
    fn budget_error(&self, bins: u32, visible: usize) -> Option<String> {
        (bins as usize * visible > MAX_DENSITY_QUADS).then(|| {
            format!(
                "{visible} series × {bins} bins would exceed the {}-bar bound",
                thousands(MAX_DENSITY_QUADS)
            )
        })
    }
    /// After an add or a show: density turns OFF, with a notice, when the
    /// visible slots at the current bin count would overrun the chart's
    /// per-frame quad bound (§8.5: "Part 4 owns the slot count").
    fn enforce_density_budget(&mut self) -> Changed {
        let Some(bins) = self.density else {
            return Changed::NONE;
        };
        match self.budget_error(bins, self.visible_slots()) {
            Some(why) => {
                self.density = None;
                self.notice = Some(format!("density off: {why}"));
                Changed::QUERY
            }
            None => Changed::NONE,
        }
    }
    pub fn set_density(&mut self, bins: Option<u32>) -> Result<Changed, String> {
        if let Some(b) = bins {
            if !(MIN_BINS..=MAX_BINS).contains(&b) {
                return Err(format!("density is {MIN_BINS}..={MAX_BINS} bins, or off"));
            }
            if let Some(why) = self.budget_error(b, self.visible_slots()) {
                return Err(format!("{why}; lower the bins or hide series"));
            }
            self.last_bins = b;
        }
        self.density = bins;
        Ok(SETTING)
    }
    pub fn toggle_density(&mut self) -> Changed {
        match self.density {
            Some(_) => {
                self.density = None;
                SETTING
            }
            None => self
                .set_density(Some(self.last_bins))
                .unwrap_or_else(|why| {
                    self.notice = Some(why);
                    Changed::NONE
                }),
        }
    }
    pub fn set_percentiles(&mut self, mut fractions: Vec<f64>) -> Result<Changed, String> {
        if fractions.iter().any(|f| !(*f > 0.0 && *f < 1.0)) {
            return Err("percentiles are numbers in (0, 100), e.g. 5 50 95".into());
        }
        fractions.sort_by(f64::total_cmp);
        fractions.dedup();
        if !fractions.is_empty() {
            self.last_percentiles = fractions.clone();
        }
        self.percentiles = fractions;
        Ok(SETTING)
    }
    pub fn toggle_percentiles(&mut self) -> Changed {
        if self.percentiles.is_empty() {
            self.percentiles = self.last_percentiles.clone();
        } else {
            self.percentiles.clear();
        }
        SETTING
    }

    // ---- the view ----
    fn view_changed(&self) -> Changed {
        if self.stats_on() {
            Changed::CHROME | Changed::QUERY
        } else {
            Changed::CHROME
        }
    }
    /// A view that was showing the WHOLE prior range (the common case: a
    /// tile that has never been zoomed, or a delivery that just widened
    /// the loaded coverage) follows the new full range so the chart
    /// stays fully zoomed out; a view the trader has since panned or
    /// zoomed is only re-clamped into the new bounds, never reset.
    pub fn set_full(&mut self, full: (f64, f64)) {
        let was_full = self.view.lo <= self.full.0 && self.view.hi >= self.full.1;
        self.full = full;
        if was_full {
            self.view.reset(full);
        } else {
            self.view.pan(0.0, full);
        }
    }
    pub fn pan(&mut self, steps: i32) -> Changed {
        self.view.pan(PAN_FRACTION * steps as f64, self.full);
        self.view_changed()
    }
    pub fn zoom_in(&mut self, count: usize) -> Changed {
        self.view
            .zoom(ZOOM_FACTOR.powi(count.max(1) as i32), 0.5, self.full);
        self.view_changed()
    }
    pub fn zoom_out(&mut self, count: usize) -> Changed {
        self.view
            .zoom(1.0 / ZOOM_FACTOR.powi(count.max(1) as i32), 0.5, self.full);
        self.view_changed()
    }
    pub fn reset_view(&mut self) -> Changed {
        self.view.reset(self.full);
        self.view_changed()
    }
    /// Zoom about a fraction of the visible range: zero anchors the left edge,
    /// one the right. Factors above one zoom in, below one zoom out. Nonpositive
    /// or nonfinite factors leave the view unchanged.
    pub fn zoom_at(&mut self, factor: f64, about: f64) -> Changed {
        self.view.zoom(factor, about, self.full);
        self.view_changed()
    }
    /// The pointer's pan: shift the view by `fraction` of its own width
    /// (negative = left). A drag hands over the dragged distance as a
    /// fraction of the plot's width, so the data follows the pointer
    /// one-to-one; a wheel hands over its pixels the same way.
    pub fn pan_by(&mut self, fraction: f64) -> Changed {
        if !fraction.is_finite() || fraction == 0.0 {
            return Changed::NONE;
        }
        self.view.pan(fraction, self.full);
        self.view_changed()
    }
    pub fn jump_start(&mut self) -> Changed {
        self.view.jump_start(self.full);
        self.view_changed()
    }
    pub fn jump_end(&mut self) -> Changed {
        self.view.jump_end(self.full);
        self.view_changed()
    }
}

/// `2_000` → `"2,000"`. `geode_core::series::group_thousands` and
/// `geode_core::format::group_thousands` are both private, and neither is
/// worth exporting for one message.
fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use geode_chart::{Axis, AxisMode};
    use geode_core::query::AsOf;
    use geode_core::series::{BucketRule, Frequency, SlotKind};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 19, 15, 0, 0).unwrap()
    }

    fn two_sources() -> Model {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        m.add_source("VIX", "demo_kdb", "series").unwrap();
        m
    }

    #[test]
    fn a_new_model_has_the_spec_defaults() {
        let m = Model::new();
        assert!(m.slots().is_empty());
        assert_eq!(m.cursor(), None);
        assert_eq!(*m.range(), Range::Relative(super::super::range::Preset::Y1));
        assert_eq!(m.frequency(), Frequency::D1);
        assert_eq!(m.axis_mode(), AxisMode::Session);
        assert_eq!(m.split(), 0.7);
        assert_eq!(m.density(), Some(DEFAULT_BINS));
        assert_eq!(m.percentiles(), &DEFAULT_PERCENTILES[..]);
        assert_eq!(m.dataset(), None);
        assert_eq!(m.header_text(), "1y · 1d");
    }

    #[test]
    fn adding_a_source_numbers_slots_from_one_marks_them_fetching_and_lands_the_cursor() {
        let mut m = Model::new();
        let (n, ch) = m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        assert_eq!(n, 1);
        assert!(
            ch.fetch() && ch.chrome() && ch.session() && !ch.query(),
            "a fetch, not yet a query (§9.10)"
        );
        assert_eq!(m.cursor(), Some(0));
        assert!(matches!(m.slots()[0].state, SlotState::Fetching));
        assert_eq!(m.slots()[0].color, Color::Palette(0));
        assert_eq!(m.slots()[0].axis, Axis::Left);
        assert_eq!(m.dataset(), Some("series"));
        let (n, _) = m.add_source("VIX", "demo_kdb", "series").unwrap();
        assert_eq!(n, 2);
        assert_eq!(
            m.slots()[1].color,
            Color::Palette(1),
            "each new slot takes the next palette colour"
        );
        assert_eq!(m.cursor(), Some(1));
        // The same pair twice is legitimate (§9.6: a second rule).
        assert_eq!(m.add_source("VIX", "demo_kdb", "series").unwrap().0, 3);
        assert!(m.holds_pair("demo_kdb", "VIX"));
        assert!(!m.holds_pair("demo_rest", "VIX"));
    }

    #[test]
    fn a_source_over_another_dataset_is_refused() {
        let mut m = two_sources();
        let err = m.add_source("X", "other_src", "other").unwrap_err();
        assert_eq!(err, "this tile plots 'series'; 'other_src' feeds 'other'");
        assert_eq!(m.slots().len(), 2);
        // Removing every source slot frees the dataset.
        m.remove(1).unwrap();
        m.remove(2).unwrap();
        assert_eq!(m.dataset(), None);
        assert!(m.add_source("X", "other_src", "other").is_ok());
    }

    #[test]
    fn slot_numbers_are_never_reused_for_the_tiles_life() {
        let mut m = two_sources();
        m.remove(2).unwrap();
        let (n, _) = m.add_source("V2X", "demo_kdb", "series").unwrap();
        assert_eq!(n, 3, "2 was used once and is gone");
    }

    #[test]
    fn cursor_moves_by_count_and_wraps() {
        let mut m = two_sources();
        m.add_source("V2X", "demo_kdb", "series").unwrap();
        assert_eq!(m.cursor(), Some(2));
        m.cursor_next(1);
        assert_eq!(m.cursor(), Some(0), "wraps");
        m.cursor_prev(1);
        assert_eq!(m.cursor(), Some(2));
        m.cursor_next(2);
        assert_eq!(m.cursor(), Some(1));
        m.set_cursor(0);
        assert_eq!(m.cursor(), Some(0));
        assert_eq!(
            Model::new().cursor_next(1),
            Changed::NONE,
            "no slots, nothing moves"
        );
    }

    #[test]
    fn visibility_axis_colour_and_rule_verbs_act_on_the_cursor() {
        let mut m = two_sources();
        let ch = m.toggle_visible();
        assert!(!m.slots()[1].visible);
        assert!(
            ch.chrome() && ch.session() && !ch.query(),
            "hiding is chrome: the query is unchanged"
        );
        m.toggle_visible();
        assert!(m.slots()[1].visible);
        m.cycle_axis(true, 1);
        assert_eq!(m.slots()[1].axis, Axis::Right);
        // Axis::ALL is [Left, Right, BottomLeft, BottomRight]; three
        // forward hops from Right walks past BottomRight and wraps to Left.
        m.cycle_axis(true, 3);
        assert_eq!(
            m.slots()[1].axis,
            Axis::Left,
            "a count of three from Right wraps to Left"
        );
        m.cycle_axis(false, 1);
        assert_eq!(m.slots()[1].axis, Axis::BottomRight);
        m.cycle_color();
        assert_eq!(m.slots()[1].color, Color::Palette(2));
        m.set_color(2, Color::Named("spx".into())).unwrap();
        m.cycle_color();
        assert_eq!(
            m.slots()[1].color,
            Color::Palette(0),
            "cycling off a named colour starts the palette over"
        );
        m.set_color(2, Color::Custom(Rgb8([1, 2, 3]))).unwrap();
        m.cycle_color();
        assert_eq!(
            m.slots()[1].color,
            Color::Palette(0),
            "and so does cycling off an absolute one"
        );
        let ch = m.cycle_rule();
        assert!(ch.query(), "a rule changes the query");
        assert!(matches!(
            &m.slots()[1].kind,
            SlotKind::Source {
                rule: BucketRule::First,
                ..
            }
        ));
        assert!(m.set_rule(9, BucketRule::Max).is_err(), "no such slot");
    }

    #[test]
    fn frequency_and_range_are_pre_checked_against_the_cap() {
        let mut m = two_sources();
        let ch = m.set_frequency(Frequency::H1, now(), &AsOf::Live).unwrap();
        assert!(ch.query() && ch.session() && ch.chrome());
        assert_eq!(m.header_text(), "1y · 1h");
        // 1m over 1y is ~525,600 buckets: over the cap.
        let err = m
            .set_frequency(Frequency::M1, now(), &AsOf::Live)
            .unwrap_err();
        assert!(
            err.starts_with("1m over 1y is 525,600 points; the cap is 500,000"),
            "{err}"
        );
        assert_eq!(m.frequency(), Frequency::H1, "refused in place");
        // The menu's question is the same check, answered without a
        // write.
        assert_eq!(
            m.frequency_refusal(Frequency::M1, now(), &AsOf::Live)
                .unwrap_err(),
            err
        );
        assert_eq!(
            m.frequency_refusal(Frequency::M15, now(), &AsOf::Live),
            Ok(())
        );
        assert_eq!(m.frequency(), Frequency::H1, "asking writes nothing");
        m.set_frequency(Frequency::W1, now(), &AsOf::Live).unwrap();
        // (Controller ruling 4) both source slots are Fetching from
        // add_source already; drop to Idle so the assertion below is
        // meaningful, then check set_range puts them back.
        m.set_pair_state("demo_kdb", "SPX.close", SlotState::Idle);
        m.set_pair_state("demo_kdb", "VIX", SlotState::Idle);
        let ch = m
            .set_range(
                Range::Relative(super::super::range::Preset::W1),
                now(),
                &AsOf::Live,
            )
            .unwrap();
        assert!(
            ch.fetch() && ch.query() && ch.session() && ch.chrome(),
            "a range change fetches AND queries (§9.10)"
        );
        assert!(
            m.slots()
                .iter()
                .all(|s| matches!(s.state, SlotState::Fetching)),
            "a range change refetches every source slot (ruling 4)"
        );
        m.set_frequency(Frequency::M1, now(), &AsOf::Live).unwrap();
        assert!(
            m.set_range(
                Range::Relative(super::super::range::Preset::Y5),
                now(),
                &AsOf::Live
            )
            .is_err()
        );
    }

    #[test]
    fn split_density_and_percentiles_have_bounds() {
        let mut m = two_sources();
        m.step_split(false, 1);
        assert!((m.split() - 0.65).abs() < 1e-6);
        m.step_split(false, 20);
        assert!((m.split() - 0.2).abs() < 1e-6, "clamped to SPLIT_MIN");
        assert!(m.set_split(0.9).is_err());
        assert!(m.set_split(0.75).unwrap().chrome());
        let ch = m.toggle_density();
        assert_eq!(m.density(), None);
        assert!(ch.query());
        m.toggle_density();
        assert_eq!(m.density(), Some(DEFAULT_BINS), "back on at the default");
        assert!(m.set_density(Some(3)).unwrap_err().contains("4"));
        assert!(m.set_density(Some(201)).is_err());
        m.set_density(Some(10)).unwrap();
        m.toggle_density();
        m.toggle_density();
        assert_eq!(m.density(), Some(10), "toggling remembers the last count");
        let ch = m.toggle_percentiles();
        assert!(m.percentiles().is_empty() && ch.query());
        m.toggle_percentiles();
        assert_eq!(m.percentiles(), &DEFAULT_PERCENTILES[..]);
        assert!(m.set_percentiles(vec![0.0]).is_err(), "fractions in (0, 1)");
        assert!(m.set_percentiles(vec![0.5, 1.0]).is_err());
        m.set_percentiles(vec![0.25, 0.75]).unwrap();
        m.toggle_percentiles();
        m.toggle_percentiles();
        assert_eq!(m.percentiles(), &[0.25, 0.75][..]);
    }

    #[test]
    fn density_is_bounded_by_the_chart_quad_budget() {
        let mut m = Model::new();
        for i in 0..11 {
            m.add_source(&format!("s{i}"), "demo_kdb", "series")
                .unwrap();
        }
        // 11 visible × 200 bins = 2,200 > MAX_DENSITY_QUADS.
        let err = m.set_density(Some(200)).unwrap_err();
        assert!(err.contains("2,000"), "{err}");
        m.set_density(Some(180)).unwrap();
        assert_eq!(m.density(), Some(180), "11 × 180 = 1,980 fits");
        // A twelfth visible slot would push 12 × 180 over: density turns
        // off with a notice rather than letting bars silently vanish.
        let (_, ch) = m.add_source("s11", "demo_kdb", "series").unwrap();
        assert_eq!(m.density(), None);
        assert!(ch.query());
        assert_eq!(
            m.take_notice().as_deref(),
            Some("density off: 12 series × 180 bins would exceed the 2,000-bar bound")
        );
    }

    #[test]
    fn view_verbs_are_chrome_plus_a_query_while_stats_are_on() {
        let mut m = two_sources();
        m.set_full((0.0, 100.0));
        let ch = m.pan(1);
        assert!(
            ch.chrome() && ch.query(),
            "percentiles/density are over the visible window (ruling 10)"
        );
        assert_eq!(
            (m.view().lo, m.view().hi),
            (0.0, 100.0),
            "a full view cannot pan"
        );
        m.zoom_in(1);
        assert!((m.view().span() - 80.0).abs() < 1e-9);
        m.pan(-1);
        assert!(
            (m.view().lo - 2.0).abs() < 1e-9,
            "10% of an 80-wide window, clamped at 0 → moved left by 8 then back... exact: lo was 10 after a centred zoom; -8 → 2"
        );
        m.jump_end();
        assert_eq!(m.view().hi, 100.0);
        m.jump_start();
        assert_eq!(m.view().lo, 0.0);
        m.reset_view();
        assert_eq!((m.view().lo, m.view().hi), (0.0, 100.0));
        m.toggle_density();
        m.toggle_percentiles();
        let ch = m.zoom_out(1);
        assert!(
            ch.chrome() && !ch.query(),
            "with both off a view move asks nothing"
        );
        m.zoom_in(1);
        m.set_full((0.0, 50.0));
        assert!(m.view().hi <= 50.0, "set_full re-clamps the view");
    }

    #[test]
    fn a_pointer_zoom_keeps_its_point_still_and_a_pointer_pan_moves_by_a_fraction() {
        let mut m = two_sources();
        m.set_full((0.0, 100.0));
        // Zoom in by 2 about the right edge: the right edge stays put.
        let ch = m.zoom_at(2.0, 1.0);
        assert!(ch.chrome());
        assert_eq!((m.view().lo, m.view().hi), (50.0, 100.0));
        // Zoom out by the same about the LEFT edge of that window: the
        // left edge stays and the width doubles, clamped to the full.
        m.zoom_at(0.5, 0.0);
        assert_eq!((m.view().lo, m.view().hi), (0.0, 100.0));
        m.zoom_at(4.0, 0.5);
        assert_eq!((m.view().lo, m.view().hi), (37.5, 62.5));
        // A drag of a tenth of the plot moves a tenth of the window.
        m.pan_by(-0.1);
        assert!((m.view().lo - 35.0).abs() < 1e-9, "{}", m.view().lo);
        assert!((m.view().hi - 60.0).abs() < 1e-9);
        // Nothing to do answers nothing.
        assert!(m.pan_by(0.0).is_none());
        assert!(m.pan_by(f64::NAN).is_none());
        let before = m.view();
        m.zoom_at(f64::NAN, 0.5);
        assert_eq!(m.view(), before, "a bad factor moves nothing");
    }

    #[test]
    fn labels_follow_the_spec() {
        let mut m = two_sources();
        assert_eq!(m.label(0, Some("demo_kdb")), "SPX.close");
        assert_eq!(
            m.label(0, Some("demo_rest")),
            "SPX.close@demo_kdb",
            "the source shows when it is not the default"
        );
        assert_eq!(m.label(0, None), "SPX.close@demo_kdb");
        let (n, _) = m
            .add_expr(
                "SPX.close / VIX",
                geode_core::series::expr::Ast::<u8>::Num(1.0),
            )
            .unwrap();
        assert_eq!(n, 3);
        assert_eq!(m.label(2, Some("demo_kdb")), "SPX.close / VIX");
        let long = "SPX.close / VIX * 100 - SPX.close";
        assert!(long.chars().count() > LABEL_MAX);
        m.add_expr(long, geode_core::series::expr::Ast::<u8>::Num(1.0))
            .unwrap();
        let label = m.label(3, Some("demo_kdb"));
        assert_eq!(
            label, "SPX.close / VIX * 100 -…",
            "over LABEL_MAX the text is cut and ends in an ellipsis"
        );
        assert_eq!(label.chars().count(), LABEL_MAX);
        let exact = "SPX.close / VIX * 100 - ";
        assert_eq!(exact.chars().count(), LABEL_MAX);
        m.add_expr(exact, geode_core::series::expr::Ast::<u8>::Num(1.0))
            .unwrap();
        assert_eq!(m.label(4, Some("demo_kdb")), exact, "at LABEL_MAX, uncut");
    }

    #[test]
    fn a_command_target_is_the_selection_or_a_source_name() {
        let mut m = two_sources();
        m.add_source("VIX", "demo_rest", "series").unwrap(); // 3
        m.add_expr(
            "SPX.close / VIX",
            geode_core::series::expr::Ast::<u8>::Ref(1),
        )
        .unwrap(); // 4, selected
        assert_eq!(m.target(None, Some("demo_kdb")), Ok(4), "the selection");
        assert_eq!(m.target(Some("SPX.close"), Some("demo_kdb")), Ok(1));
        assert_eq!(
            m.target(Some("VIX"), Some("demo_kdb")),
            Ok(2),
            "the label wins over another source's same identity"
        );
        assert_eq!(m.target(Some("VIX@demo_rest"), Some("demo_kdb")), Ok(3));
        assert_eq!(
            m.target(Some("VIX"), Some("other")).unwrap_err(),
            "'VIX' is ambiguous: VIX@demo_kdb or VIX@demo_rest"
        );
        assert_eq!(
            m.target(Some("SPX.close / VIX"), Some("demo_kdb"))
                .unwrap_err(),
            "'SPX.close / VIX' is not loaded — `a` adds it",
            "an expression is targetable only by selection"
        );
        assert_eq!(
            Model::new().target(None, None).unwrap_err(),
            "select a series or name one"
        );
    }

    #[test]
    fn series_names_are_the_source_labels_once_each() {
        let mut m = two_sources();
        m.add_source("VIX", "demo_rest", "series").unwrap();
        m.add_source("VIX", "demo_kdb", "series").unwrap();
        m.add_expr(
            "SPX.close / VIX",
            geode_core::series::expr::Ast::<u8>::Ref(1),
        )
        .unwrap();
        assert_eq!(
            m.series_names(Some("demo_kdb")),
            vec!["SPX.close", "VIX", "VIX@demo_rest"]
        );
    }

    /// A saved expression whose text could not be rewritten keeps its
    /// slot, failed with the reason, and holds no operands; an edit
    /// replaces it with a working expression.
    #[test]
    fn a_legacy_expression_is_failed_until_an_edit_replaces_it() {
        let mut m = two_sources();
        let n = m
            .add_legacy_expr(
                "s1 / s9",
                "it references a series this session no longer holds".into(),
            )
            .unwrap();
        let s = m.slot_by_number(n).unwrap();
        assert!(s.legacy);
        assert_eq!(s.text.as_deref(), Some("s1 / s9"));
        assert!(matches!(&s.state, SlotState::Failed(why) if why.contains("no longer holds")));
        assert!(m.dependants(1).is_empty(), "it references nothing");
        m.replace_expr(
            n,
            "SPX.close * 2",
            geode_core::series::expr::Ast::<u8>::Ref(1),
        )
        .unwrap();
        let s = m.slot_by_number(n).unwrap();
        assert!(!s.legacy);
        assert_eq!(s.state, SlotState::Idle);
        assert_eq!(m.dependants(1), vec![n]);
    }

    #[test]
    fn refusals_name_a_series_by_label() {
        let mut m = two_sources();
        m.add_expr(
            "SPX.close / VIX",
            geode_core::series::expr::Ast::<u8>::Ref(1),
        )
        .unwrap();
        assert_eq!(
            m.set_rule(3, BucketRule::Max).unwrap_err(),
            "SPX.close / VIX is an expression; its rule is its operands'"
        );
        assert_eq!(
            m.replace_expr(1, "x", geode_core::series::expr::Ast::<u8>::Num(1.0))
                .unwrap_err(),
            "SPX.close@demo_kdb is not an expression"
        );
        assert_eq!(m.remove(9).unwrap_err(), "that series is gone");
    }

    #[test]
    fn set_state_by_number_and_by_pair() {
        let mut m = two_sources();
        m.add_source("VIX", "demo_kdb", "series").unwrap();
        let ch = m.set_pair_state("demo_kdb", "VIX", SlotState::Idle);
        assert!(ch.chrome());
        assert!(matches!(m.slots()[1].state, SlotState::Idle));
        assert!(
            matches!(m.slots()[2].state, SlotState::Idle),
            "both slots of the pair"
        );
        assert!(matches!(m.slots()[0].state, SlotState::Fetching));
        assert_eq!(
            m.set_pair_state("demo_kdb", "nope", SlotState::Idle),
            Changed::NONE
        );
        m.set_state(1, SlotState::Failed("boom".into()));
        assert!(matches!(&m.slots()[0].state, SlotState::Failed(e) if e == "boom"));
    }

    #[test]
    fn set_visible_by_number() {
        let mut m = two_sources();
        let ch = m.set_visible(2, false).unwrap();
        assert!(!m.slots()[1].visible);
        assert!(ch.chrome());
        assert!(m.set_visible(9, false).is_err(), "no such slot");
    }

    #[test]
    fn clear_drops_everything_but_the_settings() {
        let mut m = two_sources();
        m.set_frequency(Frequency::H1, now(), &AsOf::Live).unwrap();
        let ch = m.clear();
        assert!(m.slots().is_empty() && m.cursor().is_none() && m.dataset().is_none());
        assert_eq!(m.frequency(), Frequency::H1, "settings survive a clear");
        assert!(ch.query() && ch.chrome() && ch.session());
        assert_eq!(
            m.add_source("A", "demo_kdb", "series").unwrap().0,
            1,
            "a cleared tile is a fresh tile: the numbering starts again, which is what `take_number`'s exhaustion message promises"
        );
    }
}
