//! The command palette controller (spec section 3.5, section 6): opening
//! and closing the palette overlay, scroll-syncing the selected row,
//! dispatching a chosen palette item (an action or a theme), and the
//! open-palette key handler (search text, arrow/enter navigation, tab
//! completion for sequences). Split out of `shell/mod.rs` (Phase 3c
//! Task 0) as the seam `input.rs`'s `dispatch`/`handle_key_down` and
//! `render.rs`'s palette chrome both call into.

use gpui::{Context, Focusable as _, KeyDownEvent, ScrollHandle, Window};

use crate::listfilter;
use crate::palette::{self, PaletteItem, PaletteState};

use super::ShellView;
use super::keys::convert_keystroke;

impl ShellView {
    /// Open the palette (building a fresh `PaletteState` — actions in
    /// registry order, then themes) if it's closed, or close it if it's
    /// open. Takes `window`/`cx` (added by the palette-input-polish task,
    /// unlike the old free-text version) purely for the focus handoff:
    /// [`close_palette`](Self::close_palette) on the close arm, and, on the
    /// open arm, resetting `self.palette_input`'s value to `""`
    /// (`InputState::set_value` — checked against the pinned checkout: it
    /// does *not* emit `InputEvent::Change`, so this alone never touches
    /// `self.palette`'s query, which is already starting fresh from
    /// `PaletteState::new` a few lines below) and focusing it, so typing
    /// reaches the query field the instant the palette appears rather than
    /// requiring a click first.
    ///
    /// Cancels an open command line first, unconditionally, on either arm
    /// (fix round 1, finding 1): `ctrl+k` is a shipped, always-reachable
    /// binding — `handle_key_down`'s command-line branch carves out an
    /// explicit exception for it so it still reaches here even while the
    /// line has focus (see that branch's own comment) — and without this,
    /// the palette would open over a command line still holding the
    /// input's focus and still (per its own doc comment) claiming every
    /// key, which the palette's own key handling assumes it owns
    /// exclusively.
    pub(super) fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_command_line(window, cx);
        if self.palette.is_some() {
            self.close_palette(window, cx);
            return;
        }
        // Opening the palette cancels any pending keymap sequence (spec:
        // palette-open cancels pending — supersedes the 1b-ui deferred
        // note that pending state would survive a palette session). The
        // palette has its own key handling (`handle_palette_key`) that
        // never touches `self.matcher`, so without this an unfinished
        // sequence like the first "g" of "g g" would sit in `self.matcher`
        // across the whole palette session and then resume matching
        // against whatever key closes the palette.
        self.matcher.cancel();
        let bindings = palette::build_binding_index(&self.services.keymap);
        let items = palette::build_items(&self.services.registry, &self.services.theme, &bindings);
        self.palette = Some(PaletteState::new(items));
        // Fresh scroll state for a fresh palette session — a stale offset
        // left over from a previous open (a different query, a different
        // scroll position) must not carry over now that the results list
        // scrolls a real viewport instead of always fitting on screen.
        self.palette_scroll = ScrollHandle::new();
        self.palette_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.palette_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
    }

    /// Close the palette (if open) and hand focus back to the shell root —
    /// the "return focus to shell root on close" half of the palette-
    /// input-polish task's focus contract (the "focus it on open" half
    /// lives in `toggle_palette`'s open arm). The one standard door for
    /// closing the palette from a real key/mouse event: `toggle_palette`'s
    /// close arm, `handle_palette_key`'s escape/enter arms, the click-
    /// catcher's dismiss handler (`render`, below), and `dialog::
    /// open_shell_dialog_with_key` (a modal opening over an open palette)
    /// all go through this rather than setting `self.palette = None`
    /// directly. `apply_reload`'s own silent close is the one deliberate
    /// exception — see that call site's own comment for why (no `Window`
    /// available there).
    pub(super) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self.focus_handle.focus(window, cx);
    }

    /// Scroll the palette's results viewport so the currently selected row
    /// is visible (`gpui::ScrollHandle::scroll_to_item`, a real per-frame
    /// layout measurement — see `palette::render`'s doc comment). Called
    /// from every path that can change `self.palette`'s selection: `handle_
    /// palette_key`'s up/down/ctrl+p/ctrl+n arms, the `InputEvent::Change`
    /// subscription set up in `new` (`PaletteState::set_query` resets the
    /// selection to row 0 on every edit, same as `push_char`/`backspace`
    /// used to — that's still a selection change the viewport must follow),
    /// and a row click (`set_selected`, via the click handler built in
    /// `render`, below). A no-op while the palette is closed.
    pub(super) fn sync_palette_scroll(&self) {
        if let Some(palette) = self.palette.as_ref() {
            self.palette_scroll.scroll_to_item(palette.selected());
        }
    }

    /// Dispatch one selected palette row: an `Action` item goes through the
    /// normal [`dispatch`](Self::dispatch) chain (brief: "action -> the
    /// normal dispatch chain incl. theme::toggle_mode"); a `Theme` item
    /// applies that theme directly via `ThemeService::apply`. The palette
    /// is assumed already closed by the caller (Enter closes before
    /// dispatching) — so the `palette::toggle` action id is deliberately
    /// *not* re-dispatched here: `dispatch`'s `palette::toggle` branch
    /// calls `toggle_palette`, which would reopen the just-closed palette,
    /// turning "select 'Toggle command palette'" into "close then
    /// immediately reopen". Skipping it instead makes selecting that row a
    /// true toggle: the palette just closes and stays closed, exactly like
    /// pressing the toggle keystroke a second time would.
    fn dispatch_palette_item(
        &mut self,
        item: &PaletteItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match item {
            PaletteItem::Action(id, ..) if id.0 == "palette::toggle" => {}
            PaletteItem::Action(id, ..) => self.dispatch(id, None, window, cx),
            PaletteItem::Theme(name) => {
                // The name is already fully qualified (e.g. "Gruvbox
                // Dark"), which `ThemeService::resolve` matches outright
                // regardless of the `mode` argument — so the mode passed
                // here is irrelevant to which theme gets applied.
                self.services
                    .theme
                    .apply(name, crate::theme::Mode::Dark, cx);
                self.persist_theme(cx);
            }
        }
    }

    /// Handle one *bubbled* key event while the palette is open — reworked
    /// by the palette-input-polish task from the old "owns every key,
    /// including free text entry" version. Query editing (typing,
    /// backspace/delete, caret movement, ctrl+a, ctrl+v) is no longer this
    /// method's job at all: `self.palette_input`, a real gpui-component
    /// `Input`, consumes those natively and — per the routing analysis on
    /// the `palette_input` field's own doc comment — they never reach here
    /// in the first place; `handle_key_down`'s `if self.palette.is_some()`
    /// guard (below) only ever routes here what the `Input` didn't already
    /// consume.
    ///
    /// This method now does exactly two things: act on the list of
    /// navigation/close keys the palette still owns (up/down, ctrl+p/
    /// ctrl+n, enter, escape, plus the larger steps — ctrl+d/u ±5, ctrl+f/b
    /// and pageup/pagedown ±10 — that Task 5 added as a fallback arm so
    /// this filtered list surface reads the same as the two dialogs', spec
    /// §3, "The command palette"), and otherwise do *nothing* — deliberately
    /// not `cx.stop_propagation()`, which would be the wrong kind of
    /// "swallow": a bare typed character reaches this method too (no
    /// `KeyBinding` at all matches it inside `Input`'s own "Input" context,
    /// so raw dispatch runs — see the field doc comment again), and it must
    /// keep propagating past this listener so the window's separate IME/
    /// text-input phase (`Window::dispatch_keystroke`'s second phase in
    /// tests; the platform's real text-input callback in production) still
    /// delivers it to the now-focused `palette_input`. Either way, no shell
    /// chord ever fires while the palette is open: `handle_key_down`'s own
    /// `if self.palette.is_some() { self.handle_palette_key(..); return; }`
    /// guard is a plain Rust-level branch that never falls through to
    /// `self.matcher.press` regardless of what happens in here.
    ///
    /// The named up/down/ctrl+p/ctrl+n arms below keep wrapping
    /// (`PaletteState::move_selection`, unchanged since before Task 5),
    /// while the larger steps in the fallback arm clamp
    /// (`crate::vimnav::apply`) — a page jump that teleported from the top
    /// of a long result list to the bottom would read as a glitch, not a
    /// feature. That split falls out of arm order alone: the ±1 keys
    /// return from their own arms before the fallback arm is ever reached,
    /// so nothing there has to inspect the resolved delta to pick a rule —
    /// reaching the fallback arm at all already means the key was none of
    /// those four.
    ///
    /// Reads gpui's own `Keystroke` directly (`event.keystroke`, not the
    /// shell-native one `convert_keystroke` produces) because it needs the
    /// named keys (`"up"`, `"down"`, `"enter"`, `"escape"`) and raw
    /// `modifiers` that the shell-native conversion's matcher-oriented
    /// shape doesn't carry as directly.
    pub(super) fn handle_palette_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ks = &event.keystroke;
        let mods = ks.modifiers;

        match ks.key.as_str() {
            "escape" => self.close_palette(window, cx),
            "enter" => {
                let selected = self.palette.as_ref().and_then(PaletteState::selected_item);
                self.close_palette(window, cx);
                if let Some(item) = selected {
                    self.dispatch_palette_item(&item, window, cx);
                }
            }
            "up" => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.move_selection(-1);
                }
                self.sync_palette_scroll();
            }
            "down" => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.move_selection(1);
                }
                self.sync_palette_scroll();
            }
            "p" if mods.control => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.move_selection(-1);
                }
                self.sync_palette_scroll();
            }
            "n" if mods.control => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.move_selection(1);
                }
                self.sync_palette_scroll();
            }
            // Everything the named arms above did not take. Two outcomes:
            // a larger navigation step (the vocabulary the two list
            // dialogs use, adopted here so all three filtered surfaces
            // read the same — spec §3, "The command palette"), or a
            // genuine no-op.
            //
            // These clamp, while the ±1 arms above wrap: reaching this
            // arm at all means the key was NOT up/down/ctrl+p/ctrl+n, so
            // nothing here has to inspect the delta to pick a rule. A
            // page jump that teleports from the top of a long result list
            // to the bottom reads as a glitch, not as a feature.
            //
            // A bare typed character lands here too, and must stay a true
            // no-op — deliberately not `cx.stop_propagation()`, so the
            // window's separate text-input phase still delivers it to the
            // focused `palette_input` (see this method's doc comment).
            _ => {
                if let Some(ks) = convert_keystroke(&event.keystroke)
                    && let Some(cmd) = listfilter::nav_command(&ks)
                    && let Some(palette) = self.palette.as_mut()
                {
                    let len = palette.filtered().len();
                    let next = crate::vimnav::apply(palette.selected(), len, cmd);
                    palette.set_selected(next);
                    self.sync_palette_scroll();
                }
            }
        }
    }
}
