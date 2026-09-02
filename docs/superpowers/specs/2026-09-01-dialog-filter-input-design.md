# Dialog filter input — design

**Status:** approved, not yet implemented
**Date:** 2026-09-01
**Scope:** `geode-shell`'s two list dialogs (settings, keybindings) and one
init-time keybinding in `geode-app`.

## 1. What changes and why

Both list dialogs — settings (`ctrl+,`) and keybindings — are driven today by
a vim vocabulary: `j`/`k` (with count prefixes), `gg`/`shift+g`, and a `/`
find session whose behaviour is chosen by a user setting, `[ui] find_style`,
between a vim jump model and an fzf-style filter (`crate::vimnav`,
`crate::vimfind`).

This replaces that model with one filter-first model, in both dialogs:

- a text input, front and centre, **focused on open** and always active;
- typing fuzzy-filters and re-ranks the rows live;
- arrow keys move the selection, with `ctrl+d`/`ctrl+u`/`ctrl+f`/`ctrl+b` for
  larger steps.

Because the input is always focused, bare letters can never be navigation
keys again — `j`/`k`, `gg`, `shift+g`, `/`, `n`/`N` and count prefixes all
lose their meaning inside a dialog. Neither dialog reads `[ui] find_style`
any more — but the setting itself survives untouched, for Phase 3's blotter
to consume (§8).

The command palette (`ctrl+k`) already works exactly this way. This aligns
the dialogs with the surface users already have in their hands, rather than
inventing a third interaction model.

## 2. Verified platform constraints

These were read out of the pinned checkouts, not assumed. They constrain the
key vocabulary below, and the reasoning is recorded here because it is not
recoverable from Geode's own source.

Paths are relative to
`~/.cargo/git/checkouts/gpui-component-95ce574d8a0da8b8/0e2fb7a/` and
`~/.cargo/git/checkouts/zed-a70e2ad075855582/e3adf43/`.

**(a) Keys the focused `Input` swallows.** `crates/base/src/input/base/state.rs`
attaches `on_action` listeners unconditionally for `left`/`right` (3117-3118),
`home`/`end` (3134-3135), the `select_*` family, and `on_action_search` /
`on_action_replace` (3144-3145).
Those handlers do not re-propagate, so the raw `KeyDownEvent` never reaches
`finish_dispatch_key_event` and never reaches a dialog. **`left`, `right`,
`home` and `end` are therefore unavailable to the dialogs** — which is what
retires settings' current `h`/`l`/`left`/`right` value stepping.

**(b) Keys that fall through.** `up`, `down`, `pageup`, `pagedown` (lines
3121-3128) and `tab`/`shift+tab` (lines 3110-3115) attach their listeners only
`.when(self.is_multi_line())`. Both dialog inputs are single-line, so no
listener exists and the raw event falls through untouched. `ctrl+d`,
`ctrl+u`, `ctrl+b`, `ctrl+n`, `ctrl+p` have **no** `KeyBinding` in the
`"Input"` context at all (`state.rs:122-269`), so they are never matched.
`enter` and `escape` are bound and do have listeners, but for a single-line,
non-`clean_on_escape` input those handlers call `cx.propagate()` explicitly —
the same route the palette already relies on.

**(c) `ctrl+f` dies on Windows and Linux.** `state.rs:255-257` binds
`ctrl-f` to `Search` under `#[cfg(not(target_os = "macos"))]`, and
`crates/base/src/input/editor/search.rs:198-203` returns early — without
`cx.propagate()` — when the input is not searchable. So `ctrl+f` works on
macOS and is silently swallowed everywhere else. CI builds both platforms;
a silent divergence in a keyboard-first app is not acceptable. See §7.

**(d) A custom pass-through action would not fix (c).**
`crates/gpui/src/window.rs:5468-5471` dispatches *every* matched binding in
sequence, stopping only when a handler leaves `propagate_event` false. A
Geode action bound to `ctrl-f` whose handler called `cx.propagate()` would
therefore hand the keystroke straight on to `Search`, which swallows it. The
mechanism that does work is gpui's own `NoAction`:
`crates/gpui/src/keymap.rs:201-226` drops `NoAction` bindings *and* every
binding of equal or weaker precedence that they outrank, leaving
`match_result.bindings` empty — so no action is dispatched and the raw event
reaches the key listeners. Later-registered bindings win
(`keymap.rs:173` iterates `.rev()`), and gpui-component's bindings carry no
`meta`, so they are treated as user-precedence (0) and are suppressed by a
`NoAction` also at 0.

## 3. Interaction model

Identical in both dialogs except where noted. The filter input is focused
from the moment the dialog opens.

