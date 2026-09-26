# Launch context and the empty-panel prompt

Status: approved in conversation 2026-09-26; spec awaiting review.

## 1. Why

A market-data panel shows nothing until it has an underlying. Today a new
panel opens empty, and the trader has to press the `load_underlying` key
before doing anything else. When the underlying is already on screen in
another tile (a blotter row, a pricer line), the trader also has to type it
again.

Rulings (user, 2026-09-26):

1. A panel opened without an underlying asks for one straight away.
2. A panel can be opened with its underlying already set, from the blotter
   and the pricer. From the palette or the tile picker, ruling 1 covers
   it: picking a kind opens the panel with the underlying picker up, so
   there is no separate "kind + underlying" action.
3. A launch from another tile always **splits** beside the source. It
   never switches an existing panel to the new underlying or stacks onto
   the source.
4. One key in each source tile opens a picker of the panel kinds that
   accept the context. There are no per-kind keys.

## 2. Vocabulary: `LaunchContext`

`geode-core` gains a module `launch`:

```rust
/// What a source tile knows at its cursor that another module may open on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchContext {
    pub underlying: Option<String>,
}

/// A field of `LaunchContext` a target module accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextField {
    Underlying,
}

impl LaunchContext {
    pub fn is_empty(&self) -> bool;
    pub fn has(&self, field: ContextField) -> bool;
}
```

- The context is typed rather than an opaque table, so a source module and
  a target module agree on the words without depending on each other.
  `geode-core` is the shared vocabulary both already depend on.
- **One field for now.** A new field means a new `ContextField` variant.
  Adding fields later (book, expiry, series identity) is additive.
- **Identity.** The underlying is the desk's underlying identifier, the
  string the blotter's `underlying_ref` column and the pricer's instrument
  carry. Market-data keys use the same identifier: in the demo, the risk
  and document generators share one `UNDERLYINGS` list. There is no
  mapping step. A key that isn't in the catalog behaves exactly as
  `:underlying <value>` does today.

## 3. Shell contract

### 3.1 Source side: `TileContent::launch_context`

```rust
/// The context at this tile's cursor, for `tile::open_with`. Empty by
/// default and whenever the cursor names no single value.
fn launch_context(&self, _cx: &App) -> LaunchContext {
    LaunchContext::default()
}
```

- **Pull, not push.** The shell asks the focused tile when the action
  runs. Modules get no handle into the shell, and no new route from module
  to shell is added.
- **Purity.** The answer is a read of state the tile already holds. It does
  no I/O and allocates only the returned string.

### 3.2 Target side: `ModuleFactory::accepts` and `launch_state`

```rust
/// Context fields this kind can open on. Empty (the default) keeps the
/// kind out of `tile::open_with`'s list.
fn accepts(&self) -> &'static [ContextField] {
    &[]
}
/// Translate a context into the table `create` reads as its restored
/// record. `None` (the default) creates the tile as a plain add would.
fn launch_state(&self, _ctx: &LaunchContext) -> Option<toml::Table> {
    None
}
```

- **Why a table.** `add_tile` and `create` already carry a state table, and
  the market-data panel already reads `underlying` from it. The factory
  owns the translation, so the shell never learns the module's state
  format, and `add_tile`'s signature does not change.

### 3.3 The `tile::open_with` action

A shell action, registered as "Open with context…" in the `Tile` category,
handled in `ShellView`'s action dispatch beside `tile::add`:

1. Read the focused occupant's `launch_context`. With no focused occupant,
   or with a placeholder focused, the context is empty.
2. **Empty context.** Open the tile-kind picker exactly as `tile::add`
   does.
3. **Otherwise.** Open the shared choice dialog (`shell::choicedialog`)
   with a new target, `Pick::KindWith(kind)`:
   - The title reads `Open {underlying} in…`.
   - The rows are the kinds whose factory `accepts()` covers every set
     field of the context, in roster order.
   - With no such kind, the dialog does not open. The shell shows the
     notice `no module opens on {underlying}`.
4. **Commit.** Close the modal, then call `add_tile(kind,
   AddPlacement::Split(None), factory.launch_state(&ctx))`. The context was
   captured when the dialog opened, so moving the source tile's cursor
   while the dialog is open doesn't change it.
5. Focus moves to the new tile, as with every `add_tile` split.

Placement: `Split(None)` follows the add-direction setting, as the tile
picker does (ruling 3).

### 3.4 `TileContent::launched`

```rust
/// Called once, after an occupant created by `add_tile` (not a session
/// restore) has been shown and focused. Default: nothing.
fn launched(&self, _window: &mut Window, _cx: &mut App) {}
```

- `ensure_occupants` records which occupants came from `pending_tiles`.
  After their first `set_visible(true)`, if the tile is the focused one, the
  shell schedules `launched` with `cx.defer_in(window, …)`. That keeps it
  out of render, and focus is settled by the time it runs.
