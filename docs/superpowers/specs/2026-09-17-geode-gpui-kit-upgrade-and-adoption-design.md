# gpui-kit: upgrade, audit and adoption

**Date:** 2026-09-17
**Status:** approved in conversation; spec under review
**Supersedes:** the "gpui / gpui_platform git deps must stay unpinned" invariant
(root `Cargo.toml` comment, CLAUDE.md "Workspace invariants") and the
`gpui` / `gpui-component` skills listed in `skills-lock.json`.

## 1. Summary

gpui-component was rebranded **gpui-kit** on 2026-09-03 (repo
`longbridge/gpui-kit`, the old URL redirects). Two things changed that matter
to Geode beyond the name:

1. **GPUI is on crates.io.** gpui-kit publishes weekly snapshots of zed's
   gpui crates as `gpui-pre`, `gpui-pre-platform`, `gpui-pre-macros`
   (version `0.3.<N>`, each crate's description names the zed rev). Geode no
   longer needs a git dependency on zed at all, which dissolves the
   two-copies hazard the old invariant guarded against.
2. **The crate family split.** `gpui-kit` is an umbrella facade
   (`use gpui_kit::*` re-exports gpui), `gpui-base` holds unstyled behaviour
   (focus trap, motion, scrollbars, calendar, test support), `gpui-component`
   keeps its name and holds the styled components, `gpui-component-assets`
   was **renamed** `gpui-kit-assets`.

This work is three sequential deliverables (user ruling 2026-09-17):

- **Phase 1** — the upgrade to the released versions, the invariant rewrite,
  and the skills swap. One branch, lands first, nothing else rides on it.
- **Phase 2** — the component audit: what Geode rolled itself, why, and
  what gpui-kit could add. Recorded in this spec (§4); no code.
- **Phase 3** — three approved adoptions, each its own branch after Phase 1:
  tooltips (§5.1), the as-of date picker (§5.2), ingest progress in the
  status bar (§5.3).

## 2. Versions and the pin rule

**Released:** `gpui-kit` / `gpui-component` / `gpui-kit-assets` /
`gpui-base` / `gpui-component-macros` **0.6.2** (2026-09-18 — it shipped the
morning after this spec was written, superseding 0.6.1 of 2026-09-09);
`gpui-pre` / `gpui-pre-platform` / `gpui-pre-macros` **0.3.5** (2026-09-14).
**Amendment 2026-09-18 (Phase 1, Task 1):** the first build at 0.6.1 failed
inside gpui-component itself — it names its siblings `gpui-base` and
`gpui-component-macros` with a CARET, so under a 0.6.1 pin they floated to
0.6.2 (which had removed the dock `tiles` canvas and added `Plot::hover`)
and the pair no longer compiled. That is the pin rule's own hazard one
level down, so the ruling is twofold: the family moves to 0.6.2, and the
two caret-named siblings are pinned DIRECTLY (`gpui-base = "=0.6.2"`,
`gpui-component-macros = "=0.6.2"`, carried by `geode-app` beside
`gpui-kit-assets`) so `cargo update` cannot split the family again. 0.6.2
requires `gpui-pre = "0.3.5"` exactly as pinned, and its public API in every
module Geode imports is additive over the old rev (`Input::{id, on_paste}`,
`Button::tooltip_placement`, `Theme::{motion_tokens, radius_2xl..4xl}`,
`Icon::data`). One behavioural change rides in: upstream #3108 "list,
table: drop the outline from the selected item, row and cell" — a display
check (§3.5), since the CVI panel's cursor cell was described as bordered.

**Ruling: registry, `=`-pinned.** Geode depends on the crates.io releases,
every one of them pinned exactly:

```toml
gpui                  = { package = "gpui-pre",          version = "=0.3.5" }
gpui_platform         = { package = "gpui-pre-platform", version = "=0.3.5", features = ["font-kit", "runtime_shaders"] }
gpui-component        = "=0.6.2"
gpui-kit-assets       = "=0.6.2"
gpui-base             = "=0.6.2"   # sibling gpui-component names with a caret
gpui-component-macros = "=0.6.2"   # likewise
```

Why `=` and not a caret: gpui-kit itself depends on `gpui-pre` with a caret,
so an unpinned `cargo update` could move gpui under us to a snapshot whose
API we have not read. The `=` pins make the committed `Cargo.lock` and the
manifest agree; a bump is a deliberate change on a branch, both families
together. Cargo still resolves one copy of gpui because both Geode and
gpui-component now name the same registry crate — the git two-copies hazard
cannot recur.

Why not a git rev of gpui-kit: the only things git HEAD has that 0.6.2 does
not are polish (chart hover animation and path caching, `InputGroup`, the
editor search API, mobile work) and the `gpui_kit::test` facade. None is
needed by Phase 1 or the three approved slices. A git pin would also
reintroduce keeping two families in step by hand. Revisit only if a later
slice needs something unreleased.

**Toolchain:** upstream requires Rust 1.90+ and macOS 15+; the workstation
is at 1.96 / macOS 26. Windows CI must keep building (CLAUDE.md), and the
gpui-pre crates are the same code zed ships there.

## 3. Phase 1 — the upgrade

### 3.1 Changes

| Where | Change |
|---|---|
| root `Cargo.toml` `[workspace.dependencies]` | the six entries in §2; the git-dependency comment replaced by the pin rule (§3.2) |
| `crates/geode-app/Cargo.toml` | `gpui-component-assets.workspace = true` → `gpui-kit-assets.workspace = true`, plus the two sibling pins `gpui-base.workspace = true` / `gpui-component-macros.workspace = true` |
| `crates/geode-app/src/main.rs` | `gpui_component_assets::Assets` → `gpui_kit_assets::Assets` |
| `Cargo.lock` | regenerated by the resolution; committed |
| `skills-lock.json`, `.agents/skills/`, `.claude/skills/` | `npx skills remove gpui gpui-component` then `npx skills add longbridge/gpui-kit --all` → `gpui-kit`, `gpui-kit-design-guides` |
| `CLAUDE.md` | the invariant bullet rewritten (§3.2); the "gpui skills" section names the two new skills; "pinned checkout `crates/ui/src/…`" wording points at the registry source (§3.3) |
| `probe/gpui-kit` | deleted once the branch lands — it was the feasibility probe (2026-09-08, 0.6.0 / 0.3.4), superseded by this branch |

Nothing else changes. The public API of every module Geode imports —
`table/*`, `input/*`, `button`, `title_bar`, `status_bar`, `root`, `theme`,
`icon`, `avatar`, `scroll` — is identical between the pinned rev
(0e2fb7a, 2026-08-28) and 0.6.2 (diffed by `pub fn` signature on
2026-09-18: additive only, see §2's amendment); the modules 0.6.x add are chat-UI ones (`bubble`, `message`,
`attachment`, `shimmer`) Geode does not touch. The probe branch compiled
Geode with zero source errors at 0.6.0 / 0.3.4 and passed the whole suite
(1,548 tests). Before trusting that result at 0.3.5, the implementer diffs
gpui-pre 0.3.4 → 0.3.5's public API the same way (the 0.3.4 step over our
zed rev was purely additive); a removed or re-typed item is reported before
any code changes, not worked around silently.

