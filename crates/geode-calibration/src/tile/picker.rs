//! The edit pickers: one popup under the cursor's card that lists what a
//! verb may act on. `c` lists the stages that may feed the cursor's stage
//! (`core::edits::connect_candidates`) and previews the highlighted one as
//! a dashed edge into it; `x` lists the stage's inputs; `a` lists the
//! kinds, and a pick turns the popup into a name field. The lists are
//! fieldless: the tile reports `mode == menu`, `popup == list` and
//! `tilelist`, so the shell's shared `j`/`k` and arrows step them, `enter`
//! picks and `escape` discards. The name field holds the keys in `insert`
//! (every bare key types), with `enter` and `escape` the tile's.
//!
//! A connect or disconnect pick queues one whole-object edit through the
//! frame (`CalibrationTile::queue_edits`, after the reader has agreed to
//! it); the shell writes it and the reload brings the new graph. A named
//! add writes nothing: it proposes the seeded stage through
//! `config_object` and asks the shell to open the dialog on it. Closing
//! the popup drops the preview edge, and the
//! field is blurred before it is dropped: a focused handle dropped
//! unblurred leaves the window with no focus the shell can route keys
//! through. One popup is up at a time: opening one closes the `.` menu
//! and the filter field, and any verb but the popup's own closes it.

use geode_core::calibration::StageName;
use geode_shell::actions::ActionId;
use geode_shell::choice::{ChoiceList, DEFAULT_CAP};
use geode_shell::popover;
use geode_shell::shell::control;
use geode_shell::vimnav::NavCommand;
use gpui::prelude::*;
use gpui::{
    Anchor, App, Context, Deferred, ElementId, Entity, Focusable as _, SharedString, Window, div,
};
use gpui_component::ActiveTheme as _;
use gpui_component::input::{Input, InputEvent, InputState};

use super::{CalibrationTile, PICKER_CONTEXT};
use crate::core::edits::{KINDS, add_table, connect_candidates, open_port, valid_new_name};
use crate::core::menu::{NO_CURSOR, NO_PIPELINE, OPEN_STAGE};

/// What the open popup picks.
pub(super) enum PickerKind {
    /// A stage to feed `stage`, from `candidates` (the list's options).
    Connect {
        stage: StageName,
        candidates: Vec<StageName>,
    },
    /// An input of `stage` to strike, from `inputs` (the list's options).
    Disconnect {
        stage: StageName,
        inputs: Vec<StageName>,
    },
    /// A kind for a new stage, from `KINDS`.
    AddKind,
    /// The new stage's name, typed into the field.
    AddName {
        kind: &'static str,
        input: Entity<InputState>,
    },
}

/// The open popup. The strings are prepared here so paint formats nothing.
pub(super) struct PickerState {
    pub(super) kind: PickerKind,
    pub(super) title: SharedString,
    /// The list over the kind's options; empty for the name field.
    pub(super) list: ChoiceList,
    /// Each option's name and the muted detail beside it, indexed like
    /// the options.
    pub(super) rows: Vec<(SharedString, SharedString)>,
}

impl PickerState {
    fn list(kind: PickerKind, title: String, rows: Vec<(String, &'static str)>) -> PickerState {
        let options: Vec<String> = rows.iter().map(|(name, _)| name.clone()).collect();
        PickerState {
            kind,
            title: title.into(),
            list: ChoiceList::new(options, DEFAULT_CAP),
            rows: rows
                .into_iter()
                .map(|(name, detail)| (SharedString::from(name), SharedString::new_static(detail)))
                .collect(),
        }
    }

    /// Whether the popup is a fieldless list (the shared steps move it).
    pub(super) fn is_list(&self) -> bool {
        !matches!(self.kind, PickerKind::AddName { .. })
    }

    pub(super) fn field(&self) -> Option<&Entity<InputState>> {
        match &self.kind {
            PickerKind::AddName { input, .. } => Some(input),
            _ => None,
        }
    }
}

impl CalibrationTile {
    /// The live pipeline and the cursor's stage in it: what an edit of the
    /// cursor's stage acts on. A held, dropped pipeline edits nothing.
    fn edit_target(&self) -> Result<(geode_core::calibration::Pipeline, StageName), String> {
        let pipeline = self.pipeline().ok_or(NO_PIPELINE)?;
        let cursor = self.cursor.clone().ok_or(NO_CURSOR)?;
        if pipeline.stage(&cursor).is_none() {
            return Err(NO_CURSOR.to_string());
        }
        Ok((pipeline, cursor))
    }

