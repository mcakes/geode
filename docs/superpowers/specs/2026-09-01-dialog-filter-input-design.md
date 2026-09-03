# Dialog filter input — design

**Status:** approved, not yet implemented
**Date:** 2026-09-01
**Scope:** `geode-shell`'s two list dialogs (settings, keybindings), the
command palette's navigation vocabulary, and one init-time keybinding in
`geode-app`.

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

The traffic runs both ways: the palette **gains** the larger navigation
steps the dialogs are keeping (`ctrl+d`/`ctrl+u`, `ctrl+f`/`ctrl+b`,
`pageup`/`pagedown`), which it has never had. One vocabulary, three
surfaces — see §3's palette subsection for the one place they still differ.

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
listener exists and the raw event falls through **the `Input`**. `ctrl+d`,
`ctrl+u`, `ctrl+b`, `ctrl+n`, `ctrl+p` have **no** `KeyBinding` in the
`"Input"` context at all (`state.rs:122-269`), so they are never matched.
`enter` and `escape` are bound and do have listeners, but for a single-line,
non-`clean_on_escape` input those handlers call `cx.propagate()` explicitly —
the same route the palette already relies on.

**(b2) `tab` and `shift+tab` die a second death, in `Root`.** Clearing the
`Input` is not enough: `crates/ui/src/root.rs:21-25` binds `tab`/`shift-tab`
to `Tab`/`TabPrev` in a `"Root"` key context, and `root.rs:582-584` attaches
`on_action` listeners for both unconditionally — `on_action_tab`
(`root.rs:482`) drives focus navigation and never re-propagates. Every
gpui-component app wraps its root view in `Root`, this one included
(`CLAUDE.md`), so the keystroke is consumed above `ShellView` and no raw key
listener runs.

**This was missed in the first pass of §2 — it checked the `Input` context
and stopped there — and it blocked task 3 of the implementation.** The fix
is the same `NoAction` mechanism as (d), scoped to Geode's own modal rather
than applied app-wide: the modal panel carries a `"GeodeModal"` key context,
and `tab`/`shift-tab` are bound to `NoAction` in it. Because
`bindings_for_input` sorts matches by context depth before applying the
`NoAction` suppression (`keymap.rs:188-190`, then `201-226`), the deeper
`"GeodeModal"` match outranks `"Root"` and suppresses it — but only while
focus sits inside a Geode modal. gpui-component's focus cycling survives
everywhere else in the app, which app-wide suppression would have silently
removed.

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

With a focused `Input`, an *unclaimed* key is not inert — it continues past
the dialog's `on_key` handler to the window's own text-input phase
(`Window::dispatch_keystroke`'s second phase, which runs whenever the key
event still `propagate`s after every listener; `cx.stop_propagation()` is
what a claimed key uses to skip it). That phase is exactly what feeds the
shared filter `Input`, which is always focused while a list dialog is open —
so a genuinely reserved key like `enter` in settings or `tab` in keybindings
must be *claimed and dropped* (`on_key` returns `true` having done nothing),
not left unhandled, or it lands in the filter as a stray character.
Concretely: `InputState::normalize_input` strips `\n`/`\r` but not `\t`, so
an unclaimed `tab` types a literal tab into the query and collapses the list
to "no matches"; and even though `enter` does get normalized away to nothing,
`replace_text_in_range` still fires `InputEvent::Change` unconditionally, so
the query-edit subscription resets the selection to the top match regardless.
Both dialogs claim their reserved key for exactly this reason — see
`keybindings_view::handle_key` and `settings_view::handle_key`'s own doc
comments. (On a shipped build this never reaches a real user: `enter` and
`tab` are both control characters, and macOS routes control characters to
`doCommandBySelector` while Windows' `parse_char_message` drops
`is_control()` characters, so neither ever reaches `insertText` on either
platform — the leak was only ever observable in the `#[gpui::test]` harness,
which drives `Window::dispatch_keystroke` directly rather than through a
real platform text-input callback. The rule holds regardless of platform, so
the claim is made unconditionally rather than relying on that.) A claimed
key, in either dialog, still cannot fire a shell chord behind the dialog or
close it: the modal branch in `ShellView::handle_key_down` acts only on
`escape` once the dialog's own handler has had first refusal, and the shell
keymap `Matcher`
is never reached while a modal is open.

### The command palette

The palette adopts the same navigation vocabulary, additively: `ctrl+d` /
`ctrl+u` (±5), `ctrl+f` / `ctrl+b` and `pageup` / `pagedown` (±10) join the
`up` / `down` / `ctrl+p` / `ctrl+n` it already has. Everything else about the
palette is untouched.

One difference survives on purpose. **`PaletteState::move_selection` wraps**
at both ends (`palette.rs:362` — `up` at row 0 selects the last row) where
the dialogs clamp, and it keeps wrapping: that is the behaviour already in
the user's hands for the ±1 keys, and there is no reason this change should
disturb it. The new larger steps **clamp**, in the palette as in the dialogs
— a page jump that teleports from the top of a 50-result list to the bottom
reads as a glitch, not a feature.

Implementation follows from that split rather than from a special case:
`ShellView::handle_palette_key`'s existing named arms (`escape`, `enter`,
`up`, `down`, `ctrl+p`, `ctrl+n`) stay exactly as they are and return first;
a new fallback arm below them runs `listfilter::nav_command` and applies the
result through `vimnav::apply` + `PaletteState::set_selected`, which clamps.
The ±1 keys can never reach that arm, so nothing needs to ask "is this delta
1?".

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
`listfilter::nav_command(&Keystroke) -> Option<NavCommand>` maps the §3
table onto `NavCommand`, and all three surfaces feed its result to
`vimnav::apply` — both dialogs' `ModalKeyHandler`s, and
`ShellView::handle_palette_key`'s new fallback arm. It lives in the same new
pure module as the ranking helper (§4) rather than in `dialog.rs`, so both
are unit-testable without a window, and so the palette — which is not a
dialog — can reach it without importing dialog chrome. The module is named
for what it serves (every filtered list surface), not for the dialogs alone.
`vimnav::VimListNav::press` — the `j`/`k`, count-prefix and `gg` state
machine — is not used by the dialogs any more.

The palette's keys arrive as `gpui::Keystroke` (`handle_palette_key` reads
`event.keystroke` directly), so that call site converts once via
`convert_keystroke` before consulting `nav_command`, which speaks the
shell-native type like every other keymap-facing seam in this crate.

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
- **The palette, apart from the six keys it gains** (§3: `ctrl+d`/`ctrl+u`/
  `ctrl+f`/`ctrl+b`, `pageup`/`pagedown`). Its query field,
  its `PaletteState` filtering, its wrapping ±1 keys, its rendering and its
  dispatch path are all untouched — this change adds a fallback arm to
  `handle_palette_key` and nothing else.
- The toolbar filter field, and anything in the blotter.

## 10. Testing

Per the house rules, the pure cores are TDD'd and the wiring gets real-event
`#[gpui::test]`s.

Pure, no window:

- `rank` — empty query yields natural order; a non-empty query hides
  non-matches, orders by score, breaks ties by natural order; highlight
  indices are char offsets into the match text.
- `nav_command` — every row of the §3 table, including the
  `pageup`/`pagedown` aliases, and `None` for keys no surface may claim.
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
  shell root;
- in the palette, `ctrl+d`/`u`/`f`/`b` move the selection by the §3 amounts
  and **clamp** at both ends, while `up` at row 0 still **wraps** to the last
  result — the two halves of the split this change deliberately preserves.

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
