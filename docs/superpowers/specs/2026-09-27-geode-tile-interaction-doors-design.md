# The `geode-tile` Crate: Interaction Doors — Design

Group G, first slice, of the 2026-09-25 codebase review
(`docs/superpowers/reviews/2026-09-25/architecture.md` M1 and I30; `SYNTHESIS.md`
"the architectural verdict"). Approved in conversation 2026-09-27.

## 1. Problem

`TileContent` (`crates/geode-shell/src/module.rs`) asks a module for ten methods
and gives it nothing back. The shell offers *paint* doors (`chip`, `control`,
`listrow`, `kbd`, `scale`) but no *interaction* doors, so each module rebuilds the
same behaviour. Re-measured at `e8cf6528`, 439 commits after the review:

| Mechanism | Copies today |
|---|---|
| Anchored popup surface | `popover_surface` byte-identical in marketdata `popup.rs`, timeseries `popup.rs`, pricer `popup.rs`. Geometry constants (`ROW_HEIGHT 26`, `ROW_INSET 8`, `MIN_WIDTH 240`) declared three times. The `deferred(anchored()…snap_to_window_with_margin(8)).with_priority(1)` block inlined 7 times; only timeseries has an `anchor_popup` helper and `row_shell`/`empty_row`. |
| `.` action menu | Three diverged types: marketdata `core/menu.rs` `MenuRow`, timeseries `core/menu.rs` `MenuRow`, pricer `popup.rs` `MenuItem`. Different step rules, static vs live key hints, `&'static str` vs `SharedString` disabled reasons. `render_menu` is ~120 lines in each. |
| In-tile y/n confirm | Two near word-for-word copies: marketdata `PendingUpload` (upload) and pricer `PendingRemove` (`:rm`). Each is focus + blur subscription + prompt, where `y` confirms and anything else cancels. |
| Notice slot | Five shapes and three tone enums: blotter `error: Option<(String, chip::Tone)>`; marketdata `notice` plus `upload_error` plus a header `Tone`; timeseries `notice` (always danger); pricer three slots with `NoticeTone`. |

The review's point: every copy is *correct*, because each carries the same
hard-won rulings (`occlude()`, blur before drop, a disabled row takes no fill). That
is the problem. A sixth module re-derives them, and a fix lands in one copy.
market-data and the pricer both record "a user rebind is not reflected in the menu"
as a known limitation that timeseries already solved.

## 2. Rulings (Matthew, 2026-09-27)

1. **G is decomposed.** This spec covers the interaction doors only. Later slices,
   each with its own spec, cover the flip-barrier helper, the shared motion
   vocabulary, the shared tile header with source health (review ruling 3), and
   windowed grid models (M3).
2. **A new `geode-tile` crate now**, rather than shell submodules.
3. **Menus merge on the timeseries rules.** Key hints come from the live keymap,
   disabled rows show their reason, and stepping from a non-action row lands on the
   first enabled action.
4. **Ordering with review ruling 2 (market-data panels as configuration).** This
   slice and the barrier slice leave market-data's `PanelSpec`, header layout and
   grid model untouched. Panels-as-config comes before the header and grid-model
   slices.

## 3. The crate

```
geode-core ─┬─> geode-shell ─> geode-tile ─> blotter · marketdata · timeseries · pricer · diagnostics
            └────────────────────────────┘
```

- **Dependencies.** `geode-tile` depends on `geode-shell` (for `kbd`, `control`,
  `scale`, `tips::Chords`), `geode-core`, `gpui` and `gpui-component`. It does
  **not** depend on `geode-data`. The barrier slice decides whether it may, or
  whether `Refusal` moves to `geode-core`.
- **Direction.** `geode-shell` never depends on `geode-tile`. The shell hosts tiles;
  `geode-tile` is the kit tiles are built from.
- **The rule.** A tile mechanism two modules would otherwise each write lives in
  `geode-tile`, and that covers interaction behaviour (keys, focus, open and close,
  precedence), not only paint. CLAUDE.md's module rules gain this line, and its
  dependency rules gain the crate's position.
- **Build hygiene.** Lib target `bench = false`. Its dev-dependencies enable the
  same `geode-*` test features the rest of the workspace does, so that
  `cargo test --workspace` does not build `geode-shell` twice.
- **Public surface.** Four modules, `popover`, `menu`, `confirm` and `notice`,
  documented in a crate README with a module map and the rule above.

## 4. The doors

### 4.1 `popover`

- The popup surface: today's byte-identical `popover_surface`, moved.
- `anchor_popup(content, corner)`: the anchored, deferred, snapped, priority-1
  wrapper. It replaces all 7 inlined blocks. Its snap margin is one constant.
- `ROW_HEIGHT`, `ROW_INSET`, `MIN_WIDTH`, `SNAP_MARGIN` declared once.
- `row_shell` and `empty_row`, moved from timeseries: the common row frame and the
  "nothing here" row.

The door owns geometry and layering only. What a popup lists is module content.