    /// The kind word of `stage`, for a row's detail; empty for a name the
    /// pipeline lacks.
    fn kind_word(pipeline: &geode_core::calibration::Pipeline, stage: &str) -> &'static str {
        pipeline
            .stage(stage)
            .map(|s| s.kind.kind_name())
            .unwrap_or("")
    }

    /// `c`: the picker over the stages that may feed the cursor's stage.
    /// Refused, naming the stage, when its ports are all taken or nothing
    /// left fits one.
    pub(super) fn open_connect(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let (pipeline, stage) = self.edit_target()?;
        let candidates = connect_candidates(&pipeline, &stage);
        if candidates.is_empty() {
            return Err(if open_port(&pipeline, &stage).is_none() {
                format!("{stage} has no open input")
            } else {
                format!("nothing left to connect to {stage}")
            });
        }
        let rows = candidates
            .iter()
            .map(|(name, _)| (name.clone(), Self::kind_word(&pipeline, name)))
            .collect();
        let state = PickerState::list(
            PickerKind::Connect {
                stage: stage.clone(),
                candidates: candidates.into_iter().map(|(name, _)| name).collect(),
            },
            format!("Connect {stage} from"),
            rows,
        );
        self.open_picker(state, window, cx);
        Ok(())
    }

    /// `x`: the picker over the cursor's stage's inputs, in port order.
    pub(super) fn open_disconnect(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let (pipeline, stage) = self.edit_target()?;
        let inputs = pipeline
            .stage(&stage)
            .map(|s| s.inputs.clone())
            .unwrap_or_default();
        if inputs.is_empty() {
            return Err(format!("{stage} has no inputs to disconnect"));
        }
        let rows = inputs
            .iter()
            .map(|name| (name.clone(), Self::kind_word(&pipeline, name)))
            .collect();
        let state = PickerState::list(
            PickerKind::Disconnect {
                stage: stage.clone(),
                inputs,
            },
            format!("Disconnect from {stage}"),
            rows,
        );
        self.open_picker(state, window, cx);
        Ok(())
    }

    /// `a`: the picker over the kinds; a pick asks for the name.
    pub(super) fn open_add(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.pipeline().ok_or(NO_PIPELINE)?;
        let rows = KINDS
            .iter()
            .map(|(kind, line)| ((*kind).to_string(), *line))
            .collect();
        let state = PickerState::list(PickerKind::AddKind, "Add stage".to_string(), rows);
        self.open_picker(state, window, cx);
        Ok(())
    }

    /// Put `state` up: the menu, the filter field and any earlier popup
    /// close first, and the last verb's refusal leaves with them.
    fn open_picker(&mut self, state: PickerState, window: &mut Window, cx: &mut Context<Self>) {
        self.close_picker(window, cx);
        self.cancel_filter(window, cx);
        self.close_menu(cx);
        self.verb_notice = None;
        self.picker = Some(state);
        self.update_preview(cx);
        cx.notify();
    }

