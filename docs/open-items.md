# Open items

This is the backlog of open work and owed checks that previously lived only in
per-feature handoff notes: parked follow-ups, deferred minors, decisions waiting
on the owner, known limitations flagged for later, unrun verification, and
display checks (visual facts no headless test can see, to be confirmed by eye in
a real window). It reflects the newest knowledge as of 2026-10-04. Delete an
item when it is done or ruled out; do not let this file grow a history.

Conventions: an item marked *(unverified)* comes from an older note in an area
that has changed since, and was not re-checked against the code; confirm it is
still true before working on it. Items not so marked were either checked against
the code or recorded within the last few days. Display checks are listed as
recorded; nothing on record says they were done. "Decision:" marks a question
for the owner, or a ruling made on the owner's behalf that is still open to reversal.

## Shell and dialogs

### Open work

- Decision: `TileContent::launched` (the market-data auto-prompt) can take focus
  from a dialog left underneath when `tile::add` / `tile::open_with` run from
  the palette over a dialog. Untested. Separately, a modal backdrop click also
  focuses the tile behind it.
- Decision: in the Grouping dialog, saving over a user-owned slot asks y/n and
  saving over an inherited slot forks with a notice. The mockup had every filled
  slot ask. Applying the same treatment to the "picker framework" waits for the
  owner to ask.
- Grouping dialog minors: a double-click on a filled row reaches whatever is
  beneath the closed dialog; a reload inside the 250 ms debounce after defining a
  slot from the list drops its activation; `restore_ad_hoc(_, false)` bumps the
  grouping generation; a hand-edited session with both a slot and an active ad
  hoc chain drops the slot silently; lead rows are rebuilt eagerly.
- Scope dialog: the Saved screen's `e`/`d`/`r` have no pointer route (a `⋯`
  menu would give one). Row ids on Current and Saved are positional.
  `note_saved_as` is kept after a failed write, and an optimistic refresh survives
  a rejected merge.
- Scope dialog: a saved scope deleted mid-session leaves `loaded_from` until
  restart (the title is correct). `mod+s` naming then `escape` takes two escapes
  to reach the Term editor. Counts show `n`, not the spec's `n of m`.
  `toggle_named`/`inline_named` lack follower-route tests and `link:` entries.
  `used_by_sentence`/`named_expression_users` could be `pub(super)`.
- Scope dialog: the spec's remaining retirements (the picker modal and
  `DialogKind::ScopeExpr`) still exist, as stacked steps.
- Scope expression suggestions: a failed values load is not retried within one
  opening; the connective hint never mentions `)`; the refusal shows twice (error
  line and warning line); hot reload re-ranks but never re-requests;
  `ExprVocab::new` is O(n²); no GPUI test for the derived-dimension refusal;
  Enter's unknown-column check is vocabulary-wide while the saved-scope reader
  checks per dataset; the footer omits `shift+tab`.
- Named-expression minors *(unverified, predate the scope dialog)*: duplicate
  names kept in a scope's `named`; a non-array `named` is erased on the next
  fold; validation re-parses on each committed edit; a tile in error keeps its
  previous snapshot under the red line; a palette scope change behind an open
  expression step is not re-synced; a save can overwrite an object dialog's
  unsaved same-name draft.
- Merge the three Tab-cycle completion state machines (timeseries
  `core/complete.rs`, `shell::exprcomplete`, pricer `core::complete`) into one type.
- Object dialog size: `objectdialog/mod.rs` is ~6.6k lines, `render.rs` ~4.9k
  and `tests/objectdialog.rs` ~8.5k. Seams: `objectdialog/editrow.rs` for edit
  rows; `objectdialog/actions.rs` for `browse_action_bar`/`action_bar`/`confirm_row`.
- Object dialog minors *(unverified)*: a desk-layer `view_presentation.toml`
  name is not refused by `Domain::name_taken`; a Views dataset switch queues a
  same-bytes `view_presentation.toml` write; `Draft::diagnostics` doc wrongly says
  `Diagnostic::path` does not exist; section headers are inert to the mouse; the
  drift note says "desk" when the shadowed layer may be builtin (needs
  `shadowed_layer` on `ObjectRow`); the column stage's value-field label reads
  `tree · Width` rather than `npv · Width`.
- Column stage (set/inherited): `↺` is live while a text field is open; a mere
  visit folds an all-false set entry (harmless dirty flag); the last assert in
  `delete_is_refused_and_r_inherits_in_the_column_stage` is vacuous; no mutation
  entry covers `writes_by_destination`'s set clause; a Schema set can go stale
  after a refused commit.
- Stacked object dialogs: the Views column stage keeps stale dataset layers
  after a stacked Schema edit; revert leaves `state.mode`; the `object_dialog`
  field's doc is stale; palette-close ordering in `open` is unreachable but could
  carry a `debug_assert`.
- Edit column from a tile (`config::view_column`/`schema_column`): the
  dialog-stack invariant exempts both actions (`REFUSES_IN_FIXTURE`) instead of
  focusing a tile with columns; Views-already-on-top is a silent no-op, untested;
  no two-equal-labels test; `BlotterTile::tile_columns` wiring untested; a tile
  whose own view copy vanished says "this tile has no dataset columns"; the
  choicedialog `commit` doc omits the Column arm.
- Verb minors: a `Draft::offers_i` would unify the footer's and `actions()`'s "is
  `i` live" tests; `press_verb`'s `i` arm has no armed-confirm guard; its read-only
  early return does not sync; settings' two click handlers share a verbatim
  prelude; the Find-commit body is spelled in both `leave_command_line` and the
  enter arm; no test notices removal of the value chip's `stop_propagation`;
  `GeodePalette`'s `shift-tab` reclaim is untested.