### 4.2 `menu`

One model and one renderer for `.` action menus.

- **Rows.** `Action`, `Separator`, `Section`. An `Action` carries:
  - an id, which the module maps to its own verb;
  - a title;
  - its key hint, as an action identity resolved to keystrokes through the live
    keymap (`tips::Chords`) at build time, not a string;
  - `enabled: Result<(), SharedString>`, where the error is the reason shown on the
    row;
  - an optional short reason for the compact lane;
  - `checked: Option<bool>`.

  The pricer's `View { name, current }` rows become Actions with `checked`.
  timeseries' `Trailing`, `start` and `custom_row` extensions survive only if the
  plan shows a timeseries row needs them. Otherwise they become ordinary Action
  fields.
- **Stepping.** `step(from, direction)` and `first_enabled`. From a non-action row,
  step lands on the first enabled action (the timeseries rule). Disabled actions
  are skipped. An all-disabled menu has no cursor.
- **Rendering.** One renderer with the hard-won rulings: a disabled row takes no
  fill, the key lane paints through `kbd`, and the reason sits in the row. Hover
  and pressed states go through `control`.
- **Keys.** Unchanged in this slice. Modules keep their own `menu_down`/`menu_up`
  and pick action ids and bindings, and map them onto `step` and pick. User
  overrides are untouched. The shared vocabulary is a later slice.

### 4.3 `confirm`

The in-tile y/n prompt.

- **While armed:** the confirm holds focus and paints its prompt through `notice`.
  A bare `y` confirms and runs the module's action. Any other key, a pointer press
  anywhere in the tile, or a blur cancels it. Focus returns to where it was when
  the confirm was armed. Blur-then-drop is observed: the prompt's focus handle is
  released before the state drops.
- **Module supplies:** the question text and the confirm action.
- **Door owns:** arm, disarm, cancel-on-key, cancel-on-pointer, cancel-on-blur and
  focus restore. These are the parts that are word for word the same in
  marketdata and the pricer today.

### 4.4 `notice`

- `Notice { text: SharedString, tone: Tone }`.
- `Tone { Status, Warning, Danger }`.
- One paint, in theme tokens.

Precedence between several slots stays module logic. The pricer still decides
between pricing, view and save notices, and market-data between its notice and
upload error. The door paints the winner. market-data's header `Tone`
(Plain/Key/Time/Warn/Error) is a header-segment palette, not a notice tone, and is
out of scope except where it paints a notice.

## 5. Migration

One module at a time. Each step moves behaviour, keeps the suite green, and is
separately reviewable.

1. **Pricer**, all four doors. It has every mechanism and the best-specified
   copies, so it proves each API.
2. **Market-data:** popover, menu, confirm (the upload `y`) and notices. `PanelSpec`,
   the header layout and the grid model are untouched.
3. **Timeseries:** popover, menu and notice. Its menu already follows the winning
   rules, so this is a move.
4. **Blotter and diagnostics:** notice only.

**Deliberate behaviour changes**, all in market-data and pricer menus:
- key hints follow user rebinds;
- stepping from a separator or section lands on the first enabled action;
- disabled rows show their reason.

Also, notice tones collapse to three across all tiles. Nothing else changes: keys,
action ids, user overrides, prompts' wording, and every module's content.

## 6. Testing

- **`menu`, pure:**
  - `step` and `first_enabled`: disabled rows, separators and sections, both ends,
    an all-disabled menu, and stepping from a non-action row.
- **`menu`, GPUI:** a hint follows a user rebind through the live keymap (a real
  keymap override, not a stubbed string).
- **`confirm`, GPUI**, all with real key and pointer events:
  - `y` confirms and runs the action exactly once;
  - another key cancels without running it;
  - a pointer press cancels;
  - a blur disarms;
  - focus is restored after each.
- **`popover` and `notice`:** render tests for the anchor corner and snap, and one
  paint per tone.
- **Modules:** existing tests pass unchanged. A test asserting a deliberately
  changed behaviour (a static hint string, the old step rule) is updated in the same
  commit, and the commit message names it.
- **Harness:** each door's contracts get mutation entries anchored in `geode-tile`,
  each naming a test and verified three ways (`--build-check`, named run `caught`,
  hand application failing on an assertion). Module entries whose anchors move into
  the crate are re-aimed, not deleted. Where two modules' entries now guard one
  shared line, the checker's REDUNDANT/ALSO rules decide.

**Display checks owed:**
- the market-data and pricer menus with a rebound key;
- a disabled row's reason;
- the three notice tones in each tile;
- the upload and `:rm` confirms.

## 7. Documentation

- New `crates/geode-tile/README.md`.
- CLAUDE.md: the dependency line and the module rule (§3).
- `docs/current/architecture.md`: the crate's place in the graph.
- `docs/current/features.md`: the interaction rules every module follows, now
  pointing at the doors.
- Each module README: its local copy of these mechanisms removed from the module
  map; the "rebind not reflected" limitation removed from market-data and pricer.