| Key | Effect |
| --- | --- |
| printable, `backspace`, `left`/`right`/`home`/`end`, `ctrl+a`/`c`/`v`/`x` | text editing — consumed natively by `Input`, never seen by the dialog |
| `up` / `down`, `ctrl+p` / `ctrl+n` | move selection ∓1, clamped |
| `ctrl+d` / `ctrl+u` | ±5 |
| `ctrl+f` / `ctrl+b`, `pageup` / `pagedown` | ±10 |
| `escape` | close the dialog |
| `enter` | keybindings: start listening on the selected row · settings: **nothing** (deliberately reserved) |
| `tab` / `shift+tab` | settings: step the selected row's value forward / back · keybindings: nothing (reserved) |

Mouse behaviour is unchanged: a click selects a row; a click on the
already-selected row starts listening (keybindings) or steps the value
forward (settings).

`escape` always closes outright rather than first clearing the filter — the
palette's behaviour, and the filter resets to empty on every open anyway, so
a "clear first" step would only ever cost a keystroke.

`enter` in settings does nothing on purpose. Settings apply immediately on
step, so there is nothing to confirm, and the key is held in reserve rather
than aliased onto `tab`.

A key a dialog does not claim (`enter` in settings, `tab` in keybindings,
anything else that falls through) leaves that dialog's `on_key` handler
returning `false`, unhandled. The modal branch in `ShellView::handle_key_down`
acts only on `escape`, and the shell keymap `Matcher` is never reached while a
modal is open, so an unclaimed key is inert — it can neither fire a shell
chord behind the dialog nor close it.

### Rebind capture (keybindings only)

Rebinding needs every keystroke, including bare letters the input would
otherwise swallow. Entering listen mode therefore **blurs the filter input**,
moving focus back to `ShellView::focus_handle`; raw keystrokes then reach the
modal's `on_key` handler exactly as they do today, so
`keybindings_view::press_while_listening` is untouched. The filter text and
the narrowed list stay on screen, dimmed, with the selected row showing a
"press a binding" affordance. `enter` commits, `escape` cancels; both refocus
the input.

## 4. Ranking, filtering and highlighting

Match text per row is unchanged: `"{title} {category}"` via each module's
existing `searchable_text` — never the invisible action id, never a settings
row's values.

- **Empty query** — every row, in its natural order (keybindings:
  `(category, title)`, as `derive_rows` already sorts; settings: declared
  row order).
- **Non-empty query** — only rows whose match text fuzzy-matches, ordered by
  `palette::fuzzy_match` score descending, ties broken by natural order.
  Non-matching rows are hidden.
- Selection resets to the top-ranked row on every query edit (the palette's
  `set_query` semantics).
- Zero matches paints the muted "no matches" line both dialogs already have
  for their fzf sessions.

Highlighting moves from `vimfind::match_range`'s substring span to the
per-character indices `palette::fuzzy_match` already returns — the same
highlight the palette paints, so the three surfaces agree.

`palette::fuzzy_match` is the single matcher; nothing new implements
matching. A small pure helper (`rank(texts, query) -> Vec<Match>`, where
`Match` carries the row index and its highlight indices) wraps it for the two
dialogs. The palette keeps its own `PaletteState` filtering as-is — it filters
a richer item type, and rewriting a working surface for no behaviour change is
out of scope (§10).

## 5. State and ownership

`ShellView` gains one field, `dialog_input: Entity<InputState>`, built once in
`new` and mirroring `palette_input` in every respect: the *value* is reset to
`""` on each dialog open rather than the entity being rebuilt, so the same
`FocusHandle` survives close/reopen and the single `InputEvent::Change`
subscription set up in `new` stays wired for the life of the window. One
entity is shared by both dialogs because the modal system guarantees only one
is ever open; the subscription routes the query into whichever of
`ShellView::settings` / `ShellView::keybindings` is `Some`.

Both dialog state structs lose `find: VimFind`, `find_anchor` and
`nav: VimListNav`, and gain `query: String` plus the ranked visible-row list
derived from it. `selected` becomes an index into the **filtered** list — the
palette's convention — and is what the nav commands clamp against. Click
handlers stay keyed by row identity (`SettingId`, row index into the full
list), so they remain correct across filtering.

Both structs stay free of `gpui` types and fully unit-testable without a
window, exactly as today.

## 6. Focus and key routing

`dialog::open_shell_dialog_with_key` grows an arm for "focus this handle on
open"; every close path returns focus to `ShellView::focus_handle`, the way
`close_palette` already does. Each dialog keeps its `dialog::ModalKeyHandler`
and its first refusal on keystrokes — it simply now only ever sees what the
`Input` did not consume.

Navigation needs no new state machine: `vimnav::apply(selected, len, cmd)`
already does the clamped arithmetic. A small shared
`dialog::list_nav_command(&Keystroke) -> Option<NavCommand>` maps the §3 table
onto `NavCommand`, and both dialogs feed its result to `vimnav::apply`.
`vimnav::VimListNav::press` — the `j`/`k`, count-prefix and `gg` state
machine — is not used by the dialogs any more.

## 7. The `ctrl+f` shim

In `geode-app`'s init, immediately after `gpui_component::init(cx)`:

```rust
// gpui-component binds ctrl-f to its editor Search action in the "Input"
// context on non-macOS, and that handler swallows the key without
// re-propagating when the input isn't searchable — so ctrl+f would work on
// macOS and silently die on Windows. NoAction is gpui's own mechanism for
// this: it suppresses every equal-or-weaker binding it outranks, leaving no
// action to dispatch, so the raw KeyDownEvent reaches our key listeners.
// Later registrations win, so this must come after gpui_component::init.
// Registered unconditionally so both platforms run one code path.
cx.bind_keys([KeyBinding::new("ctrl-f", NoAction, Some("Input"))]);
```

This is window-wide, so it also frees `ctrl+f` in the palette and the toolbar
filter field. That is intended: no Geode surface wants gpui-component's editor
Search, and one rule beats three exceptions.

## 8. `[ui] find_style` is untouched

Nothing about the setting changes. `vimfind::FindStyle`, its `from_config` /
`config_value` / `label` surface and its `persist_to_user_config` sibling,
`ShellView::find_style`, `settings_view::set_find_style` /
`set_find_style_on`, the settings row itself (settings keeps four rows), and
the key in the defaults and docs all stay exactly as they are.

What changes is only that **the two dialogs stop reading it** — with `/`
gone they have no find session whose style there is anything to choose. The
setting is retained for Phase 3's blotter, which is expected to consume both
it and the `vimfind` session drivers it selects between (§9).

The consequence to be honest about: until the blotter lands, the Find style
row steers nothing. That is a deliberate trade for keeping this change
purely subtractive from the dialogs' side and leaving a rollback with
nothing to restore here.

## 9. What deliberately does not change

- **`vimnav` and `vimfind` stay in the crate whole, intact and tested** —
  `FindStyle` and its config plumbing included (§8). Their `j`/`k`/count/`gg`
  and `/`-session cores are unreachable from a dialog now, but Phase 3's
  blotter is expected to want exactly them: a grid with no always-focused
  filter can still offer the full vim vocabulary, in whichever style
  `[ui] find_style` names. `vimnav::apply` remains in active use (§6).
- **`press_while_listening` and the whole rebind-persistence path**
  (`keymap_edit::apply_rebind`, the background write, the reload-driven
  refresh) are untouched. Blurring the input is what makes that possible.
- **The four settings setter seams** (`set_theme`, `set_dark_mode`,
  `set_font_size` and their `*_on` cores) keep their signatures and
  semantics; `shell::mod`'s tests drive them directly.
- **`derive_rows`' no-caching contract** in both dialogs: rows are still
  derived fresh on every render and every keystroke.
- The toolbar filter field, the palette, and anything in the blotter.

## 10. Testing

Per the house rules, the pure cores are TDD'd and the wiring gets real-event
`#[gpui::test]`s.

Pure, no window:

- `rank` — empty query yields natural order; a non-empty query hides
  non-matches, orders by score, breaks ties by natural order; highlight
  indices are char offsets into the match text.
- `list_nav_command` — every row of the §3 table, including the
  `pageup`/`pagedown` aliases, and `None` for keys the dialogs must not claim.
- Dialog state — a query edit resets the selection to the top match; nav
  clamps at both ends of the *filtered* list; a click maps a filtered index
  back to the right row identity.
- Settings — `tab`/`shift+tab` step forward/back through a row's values,
  wrapping at both ends (the existing `step` tests, rekeyed).

With `TestAppContext`:

- opening either dialog focuses the filter input;
- typing filters rather than navigating — pressing `j` inserts a `j`;
- `up`/`down` and `ctrl+d`/`u`/`f`/`b` move the selection while the input
  stays focused and the query is unchanged;
- in keybindings, `enter` starts listening and blurs the input; a subsequent
  letter is captured as a binding and does **not** appear in the filter;
  `escape` cancels, refocuses the input, and leaves the query intact;
- `escape` from the normal state closes the dialog and restores focus to the
  shell root.

Deleted: the vim/fzf find tests inside `settings_view` and
`keybindings_view` — those drive a find session neither dialog has any more.
`vimfind`'s and `vimnav`'s own test modules stay whole, `FindStyle`'s
included; `settings_view`'s row tests keep covering the Find style row, which
still steps like every other (§8).

## 11. Risks

- **Rollback likelihood is material.** This is a UX experiment; the work is
  branched so it can be dropped whole. Keeping `vimnav`/`vimfind` intact
  (§9) is part of that: a revert restores the old dialogs without having to
  resurrect deleted modules.
- **The `NoAction` shim is a cross-crate precedence argument.** It is
  verified against the pinned revs (§2d) and pinned by the `Cargo.lock`, but
  a gpui or gpui-component upgrade should re-check it. The comment in §7
  names the mechanism so the next reader can.
- **The blur-on-listen transition is the one moment focus moves twice** (out
  of the input and back). The `TestAppContext` cases in §10 exist mainly to
  pin that down, since it is not visually verifiable in this environment.