- Dialog text sync: `KeybindingsState::set_query` clears `listening` off-seam
  (unreachable today). A grep guard (CI or harness) asserting `focus_handle` and
  `set_value` stay out of `keybindings_view.rs` and `objectdialog/render.rs`
  would close the rest of the seam rule.
- Dialog mouse minors: `on_edit_row_clicked` moves the cursor during an armed
  confirm; `can_drop` is tautological; no window test covers the completion
  click's failure paths.
- Choice typeahead minors: `dialog::choice_rows` clones an option `String` per
  row per frame; a past-cap value lands as the bottom painted row (a centred
  window is a display call); `enter_does_nothing_in_the_settings_dialog` is
  misnamed; settings' footer says "choose" where the object dialog says "choose a value".
- Decision: cursor stops skip Groupings' `Slot`, Sources' `adapter` and similar
  rows, so their help line is reachable only by filtering to that row. Decide
  whether that help should exist. Views' `Dataset` row is a stop only with more
  than one dataset, so "opens on the first column" is fixture-dependent.
- Back button (`dialog::set_back`): a click while a y/n confirm is pending gives
  no feedback, though the tooltip still names Escape; no harness entries for
  `step_back`'s guard or the picker/choice registrations; the title shifts right
  when the button appears. The timeseries dates editor could get its own `‹`.
- Palette: render allocates a small `Vec` per row with category hits (a rebasing
  `highlight_runs` would retire it); the whole-query `ORDER_BONUS` path in
  `fuzzy_match_lowered` is unguarded by tests; a scope-only reload leaves an open
  palette's Scope rows stale; the keybindings list is not virtualized.
- Raw `theme.danger` text still bypasses the `chip_paint(Tone::DangerText)`
  readability floor in `shell/status.rs` and `shell/commandline_view.rs`, plus one
  site in `objectdialog/render.rs` *(unverified)*.
- Command-line locality minors: the `ParseError` "at column" text is duplicated
  in the blotter's `tile.rs` and `scope_expr_view.rs`; the log-level step swap is
  duplicated in choicedialog's `commit`/`Cancel` arms, and escape-back lands on row 0;
  the malformed-`as_of` restore test does not assert its log line.
- Key chips (`shell::kbd`): `marked` and `menu_spec` parse on every paint;
  hardcoded hints name the shipped key, not the live binding; macOS `Kbd` shows ⌫
  for both backspace and delete.
- Settings: the `find_style` route may still reach the blotter only on the next
  views reload, unlike `UiSettings` *(unverified)*.
- Design-guide residue (recorded, not fixed): muted secondary lines over the
  active row fill fall under 3:1 on 16 bundled themes; active/hover composites sit
  within 1.01:1 on 7. Optional theme retune for the named-color readability floor:
  Everforest Light, Solarized Dark, Catppuccin Latte, Ayu Light, Mellifluous Light.
- Control affordance minors: the sidebar's `w_full` click target is wider than its
  24 px hover disc; `value_chip` copies its selector into a `SharedString` per
  render; the pressed-vs-hover distinctness (1.05) is not sweep-calibrated.

### Display checks

- Object dialog against the "Geode Config Dialogs" mockup: crumb and pill in the
  title row, badges, grip/tick, section headers, outlined buttons.
- Object dialog rows: diagnostic glyph + label block, dimmed dataset prefix on
  Sources rows, `Field.layer` badges, drifted badge + note, `edit` pill.
- Column stage: browse-row and edit-header swatches, member-row summary
  (`npv · 120 px · k · 0 dp · delta`), crumb `tree › npv`, its seven rows and
  footer hints; provenance chip (`desk`/`dataset`/`view`); Schema column-row
  summary and crumb; set badge + `↺`, inherited rows muted with aligned spacer,
  footer `r inherit` / `shift+r inherit all`.
- Footers: change/type/reorder chip groups in every row state, the dropped
  separator on an empty change group; filter-mode footers reading
  `enter: keep the filter` / `escape: discard the filter`; the help line under the
  action bar, its ellipsis, and the blank slot under a confirm.
- Mouse: drag ghost, `drag_over` top border, cursor styles (I-beam on the frozen
  row, grab on draggable rows; `OpenHand` is unmapped on Windows), tick hit area;
  value chips in both dialogs; keybindings action bar + confirm row, `r`/`shift+r`.
- Browse bar with Delete/Revert/New buttons, the confirm row in its place, and
  the cursor landing after a last-row delete.
- Choice lists: the 12-row option list in both dialogs, Theme opening with the
  active theme as bottom row, the window sliding under a held `down`, the action
  bar's `min_h_6`.
- Edit stages (Groupings, Views, Schema) now open on a different row than before
  (cursor stops).
- Dialog stack: a pushed dialog paints alone (no bleed-through), the palette
  paints above a dialog, the backdrop dims once at depth 2, a refusal notice is
  legible under the backdrop; Views column stage → `ctrl+k` "Edit colors…" →
  create → Escape returns correctly; back button in the title row.
- Grouping dialog: rows, badges, hover controls, save prompt, the `* · chain`
  readout.
- Scope dialog Current: sections, empty rows, add controls, glyphs, counts,
  title, text field, `+` pressed fill and tooltip, stacked step titles. Saved:
  sections, notes, applied/broken marks, filter, empty-row stops, save prompt and
  its question, definition field with suggestions and used-by note, name preview,
  load glyph pressed.
- Scope expression suggestions: row layout and detail alignment, warning color
  across themes, dialog height as the list changes.
