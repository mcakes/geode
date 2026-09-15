//! The per-tile command line controller (spec section 3.4): opening it
//! for `/` (find) or `:` (command) prompts, closing/cancelling it,
//! reacting to its text changing (live find-as-you-type), and its key
//! handler — the Accept branch that runs a submitted command or completion
//! against the focused occupant lives here. Split out of `shell/mod.rs`
//! (Phase 3c Task 0): the 3b final review found this accept branch sitting
//! 1,400 lines from the pure `commandline` core it depends on.

use gpui::{Context, Focusable as _, KeyDownEvent, Window};

use crate::commandline::{self, CommandLine, Prompt};
use crate::module::FindEvent;

use super::ShellView;
use super::keys::convert_keystroke;

impl ShellView {
    /// Open the per-tile command line (§3.4) with the given prompt: builds
    /// a fresh [`CommandLine`] over the focused tile, cancels any pending
    /// keymap sequence (mirrors `toggle_palette`'s own cancel — same
    /// reasoning: the line has its own key handling that never touches
    /// `self.matcher`), resets the shared input's value, and focuses it.
    /// A no-op when there is no focused tile, or the focused tile has no
    /// occupant — nothing to run a command against.
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
        self.command_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.command_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    /// Close the command line (if open) and hand focus back to the shell
    /// root — the command-line twin of [`close_palette`](Self::close_palette).
    fn close_command_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.command_line = None;
        // Only reclaim keyboard focus for the shell root if `command_
        // input` still holds it (I1, final review). Every established
        // close path — Enter, Escape, `ctrl+k`, a dialog opening — runs
        // while that is true, so this was always a no-op guard for them.
        // It matters for the render-time backstop above (`render`'s own
        // doc comment, beside `ensure_occupants`'s drag-cancel
        // neighbours): that path also closes the line when some OTHER
        // surface — the toolbar's `filter_input` — has already taken
        // focus for itself, and reclaiming it here would steal it right
        // back the instant the user clicked it.
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

    /// Close the command line exactly as pressing Escape on it would
    /// (§3.4): a `Find` prompt tells the occupant it was cancelled first
    /// (`FindEvent::Cancelled`); either prompt then just closes. A no-op
    /// when none is open.
    ///
    /// This is the one door every OTHER exclusive-focus surface uses to
    /// take the command line's input away from under it unconditionally
    /// — mirroring `close_palette`'s own call sites: `handle_command_
    /// line_key`'s own escape arm, `toggle_palette` (opening OR closing
    /// the palette while the line is open), and `dialog::open_shell_
    /// dialog_with_key` (a modal opening over an open line). A chord
    /// that opens an overlay is not a click away, so none of those three
    /// routes through this door's mouse-facing sibling instead. Without
    /// this, the line stayed `Some` and painted but stopped receiving
    /// any of its own keys the moment a newer surface's branch in
    /// `handle_key_down` started winning ahead of it. A mouse click away
    /// from the line — both tile mouse-down handlers in `render`, and
    /// `render`'s own generic backstop for every other focus-stealing
    /// surface (I1, final review — same doc comment location as the
    /// `pending_focus_restore`/drag-cancel block above it) — goes
    /// through [`leave_command_line`](Self::leave_command_line) instead,
    /// which commits a non-empty `Find` prompt's text before falling
    /// back to this door for everything else (spec §20.4).
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

    /// The mouse's way out of the command line (spec §20.4): a click
    /// away from a `/` line whose text has already moved the cursor
    /// COMMITS it — `FindEvent::Committed` with the field's text, so the
    /// cursor stays on the match and `n`/`N` have a target — exactly as
    /// the scope bar keeps its text on blur. An empty `/` line, and any
    /// `:` line (nothing typed there has applied, and a stray click must
    /// not run a command), cancel through [`cancel_command_line`].
    /// `escape` is still `cancel_command_line` for both prompts.
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

    /// Re-rank (`:`) or forward (`/`) every change to the command line's
    /// text — the `InputEvent::Change` subscription wired up in `new`.
    /// `/` forwards the raw text to the occupant's own `find` on every
    /// keystroke (§3.4: `FindEvent::Changed`); `:` asks the occupant for
    /// completions over the word under the cursor and re-ranks them
    /// through the pure core (`CommandLine::refresh`).
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
                }
            }
        }
        cx.notify();
    }

    /// Keys while the command line's input has focus (routed from
    /// `handle_key_down`'s own command-line branch, ahead of the modal/
    /// filter-input guards). `true` if the key was consumed here and must
    /// not also reach the window's text-input phase; everything not
    /// claimed here (printable characters, caret movement, ...) falls
    /// through to the focused `Input`, exactly as the filter field and the
    /// dialogs' shared filter input already do.
    ///
    /// `escape` cancels (a `/` cancel is forwarded to the occupant first);
    /// `enter` commits — `/` forwards the committed text, `:` resolves the
    /// line through the pure core (`commandline::resolve_submit`) and
    /// either runs it on the occupant, accepts a unique match and runs
    /// that, or shows an ambiguous-match error inline without closing.
    /// `tab`/`ctrl+n`/`ctrl+p` (command-line only) step or accept the
    /// completion popup.
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
                        // C1 (final review): `c.word` is the range the
                        // NEXT accept splices into — it must move to
                        // cover exactly the candidate just written
                        // (`word.start..cursor`), or a second `tab` uses
                        // the stale pre-accept range against the
                        // already-accepted line and corrupts it. Nothing
                        // else can refresh `word` here: `set_value`
                        // below emits no `InputEvent::Change` at the
                        // pinned gpui-component rev (`on_command_line_
                        // changed`, this struct's only other writer of
                        // `word`, never runs), which is also why the
                        // `candidates`/`words`/`highlighted` save-and-
                        // restore this replaced was provably inert: the
                        // `refresh` those three fields were being
                        // defended against never fires from `set_value`
                        // either.
                        c.word = c.word.start..cursor;
                        // Cycle on repeat: the next tab highlights the next
                        // candidate over the same typed word.
                        c.step(1);
                        self.command_input
                            .update(cx, |input, cx| input.set_value(line, window, cx));
                    }
                }
            }
            cx.notify();
            return true;
        }
        false
    }
}