`gpui-pre-macros` rewrites `gpui::` paths inside macro output to
`gpui_kit::` only when the calling crate depends on `gpui-kit`. Geode
aliases `gpui = { package = "gpui-pre" }` and never depends on the umbrella,
so `#[gpui::test]` and every other macro use is unchanged.

### 3.2 The invariant, rewritten

The old rule (root `Cargo.toml` comment; CLAUDE.md "gpui / gpui_platform git
deps must stay unpinned") said: gpui-component references gpui as an
unpinned git dependency, so pinning our copy would make cargo build two
incompatible copies; reproducibility comes from `Cargo.lock` alone;
gpui-component itself is pinned by rev. All three sentences are now false.

The new rule, in both places:

> **Every gpui-kit crate (`gpui-component`, `gpui-kit-assets`, `gpui-base`,
> `gpui-component-macros`) and every gpui-pre crate (`gpui-pre`, `gpui-pre-platform`) is `=`-pinned in the root
> `Cargo.toml`.** gpui-kit depends on `gpui-pre` with a caret, so an unpinned
> entry would let `cargo update` move gpui under us. Bump both families
> together, deliberately, on a branch; read the gpui-pre crate description
> for the zed rev it snapshots. There is no git dependency on zed or on
> gpui-kit any more, so the old two-copies hazard cannot recur.

### 3.3 Where the source lives now

CLAUDE.md, the `gpui-component-inventory-check` memory and several module
headers say "the pinned checkout's `crates/ui/src/…`". After Phase 1 the
source a maintainer reads is the registry copy,
`~/.cargo/registry/src/*/gpui-component-0.6.2/src/` (and `gpui-base-0.6.2/`
for the unstyled layer). CLAUDE.md, the memory and **every module header
or comment that cites the pinned checkout** (`sidebar.rs`, `toolbar.rs`,
`dialog.rs`, the market-data and blotter delegates' "pinned rev" remarks,
and any other `crates/ui/src/<file>` citation `grep` finds) are updated to
cite the registry path with the version in it
(`gpui-component-0.6.2/src/<file>`) — user ruling 2026-09-17: a comment
that names a path a maintainer cannot open is worse than none. Where a
header records a decision made against the old rev ("the pinned checkout's
`Sidebar` is a 255 px drawer…"), the implementer re-reads the cited file at
0.6.2 and either confirms the basis still holds (and cites the new path) or
reports that it no longer does — a re-pointed citation must not vouch for a
file that changed.

### 3.4 Re-verification list

Geode leans on several behaviours of the pinned rev that were discovered
the hard way. Each has a test; the upgrade must keep every one green, and
a change in any of them is a finding to report, not a test to relax:

| Behaviour | Test |
|---|---|
| `Root` holds a focused `InputState` strongly; an occupant must blur, then drop (CLAUDE.md, market-data §8.7) | `the_editor_gives_up_focus_before_it_is_dropped`, `menu_closes_an_open_picker_with_a_blur_before_opening` (`geode-marketdata/src/tile.rs`) |
| `TableState` caches `column()`'s answers in `col_groups`; a plan change needs `refresh` | `a_presentation_change_rebuilds_the_plan_on_a_same_column_snapshot` (`geode-blotter/src/delegate.rs`), `the_line_numbers_global_paints_a_gutter_on_the_next_draw` (`geode-blotter/src/tile.rs`); the CVI ladder header is a display check |
| `TableState::set_selected_row` stops propagation of its own | `the_menu_rows_stop_propagation_keeps_the_pickers_focus`, `a_double_click_opens_the_editor_on_the_cell` (`geode-marketdata/src/tile.rs`) |
| `warning_foreground` falls back to `primary_foreground`; `muted_foreground` over `muted` is under 3:1 on 15 themes | `dirty_and_sent_cells_are_readable_on_every_bundled_theme` (`geode-marketdata/src/delegate.rs`), `every_header_tone_is_readable_on_every_bundled_theme` (`tile.rs`), `every_bundled_theme_keeps_generated_hues_readable` (`geode-shell/src/theme.rs`) |
| `DataTable`'s key context is bound to `NoAction` so vim keys reach the blotter | the blotter and market-data key tests as a body |
| The theme JSON schema (38 bundled themes load) | `geode-shell/src/theme.rs`'s bundled-theme tests |
| `Dialog`'s 250 ms entrance animation is still hardwired (`dialog/dialog.rs`, `ANIMATION_DURATION`, ungated by `reduce_motion` even on git HEAD) | not a test — recorded so nobody reopens "use `Dialog` now" without reading §4.1 |

**As verified (Task 8 of the plan, 2026-09-18):** every behaviour in the
table held at gpui-component 0.6.2 / gpui-pre 0.3.5 and every named test
passed unchanged — except that one named test, `a_double_click_only_moves_the_cursor`,
had already been superseded on `main` by `a_double_click_opens_the_editor_on_the_cell`
before this branch started (the 2026-09-17 mouse-editing reversal, `main`
ea429bb→eddb4ea: a double-click now opens the editor); the table above now
names the successor, and the pinned-rev behaviour that row guards
(`TableState::set_selected_row` stopping propagation of its own) is still
covered by `the_menu_rows_stop_propagation_keeps_the_pickers_focus`. The
plan enumerated 63 comment sites citing the pinned checkout; the sweeps
found 9 more the regexes had missed (72 in all). Every one was re-read at
the new versions: 5 no longer held as written and were restated under a
controller ruling, and 2 more were rewritten under a ruling on other
grounds (a missed checkout path; a wiring not re-readable on this host),
and the rest were confirmed, with those citing a path or line number
re-pointed:

- `crates/geode-shell/src/shell/dialog.rs:225` — gpui-base's
  `on_action_search` now propagates `ctrl-f` when the input is not
  `searchable` (it returned without propagating at the old pinned rev)
- `crates/geode-shell/src/tiling/docks.rs:17` — the dock framework is now
  split between `gpui-component-0.6.2/src/dock/` (the `Panel`/`PanelView`
  skins and `DockSkin`) and `gpui-base-0.6.2/src/dock/`
  (`DockArea`/`PaneTree`/`TabGroup`/`DockAreaState`/drag-and-drop), not one
  directory
- `crates/geode-shell/src/listfilter.rs:23` — `left`/`right` now
  propagate at the text's edges when the selection is empty, rather than
  being swallowed unconditionally
- `crates/geode-shell/src/shell/occupants.rs:318` —
  `Window::focused_node_id` was renamed `focus_node_id_in_rendered_frame`
- `crates/geode-shell/src/shell/hot_reload.rs:429` — the zed-checkout
  path citation is re-pointed to `App::push_effect` in
  `gpui-pre-0.3.5/src/app.rs`
- `crates/geode-shell/src/shell/mod.rs:1133` — the Windows activation
  wiring citation was not re-read on this macOS host
- `crates/geode-marketdata/src/popup.rs:60` — `SharedString` now wraps
  `smol_str::SmolStr`, inline at ≤ 23 bytes

### 3.5 Verification

In order: `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D
warnings`; `cargo test --workspace`; `cargo bench --workspace --no-run`;
`cargo check -p geode-shell --features test-support --all-targets`;
`zsh scripts/mutation-check.sh --anchors-only`; push and watch Windows CI;
then the display checks below on a real window, which the implementer
cannot do and the user does:

- the shell opens, the title bar's traffic lights and drag still work;
- a blotter and a CVI panel paint, sort, expand, and edit a cell
  (blur-then-drop: after `escape` from a cell editor, `ctrl+k` opens the
  palette);
- a modal dialog opens instantly (our modal, not `Dialog`);
- the 38 themes still switch from the palette and the named-colour swatches
  paint;
- the scope-bar text field types and `mod+/` returns focus to it after an
  overlay;
- the CVI panel's cursor cell and the blotter's selected row still read as
  selected (upstream 0.6.2 dropped the component's own selection OUTLINE —
  #3108 — so if the cursor cell lost its border, that is the reason).

Any pixel difference beyond the selection outline is unexpected: nothing
else in a styled component Geode uses changed between the two revs.

## 4. Phase 2 — the audit

The question was: Geode rolled a lot of its own components, often for good
reason, but some are bare; where would a fuller gpui-kit component make
sense? The answer, surface by surface, from the 0.6.1 source and Geode's
own module headers (which record most of the original reasons).

### 4.1 Surfaces we keep our own — with the reason on record

| Surface | Ours | Theirs | Ruling |
|---|---|---|---|
| **Modal dialogs** (settings, keybindings, object dialog) — `shell::dialog::render_modal`, the `GeodeModal` scaffold | instant backdrop + panel painted in `ShellView::render`; normal/filter modes; escape ladder; `tab`/`ctrl+a`/`ctrl+x` reclaimed; digit slot jumps | `Dialog` (ok/cancel props, `close_on_escape`, gpui-base `focus_trap`), `AlertDialog`, `Sheet`; **250 ms entrance animation hardwired**, ungated by `reduce_motion` on git HEAD | **Keep ours.** The animation reason stands and the keyboard-ownership reason has grown since (interaction-model spec §§4, 16–20). `AlertDialog` is the nearest thing to `dialog::confirm_row`; ours is a row in the same keyboard model, which is the point. |
| **Command palette** — `palette.rs` | fuzzy scoring with per-character highlight, `"{title} {category}"` at half weight, frecency bonus persisted in `session.toml`, themes and scopes as rows | `Command`/`CommandState`: groups, separators, keywords, async `on_query`, loading state, wrap navigation skipping disabled rows; matching is **case-insensitive substring** | **Keep ours.** Strictly more capable where it matters (ranking). Section headings are the one idea worth borrowing, paintable in our own list. |
| **Dimension picker** — `shell/picker.rs` | two stages (columns → values), `tab` ticks, `ctrl+a`/`ctrl+x`, modal keyboard ownership | `Select`/`Combobox` are single-value dropdowns over `SearchableList` (substring match, `is_item_checked` for a multi-select renderer) | **Keep ours.** The two-stage flow and keyboard model have no upstream shape. |
| **As-of selector** — `shell/asof_view.rs` | text field + generation presets, `HH:MM` on today's local date | `time::DatePicker`/`Calendar` (presets, ranges, `first_day_of_week`) | **Keep ours, add a calendar** — slice §5.2. |
| **Market-data `⋯` popup** — `geode-marketdata/src/popup.rs` | `deferred(anchored)` to escape the tile clip; `Popup::Menu`/`Popup::Picker`; capture-phase toggle; insert-mode contract | `PopupMenu`: submenus, checks, shortcut hints from the keymap, `SelectUp`/`SelectDown`/`Confirm`, scrollable | **Keep ours.** Theirs is richer (submenus, shortcut hints) but action-dispatch-based, which fights registry-dispatched verbs, and the focus/propagation traps §8.8 records would recur with less control. Borrow the shortcut-hint idea (§5.1 paints chords). |
| **Sidebar rail** — `shell/sidebar.rs` | fixed 40 px strip, static icons, no collapse | `Sidebar<E>`: 255 px animated nav drawer, `ListState`-virtualised, `SidebarItem` groups | **Keep ours.** Recorded decision in `sidebar.rs`, still right. |
| **Settings dialog** — `shell/settings_view.rs` | flat row list in the keybinding dialog's mould, normal/filter modes | `setting::Settings` composite (sidebar pages, groups, items, own search) — the very composite Geode **used and replaced** | **Keep ours.** 0.6.1's is the same mouse-first shape. |
| **Which-key** — `shell/whichkey.rs` | overlay of continuations | nothing equivalent; `Kbd` overlaps on chip formatting | **Keep ours.** |
| **Small chrome** — `dialog::badge`, `value_chip`, key chips, the tick, `title_extra` crumb, the 14 px `swatch` | tiny, and every click routes through our own handlers (`stop_propagation` on the tick, §18.9) | `Badge` (dot/count), `Tag` (variants), `Kbd` (`binding_for_action`), `Checkbox`, `Breadcrumb`, `ColorPicker` | **Keep ours.** Swapping buys consistency with upstream's look, not with Geode's; low value, nonzero routing risk. |
| **Tiling and docks** — `geode-shell::tiling` | i3-style pure tree, global workspaces | `DockArea` (tabbed panels; the `tiles` canvas was removed on git HEAD) | **Keep ours.** Fundamental. |

### 4.2 Capabilities Geode has nothing for

These are where gpui-kit adds rather than replaces. Numbered as presented;
the user approved 1, 3 and 4 for Phase 3.

1. **`Tooltip`** — hover hints with a `key_binding`. Geode has none; the
   TODO's "not discoverable to press enter" is this gap. **Approved → §5.1.**
2. **`Notification`** (toasts, autohide, placement) — for "sources changed —
   restart to apply", a rejected reload, an egress ack. Deferred; today's
   stripes and tile notices stay.
3. **`DatePicker`/`Calendar`** for the as-of selector. **Approved → §5.2.**
4. **`Progress` (indeterminate)/`Spinner`/`Skeleton`** — ingest progress, a
   tile awaiting a delivery. **Approved for the status bar only → §5.3**,
   as an indeterminate `Progress` strip (the spinner's look was ruled out);
   the tile-skeleton and requery-in-flight forms were offered and not taken.
5. **`TextView`** (Markdown) + `highlighter` — a help tile rendering `docs/`,
   perhaps a highlighted `:filter <expr>`. Not the retired config editor
   (Phase 4c ruling stands: no editor, no `tree-sitter-toml`). Deferred.
6. **`Collapsible`/`Accordion`, `DescriptionList`** — the diagnostics tile's
   sections and the schema inspector rows. Deferred pending the diagnostics
   rework (interaction-model §20 left that tile untouched on purpose).
7. **`InputGroup`** (git HEAD only) — a real prefix affix for the command
   line's `/`/`:`. Deferred; unreleased.
8. **Charts.** The chart spike (`docs/superpowers/spikes/`) concluded Geode's
   dense cases — 1M-point polylines through `paint_path` with real min-max
   decimation, dense fields through `paint_image` — need our own core
   (scales, hit-testing, hover bus as pure logic). gpui-kit's `plot` layer
   (scales, shapes, grid, axes, tooltips; `f32`; lyon-tessellated per build,
   no decimation, no field path) is a fit for the **light end**: a term
   structure, a smile, a spot-ladder bar chart, a header sparkline. The
   git-HEAD chart commits since 0.6.1 are hover animation and path caching,
   nothing structural. Ruling deferred to the chart module's own design:
   the choice is "our core for dense, `plot` for light, or one core for
   both", and it needs the roadmap's chart slice to decide.
9. **`gpui_base::test_support`** (`find`, `snapshots`, `registered_paths`)
   — headless UI assertions, released in 0.6.1 (only the `gpui_kit::test`
   facade is git-only). Could replace some `VisualTestContext` scraping.
   Deferred; try on the first new window test that would be shorter with it.
10. **`focus_trap`** — read before any more focus-restore work; it may be the
    primitive under `pending_focus_restore`. Deferred.

## 5. Phase 3 — the three slices

Each is its own branch after Phase 1, with its own plan, tests in the
seam's existing test file, and mutation-harness entries for every behaviour
it adds. None touches the keyboard model: a tooltip and a calendar are
mouse forms of things the keyboard already does, and a spinner is display.

### 5.1 Tooltips

**What:** gpui-kit `Tooltip` on every mouse affordance that has a keyboard
twin, showing the affordance's title and, where one exists, its chord as a
`Kbd` chip.

**Where** (each a row in the plan): toolbar buttons; scope-bar chips — the
full selection list when a chip is truncated; status-bar segments — the
as-of segment shows the full resolved timestamp, the diagnostics summary
says "click to open" and names the count; sidebar workspace discs — name
plus `mod+N`; the market-data `⋯` button; tile-header badges — `filtered`
shows the expression, `Behind` shows the newer generation's time. The
palette, dialogs and command line get none: they are keyboard surfaces
whose footers already teach their keys.

**How:** one helper, `shell::tips` (pure where it can be: the chord text is
resolved from the live keymap through the same lookup `whichkey` uses, so a
rebound chord shows the trader's binding, not the default). The helper
decides the delay, size and chip formatting once; a call site passes a title
and an optional action id. A module calls it directly — every module
already depends on `geode-shell` for the hosting contract — and the chord
lookup covers a module's own fragment bindings, since those are spliced
into the same keymap.

**Rules:** hover-delayed and mouse-only by nature — the keyboard has
which-key and the footers, so every action stays keyboard-reachable without
the tooltip. A tooltip never carries an action of its own. It is built per
hover, not per frame: `Tooltip` is gpui's own deferred tooltip mechanism, so
an idle window pays nothing.

**Tests:** window tests asserting the tooltip element for each site's id
carries the expected title and chord text; a rebinding test showing the
chip follows the user layer.

**As built (2026-09-18):** `geode_shell::tips` — `TipModel::resolve`
(pure), `tip(selector, title, action, detail)` for all-literal sites and
`tip_with(SharedString, SharedString, action, detail)` for owned strings
(both return the `.tooltip(..)` closure; the chord is resolved on hover
from the `Chords` global through `try_global`, so a fixture without one
paints a chord-less tooltip), and `render_tip` (title, one `key_chip` per
keystroke, muted detail; selectors `tip-<site>`, `-title`,
`-chord-<ctrl+k>`). Three departures from the paragraph above, each a
ruling: the chip is Geode's `key_chip`, not gpui-component's `Kbd` (`Kbd`
hardwires uppercase key names; every chip in the app is lowercase in the
data face); the module-visible keymap read is a second gpui global,
`tips::Chords`, republished by `hot_reload` after every keymap rebuild —
the market-data `⋯` button has no other path to the live keymap; and
**attaching a tooltip allocates nothing of OURS per render** — the model
owns every string a tooltip needs as a `SharedString` (`Chip.full`,
`tip_selector`, `close_selector`, `close_title`; `ScopeBarModel.expr_full`,
`text_tip`; the blotter's `filter_tip`, recomputed at every `tile_scope`
assignment; the market-data tile's two selectors and the prepared state
text), and a literal is always `SharedString::new_static` — `From<&str>`
inlines ≤ 23 bytes and heap-allocates above, and a site must not depend
on a title's length. That claim does not extend to gpui's own `.tooltip()`
plumbing (`Rc::new` of the builder at attach, plus the hover-check
closures and boxed mouse listeners at paint — roughly eight small objects
per stateful element per paint): that cost is the same unavoidable cost
every `on_mouse_down` in the app already pays, ours to reduce no further,
and an idle window draws no frame and pays none of it. Nor does the helper
decide the hover delay, despite the "How" paragraph above saying so — it
inherits gpui's own `DEFAULT_TOOLTIP_SHOW_DELAY` (500 ms) and sets nothing;
what it decides is the content shape (title, chord, detail) and the chip
formatting. `render_tip` itself is called once per hover, not once per
frame, but the element tree it returns is re-rendered by gpui on every
frame the tooltip stays shown — that re-render formats nothing in release
builds, since `render_tip`'s own `format!`s live only inside
`debug_selector` closures, which release's own `debug_selector` never
calls. Sites: sidebar discs + profile icon; scope-chip bodies (the full
selection) and close glyphs, text/expr (whole expression)/impossible
chips, the AS OF badge (whose tooltip title is the as-of instant's full
`YYYY-MM-DD HH:MM:SS` local form, `ScopeBarModel.as_of_full`, with the
elided badge/segment text moved to the detail line); the status bar's
diagnostics summary and as-of segment (the same `as_of_full` title); the
blotter's `filtered` (the tile's filter, re-read after a text filter same
as an expression one) and `unscoped` pills; the market-data `⋯` (chord
`.`) and `Behind` state run — the one title left at minute precision,
since it already reads "update HH:MM" and a Behind tooltip only widens
that same run's own text, a known, deliberate limit rather than an
oversight. Tested by hover in `VisualTestContext` (mouse move → 600 ms →
`run_until_parked` → `tip-<site>` bounds), including a negative case for
the `Behind`-gated state tooltip and the filter-text path. Display check
pending: placement and theme colours on a real window.

### 5.2 As-of date picker

**Keyboard path (grammar):** `parse_as_of` (`geode-core::query`) accepts
two more forms beside today's `HH:MM`, `HH:MM:SS`, RFC 3339 and `live`:
`YYYY-MM-DD` (that day's 23:59:59.999 local — the newest generation of that
day, the same "at or before" resolution as everything else) and
`YYYY-MM-DD HH:MM[:SS]` (local). Times stay the trader's local clock
throughout (Phase 4a ruling). The preview, the scope bar and the status
segment already display local time and need no change.

**Mouse form (§17 parity):** a gpui-base `Calendar` pane in the as-of
dialog beside the presets. Clicking a day rewrites the date portion of the
field's text — keeping any time already typed, defaulting to the
end-of-day form otherwise — through `dialog::sync_dialog_text`, the one
door that writes the shared `Input` (interaction-model §16). The calendar
never takes focus: it is a mouse affordance for the field, the same
relationship the frozen filter row has to `/`. Its selected day mirrors the
field's parsed date on every keystroke, and it is not shown while the field
reads `live`.

**Unchanged:** presets, `live`, the warning stripe, `:asof undo`, the
`AsOfState` pure core's shape (it gains a `date: Option<NaiveDate>` derived
from the parse, nothing more).

**Tests:** grammar tests in `geode-core` for both new forms and their
local-time resolution across a DST boundary; window tests that a day click
rewrites the field and that typing moves the calendar's selection; a
mutation entry for the end-of-day default.

### 5.3 Ingest progress in the status bar

**Data layer:** the ingest runner emits `IngestEvent::Started { source,
path, queued }` when it pops a job (file or document), where `queued` is the
depth left behind it; the service forwards it as `DataEvent::Loading {
source, path, queued }`. `Published` and `Failed` already carry the path
and are the clear. No other event changes; `PlanComplete` is untouched.

**Shell:** `geode_shell::diagnostics::Diagnostics` gains an `ingest:
Option<IngestActivity { source, path, queued, since: SystemTime }>` under its
own `DiagVersions` counter (`sources` is the natural one — the sources
section paints it), set by `Loading` and cleared by the `Published`/`Failed`
whose path matches; a mismatched clear is ignored, so an event delivered
out of order cannot erase a newer load's record. The bridge routes the
event beside `Health`.

**Status bar:** while `ingest` is `Some`, two things paint; while it is
`None`, neither does, so the animation's redraw loop runs only during a
load and an idle window pays nothing per frame (charter: nothing may stall
the render thread; per-frame churn is a defect).

1. **A 2 px indeterminate strip along the status bar's top edge** — gpui-kit
   `Progress::new("ingest").loading(true)` at `Size::Size(px(2.))`, full
   width, themed on `primary` by the component's default. It is placed on
   the bar's top border rather than inside a segment so that nothing in the
   bar's layout moves when a load starts or ends: the strip appears, slides,
   and vanishes; the segments beside it never shift. `loading(true)` is the
   component's own indeterminate mode (a sliding indicator over a
   20 %-tinted track, 1 s repeat, ease-in-out), and it already honours
   `reduce_motion` by parking a static centred segment — no Geode code
   animates anything.
2. **A text segment**, `loading <source> · <n> queued`, static, reading the
   entity's cached summary the way the diagnostics-summary segment does,
   never formatting per frame.

**Why an indeterminate bar, not `Spinner` or a determinate `Progress`:** a
CSV load has no known fraction — staging reads the whole file before it
knows the row count — and a document publish is milliseconds, so a
determinate bar would be invented; and the user ruled against the spinner's
appearance (2026-09-17). The indeterminate strip is the idiom every browser
and IDE uses for "working, duration unknown", and gpui-kit ships it.

**Diagnostics tile:** the sources section paints the same record on the
loading source's row (`loading <path> since HH:MM:SS`).

**Tests:** runner tests that `Started` precedes each `Published`/`Failed`
with the right depth; a shell test that `Loading` then `Published` for the
same path leaves `ingest` `None` and a foreign path's `Published` leaves it
`Some`; a status-bar test that neither the strip nor the segment is present when
idle and both are while loading; mutation entries for the path-matched
clear and the idle-absent rule.

## 6. Out of scope

- Replacing any surface in §4.1. Each ruling there is deliberate and dated.
- The tile-skeleton and requery-in-flight indicator (§4.2 item 4, not taken).
- A git-rev pin of gpui-kit, `gpui_kit::test`, `InputGroup`, charts, the
  help tile, notifications, focus-trap — all §4.2 deferrals with their
  reason beside them.
- Multi-window (foundation spec §3.6) — still deliberately unbuilt.

## 7. Open items

None at approval. The gpui-pre 0.3.4 → 0.3.5 API diff (§3.1) is the one
check whose result could change Phase 1's shape, and it is done first.