    /// A kind picked: the popup becomes the name field, focused.
    fn open_name_field(&mut self, kind: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("stage name"));
        // A typed name clears the last refusal: the field answers afresh.
        cx.subscribe_in(&input, window, |this, _, event, _window, cx| {
            if let InputEvent::Change = event
                && this.verb_notice.take().is_some()
            {
                cx.notify();
            }
        })
        .detach();
        input.read(cx).focus_handle(cx).focus(window, cx);
        self.picker = Some(PickerState {
            kind: PickerKind::AddName { kind, input },
            title: format!("Name the {kind}").into(),
            list: ChoiceList::new(Vec::new(), DEFAULT_CAP),
            rows: Vec::new(),
        });
        cx.notify();
    }

    /// Close the popup. `true` when one was up. The field is blurred first
    /// when it holds the keyboard; the preview edge leaves with the popup.
    pub(super) fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(picker) = self.picker.take() else {
            return false;
        };
        if let Some(input) = picker.field()
            && input.read(cx).focus_handle(cx).is_focused(window)
        {
            window.blur(cx);
        }
        self.update_preview(cx);
        cx.notify();
        true
    }

    /// A config push while a list is up: its rows were about the old
    /// pipeline, so it closes (no field, no focus to return). The name
    /// field stays: it is checked against the document when committed.
    pub(super) fn drop_stale_picker(&mut self, cx: &mut Context<Self>) {
        if self.picker.as_ref().is_some_and(PickerState::is_list) {
            self.picker = None;
            self.update_preview(cx);
            cx.notify();
        }
    }

    /// The connect picker's highlighted candidate is the preview edge into
    /// the stage; anything else previews nothing. The model is rebuilt only
    /// when the edge changed.
    fn update_preview(&mut self, cx: &App) {
        let next = match &self.picker {
            Some(PickerState {
                kind: PickerKind::Connect { stage, candidates },
                list,
                ..
            }) => list
                .highlighted_option()
                .map(|i| (candidates[i].clone(), stage.clone())),
            _ => None,
        };
        self.set_preview(next, cx);
    }

    /// The preview edge, `(from, to)`, from the picker's highlight or a
    /// port drag over an eligible card; the model is rebuilt only when it
    /// changed.
    pub(super) fn set_preview(&mut self, next: Option<(StageName, StageName)>, cx: &App) {
        if self.preview_edge != next {
            self.preview_edge = next;
            self.rebuild_model(cx);
        }
    }

    /// Step the open list by one row, clamped. `false` with no list up.
    pub(super) fn step_picker(&mut self, delta: i64, cx: &mut Context<Self>) -> bool {
        let Some(p) = &mut self.picker else {
            return false;
        };
        if !p.is_list() {
            return false;
        }
        p.list.nav_clamped(NavCommand::Move(delta));
        self.update_preview(cx);
        cx.notify();
        true
    }

    /// Whether a fieldless list is up (the shared steps are its).
    pub(super) fn picker_list_up(&self) -> bool {
        self.picker.as_ref().is_some_and(PickerState::is_list)
    }

    /// Whether the name field is up (the keys are its).
    pub(super) fn picker_field_up(&self) -> bool {
        self.picker.as_ref().is_some_and(|p| p.field().is_some())
    }

    /// `enter`: act on the highlighted row, or on the typed name.
    pub(super) fn commit_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = &self.picker else {
            return;
        };
        match &p.kind {
            PickerKind::Connect { stage, candidates } => {
                let Some(i) = p.list.highlighted_option() else {
                    return;
                };
                let (stage, from) = (stage.clone(), candidates[i].clone());
                self.close_picker(window, cx);
                if let Err(reason) = self.connect(&stage, &from, cx) {
                    self.refuse(reason, cx);
                }
            }
            PickerKind::Disconnect { stage, inputs } => {
                let Some(i) = p.list.highlighted_option() else {
                    return;
                };
                let (stage, input) = (stage.clone(), inputs[i].clone());
                self.close_picker(window, cx);
                if let Err(reason) = self.disconnect(&stage, &input, cx) {
                    self.refuse(reason, cx);
                }
            }
            PickerKind::AddKind => {
                let Some(i) = p.list.highlighted_option() else {
                    return;
                };
                self.open_name_field(KINDS[i].0, window, cx);
            }
            PickerKind::AddName { kind, input } => {
                let name = input.read(cx).value().to_string();
                if let Err(reason) = self.add_stage(kind, &name, window, cx) {
                    // The field stays up for a corrected name.
                    self.refuse(reason, cx);
                }
            }
        }
    }

    /// A good name writes nothing: the seeded table (`pipeline`, `kind`,
    /// the kind's required fields at the builtin's values) is held as
    /// `pending_add`, which `config_object` answers with the seed, and the
    /// shell is asked for `config::calibration`, so the dialog opens at
    /// the new stage's Edit screen on a draft from the seed — where the
    /// inputs are ticked and the save is the one write. The shell reads
    /// the proposal in its frame observer, before the tile's own clears it
    /// (`on_frame_changed`). The cursor stays where it is.
    fn add_stage(
        &mut self,
        kind: &'static str,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let pipeline = self.pipeline().ok_or(NO_PIPELINE)?;
        let sources = self.sources().ok_or(NO_PIPELINE)?;
        valid_new_name(&sources, name)?;
        self.close_picker(window, cx);
        self.verb_notice = None;
        self.pending_add = Some((name.to_string(), add_table(&pipeline.name, kind)));
        self.frame
            .request_action(ActionId(OPEN_STAGE.to_string()), cx);
        cx.notify();
        Ok(())
    }

    /// A painted row pressed: highlight it and commit as `enter` would.
    pub(super) fn picker_pick(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = &mut self.picker else {
            return;
        };
        if p.is_list() && p.list.set_highlighted(row) {
            self.update_preview(cx);
            self.commit_picker(window, cx);
        }
    }

    /// The pointer resting on a row: the mouse form of `j`/`k`.
    /// Change-only: gpui fires it on every move over the row.
    pub(super) fn picker_hover(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(p) = &mut self.picker else {
            return;
        };
        if p.is_list() && p.list.highlighted() != row && p.list.set_highlighted(row) {
            self.update_preview(cx);
            cx.notify();
        }
    }

    /// The open popup, hung by its top-left corner from the point the
    /// caller paints it at (under the cursor's card); `None` with none up.
    pub(super) fn render_picker(&self, tile: &Entity<Self>, cx: &App) -> Option<Deferred> {
        let p = self.picker.as_ref()?;
        let id = self.id.0;
        let theme = cx.theme();
        let hover = control::paint(
            theme,
            control::Rest::Bare,
            theme.popover,
            theme.popover_foreground,
        )
        .hover;
        let mut surface = popover::surface(cx)
            .id(ElementId::NamedInteger(
                SharedString::new_static("calibration-picker"),
                id,
            ))
            .debug_selector(move || format!("calibration-picker-{id}"))
            .occlude()
            .on_mouse_down_out({
                let tile = tile.clone();
                move |_, window, cx| {
                    tile.update(cx, |t, cx| {
                        t.close_picker(window, cx);
                    })
                }
            })
            .child(
                div()
                    .px(geode_shell::shell::scale::design(popover::ROW_INSET))
                    .pb_0p5()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(p.title.clone()),
            );
        if let Some(input) = p.field() {
            surface = surface.child(
                div()
                    .w_full()
                    .px(geode_shell::shell::scale::design(popover::ROW_INSET))
                    .key_context(PICKER_CONTEXT)
                    .debug_selector(move || format!("calibration-picker-name-{id}"))
                    .child(Input::new(input).appearance(false).w_full()),
            );
            return Some(popover::anchor_popup(surface, Anchor::TopLeft));
        }
        let lit = p.list.highlighted();
        for (i, r) in p.list.painted().iter().enumerate() {
            let (name, detail) = p.rows[r.row].clone();
            surface = surface.child(
                popover::row_shell(
                    theme,
                    hover,
                    ElementId::NamedInteger(
                        SharedString::new_static("calibration-picker-row"),
                        r.row as u64,
                    ),
                    i == lit,
                    move || format!("calibration-picker-row-{id}-{i}"),
                    {
                        let tile = tile.clone();
                        move |window, cx| tile.update(cx, |t, cx| t.picker_pick(i, window, cx))
                    },
                )
                .justify_between()
                .on_mouse_move({
                    let tile = tile.clone();
                    move |_, _, cx| tile.update(cx, |t, cx| t.picker_hover(i, cx))
                })
                .child(name)
                .child(
                    div()
                        .text_xs()
                        .when(i != lit, |el| el.text_color(theme.muted_foreground))
                        .child(detail),
                ),
            );
        }
        Some(popover::anchor_popup(surface, Anchor::TopLeft))
    }

    /// The open list's rows (name and detail) and highlighted row; `None`
    /// with no list up.
    #[cfg(test)]
    pub(crate) fn picker_rows(&self) -> Option<(Vec<(String, String)>, usize)> {
        let p = self.picker.as_ref().filter(|p| p.is_list())?;
        Some((
            p.list
                .painted()
                .iter()
                .map(|r| {
                    let (name, detail) = &p.rows[r.row];
                    (name.to_string(), detail.to_string())
                })
                .collect(),
            p.list.highlighted(),
        ))
    }

    /// The open popup's title line.
    #[cfg(test)]
    pub(crate) fn picker_title(&self) -> Option<String> {
        self.picker.as_ref().map(|p| p.title.to_string())
    }

    /// The name field's text, while it is up.
    #[cfg(test)]
    pub(crate) fn picker_name(&self, cx: &App) -> Option<String> {
        self.picker
            .as_ref()
            .and_then(PickerState::field)
            .map(|input| input.read(cx).value().to_string())
    }
}
