# Scope expression suggestions — design

Date: 2026-09-26. Status: approved in conversation, awaiting written-spec review.
Mockups: https://claude.ai/artifact/MvT9e9AaGPeFkXLGJypN41 (option A chosen).

## Goal

Guide the user through writing a scope expression instead of leaving them to
recall the column names, operators, and values, then fail at Enter or later at
query time. Today the frame expression dialog
(`geode-shell/src/shell/scope_expr_view.rs`) offers nothing while typing and
checks only syntax on Enter; an unknown column is accepted and fails in the
query. `TODO.md` lists "scope expression needs suggestions".

## Scope

In scope:

- The frame scope expression dialog in all three modes (Whole, Add, Term).
- The Scopes object dialog's `expression` text field
  (`shell/objectdialog/scopes.rs`).

Out of scope (follow-up): the blotter `:filter <expr>` command-line
completion. It has its own popup and key rules. It can later swap its
whitespace-split `word_at` for the core tokenizer defined here.

## Behaviour

### Presentation (option A)

The dialog body under the field is a live suggestion list that is always
shown. It updates on every keystroke and every caret move. It has three
parts:

- A **hint line** above the rows names what fits at the caret.
- The **rows** show the candidates. Each row highlights its matched
  characters and carries a right-aligned detail (a role and type, or a row
  count).
- A **warning line** below the rows carries the live schema warning. Only
  one warning is shown, the first by position.

An empty field lists the columns, so the dialog is never blank. The row window
uses the existing choice-list cap and scrolls with the highlight.

### What the list offers at the caret

| Caret position | Rows | Hint |
|---|---|---|
| Empty, or after `(`, `and`, `or`, `not` | Columns, then `not` and `(`. Detail: `dimension · text`, `measure · number`, `derived`, `key · text`, `attribute · text` | `column` |
| After a column | Operators valid for its type (see below) | `operator for <col>` |
| After an operator, or inside `in (` | Values (see below). Values already listed in the `in (…)` are omitted | `value for <col> · <type> · <n> values` |
| After a complete term | `and`, `or`, plus `)` when a paren is open, or `,` / `)` inside an `in` list | `and / or, or enter to apply` |

Operators by column:

| Column | Operators |
|---|---|
| text | `= != in like` |
| number, date, timestamp | `= != < <= > >= in` |
| bool | `= !=` |
| derived | `= != in` |

`<>` is accepted by the parser but not offered. It is a synonym for `!=`.

Values by column:

