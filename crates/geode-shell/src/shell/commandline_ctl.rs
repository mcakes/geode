//! Lifecycle and routing for the per-tile `/` and `:` prompts.
//! Each prompt captures its tile ID when opened. Find changes and commit/cancel
//! events go to that occupant; command completion and execution use its vocabulary.
//! Errors remain inline until a text edit or successful close.

use gpui::{Context, Focusable as _, KeyDownEvent, ScrollHandle, Window};

use crate::commandline::{self, CommandLine, Prompt};
use crate::module::FindEvent;

use super::ShellView;
use super::keys::convert_keystroke;

impl ShellView {
    /// Open a fresh prompt for the focused occupied tile, cancel pending key
    /// sequences, clear Input, and focus it. Without an occupied tile, do nothing.
    pub(super) fn open_command_line(
        &mut self,
        prompt: Prompt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tile) = self.services.workspaces.active().focused_tile() else {
            return;
        };
        if !self.occupants.contains_key(&tile) {
            return;
        }
        self.matcher.cancel();
        self.command_line = Some(CommandLine::new(prompt, tile));
        self.command_scroll = ScrollHandle::new();
        self.command_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        // set_value does not emit a change event; populate suggestions on open.
        if prompt == Prompt::Command {
            self.on_command_line_changed(window, cx);
        }
        self.command_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    /// Close the prompt, returning focus to the shell root only when the
    /// command Input still owns keyboard focus.
    fn close_command_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.command_line = None;
        // Restore shell focus only if this Input still owns it. A blur-driven
        // close must not steal focus back from the surface the user just selected.
        if self
            .command_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
        {
            self.focus_handle.focus(window, cx);
        }
        cx.notify();
    }

    /// Cancel an open prompt. A find sends Cancelled to its captured occupant;
    /// either prompt then closes. Escape and overlay-opening routes use this operation,
    /// while pointer-driven blur uses [`Self::leave_command_line`].
    pub(super) fn cancel_command_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(line) = self.command_line.as_ref() else {
            return;
        };
        if line.prompt == Prompt::Find
            && let Some(o) = self.occupants.get(&line.tile)
        {
            o.content.find(FindEvent::Cancelled, window, cx);
        }
        self.close_command_line(window, cx);
    }

    /// Leave on blur: commit nonempty find text to the captured occupant,
    /// retaining its match, but cancel empty finds and all command prompts.
    /// A stray focus change must not execute an unsubmitted command.
    pub(super) fn leave_command_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(line) = self.command_line.as_ref() else {
            return;
        };
        let text = self.command_input.read(cx).value().to_string();
        if line.prompt == Prompt::Find && !text.is_empty() {
            if let Some(o) = self.occupants.get(&line.tile) {
                o.content.find(FindEvent::Committed(text), window, cx);
            }
            self.close_command_line(window, cx);
        } else {
            self.cancel_command_line(window, cx);
        }
    }

    /// On Input text change, forward FindEvent::Changed or refresh command
    /// completions from the captured occupant using the current text and byte cursor.
    /// Without that occupant, retain prompt state unchanged.
    pub(super) fn on_command_line_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(line) = self.command_line.as_ref() else {
            return;
        };
        let (prompt, tile) = (line.prompt, line.tile);
        let text = self.command_input.read(cx).value().to_string();
        let cursor = self.command_input.read(cx).cursor();
        let Some(o) = self.occupants.get(&tile) else {
            return;
        };
        match prompt {
            Prompt::Find => o.content.find(FindEvent::Changed(text), window, cx),
            Prompt::Command => {
                let words = o.content.completions(&text, cursor, cx);
                if let Some(line) = self.command_line.as_mut() {
                    line.refresh(&text, cursor, words);
                    self.command_scroll.scroll_to_item(line.highlighted);
                }
            }
        }
        cx.notify();
    }

    /// Handle keys for an open prompt. Escape cancels and Enter submits regardless
    /// of modifiers. Find Enter commits even empty text; command Enter resolves exact,
    /// unique, or ambiguous completions, then runs on the captured occupant. Execution
    /// and ambiguity errors keep the prompt open. Bare Tab accepts a candidate; bare
    /// arrows and Control-N/P cycle candidates. Return true for handled keys so the
    /// caller consumes them; other keys remain available to Input.
    pub(super) fn handle_command_line_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(prompt) = self.command_line.as_ref().map(|c| c.prompt) else {
            return false;
        };
        let tile = self.command_line.as_ref().map(|c| c.tile).unwrap();
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            self.cancel_command_line(window, cx);
            return true;
        }
        if key == "enter" {
            let text = self.command_input.read(cx).value().to_string();
            let cursor = self.command_input.read(cx).cursor();
            match prompt {
                Prompt::Find => {
                    if let Some(o) = self.occupants.get(&tile) {
                        o.content.find(FindEvent::Committed(text), window, cx);
                    }
                    self.close_command_line(window, cx);
                }
                Prompt::Command => {
                    let (candidates, words) = {
                        let c = self.command_line.as_ref().unwrap();
                        (c.candidates.clone(), c.words.clone())
                    };
                    let to_run =
                        match commandline::resolve_submit(&text, cursor, &candidates, &words) {
                            commandline::Submit::Run(line) => line,
                            commandline::Submit::Accepted(line, _) => {
                                self.command_input.update(cx, |input, cx| {
                                    input.set_value(line.clone(), window, cx)
                                });
                                line
                            }
                            commandline::Submit::Ambiguous(names) => {
                                if let Some(c) = self.command_line.as_mut() {
                                    c.error = Some(format!("ambiguous: {}", names.join(", ")));
                                }
                                cx.notify();
                                return true;
                            }
                        };
                    let result = match self.occupants.get(&tile) {
                        Some(o) => o.content.command(&to_run, window, cx),
                        None => Err("the tile is gone".into()),
                    };
                    match result {
                        Ok(()) => self.close_command_line(window, cx),
                        Err(e) => {
                            if let Some(c) = self.command_line.as_mut() {
                                c.error = Some(e);
                            }
                            cx.notify();
                        }
                    }
                }
            }
            return true;
        }
        if prompt == Prompt::Command
            && let Some(ks) = convert_keystroke(&event.keystroke)
            && let Some(ck) = commandline::completion_key(&ks)
        {
            let c = self.command_line.as_mut().unwrap();
            match ck {
                commandline::CompletionKey::Next => c.step(1),
                commandline::CompletionKey::Prev => c.step(-1),
                commandline::CompletionKey::Accept => {
                    if let Some(word) = c.highlighted_word().map(str::to_string) {
                        let text = self.command_input.read(cx).value().to_string();
                        let (line, cursor) = commandline::accept(&text, c.word.clone(), &word);
                        // The next Tab replaces the candidate just written, not the original
                        // short token. Update its cached byte range explicitly because set_value
                        // does not emit the Input change event that normally refreshes completions.
                        c.word = c.word.start..cursor;
                        // Cycle on repeat: the next tab highlights the next
                        // candidate over the same typed word.
                        c.step(1);
                        self.command_input
                            .update(cx, |input, cx| input.set_value(line, window, cx));
                    }
                }
            }
            self.command_scroll.scroll_to_item(c.highlighted);
            cx.notify();
            return true;
        }
        false
    }
}