- **Restore never calls it.** A restored session does not take focus away
  from anything, even when several panels were saved empty.
- **Duplicate does call it**, because it goes through `add_tile`. A
  duplicated panel that already has an underlying ignores the call.
- **Why a hook rather than `restored == None`.** The shell knows exactly
  which tiles are fresh, and it has the window. The module only decides
  what "fresh and empty" means for itself.

## 4. Market data

- **Factories.** `CVI` and `DIVIDEND` return `&[ContextField::Underlying]`
  from `accepts`. `launch_state` returns `{ underlying = [<value>] }`,
  which is the one-element display key `MarketDataTile::new` already reads.
- **Auto-prompt.** `launched`: if the panel has no key, call `open_picker`.
  Escape closes the picker and leaves the empty panel as it is today; the
  prompt is not repeated.
- **With a context.** The panel is created on its key and requests the
  document as a restored panel would. The picker does not open.

## 5. Sources

### 5.1 Blotter

- `launch_context` returns the cursor row's `underlying_ref` value when
  that column is one of the row's grouping levels, using `path_of(snapshot,
  plan, cursor)`. Otherwise it returns empty:
  - for a row above that level (for example a `lhu` subtotal),
  - for a grouping without `underlying_ref`,
  - for the grand total,
  - for a NULL value.
- A NULL underlying is empty, not the string `"NULL"`. A panel opened on a
  made-up key would be a plausible wrong answer.
- In visual mode, the value is taken from the cursor row, not the
  selection.

### 5.2 Pricer

`launch_context` returns empty whenever the cursor row has no single
underlying:

- **A line or leg row** returns its instrument's `underlying()`.
- **A package row** returns the underlying its legs share when they share
  exactly one. With several underlyings, or none, it returns empty.
- **An empty sheet, or a row with no parsed instrument,** returns empty.

## 6. Keys

- The blotter fragment binds `"g m" = "tile::open_with"` under
  `blotter && mode == normal`.
- The pricer fragment binds `"g m" = "tile::open_with"` under its normal
  context.
- `g` already starts `g g`, `g p` and `g u` in these contexts. `g m` is
  unbound in both.
- **Fragment rules.** A fragment's predicate must begin with the module's
  own context. The action id is not restricted, so binding a shell action
  from a module fragment is allowed. `check_fragment` is unchanged.
- **Palette.** "Open with context…" runs against the tile that was focused
  when the palette opened. This is the same focus the palette already
  restores on dispatch.
- **Out of scope: the pricer `.` menu row.** Menu rows dispatch pricer
  actions only, so a menu row would need a new pointer route to the shell.
  The keyboard route and the palette cover the feature.

## 7. Tests

At the lowest layer that proves each fact, through production routes:

- **`geode-core`.** `LaunchContext::is_empty` and `has`.
- **Blotter, pure.** Cursor on an underlying row, a subtotal above it, a
  grouping without the column, the grand total, and a NULL value.
- **Pricer, pure.**
  - A line row.
  - A leg row.
  - A package with one shared underlying.
  - A package with two underlyings.
  - An empty sheet.
- **Shell, GPUI.** Using `RecordingFactory` extended with `accepts` and
  `launch_state`:
  - Typing `g m` on a source whose context is set opens the dialog titled
    `Open SPX in…`, listing only the accepting kinds.
  - Enter creates a split whose `create` received the translated table.
  - With an empty context, `g m` opens the plain tile-kind picker.
  - With no accepting kind, `g m` shows the notice and no dialog.
  - Moving the source cursor after the dialog opens doesn't change the
    committed context.
- **Shell, GPUI: `launched`.**
  - It is called once for an add.
  - It is called for a duplicate.
  - It is not called for a session restore.
  - It is not called for an add that did not end up focused.
- **Market data, GPUI.**
  - A panel created through `add_tile` with no state has the picker open,
    and its input focused.
  - A panel created with `{ underlying = ["SPX"] }` shows SPX and has no
    picker.
  - A restored empty panel has no picker.
- **Mutation-harness entries.** One for each of these contracts:
  - The accepts filter.
  - The captured context.
  - `launched` skipped on restore.
  - NULL underlying reads as empty.
  - The package single-underlying rule.
  - The auto-prompt gate on "no key".

## 8. Documentation

In the same change:

- `docs/current/shell.md`: launch context, `tile::open_with`, `launched`.
- `docs/current/features.md`: the market-data prompt, and the blotter and
  pricer sources.
- `docs/current/keymaps.md`: `g m`.
- The `geode-shell`, `geode-marketdata`, `geode-blotter` and `geode-pricer`
  READMEs.

## 9. Not in scope

- Switching an existing panel to a new underlying (ruling 3 chose split).
- Context fields beyond `underlying`.
- Sources other than the blotter and the pricer (timeseries, diagnostics).
- A pointer route from module menus to shell actions.