| Column | Values offered |
|---|---|
| Categorical text (a dimension's dictionary) | Distinct values from the data, with row counts. |
| Non-categorical text (keys, free text) | No list. The hint says `text in quotes, e.g. 'ABC'`. |
| Derived | The configured labels. No query. |
| Bool | `true`, `false` |
| Number, date, timestamp | No list. The hint says what to type, e.g. `a number, e.g. 1000` for a number. |

For date and timestamp columns, the hint is `a date in quotes, e.g. '2026-09-26'`,
and the operators are `= != < <= > >= in`. Verified against the pinned DuckDB:

- a text parameter compares correctly with both `date` and `timestamptz`
  columns;
- text that is not a date fails at query time with DuckDB's conversion
  error.

For text values, the rows appear in the order the distinct query returns them
(by value). While the user types, the fuzzy matcher re-ranks them.

When the typed token matches nothing, the hint line stays and the rows area
shows `no matches`. The user may type anything; suggestions never block input.

### Keys and pointer

Tab and shift-tab are already reclaimed from focus cycling in the modal
contexts (`dialog.rs:106-126`).

- **tab** inserts the highlighted row and re-reads the position. The list
  moves on (column → operators → values → connectives).
- **shift+tab** moves the highlight back one row.
- **↑ / ↓** and **ctrl+p / ctrl+n** move the highlight.
- **click on a row** inserts it. Focus stays in the field.
- **enter** applies the whole expression. It never inserts a suggestion.
- **escape** closes the dialog, unchanged from today.

The Scopes object dialog's `expression` field keeps its own open/commit keys.
There, **enter** commits the field, as other text fields do, and
**escape** reverts it. Tab, the arrows and ctrl+p/n behave as above while the
field is open.

The footer names tab, the arrows, enter, and escape through `shell::kbd`.

### Inserting

Accepting a row replaces the token under the caret, not the whole field.

| Accepted row | Text written |
|---|---|
| Column | `<name> ` |
| Operator | `<op> ` |
| `in` | `in (` |
| Text value | `'<value>'`, with embedded `'` doubled |
| Derived label or bool | Written the same way as a text value or bare literal, per the parser's rules. |
| Connective | `and ` / `or ` / `) ` / `, ` |

The write goes through the input's own range replacement, so **cmd+z undoes an
insertion**. The controller ignores the Change event its own write causes.

### Values: request and narrowing

Values are requested through the existing distinct query
(`ShellEvent::DistinctRequested`), once per column per dialog lifetime.

- The cache keeps a per-column state: `Loading`, `Ready`, or `Failed`.
- The hint shows `loading values…` while loading.
- On failure, the hint shows `values unavailable: <reason>` and typing stays
  free.

The request's scope is the scope the finished expression will be ANDed with,
never the in-progress text. Half-typed text and `or` make the typed prefix
unsound as a narrowing.

| Surface | Values narrowed by |
|---|---|
| Whole mode | The frame's dimension selections, text filter and as-of. The frame expression is excluded because the dialog replaces it. |
| Add mode | The frame's full current scope, including its expression. |
| Term mode | The frame's scope with the edited term removed from the expression. |
| Scopes dialog | The edited scope's own dimension selections and text filter, with the frame's as-of. |

A reply whose tag is not the latest for that column is dropped.

### Schema checking

- **Live warnings.** These appear while the user types:
  - an unknown column, with `did you mean <nearest>?` when a close name
    exists;
  - `like`, `<`, `<=`, `>` or `>=` on a derived dimension.

  A warning shows once the caret has left the offending word, so it never
  flags a word still being typed.
- **Syntax errors are silent while typing.** They are reported on Enter only,
  as today.
- **Enter refuses** a syntax error or a schema error with its message. The
  text stays in the field. This is a behaviour change: an unknown column used
  to be accepted and fail at query time. It now matches how saved scopes are
  validated. The Scopes dialog's `parse_text` gains the same refusal.

## Architecture

### Core: `geode-core/src/scope/complete.rs` (pure, no I/O)

- **`lex(text) -> Vec<Token>`** is a forgiving tokenizer that records each
  token's byte span. It accepts partial input: an unterminated string, a
  dangling operator, an open paren or `in (`. It uses the same character
  classes and keywords as `expr.rs`. The existing parser is unchanged.
- **`context_at(text, caret) -> Position`** returns the token range under or
  before the caret plus one of:

  ```text
  Column
  Operator { column }
  Value { column, listed: Vec<String> }
  Connective { open_parens, in_list }
  ```

  A caret inside a string, including an unterminated one, is `Value` with
  the token spanning the whole literal.
- **`ExprVocab`** holds per-column `{ name, role, ty, derived_labels }`. It is
  built from `SchemaSpec` over all datasets plus the derived dimensions, and
  includes measures and keys. When datasets disagree on a column, the vocab
  takes the first dataset's type in config order.
- **`check(text, &ExprVocab) -> Vec<Warning { span, message }>`** produces
  the live schema warnings. The did-you-mean uses a small edit distance and
  proposes only within distance 2.
- **One shared column check.** `Scope::validate` and the Enter check use the
  same column-existence and derived-operator test, so they cannot drift.

### Shell pure state: `geode-shell/src/exprcomplete.rs`

`ExprCompletion` holds:

- the current `Position` and the token range;
- the ranked candidates, via `listfilter::rank`;
- the highlight;
- the values cache, `BTreeMap<column, ValuesState>` plus a tag per column;
- the last `(text, caret)`, so refreshes can be skipped.

Operations:

- `refresh(text, caret, &vocab) -> Option<ValuesRequest>`. It returns a
  request when a value position names a column with no cache entry.
- `step(delta)`
- `accept(i) -> Write { range, text }`
- `deliver(column, tag, result)`, which drops stale tags.
- `rows()` and `hint()` for rendering.

`Write` and the accept rules take their shape from
`geode-timeseries/src/core/complete.rs` and use the scope grammar.

### Shell wiring

- **One controller serves both surfaces.**
  - It observes `dialog_input` for any notify, not only Change, so a caret
    move by arrow or click refreshes the list. It skips the refresh when
    `(text, caret)` is unchanged.
  - It writes an accepted row with `set_selected_range` plus `replace`,
    recording an echo so its own Change is skipped. This matches the
    timeseries expression field.
- **Values delivery.** The new reserved key is
  `EXPR_KEY = QueryKey(u64::MAX - 4)` in `shell/mod.rs`, with an arm in
  `deliver_distinct` that routes to whichever surface holds the completion.
  The tag comes from the shell-wide `next_picker_tag`.
- **Vocab lifetime.** The vocab is built at dialog open and rebuilt when
  config reloads while the dialog is open.
- **Renderer.** A shared function draws the hint line, the rows (through the
  `shell::listrow` row door, with the matched characters highlighted and a
  right-aligned detail), and the warning line. The rows use theme tokens and
  the rem scale only.
  - The frame dialog draws it under `dialog::filter_row`.
  - The Scopes object dialog draws it under the open `expression` field, in
    the place `choice_rows` occupies for choice fields.
- **Keys.**
  - `scope_expr_view::handle_key` claims tab, shift+tab, up/down and
    ctrl+p/n; enter behaves as before.
  - The object dialog claims the same keys while the `expression` field is
    open.
  - Pointer and keyboard insert through one function.

## Performance

A refresh costs one lex of the field (a short string) plus ranking of the
candidates.

- Columns and operators are small sets.
- Values can be large. The perf log puts the shared fuzzy matcher at about 400 µs per 2,000
  items. A key column such as `position_ref` can hold 1M distinct values.
  Ranking those would cost about 20 ms per keystroke, over the 8 ms pure-UI
  budget, and the distinct query itself would be large.

Value lists are therefore fetched only for categorical columns. Those are
dictionary-backed, so their size is bounded.

- Non-categorical text, keys included, gets a hint and no list. This follows
  the picker's own rule: a key has no dictionary to pick from.
- The list keeps at most 50 ranked rows. The hint still states the total
  value count.
- A Criterion bench ranks 20,000 values, and the result is recorded in the
  performance log.

## Testing

- **Core table tests.**
  - `context_at` at every caret offset of representative expressions,
    including partial input: `book`, `book =`, `book = 'E`, `book in ('A', `,
    `not (`, `a = 1 and `, `(a = 1 or b = 2) `.
  - Operator filtering by type.
  - Quoting and escaping on accept.
  - The did-you-mean distance bound.
- **Agreement test.** For every prefix of a corpus of valid expressions, the
  position `context_at` reports must be consistent with the parser's error
  or success on that prefix. For example, a prefix the parser rejects with
  "expected a value" must be at a `Value` position.
- **Shell pure tests** for `ExprCompletion`:
  - refresh skipping;
  - stale tag drop;
  - a request is emitted once per column;
  - the `Loading`, `Ready` and `Failed` hints.
- **GPUI tests through production routes.** Open via the palette action,
  then type keys:
  - tab inserts and the list moves on;
  - shift+tab and the arrows move the highlight;
  - a row click inserts and focus stays in the field;
  - a values reply delivered through `deliver_distinct` fills the rows;
  - a stale reply is ignored;
  - cmd+z undoes an insertion;
  - enter refuses an unknown column with its message;
  - Add and Term modes narrow the request scope as specified;
  - the Scopes dialog's open `expression` field shows and accepts
    suggestions.
- **Mutation entries** cover:
  - position classification after an operator;
  - the operator-by-type filter;
  - quote escaping;
  - the stale-tag check;
  - the Enter schema refusal;
  - the Add-mode narrowing.

## Documentation

- `docs/current/input-and-dialogs.md`: the Frame expression section describes
  suggestions, keys, narrowing, and the schema refusal.
- `docs/current/configuration-dialogs.md`: the Scopes `expression` field.
- `docs/current/configuration.md:107`: the claim "validated against the schema"
  now holds for the dialog too.
- The `geode-core` and `geode-shell` READMEs: the new modules.
- `geode-blotter/src/tile.rs:456-470`: fix the stale "validated in
  `scope_expr_view`" comment so it is true.

## Known limitations

- There is no date literal in the grammar. A date column gets a hint and no
  list, and a malformed date fails at query time.
- Non-categorical text columns, such as keys, get no value list.
- Values are not narrowed by the in-progress expression.
- `:filter` completion is unchanged until the follow-up.