- Toolbar: mono expression term chips with `×` inset; `≡` named-expression chip
  and its danger tone; neutral `AS OF`/`LIVE` chip including `LIVE` under the
  historical stripe; expression inline error; the pressed fill of the grouping
  trigger; the hairline color (`theme.border`, faint on Bloomberg Modern).
- Edit column from a tile: palette rows under Configuration, list title
  `Edit column in view · tree`, `NPV · npv` rows, highlight on the cursor column,
  tree-column cursor opening on the first row.
- Palette category highlight (muted label, primary bold runs); two-step log
  level picker rows; palette scroll (`ctrl+f`, up from top, wheel).
- Keybindings and settings rows with highlights; Settings dialog in normal/filter
  modes.
- Key chips: glyph labels, the menu lane on highlighted rows, mono chips in
  object-dialog section headers, the timeseries footer clipping on Windows
  (`Shift+L` is longer).
- Tooltips: placement and colors.
- Status bar: count / `AS OF` / theme segments; a reload failing twice with
  different counts; fullscreen segment (`fullscreen · N hidden`, right region);
  ingest progress strip color and motion, a permanently failing file re-blinking
  the strip per poll, a possible single-frame flicker per CVI publish in `--demo`.
- Design-guide audit: shared leading spine across toolbar/header/rows/status,
  double hairlines at the tile/table boundary, browse-list scrollbar at the edge;
  Font size → Large (dialog list, status bar, `:` line grow together); rows at the
  theme's 6 px radius; hover fill on rows; neutral pills.
