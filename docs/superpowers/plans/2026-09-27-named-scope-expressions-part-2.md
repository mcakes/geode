# Named Scope Expressions: Part 2 (Frame Surfaces) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** In the frame's scope expression dialog you can pick named expressions from the suggestion list, see them staged as chips, and apply them together with typed text in one step. You can also save the typed text as a new named expression with `mod+s`. The `+` menu gains a "Named expression…" row. Clicking a scope-bar named chip opens that expression in the Expressions dialog.

**Architecture:**
- **Suggestion rows.** `ExprCompletion` gains opt-in "named offers" and a row kind. A named row stages its name instead of inserting text: `accept` returns `Accept::Stage`, which erases the typed token.
- **Opt-in.** Only the frame dialog sets offers. The Scopes and Expressions object dialogs never do, so their lists are unchanged.
- **Staging.** `ScopeExprState` holds the staged names. Enter applies names and text as one `set_scope` (one undo step).
- **Saving.** `mod+s` switches the dialog into a naming sub-state on the shared input. On Enter it writes through the object dialog's existing write path (`apply::queue_batch`, exposed as `queue_object`), so memory updates immediately. It then refreshes the frame's named expressions so the new name resolves at once.

**Tech Stack:** Rust, GPUI and gpui-component 0.6.2. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-27-named-scope-expressions-design.md`, "Frame surfaces" and "Delivery 2". Part 1 (merged at 48d85f1f) already paints `≡ name` chips on the scope bar with × and a tooltip.

## Global Constraints

- **Branch and commits.** Work on the worktree branch `worktree-named-expressions-2`. Every commit ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **Where named rows appear.**
  - Named rows are offered **only at the `Column` position**, before the columns.
  - Offers **exclude names already staged**.
  - Named rows appear **only in the frame dialog's Whole and Add modes**, never in Term mode. The Scopes and Expressions object dialogs never show them.
  - A named row is ranked by its **name**. Its detail is a truncated preview of its text. A broken row (missing or invalid) paints its detail in danger text.
  - A named row has its **own selector**, `scope-expr-named-row-{name}`, so a name equal to a column name cannot collide.
- **Accepting a named row** (tab or click) erases the typed token and appends the name to the staged list. It never inserts text.
- **Staged chips.**
  - Staged chips paint above the field, as `≡ name` with a ×.
  - **Backspace** with the caret at offset 0 removes the last staged chip. Clicking a staged chip's × removes that chip.
  - Whole mode opens with the frame's current names staged. Add mode opens with none.
- **Enter.**
  - Whole mode sets `named = staged` and `expression = parsed`, both in **one** `set_scope`.
  - Add mode appends the staged names not already present and ANDs the parsed text, in **one** `set_scope`.
  - An empty field with staged names applies only the names.
  - An empty field with nothing staged keeps today's behaviour: Whole clears the expression (and, now, the names). Add closes without a change.
  - The existing text refusals still apply.
- **Values narrowing.** In Whole mode, values are narrowed by the frame scope with its expression removed and `named = staged`. In Add mode they are narrowed by the whole frame scope plus the staged names. This keeps the Part 1 rule: never narrow by in-progress text.
- **`mod+s`** means the configured mod alias (`services.mod_alias`) plus `s`.
  - It is offered in Whole and Add modes. In Term mode it shows the notice `save a named expression from the whole or add dialog`.
  - It switches to a name entry labelled `Save this expression as a named expression · name`. Enter saves and Escape goes back, restoring the text.
  - Saving is refused for:
    - an invalid name (`check_object_name`);
    - a name that is taken or reserved (`Domain::Expressions.name_taken` over the pending config);
    - empty text;
    - text that fails the Enter checks.
    - Each refusal shows inline, and the naming stays open.
  - **On save:**
    1. Write `[name] expression = "<text>"` to the user layer of `expressions.toml` through the object dialog's write path.
    2. Refresh the frame's named expressions from the pending config.
    3. Clear the field and stage the name.
- **Scope-bar chip click.** Clicking a scope-bar named chip's body opens the Expressions dialog on that object, in its edit stage. A **missing** name opens Browse with the notice `'<name>' is not defined`. An invalid name opens its edit stage.
- **`+` menu.** A third row, **Named expression…**, dispatches a new action `frame::add_named_expression` ("Add named expression…"). The action opens the dialog in Add mode, so the list opens with named rows first.
- **Keyboard route for removing one name.** Open `frame::scope_expression` (Whole mode), press backspace at offset 0 (removes the last chip), then Enter. Update the documented limitation in `docs/current/shell.md` to describe this route.
- **General rules.**
  - Never mutate state or do I/O during render. Use theme tokens and existing chip tones only. Element IDs come from the name.
  - Tests go through production routes: keys, clicks and actions.
  - Code comments state the local invariant and the failure it prevents. They never cite task numbers.
  - Docs describe current behaviour only.

## Review Focus

1. **A name that equals a column name.** Both rows show, each has its own selector, and accepting one never accepts the other. Covered in Task 1.
2. **Enter in Whole mode with the staged list emptied by backspace.** The frame's names are cleared, and one undo restores both the names and the expression. Covered in Task 2.
3. **`mod+s` right after typing, then Enter.** The staged new name resolves immediately; the tile never flashes "missing". Covered in Task 3.
4. **Suggestions run on name text while naming.** They must not: the completion is off while naming. Covered in Task 3.
5. **Clicking a missing chip.** It never opens an edit stage on a phantom, empty draft. Covered in Task 4.

---

### Task 1: Named rows in `ExprCompletion` (pure) and their accept path

**Files:**
- `crates/geode-shell/src/exprcomplete.rs`
- `crates/geode-shell/src/shell/expr_suggest.rs` (the `accept` / `accept_label` / `handle_key` tab branch; `render` gains the named-row paint and selector)
- Tests: unit tests in `exprcomplete.rs`

**Interfaces (produced):**
- `pub struct NamedOffer { pub name: String, pub preview: String, pub broken: bool }`
- `pub enum RowKind { Insert, Named { broken: bool } }`, and `Row` gains `pub kind: RowKind`
- `ExprCompletion::set_named_offers(&mut self, offers: Vec<NamedOffer>, vocab: &ExprVocab)`. It replaces the offers and rebuilds. The default is empty, which means no named rows.
- `pub enum Accept { Write(Write), Stage { name: String, erase: Write } }`. Here `erase` is the token range with empty text.
- `ExprCompletion::accept(&self, i) -> Option<Accept>`. Existing callers map `Accept::Write`.
- In `expr_suggest.rs`, `accept_label` becomes `accept_row(view, kind_is_named: bool, label: &str, window, cx)`. It resolves the row by kind and label together.

- [ ] **Step 1: Failing unit tests**
  - With offers `liq` (valid) and `gone` (broken), the rows at an empty field start with the named rows, then the columns, `not` and `(`.
  - The named rows have `kind == Named`. `gone` has `broken: true`.
  - The detail is the preview, truncated to 40 characters with `…`.
  - Typing `li` ranks `liq` first.
  - Named rows never appear at `Operator`, `Value` or `Connective` positions.
  - With no offers, the rows are exactly as before (regression).
  - `accept` on a named row returns `Stage { name: "liq", erase }` whose `erase.apply("li")` returns `("", 0)`.
  - `accept` on a column still returns `Write`.
  - An offer whose name equals a column name (`book`) yields two rows. Accepting each by `(kind, label)` picks the right one.
- [ ] **Step 2: Run** `cargo test -p geode-shell --lib exprcomplete`. Expected: FAIL.
- [ ] **Step 3: Implement**
  - Candidates at `Position::Column`: named rows first (`label = name`, `insert = String::new()`, `detail = preview`), then the existing rows.
  - `accept` branches on the row kind.
  - In `expr_suggest.rs`:
    - `accept` handles `Accept::Write` exactly as today.
    - `accept` handles `Accept::Stage` by writing the erase as the same undoable range replace, then calling a new `stage_named(view, name, cx)`. Task 2 implements `stage_named`; this task adds it as a no-op stub that returns without effect.
    - `render` paints named rows with a leading `≡` (muted), selector `scope-expr-named-row-{name}`, and danger text on the detail when broken (`chip::chip_paint(theme, chip::Tone::DangerText).text`).
    - The row click passes the kind and the label.
- [ ] **Step 4: Run** `cargo test -p geode-shell --lib`, fmt and clippy.
- [ ] **Step 5: Mutation entries and commit**
  - Add one entry that stops excluding named rows from non-`Column` positions. Filter to that unit test.
  - Add one entry that makes named-row accept return `Write`. Filter to the Stage test.
  - Commit with `feat(shell): named rows in the expression suggestion list`.

### Task 2: Staging in the frame dialog, and Enter semantics

**Files:**
- `crates/geode-shell/src/shell/scope_expr_view.rs`
- `crates/geode-shell/src/shell/expr_suggest.rs` (`stage_named`, `values_scope`)
- `crates/geode-shell/src/frame.rs`, only if a helper reads better than an inline clone
- Tests: `scope_expr_view.rs` unit tests and `crates/geode-shell/src/shell/tests/scope_expr.rs`

**Behaviour:** as in Global Constraints (staging, Enter, values narrowing).
- `ScopeExprState` gains `pub staged: Vec<String>`.
- `open` seeds `staged` (Whole: `frame.scope().named`; Add: empty). It then calls `completion.set_named_offers(offers)`, where offers are built from `frame.named_expressions()`: every name not staged, with its preview and `broken` state. Term mode passes no offers.
- `stage_named` appends the name if it is absent, recomputes the offers, and rebuilds. Chip × and backspace removal do the same.
- `apply(frame, mode, text, staged, vocab)`: implement Whole and Add as one `set_scope` on a cloned scope. Term is unchanged.
- `request_scope(mode, current, staged)` implements the new values rule.
- `build`:
  - Paints the staged chip row above `filter_row`, only when `staged` is non-empty. Each chip is `≡ name` with a × (use the existing `chip` door and `chip_states`/`broken_states`). Selectors are `scope-expr-staged-{name}` and `scope-expr-staged-close-{name}`. A broken staged name uses the broken tone.
  - Updates the hints to name `backspace` for "remove chip" when chips are staged.
- `handle_key` claims backspace only when `staged` is non-empty **and** `dialog_input.cursor() == 0` **and** the selection is empty. Otherwise backspace stays the field's.

- [ ] **Step 1: Failing tests**
  - Pure unit tests (`scope_expr_view.rs`):
    - Whole apply sets both names and expression, and a single `undo_scope` restores the prior scope.
    - Add apply appends only the missing names.
    - Empty text with staged names applies the names.
    - Empty text with nothing staged keeps today's results.
    - `request_scope` Whole uses `staged`.
  - GPUI tests (`services_with_schema` plus `[liq] expression = "npv > 0"`, `[hedges] expression = "npv < 0"`):
    1. `frame::add_expression`, type `li`, tab: the field is empty and the chip `scope-expr-staged-liq` paints; Enter makes the frame's `named == ["liq"]`.
    2. The frame has `named = ["liq", "hedges"]` and expression `npv > 5`. `frame::scope_expression` opens with both chips. Move the caret to offset 0 (`home`) and press backspace: `hedges` is removed. Enter gives `named == ["liq"]` with the expression unchanged. `frame::scope_undo` restores both names.
    3. The named rows exclude already-staged names (after staging `liq`, `scope-expr-named-row-liq` is absent).
    4. A term chip click (Term mode) shows no named rows.
    5. Clicking a staged chip's × removes it.
- [ ] **Step 2: Run to verify failure.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p geode-shell --lib`, fmt and clippy.
- [ ] **Step 5: Mutation entries and commit**
  - Add entries for:
    - Whole apply ignoring `staged` (filter to GPUI test 2);
    - backspace removal not firing (test 2);
    - offers not excluding staged names (test 3).
  - Commit with `feat(shell): stage named expressions in the scope expression dialog`.

### Task 3: `mod+s` saves the typed text as a named expression

**Files:**
- `crates/geode-shell/src/shell/scope_expr_view.rs`
- `crates/geode-shell/src/shell/objectdialog/apply.rs` (expose `pub(crate) fn queue_object(shell, doc: &str, name: &str, value: toml::Value, cx) -> Result<(), String>`, wrapping `queue_batch` with zero delay; it refuses with `commit_create`'s wording when `user_dir` is `None`)
- `crates/geode-shell/src/shell/expr_suggest.rs` (`completion_mut` returns `None` while naming)
- Tests: `crates/geode-shell/src/shell/tests/scope_expr.rs`, using `dialog_test_shell_in_dir` for the file write

**Behaviour:** as in Global Constraints (`mod+s`).
- `ScopeExprState` gains `naming: Option<String>`, holding the stashed expression text.
- **Entering naming:** stash the text, then `set_value("")`, then paint `dialog::name_row(&input, "Save this expression as a named expression · name", cx)` in place of the filter row. Staged chips stay visible.
- **Enter while naming** validates in this order:
  1. The name: `check_object_name`, then `Domain::Expressions.name_taken(config_with_pending)`. Refusals are `name: <reason>` or `'<name>' already exists`.
  2. The stashed text: it must be non-empty, then pass `commit_text(text, vocab)`.

  On success:
  - `queue_object(shell, EXPRESSIONS_DOC, name, {expression = text})`;
  - `frame.replace_named_expressions(rebuild_named_expressions(&config_with_pending(shell)))`;
  - `naming = None`, the field empty, `staged.push(name)`, and the offers rebuilt.
- **Escape while naming** restores the stashed text and leaves naming. It is claimed, so the modal stays open.
- **The footer** shows the `mod+s` chip resolved through `services.mod_alias`. It is built in `build` via `kbd::chip`/`kbd::hint` from the parsed keystroke, because `Hint::Key` parses with no modifiers.

- [ ] **Step 1: Failing GPUI tests**
  1. Type `npv > 0`, press `mod+s` (the test config's alias), type `liq2`, press Enter. The user `expressions.toml` holds `[liq2] expression = "npv > 0"`. The field is empty and `scope-expr-staged-liq2` paints. Enter then sets the frame's `named == ["liq2"]`. The blotter, or `frame.effective_scope`, resolves without a "missing" error.
  2. `mod+s` with an empty field refuses inline, and no file is written.
  3. Naming `liq` when it already exists refuses with `'liq' already exists`.
  4. Escape while naming restores `npv > 0` and the modal stays open.
  5. While naming, typing `bo` paints no `scope-expr-row-book` (suggestions are off).
  6. In Term mode, `mod+s` shows the Term notice and nothing changes.
- [ ] **Step 2: Run to verify failure.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p geode-shell --lib`, fmt and clippy.
- [ ] **Step 5: Mutation entries and commit**
  - Add entries for:
    - skipping the immediate `replace_named_expressions` (filter to test 1's no-missing assertion);
    - the name-taken check (test 3);
    - suggestions staying on while naming (test 5).
  - Commit with `feat(shell): save the typed expression as a named expression`.

### Task 4: Scope-bar chip click and the `+` menu row

**Files:**
- `crates/geode-shell/src/shell/toolbar.rs` (the named chip body gets `pointer_states` and `on_mouse_down` through a new `on_named_open: impl Fn(&str, &mut Window, &mut App) + Clone + 'static` parameter, plus the tooltip line `click: edit this expression`)
- `crates/geode-shell/src/shell/render.rs` (wire `on_named_open`)
- `crates/geode-shell/src/shell/objectdialog/render.rs` (new `pub(in crate::shell) fn open_object(shell, domain, name, window, cx)`: open, then `enter_edit_stage`, then `sync_dialog_text`; if the object is not defined in the pending config, stay in Browse with the notice `'<name>' is not defined`)
- `crates/geode-shell/src/shell/addfilter.rs` (a third `Entry::Named`, titled "Named expression…", action `frame::add_named_expression`, key `named`; update the menu's unit tests)
- `crates/geode-shell/src/defaults.rs` (register `frame::add_named_expression`, "Add named expression…", beside `frame::add_expression`)
- `crates/geode-shell/src/shell/input.rs` (dispatch it to `scope_expr_view::open(self, Mode::Add, ..)`)
- Tests:
  - `crates/geode-shell/src/shell/tests/scopebar.rs` (`services_with_a_named_scope`, `click_selector`)
  - `crates/geode-shell/src/shell/tests/addfilter.rs`

- [ ] **Step 1: Failing GPUI tests**
  1. Clicking the body of `scope-named-chip-liq` opens the Expressions dialog in its edit stage on `liq`: `dialog_state(domain) == Expressions`, and the edit stage's object is `liq`.
  2. Clicking the body of a missing chip (`gone`) opens Browse with the notice `'gone' is not defined`, and no edit stage.
  3. Clicking the chip's × still only removes the name. It opens nothing, because the × occludes the body.
  4. The `+` menu lists three rows. Choosing "Named expression…" opens the scope expression dialog in Add mode, with `scope-expr-named-row-liq` painted.
  5. `frame::add_named_expression` dispatched from the palette does the same.
- [ ] **Step 2: Run to verify failure.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p geode-shell --lib`, fmt and clippy.
- [ ] **Step 5: Mutation entries and commit**
  - Add entries for:
    - the chip body not opening (test 1);
    - the missing-name branch opening an edit stage anyway (test 2).
  - Commit with `feat(shell): named chips open their expression; + menu row`.

### Task 5: Documentation and full verification

**Update these docs:**
- `docs/current/input-and-dialogs.md`
  - "Frame expression" (`~311-346`): the mode table, the Add row naming both `+` rows, Enter semantics with staged names, and `mod+s`.
  - "Suggestions" (`~348-423`): named rows at the Column position, staging, backspace, and that the object dialogs show no named rows.
- `docs/current/shell.md`
  - The named-chip paragraph (`~246-258`): the body click opens the Expressions dialog, and a missing name opens Browse with a notice.
  - Replace the "no key removes one name" limitation with the keyboard route: Whole dialog, backspace, Enter.
  - The `+` menu paragraph (`~260-265`): three rows.
- `docs/current/configuration-dialogs.md`: the frame dialog's `mod+s` is a second way to create a named expression, and it writes the user layer.
- `crates/geode-shell/README.md`: `expr_suggest` (named rows and staging) and `objectdialog::open_object`.

- [ ] **Step 1:** Write the docs. Verify every sentence against the merged code.
- [ ] **Step 2: Full verification.** Run each command in the foreground:
  - `cargo fmt --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace`
  - `cargo check -p geode-shell --features test-support --all-targets`
  - `zsh scripts/mutation-check.sh --anchors-only`
  - `zsh scripts/mutation-check.sh "named expr"`
- [ ] **Step 3:** Commit with `docs: named expressions in the frame dialog`.
- [ ] **Step 4:** List the display checks:
  - named rows and their danger detail;
  - the staged chip row above the field;
  - the naming row;
  - the `mod+s` footer chip under the configured alias;
  - the chip body hover and click.
