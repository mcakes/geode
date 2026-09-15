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
        let saved = self.frame.read(cx).saved_scopes().clone();
        let items = palette::build_items(
            &self.services.registry,
            &self.services.theme,
            &bindings,
            &saved,
        );
        self.palette = Some(PaletteState::with_usage(
            items,
            &self.palette_usage,
            unix_now(),
        ));
        // Fresh scroll state for a fresh palette session — a stale offset
        // left over from a previous open (a different query, a different
        // scroll position) must not carry over now that the results list
        // scrolls a real viewport instead of always fitting on screen.
        self.palette_scroll = ScrollHandle::new();
        self.palette_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        // Recorded before the palette takes focus, for `close_palette`
        // (see `ShellView::overlay_return_to_filter`).
        self.overlay_return_to_filter = self.filter_field_focused(window, cx);
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
        // Only a palette that WAS open moves focus. The dialog door calls
        // this unconditionally on every open, and an unconditional focus
        // of the root here blurred the scope bar's text field a line
        // before the door asked whether the field held focus — so every
        // dialog opened from the field returned to the root instead.
        if self.palette.take().is_none() {
            return;
        }
        self.return_focus_from_overlay(window, cx);
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
    /// normal dispatch chain"); a `Theme` item
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
        // Every real choice counts as a use — the one exception is the
        // palette's own toggle row, which only ever closes the palette
        // (the empty arm below) and would otherwise climb the ranking for
        // doing nothing. Decided once here, for both the record and the
        // dispatch.
        let is_toggle = matches!(item, PaletteItem::Action(id, ..) if id.0 == "palette::toggle");
        if !is_toggle {
            self.palette_usage.record(&item.usage_key(), unix_now());
            self.palette_usage_version += 1;
        }
        match item {
            PaletteItem::Action(..) if is_toggle => {}
            PaletteItem::Action(id, ..) => self.dispatch(id, None, window, cx),
            PaletteItem::Theme(name) => {
                // The name is already fully qualified (e.g. "Gruvbox
                // Dark"), which `ThemeService::resolve` matches outright.
                self.services.theme.apply(name, cx);
                self.persist_theme(cx);
            }
            PaletteItem::Scope(name) => {
                // Phase 4a §3.9: load a saved scope, undoable like any
                // other scope change (`Frame::load_scope` goes through
                // `set_scope`).
                let name = name.clone();
                self.frame.update(cx, |f, cx| {
                    if let Ok(true) = f.load_scope(&name) {
                        cx.notify();
                    }
                });
            }
        }
    }

    /// Commit the highlighted row: close the palette, then dispatch the
    /// item that was under the highlight. **The one door** for both the
    /// keyboard (`enter`, in [`Self::handle_palette_key`]) and the mouse
    /// (a row click, via `ShellView::render`'s `on_row_click`, which
    /// selects the clicked row first and then comes here) — the palette's
    /// own half of the interaction model's §17.1 rule 2, "a row click
    /// does what `enter` would", adopted for this filter-only surface by
    /// user request (2026-09-12) rather than by that rule's own scope.
    /// One method rather than two copies so the key and the click cannot
    /// drift: whatever the palette does on commit (close first, so the
    /// dispatched action sees the shell without the overlay; record the
    /// use for the ranking) happens identically from either.
    ///
    /// The item is read before `close_palette` because closing drops
    /// `self.palette`, and dispatched after it so an action that opens a
    /// dialog or another palette-sized overlay is not immediately painted
    /// under this one.
    pub(super) fn commit_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.palette.as_ref().and_then(PaletteState::selected_item);
        self.close_palette(window, cx);
        if let Some(item) = selected {
            self.dispatch_palette_item(&item, window, cx);
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

        match ks.key.as_str() {
            "escape" => self.close_palette(window, cx),
            "enter" => self.commit_selected(window, cx),
            // Everything but escape/enter: the whole `listfilter::nav_command`
            // set through `vimnav::apply` — a bare ±1 wraps, a page step
            // clamps (spec §20.5), the same rule every list and tile has.
            // A bare typed character lands here too and must stay a true
            // no-op — deliberately not `cx.stop_propagation()`, so the
            // window's text-input phase still delivers it to the focused
            // `palette_input` (see this method's doc comment).
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

/// The wall clock as unix seconds, for `palette_usage`'s `now` — read
/// once per palette open and once per palette dispatch, never per frame.
/// A clock before the epoch reads as 0 rather than panicking.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
