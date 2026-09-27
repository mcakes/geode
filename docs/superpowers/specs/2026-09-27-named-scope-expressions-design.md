# Named scope expressions — design

Date: 2026-09-27. Status: approved in conversation. Awaiting review of this
written spec.

Mockups: https://claude.ai/artifact/7EPc8T4mo6YTimreG5P9tg. Option A (the
suggestion list) was chosen.

## Goal

A trader can save a complex compound expression once, under a name, and pair
it with different books. Each book-plus-expression combination is a saved
scope. Today the expression text is copied into every saved scope, so a change
to the expression means editing each scope. `TODO.md` asks for this under
"save expressions separately? (may want to mix and match categorical scopes
with expressions.)"

## Decisions (user rulings)

1. **Live reference, not a copy.** Every scope that uses a named expression
   refers to it by name. Editing the named expression changes every such
   scope.
2. **A list on the scope, not a token in the grammar.** A scope holds a list of
   named expressions, ANDed with its other parts. The expression grammar is
   unchanged, and named expressions cannot be combined with `or` or `not`.
3. **The frame scope holds references too.** Picking a named expression for
   the frame adds a reference, shown as a toolbar chip. Saving the frame as a
   scope keeps the reference.
4. **The expression dialog lists named expressions in its suggestion list**
   (mockup option A). It can also save the typed text as a named expression.
5. **Named expressions have their own object dialog**, Expressions, with
   browse, edit, new, copy, delete and revert.
6. **A missing or invalid reference is an error, never "match everything".**
7. **Not in scope:**
   - named expressions that refer to other named expressions;
   - named expressions in a tile's `:filter`;
   - renaming in place (copy, then delete).

## Model

### `expressions.toml`

```toml
[liquid_otc]
expression = "product in ('VAR', 'CORR', 'DISP') and notional > 5000000 and not counterparty like 'INTERNAL'"
```

- **Layering.** The document layers builtin → desk → user, and replaces whole
  objects by name (`atomic_depth` 1, like `scopes`). Runtime writes go only to
  the user layer through `geode_shell::config_write`.
- **Names.** Object names follow the saved-scope naming rules.
- **Reader.** `geode_core::named::NamedExpressions` holds an ordered map from
  name to `NamedExpr`:

  ```rust
  enum NamedExpr {
      Valid { text: String, expr: Expr },
      Invalid { text: String, reason: String },
  }
  ```

  - An expression that fails to parse is kept as `Invalid` with the parse
    error, so references can say "invalid" rather than "missing".
  - Unknown columns (checked against `ExprVocab`) produce a config diagnostic
    and leave the expression `Valid`. The query reports the column error, as
    it does for any expression today.
- **Pure logic.** The reader does no I/O beyond the existing `Config` entry
  points.

### `Scope.named`

- `Scope` gains `named: Vec<String>`. Order is insertion order, and duplicates
  are never stored.
- `is_empty` accounts for it.
- In `and_then`, the inner list follows the outer list, and a name already
  present is not added again.
- Saved scopes store it as `named = ["liquid_otc"]` in `scopes.toml`, read by
  `saved_scopes_from_doc` and written by `scope_to_table`.
- **Deferred validation.** The saved-scope reader does not validate names;
  they are checked at resolution. A scope is not dropped because a name is
  missing. Otherwise the scope would vanish from the palette instead of
  showing its error.
- `[frame]` in `session.toml` stores `named` the same way. On restore, a name
  that is missing is kept, and the chip and the tile show the error.

### Resolution

```rust
Scope::resolve(&self, named: &NamedExpressions) -> Result<Scope, String>
```

- It returns a scope with an empty `named`. Its expression is the named
  expressions, in list order, ANDed with each other and then with the existing
  expression.
- **Errors** name the first bad reference: `named expression 'x' is missing`,
  or `named expression 'x' is invalid: <reason>`.
- **Where it runs.** Resolution runs wherever a scope leaves the shell for the
  query layer:
  - `Frame::effective_scope`, which becomes `Result<Scope, String>`. The
    blotter tile shows an `Err` as its error line, the way it shows
    `view 'x' is not configured`.
  - The distinct-value requests from the dimension picker, the Scopes dialog's
    Values stage, and the expression suggestions. A resolution error there
    becomes that request's error text.
- **Safety net.** `geode-data`'s scope compiler refuses any scope with a
  non-empty `named`, with the explicit error `scope carries unresolved named
  expressions`. A missed call site therefore fails loudly and never widens
  totals.

### Reload

- A change to `expressions` rebuilds `NamedExpressions`.
- The frame holds its own copy, the way it holds `saved_scopes`, and replaces
  it on reload. That replacement bumps what tiles key their requery on, so an
  edited named expression reaches the screen without a scope edit.
- Named-expression diagnostics are logged and do not block the reload.

## Surfaces

### Scope bar

- Each name renders as a chip, `≡ name`, placed after the dimension chips and
  before the expression term chips.
- A missing or invalid name renders as a danger-toned chip,
  `≡ name · missing` or `≡ name · invalid`.
- Hovering a chip shows a tooltip with the full text, or the reason when the
  name is broken.
- **×** removes the name through `Frame::set_scope`, so the removal can be
  undone.
- Clicking the chip opens the Expressions dialog on that object. Every pointer
  action here has a keyboard route (the dialog and the palette).
