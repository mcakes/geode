# Edit a tile's column in Views or Schema

Status: approved in conversation 2026-09-27; spec awaiting review.

## 1. Why

To change how a blotter column looks (label, width, scale, precision,
color), the trader today opens Views or Schema from the palette, finds the
tile's view or the owning dataset in Browse, opens it, then finds the column
row and opens its Column stage. The tile already knows the view and the
column under the cursor. One palette action should land on that column's
Column stage directly.

Rulings (user, 2026-09-27):

1. The route is **palette only**. There is no `:` command; the tile-local
   `:` rule is unchanged.
2. Two actions, one per dialog: "Edit column in view…" and "Edit column in
   schema…".
3. Both **always** open a column list first, with the cursor's column
   preselected. Enter takes the active column; typing filters to name a
   different one. The list is how a column is given explicitly.
4. Every column the tile shows qualifies, not only measures: measures,
   derived measures and dimensions. The tree column does not.

## 2. Vocabulary: `ColumnContext`

`geode-core` gains a module `column_context` beside `launch`:

```rust
/// The columns a tile presents from a named view, and the one at its cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnContext {
    pub view: String,
    pub columns: Vec<ContextColumn>,
    /// Index into `columns`; `None` when the cursor is on no listed column.
    pub active: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextColumn {
    /// Dataset/view column name; the identity every lookup uses.
    pub name: String,
    /// Header label as painted, for the list row.
    pub label: String,
    /// True for a view-derived column, which has no declared dataset column.
    pub derived: bool,
}
```

The tile reports names only. It never resolves which dataset owns a
column; the shell does that against current configuration (§5).

## 3. `TileContent::column_context`

`TileContent` gains a pull method, defaulting to `None`:

```rust
fn column_context(&self, cx: &App) -> Option<ColumnContext> { None }
```

The blotter implements it from its current plan: the non-tree
`PlannedColumn`s in display order (hidden columns and grouped dimensions
are not in the plan and so are not offered), `derived` from
`ViewColumn::Derived`, and `active` set from the cursor column when it is
not the tree column. Before a plan exists it returns `None`.

No other tile implements it. The pricer and market-data panels do not use
Views or Schema.

## 4. Actions and the column list

Two built-in actions, no default binding:

| Id | Title |
|---|---|
| `config::view_column` | Edit column in view… |
| `config::schema_column` | Edit column in schema… |

On dispatch the shell pulls the focused occupant's `column_context`.

- No focused tile, or `None`: notice "This tile has no dataset columns";
  nothing opens.
- The target domain's object dialog is already open anywhere in the dialog
  stack: the existing stack refusal and its notice apply, before the list
  opens.
- Otherwise the column list opens through `shell::choicedialog`, like the
  tile and grouping pickers. Rows show the label, followed by the name
  when it differs. The title names the view and the target dialog.
- The Schema list omits derived columns: they have no declared column to
  edit. If the cursor is on one, nothing is preselected and the list
  opens at its first row.
- An empty list (a Schema list over a view of only derived columns) does
  not open; notice "No schema columns in view 'x'".

Escape closes the list and nothing else. Enter or a click on a row
commits it.

## 5. Resolution and opening

On commit the shell resolves against the pending-aware configuration at
that moment, not the tile's factory copy, because a reload or a queued
dialog edit may have changed the view since the tile last planned.

- **View**: `open_object(Views, view)` then `enter_column_stage(column)`.
- **Schema**: look up the view, then the owning dataset with
  `DatasetPresentationSpec::owner_of(view, column, schema)`. That is the
  view's dataset for its own columns and a joined dataset for a joined
  column. Then `open_object(Schema, dataset)` then
  `enter_column_stage(column)`.

One new function in `objectdialog/render.rs`, beside `open_object`, runs
the chain. `enter_column_stage` fails silently today; the new route
reports each failure and leaves the dialog at the stage it reached:

| Failure | Result |
|---|---|
| view no longer defined | Views: Browse with the existing "'x' is not defined" notice; Schema: notice, nothing opens |
| column no longer in the view (Views) | Edit stage of the view, notice "'col' is not a column of view 'x'" |
| no owning dataset declares the column (Schema) | notice "'col' is not declared by any dataset of view 'x'", nothing opens |

Escape and Back are unchanged: Column → the object's Edit stage → Browse →
close. Closing returns focus to the tile through the existing dialog-close
restoration.

## 6. Tests

Lowest layer first:

- Pure: blotter `column_context` from a plan (tree column excluded,
  `active` from the cursor, `None` on the tree column, `derived` flag);
  the list model for Schema drops derived columns and clears a derived
  preselect.
- Pure: owner resolution picks the joined dataset for a joined column.
- GPUI, through the palette route (type the title, Enter), with a
  blotter focused:
  - cursor on a measure → Enter → Views Column stage for that column;
  - typed filter picks a different column;
  - joined measure → Schema opens the owning dataset's Column stage;
  - derived column is absent from the Schema list;
  - a non-blotter tile focused → notice, no dialog;
  - Views already open under the stack → refusal notice, no list;
  - view removed between list open and commit → the §5 notice.
- Mutation-harness entries for the preselect, the owner resolution, the
  derived-column omission and the stage chain.

## 7. Documentation

Same change: `docs/current/configuration-dialogs.md` (a direct Column
stage entry), `docs/current/input-and-dialogs.md` (the two actions and the
column list), `docs/current/features.md` Blotter, the blotter and shell
READMEs, and the `TileContent` doc for `column_context`. Remove the TODO
line "Blotter shortcut to edit column in either view or schema".

## 8. Out of scope

- Palette argument syntax (actions taking typed arguments).
- A `:` command for this route (ruling 1).
- Grouped dimensions shown only as tree levels, and hidden columns: not
  in the plan, not offered. Reach them through the dialogs as today.