- Selection outline at gpui-component 0.6.2 (upstream #3108 dropped it): the
  market-data cursor cell and the blotter selected row.
- `config::open_directory` opens the config directory in Finder.

## Keymaps and input

### Open work

- Vim-style deferred resolution of ambiguous sequences is approved but not built:
  an exact match that is also a prefix waits ~500 ms or for a non-continuing key
  (replayed). `docs/current/keymaps.md` still says the matcher has no timeout.
  When it lands, bind the pricer's cell yank `y` beside `y y`/`y c`; market-data's
  `y y`/`y c` (currently unreachable behind `y`) become reachable. List every
  overlapping pair across builtin and module fragments first.
- Diagnostics page keyboard gaps: details-area scroll and column resize are
  mouse-only; level sets that are not contiguous ("only WARN") are mouse-only;
  `tab` is a no-op on sections without views.

### Display checks

- macOS reports `-`, `=`, `shift+tab` and `z shift+r` as the diagnostics page
  expects; `option+u` is a dead key on macOS for the `mod+u` link chooser.

## Tiling and tile chrome

### Open work

- Pre-existing: a closing tile that is the last key a flip barrier awaits
  notifies the frame during render; gpui drops that notify, so staged tiles keep
  pre-flip data until the next frame notification. Cheap fix: compare `flip`
  around `ensure_occupants` and defer one notify *(unverified; may be covered by
  the `DeferredDoor` work)*.
- Other modules' `TableState` subscribers may carry the `SelectRow` echo-loop
  hazard fixed in the diagnostics page (two selections in one update ping-pong
  forever unless stale echoes are dropped). Audit them.
- Row menu: the pointer menu drifts when the tile scrolls under it; no
  unfocused-tile right-press test; a deferred-close race within one effect flush in
  choicedialog. gpui-component leaves its `right_clicked_cell` outline on the
  pressed cell (no public setter).
- Workspace pin: `FrameViewMut`'s `DerefMut` lets a module reach other lanes; the
  pin glyph click is guarded only by modal-backdrop occlusion, not the refusal.
- Tile close: the double-click swallow flag clears only on a single press the
  root hovers, so a press on an overlay then the second press of a double-click
  on the root is swallowed once.
- Tile header: the blotter has no `⋯` menu; the pricer's health chip asks about
  `pricer_sheets` but never fires in production (nothing feeds it).
- Notice dismissal: the pricer's escape still clears a transient notice masked by
  `STOPPED`/`REFUSED`; a click cannot reach it.
- Stack minors: `stack_after` ignores its inner insert's bool;
  `toggle_split_orientation`'s >2-child branch with a stack sibling is untested;
  `stack::unstack` computes `content_area` unconditionally; per-keystroke
  `convert_keystroke` in the list branch; no test for the list's render-staleness
  drop or "any dispatch closes it"; a third copy of the drag tests' `area` math;
  `remove_leaf`'s `ix < active` arm is unreachable and unguarded; `validate_node`
  keeps a later leaf duplicating a stack member; `Tree::remove` activates a hidden
  member; a member hidden mid-barrier waits out the 250 ms deadline; a click into
  the scope bar leaves an open stack list painted.
- Launch context: no placeholder-fill or stacked-add launch test; a skipped
  `launched` is not logged; the rendered modal title is unasserted; the capture
  mutation hard-codes `NDX`.
- Autosize widths are stored in px, so they go stale after a font-size change
  until re-run.

### Display checks

- Stacks: the `2/4` chip in each module header and on the placeholder; the member
  list's anchor under the header; the focus ring following a cycle; a stacked
  placeholder; stack pull (`mod+shift+hjkl`) and split.
- Tile picker: double-click on a placeholder, a docked tile, and an empty dock.
- Tile header: health chip in blotter/market-data/timeseries with a degraded
  demo source; header heights line up across a split; timeseries at 22 px with
  `gap_3`; blotter times on the right; pricer notices after prompt/counts, `unscoped`
  / `N hidden` at the end of the left side; long left sides clip hard (market-data
  attribute strip or editor past the edge is invisible); status items cut without
  a tooltip.
- Close `×`: visible and muted on light and dark themes, hover/pressed fills,
  tooltip `Close tile` with its key; a narrow tile keeps it; the placeholder's `×`
  in its corner. Decision: market-data and pricer action menus now hang under the
  `×` rather than under `⋯`.
- Menus (`geode_tile::menu`) in pricer, market-data and timeseries with a rebound
  key, and an open menu during a keymap reload; lane text size, lit-row lane color,
  row hover, floored text, ellipsized headings.
- Notices: tones in each tile; hover/pressed fill on bare notice text, tooltip
  "click or `escape` dismisses", the timeseries notice line and volslice footer.
- Confirm bar (`confirm::bar`) in a narrow pricer and a narrow market-data tile,
  including clicking the prompt; upload and `:rm` confirms.
- Pinned-workspace glyph on light and dark themes.
- Autosize: painted widths, clipping at each font size, the blotter's Inter header
  against the mono measure, the refusal notice.
- Launch context: `mod+n` → CVI picker focused; placeholder double-click → CVI
  picker in place; blotter `g m` on an SPX row → "Open SPX in…" → split CVI on SPX
  with no picker; the same from a pricer line; `g m` on a subtotal → plain picker;
  restart with empty panels saved → no pickers.

## Blotter

### Open work

- A drag-resized column width resets on every table refresh (including a header
  click): the blotter handles no `TableEvent::ColumnWidthsChanged`, unlike the
  pricer and classifications tiles. Autosize widths survive; dragged ones do not.
- Grid selection minors: a selection notice can overwrite the sort-dropped
  notice; `:view` leaves a stale tint until the next delivery; no stuck-press test
  for `drag_origin`; `find_by_path`'s doc keeps an old "I3" tag.
- The `:filter` completion should switch to the core tokenizer
  (`geode_core::scope::complete`).
- Decision: the blotter has no group-row ground (the pricer has its own), and it
  keeps repeated grouping levels where the pricer drops them.
- Groupable columns: a numeric grouping label paints DuckDB's DOUBLE→VARCHAR text
  (`4200.0`) rather than `format_number` *(unverified)*; groupings test fixtures
  carry an unasserted "grain Position requires key column 'counterparty'" warning;
  the groupable list is a union across datasets, so a chain mixing two datasets'
  columns compiles for neither; the `mod+p` picker still offers `underlying2_ref`.
- Ungrouped dimensions: a spine entity with no row in the chosen grain table does
  not take part (documented); a text value literally `mixed` is distinguishable
  only by its muted color.
- The abs-sort mutation entry "click cycle: a text column's click clears after
  asc…" compiles with an unreachable-pattern warning.

### Display checks

- ` |x|` header suffix and the sort arrow direction on the 3rd/4th header click.
- Named colors in the header (`render_th`), a `tint_sign` column (define a
  `colors.toml` in the user layer; the demo ships none), the header triad.
- Value colors: tree label color per depth; `g .` → Color… → pick repaints without
  restart; pick-list swatches and muted empty line.
- `V` row tint, `v` block tint with the cursor border inside, footer totals and
  the `—†`/`—‡` legends, shift+click/drag feel, light and dark themes.
- The muted `mixed` marker on ungrouped dimension columns.
- Line-number gutter alignment and tree column widening.
- `/` scrolled and narrowed, then a snapshot landing.
- A long view-refusal message (~190 chars) in the fixed-height tile header.

## Pricer

### Open work

- No test proves that a commit whose line went away is refused; the mutation
  "pricer tile: a commit ignores that its line went away" survives by design until
  that test exists (silent-wrong-data contract).
- Storage parked items: a service-open failure strands admitted requests (all
  kinds); the quit flush through the 64-slot channel can refuse with many tiles;
  `:e`/`:new` after an unconfirmed `:name` can leave both documents.
- Core minors: `encode_overrides` does no `;`/`=` escaping; `parent < 0` is
  treated as root; `restore` does not validate that `at` is a root boundary.
- Deferred from the seam work *(unverified)*: a `DataServiceConfig` test
  constructor (many literal sites); "queue full" wording after shutdown;
  thread-spawn `expect` vs `Result`; a `PricingError` Display.
- Frame/views deferred: `PricerConfigKey.views` holds the whole views doc; a user
  blotter view named `vanilla` overrides the pricer's; `CellState::Gap` instead of
  borrowing Stale's paint; lift `theme_inputs` into `geode-tile`; an old `__tree`
  autosize width clips until re-autosized; `.` menu rows are enabled on partial
  packages and `g u` on split rows; the cursor on a group row is not persisted;
  the entry's `after <row>` reads sheet order; the find walk is unmeasured;
  find-results paint leg/group palettes without their grounds; the find painter's
  theme signature can go stale.
- Sort, left as-is: `/` results go out of date on each reordering tick; the
  viewport follows the cursor under live re-rank; selection totals sum in display
  order; `views.toml` `sort` keys do not seed the pricer's sort.
- `:view` with no argument errors ("usage: view <name>"); it could open the view
  menu. The "no views loaded" refusal is untested.
- Entry completion follow-ups: re-rank on caret-only moves; move the choice list
  and entry list popups onto `shell::listrow` with ids and hover; measure
  per-keystroke re-rank allocation on large lists.
- The expiry date field's render/key routing is copied from market-data; a shared
  helper in `geode-widgets` is a possible follow-up.
- Row striping deferred by the owner: open questions are granularity (per row vs
  per item) and pricer-only vs all tables.
- Decision, flags awaiting the owner: mixed-currency local sums paint `—` at the
  fold and footer; measures default to sign color; scoped measures are result ×
  qty while cells paint per unit; `position_ref`/`instrument_ref` are not
  scope-applicable; expiry's scope value is the ISO date; the status scope value is
  `fresh`; line-row sign colors are unfloored; structure-badge matching is
  leg-order-sensitive (RR moved call-first → CUSTOM) and a fresh `g p` is
  identified too; a take-over moves the holder to a fresh untitled sheet; Yes/No
  buttons live in `geode_tile::confirm`, so market-data's upload confirm has them too.
- Payout currency: decide whether a cleared currency re-looks-up immediately or
  sticks (today it refills on any reference dataset change); undo across a
  reference-refresh fill can leave an old underlying under the refreshed currency;
  a line restored by undo/redo across a refresh stays blank until the next refresh
  or load. The mock pricer applies no quanto adjustment.

### Display checks

- Tile basics: header row, tree/package ground, stale vs fresh on three themes,
  entry field and its error line (the table jumps when it appears — reserving the
  line is the likely fix), editor and typeahead unclipped, `.` menu overlay, footer
  height, the text editor left open after an outside-grid click.
- Entry bar: height and border against the header, label `…` truncation, blank
  column-0 header, real-mouse double-click after the bar closes, hint line and list
  placement and contrast, a real Tab cycle, the "none" row covering row 0,
  `--demo` underlyings, a desk template edit reaching an open tile.
- Tree column: drawn connectors (half-pixel lines at 1x on Windows), chip and
  count on three themes, chip on a selected row, width per font size (230 px
  fixed), custom summary, gutter beside legs; leg tint on strong-stripe themes
  (Modus Vivendi, Fahrenheit, Bloomberg) and selected ≈ leg on Aurora Light,
  Default Light, Modus Operandi.
- Package rows: `/`-joined lists ellipsizing in narrow columns, package text in
  package paint, a double-click on a package cell.
- Views and scope: `:view barrier`; sign color on three themes; named color;
  negative package sum on a dark theme; `—` on a cross-underlying package in
  `--demo`; header drag + resize then edit; the Views dialog lists `pricer`; hidden
  chip, unscoped chip and tooltip, refusal notice, `· N of M legs`, hidden-landing
  footer.
- Grouping and sort: group-row grounds, sort arrow after a header click, the icon
  beside labels at Large font, rows reordering live, group-row double-click, find
  highlight color.
- Selection: tint over package ground, footer position-risk totals, held
  `shift+up` repaint and reprice, shift+click/drag, click inside the editor, the
  entry-bar shift press, light and dark.
- Row reorder grip: spacing beside chevrons/connectors at every depth, line
  numbers on/off, rem scales, hover show/hide, the drop line under tints, grab
  cursor, auto-scroll speed, a real escape during a live drag.
- Sheets: name hover/pressed + tooltip; sheet picker position, tick, `open` mark,
  empty row; the inline rename field in the 22 px header; the `⋯` Sheet section with
  Rename greyed while loading; holder footer text after a take-over; the package
  chip renaming after a leg edit; view-name control hover and menu placement.
- Persistence: type a line in `--demo`, quit within a second, relaunch, the line
  is there (repeat after `:e other`); `o`, line, enter, escape closes the entry; the
  joined save+load failure notice length.
- `STOPPED`/`SAVE_STOPPED` notices; line-number gutter; in-cell editor restyle and
  the expiry date field.

## Market data

### Open work

- No as-of override on market-data panels (the blotter has `:asof`); listed as an
  open point in the command-line locality design.
- When a data thread stops, a tile's in-flight upload stays "in flight" until
  restart. Clear it on its target's `ThreadStopped`.
- Egress minors: the column-axis type is inferred, not declared; one diagnostic
  per egress target; no transport timeout and an unbounded shutdown join, so a hung
  upload keeps `:upload` refused on that tile; a double clean-model build per
  confirmed echo; end-to-end loops do not fail fast; task-numbered comments remain
  in `demo.rs` and the market-data `tile.rs`. A pure same-day reorder of dividend
  rows is a recorded limitation.
- Selection minors: an upload `Ok` arriving mid-step leaves the draft `Editing`
  after escape (safe side); `bulk_step`'s member check uses the captured cell index
  and can refuse misleadingly after an automatic rebase (use `cursor_in_selection()`).
- Panels as config: `choices` are not checked against the kind vocabulary at load
  (refused at upload instead); end-to-end tests cannot attach the data bridge, so
  per-panel `stale_after`/reload is unexercised; `MODULE_KINDS` is hard-coded;
  `build_shell_services` carries a `too_many_arguments` allow.
- Windowed grid: a failed `MatrixIndex` rebuild refills from the live draft against
  the kept index (unreachable today; the real fix refuses the draft change); a
  one-row tile can paint its row blank.
- Dividend minors: pivot's numeric check reads row 0 only; the "row click never
  selects the cell beneath" popup test is vacuous on a 2-row fixture; one harness
  entry mutates `prepare` and `sync_editor` together; `rebase` leaves
  `base = Some` on an empty result; the splice's `known` includes Deleted labels;
  `shift+o` could fold `reanchor_row` into `rehang_followers`.
- Header minors *(unverified)*: `Draft::set`/`set_attr` duplicate base-guard
  logic; "a key part may not contain the storage separator" wording; `menu_pick`
  runs `sync_cursor` twice; no picker row-click mouse test; `picker_pick` skips the
  re-rank (rows are ranked; needs a comment); `toggle_menu`'s `close_editor` leaves
  the editor mirror painting until the next `sync_cursor`; `parse_cell`'s I64 arm
  for grid cells; `HeaderAttr.ty` has no cross-check against `geode_documents::cvi`;
  the `:` line paints over an open menu until a command is typed.
- Document plumbing *(unverified)*: `failed_topics` and the tracker's load map
  are never pruned (bounded by the topic space); `check_kind_against` compares
  column sets, not order, while writers are order-exact; every document publish
  emits a `PlanComplete` behind it; `DataService::document` stamps
  `Freshness.as_of` at submit while the select runs later; `painted_snapshot`'s
  base-first arm is unreachable and unguarded.
- CVI slice-value tag names (`SLICE_VALUES` in `geode-documents/src/cvi.rs`) are
  assumed until the real XSD is seen; demo params do not resemble the spec sample,
  and the per-key period is cadence × keys.

### Display checks

- Panel paint at 10,000 rows; the editor `Input` in the cursor cell; panel theme
  colors.
- Dense 22 px header: badge, bold underlying, 8 px dot, inline attribute strip with
  edited tint and cursor border; the anchored popup escaping the tile clip; the
  picker field with 12 rows max; menu row highlight and greyed reasons; `update
  HH:MM` / `no document yet` / stale wordings.
- A real press/release double-click opening the editor; caret position after a
  nudge; the segmented date field's three segments in the strip and inside an 84 px
  cell (`CELL_WIDTH` may clip the day segment).
- Dividend panel: no id column, first value column pinned under horizontal
  scroll; choice popup occlusion over grid rows; deleted-row legibility
  (`muted_foreground` unfloored, nine themes under 3:1); selected-cell highlight
  beside an open label editor; an index-sized schedule.
- Republish notices `republished at HH:MM — :rebase or :revert` and `… — your
  edits moved onto it`, beside an `update HH:MM` badge with the same time.
- Upload: confirm placement; `sent HH:MM` → `sent …, confirmed …` in `--demo`;
  `echo differs (N rows)` when the demo republishes over an upload; menu Upload
  with several targets answers a notice; whether `ctrl+c`/`cmd+c` under an armed
  confirm is swallowed by `Root`'s Copy binding.
- Selection: tint over edited/sent/deleted cells, footer extent, live repaint under
  held `shift+up`, shift+click/drag, click inside the editor, light and dark.
- Windowed grid: scrolling large market-data and pricer tiles, autofit widths
  following the screen, one-cell edit paint, pricer prices updating on ticks.

## Timeseries and charts

### Open work

- Each stats request still recomputes points over the whole range; a stats-only
  request is the next step (a known gap in `docs/current/performance.md`).
- A color slider step costs 15 ms at 100k buckets × 4 and 51 ms at 500k because a
  color change drops `geode-chart`'s cached paths; a color-only change that keeps
  paths would fix it.
- Decision: `[colors]` names starting with `#` are reserved so custom colors can be
  stored as `#rrggbb` (alternative: store Custom as `{ rgb = "#.." }`). Escape or
  click-out keeps slider changes already applied; revert-on-escape was offered.
- Tile minors: a scope keystroke while the first fetch is out sends one premature
  query; `for_chip` is derived per chip per frame; the picker observer rebuilds the
  catalogue per notify while open; `(have: )` renders empty with no fetch sources.
- Mouse: no test that a wheel under the open menu is inert, nor for the
  `on_mouse_up_out` route; the drag catcher covers the chart only, so a fast drag
  out of the tile pauses until re-entry; the pointer never opens the series list.
- Chart (`geode-chart`) follow-ups: the session resolution floor derives from the
  visible window (monthly buckets could relabel on zoom); `MAX_CANDIDATES` admits
  4,097; `at()` reads out-of-range micros as 1970; `MAX_PERCENTILES` = 8 and a short
  `percentile_labels` truncate silently; `slot.bins` max is recomputed per frame;
  the polyline overhangs horizontally (masked, not clamped); no two-element
  `ElementId` test.
- Series query follow-ups: `SeriesParams` derives `PartialEq` including an
  `Instant`; `log (s1)` with a space reads "unexpected token" rather than the
  arithmetic-only message; no harness entry for the bins window clause;
  `MAX_TOKENS` cost unmeasured; no boundary tests for `as_of == received_at` or the
  W1 bucket origin; `describe_span` prints "0h" for a sub-hour span;
  `Recorded::Delivered` is shared by Query and Series in the recording fixture; a
  per-broadcast `Vec` allocation in `deliver`.
- Series data tier: `demo_series::open_level` re-walks from epoch per generated day
  (quadratic); `sweep_pair` bases on `received_at`, so the oldest bars of a slow
  backfill can be swept while their coverage survives; no `AppendFn` seam on
  `append_one_series`; `service.rs::open` could extract a `spawn_fetch_source`.

### Display checks

- The `--demo` walk; a chip click on an unfocused tile; `tab` moving the chip
  cursor; the dimmed segment color.
- Pointer: wheel direction and feel, drag-pan one-to-one, divider band cursor and
  its 12 px grab height, grabbing cursor during a pan, the crosshair tooltip
  vanishing during a drag; density no longer freezing mid-pan.
- `⋯` button weight; the menu's tick column, hints, disabled rows, hover fill vs
  the lit row; swatch hover square; the empty state's ghost buttons.
- Range/frequency triggers (`1y ▾` / `1d ▾`) at rest/hover/open, menu placement
  under them, `over cap` width, the dates editor under its trigger, absolute-range
  trigger width, `Kbd` vs text in the trailing column.
- Expression-field completions: list overlay, lit vs hover, the no-series line,
  Tab then `cmd+z`, a refused Enter then a click.
- Color picker: trigger square in the chip (including a hidden chip), popover
  placement near the right edge, featured row with many names, live slider
  repaint, escape/click-out focus return, hex typing not firing tile keys.
- As-of: backfilled bars paint under a historical as-of.
- The `geode-chart` example (`cargo run -p geode-chart --example chart`) against
  the chart spec's display list, including content masks.

## Vol slice and link groups

### Open work

- A group scope change that keeps the underlying still refetches both documents.
  After an install, one vol round trip paints new chrome over old curves
  (documented limitation).
- Link groups: a group's scope is saved in the session (reversible ruling);
  duplicating a tile does not copy its membership; a mistyped `follow` (e.g. `fow`
  + Enter) unfollows.
- Decision: rulings not yet confirmed by the owner: density fill opacity 0.3, the
  demo's switch to a Hart double-precision normal CDF, the floor kink in the wide
  call wing. Expiry hues 21 positions apart sit about 8° apart.

### Display checks

- Link chooser rows; solid group chips including on Alduin and Solarized Light,
  letter polarity differing between chips on 15 themes; the `following A · SPX.Z`
  status segment; a restart restoring membership.
- Density fill contrast on light and dark themes; negative lobes after a long demo
  walk; curve x extent against the chain; expiry colors on light and dark.
- The `⋯` button, action menu placement, `y −2.75%…2.75%` ylim chip, out-of-domain
  clipping.

## Data path

### Open work

- Work continues for a closed window until quit (macOS keeps the process
  alive). The bridge's attach observers hold the shell and diagnostics
  entities strongly, and the shell holds its tiles, so module timers outlive
  the window: a pricer tile's periodic refresh and refusal retry keep
  submitting `price` requests, and the diagnostics ages tick and tile stale
  timers keep waking. Only the bridge's own lanes and the shell's reload poll
  check the window. Fix by breaking the bridge's retain cycles with weak
  handles, or by ending module timers on window closure (an
  `on_window_closed` hook, or quit on the last window).
- DuckDB `memory_limit` is never set (default 80% of RAM). The leading suspect for
  large footprints; the owner deferred it as a separate change. Use the
  `geode::memory` log lines and the Performance page Memory block to confirm.
- Live/archive sweeps are not scheduled for measure datasets or feed-published
  documents, so feed archives grow with every publish (documented in
  `docs/current/data-path.md`). A sweep also blocks publishes for its duration;
  decide when a sweeper is wired.
- Background collector: Parts 1–3 are built (recovery on subscribe;
  `geode-compose`; the `geode-collector` binary with lease, handoff, stamp and
  install). Decision: `[collector] memory_limit` is unset by default (512 MB
  aborted DuckDB on large CSV loads) until the R4 overnight measurement chooses
  a value. Owed to the owner: the display check of the `store: waiting for
  collector` segment, a real macOS `install` check, Windows Task Scheduler
  registration, and a Windows CI run of the handoff tests.
- Recovery parked items: a NOTIFY processed before the receiver notices a
  reconnect does not count toward report coverage; demo chain recovery restores
  one expiry per underlying. Demo run check pending.
- Expiry calendars (open decision from the 2026-09-25 review): a `geode-dates` leaf
  crate should own month-code and tenor resolution, with a per-underlying trading
  calendar (e.g. `NYS`, `LnS`) held in reference data, replacing the hardcoded third
  Friday (`third_friday` in the pricer). Still to decide: where the calendar is
  obtained at resolution time (the pricer builds requests on the UI thread), and
  whether `geode-dates` is called directly or through a request/outcome door.
- Containment: work queued to or claimed by a thread when it dies is never
  answered; `N refused` is read only when an event drains; the stale-check
  store-error arm (`Ok(Err)`) is untested.
- View strictness: about 45 characters of `executing '…': Invalid parameter name:`
  precede the refusal message (`StoreError`'s shared Display); no test carries a
  refusal past `DataService::query` into the painting tile; diagnostic paths are
  object-level, not column-level.
- Context columns: numeric context values come out as text in one reader pass and
  `None` in the others; context columns a JOIN declares are skipped; a `#mixed` name
  collision errors instead of skipping *(unverified)*.
- Document family minors *(unverified)*: a malformed `key`/`axes` on a measure
  dataset drops the dataset where a present one only warns;
  `ScopeSemantics::meet` has an `unreachable!`; `history_of` keeps an unused
  `dataset` parameter.
- The subscription shutdown test in `subscribe.rs` is timing-based (125 ms); adopt
  the deterministic form (unsubscribe, wait for the thread to exit on
  `Disconnected`) if it flakes.

## Reference data and classifications

### Open work

- Reference: the diagnostics Reference table snaps its scroll to the cursor on
  every `sources` counter bump (~2 s in demo); recompute only the chip on a
  sources-only change. The live-read race test in `store/reference.rs` spins forever
  if its writer thread panics; loop on `writer.is_finished()`.
- Reference rulings to confirm: at most one snapshot source per reference dataset;
  `priority` is ignored on snapshot sources; `table` on a non-snapshot source warns
  (the spec said refuse). Further consumers (risk enrichment, vendor-ticker
  resolution) need their own designs.
- Classifications: `references()` misses ad hoc lane chains, pricer views and view
  `sort`; `validate_name` should use the app-pinned schema; a blank `''` label in
  hand-written TOML reads as classified (decide the grid paint) *(unverified)*.
- Classifications CSV: padded sources are trimmed on import but quoted on export
  (no round trip); non-UTF-8 files are refused (no transcoding); applying an import
  at `y` runs on the UI thread (~28 ms at 100k changes); export text is built on the
  UI thread; a held import plan after a shell modal or `:` closes over the tile
  needs a refocus; `ask_import` closes the `.`/switcher menu.
- Classifications minors: a misplaced doc comment near `grid.rs:439`; `menu_at` is
  not cleared on a direct menu close; undo says "not saved" after a `KeptLastGood`
  refusal although the write reached disk; the bench omits `Prepared::build`; door
  removal leaves the sidecar entry; an editor row filtered out by a reload while open
  paints on its neighbour; no mutation entries for the keyboard-free prompt/confirm
  terms and offer hooks.

### Display checks

- Reference section in the diagnostics page.
- Classifications: header and layer badge, unclassified / `not in data` marks
  (clipping), editor popup anchoring and arrow keys, prompt field, confirm bar,
  right-click menu position, "loading values…" mark, empty and removed states.
- Classifications CSV: native open/save dialogs on macOS and Windows, confirm text
  wrapping, rejected-rows notice truncation, the held-plan notice and arm-on-focus.

## Diagnostics page

### Open work

- A page retained from one workspace and reopened over a pinned workspace keeps
  its first `FrameRef` (Data markers follow the first lane; the chip can sit at
  "catalog pending"). Fix: rebind on open.
- The Sources section lists no stopped data threads (the retired tile did; the
  status bar still does).
- The query-latency overflow bucket collapses everything over 100 ms to the max
  (a wider histogram was offered, not taken).
- Decision: deviations from the page spec — the Levels popover has no
  add-a-target row; `mod+1..9` workspace switches are context-free; most workspace,
  dock, stack and tile verbs are refused while a page is open.
- Path rows lose their tail to an end ellipsis; `text_ellipsis_middle` would keep
  the file name, if it reads badly.

### Display checks

- Rail active state, header chips, table density at 800/1200 px and numeric
  right-alignment, detail strip + Copy, histogram tint, Levels popover geometry and
  Escape inside it, toolbar wrap, Config two-panel height, back-control tooltip,
  Escape after a row click from the filter.
- Old sessions with a saved `diagnostics` tile paint a placeholder.
- Footer wrap with the longer key hints, and their tooltips.
- Fuzzy filter: accent legibility on selected/hover rows, ellipsis with bold runs,
  the Log's brief pending state.
- Memory block layout (macOS; the Windows memory code is type-checked only).
- With a fresh `--demo`: no degraded source and `Refused requests 0`; editing only
  `stale_after` applies live; a missing source directory shows `Degraded path '…'
  not found`.
- Data-service stopped segment: placement, wording, tone and tooltip beside the
  summary; `N refused`.

## Configuration and themes

### Open work

- `assets/themes/default.json` still carries key spellings that deserialize to
  nothing (`drag_border`, `link.foreground`, `progress_bar.background`,
  `slider.bar.background`, and per the earlier audit `chart_1`, `window_border`);
  the live names are `drag.border`, `link`, `progress.bar.background`,
  `slider.background`, `chart.1`, `window.border`.
- Code identifiers still say "colour" in ~80 Rust files (`geode_core::colour`,
  `NamedColours`, `view::Colour`, `objectdialog/colours.rs`); convert
  opportunistically when a file is touched (user-facing text is already "color").
- `[ui] first_day_of_week` is unbuilt (planned as its own slice).
- As-of: presets are fixed (configurable presets are a follow-up); `Clock::zone()`
  is unused; the `[time] sod`/`eod` bad-time arm is unpinned.
- A theme-contrast floor test could become permanent if a floor is agreed.

## Performance

### Open work

- No in-app render-duration instrument: the shell histogram records
  render-to-render intervals (`last_render_started` in `shell/render.rs`), which
  read key-repeat under a held key. Add `now - render_started` at the end of
  `ShellView::render` plus a "render p95" overlay row. Not urgent.
- Re-measure on an idle machine (all were recorded under load): `query_carried`
  depth 2 (32–41 ms at load ~40); `query_classification` at 5,000 values (51–65 ms;
  next lever is a keyed lookup join); context columns (`query_context` 35.0 ms vs
  20.5 ms without); pricer grid build after the tree-column and scope slices
  (1.66–2.15 ms); the dividend benches' anomalous readings.
- Painted-frame readings never taken: the per-keystroke scope text filter (recipe
  in `docs/perf.md`), the market-data panel at 10,000 rows, the diagnostics page
  p95 with the Log following.
- The `spx` text filter under as-of stayed ~150 ms because an extra scan filter
  flips DuckDB's decorrelation of the membership probes *(unverified)*.

## Tooling, tests and the mutation harness

### Open work

- Known mutation survivors on main, none from recent branches: "palette: tab is
  reclaimed inside the palette (spec §20.5)" (redundant binding); "dblclick: the
  browse click that opened a stage does not also open a field" (needs a real
  ancestor mouse-down focus GPUI test); "mdtable: a click while editing cancels the
  editor" (re-anchor to `pointer()`'s close); "service: a forget to a non-local
  dataset is queued"; "volslice: a refused read under unmoved versions is retried
  by a scope change"; "objectdialog: the tick's click does not stop propagation"
  reports `caught*`; plus the pricer entry above.
- Decision: should `SURVIVED` make the harness exit 1? If so, give it an explicit
  `# EXPECT SURVIVED` marker for the honest pricer entry.
- The scope-dialog retirement's `--changed` harness run (531 entries) was stopped
  and never completed.
- Move the unfiltered full harness run to a scheduled CI job.
- Flakes: geode-data
  `a_flooded_subscription_reports_its_drops_as_degraded_source_health` (seen
  failing ~1/3 under load, sometimes alone); geode-app
  `bridge::tests::catalog_refusal_and_failure_retry_without_new_events` under heavy
  load; `link::a_refused_door_logs_the_tile_and_the_reason`; a scheduler timing
  test; the 1 s window in `a_notify_inside_an_unanswered_window_keeps_recovery_ok`.
- Two tiling `should_panic` tests fail under `cargo test --release` *(unverified)*.
- Missing harness entries: the confirm bar buttons' redundant `flex_none`; the back
  button's `step_back` guard and registrations.