- Chips use stable IDs derived from the name.

### Add-a-filter menu

The menu gains a third row, **Named expression…**. It opens the scope
expression dialog in Add mode, with its field empty, so the suggestion list
opens on named expressions and columns.

### Scope expression dialog (all modes)

- **Suggestion rows.** Wherever the caret wants a column (the `Column`
  position), named expressions are offered as rows before the columns.
  - Each row is marked `≡`, and its detail is a truncated preview of the text.
  - Invalid ones are shown in danger text.
  - Ranking matches against the name.
- **Staging.** Accepting a named row (tab or click) removes the typed token
  and stages the name as a chip above the field. A name already staged is not
  offered.
- **Staged chips.**
  - Whole mode opens with the frame's current names staged.
  - Term mode stages nothing and offers no named rows, because a term is one
    expression term.
  - Backspace in an empty field, or with the caret at offset 0, removes the
    last staged chip. Clicking a staged chip's × removes that chip.
- **Enter.**
  - In Whole mode, Enter sets the frame's `named` to the staged list and its
    expression to the typed text, as one `set_scope`.
  - In Add mode, Enter appends the staged names (skipping ones already
    present) and ANDs the typed text, as one `set_scope`.
  - An empty field with staged names applies only the names.
  - The existing refusals still apply to the typed text.
- **Save as named (mod+s).**
  - The field switches to a name entry, labelled "save this expression as a
    named expression". Enter saves and Escape goes back.
  - Saving refuses a name that is already taken, an empty expression, and text
    that fails the Enter checks.
  - On save, the text is written as a new object in the user layer of
    `expressions.toml`. The field is then cleared and the new name is staged,
    so applying the dialog makes the frame use the name.
  - Term mode offers no save.
- **Footer.** The footer names `tab`, `mod+s`, `enter` and `escape` through
  `shell::kbd`.

### Scopes object dialog

- A saved scope gets a **Named expressions** field, an ordered-list field
  shaped like Dimensions. Ticked names are the scope's `named`. The available
  (unticked) names are listed below them.
- Ticking and unticking work as they do for Dimensions (space, and click).
- A ticked name that is missing is shown in danger text and can be unticked.

### Expressions object dialog (new domain)

- It is a new `Domain::Expressions`, opened by a palette action
  (`config::expressions`).
- **Browse** lists the name, a preview of the text, and the layer.
- **Edit** has one text field, `expression`, with the scope-expression
  suggestions (columns, operators and values, with no named rows) and the
  Enter schema refusal.
- `n` creates, `c` copies, `d` deletes and `r` reverts, with the same verbs,
  confirmations and fork-on-edit rules as Scopes.
- **Delete confirmation** names every saved scope that references the object,
  and the frame if it does. For example: "delete liquid_otc? used by EQ liquid,
  RATES liquid and the current scope". It never rewrites those scopes, which
  then show the name as missing.

## Delivery

The change ships as two plans, each merged on its own:

1. **Core, storage and resolution.** This part alone delivers the requested
   workflow: saved scopes built as one book plus a named expression.
   - `NamedExpressions` and the `expressions.toml` reader;
   - `Scope.named` with composition and serialization;
   - `resolve` and the fallible `effective_scope`;
   - the resolve calls at the distinct-request sites;
   - the compiler's refusal;
   - session storage;
   - reload;
   - the Scopes-dialog field;
   - the Expressions dialog.
2. **Frame surfaces.** Scope-bar chips, the add-a-filter row, and the
   expression dialog's named rows, staging, Enter semantics and mod+s.

## Testing

- **Pure tests:**
  - `resolve`: ordering, the missing and invalid errors, and the empty list as
    identity;
  - `and_then` merging of `named`;
  - the round trips through `saved_scopes_from_doc` and `scope_to_table`;
  - the session round trip;
  - the `NamedExpressions` reader, covering valid entries, invalid entries and
    layering.
- **`geode-data`:** the compiler refuses a scope whose `named` is non-empty.
- **GPUI, through production routes:**
  - a tile with a missing name shows the resolution error, never unscoped
    rows;
  - editing a named expression through reload requeries the tile;
  - the Scopes field ticks a name;
  - the Expressions dialog creates, edits and deletes an object, and its
    delete confirmation lists the referencing scopes.
  - Part 2 adds:
    - tab stages a named row;
    - Enter in Whole and Add modes;
    - backspace removes a staged chip;
    - mod+s saves and stages;
    - the toolbar × and chip click;
    - the danger chip.
- **Mutation entries** cover:
  - the resolver's missing-name error;
  - the compiler's refusal;
  - the `and_then` duplicate skip;
  - the delete confirmation's reference list;
  - Part 2's staged Enter.

## Documentation

- `docs/current/configuration.md`: the `expressions` document, its layering,
  and its validation boundary.
- `docs/current/configuration-dialogs.md`: the Scopes field and the
  Expressions domain.
- `docs/current/shell.md` and `input-and-dialogs.md`: the scope parts, the
  chips, staging and mod+s (Part 2).
- The `geode-core` and `geode-shell` READMEs.

## Known limitations

- Named expressions combine only with `and`.
- Named expressions cannot refer to other named expressions.
- A rename breaks references. The delete confirmation lists the affected
  scopes.
- A tile's `:filter` cannot use a name.
- Palette `scope::<name>` actions for saved scopes are still registered at
  startup only.
